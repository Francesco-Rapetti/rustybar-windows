//! The app, all on the main thread: config, theme, widgets, one bar per screen, and the loop that
//! sleeps in `MsgWaitForMultipleObjectsEx` until something happens: a window message (the shell,
//! Windows' broadcasts), the foreground hook, a widget's handle (the clock's timer, the registry)
//! or the config file. Window procedures and the hook only queue an `Event`; the loop handles the
//! queue after dispatching, so nothing re-enters the app while it works. A click on a pill opens
//! its widget's popup (`popup.rs`), one at a time, closed by a click anywhere else, Esc or the
//! pill again.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Instant, SystemTime};

use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint};
use windows::Win32::Storage::FileSystem::{
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FindCloseChangeNotification, FindFirstChangeNotificationW,
    FindNextChangeNotification,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::INFINITE;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::Shell::{ABN_FULLSCREENAPP, ABN_POSCHANGED};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EVENT_SYSTEM_FOREGROUND, GetCursorPos, GetForegroundWindow, IsWindow, MSG,
    MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PM_REMOVE, PeekMessageW,
    PostQuitMessage, QS_ALLINPUT, RegisterClassExW, RegisterWindowMessageW, SPI_GETCLIENTAREAANIMATION,
    SPI_SETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetForegroundWindow, SystemParametersInfoW,
    TranslateMessage, WINDOW_EX_STYLE, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_CLOSE, WM_COPYDATA, WM_DEVICECHANGE, WM_DISPLAYCHANGE,
    WM_DWMCOLORIZATIONCOLORCHANGED, WM_ENDSESSION, WM_POWERBROADCAST, WM_QUIT, WM_SETTINGCHANGE, WM_TIMECHANGE, WNDCLASSEXW,
    WS_POPUP,
};
use windows::core::{BOOL, HSTRING, PCWSTR, Result, w};

use crate::bar::{self, Bar, Sides};
use crate::config::{self, Config, Options};
use crate::crash;
use crate::gfx::Gfx;
use crate::ipc;
use crate::log;
use crate::popup::{Anchor, Popup, Side};
use crate::process;
use crate::theme::Theme;
use crate::fit::{self, Fitted, Form, Room};
use crate::ui::{Align, Font, Item, Measured, Pill, Rect, Scene, Text, Weight};
use crate::widgets::front_app::{self, Foreground, Names};
use crate::widgets::{self, Input, Reply, Signal, Widget};

/// What window procedures and the hook queue for the loop.
pub enum Event {
    AppBar { hwnd: HWND, code: u32, on: bool },
    Dpi { hwnd: HWND, dpi: u32 },
    /// A click on a bar, `x` pixels from its left edge.
    Click { hwnd: HWND, x: i32 },
    /// The popup isn't in front anymore: the user clicked somewhere else.
    PopupInactive(HWND),
    /// The mouse over the popup, in client pixels (None: it left), and whether it clicked.
    PopupMouse { hwnd: HWND, at: Option<(i32, i32)>, click: bool },
    /// The mouse went up over the popup (the end of a drag).
    PopupRelease(HWND),
    PopupWheel { hwnd: HWND, delta: i32 },
    PopupKey { hwnd: HWND, key: u16 },
    /// A closed popup finished fading out.
    PopupGone(HWND),
    Foreground(HWND),
    Displays,
    ShellRestarted,
    ThemeChanged,
    AnimationsChanged,
    TimeChanged,
    Resume,
    DevicesChanged,
    /// `rustybar msg`: the command's words, one a line, and the window to answer.
    Command { text: String, reply: HWND },
    /// `rb.trigger` in a Lua widget: an event for the Lua widgets that listen to it.
    Trigger { name: String, args: Vec<(String, String)> },
}

/// What `rustybar msg` takes.
pub const USAGE: &str = "usage: rustybar msg <command>
  ping              answers pong if the bar runs
  reload            reads the config again
  query             the widgets, where their pills are, the screens
  popup <widget>    opens the widget's popup, or closes it if it's open
  shot <widget|bar[:left,center,right]> <file.png> [scale]
                    draws the widget's popup, or the pills, into a PNG (not the screen)
  trigger <name> [key=value…]
                    an event for the Lua widgets that listen to it
  quit              closes the bar";

thread_local! {
    static EVENTS: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
}

pub fn push(event: Event) {
    EVENTS.with(|q| q.borrow_mut().push(event));
}

