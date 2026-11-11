//! What's playing, from the media sessions Windows keeps for every app with a player (the same
//! ones as the volume flyout's: Spotify, a browser, the Media Player…): `♪ Title  Artist`, dimmed
//! with a pause sign while paused (or hidden, with `paused = false`), hidden when nothing plays.
//! The popup is a player: the cover, the title, the artist and the app, where the track is (a
//! click or a drag on the bar seeks, when the app allows it) and ⏮ ⏯ ⏭.
//!
//! The sessions come from `GlobalSystemMediaTransportControlsSessionManager` (GSMTC), which says
//! when the current session changes, and the session when its track or its state does: the
//! handlers run on threads of their own and only set an event (the track's properties, which come
//! asynchronously, are fetched there too). Nothing is read on a timer: while the popup is open and
//! the track plays, a timer moves the position once a second. The cover is read only for the open
//! popup, decoded and scaled with WIC on the thread the properties arrive on, and dropped with
//! the popup. While no bar shows every handler is removed.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session, GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionMediaProperties as Properties, GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapInterpolationModeHighQualityCubic,
    WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand, WICRect,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IStream};
use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
use windows::Win32::System::WinRT::CreateStreamOverRandomAccessStream;
use windows::core::HSTRING;

use super::{Input, Reply, Signal};
use crate::config::Options;
use crate::i18n::t;
use crate::log;
use crate::theme::Theme;
use crate::ui::{Align, Font, Fonts, Image, Item, Pill, Rect, Scene, Weight};
use crate::wake::{Timer, Wake};

// Segoe Fluent Icons
const NOTE: &str = "\u{EC4F}";
const PLAY: &str = "\u{E768}";
const PAUSE: &str = "\u{E769}";
const PREVIOUS: &str = "\u{E892}";
const NEXT: &str = "\u{E893}";

/// The cover's side, in pixels: twice its size in the popup, so it stays sharp at 200%.
const COVER_PX: u32 = 152;

// The popup's layout, in DIPs.
const MARGIN: f32 = 8.0;
const INNER: f32 = 304.0;
const WIDTH: f32 = INNER + 2.0 * MARGIN;
const PAD: f32 = 12.0;
const COVER: f32 = 76.0;
const BAR_H: f32 = 20.0;
const TIMES_H: f32 = 16.0;
const CONTROLS_H: f32 = 44.0;

#[derive(Clone, Debug, Default, PartialEq)]
struct Track {
    title: String,
    artist: String,
}

/// The state, and what the app lets the controls do.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Playback {
    status: i32,
    previous: bool,
    next: bool,
    toggle: bool,
    seek: bool,
}

impl Playback {
    fn playing(&self) -> bool {
        self.status == Status::Playing.0
    }

    fn paused(&self) -> bool {
        self.status == Status::Paused.0
    }
}

/// Where the track is, in 100 ns, as the app said it at `updated` (Windows' clock, 100 ns since
/// 1601).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Timeline {
    start: i64,
    end: i64,
    position: i64,
    updated: i64,
}

/// What the handlers, on threads of their own, leave for the main thread.
struct Shared {
    wake: Arc<Wake>,
    manager: Mutex<Option<Manager>>,
    session_changed: AtomicBool,
    playback_changed: AtomicBool,
    timeline_changed: AtomicBool,
    /// The session's number: results for an older one are dropped.
    generation: AtomicU64,
    track: Mutex<Option<(u64, Track)>>,
    /// Whether the popup wants the cover, and the cover.
    cover_wanted: AtomicBool,
    cover: Mutex<Option<(u64, Option<Arc<Image>>)>>,
}

