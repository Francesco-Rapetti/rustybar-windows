//! What's playing, from the media sessions Windows keeps for every app with a player (the same
//! ones as the volume flyout's: Spotify, a browser, the Media Player…): `♪ Title  Artist`, dimmed
//! with a pause sign while paused (or hidden, with `paused = false`), hidden when nothing plays.
//! The popup is a player: the cover, the title, the artist and the app, where the track is (a
//! click or a drag on the bar seeks, when the app allows it), shuffle, ⏮ ⏯ ⏭ and repeat (shuffle
//! and repeat only for an app that takes them), and the app's own volume, as the volume mixer has
//! it. With more than one app that has a session, a switch at the top picks the one the player is
//! for (and the pill, while the popup is open); closing the popup goes back to the one Windows
//! calls current. The popup takes its colors from the cover, as Android's media player does (see
//! `palette`), unless `cover_colors = false`.
//!
//! The sessions come from `GlobalSystemMediaTransportControlsSessionManager` (GSMTC), which says
//! when the current session changes, and the session when its track or its state does: the
//! handlers run on threads of their own and only set an event (the track's properties, which come
//! asynchronously, are fetched there too). Nothing is read on a timer: while the popup is open and
//! the track plays, a timer moves the position once a second. The cover is read only for the open
//! popup, decoded and scaled with WIC on the thread the properties arrive on, where its colors are
//! worked out too, and dropped with the popup. The list of sessions is read, and followed, only
//! while the popup is open. The app's volume is its audio sessions (Core Audio's
//! `IAudioSessionManager2`, on every output in use), found by the app's executable or, for a
//! packaged app, by its id; found when the popup opens and followed with `IAudioSessionEvents`
//! until it closes. An app whose id is only a code (Firefox's) can't be told from its processes:
//! no volume for it. While no bar shows every handler is removed.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session, GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionMediaProperties as Properties,
    GlobalSystemMediaTransportControlsSessionPlaybackControls as Controls, GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Media::MediaPlaybackAutoRepeatMode as Repeat;
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, GENERIC_READ, HANDLE, S_OK};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmapDecoder, IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapInterpolationModeHighQualityCubic,
    WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand, WICRect,
};
use windows::Win32::Media::Audio::{
    AudioSessionDisconnectReason, AudioSessionState, AudioSessionStateExpired, DEVICE_STATE_ACTIVE, IAudioSessionControl, IAudioSessionControl2,
    IAudioSessionEvents, IAudioSessionEvents_Impl, IAudioSessionManager2, IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator, eRender,
};
use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
use windows::Win32::System::Com::{CLSCTX_ALL, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IStream};
use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows::Win32::System::WinRT::CreateStreamOverRandomAccessStream;
use windows::core::{BOOL, GUID, HSTRING, Interface, PCWSTR, PWSTR, implement};

use super::{Input, Reply, Signal};
use crate::config::Options;
use crate::i18n::t;
use crate::json::Json;
use crate::log;
use crate::palette::Palette;
use crate::theme::Theme;
use crate::ui::{Align, Backdrop, Font, Fonts, Image, Item, Pill, Rect, Scene, Weight};
use crate::wake::{Timer, Wake};

// Segoe Fluent Icons
const NOTE: &str = "\u{EC4F}";
const PLAY: &str = "\u{E768}";
const PAUSE: &str = "\u{E769}";
const PREVIOUS: &str = "\u{E892}";
const NEXT: &str = "\u{E893}";
const SHUFFLE: &str = "\u{E8B1}";
const REPEAT_ALL: &str = "\u{E8EE}";
const REPEAT_ONE: &str = "\u{E8ED}";

/// The cover's side, in pixels: twice its size in the popup, so it stays sharp at 200%.
const COVER_PX: u32 = 152;

// The popup's layout, in DIPs.
const MARGIN: f32 = 8.0;
const INNER: f32 = 304.0;
const WIDTH: f32 = INNER + 2.0 * MARGIN;
const PAD: f32 = 12.0;
const SOURCES_H: f32 = 30.0;
const COVER: f32 = 76.0;
const BAR_H: f32 = 20.0;
const TIMES_H: f32 = 16.0;
const CONTROLS_H: f32 = 44.0;
const VOLUME_H: f32 = 32.0;

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
    /// Shuffle on or off, for an app that lets it change.
    shuffle: Option<bool>,
    /// What repeats (`MediaPlaybackAutoRepeatMode`: nothing, the track, the list), for an app that
    /// lets it change.
    repeat: Option<i32>,
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

/// A cover, decoded, with its colors.
struct Cover {
    image: Arc<Image>,
    palette: Option<Palette>,
}