fn take() -> Vec<Event> {
    EVENTS.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// The message Explorer broadcasts when it starts again (after a crash or a restart): every
/// AppBar has to register again.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

struct Slot {
    name: String,
    /// The kind and the options the widget was made with: a reload keeps it while they stay the
    /// same (see `make_widgets`).
    kind: String,
    options: toml::Table,
    /// Written in Lua: it goes, with the Lua state, when a `.lua` file changes.
    lua: bool,
    widget: Box<dyn Widget>,
}

/// The Lua state, and why some of its files didn't load (each Lua widget made from it says so).
struct LuaState {
    host: crate::lua::Host,
    problem: Option<String>,
}

pub struct App {
    config: Config,
    theme: Theme,
    text: Text,
    widgets: Vec<Slot>,
    /// The Lua state, while some widget of the layout is written in Lua.
    lua: Option<LuaState>,
    /// The pills as last measured, by widget name.
    measured: HashMap<String, Measured>,
    // The popups and the bars go before the devices they draw with
    /// The open popup, if any.
    popup: Option<Popup>,
    /// Closed popups still fading out.
    closing: Vec<Popup>,
    bars: Vec<Bar>,
    gfx: Option<Gfx>,
    control: HWND,
    hook: HWINEVENTHOOK,
    config_watch: Option<HANDLE>,
    /// Runs from a change in the config dir until the writes have settled (see `config_changed`).
    config_settle: crate::wake::Timer,
    config_stamp: Stamp,
    names: Names,
    front: Foreground,
    game_in_front: bool,
    system_animations: bool,
    dirty: bool,
    /// The open popup's scene is to make again (and to draw, if it changed).
    popup_dirty: bool,
    failures: u32,
    started: Instant,
    first_frame: bool,
}

pub fn run() {
    let started = Instant::now();
    log::init();
    log!("rustybar {} starting, config {}", env!("CARGO_PKG_VERSION"), config::file().display());
    crash::after_failure();
    crash::install(true);
    process::setup();
    let Some(_instance) = process::single_instance() else {
        log!("a bar already runs for this config dir");
        return;
    };
    // COM for the widgets that need it (Core Audio, the media sessions): a single-threaded
    // apartment, the loop pumps its messages
    if let Err(e) = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok() {
        log!("COM: {e}");
    }
    match App::new(started) {
        Ok(mut app) => app.run_loop(),
        Err(e) => log!("can't start: {e}"),
    }
    unsafe { CoUninitialize() };
    log!("quitting");
}

impl App {
    fn new(started: Instant) -> Result<App> {
        let (config, error) = config::load();
        if let Some(e) = error {
            log!("config: {e}");
        }
        let theme = Theme::from_config(&config.theme);
        let text = Text::new(&theme)?;
        let control = control_window()?;
        let hook = unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(on_winevent),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };
        let (widgets, lua, _) = make_widgets(&config, &theme, Vec::new(), None);
        let mut app = App {
            widgets,
            lua,
            measured: HashMap::new(),
            config,
            theme,
            text,
            popup: None,
            closing: Vec::new(),
            bars: Vec::new(),
            gfx: None,
            control,
            hook,
            config_watch: watch_config_dir(),
            config_settle: crate::wake::Timer::new(),
            config_stamp: config_stamp(),
            names: Names::default(),
            front: Foreground::default(),
            game_in_front: false,
            system_animations: system_animations(),
            dirty: true,
            popup_dirty: false,
            failures: 0,
            started,
            first_frame: false,
        };
        let gfx = Gfx::new()?;
        log!("drawing with {}", if gfx.hardware { "the GPU" } else { "WARP (software)" });
        app.gfx = Some(gfx);
        app.build_bars();
        app.front_changed(unsafe { GetForegroundWindow() });
        app.update_visibility();
        Ok(app)
    }

    fn run_loop(&mut self) {
        loop {
            self.frame();
            let mut handles: Vec<HANDLE> = Vec::new();
            // Each handle's owner: a widget and which of its handles, or the config dir
            let mut owners: Vec<Owner> = Vec::new();
            if let Some(h) = self.config_watch {
                handles.push(h);
                owners.push(Owner::ConfigDir);
            }
            if let Some(h) = self.config_settle.handle() {
                handles.push(h);
                owners.push(Owner::Settled);
            }
            for (i, slot) in self.widgets.iter().enumerate() {
                for (j, h) in slot.widget.handles().into_iter().enumerate() {
                    handles.push(h);
                    owners.push(Owner::Widget(i, j));
                }
            }
            let r = unsafe { MsgWaitForMultipleObjectsEx(Some(&handles), INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
            let index = r.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
            if index < handles.len() {
                match owners[index] {
                    Owner::ConfigDir => self.config_dir_changed(),
                    Owner::Settled => self.config_changed(),
                    Owner::Widget(i, j) => {
                        if self.widgets[i].widget.signaled_at(j) {
                            self.changed(i);
                        }
                    }
                }
            }
            let mut msg = MSG::default();
            while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
                if msg.message == WM_QUIT {
                    return;
                }
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            for event in take() {
                self.handle(event);
            }
        }
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Click { hwnd, x } => self.click(hwnd, x),
            Event::PopupInactive(hwnd) => {
                if self.popup.as_ref().is_some_and(|p| p.hwnd == hwnd) {
                    self.close_popup(false);
                }
            }
            Event::PopupMouse { hwnd, at, click } => self.popup_mouse(hwnd, at, click),
            Event::PopupRelease(hwnd) => {
                if let Some(action) = self.popup.as_mut().filter(|p| p.hwnd == hwnd).and_then(|p| p.release()) {
                    self.popup_input(&Input::Release(&action));
                }
            }
            Event::PopupWheel { hwnd, delta } => {
                if let Some(popup) = self.popup.as_mut().filter(|p| p.hwnd == hwnd) {
                    let notches = popup.wheel(delta);
                    if notches != 0 {
                        self.popup_input(&Input::Wheel(notches));
                    }
                }
            }
            Event::PopupKey { hwnd, key } => {
                if self.popup.as_ref().is_some_and(|p| p.hwnd == hwnd) {
                    if key == VK_ESCAPE.0 {
                        self.close_popup(true);
                    } else {
                        self.popup_input(&Input::Key(key));
                    }
                }
            }
            Event::PopupGone(hwnd) => {
                self.closing.retain(|p| p.hwnd != hwnd);
                self.trim_if_idle();
            }
            Event::Foreground(hwnd) => self.front_changed(hwnd),
            Event::AppBar { hwnd, code, on } => match code {
                ABN_POSCHANGED => {
                    if let Some(bar) = self.bars.iter_mut().find(|b| b.hwnd == hwnd) {
                        bar.place();
                    }
                }
                ABN_FULLSCREENAPP => {
                    log!("the shell says an app {} full screen", if on { "went" } else { "left" });
                    self.update_visibility();
                }
                _ => {}
            },
            Event::Dpi { hwnd, dpi } => {
                let height = self.config.bar.height;
                if let Some(bar) = self.bars.iter_mut().find(|b| b.hwnd == hwnd) {
                    bar.set_dpi(dpi, height);
                    self.dirty = true;
                }
                if self.popup.as_ref().is_some_and(|p| p.bar == hwnd) {
                    self.close_popup(false);
                }
            }
            Event::Displays | Event::ShellRestarted => {
                self.drop_popups();
                self.build_bars();
                self.update_visibility();
                self.broadcast(&Signal::Displays);
            }
            Event::ThemeChanged => {
                if self.theme.follows_windows {
                    self.reload_theme();
                }
            }
            Event::AnimationsChanged => self.system_animations = system_animations(),
            Event::TimeChanged => self.broadcast(&Signal::TimeChanged),
            Event::Resume => self.broadcast(&Signal::Resume),
            Event::DevicesChanged => self.broadcast(&Signal::DevicesChanged),
            Event::Command { text, reply } => {
                let words: Vec<&str> = text.lines().collect();
                let answer = self.command(&words);
                ipc::answer(reply, self.control, &answer);
            }
            Event::Trigger { name, args } => self.broadcast(&Signal::Trigger { name: &name, args: &args }),
        }
    }

    /// A command of `rustybar msg`, and its answer.
    fn command(&mut self, words: &[&str]) -> String {
        match words {
            ["ping"] => "pong".into(),
            // To try the restart after a failure, in a bar started with RUSTYBAR_TEST_CRASH
            ["crash"] if std::env::var_os("RUSTYBAR_TEST_CRASH").is_some() => panic!("a test crash (rustybar msg crash)"),
            // Everything from the start, as a restart would, even what didn't change
            ["reload"] => match self.reload(true, true) {
                Ok(()) => "ok".into(),
                Err(e) => e,
            },
            ["query"] => {
                // Up to date: the pills where they'll be
                self.frame();
                self.describe()
            }
            ["popup", name] => match self.toggle_popup(name) {
                Ok(()) => "ok".into(),
                Err(e) => e,
            },
            // `key=value` pairs, for the Lua widgets that listen to `name`
            ["trigger", name, pairs @ ..] => {
                let args: Vec<(String, String)> = pairs.iter().map(|p| p.split_once('=').map_or((p.to_string(), String::new()), |(k, v)| (k.to_string(), v.to_string()))).collect();
                self.broadcast(&Signal::Trigger { name, args: &args });
                "ok".into()
            }
            ["shot", target, path, rest @ ..] if rest.len() <= 1 => {
                let scale = match rest.first().map(|s| s.parse::<f32>()) {
                    Some(Ok(s)) if (0.5..=4.0).contains(&s) => Some(s),
                    Some(_) => return "the scale goes from 0.5 to 4".into(),
                    None => None,
                };
                match self.shot(target, std::path::Path::new(path), scale) {
                    Ok((w, h)) => format!("ok {w}x{h}"),
                    Err(e) => e,
                }
            }
            ["quit"] => {
                unsafe { PostQuitMessage(0) };
                "ok".into()
            }
            _ => USAGE.into(),
        }
    }

    /// `rustybar msg shot <target> <file> [scale]`: a widget's popup (or a `⋯` pill's), or with
    /// `bar` every pill in full one after the other on the theme's darkest color (`bar:right`,
    /// `bar:left,center`: those groups alone), drawn away from
    /// the screen into a PNG at `scale` (the first screen's by default). Never a capture of the
    /// screen: what's behind the bar isn't in it. Made for the made-up data (see `mock`), where
    /// everything is there at once; a widget whose popup reads something in the background shows
    /// what it has when the shot is taken.
    fn shot(&mut self, target: &str, path: &std::path::Path, scale: Option<f32>) -> std::result::Result<(u32, u32), String> {
        const PAD: f32 = 8.0;
        const GROUPS: f32 = 24.0;
        self.frame();
        let bar = self.bars.iter().position(|b| b.visible).ok_or("no bar shows")?;
        let scale = scale.unwrap_or(self.bars[bar].scale);
        let fail = |e: windows::core::Error| format!("{}: {e}", path.display());
        if let Some(groups) = target.strip_prefix("bar").filter(|g| g.is_empty() || g.starts_with(':')) {
            let sides = self.measure();
            let gap = self.theme.item_gap;
            // `bar` every group, `bar:right` or `bar:left,center` those alone
            let wanted = |name: &str| groups.is_empty() || groups[1..].split(',').any(|g| g == name);
            if !["left", "center", "right"].iter().any(|g| wanted(g)) {
                return Err("bar takes :left, :center, :right, or more of them with commas".into());
            }
            let mut places = Vec::new();
            let mut x = PAD;
            for (name, group) in [("left", &sides.left), ("center", &sides.center), ("right", &sides.right)] {
                if !wanted(name) {
                    continue;
                }
                for m in group {
                    places.push((m, (x * scale).round() / scale));
                    x += m.width + gap;
                }
                if !group.is_empty() {
                    x += GROUPS - gap;
                }
            }
            let (width, height) = (x - GROUPS + PAD, self.config.bar.height);
            let y = ((height - self.theme.pill_height) / 2.0 * scale).round() / scale;
            let theme = &self.theme;
            let gfx = self.gfx.as_ref().ok_or("no device to draw with")?;
            let size = ((width * scale).ceil() as u32, (height * scale).ceil() as u32);
            let backdrop = theme.color("crust").unwrap_or(theme.base);
            gfx.png(size.0, size.1, scale, path, |ctx, brush| {
                unsafe {
                    brush.SetColor(&crate::gfx::color(backdrop));
                    ctx.FillRectangle(&crate::gfx::rect(0.0, 0.0, width, height), brush);
                }
                for (m, x) in &places {
                    bar::paint_pill(ctx, brush, scale, theme, m, *x, y);
                }
            })
            .map_err(fail)?;
            gfx.trim();
            return Ok(size);
        }
        let fonts = crate::ui::Fonts::new(&self.theme).map_err(|e| e.to_string())?;
        let scene = match fit::more_side(target) {
            Some(side) => self.more_scene(bar, side),
            None => {
                let open = self.popup.as_ref().is_some_and(|p| p.widget == target);
                let theme = &self.theme;
                let slot = self.widgets.iter_mut().find(|s| s.name == target).ok_or_else(|| format!("unknown widget {target:?}"))?;
                if !slot.widget.has_popup() {
                    return Err(format!("{target} has no popup"));
                }
                // Opened for the shot alone, as a click would, and closed again
                if !open {
                    slot.widget.popup_shown(true);
                }
                let scene = slot.widget.popup(theme, &fonts);
                if !open {
                    slot.widget.popup_shown(false);
                    self.dirty = true;
                }
                scene
            }
        };
        let scene = scene.ok_or_else(|| format!("{target}: the popup has nothing to show"))?;
        let gfx = self.gfx.as_ref().ok_or("no device to draw with")?;
        let size = crate::popup::shot(gfx, &self.theme, &scene, &fonts, scale, path).map_err(fail)?;
        gfx.trim();
        Ok(size)
    }

    /// `rustybar msg query`: each widget with its kind, side and whether it shows a pill, then
    /// each screen with where its pills are, in screen pixels.
    fn describe(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        let layout = &self.config.layout;
        for (side, names) in [("left", &layout.left), ("center", &layout.center), ("right", &layout.right)] {
            for name in names {
                let Some(slot) = self.widgets.iter().find(|s| &s.name == name) else {
                    let _ = writeln!(out, "{name:<16} {:<12} {side:<7} unknown", "?");
                    continue;
                };
                let kind = config::widget_kind(&self.config, name);
                let shown = if slot.widget.pill(&self.theme).is_some() { "shown" } else { "hidden" };
                let popup = if self.popup.as_ref().is_some_and(|p| &p.widget == name) { "  popup open" } else { "" };
                let _ = writeln!(out, "{name:<16} {kind:<12} {side:<7} {shown}{popup}");
            }
        }
        for bar in &self.bars {
            let s = bar.screen;
            let state = if bar.visible { "shown" } else { "hidden (full screen app)" };
            let _ = writeln!(out, "screen {}x{} at {},{}, scale {:.2}: {state}", s.right - s.left, s.bottom - s.top, s.left, s.top, bar.scale);
            for (name, r) in bar.pill_rects() {
                let _ = writeln!(out, "  {name:<16} {},{} {}x{}", r.left, r.top, r.right - r.left, r.bottom - r.top);
            }
            if !bar.fit.compact.is_empty() {
                let _ = writeln!(out, "  compact: {}", bar.fit.compact.join(", "));
            }
            for (side, hidden) in fit::MORE.iter().zip(&bar.fit.hidden) {
                if !hidden.is_empty() {
                    let _ = writeln!(out, "  in {side}: {}", hidden.join(", "));
                }
            }
        }
        let _ = write!(out, "game in front: {}", if self.game_in_front { "yes" } else { "no" });
        out
    }

    /// `rustybar msg popup <name>`: opens the widget's popup under its pill on the screen with the
    /// mouse (or the first screen that shows it), or closes it if it's open.
    fn toggle_popup(&mut self, name: &str) -> std::result::Result<(), String> {
        // A `⋯` pill's popup too, by its name (`⋯left`, `⋯right`)
        if fit::more_side(name).is_none() {
            let Some(slot) = self.widgets.iter().find(|s| s.name == name) else {
                return Err(format!("unknown widget {name:?}"));
            };
            if !slot.widget.has_popup() {
                return Err(format!("{name} has no popup"));
            }
        }
        if self.popup.as_ref().is_some_and(|p| p.widget == name) {
            self.close_popup(true);
            return Ok(());
        }
        self.frame();
        let mut cursor = POINT::default();
        let here = unsafe { GetCursorPos(&mut cursor) }.ok().map(|()| unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) });
        let shows = |b: &Bar| b.visible && b.pill_span(name).is_some();
        let index = self
            .bars
            .iter()
            .position(|b| Some(b.monitor) == here && shows(b))
            .or_else(|| self.bars.iter().position(shows))
            .ok_or_else(|| format!("{name} shows no pill on any screen"))?;
        self.open_popup(name, index);
        Ok(())
    }

    fn broadcast(&mut self, signal: &Signal) {
        for i in 0..self.widgets.len() {
            if self.widgets[i].widget.event(signal) {
                self.changed(i);
            }
        }
    }

    /// The widget in slot `i` changed: its pill, and its popup if it's open.
    fn changed(&mut self, i: usize) {
        self.dirty = true;
        // A `⋯` popup may show its pill
        if self.popup.as_ref().is_some_and(|p| p.widget == self.widgets[i].name || fit::more_side(&p.widget).is_some()) {
            self.popup_dirty = true;
        }
    }

    /// A new app in front: the name for `front_app`, whether it's a listed game, and whether it
    /// covers a screen.
    fn front_changed(&mut self, hwnd: HWND) {
        if hwnd.0.is_null() {
            return;
        }
        // Another app came in front (the hook doesn't see this process's windows): the popup
        // closes, as if clicked away, even if it never got the front itself
        self.close_popup(false);
        // The taskbar and the desktop leave the app that was in front as it was
        if !bar::is_shell(hwnd)
            && let Some(front) = front_app::resolve(hwnd, &mut self.names)
            && front != self.front
        {
            let game = self.config.games.apps.iter().any(|a| a.eq_ignore_ascii_case(&front.exe));
            self.front = front.clone();
            self.broadcast(&Signal::Foreground(&front));
            if game != self.game_in_front {
                log!("{} {}", front.exe, if game { "is a listed game: no animations, timers stopped" } else { "is in front" });
                self.game_in_front = game;
                self.broadcast(&Signal::Game(game));
            }
        }
        self.update_visibility();
    }

    /// Hides the bar of each screen that has an app full screen, shows the others, and keeps
    /// the devices only while some bar shows.
    fn update_visibility(&mut self) {
        let mut changed = false;
        for bar in &mut self.bars {
            let show = !bar.fullscreen_here();
            if show != bar.visible {
                bar.set_visible(show);
                changed = true;
            }
        }
        if let Some(popup) = &self.popup
            && !self.bars.iter().any(|b| b.hwnd == popup.bar && b.visible)
        {
            self.close_popup(false);
        }
        let any = self.bars.iter().any(|b| b.visible);
        if any && self.gfx.is_none() {
            match Gfx::new() {
                Ok(gfx) => {
                    for bar in &mut self.bars {
                        if let Err(e) = bar.attach(&gfx) {
                            log!("attach: {e}");
                        }
                    }
                    self.gfx = Some(gfx);
                    log!("a bar shows again: devices back");
                }
                Err(e) => log!("devices: {e}"),
            }
        } else if !any && self.gfx.is_some() {
            self.drop_popups();
            for bar in &mut self.bars {
                bar.release();
            }
            self.gfx = None;
            log!("no bar shows: devices released");
        }
        if changed {
            self.broadcast(&Signal::Shown(any));
            self.dirty = true;
        }
    }

    /// No animations while a game is in front or full screen anywhere, or when Windows has them
    /// off.
    fn animate(&self) -> bool {
        self.theme.animations && self.system_animations && !self.game_in_front && self.bars.iter().all(|b| b.visible)
    }

    fn build_bars(&mut self) {
        self.bars.clear();
        let primary_only = self.config.bar.screens == "primary";
        for (monitor, screen, primary) in monitors() {
            if primary_only && !primary {
                continue;
            }
            let mut dpi = (96u32, 96u32);
            unsafe {
                let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi.0, &mut dpi.1);
            }
            match Bar::new(monitor, screen, dpi.0, self.config.bar.height, self.config.bar.reserve) {
                Ok(mut bar) => {
                    if let Some(gfx) = &self.gfx
                        && let Err(e) = bar.attach(gfx)
                    {
                        log!("attach: {e}");
                    }
                    self.bars.push(bar);
                }
                Err(e) => log!("bar for a screen: {e}"),
            }
        }
        log!("{} bars", self.bars.len());
        self.dirty = true;
    }

    /// Every pill measured: a pill that's the same as last time keeps its layout, so a number that
    /// changes in one widget lays out that widget's text alone.
    fn measure(&mut self) -> Sides {
        let mut cache = std::mem::take(&mut self.measured);
        let mut group = |names: &[String]| -> Vec<Measured> {
            names
                .iter()
                .filter_map(|name| {
                    let slot = self.widgets.iter().find(|s| &s.name == name)?;
                    let pill = slot.widget.pill(&self.theme)?;
                    if let Some(m) = cache.get(name).filter(|m| m.pill == pill) {
                        return Some(m.clone());
                    }
                    let m = self.text.measure(name, pill, &self.theme).map_err(|e| log!("{name}: {e}")).ok()?;
                    cache.insert(name.clone(), m.clone());
                    Some(m)
                })
                .collect()
        };
        let layout = &self.config.layout;
        let sides = Sides { left: group(&layout.left), center: group(&layout.center), right: group(&layout.right) };
        self.measured = cache;
        sides
    }

    /// What each bar shows: the pills in the forms that fit it (see `fit`), with a `⋯` pill for the
    /// widgets of a side that don't. A bar where everything fits in full, as usual on a wide
    /// screen, costs a sum; the compact pills are measured only while some bar needs them.
    fn fit_bars(&mut self, full: Sides) -> Vec<Sides> {
        let full = self.always_compact(full);
        let (margin, gap) = (self.config.bar.margin, self.theme.item_gap);
        let wide = self.measure_wide(&full);
        let fits_full = |bar: &Bar| {
            let room = room(bar.width(), &full, total(full.center.iter().map(|m| m.width), gap), margin, gap);
            bar.fit.full() && fit::fits(room, (total(full.left.iter().map(|m| m.width), gap), total(full.right.iter().map(|m| m.width), gap)), 0.0)
        };
        if self.bars.iter().all(fits_full) {
            for bar in &mut self.bars {
                let was_wide = bar.fit.center_wide;
                bar.fit = fit::Fitted::default();
                bar.fit.center_wide = was_wide;
            }
            return self.bars.iter_mut().map(|bar| widen(bar.width(), &mut bar.fit, full.clone(), &wide, margin, gap)).collect();
        }
        let compact = self.measure_compact(&full);
        let more = [self.measure_more(0), self.measure_more(1)];
        let [Some(left), Some(right)] = more else { return self.bars.iter().map(|_| full.clone()).collect() };
        let more = [left, right];
        self.bars
            .iter_mut()
            .map(|bar| {
                let was_wide = bar.fit.center_wide;
                let sides = fit_bar(bar.width(), &mut bar.fit, &full, &compact, &more, margin, gap);
                bar.fit.center_wide = was_wide;
                widen(bar.width(), &mut bar.fit, sides, &wide, margin, gap)
            })
            .collect()
    }

    /// The widgets of `[bar] always_compact` in their compact form, on every screen, as if that
    /// were their own.
    fn always_compact(&mut self, mut full: Sides) -> Sides {
        if self.config.bar.always_compact.is_empty() {
            return full;
        }
        let names = self.config.bar.always_compact.clone();
        for m in full.left.iter_mut().chain(full.center.iter_mut()).chain(full.right.iter_mut()) {
            if !names.contains(&m.key) {
                continue;
            }
            let Some(pill) = self.widgets.iter().find(|s| s.name == m.key).and_then(|s| s.widget.compact(&self.theme)) else { continue };
            let key = format!("{}\u{0}compact", m.key);
            let measured = match self.measured.get(&key).filter(|c| c.pill == pill) {
                Some(c) => c.clone(),
                None => match self.text.measure(&m.key, pill, &self.theme) {
                    Ok(c) => {
                        self.measured.insert(key, c.clone());
                        c
                    }
                    Err(_) => continue,
                },
            };
            *m = measured;
        }
        full
    }

    /// The wide pills of the center group's widgets that have one wider than their own, by name.
    fn measure_wide(&mut self, full: &Sides) -> HashMap<String, Measured> {
        let mut out = HashMap::new();
        for m in &full.center {
            let Some(slot) = self.widgets.iter().find(|s| s.name == m.key) else { continue };
            let Some(pill) = slot.widget.wide(&self.theme).filter(|p| *p != m.pill) else { continue };
            let key = format!("{}\u{0}wide", m.key);
            let measured = match self.measured.get(&key).filter(|c| c.pill == pill) {
                Some(c) => c.clone(),
                None => match self.text.measure(&m.key, pill, &self.theme) {
                    Ok(c) => {
                        self.measured.insert(key, c.clone());
                        c
                    }
                    Err(e) => {
                        log!("{}: {e}", m.key);
                        continue;
                    }
                },
            };
            if measured.width > m.width {
                out.insert(m.key.clone(), measured);
            }
        }
        out
    }

    /// The compact pills of the widgets that have one narrower than their own, by name.
    fn measure_compact(&mut self, full: &Sides) -> HashMap<String, Measured> {
        let mut out = HashMap::new();
        for m in full.left.iter().chain(&full.center).chain(&full.right) {
            let Some(slot) = self.widgets.iter().find(|s| s.name == m.key) else { continue };
            let Some(pill) = slot.widget.compact(&self.theme).filter(|p| *p != m.pill) else { continue };
            let key = format!("{}\u{0}compact", m.key);
            let measured = match self.measured.get(&key).filter(|c| c.pill == pill) {
                Some(c) => c.clone(),
                None => match self.text.measure(&m.key, pill, &self.theme) {
                    Ok(c) => {
                        self.measured.insert(key, c.clone());
                        c
                    }
                    Err(e) => {
                        log!("{}: {e}", m.key);
                        continue;
                    }
                },
            };
            if measured.width < m.width {
                out.insert(m.key.clone(), measured);
            }
        }
        out
    }

    /// The `⋯` pill of a side.
    fn measure_more(&mut self, side: usize) -> Option<Measured> {
        let name = fit::MORE[side];
        let pill = Pill::new(vec![Item::icon(MORE_ICON, self.theme.text)]);
        if let Some(m) = self.measured.get(name).filter(|m| m.pill == pill) {
            return Some(m.clone());
        }
        let m = self.text.measure(name, pill, &self.theme).map_err(|e| log!("{name}: {e}")).ok()?;
        self.measured.insert(name.to_string(), m.clone());
        Some(m)
    }

    /// The popup of a `⋯` pill: the pills of the widgets of its side that bar `bar` has no room
    /// for, in their full form, one under the other, lined up on that side. A click on one opens
    /// its popup from the `⋯` pill, or does what a click on it in the bar does.
    fn more_scene(&self, bar: usize, side: usize) -> Option<Scene> {
        const PAD: f32 = 8.0;
        let t = &self.theme;
        let pills: Vec<&Measured> = self.bars.get(bar)?.fit.hidden[side].iter().filter_map(|n| self.measured.get(n)).collect();
        if pills.is_empty() {
            return None;
        }
        let inner = pills.iter().map(|m| m.width).fold(0.0, f32::max);
        let gap = t.item_gap.max(6.0);
        let height = 2.0 * PAD + pills.len() as f32 * t.pill_height + gap * (pills.len() - 1) as f32;
        let mut scene = Scene::new(inner + 2.0 * PAD, height);
        let mut y = PAD;
        for m in pills {
            let x = if side == 0 { PAD } else { PAD + inner - m.width };
            pill_shapes(&mut scene, t, m, x, y);
            let r = Rect::new(x, y, m.width, t.pill_height);
            match self.widgets.iter().find(|s| s.name == m.key) {
                Some(slot) if slot.widget.has_popup() => scene.zone(r, t.pill_radius, format!("open:{}", m.key)),
                // Each item, half the gap on either side, as in the bar
                Some(slot) if slot.widget.clickable() => {
                    for (i, p) in m.items.iter().enumerate() {
                        let item = Rect::new(x + p.x - m.pill.gap / 2.0, y, p.width + m.pill.gap, t.pill_height);
                        scene.zone(item, t.pill_radius, format!("click:{i}:{}", m.key));
                    }
                }
                _ => {}
            }
            y += t.pill_height + gap;
        }
        Some(scene)
    }

    /// Brings every bar, and the open popup, up to date if something changed.
    fn frame(&mut self) {
        if self.dirty && self.gfx.is_some() {
            self.dirty = false;
            self.render_bars();
        }
        if self.popup_dirty && self.gfx.is_some() {
            self.popup_dirty = false;
            self.render_popup();
        }
    }

    /// Draws what changed in the bars and hands it to DWM at once.
    fn render_bars(&mut self) {
        let full = self.measure();
        let sides = self.fit_bars(full);
        let animate = self.animate();
        let Some(gfx) = &self.gfx else { return };
        let mut result = Ok(());
        for (bar, sides) in self.bars.iter_mut().zip(&sides) {
            result = result.and(bar.render(gfx, &self.theme, sides, self.config.bar.margin, animate));
        }
        // What a `⋯` popup has may have changed with the bar
        if self.popup.as_ref().is_some_and(|p| fit::more_side(&p.widget).is_some()) {
            self.popup_dirty = true;
        }
        let result = result.and(gfx.commit());
        match result {
            Ok(()) => {
                self.failures = 0;
                if !self.first_frame {
                    self.first_frame = true;
                    log!("first frame after {} ms", self.started.elapsed().as_millis());
                }
                for bar in &mut self.bars {
                    bar.reserve();
                }
            }
            Err(e) => self.recover(e),
        }
    }

    /// Drawing failed (the device went away, as after a driver update): new devices, everything
    /// drawn again. After a few failures in a row the bar stops trying until the next change.
    fn recover(&mut self, error: windows::core::Error) {
        self.failures += 1;
        log!("drawing failed ({error}), new devices (attempt {})", self.failures);
        self.drop_popups();
        for bar in &mut self.bars {
            bar.release();
        }
        self.gfx = None;
        if self.failures > 3 {
            return;
        }
        if let Ok(gfx) = Gfx::new() {
            for bar in &mut self.bars {
                let _ = bar.attach(&gfx);
            }
            self.gfx = Some(gfx);
            self.dirty = true;
        }
    }

    fn reload_theme(&mut self) {
        self.theme = Theme::from_config(&self.config.theme);
        if let Some(lua) = &self.lua {
            lua.host.set_theme(&self.theme);
        }
        match Text::new(&self.theme) {
            Ok(text) => self.text = text,
            Err(e) => log!("fonts: {e}"),
        }
        // Laid out with the old fonts
        self.measured.clear();
        self.dirty = true;
        self.popup_dirty = true;
    }

    // ---- popups ----------------------------------------------------------------------------

    /// A click on a bar: on a pill with a popup it opens it, or closes it if it's open; anywhere
    /// else it closes the open popup.
    fn click(&mut self, hwnd: HWND, x: i32) {
        let Some(index) = self.bars.iter().position(|b| b.hwnd == hwnd) else { return };
        let hit = self.bars[index].hit(x);
        if self.popup.as_ref().is_some_and(|p| p.bar == hwnd && Some(&p.widget) == hit.as_ref().map(|(name, _)| name)) {
            self.close_popup(true);
            return;
        }
        let Some((name, item)) = hit else {
            self.close_popup(true);
            return;
        };
        if fit::more_side(&name).is_some() {
            self.open_popup(&name, index);
            return;
        }
        let Some(i) = self.widgets.iter().position(|s| s.name == name) else { return };
        if self.widgets[i].widget.has_popup() {
            self.open_popup(&name, index);
        } else {
            // A pill that does something by itself (the desktops' switches the desktop)
            self.close_popup(true);
            if self.widgets[i].widget.click(item) {
                self.changed(i);
            }
        }
    }

    fn side_of(&self, name: &str) -> Side {
        let layout = &self.config.layout;
        if let Some(side) = fit::more_side(name) {
            return if side == 0 { Side::Left } else { Side::Right };
        }
        if layout.left.iter().any(|n| n == name) {
            Side::Left
        } else if layout.center.iter().any(|n| n == name) {
            Side::Center
        } else {
            Side::Right
        }
    }

    /// Opens the popup of widget `name` under its pill in bar `index`, closing the open one.
    fn open_popup(&mut self, name: &str, index: usize) {
        // From one popup to the next, the window to go back to stays the one before the first
        let previous = match self.popup.take() {
            Some(popup) => {
                let previous = popup.previous;
                self.retire(popup);
                previous
            }
            None => unsafe { GetForegroundWindow() },
        };
        let side = self.side_of(name);
        let animate = self.animate();
        let bar = &self.bars[index];
        let Some((left, right, bottom)) = bar.pill_span(name) else { return };
        let s = bar.scale;
        let anchor = Anchor {
            left,
            right,
            top: bottom + (self.config.bar.popup_gap * s).round() as i32,
            side,
            screen: bar.screen,
            margin: (self.config.bar.margin * s).round() as i32,
        };
        let bar_hwnd = bar.hwnd;
        let Some(gfx) = &self.gfx else { return };
        if let Some(side) = fit::more_side(name) {
            let opened = Popup::new(name, bar_hwnd, previous, anchor, s, &self.theme).and_then(|mut popup| match self.more_scene(index, side) {
                Some(scene) => {
                    popup.render(gfx, &self.theme, scene)?;
                    popup.show(gfx, animate)?;
                    Ok(Some(popup))
                }
                None => Ok(None),
            });
            match opened {
                Ok(popup) => self.popup = popup,
                Err(e) => log!("{name}: popup: {e}"),
            }
            return;
        }
        let Some(slot) = self.widgets.iter_mut().find(|s| s.name == name) else { return };
        let theme = &self.theme;
        slot.widget.popup_shown(true);
        let opened = Popup::new(name, bar_hwnd, previous, anchor, s, theme).and_then(|mut popup| match slot.widget.popup(theme, &popup.fonts) {
            Some(scene) => {
                popup.render(gfx, theme, scene)?;
                popup.show(gfx, animate)?;
                Ok(Some(popup))
            }
            None => Ok(None),
        });
        match opened {
            Ok(Some(popup)) => self.popup = Some(popup),
            Ok(None) => slot.widget.popup_shown(false),
            Err(e) => {
                log!("{name}: popup: {e}");
                slot.widget.popup_shown(false);
            }
        }
    }

    /// Closes the open popup. With `restore` the window that was in front before comes back, if
    /// the popup still is in front (the user didn't go somewhere else meanwhile).
    fn close_popup(&mut self, restore: bool) {
        let Some(popup) = self.popup.take() else { return };
        if restore {
            unsafe {
                if GetForegroundWindow() == popup.hwnd && IsWindow(Some(popup.previous)).as_bool() {
                    let _ = SetForegroundWindow(popup.previous);
                }
            }
        }
        self.retire(popup);
        // Gone at once, without a fade: nothing will come to say so
        self.trim_if_idle();
    }

    /// With no popup left, not even fading, the devices let go of what the popups left cached,
    /// and the heaps give the freed pages back to Windows.
    fn trim_if_idle(&self) {
        if self.popup.is_none()
            && self.closing.is_empty()
            && let Some(gfx) = &self.gfx
        {
            gfx.trim();
            process::release_memory();
        }
    }

    /// A popup that closed: its widget lets go of what only the popup needed, and the window
    /// fades out (and goes at `PopupGone`) or goes at once.
    fn retire(&mut self, popup: Popup) {
        if let Some(slot) = self.widgets.iter_mut().find(|s| s.name == popup.widget) {
            slot.widget.popup_shown(false);
        }
        if self.animate()
            && let Some(gfx) = &self.gfx
            && popup.fade_out(gfx).and_then(|()| gfx.commit()).is_ok()
        {
            self.closing.push(popup);
        }
    }

    /// Every popup goes at once, open or fading: before the bars or the devices go.
    fn drop_popups(&mut self) {
        if let Some(popup) = self.popup.take()
            && let Some(slot) = self.widgets.iter_mut().find(|s| s.name == popup.widget)
        {
            slot.widget.popup_shown(false);
        }
        self.closing.clear();
    }

    /// Makes the open popup's scene again and draws it if it changed; None from the widget
    /// closes the popup.
    fn render_popup(&mut self) {
        let Some(popup) = &self.popup else { return };
        let scene = match fit::more_side(&popup.widget) {
            Some(side) => {
                let bar = self.bars.iter().position(|b| b.hwnd == popup.bar);
                bar.and_then(|b| self.more_scene(b, side))
            }
            None => {
                let Some(slot) = self.widgets.iter().find(|s| s.name == popup.widget) else { return };
                slot.widget.popup(&self.theme, &popup.fonts)
            }
        };
        let Some(popup) = &mut self.popup else { return };
        let Some(gfx) = &self.gfx else { return };
        let result = match scene {
            Some(scene) => popup.render(gfx, &self.theme, scene).and_then(|()| gfx.commit()),
            None => {
                self.close_popup(true);
                return;
            }
        };
        if let Err(e) = result {
            self.recover(e);
        }
    }

    /// The mouse over the popup: a click runs the action of the zone under it, a move takes the
    /// hover tint along.
    fn popup_mouse(&mut self, hwnd: HWND, at: Option<(i32, i32)>, click: bool) {
        let Some(popup) = self.popup.as_mut().filter(|p| p.hwnd == hwnd) else { return };
        if click {
            match at.and_then(|at| popup.press(at)) {
                Some((action, None)) => self.popup_input(&Input::Action(&action)),
                Some((action, Some(fraction))) => self.popup_input(&Input::Slide(&action, fraction)),
                None => {}
            }
            return;
        }
        if let Some((action, fraction)) = at.and_then(|at| popup.drag_to(at)) {
            self.popup_input(&Input::Slide(&action, fraction));
            return;
        }
        let Some(gfx) = &self.gfx else { return };
        match popup.mouse(gfx, &self.theme, at) {
            Ok(true) => {
                if let Err(e) = gfx.commit() {
                    log!("popup: {e}");
                }
            }
            Ok(false) => {}
            Err(e) => log!("popup hover: {e}"),
        }
    }

    /// Hands an input to the open popup's widget and does what it replies.
    fn popup_input(&mut self, input: &Input) {
        let Some(name) = self.popup.as_ref().map(|p| p.widget.clone()) else { return };
        if fit::more_side(&name).is_some() {
            self.more_input(input);
            return;
        }
        let Some(slot) = self.widgets.iter_mut().find(|s| s.name == name) else { return };
        match slot.widget.popup_input(input) {
            Reply::Nothing => {}
            Reply::Redraw => {
                // The pill may show the change too
                self.dirty = true;
                self.popup_dirty = true;
            }
            Reply::Close => self.close_popup(true),
            Reply::Opened => self.close_popup(false),
        }
    }

    /// A click in a `⋯` popup: on a widget with a popup, its popup, from the `⋯` pill; on one
    /// without, what a click on its pill in the bar does.
    fn more_input(&mut self, input: &Input) {
        let Input::Action(action) = input else { return };
        let Some(index) = self.popup.as_ref().and_then(|p| self.bars.iter().position(|b| b.hwnd == p.bar)) else { return };
        if let Some(widget) = action.strip_prefix("open:") {
            let widget = widget.to_string();
            self.open_popup(&widget, index);
        } else if let Some((item, widget)) = action.strip_prefix("click:").and_then(|r| r.split_once(':')) {
            let (Ok(item), widget) = (item.parse::<usize>(), widget.to_string()) else { return };
            self.close_popup(true);
            if let Some(i) = self.widgets.iter().position(|s| s.name == widget)
                && self.widgets[i].widget.click(Some(item))
            {
                self.changed(i);
            }
        }
    }

    /// Something changed in the config dir: the files are read once the writes have settled, a
    /// moment later, so that a file being written (by the settings window, by an editor) is never
    /// read half done.
    fn config_dir_changed(&mut self) {
        if let Some(h) = self.config_watch {
            unsafe {
                let _ = FindNextChangeNotification(h);
            }
        }
        self.config_settle.start(SETTLE_MS, 0);
    }

    /// The config dir is quiet again: if a config file or a Lua file changed, the bar follows it
    /// (see `reload`).
    fn config_changed(&mut self) {
        self.config_settle.stop();
        let stamp = config_stamp();
        if stamp == self.config_stamp {
            return;
        }
        let lua_changed = stamp.1 != self.config_stamp.1;
        self.config_stamp = stamp;
        let _ = self.reload(false, lua_changed);
    }

    /// The bar follows the config files again. Only what changed starts again: a widget whose
    /// kind and options are the same keeps running as it was (no new read, connection or child
    /// process), the Lua state stays unless `lua_changed` (a `.lua` file changed), the theme and
    /// the bars are made again only when `[theme]` or `[bar]` changed. With `full` everything
    /// starts again, as at start-up. A config that can't be read leaves the bar as it was.
    fn reload(&mut self, full: bool, lua_changed: bool) -> std::result::Result<(), String> {
        let (config, error) = config::load();
        if let Some(e) = error {
            log!("config: {e}");
            return Err(e);
        }
        self.drop_popups();
        let bars_changed = config.bar != self.config.bar;
        let theme_changed = full || config.theme != self.config.theme;
        self.config = config;
        // What goes, goes before the new comes: the Lua widgets with their state
        if full {
            self.widgets.clear();
        } else if lua_changed {
            self.widgets.retain(|s| !s.lua);
        }
        if full || lua_changed {
            self.lua = None;
        }
        if theme_changed {
            self.reload_theme();
        }
        let old = std::mem::take(&mut self.widgets);
        let (widgets, lua, fresh) = make_widgets(&self.config, &self.theme, old, self.lua.take());
        (self.widgets, self.lua) = (widgets, lua);
        log!(
            "config reloaded: {} of {} widgets made again{}",
            fresh.len(),
            self.widgets.len(),
            if fresh.is_empty() { String::new() } else { format!(" ({})", fresh.iter().map(|&i| self.widgets[i].name.as_str()).collect::<Vec<_>>().join(", ")) }
        );
        // The new widgets start as if a bar showed and no game was in front: they hear how
        // things are; the kept ones only what changed for them
        let front = self.front.clone();
        let game = self.config.games.apps.iter().any(|a| a.eq_ignore_ascii_case(&front.exe));
        let shown = self.bars.iter().any(|b| b.visible);
        let kept_hear_game = game != self.game_in_front;
        self.game_in_front = game;
        for i in 0..self.widgets.len() {
            let new = fresh.contains(&i);
            let w = &mut self.widgets[i].widget;
            let mut changed = false;
            if new {
                changed |= w.event(&Signal::Foreground(&front));
                if !shown {
                    changed |= w.event(&Signal::Shown(false));
                }
            }
            if (new && game) || (!new && kept_hear_game) {
                changed |= w.event(&Signal::Game(game));
            }
            if changed || new {
                self.changed(i);
            }
        }
        if bars_changed {
            self.build_bars();
        } else if theme_changed {
            // Every pill drawn again, in the new colors and fonts
            for bar in &mut self.bars {
                if let Some(gfx) = &self.gfx {
                    let _ = bar.attach(gfx);
                }
            }
        }
        self.update_visibility();
        self.dirty = true;
        Ok(())
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.drop_popups();
        self.bars.clear();
        self.gfx = None;
        unsafe {
            let _ = UnhookWinEvent(self.hook);
            if let Some(h) = self.config_watch {
                let _ = FindCloseChangeNotification(h);
            }
            let _ = DestroyWindow(self.control);
        }
    }
}