impl Shared {
    /// Asks the session for its track; the answer, and with the popup open the cover, come on
    /// another thread.
    fn fetch(self: &Arc<Shared>, session: &Session) {
        let generation = self.generation.load(Ordering::Acquire);
        let shared = self.clone();
        let asked = session.TryGetMediaPropertiesAsync().and_then(|op| {
            op.when(move |result| {
                let Ok(properties) = result else { return };
                let text = |s: windows::core::Result<HSTRING>| s.map(|s| s.to_string()).unwrap_or_default();
                let track = Track { title: text(properties.Title()), artist: text(properties.Artist()) };
                *lock(&shared.track) = Some((generation, track));
                if shared.cover_wanted.load(Ordering::Acquire) {
                    let cover = decode_cover(&properties).map(Arc::new);
                    *lock(&shared.cover) = Some((generation, cover));
                }
                shared.wake.set();
            })
        });
        if let Err(e) = asked {
            log!("media: can't ask for the track ({e})");
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The session followed, and the tokens of its handlers.
struct Hooked {
    session: Session,
    properties: i64,
    playback: i64,
    timeline: Option<i64>,
}

/// What the open popup needs.
struct Open {
    app: String,
    cover: Option<Arc<Image>>,
    /// Where a drag on the bar is, until the mouse goes up.
    seek: Option<f32>,
}

pub struct Media {
    shared: Arc<Shared>,
    manager: Option<Manager>,
    /// The token of the manager's handler, while a bar shows.
    manager_token: Option<i64>,
    hooked: Option<Hooked>,
    aumid: String,
    track: Track,
    playback: Playback,
    timeline: Timeline,
    timer: Timer,
    shown: bool,
    show_paused: bool,
    /// Whether the popup shows the cover (`cover = false`: no thumbnail read, no WIC).
    covers: bool,
    open: Option<Open>,
}

impl Media {
    pub fn new(options: &Options) -> Media {
        let shared = Arc::new(Shared {
            wake: Wake::new(),
            manager: Mutex::new(None),
            session_changed: AtomicBool::new(false),
            playback_changed: AtomicBool::new(false),
            timeline_changed: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            track: Mutex::new(None),
            cover_wanted: AtomicBool::new(false),
            cover: Mutex::new(None),
        });
        // The manager comes asynchronously: the first frame doesn't wait for it
        let s = shared.clone();
        let asked = Manager::RequestAsync().and_then(|op| {
            op.when(move |result| match result {
                Ok(manager) => {
                    *lock(&s.manager) = Some(manager);
                    s.session_changed.store(true, Ordering::Release);
                    s.wake.set();
                }
                Err(e) => log!("media: no media sessions ({e})"),
            })
        });
        if let Err(e) = asked {
            log!("media: no media sessions ({e})");
        }
        Media {
            shared,
            manager: None,
            manager_token: None,
            hooked: None,
            aumid: String::new(),
            track: Track::default(),
            playback: Playback::default(),
            timeline: Timeline::default(),
            timer: Timer::new(),
            shown: true,
            show_paused: options.bool("paused").unwrap_or(true),
            covers: options.bool("cover").unwrap_or(true),
            open: None,
        }
    }

    /// Follows the manager's current session.
    fn hook_manager(&mut self) {
        let Some(manager) = &self.manager else { return };
        if self.manager_token.is_none() {
            let shared = self.shared.clone();
            let handler = TypedEventHandler::new(move |_, _| {
                shared.session_changed.store(true, Ordering::Release);
                shared.wake.set();
                Ok(())
            });
            self.manager_token = manager.CurrentSessionChanged(&handler).map_err(|e| log!("media: {e}")).ok();
        }
    }

    fn unhook(&mut self) {
        if let (Some(manager), Some(token)) = (&self.manager, self.manager_token.take()) {
            let _ = manager.RemoveCurrentSessionChanged(token);
        }
        self.unhook_session();
    }

    fn unhook_session(&mut self) {
        if let Some(h) = self.hooked.take() {
            let _ = h.session.RemoveMediaPropertiesChanged(h.properties);
            let _ = h.session.RemovePlaybackInfoChanged(h.playback);
            if let Some(t) = h.timeline {
                let _ = h.session.RemoveTimelinePropertiesChanged(t);
            }
        }
    }

    /// Starts following the current session, or none: its handlers, its state now, its track.
    fn follow_current(&mut self) {
        self.unhook_session();
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        let session = self.manager.as_ref().and_then(|m| m.GetCurrentSession().ok());
        let Some(session) = session else {
            self.aumid.clear();
            self.track = Track::default();
            self.playback = Playback::default();
            self.timeline = Timeline::default();
            return;
        };
        let shared = self.shared.clone();
        let on_properties = TypedEventHandler::new(move |sender: windows::core::Ref<Session>, _| {
            if let Ok(session) = sender.ok() {
                shared.fetch(session);
            }
            Ok(())
        });
        let shared = self.shared.clone();
        let on_playback = TypedEventHandler::new(move |_, _| {
            shared.playback_changed.store(true, Ordering::Release);
            shared.wake.set();
            Ok(())
        });
        let properties = session.MediaPropertiesChanged(&on_properties).unwrap_or_default();
        let playback = session.PlaybackInfoChanged(&on_playback).unwrap_or_default();
        self.aumid = session.SourceAppUserModelId().map(|s| s.to_string()).unwrap_or_default();
        self.shared.fetch(&session);
        self.hooked = Some(Hooked { session, properties, playback, timeline: None });
        self.read_playback();
        self.read_timeline();
        if self.open.is_some() {
            self.hook_timeline();
        }
    }

    fn hook_timeline(&mut self) {
        let Some(h) = &mut self.hooked else { return };
        if h.timeline.is_none() {
            let shared = self.shared.clone();
            let handler = TypedEventHandler::new(move |_, _| {
                shared.timeline_changed.store(true, Ordering::Release);
                shared.wake.set();
                Ok(())
            });
            h.timeline = h.session.TimelinePropertiesChanged(&handler).ok();
        }
    }

    fn read_playback(&mut self) {
        let Some(h) = &self.hooked else { return };
        let Ok(info) = h.session.GetPlaybackInfo() else { return };
        let controls = info.Controls().ok();
        let can = |f: fn(&windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackControls) -> windows::core::Result<bool>| {
            controls.as_ref().and_then(|c| f(c).ok()).unwrap_or(false)
        };
        self.playback = Playback {
            status: info.PlaybackStatus().map(|s| s.0).unwrap_or(0),
            previous: can(|c| c.IsPreviousEnabled()),
            next: can(|c| c.IsNextEnabled()),
            toggle: can(|c| c.IsPlayPauseToggleEnabled()),
            seek: can(|c| c.IsPlaybackPositionEnabled()),
        };
    }

    fn read_timeline(&mut self) {
        let Some(h) = &self.hooked else { return };
        let Ok(t) = h.session.GetTimelineProperties() else { return };
        self.timeline = Timeline {
            start: t.StartTime().map(|d| d.Duration).unwrap_or(0),
            end: t.EndTime().map(|d| d.Duration).unwrap_or(0),
            position: t.Position().map(|d| d.Duration).unwrap_or(0),
            updated: t.LastUpdatedTime().map(|d| d.UniversalTime).unwrap_or(0),
        };
    }

    /// Takes what the handlers left: a new manager, a new session, a new state, a track, a cover.
    fn sync(&mut self) {
        if self.manager.is_none() {
            self.manager = lock(&self.shared.manager).take();
            if self.manager.is_some() && self.shown {
                self.hook_manager();
            }
        }
        if !self.shown {
            return;
        }
        if self.shared.session_changed.swap(false, Ordering::AcqRel) {
            self.follow_current();
        }
        if self.shared.playback_changed.swap(false, Ordering::AcqRel) {
            self.read_playback();
            self.read_timeline();
        }
        if self.shared.timeline_changed.swap(false, Ordering::AcqRel) {
            self.read_timeline();
        }
        let generation = self.shared.generation.load(Ordering::Acquire);
        if let Some((g, track)) = lock(&self.shared.track).take()
            && g == generation
        {
            self.track = track;
        }
        if let Some((g, cover)) = lock(&self.shared.cover).take()
            && g == generation
            && let Some(open) = &mut self.open
        {
            open.cover = cover;
        }
        self.schedule();
    }

    /// The timer moves the position while the popup is open and the track plays.
    fn schedule(&mut self) {
        if self.open.is_some() && self.playback.playing() && self.timeline.end > self.timeline.start {
            self.timer.start(1000, 1000);
        } else {
            self.timer.stop();
        }
    }

    /// Where the track is now and how long it is, in 100 ns: from where the app said it was,
    /// plus the time since, while it plays.
    fn position(&self) -> Option<(i64, i64)> {
        let t = self.timeline;
        let length = t.end - t.start;
        if length <= 0 {
            return None;
        }
        let mut position = t.position - t.start;
        if self.playback.playing() && t.updated > 0 {
            position += (now() - t.updated).max(0);
        }
        Some((position.clamp(0, length), length))
    }

    fn visible(&self) -> bool {
        !self.track.title.is_empty() && (self.playback.playing() || (self.playback.paused() && self.show_paused))
    }

    /// What the pill and the popup show, to tell whether something changed.
    fn shown_state(&self) -> (Track, Playback, bool) {
        (self.track.clone(), self.playback, self.visible())
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        self.unhook();
    }
}

impl super::Widget for Media {
    fn pill(&self, theme: &Theme) -> Option<Pill> {
        if !self.visible() {
            return None;
        }
        let paused = !self.playback.playing();
        let (icon, title) = if paused { (PAUSE, theme.subtext) } else { (NOTE, theme.text) };
        let mut items = vec![Item::icon(icon, title), Item::text(&self.track.title, title).max_width(240.0)];
        if !self.track.artist.is_empty() {
            items.push(Item::text(&self.track.artist, theme.subtext).max_width(160.0).space(8.0));
        }
        Some(Pill::new(items).gap(6.0))
    }

    fn handles(&self) -> Vec<HANDLE> {
        self.shared.wake.handle().into_iter().chain(self.timer.handle()).collect()
    }

    fn signaled_at(&mut self, index: usize) -> bool {
        if index == 1 {
            // The position, once a second: the popup draws again only if the time shown changed
            return self.open.is_some();
        }
        let before = self.shown_state();
        self.sync();
        // With the popup open anything in it may have changed (the cover, the position): its
        // scene is drawn again only if it differs
        self.open.is_some() || self.shown_state() != before
    }

    fn event(&mut self, signal: &Signal) -> bool {
        match signal {
            Signal::Shown(shown) if *shown != self.shown => {
                self.shown = *shown;
                if *shown {
                    // Everything may have changed meanwhile
                    self.hook_manager();
                    self.shared.session_changed.store(true, Ordering::Release);
                    let before = self.shown_state();
                    self.sync();
                    self.shown_state() != before
                } else {
                    self.unhook();
                    self.timer.stop();
                    false
                }
            }
            Signal::Resume => {
                self.shared.session_changed.store(true, Ordering::Release);
                self.shared.wake.set();
                false
            }
            _ => false,
        }
    }

    fn has_popup(&self) -> bool {
        true
    }

    fn popup_shown(&mut self, open: bool) {
        self.shared.cover_wanted.store(open && self.covers, Ordering::Release);
        if open {
            self.open = Some(Open { app: app_name(&self.aumid), cover: None, seek: None });
            self.hook_timeline();
            self.read_timeline();
            if let Some(h) = &self.hooked {
                let session = h.session.clone();
                self.shared.fetch(&session);
            }
        } else {
            self.open = None;
            *lock(&self.shared.cover) = None;
            if let Some(h) = &mut self.hooked
                && let Some(t) = h.timeline.take()
            {
                let _ = h.session.RemoveTimelinePropertiesChanged(t);
            }
        }
        self.schedule();
    }

    fn popup(&self, theme: &Theme, _fonts: &Fonts) -> Option<Scene> {
        let open = self.open.as_ref()?;
        if !self.visible() && self.track.title.is_empty() {
            return None;
        }
        let position = self.position();
        let mut height = MARGIN + PAD + COVER + PAD;
        if position.is_some() {
            height += BAR_H + TIMES_H;
        }
        height += CONTROLS_H + PAD + MARGIN - 4.0;
        let mut scene = Scene::new(WIDTH, height);
        let left = MARGIN + PAD;

        // The cover, or a note where it would be
        let cover = Rect::new(left, MARGIN + PAD, COVER, COVER);
        match &open.cover {
            Some(image) => scene.image(cover, 6.0, image.clone()),
            None => {
                scene.fill(cover, 6.0, theme.tile(theme.primary, false));
                scene.text(cover, NOTE, Font::icons(28.0), theme.subtext, Align::Center);
            }
        }
        // Title, artist, app
        let x = cover.x + COVER + 12.0;
        let w = MARGIN + INNER - PAD - x;
        let title = if self.track.title.is_empty() { t("Niente in riproduzione", "Nothing playing") } else { self.track.title.as_str() };
        scene.text(Rect::new(x, cover.y + 6.0, w, 22.0), title, Font::new(theme.font_size + 1.0, Weight::SemiBold), theme.text, Align::Start);
        scene.text(Rect::new(x, cover.y + 30.0, w, 20.0), self.track.artist.as_str(), Font::new(theme.font_size, Weight::Regular), theme.text, Align::Start);
        scene.text(Rect::new(x, cover.y + 52.0, w, 18.0), open.app.as_str(), Font::new(theme.small_size, Weight::Medium), theme.subtext, Align::Start);

        let mut y = cover.y + COVER + PAD;
        // Where the track is
        if let Some((now, length)) = position {
            let fraction = open.seek.unwrap_or((now as f64 / length as f64) as f32);
            let track = Rect::new(left + 4.0, y, INNER - 2.0 * PAD - 8.0, BAR_H);
            super::audio::slider(&mut scene, theme, track, fraction, self.playback.seek);
            if self.playback.seek {
                scene.slider(Rect::new(track.x - 8.0, track.y, track.w + 16.0, track.h), "seek");
            }
            y += BAR_H;
            let small = Font::new(theme.small_size, Weight::Medium);
            let shown_now = (fraction as f64 * length as f64) as i64;
            scene.text(Rect::new(left, y, 80.0, TIMES_H), clock(shown_now), small, theme.subtext, Align::Start);
            scene.text(Rect::new(MARGIN + INNER - PAD - 80.0, y, 80.0, TIMES_H), clock(length), small, theme.subtext, Align::End);
            y += TIMES_H;
        }
        // ⏮ ⏯ ⏭
        let mid = WIDTH / 2.0;
        let toggle = Rect::new(mid - 20.0, y + 2.0, 40.0, 40.0);
        let can = |on: bool| if on { theme.text } else { theme.overlay };
        scene.fill(toggle, 20.0, if self.playback.toggle { theme.primary } else { theme.tile(theme.primary, false) });
        let glyph = if self.playback.playing() { PAUSE } else { PLAY };
        scene.text(toggle, glyph, Font::icons(16.0), if self.playback.toggle { theme.on_primary } else { theme.overlay }, Align::Center);
        let previous = Rect::new(mid - 76.0, y + 4.0, 36.0, 36.0);
        let next = Rect::new(mid + 40.0, y + 4.0, 36.0, 36.0);
        scene.text(previous, PREVIOUS, Font::icons(15.0), can(self.playback.previous), Align::Center);
        scene.text(next, NEXT, Font::icons(15.0), can(self.playback.next), Align::Center);
        if self.playback.toggle {
            scene.zone(toggle, 20.0, "toggle");
        }
        if self.playback.previous {
            scene.zone(previous, 18.0, "previous");
        }
        if self.playback.next {
            scene.zone(next, 18.0, "next");
        }
        Some(scene)
    }

    fn popup_input(&mut self, input: &Input) -> Reply {
        let Some(h) = &self.hooked else { return Reply::Nothing };
        let session = h.session.clone();
        match input {
            Input::Action("toggle") => {
                let _ = session.TryTogglePlayPauseAsync();
                Reply::Nothing
            }
            Input::Action("previous") => {
                let _ = session.TrySkipPreviousAsync();
                Reply::Nothing
            }
            Input::Action("next") => {
                let _ = session.TrySkipNextAsync();
                Reply::Nothing
            }
            Input::Key(k) if *k == windows::Win32::UI::Input::KeyboardAndMouse::VK_SPACE.0 && self.playback.toggle => {
                let _ = session.TryTogglePlayPauseAsync();
                Reply::Nothing
            }
            Input::Slide("seek", fraction) => {
                if let Some(open) = &mut self.open {
                    open.seek = Some(*fraction);
                }
                Reply::Redraw
            }
            // The seek happens when the mouse goes up: a drag doesn't make the app jump around
            Input::Release("seek") => {
                let Some(fraction) = self.open.as_mut().and_then(|o| o.seek.take()) else { return Reply::Nothing };
                let t = self.timeline;
                let target = t.start + ((t.end - t.start) as f64 * fraction as f64) as i64;
                let _ = session.TryChangePlaybackPositionAsync(target);
                // Shown there until the app says where it is
                self.timeline.position = target;
                self.timeline.updated = now();
                Reply::Redraw
            }
            _ => Reply::Nothing,
        }
    }
}

/// Windows' clock, as the media sessions write their times: 100 ns since 1601.
fn now() -> i64 {
    let t = unsafe { GetSystemTimePreciseAsFileTime() };
    ((t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64) as i64
}

/// `3:07`, `1:02:03`.
fn clock(ticks: i64) -> String {
    let s = (ticks.max(0) / 10_000_000) as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// The app behind a session, from its id made readable: `Spotify.exe` → `Spotify`,
/// `chrome` → `Chrome`, a packaged app by the name after its `!`. Nothing for an id that's only a
/// code (Firefox's is a hash of where it's installed). The name Start shows would take the shell's
/// app folder: 54 ms and 1.2 MB that stay, for a line of text.
fn app_name(aumid: &str) -> String {
    let code = aumid.len() >= 8 && aumid.chars().all(|c| c.is_ascii_hexdigit());
    if code { String::new() } else { readable(aumid) }
}

fn readable(aumid: &str) -> String {
    let name = aumid.rsplit('!').next().unwrap_or(aumid);
    let name = name.strip_suffix(".exe").or_else(|| name.strip_suffix(".EXE")).unwrap_or(name);
    let name = name.rsplit('.').next().filter(|s| !s.is_empty()).unwrap_or(name);
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The track's cover, cut to a square from its middle and scaled to `COVER_PX`, ready to draw.
/// On the thread the properties came on (a worker of Windows' thread pool).
fn decode_cover(properties: &Properties) -> Option<Image> {
    static IDS: AtomicU64 = AtomicU64::new(1);
    let reference = properties.Thumbnail().ok()?;
    let stream = reference.OpenReadAsync().ok()?.join().ok()?;
    unsafe {
        // The pool's threads may have no COM yet: WIC needs it
        let initialized = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let image = (|| -> windows::core::Result<Image> {
            let source: IStream = CreateStreamOverRandomAccessStream(&stream)?;
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let decoder = wic.CreateDecoderFromStream(&source, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?;
            let frame = decoder.GetFrame(0)?;
            let (mut w, mut h) = (0u32, 0u32);
            frame.GetSize(&mut w, &mut h)?;
            let side = w.min(h).max(1);
            let clipper = wic.CreateBitmapClipper()?;
            let square = WICRect { X: ((w - side) / 2) as i32, Y: ((h - side) / 2) as i32, Width: side as i32, Height: side as i32 };
            clipper.Initialize(&frame, &square)?;
            let scaler = wic.CreateBitmapScaler()?;
            scaler.Initialize(&clipper, COVER_PX, COVER_PX, WICBitmapInterpolationModeHighQualityCubic)?;
            let converter = wic.CreateFormatConverter()?;
            converter.Initialize(&scaler, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
            let mut pixels = vec![0u8; (COVER_PX * COVER_PX * 4) as usize];
            converter.CopyPixels(std::ptr::null(), COVER_PX * 4, &mut pixels)?;
            Ok(Image { id: IDS.fetch_add(1, Ordering::Relaxed), width: COVER_PX, height: COVER_PX, pixels })
        })();
        if initialized {
            CoUninitialize();
        }
        image.map_err(|e| log!("media: cover: {e}")).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_and_names() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(187 * 10_000_000), "3:07");
        assert_eq!(clock(3723 * 10_000_000), "1:02:03");
        assert_eq!(readable("Spotify.exe"), "Spotify");
        assert_eq!(readable("chrome"), "Chrome");
        assert_eq!(readable("Contoso.Player_abc123!App"), "App");
        assert_eq!(app_name("308046B0AF4A39CB"), "");
        assert_eq!(app_name("Spotify.exe"), "Spotify");
    }
}