/// What the handlers, on threads of their own, leave for the main thread.
struct Shared {
    wake: Arc<Wake>,
    manager: Mutex<Option<Manager>>,
    session_changed: AtomicBool,
    playback_changed: AtomicBool,
    timeline_changed: AtomicBool,
    /// The list of sessions changed (followed only while the popup is open).
    sessions_changed: AtomicBool,
    /// The app's volume changed (followed only while the popup is open).
    volume_changed: AtomicBool,
    /// The session's number: results for an older one are dropped.
    generation: AtomicU64,
    track: Mutex<Option<(u64, Track)>>,
    /// Whether the popup wants the cover, and the cover.
    cover_wanted: AtomicBool,
    cover: Mutex<Option<(u64, Option<Cover>)>>,
    /// Whether covers get their colors worked out (`cover_colors`).
    cover_colors: bool,
    /// The titles of the sessions whose app has no readable name, by the app's id, for the switch.
    labels: Mutex<Vec<(String, String)>>,
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
                    let cover = decode_cover(&properties).map(|image| {
                        let palette = if shared.cover_colors { Palette::of_bgra(&image.pixels, image.width as usize, 2) } else { None };
                        Cover { image: Arc::new(image), palette }
                    });
                    *lock(&shared.cover) = Some((generation, cover));
                }
                shared.wake.set();
            })
        });
        if let Err(e) = asked {
            log!("media: can't ask for the track ({e})");
        }
    }

    /// Asks a session for its title, to name it in the switch when its app has no readable name.
    fn label(self: &Arc<Shared>, session: &Session, aumid: String) {
        let shared = self.clone();
        let _ = session.TryGetMediaPropertiesAsync().and_then(|op| {
            op.when(move |result| {
                let title = result.and_then(|p| p.Title()).map(|s| s.to_string()).unwrap_or_default();
                if !title.is_empty() {
                    lock(&shared.labels).push((aumid, title));
                    shared.wake.set();
                }
            })
        });
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

/// An app with a session, in the popup's switch: its id and what to call it.
#[derive(Clone, Debug, PartialEq)]
struct Source {
    aumid: String,
    name: String,
}

/// What the open popup needs.
struct Open {
    app: String,
    cover: Option<Arc<Image>>,
    palette: Option<Palette>,
    /// Where a drag on the bar is, until the mouse goes up.
    seek: Option<f32>,
    /// The apps with a session, and the one picked in the switch.
    sources: Vec<Source>,
    pick: Option<String>,
    volume: Option<AppVolume>,
}

pub struct Media {
    shared: Arc<Shared>,
    manager: Option<Manager>,
    /// The token of the manager's handler, while a bar shows.
    manager_token: Option<i64>,
    /// The token of the handler of the list of sessions, while the popup is open.
    sessions_token: Option<i64>,
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
    mock: Option<Mock>,
}

/// The made-up players of `media.json` (see `mock`), and the one Windows would call current.
struct Mock {
    players: Vec<Player>,
    current: usize,
}

/// A made-up player: its app, its track and its state, its cover's file and its volume.
struct Player {
    aumid: String,
    track: Track,
    playback: Playback,
    timeline: Timeline,
    cover: Option<std::path::PathBuf>,
    volume: Option<(f32, bool)>,
}

impl Media {
    pub fn new(options: &Options) -> Media {
        let shared = Arc::new(Shared {
            wake: Wake::new(),
            manager: Mutex::new(None),
            session_changed: AtomicBool::new(false),
            playback_changed: AtomicBool::new(false),
            timeline_changed: AtomicBool::new(false),
            sessions_changed: AtomicBool::new(false),
            volume_changed: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            track: Mutex::new(None),
            cover_wanted: AtomicBool::new(false),
            cover: Mutex::new(None),
            cover_colors: options.bool("cover_colors").unwrap_or(true),
            labels: Mutex::new(Vec::new()),
        });
        if let Some(m) = crate::mock::load("media") {
            return Media::mock(options, shared, &m);
        }
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
            sessions_token: None,
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
            mock: None,
        }
    }

    /// The widget with the made-up players of `media.json`: no media session is asked for or
    /// followed. Each player: `app` (its id, as `Spotify.exe`), `title`, `artist`, `state`
    /// (`playing` or `paused`), `duration` and `position` in seconds, `shuffle`, `repeat` (`off`,
    /// `all`, `one`), `volume` in percent, `cover` (a file next to `media.json`) and `controls`
    /// (`previous`, `next`, `seek`: false for one the app doesn't allow).
    fn mock(options: &Options, shared: Arc<Shared>, m: &Json) -> Media {
        let dir = crate::mock::dir().unwrap_or_default();
        let players: Vec<Player> = m["players"]
            .array()
            .iter()
            .map(|p| {
                let can = |k: &str| p["controls"][k].bool().unwrap_or(true);
                let ticks = |k: &str| (p[k].f64().unwrap_or(0.0) * 10_000_000.0) as i64;
                let status = if p["state"].str() == Some("paused") { Status::Paused.0 } else { Status::Playing.0 };
                let repeat = p["repeat"].str().map(|r| match r {
                    "one" => Repeat::Track.0,
                    "all" => Repeat::List.0,
                    _ => Repeat::None.0,
                });
                Player {
                    aumid: p["app"].str().unwrap_or_default().to_string(),
                    track: Track { title: p["title"].str().unwrap_or_default().to_string(), artist: p["artist"].str().unwrap_or_default().to_string() },
                    playback: Playback { status, previous: can("previous"), next: can("next"), toggle: true, seek: can("seek"), shuffle: p["shuffle"].bool(), repeat },
                    timeline: Timeline { start: 0, end: ticks("duration"), position: ticks("position"), updated: now() },
                    cover: p["cover"].str().map(|c| dir.join(c)),
                    volume: p["volume"].f64().map(|v| (v as f32 / 100.0, false)),
                }
            })
            .collect();
        let current = (m["current"].f64().unwrap_or(0.0) as usize).min(players.len().saturating_sub(1));
        let mut media = Media {
            shared,
            manager: None,
            manager_token: None,
            sessions_token: None,
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
            mock: Some(Mock { players, current }),
        };
        media.play_mock(current);
        media
    }

    /// Shows the made-up player `index`: in the pill, and in the popup if it's open.
    fn play_mock(&mut self, index: usize) {
        let Some(p) = self.mock.as_ref().and_then(|m| m.players.get(index)) else { return };
        self.aumid = p.aumid.clone();
        self.track = p.track.clone();
        self.playback = p.playback;
        self.timeline = p.timeline;
        self.fill_mock_popup();
    }

    /// What the open popup shows of the made-up player in the pill: its cover with its colors,
    /// its volume, and the apps of the switch.
    fn fill_mock_popup(&mut self) {
        if self.open.is_none() {
            return;
        }
        let Some(p) = self.mock.as_ref().and_then(|m| m.players.iter().find(|p| p.aumid == self.aumid)) else { return };
        let cover = p.cover.as_deref().filter(|_| self.covers).and_then(decode_cover_file);
        let palette = cover.as_ref().filter(|_| self.shared.cover_colors).and_then(|c| Palette::of_bgra(&c.pixels, c.width as usize, 2));
        let volume = p.volume.map(|(level, muted)| AppVolume::mock(&self.shared, level, muted));
        let sources: Vec<Source> = self.mock.as_ref().map_or(Vec::new(), |m| {
            m.players.iter().map(|p| Source { aumid: p.aumid.clone(), name: app_name(&p.aumid) }).collect()
        });
        if let Some(open) = &mut self.open {
            open.app = app_name(&self.aumid);
            open.cover = cover.map(Arc::new);
            open.palette = palette;
            open.volume = volume;
            open.sources = sources;
        }
    }

    /// With made-up data the controls change the made-up player alone.
    fn mock_input(&mut self, input: &Input) -> Reply {
        match input {
            Input::Action(a) if a.starts_with("source:") => {
                let Some(index) = a.strip_prefix("source:").and_then(|i| i.parse::<usize>().ok()) else { return Reply::Nothing };
                self.play_mock(index);
                if let Some(open) = &mut self.open {
                    open.pick = Some(self.aumid.clone());
                }
                self.schedule();
                Reply::Redraw
            }
            Input::Action("mute") => match self.open.as_mut().and_then(|o| o.volume.as_mut()) {
                Some(v) => {
                    v.set_mute(!v.muted);
                    Reply::Redraw
                }
                None => Reply::Nothing,
            },
            Input::Slide("volume", fraction) => match self.open.as_mut().and_then(|o| o.volume.as_mut()) {
                Some(v) => {
                    v.set_level(*fraction);
                    Reply::Redraw
                }
                None => Reply::Nothing,
            },
            Input::Action("toggle") | Input::Key(0x20) => {
                // Paused where it is, or playing on from there
                if let Some((position, _)) = self.position() {
                    self.timeline.position = self.timeline.start + position;
                }
                self.timeline.updated = now();
                self.playback.status = if self.playback.playing() { Status::Paused.0 } else { Status::Playing.0 };
                self.schedule();
                Reply::Redraw
            }
            Input::Action("shuffle") => {
                self.playback.shuffle = self.playback.shuffle.map(|on| !on);
                Reply::Redraw
            }
            Input::Action("repeat") => {
                self.playback.repeat = self.playback.repeat.map(next_repeat);
                Reply::Redraw
            }
            Input::Slide("seek", fraction) => {
                if let Some(open) = &mut self.open {
                    open.seek = Some(*fraction);
                }
                Reply::Redraw
            }
            Input::Release("seek") => {
                let Some(fraction) = self.open.as_mut().and_then(|o| o.seek.take()) else { return Reply::Nothing };
                let t = self.timeline;
                self.timeline.position = t.start + ((t.end - t.start) as f64 * fraction as f64) as i64;
                self.timeline.updated = now();
                Reply::Redraw
            }
            _ => Reply::Nothing,
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

    /// Follows the list of sessions, for the popup's switch.
    fn hook_sessions(&mut self) {
        let Some(manager) = &self.manager else { return };
        if self.sessions_token.is_none() {
            let shared = self.shared.clone();
            let handler = TypedEventHandler::new(move |_, _| {
                shared.sessions_changed.store(true, Ordering::Release);
                shared.wake.set();
                Ok(())
            });
            self.sessions_token = manager.SessionsChanged(&handler).ok();
        }
    }

    fn unhook_sessions(&mut self) {
        if let (Some(manager), Some(token)) = (&self.manager, self.sessions_token.take()) {
            let _ = manager.RemoveSessionsChanged(token);
        }
    }

    fn unhook(&mut self) {
        if let (Some(manager), Some(token)) = (&self.manager, self.manager_token.take()) {
            let _ = manager.RemoveCurrentSessionChanged(token);
        }
        self.unhook_sessions();
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

    /// The session to follow: the one picked in the popup, else the one Windows calls current.
    fn target(&self) -> Option<Session> {
        let manager = self.manager.as_ref()?;
        if let Some(pick) = self.open.as_ref().and_then(|o| o.pick.as_deref())
            && let Some(session) = sessions(manager).into_iter().find(|s| aumid_of(s) == pick)
        {
            return Some(session);
        }
        manager.GetCurrentSession().ok()
    }

    /// Starts following the session to follow, or none: its handlers, its state now, its track.
    fn follow(&mut self) {
        self.unhook_session();
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        let Some(session) = self.target() else {
            self.aumid.clear();
            self.track = Track::default();
            self.playback = Playback::default();
            self.timeline = Timeline::default();
            if let Some(open) = &mut self.open {
                open.app.clear();
                open.cover = None;
                open.palette = None;
                open.volume = None;
            }
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
        let aumid = aumid_of(&session);
        let other_app = aumid != self.aumid;
        self.aumid = aumid;
        self.shared.fetch(&session);
        self.hooked = Some(Hooked { session, properties, playback, timeline: None });
        self.read_playback();
        self.read_timeline();
        if self.open.is_some() {
            self.hook_timeline();
            if other_app {
                // Not the last app's cover, colors and volume until this one's come
                if let Some(open) = &mut self.open {
                    open.app = app_name(&self.aumid);
                    open.cover = None;
                    open.palette = None;
                    open.seek = None;
                }
                self.read_volume();
            }
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
        let can = |f: fn(&Controls) -> windows::core::Result<bool>| controls.as_ref().and_then(|c| f(c).ok()).unwrap_or(false);
        let shuffle = can(|c| c.IsShuffleEnabled()).then(|| info.IsShuffleActive().and_then(|r| r.Value()).unwrap_or(false));
        let repeat = can(|c| c.IsRepeatEnabled()).then(|| info.AutoRepeatMode().and_then(|r| r.Value()).map(|m| m.0).unwrap_or(0));
        self.playback = Playback {
            status: info.PlaybackStatus().map(|s| s.0).unwrap_or(0),
            previous: can(|c| c.IsPreviousEnabled()),
            next: can(|c| c.IsNextEnabled()),
            toggle: can(|c| c.IsPlayPauseToggleEnabled()),
            seek: can(|c| c.IsPlaybackPositionEnabled()),
            shuffle,
            repeat,
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

    /// The apps with a session, for the popup's switch; a picked app that has none any more goes
    /// back to the current one.
    fn read_sources(&mut self) {
        let (Some(manager), Some(open)) = (&self.manager, &mut self.open) else { return };
        let mut sources: Vec<Source> = Vec::new();
        for session in sessions(manager) {
            let aumid = aumid_of(&session);
            if aumid.is_empty() || sources.iter().any(|s| s.aumid == aumid) {
                continue;
            }
            let mut name = app_name(&aumid);
            if name.is_empty() {
                // Its title, which comes later, unless it came already
                name = open.sources.iter().find(|s| s.aumid == aumid).map(|s| s.name.clone()).unwrap_or_default();
                if name.is_empty() {
                    self.shared.label(&session, aumid.clone());
                }
            }
            sources.push(Source { aumid, name });
        }
        if let Some(pick) = &open.pick
            && !sources.iter().any(|s| &s.aumid == pick)
        {
            open.pick = None;
            self.shared.session_changed.store(true, Ordering::Release);
        }
        open.sources = sources;
    }

    /// Finds the audio sessions of the app followed, for the open popup.
    fn read_volume(&mut self) {
        let Some(open) = &mut self.open else { return };
        // The sessions of the app before stop telling first
        open.volume = None;
        open.volume = AppVolume::find(&self.aumid, &self.shared);
    }

    /// Takes what the handlers left: a new manager, a new session, a new state, a track, a cover.
    fn sync(&mut self) {
        if self.manager.is_none() {
            self.manager = lock(&self.shared.manager).take();
            if self.manager.is_some() && self.shown {
                self.hook_manager();
                if self.open.is_some() {
                    self.hook_sessions();
                    self.read_sources();
                }
            }
        }
        if !self.shown {
            return;
        }
        if self.shared.sessions_changed.swap(false, Ordering::AcqRel) {
            self.read_sources();
        }
        if self.shared.session_changed.swap(false, Ordering::AcqRel) {
            self.follow();
        }
        if self.shared.playback_changed.swap(false, Ordering::AcqRel) {
            self.read_playback();
            self.read_timeline();
        }
        if self.shared.timeline_changed.swap(false, Ordering::AcqRel) {
            self.read_timeline();
        }
        let labels = std::mem::take(&mut *lock(&self.shared.labels));
        if let Some(open) = &mut self.open {
            for (aumid, title) in labels {
                if let Some(s) = open.sources.iter_mut().find(|s| s.aumid == aumid && s.name.is_empty()) {
                    s.name = title;
                }
            }
            if self.shared.volume_changed.swap(false, Ordering::AcqRel)
                && let Some(v) = &mut open.volume
            {
                v.read();
            }
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
            (open.cover, open.palette) = match cover {
                Some(c) => (Some(c.image), c.palette),
                None => (None, None),
            };
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
        self.open = None;
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

    /// The title alone, cut shorter.
    fn compact(&self, theme: &Theme) -> Option<Pill> {
        if !self.visible() {
            return None;
        }
        let paused = !self.playback.playing();
        let (icon, title) = if paused { (PAUSE, theme.subtext) } else { (NOTE, theme.text) };
        Some(Pill::new(vec![Item::icon(icon, title), Item::text(&self.track.title, title).max_width(110.0)]).gap(6.0))
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
        if self.mock.is_some() {
            return false;
        }
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
        if let Some(current) = self.mock.as_ref().map(|m| m.current) {
            if open {
                self.open = Some(Open { app: String::new(), cover: None, palette: None, seek: None, sources: Vec::new(), pick: None, volume: None });
                self.fill_mock_popup();
            } else if self.open.take().is_some_and(|o| o.pick.is_some()) {
                // Back to the player Windows would call current
                self.play_mock(current);
            }
            self.schedule();
            return;
        }
        self.shared.cover_wanted.store(open && self.covers, Ordering::Release);
        if open {
            self.open = Some(Open {
                app: app_name(&self.aumid),
                cover: None,
                palette: None,
                seek: None,
                sources: Vec::new(),
                pick: None,
                volume: None,
            });
            self.hook_timeline();
            self.read_timeline();
            if let Some(h) = &self.hooked {
                let session = h.session.clone();
                self.shared.fetch(&session);
            }
            self.hook_sessions();
            self.read_sources();
            self.read_volume();
        } else {
            let picked = self.open.take().is_some_and(|o| o.pick.is_some());
            self.unhook_sessions();
            *lock(&self.shared.cover) = None;
            lock(&self.shared.labels).clear();
            self.shared.volume_changed.store(false, Ordering::Release);
            if let Some(h) = &mut self.hooked
                && let Some(t) = h.timeline.take()
            {
                let _ = h.session.RemoveTimelinePropertiesChanged(t);
            }
            // Back to the session Windows calls current
            if picked && self.shown {
                self.follow();
            }
        }
        self.schedule();
    }

    fn popup(&self, theme: &Theme, fonts: &Fonts) -> Option<Scene> {
        let open = self.open.as_ref()?;
        let switch = open.sources.len() >= 2;
        if self.track.title.is_empty() && !switch {
            return None;
        }
        // In the cover's colors, once they're there
        let tinted = open.palette.map(|p| p.theme(theme));
        let theme = tinted.as_ref().unwrap_or(theme);
        let position = self.position();
        let volume = open.volume.as_ref().filter(|v| !v.sessions.is_empty() || self.mock.is_some());
        let mut height = MARGIN + PAD + COVER + PAD;
        if switch {
            height += SOURCES_H + 4.0;
        }
        if position.is_some() {
            height += BAR_H + TIMES_H;
        }
        height += CONTROLS_H;
        if volume.is_some() {
            height += VOLUME_H + 4.0;
        }
        height += PAD + MARGIN - 4.0;
        let mut scene = Scene::new(WIDTH, height);
        if tinted.is_some() {
            scene.backdrop = Some(Backdrop::of(theme));
        }
        let left = MARGIN + PAD;
        let mut top = MARGIN;

        // The apps with a session: which one the player is for
        if switch {
            let row = Rect::new(MARGIN, top, INNER, SOURCES_H);
            scene.fill(row, 8.0, theme.tile(theme.primary, false));
            let w = (INNER - 4.0) / open.sources.len() as f32;
            let font = Font::new(theme.small_size, Weight::SemiBold);
            for (i, source) in open.sources.iter().enumerate() {
                let r = Rect::new(MARGIN + 2.0 + i as f32 * w, top + 2.0, w, SOURCES_H - 4.0);
                let on = source.aumid == self.aumid;
                if on {
                    scene.fill(r, 6.0, theme.primary);
                } else {
                    scene.zone(r, 6.0, format!("source:{i}"));
                }
                let name = if source.name.is_empty() { "App" } else { source.name.as_str() };
                let color = if on { theme.on_primary } else { theme.text };
                scene.text(Rect::new(r.x + 6.0, r.y, r.w - 12.0, r.h), name, font, color, Align::Center);
            }
            top += SOURCES_H + 4.0;
        }

        // The cover, or a note where it would be
        let cover = Rect::new(left, top + PAD, COVER, COVER);
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
        // Shuffle, ⏮ ⏯ ⏭, repeat
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
        // Lit in the accent when on
        let mut mode = |r: Rect, glyph: &str, on: bool, action: &str| {
            if on {
                scene.fill(r, 18.0, theme.tile(theme.primary, true));
            }
            scene.text(r, glyph, Font::icons(14.0), if on { theme.primary } else { theme.text }, Align::Center);
            scene.zone(r, 18.0, action);
        };
        if let Some(on) = self.playback.shuffle {
            mode(Rect::new(left, y + 4.0, 36.0, 36.0), SHUFFLE, on, "shuffle");
        }
        if let Some(repeat) = self.playback.repeat {
            let glyph = if repeat == Repeat::Track.0 { REPEAT_ONE } else { REPEAT_ALL };
            mode(Rect::new(MARGIN + INNER - PAD - 36.0, y + 4.0, 36.0, 36.0), glyph, repeat != Repeat::None.0, "repeat");
        }
        y += CONTROLS_H;

        // The app's own volume: the speaker (a click mutes), the slider, the percentage
        if let Some(v) = volume {
            y += 4.0;
            let percent = (v.level * 100.0).round() as u32;
            let color = if v.muted { theme.subtext } else { theme.text };
            let button = Rect::new(left - 6.0, y, 32.0, VOLUME_H);
            scene.text(button, super::audio::speaker(percent, v.muted), Font::icons(14.0), color, Align::Center);
            scene.zone(button, theme.row_radius, "mute");
            let small = Font::new(theme.small_size, Weight::Medium);
            let percent_w = fonts.width("100%", small).ceil();
            let right = MARGIN + INNER - PAD;
            scene.text(Rect::new(right - percent_w, y, percent_w, VOLUME_H), format!("{percent}%"), small, color, Align::End);
            let track = Rect::new(button.x + button.w + 8.0, y, right - percent_w - 12.0 - (button.x + button.w + 8.0), VOLUME_H);
            super::audio::slider(&mut scene, theme, track, v.level, !v.muted);
            scene.slider(Rect::new(track.x - 8.0, track.y, track.w + 16.0, track.h), "volume");
        }
        Some(scene)
    }

    fn popup_input(&mut self, input: &Input) -> Reply {
        if self.mock.is_some() {
            return self.mock_input(input);
        }
        // What doesn't go to the session: the switch and the app's volume
        match input {
            Input::Action(a) if a.starts_with("source:") => {
                let index = a.strip_prefix("source:").and_then(|i| i.parse::<usize>().ok());
                let picked = index.and_then(|i| self.open.as_ref()?.sources.get(i)).map(|s| s.aumid.clone());
                let (Some(aumid), Some(open)) = (picked, self.open.as_mut()) else { return Reply::Nothing };
                open.pick = Some(aumid);
                self.follow();
                self.schedule();
                return Reply::Redraw;
            }
            Input::Action("mute") => {
                if let Some(v) = self.open.as_mut().and_then(|o| o.volume.as_mut()) {
                    v.set_mute(!v.muted);
                    return Reply::Redraw;
                }
                return Reply::Nothing;
            }
            Input::Slide("volume", fraction) => {
                if let Some(v) = self.open.as_mut().and_then(|o| o.volume.as_mut()) {
                    v.set_level(*fraction);
                    return Reply::Redraw;
                }
                return Reply::Nothing;
            }
            _ => {}
        }
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
            // The change shows at once; the app's word corrects it
            Input::Action("shuffle") => {
                let Some(on) = self.playback.shuffle else { return Reply::Nothing };
                let _ = session.TryChangeShuffleActiveAsync(!on);
                self.playback.shuffle = Some(!on);
                Reply::Redraw
            }
            Input::Action("repeat") => {
                let Some(repeat) = self.playback.repeat else { return Reply::Nothing };
                let next = next_repeat(repeat);
                let _ = session.TryChangeAutoRepeatModeAsync(Repeat(next));
                self.playback.repeat = Some(next);
                Reply::Redraw
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

/// Repeat, one click after another: nothing, the whole list, the track, nothing again.
fn next_repeat(repeat: i32) -> i32 {
    match Repeat(repeat) {
        Repeat::None => Repeat::List.0,
        Repeat::List => Repeat::Track.0,
        _ => Repeat::None.0,
    }
}

/// Every session Windows has, in its order.
fn sessions(manager: &Manager) -> Vec<Session> {
    let Ok(list) = manager.GetSessions() else { return Vec::new() };
    (0..list.Size().unwrap_or(0)).filter_map(|i| list.GetAt(i).ok()).collect()
}

fn aumid_of(session: &Session) -> String {
    session.SourceAppUserModelId().map(|s| s.to_string()).unwrap_or_default()
}

/// Core Audio's word that an audio session of the app changed: its volume, or its state.
#[implement(IAudioSessionEvents)]
struct SessionEvents {
    shared: Arc<Shared>,
}

impl SessionEvents_Impl {
    fn poke(&self) {
        self.shared.volume_changed.store(true, Ordering::Release);
        self.shared.wake.set();
    }
}

impl IAudioSessionEvents_Impl for SessionEvents_Impl {
    fn OnDisplayNameChanged(&self, _: &PCWSTR, _: *const GUID) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnIconPathChanged(&self, _: &PCWSTR, _: *const GUID) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnSimpleVolumeChanged(&self, _: f32, _: BOOL, _: *const GUID) -> windows::core::Result<()> {
        self.poke();
        Ok(())
    }

    fn OnChannelVolumeChanged(&self, _: u32, _: *const f32, _: u32, _: *const GUID) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnGroupingParamChanged(&self, _: *const GUID, _: *const GUID) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnStateChanged(&self, _: AudioSessionState) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnSessionDisconnected(&self, _: AudioSessionDisconnectReason) -> windows::core::Result<()> {
        self.poke();
        Ok(())
    }
}

/// The app's own volume, as the volume mixer has it: its audio sessions, on every output in use.
/// They're set together and read from the first.
struct AppVolume {
    sessions: Vec<(IAudioSessionControl, ISimpleAudioVolume)>,
    events: IAudioSessionEvents,
    level: f32,
    muted: bool,
}

impl AppVolume {
    /// The audio sessions of the app behind the media session `aumid`, told by Core Audio when
    /// they change; None for an app whose id is only a code, or without Core Audio.
    fn find(aumid: &str, shared: &Arc<Shared>) -> Option<AppVolume> {
        if app_name(aumid).is_empty() {
            return None;
        }
        let events: IAudioSessionEvents = SessionEvents { shared: shared.clone() }.into();
        let mut sessions = Vec::new();
        unsafe {
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
            let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE).ok()?;
            // Whether each process is the app's, asked once
            let mut processes: HashMap<u32, bool> = HashMap::new();
            for i in 0..devices.GetCount().unwrap_or(0) {
                let Ok(device) = devices.Item(i) else { continue };
                let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
                let Ok(list) = manager.GetSessionEnumerator() else { continue };
                for j in 0..list.GetCount().unwrap_or(0) {
                    let Ok(control) = list.GetSession(j) else { continue };
                    let Ok(control2) = control.cast::<IAudioSessionControl2>() else { continue };
                    if control2.IsSystemSoundsSession() == S_OK || control.GetState().is_ok_and(|s| s == AudioSessionStateExpired) {
                        continue;
                    }
                    let Ok(pid) = control2.GetProcessId() else { continue };
                    if pid == 0 || !*processes.entry(pid).or_insert_with(|| same_app(aumid, pid)) {
                        continue;
                    }
                    let Ok(volume) = control.cast::<ISimpleAudioVolume>() else { continue };
                    let _ = control.RegisterAudioSessionNotification(&events);
                    sessions.push((control, volume));
                }
            }
        }
        let mut v = AppVolume { sessions, events, level: 0.0, muted: false };
        v.read();
        Some(v)
    }

    /// A made-up volume (see `mock`), with no audio session behind it.
    fn mock(shared: &Arc<Shared>, level: f32, muted: bool) -> AppVolume {
        AppVolume { sessions: Vec::new(), events: SessionEvents { shared: shared.clone() }.into(), level, muted }
    }

    fn read(&mut self) {
        let Some((_, volume)) = self.sessions.first() else { return };
        unsafe {
            self.level = volume.GetMasterVolume().unwrap_or(self.level);
            self.muted = volume.GetMute().map(|m| m.as_bool()).unwrap_or(self.muted);
        }
    }

    /// Sets the level; raising it takes the mute away, as Windows' sliders do.
    fn set_level(&mut self, level: f32) {
        let level = level.clamp(0.0, 1.0);
        for (_, volume) in &self.sessions {
            unsafe {
                let _ = volume.SetMasterVolume(level, std::ptr::null());
                if self.muted && level > self.level {
                    let _ = volume.SetMute(false, std::ptr::null());
                }
            }
        }
        if level > self.level {
            self.muted = false;
        }
        self.level = level;
    }

    fn set_mute(&mut self, muted: bool) {
        for (_, volume) in &self.sessions {
            unsafe {
                let _ = volume.SetMute(muted, std::ptr::null());
            }
        }
        self.muted = muted;
    }
}

impl Drop for AppVolume {
    fn drop(&mut self) {
        for (control, _) in &self.sessions {
            unsafe {
                let _ = control.UnregisterAudioSessionNotification(&self.events);
            }
        }
    }
}

/// Whether the process `pid` is the app behind the media session `aumid`: a packaged app by its
/// id, any other by its executable's name.
fn same_app(aumid: &str, pid: u32) -> bool {
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let same = if aumid.contains('!') {
            GetApplicationUserModelId(process, &mut len, Some(PWSTR(buf.as_mut_ptr()))) == ERROR_SUCCESS
                && String::from_utf16_lossy(&buf[..len.saturating_sub(1) as usize]).eq_ignore_ascii_case(aumid)
        } else {
            QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok()
                && same_executable(aumid, &String::from_utf16_lossy(&buf[..len as usize]))
        };
        let _ = CloseHandle(process);
        same
    }
}

/// Whether a desktop app's id names the executable at `path`: `Spotify.exe`, `chrome` for
/// `chrome.exe`, `MSEdge` for `msedge.exe`, a path for itself.
fn same_executable(aumid: &str, path: &str) -> bool {
    let stem = |s: &str| {
        let file = s.rsplit(['\\', '/']).next().unwrap_or(s).to_ascii_lowercase();
        file.strip_suffix(".exe").map(str::to_string).unwrap_or(file)
    };
    let id = stem(aumid);
    !id.is_empty() && id == stem(path)
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
    let name = name.rsplit(['\\', '/']).next().unwrap_or(name);
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
    let reference = properties.Thumbnail().ok()?;
    let stream = reference.OpenReadAsync().ok()?.join().ok()?;
    decode(
        |wic| unsafe {
            let source: IStream = CreateStreamOverRandomAccessStream(&stream)?;
            wic.CreateDecoderFromStream(&source, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
        },
        COVER_PX,
        COVER_PX,
    )
}

/// A made-up cover, from its file (see `mock`).
fn decode_cover_file(path: &std::path::Path) -> Option<Image> {
    decode_file(path, COVER_PX, COVER_PX)
}

/// A picture from a file, cut from its middle to the shape of `width` × `height` pixels and
/// scaled to them: a made-up cover, the image of a Lua widget.
pub fn decode_file(path: &std::path::Path, width: u32, height: u32) -> Option<Image> {
    let path = HSTRING::from(path);
    decode(|wic| unsafe { wic.CreateDecoderFromFilename(&path, None, GENERIC_READ, WICDecodeMetadataCacheOnDemand) }, width, height)
}

/// A picture from the decoder `open` makes, cut from its middle to the shape of `width` ×
/// `height` and scaled to them, in premultiplied BGRA.
fn decode(open: impl FnOnce(&IWICImagingFactory) -> windows::core::Result<IWICBitmapDecoder>, width: u32, height: u32) -> Option<Image> {
    static IDS: AtomicU64 = AtomicU64::new(1);
    let (width, height) = (width.clamp(1, 4096), height.clamp(1, 4096));
    unsafe {
        // The pool's threads may have no COM yet: WIC needs it
        let initialized = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let image = (|| -> windows::core::Result<Image> {
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let decoder = open(&wic)?;
            let frame = decoder.GetFrame(0)?;
            let (mut w, mut h) = (0u32, 0u32);
            frame.GetSize(&mut w, &mut h)?;
            // The biggest part of the picture with the shape wanted, from its middle
            let (w, h) = (w.max(1), h.max(1));
            let (cw, ch) = if w as u64 * height as u64 > h as u64 * width as u64 {
                (((h as u64 * width as u64) / height as u64).max(1) as u32, h)
            } else {
                (w, ((w as u64 * height as u64) / width as u64).max(1) as u32)
            };
            let clipper = wic.CreateBitmapClipper()?;
            let part = WICRect { X: ((w - cw) / 2) as i32, Y: ((h - ch) / 2) as i32, Width: cw as i32, Height: ch as i32 };
            clipper.Initialize(&frame, &part)?;
            let scaler = wic.CreateBitmapScaler()?;
            scaler.Initialize(&clipper, width, height, WICBitmapInterpolationModeHighQualityCubic)?;
            let converter = wic.CreateFormatConverter()?;
            converter.Initialize(&scaler, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
            let mut pixels = vec![0u8; (width * height * 4) as usize];
            converter.CopyPixels(std::ptr::null(), width * 4, &mut pixels)?;
            Ok(Image { id: IDS.fetch_add(1, Ordering::Relaxed), width, height, pixels })
        })();
        if initialized {
            CoUninitialize();
        }
        image.map_err(|e| log!("image: {e}")).ok()
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
        assert_eq!(readable(r"C:\Program Files\Player\player.exe"), "Player");
        assert_eq!(app_name("308046B0AF4A39CB"), "");
        assert_eq!(app_name("Spotify.exe"), "Spotify");
    }

    /// A desktop app's id against the executables of the audio sessions.
    #[test]
    fn apps_by_their_executable() {
        assert!(same_executable("Spotify.exe", r"C:\Users\someone\AppData\Roaming\Spotify\Spotify.exe"));
        assert!(same_executable("chrome", r"C:\Program Files\Google\Chrome\Application\chrome.exe"));
        assert!(same_executable("MSEdge", r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"));
        assert!(same_executable(r"C:\Tools\player.exe", r"C:\Tools\Player.EXE"));
        assert!(!same_executable("Spotify.exe", r"C:\Program Files\Google\Chrome\Application\chrome.exe"));
        assert!(!same_executable("", r"C:\a.exe"));
    }

    #[test]
    fn repeat_goes_round() {
        assert_eq!(next_repeat(Repeat::None.0), Repeat::List.0);
        assert_eq!(next_repeat(Repeat::List.0), Repeat::Track.0);
        assert_eq!(next_repeat(Repeat::Track.0), Repeat::None.0);
    }
}