/// Who a handle the loop waits on belongs to.
#[derive(Clone, Copy)]
enum Owner {
    /// The watch of the config dir.
    ConfigDir,
    /// The config dir has been quiet since its last change.
    Settled,
    /// A widget, and which of its handles.
    Widget(usize, usize),
}

/// How long the config dir stays quiet before the files are read, in ms.
const SETTLE_MS: i64 = 150;

/// The `⋯` pill's icon (Segoe Fluent Icons' «More»).
const MORE_ICON: &str = "\u{E712}";

/// How wide pills are side by side, `gap` apart.
fn total(widths: impl Iterator<Item = f32>, gap: f32) -> f32 {
    let (n, sum) = widths.fold((0usize, 0.0), |(n, s), w| (n + 1, s + w));
    sum + gap * n.saturating_sub(1) as f32
}

/// The room the sides have in a bar `width` DIPs wide: each its own beside the center group,
/// `center` wide, or together without one.
fn room(width: f32, sides: &Sides, center: f32, margin: f32, gap: f32) -> Room {
    if sides.center.is_empty() {
        Room::Shared(width - 2.0 * margin - gap)
    } else {
        let side = (width - center) / 2.0 - margin - gap;
        Room::Split { left: side, right: side }
    }
}

/// The center group in its wide form (see `Widget::wide`), when every pill keeps its full form
/// beside it: with `SLACK` to spare the first time, so that it doesn't flip at the limit. `state`
/// says whether it was wide (and says it for the next time).
fn widen(width: f32, state: &mut Fitted, mut sides: Sides, wide: &HashMap<String, Measured>, margin: f32, gap: f32) -> Sides {
    let was_wide = std::mem::take(&mut state.center_wide);
    if !state.full() || !sides.center.iter().any(|m| wide.contains_key(&m.key)) {
        return sides;
    }
    let center = total(sides.center.iter().map(|m| wide.get(&m.key).unwrap_or(m).width), gap);
    let lr = (total(sides.left.iter().map(|m| m.width), gap), total(sides.right.iter().map(|m| m.width), gap));
    if fit::fits(room(width, &sides, center, margin, gap), lr, if was_wide { 0.0 } else { fit::SLACK }) {
        for m in &mut sides.center {
            if let Some(w) = wide.get(&m.key) {
                *m = w.clone();
            }
        }
        state.center_wide = true;
    }
    sides
}

