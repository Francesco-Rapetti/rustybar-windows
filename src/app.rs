//! The app, all on the main thread: config, theme, widgets, one bar per screen, and the loop that
//! sleeps in `MsgWaitForMultipleObjectsEx` until something happens: a window message (the shell,
//! Windows' broadcasts), the foreground hook, a widget's handle (the clock's timer, the registry)
//! or the config file. Window procedures and the hook only queue an `Event`; the loop handles the
//! queue after dispatching, so nothing re-enters the app while it works.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Instant, SystemTime};

use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, RECT, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows::Win32::Storage::FileSystem::{
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FindCloseChangeNotification, FindFirstChangeNotificationW,
    FindNextChangeNotification,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::INFINITE;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Shell::{ABN_FULLSCREENAPP, ABN_POSCHANGED};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EVENT_SYSTEM_FOREGROUND, GetForegroundWindow, MSG,
    MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PM_REMOVE, PeekMessageW,
    PostQuitMessage, QS_ALLINPUT, RegisterClassExW, RegisterWindowMessageW, SPI_GETCLIENTAREAANIMATION,
    SPI_SETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW, TranslateMessage, WINDOW_EX_STYLE,
    WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_CLOSE, WM_DISPLAYCHANGE, WM_DWMCOLORIZATIONCOLORCHANGED, WM_ENDSESSION,
    WM_POWERBROADCAST, WM_QUIT, WM_SETTINGCHANGE, WM_TIMECHANGE, WNDCLASSEXW, WS_POPUP,
};
use windows::core::{BOOL, HSTRING, PCWSTR, Result, w};

use crate::bar::{self, Bar, Sides};
use crate::config::{self, Config, Options};
use crate::gfx::Gfx;
use crate::log;
use crate::process;
use crate::theme::Theme;
use crate::ui::{Measured, Text};
use crate::widgets::front_app::{self, Foreground, Names};
use crate::widgets::{self, Signal, Widget};

/// What window procedures and the hook queue for the loop.
pub enum Event {
    AppBar { hwnd: HWND, code: u32, on: bool },
    Dpi { hwnd: HWND, dpi: u32 },
    Foreground(HWND),
    Displays,
    ShellRestarted,
    ThemeChanged,
    AnimationsChanged,
    TimeChanged,
    Resume,
}

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
    widget: Box<dyn Widget>,
}

pub struct App {
    config: Config,
    theme: Theme,
    text: Text,
    widgets: Vec<Slot>,
    // The bars go before the devices they draw with
    bars: Vec<Bar>,
    gfx: Option<Gfx>,
    control: HWND,
    hook: HWINEVENTHOOK,
    config_watch: Option<HANDLE>,
    config_stamp: Option<(SystemTime, u64)>,
    names: Names,
    front: Foreground,
    game_in_front: bool,
    system_animations: bool,
    dirty: bool,
    failures: u32,
    started: Instant,
    first_frame: bool,
}

