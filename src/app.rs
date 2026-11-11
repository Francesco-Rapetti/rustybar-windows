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

use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, RECT, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
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
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EVENT_SYSTEM_FOREGROUND, GetForegroundWindow, IsWindow, MSG,
    MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PM_REMOVE, PeekMessageW,
    PostQuitMessage, QS_ALLINPUT, RegisterClassExW, RegisterWindowMessageW, SPI_GETCLIENTAREAANIMATION,
    SPI_SETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetForegroundWindow, SystemParametersInfoW,
    TranslateMessage, WINDOW_EX_STYLE, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_CLOSE, WM_DISPLAYCHANGE,
    WM_DWMCOLORIZATIONCOLORCHANGED, WM_ENDSESSION, WM_POWERBROADCAST, WM_QUIT, WM_SETTINGCHANGE, WM_TIMECHANGE, WNDCLASSEXW,
    WS_POPUP,
};
use windows::core::{BOOL, HSTRING, PCWSTR, Result, w};

use crate::bar::{self, Bar, Sides};
use crate::config::{self, Config, Options};
use crate::gfx::Gfx;
use crate::log;
use crate::popup::{Anchor, Popup, Side};
use crate::process;
use crate::theme::Theme;
use crate::ui::{Measured, Text};
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
    config_stamp: Option<(SystemTime, u64)>,
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
        let mut app = App {
            widgets: make_widgets(&config),
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
            let mut owners: Vec<Option<(usize, usize)>> = Vec::new();
            if let Some(h) = self.config_watch {
                handles.push(h);
                owners.push(None);
            }
            for (i, slot) in self.widgets.iter().enumerate() {
                for (j, h) in slot.widget.handles().into_iter().enumerate() {
                    handles.push(h);
                    owners.push(Some((i, j)));
                }
            }
            let r = unsafe { MsgWaitForMultipleObjectsEx(Some(&handles), INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
            let index = r.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
            if index < handles.len() {
                match owners[index] {
                    None => self.config_changed(),
                    Some((i, j)) => {
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
        for i in 0..self.widgets.len() {
            if self.widgets[i].widget.event(signal) {
                self.changed(i);
            }
        }
    }

    /// The widget in slot `i` changed: its pill, and its popup if it's open.
    fn changed(&mut self, i: usize) {
        self.dirty = true;
        if self.popup.as_ref().is_some_and(|p| p.widget == self.widgets[i].name) {
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
        let Some(popup) = &mut self.popup else { return };
        let Some(gfx) = &self.gfx else { return };
        let Some(slot) = self.widgets.iter().find(|s| s.name == popup.widget) else { return };
        let result = match slot.widget.popup(&self.theme, &popup.fonts) {
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
        self.drop_popups();
        let bars_changed = config.bar != self.config.bar;
        self.config = config;
        self.widgets = make_widgets(&self.config);
        self.reload_theme();
        let front = self.front.clone();
        self.broadcast(&Signal::Foreground(&front));
        // The new widgets start as if a bar showed and no game was in front
        self.game_in_front = self.config.games.apps.iter().any(|a| a.eq_ignore_ascii_case(&front.exe));
        if self.game_in_front {
            self.broadcast(&Signal::Game(true));
        }
        if !self.bars.iter().any(|b| b.visible) {
            self.broadcast(&Signal::Shown(false));
        }
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