/// The pills of a bar `width` DIPs wide in the forms that fit it: the center group turns compact
/// first, when the sides don't fit beside it in full; then the sides give way (see `fit`). `state`
/// is the bar's last fit, and becomes this one.
fn fit_bar(width: f32, state: &mut Fitted, full: &Sides, compact: &HashMap<String, Measured>, more: &[Measured; 2], margin: f32, gap: f32) -> Sides {
    // A widget's compact pill, or its own if it has none
    fn small<'a>(compact: &'a HashMap<String, Measured>, m: &'a Measured) -> &'a Measured {
        compact.get(&m.key).unwrap_or(m)
    }
    let center = |c: bool| total(full.center.iter().map(|m| if c { small(compact, m).width } else { m.width }), gap);
    let sides_full = (total(full.left.iter().map(|m| m.width), gap), total(full.right.iter().map(|m| m.width), gap));
    let slack = if state.center_compact { fit::SLACK } else { 0.0 };
    let center_compact =
        full.center.iter().any(|m| compact.contains_key(&m.key)) && !fit::fits(room(width, full, center(false), margin, gap), sides_full, slack);
    let room = room(width, full, center(center_compact), margin, gap);

    let pills: Vec<(bool, &Measured)> = full.left.iter().map(|m| (true, m)).chain(full.right.iter().map(|m| (false, m))).collect();
    let (lefts, rights) = (full.left.len(), full.right.len());
    let items: Vec<fit::Item> = pills
        .iter()
        .enumerate()
        .map(|(i, &(left, m))| fit::Item { left, rank: if left { i } else { rights - 1 - (i - lefts) }, compact: compact.contains_key(&m.key) })
        .collect();
    let measure = |forms: &[Form]| {
        // Each side: how many pills, how wide, and whether one went to its `⋯`
        let mut sides = [(0usize, 0f32, false); 2];
        for (&(left, m), form) in pills.iter().zip(forms) {
            let s = &mut sides[usize::from(!left)];
            match form {
                Form::Full => (s.0, s.1) = (s.0 + 1, s.1 + m.width),
                Form::Compact => (s.0, s.1) = (s.0 + 1, s.1 + small(compact, m).width),
                Form::Hidden => s.2 = true,
            }
        }
        let side = |k: usize| {
            let (n, w, hidden) = sides[k];
            let (n, w) = if hidden { (n + 1, w + more[k].width) } else { (n, w) };
            w + gap * n.saturating_sub(1) as f32
        };
        (side(0), side(1))
    };
    let (levels, forms) = fit::fit(&items, room, &state.levels, measure);

    let mut out = Sides { left: Vec::new(), center: Vec::new(), right: Vec::new() };
    let mut next = Fitted { levels, center_compact, ..Fitted::default() };
    for (&(left, m), form) in pills.iter().zip(&forms) {
        let (list, k) = if left { (&mut out.left, 0) } else { (&mut out.right, 1) };
        match form {
            Form::Full => list.push(m.clone()),
            Form::Compact => {
                list.push(small(compact, m).clone());
                next.compact.push(m.key.clone());
            }
            Form::Hidden => next.hidden[k].push(m.key.clone()),
        }
    }
    // A `⋯` at the inner end of its side, where the widgets that went were
    if !next.hidden[0].is_empty() {
        out.left.push(more[0].clone());
    }
    if !next.hidden[1].is_empty() {
        out.right.insert(0, more[1].clone());
    }
    for m in &full.center {
        if center_compact && compact.contains_key(&m.key) {
            out.center.push(small(compact, m).clone());
            next.compact.push(m.key.clone());
        } else {
            out.center.push(m.clone());
        }
    }
    *state = next;
    out
}