pub fn run() {
    let started = Instant::now();
    log::init();
    log!("rustybar {} starting, config {}", env!("CARGO_PKG_VERSION"), config::file().display());
    process::setup();
    let Some(_instance) = process::single_instance() else {
        log!("a bar already runs for this config dir");
        return;
    };
    let mut app = match App::new(started) {
        Ok(app) => app,
        Err(e) => {
            log!("can't start: {e}");
            return;
        }
    };
    app.run_loop();
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
        let mut app = App {
            widgets: make_widgets(&config),
            config,
            theme,
            text,
            bars: Vec::new(),
            gfx: None,
            control,
            hook,
            config_watch: watch_config_dir(),
            config_stamp: config_stamp(),
            names: Names::default(),
            front: Foreground::default(),
            game_in_front: false,
            system_animations: system_animations(),
            dirty: true,
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
            let mut owners: Vec<Option<usize>> = Vec::new();
            if let Some(h) = self.config_watch {
                handles.push(h);
                owners.push(None);
            }
            for (i, slot) in self.widgets.iter().enumerate() {
                if let Some(h) = slot.widget.handle() {
                    handles.push(h);
                    owners.push(Some(i));
                }
            }
            let r = unsafe { MsgWaitForMultipleObjectsEx(Some(&handles), INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
            let index = r.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
            if index < handles.len() {
                match owners[index] {
                    None => self.config_changed(),
                    Some(i) => {
                        if self.widgets[i].widget.signaled() {
                            self.dirty = true;
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
            }
            Event::Displays | Event::ShellRestarted => {
                self.build_bars();
                self.update_visibility();
            }
            Event::ThemeChanged => {
                if self.theme.follows_windows {
                    self.reload_theme();
                }
            }
            Event::AnimationsChanged => self.system_animations = system_animations(),
            Event::TimeChanged => self.broadcast(&Signal::TimeChanged),
            Event::Resume => self.broadcast(&Signal::Resume),
        }
    }

    fn broadcast(&mut self, signal: &Signal) {
        for slot in &mut self.widgets {
            if slot.widget.event(signal) {
                self.dirty = true;
            }
        }
    }

    /// A new app in front: the name for `front_app`, whether it's a listed game, and whether it
    /// covers a screen.
    fn front_changed(&mut self, hwnd: HWND) {
        if hwnd.0.is_null() {
            return;
        }
        // The taskbar and the desktop leave the app that was in front as it was
        if !bar::is_shell(hwnd)
            && let Some(front) = front_app::resolve(hwnd, &mut self.names)
            && front != self.front
        {
            let game = self.config.games.apps.iter().any(|a| a.eq_ignore_ascii_case(&front.exe));
            if game != self.game_in_front {
                log!("{} {}", front.exe, if game { "is a listed game: no animations" } else { "is in front" });
                self.game_in_front = game;
            }
            self.front = front.clone();
            self.broadcast(&Signal::Foreground(&front));
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
        for (monitor, screen) in monitors() {
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

    fn measure(&self) -> Sides {
        let group = |names: &[String]| -> Vec<Measured> {
            names
                .iter()
                .filter_map(|name| {
                    let slot = self.widgets.iter().find(|s| &s.name == name)?;
                    let pill = slot.widget.pill(&self.theme)?;
                    self.text.measure(name, pill, &self.theme).map_err(|e| log!("{name}: {e}")).ok()
                })
                .collect()
        };
        Sides { left: group(&self.config.layout.left), center: group(&self.config.layout.center), right: group(&self.config.layout.right) }
    }

    /// Brings every bar up to date, if something changed, and hands the changes to DWM at once.
    fn frame(&mut self) {
        if !self.dirty || self.gfx.is_none() {
            return;
        }
        self.dirty = false;
        let sides = self.measure();
        let animate = self.animate();
        let Some(gfx) = &self.gfx else { return };
        let mut result = Ok(());
        for bar in &mut self.bars {
            result = result.and(bar.render(gfx, &self.theme, &sides, self.config.bar.margin, animate));
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
        match Text::new(&self.theme) {
            Ok(text) => self.text = text,
            Err(e) => log!("fonts: {e}"),
        }
        self.dirty = true;
    }

    /// Something changed in the config dir: if it's the config file, everything starts again
    /// from it but the bars, which only move when `[bar]` changed.
    fn config_changed(&mut self) {
        if let Some(h) = self.config_watch {
            unsafe {
                let _ = FindNextChangeNotification(h);
            }
        }
        let stamp = config_stamp();
        if stamp == self.config_stamp {
            return;
        }
        self.config_stamp = stamp;
        let (config, error) = config::load();
        if let Some(e) = &error {
            log!("config: {e}");
            return;
        }
        log!("config reloaded");
        let bars_changed = config.bar != self.config.bar;
        self.config = config;
        self.widgets = make_widgets(&self.config);
        self.reload_theme();
        let front = self.front.clone();
        self.broadcast(&Signal::Foreground(&front));
        if bars_changed {
            self.build_bars();
        } else {
            for bar in &mut self.bars {
                if let Some(gfx) = &self.gfx {
                    let _ = bar.attach(gfx);
                }
            }
        }
        self.update_visibility();
        self.dirty = true;
    }
}

impl Drop for App {
    fn drop(&mut self) {
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

/// The widgets of `[layout]`, in order, each name once.
fn make_widgets(config: &Config) -> Vec<Slot> {
    let mut slots: Vec<Slot> = Vec::new();
    let names = config.layout.left.iter().chain(&config.layout.center).chain(&config.layout.right);
    for name in names {
        if slots.iter().any(|s| &s.name == name) {
            log!("{name} is in [layout] twice: shown once");
            continue;
        }
        let kind = config::widget_kind(config, name);
        let options = Options(config.widgets.get(name).cloned().unwrap_or_default());
        match widgets::create(&kind, &options) {
            Some(widget) => slots.push(Slot { name: name.clone(), widget }),
            None => log!("unknown widget {name:?} (kinds: {})", widgets::KINDS.join(", ")),
        }
    }
    slots
}

fn monitors() -> Vec<(HMONITOR, RECT)> {
    unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let list = unsafe { &mut *(data.0 as *mut Vec<(HMONITOR, RECT)>) };
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            list.push((monitor, info.rcMonitor));
        }
        BOOL(1)
    }
    let mut list: Vec<(HMONITOR, RECT)> = Vec::new();
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

fn watch_config_dir() -> Option<HANDLE> {
    let dir = config::dir();
    let _ = std::fs::create_dir_all(&dir);
    let wide = HSTRING::from(dir.to_string_lossy().as_ref());
    match unsafe { FindFirstChangeNotificationW(&wide, false, FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_FILE_NAME) } {
        Ok(h) => Some(h),
        Err(e) => {
            log!("can't watch {}: {e}", dir.display());
            None
        }
    }
}

fn config_stamp() -> Option<(SystemTime, u64)> {
    let m = std::fs::metadata(config::file()).ok()?;
    Some((m.modified().ok()?, m.len()))
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
        WM_POWERBROADCAST => {
            if wparam.0 as u32 == PBT_APMRESUMEAUTOMATIC || wparam.0 as u32 == PBT_APMRESUMESUSPEND {
                push(Event::Resume);
            }
            return LRESULT(1);
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