/// A pill as the bar draws it, at `x`, `y` in a popup's scene: background and border, the
/// highlight, the text.
fn pill_shapes(scene: &mut Scene, t: &Theme, m: &Measured, x: f32, y: f32) {
    let b = t.pill_border;
    // A bare pill has its items alone, as on the bar
    if !m.pill.bare && b > 0.0 {
        scene.fill(Rect::new(x, y, m.width, t.pill_height), t.pill_radius, t.surface);
        scene.fill(Rect::new(x + b, y + b, m.width - 2.0 * b, t.pill_height - 2.0 * b), (t.pill_radius - b).max(0.0), t.base);
    } else if !m.pill.bare {
        scene.fill(Rect::new(x, y, m.width, t.pill_height), t.pill_radius, t.base);
    }
    if let Some(p) = m.pill.highlight.and_then(|i| m.items.get(i)) {
        let h = t.pill_height - 2.0 * bar::HIGHLIGHT_Y;
        scene.fill(Rect::new(x + p.x - bar::HIGHLIGHT_X, y + bar::HIGHLIGHT_Y, p.width + 2.0 * bar::HIGHLIGHT_X, h), h / 2.0, t.primary);
    }
    for (item, p) in m.pill.items.iter().zip(&m.items) {
        if let Some(image) = &item.image {
            scene.image(Rect::new(x + p.x, y + (t.pill_height - p.width) / 2.0, p.width, p.width), 4.0, image.clone());
            continue;
        }
        if let Some(drawn) = item.drawn {
            let (track, radius, fill) = crate::ui::drawn(drawn, x + p.x, y, p.width, t.pill_height, t.font_size, 1.0);
            scene.fill(track, radius, item.color);
            if let (Some(f), crate::ui::Drawn::Gauge(_, color)) = (fill, drawn) {
                scene.fill(f, radius, color);
            }
            continue;
        }
        if item.logo {
            for r in crate::ui::logo(x + p.x + p.text_x, y, p.width - p.text_x, t.pill_height, t.font_size, 1.0) {
                scene.fill(r, 0.0, item.color);
            }
            continue;
        }
        let font = if item.icon {
            Font::icons(t.font_size + 1.0)
        } else if item.small {
            Font::new(t.small_size, Weight::Medium)
        } else {
            Font::new(t.font_size, Weight::SemiBold)
        };
        scene.text(Rect::new(x + p.x + p.text_x, y, p.width - p.text_x + 1.0, t.pill_height), item.text.as_str(), font, item.color, Align::Start);
    }
}

/// The widgets of `[layout]`, and the Lua state when one of them is written in Lua: made only
/// when the layout names a widget no built-in has and `widgets\*.lua` files are there. A widget
/// of `old` with the same name, kind and options is kept as it is (and so is `lua`, the Lua state
/// of the kept Lua widgets); the others of `old` go before any new one is made. Also returns
/// where the widgets made anew are in the list.
fn make_widgets(config: &Config, theme: &Theme, mut old: Vec<Slot>, lua: Option<LuaState>) -> (Vec<Slot>, Option<LuaState>, Vec<usize>) {
    let mut names: Vec<&String> = Vec::new();
    for name in config.layout.left.iter().chain(&config.layout.center).chain(&config.layout.right) {
        if names.contains(&name) {
            log!("{name} is in [layout] twice: shown once");
        } else {
            names.push(name);
        }
    }
    let wanted = |name: &str| (config::widget_kind(config, name), config.widgets.get(name).cloned().unwrap_or_default());
    // What stays, taken out of `old` by name; the rest goes now
    let mut kept: HashMap<String, Slot> = HashMap::new();
    for slot in old.drain(..) {
        let (kind, options) = wanted(&slot.name);
        if names.contains(&&slot.name) && slot.kind == kind && slot.options == options {
            kept.insert(slot.name.clone(), slot);
        }
    }
    drop(old);
    let needs_lua = names.iter().any(|n| !kept.contains_key(n.as_str()) && !widgets::KINDS.contains(&config::widget_kind(config, n).as_str()));
    let lua = match lua {
        Some(state) => Some(state),
        None => needs_lua.then(|| crate::lua::Host::load(&config::dir(), theme)).flatten().map(|(host, errors)| {
            for e in &errors {
                log!("{e}");
            }
            // A widget whose file didn't load says why
            LuaState { host, problem: (!errors.is_empty()).then(|| errors.join("\n")) }
        }),
    };
    let mut slots: Vec<Slot> = Vec::new();
    let mut fresh = Vec::new();
    for name in names {
        if let Some(slot) = kept.remove(name.as_str()) {
            slots.push(slot);
            continue;
        }
        let (kind, table) = wanted(name);
        let options = Options(table.clone());
        let (widget, is_lua): (Box<dyn Widget>, bool) = match widgets::create(&kind, &options) {
            Some(widget) => (widget, false),
            None if lua.is_some() => {
                let state = lua.as_ref().expect("checked");
                (Box::new(widgets::lua::LuaWidget::new(Some(&state.host), name, &kind, &options, state.problem.clone())), true)
            }
            None => {
                let hint = config::closest(&kind, widgets::KINDS.iter().copied()).map(|n| format!(", maybe {n:?}")).unwrap_or_default();
                log!("unknown widget {name:?}{hint} (kinds: {}, or a Lua widget in widgets\\*.lua)", widgets::KINDS.join(", "));
                continue;
            }
        };
        fresh.push(slots.len());
        slots.push(Slot { name: name.clone(), kind, options: table, lua: is_lua, widget });
    }
    let lua_used = slots.iter().any(|s| s.lua);
    (slots, lua.filter(|_| lua_used), fresh)
}

/// The screens: each with its rectangle, and whether it's the main one.
fn monitors() -> Vec<(HMONITOR, RECT, bool)> {
    unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let list = unsafe { &mut *(data.0 as *mut Vec<(HMONITOR, RECT, bool)>) };
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            // MONITORINFOF_PRIMARY
            list.push((monitor, info.rcMonitor, info.dwFlags & 1 != 0));
        }
        BOOL(1)
    }
    let mut list: Vec<(HMONITOR, RECT, bool)> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut list as *mut _ as isize));
    }
    list
}

fn system_animations() -> bool {
    let mut on = BOOL(1);
    let read = unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some(&mut on as *mut BOOL as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0))
    };
    read.is_err() || on.as_bool()
}

/// The config dir and what's under it: the config files, and the Lua widgets in `widgets\`.
fn watch_config_dir() -> Option<HANDLE> {
    let dir = config::dir();
    let _ = std::fs::create_dir_all(&dir);
    let wide = HSTRING::from(dir.to_string_lossy().as_ref());
    match unsafe { FindFirstChangeNotificationW(&wide, true, FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_FILE_NAME) } {
        Ok(h) => Some(h),
        Err(e) => {
            log!("can't watch {}: {e}", dir.display());
            None
        }
    }
}

/// When the config files were written, and how long they are (`rustybar.toml` and
/// `rustybar.local.toml`), and the same of the Lua files of `widgets\` (`lib\` too). The dir's
/// watch fires for any file under it; this tells theirs apart.
type Stamp = ([Option<(SystemTime, u64)>; 2], Vec<(std::path::PathBuf, SystemTime, u64)>);

fn config_stamp() -> Stamp {
    let stamp = |path: &std::path::Path| {
        let m = std::fs::metadata(path).ok()?;
        Some((m.modified().ok()?, m.len()))
    };
    let lib = config::dir().join("widgets").join("lib");
    let lua: Vec<_> = crate::lua::files(&config::dir())
        .into_iter()
        .chain(crate::lua::files_in(&lib))
        .filter_map(|p| stamp(&p).map(|(t, l)| (p, t, l)))
        .collect();
    ([stamp(&config::file()), stamp(&config::local_file())], lua)
}

unsafe extern "system" fn on_winevent(_: HWINEVENTHOOK, event: u32, hwnd: HWND, _: i32, _: i32, _: u32, _: u32) {
    if event == EVENT_SYSTEM_FOREGROUND {
        push(Event::Foreground(hwnd));
    }
}

unsafe extern "system" fn control_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_DISPLAYCHANGE => push(Event::Displays),
        WM_SETTINGCHANGE => {
            let area = if lparam.0 != 0 { unsafe { PCWSTR(lparam.0 as *const u16).to_string() }.unwrap_or_default() } else { String::new() };
            if wparam.0 as u32 == SPI_SETCLIENTAREAANIMATION.0 {
                push(Event::AnimationsChanged);
            } else if area == "ImmersiveColorSet" {
                push(Event::ThemeChanged);
            } else if area == "intl" {
                push(Event::TimeChanged);
            }
        }
        WM_DWMCOLORIZATIONCOLORCHANGED => push(Event::ThemeChanged),
        WM_TIMECHANGE => push(Event::TimeChanged),
        // DBT_DEVNODES_CHANGED
        WM_DEVICECHANGE if wparam.0 == 0x0007 => push(Event::DevicesChanged),
        WM_POWERBROADCAST => {
            if wparam.0 as u32 == PBT_APMRESUMEAUTOMATIC || wparam.0 as u32 == PBT_APMRESUMESUSPEND {
                push(Event::Resume);
            }
            return LRESULT(1);
        }
        WM_COPYDATA => {
            // `rustybar msg`: wparam is the window to answer
            return match unsafe { ipc::read(lparam) } {
                Some(text) => {
                    push(Event::Command { text, reply: HWND(wparam.0 as *mut c_void) });
                    LRESULT(1)
                }
                None => LRESULT(0),
            };
        }
        WM_CLOSE => unsafe { PostQuitMessage(0) },
        WM_ENDSESSION if wparam.0 != 0 => unsafe { PostQuitMessage(0) },
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => push(Event::ShellRestarted),
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    LRESULT(0)
}

/// The hidden window that receives what Windows broadcasts to every top-level window: display,
/// theme, time and power changes, Explorer starting again, and `rustybar quit`.
fn control_window() -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(control_proc),
            hInstance: instance.into(),
            lpszClassName: process::CONTROL_CLASS,
            ..Default::default()
        };
        RegisterClassExW(&class);
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Relaxed);
        CreateWindowExW(WINDOW_EX_STYLE(0), process::CONTROL_CLASS, &process::instance_title(), WS_POPUP, 0, 0, 0, 0, None, None, Some(instance.into()), None)
    }
}
