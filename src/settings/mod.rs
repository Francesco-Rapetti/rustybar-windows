//! The settings window: `rustybar.toml` (and `rustybar.local.toml`) edited in place, in the style
//! of Windows 11's Settings. On the left the pages: the bar, the theme, the layout, one for each
//! widget, and About; on the right the page. A change is written at once (see `store`), and the
//! bar, which watches the files, reloads by itself (only what changed: see `app::reload`).
//!
//! It runs in a process of its own, `rustybar settings [page]`, which ends when the window
//! closes: the bar carries none of it. One window per config dir: a second `rustybar settings`
//! brings it to the front, on the page asked for. It draws as the popups do (Direct2D and
//! DirectWrite into DirectComposition surfaces, with WARP), only when something changes, and only
//! what changed: a page is built anew after every change and drawn again where its scene differs.
//! Scrolling moves the page's visual, which DWM animates; nothing is drawn. The text fields are
//! Windows' own EDIT control while they're edited (see `edit`).

mod edit;
mod form;
mod pages;
mod schema;
mod store;

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;
use std::time::Instant;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{D2D1_ANTIALIAS_MODE_ALIASED, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, ID2D1DeviceContext};
use windows::Win32::Graphics::DirectComposition::{IDCompositionSurface, IDCompositionTarget, IDCompositionVisual2};
use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{ClientToScreen, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_CONTROL, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME,
    VK_NEXT, VK_PRIOR, VK_RETURN, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EnumWindows,
    GetClassNameW, GetClientRect, GetCursorPos, GetForegroundWindow, GetMessageW, GetPropW, GetWindowThreadProcessId,
    HTCLIENT, IDC_ARROW, IDC_IBEAM, IsIconic, LoadCursorW, MINMAXINFO, MSG, PostQuitMessage, RegisterClassExW, SM_CXSCREEN, SW_RESTORE, SW_SHOW,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageTimeoutW, SetCursor, SetForegroundWindow, SetPropW, SetWindowPos,
    SMTO_ABORTIFHUNG, ShowWindow, TranslateMessage, WM_CHAR, WM_CLOSE, WM_COPYDATA, WM_DESTROY, WM_DWMCOLORIZATIONCOLORCHANGED,
    WM_ERASEBKGND, WM_GETMINMAXINFO, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_MOVE, WM_NCACTIVATE, WM_SETCURSOR,
    WM_SETTINGCHANGE, WM_SIZE, WNDCLASSEXW, WS_EX_NOREDIRECTIONBITMAP, WS_OVERLAPPEDWINDOW, GetSystemMetrics,
};
use windows::core::{BOOL, HSTRING, Interface, PCWSTR, Result, w};
use windows_numerics::Matrix3x2;

use self::form::{Do, Look, Opt, Page};
use crate::gfx::{self, Gfx};
use crate::i18n::t;
use crate::log;
use crate::theme::{Rgba, Theme};
use crate::ui::{Align, Fonts, Rect, Scene, Shape};

const CLASS: PCWSTR = w!("RustyBarSettings");
/// The window property that tells which config dir a window edits.
const DIR_PROP: PCWSTR = w!("RustyBarDir");
/// Not in the parts of the `windows` crate the bar uses.
const WM_MOUSELEAVE: u32 = 0x02a3;
const WM_DPICHANGED: u32 = 0x02e0;
/// The sidebar's width, the page's side room and widest content, in DIPs.
const SIDE_W: f32 = 280.0;
const PAD: f32 = 40.0;
const MAX_W: f32 = 1000.0;
/// The hover tint: the text color, this opaque.
const TINT: f32 = 0.06;
/// How long a scroll glides, in seconds.
const GLIDE: f64 = 0.16;
/// The rows of a menu, and how many show at once.
const MENU_ROW: f32 = 34.0;
const MENU_ROWS: usize = 12;

/// What the window procedures queue for the window.
pub enum Ev {
    Size,
    Dpi(u32, RECT),
    Mouse(i32, i32),
    Leave,
    Down(i32, i32),
    Up,
    Wheel(i32),
    Key(u16),
    Char(u16),
    /// The text box is done: keep its text, or put it back.
    EditDone { host: HWND, keep: bool },
    /// A second `rustybar settings` asks for this page.
    Page(String),
    Theme,
    /// A worker brought something for the page (the entities of Home Assistant): it's made again.
    Refresh,
    Moved,
    Close,
}

thread_local! {
    static EVENTS: RefCell<Vec<Ev>> = const { RefCell::new(Vec::new()) };
    static WIN: RefCell<Option<Win>> = const { RefCell::new(None) };
    /// The mouse is over a text field: the cursor is the I-beam.
    static IBEAM: Cell<bool> = const { Cell::new(false) };
    /// The text box is open: the window's title bar stays as if active while it has the keyboard.
    static EDITING: Cell<bool> = const { Cell::new(false) };
    static TRACKING: Cell<bool> = const { Cell::new(false) };
}

/// The window, for a worker thread to tell (`refresh`).
static WINDOW: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
/// The message `refresh` posts: `WM_APP + 1`.
const REFRESH: u32 = 0x8001;

/// From any thread: the page is made again, with what a worker just brought.
pub fn refresh() {
    let hwnd = WINDOW.load(std::sync::atomic::Ordering::Relaxed);
    if hwnd != 0 {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(Some(HWND(hwnd as *mut c_void)), REFRESH, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn push(ev: Ev) {
    EVENTS.with(|q| q.borrow_mut().push(ev));
}

/// Handles what's queued, unless the window is already handling something (a Win32 call made
/// while handling sent a message back): then the outer call gets it when it's done.
fn drain() {
    loop {
        let events = EVENTS.with(|q| std::mem::take(&mut *q.borrow_mut()));
        if events.is_empty() {
            return;
        }
        let handled = WIN.with(|w| match w.try_borrow_mut() {
            Ok(mut w) => {
                if let Some(w) = w.as_mut() {
                    for ev in events {
                        w.handle(ev);
                    }
                }
                true
            }
            Err(_) => {
                EVENTS.with(|q| {
                    let mut q = q.borrow_mut();
                    let later = std::mem::take(&mut *q);
                    *q = events;
                    q.extend(later);
                });
                false
            }
        });
        if !handled {
            return;
        }
    }
}

/// `rustybar settings [page]` or `rustybar settings --shot <page> <file.png> [scale] [width]`.
pub fn run(args: &[String]) -> i32 {
    // With made-up data the window edits the copies in examples/mock/settings: everything that
    // reads the config dir reads that one
    if let Some(dir) = store::mock_dir() {
        let _ = std::fs::create_dir_all(&dir);
        // SAFETY: at start-up, before any thread
        unsafe { std::env::set_var("RUSTYBAR_CONFIG_DIR", &dir) };
    }
    if args.first().is_some_and(|a| a == "--shot") {
        crate::process::attach_console();
        return shot(&args[1..]);
    }
    let page = args.first().cloned().unwrap_or_else(|| "bar".into());
    crate::log::init_named("settings");
    crate::crash::install(false);
    log!("settings {} for {}", env!("CARGO_PKG_VERSION"), crate::config::dir().display());
    crate::process::setup();
    let Some(_instance) = single_instance() else {
        // Another window edits this config: it comes in front, on this page
        bring_back(&page);
        return 0;
    };
    let _ = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED) };
    let started = Instant::now();
    match Win::new(&page) {
        Ok(win) => {
            WIN.with(|w| *w.borrow_mut() = Some(win));
            WIN.with(|w| {
                if let Some(w) = w.borrow_mut().as_mut() {
                    w.show(started);
                }
            });
        }
        Err(e) => {
            log!("can't open the window: {e}");
            return 1;
        }
    }
    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        drain();
    }
    WIN.with(|w| *w.borrow_mut() = None);
    log!("settings closed");
    0
}

/// A window for tests (`RUSTYBAR_SETTINGS_TEST`): off every screen, and it never asks for the front, so
/// that driving it with messages leaves alone what the user is doing (a game in front).
fn testing() -> bool {
    std::env::var_os("RUSTYBAR_SETTINGS_TEST").is_some()
}

fn dir_tag() -> usize {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    crate::config::dir().to_string_lossy().to_lowercase().hash(&mut h);
    // Never 0: a window without the property reads as 0
    (h.finish() as usize) | 1
}

fn single_instance() -> Option<windows::Win32::Foundation::HANDLE> {
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    let name = HSTRING::from(format!("Local\\RustyBar-settings-{:x}", dir_tag()));
    unsafe {
        let mutex = windows::Win32::System::Threading::CreateMutexW(None, true, &name).ok()?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = windows::Win32::Foundation::CloseHandle(mutex);
            return None;
        }
        Some(mutex)
    }
}

/// The open window of this config dir, if there's one.
fn find_window() -> Option<HWND> {
    unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
        let found = unsafe { &mut *(data.0 as *mut Option<HWND>) };
        let mut buf = [0u16; 32];
        let n = unsafe { GetClassNameW(hwnd, &mut buf) };
        if String::from_utf16_lossy(&buf[..n.max(0) as usize]) == "RustyBarSettings" && unsafe { GetPropW(hwnd, DIR_PROP) }.0 as usize == dir_tag() {
            *found = Some(hwnd);
            return BOOL(0);
        }
        BOOL(1)
    }
    let mut found: Option<HWND> = None;
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut found as *mut _ as isize));
    }
    found
}

/// Hands the page to the window that's open, which comes in front.
fn bring_back(page: &str) {
    // The other process may just be starting: its window comes within a moment
    for _ in 0..30 {
        if let Some(hwnd) = find_window() {
            unsafe {
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                if AllowSetForegroundWindow(pid).is_err() {
                    let _ = AllowSetForegroundWindow(ASFW_ANY);
                }
                let bytes = page.as_bytes();
                let data = windows::Win32::System::DataExchange::COPYDATASTRUCT {
                    dwData: crate::ipc::MAGIC,
                    cbData: bytes.len() as u32,
                    lpData: bytes.as_ptr() as *mut c_void,
                };
                let mut result = 0usize;
                SendMessageTimeoutW(hwnd, WM_COPYDATA, WPARAM(0), LPARAM(&data as *const _ as isize), SMTO_ABORTIFHUNG, 2000, Some(&mut result));
            }
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let point = || ((lparam.0 & 0xffff) as i16 as i32, ((lparam.0 >> 16) & 0xffff) as i16 as i32);
    let ev = match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_GETMINMAXINFO => {
            let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
            let s = unsafe { GetDpiForWindow(hwnd) }.max(96) as f32 / 96.0;
            info.ptMinTrackSize = POINT { x: (760.0 * s) as i32, y: (520.0 * s) as i32 };
            return LRESULT(0);
        }
        // While the text box has the keyboard, the title bar stays as it was: the box is part of
        // this window
        WM_NCACTIVATE if wparam.0 == 0 && EDITING.get() => return unsafe { DefWindowProcW(hwnd, msg, WPARAM(1), LPARAM(-1)) },
        WM_SETCURSOR if (lparam.0 & 0xffff) as u32 == HTCLIENT => {
            unsafe { SetCursor(LoadCursorW(None, if IBEAM.get() { IDC_IBEAM } else { IDC_ARROW }).ok()) };
            return LRESULT(1);
        }
        WM_SIZE => Ev::Size,
        WM_MOVE => Ev::Moved,
        WM_DPICHANGED => Ev::Dpi((wparam.0 & 0xffff) as u32, unsafe { *(lparam.0 as *const RECT) }),
        WM_MOUSEMOVE => {
            if !TRACKING.get() {
                let mut track = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                TRACKING.set(unsafe { TrackMouseEvent(&mut track) }.is_ok());
            }
            let (x, y) = point();
            Ev::Mouse(x, y)
        }
        WM_MOUSELEAVE => {
            TRACKING.set(false);
            Ev::Leave
        }
        WM_LBUTTONDOWN => {
            let (x, y) = point();
            Ev::Down(x, y)
        }
        WM_LBUTTONUP => Ev::Up,
        WM_MOUSEWHEEL => Ev::Wheel(((wparam.0 >> 16) & 0xffff) as i16 as i32),
        WM_KEYDOWN => Ev::Key(wparam.0 as u16),
        WM_CHAR => Ev::Char(wparam.0 as u16),
        WM_SETTINGCHANGE => {
            let area = if lparam.0 != 0 { unsafe { PCWSTR(lparam.0 as *const u16).to_string() }.unwrap_or_default() } else { String::new() };
            if area != "ImmersiveColorSet" {
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            Ev::Theme
        }
        WM_DWMCOLORIZATIONCOLORCHANGED => Ev::Theme,
        REFRESH => Ev::Refresh,
        WM_COPYDATA => {
            return match unsafe { crate::ipc::read(lparam) } {
                Some(page) => {
                    push(Ev::Page(page));
                    drain();
                    LRESULT(1)
                }
                None => LRESULT(0),
            };
        }
        WM_CLOSE => Ev::Close,
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    };
    push(ev);
    drain();
    LRESULT(0)
}

/// Where the mouse is: over a zone of the sidebar, of the page, or of the open menu.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Layer {
    Side,
    Page,
    Menu,
}

/// The menu that's open: its choices, the first one shown, the one the keys are on.
struct Menu {
    options: Rc<Vec<Opt>>,
    current: String,
    on: Rc<dyn Fn(String)>,
    /// Where it is, in DIPs of the window.
    rect: Rect,
    first: usize,
    key: Option<usize>,
    scene: Scene,
    surface: Option<IDCompositionSurface>,
    size: (u32, u32),
}

/// The open text box, the field it's over (DIPs of the page), and what to do with its text.
type Editing = (edit::Edit, Rect, Rc<dyn Fn(String)>);

/// A press that may become a drag: on a slider, a chip of the layout, the scroll bar's thumb.
enum Press {
    Slide { key: String, num: form::Num, on: Rc<dyn Fn(f64)>, zone: Rect, value: f64 },
    Chip { side: usize, index: usize, at: (f32, f32), dragging: bool, target: Option<(usize, usize)> },
    Thumb { grab: f32 },
}

struct Comp {
    _target: IDCompositionTarget,
    root: IDCompositionVisual2,
    bg: IDCompositionVisual2,
    bg_surface: IDCompositionSurface,
    side: IDCompositionVisual2,
    side_surface: Option<IDCompositionSurface>,
    side_size: (u32, u32),
    clip: IDCompositionVisual2,
    page: IDCompositionVisual2,
    page_surface: Option<IDCompositionSurface>,
    page_size: (u32, u32),
    thumb: IDCompositionVisual2,
    thumb_surface: Option<(IDCompositionSurface, u32)>,
    menu: IDCompositionVisual2,
    menu_shown: bool,
    hover: IDCompositionVisual2,
    hover_surface: Option<IDCompositionSurface>,
    hover_key: (u32, u32, u32, u32),
    hover_parent: Option<Layer>,
    ghost: IDCompositionVisual2,
    ghost_surface: Option<IDCompositionSurface>,
    marker: IDCompositionVisual2,
    marker_surface: Option<(IDCompositionSurface, u32)>,
    overlays_shown: bool,
}

pub struct Win {
    hwnd: HWND,
    gfx: Gfx,
    theme_config: crate::config::ThemeConfig,
    theme: Theme,
    look: Look,
    fonts: Fonts,
    scale: f32,
    /// The client area, in pixels.
    client: (u32, u32),
    comp: Option<Comp>,
    page_id: String,
    side: Scene,
    /// How far the sidebar's list is scrolled, and how tall it is, in DIPs.
    side_scroll: f32,
    side_tall: f32,
    page: Page,
    /// What the surfaces show now, to draw again only what differs.
    side_drawn: Option<Scene>,
    page_drawn: Option<Scene>,
    scroll: f32,
    glide: Option<(f32, f32, Instant)>,
    mouse: Option<(f32, f32)>,
    hover: Option<(Layer, Rect, f32)>,
    menu: Option<Menu>,
    edit: Option<Editing>,
    press: Option<Press>,
    /// What the last change couldn't do, shown at the top until the next one.
    problem: Option<String>,
    /// The Lua widgets found in `widgets\`, read once.
    lua: Rc<Vec<String>>,
}

/// The theme of the config the window edits: the bar's.
fn theme_config() -> crate::config::ThemeConfig {
    store::get(&store::rb("theme")).and_then(|v| v.try_into().ok()).unwrap_or_default()
}

/// The window's fonts: Windows' UI font, whatever the bar uses.
fn ui_fonts(theme: &Theme) -> Result<Fonts> {
    let mut t = theme.clone();
    t.font = "Segoe UI Variable Text".into();
    Fonts::new(&t)
}

impl Win {
    fn new(page: &str) -> Result<Win> {
        let theme_config = theme_config();
        let theme = Theme::from_config(&theme_config);
        let look = Look::of(&theme);
        let fonts = ui_fonts(&theme)?;
        let gfx = Gfx::new()?;
        let hwnd = create_window()?;
        WINDOW.store(hwnd.0 as isize, std::sync::atomic::Ordering::Relaxed);
        let scale = unsafe { GetDpiForWindow(hwnd) }.max(96) as f32 / 96.0;
        let lua = Rc::new(pages::lua_names());
        let mut win = Win {
            hwnd,
            gfx,
            theme_config,
            theme,
            look,
            fonts,
            scale,
            client: (0, 0),
            comp: None,
            page_id: pages::known(page),
            side: Scene::default(),
            side_scroll: 0.0,
            side_tall: 0.0,
            page: Page { scene: Scene::default(), dos: Vec::new(), columns: None },
            side_drawn: None,
            page_drawn: None,
            scroll: 0.0,
            glide: None,
            mouse: None,
            hover: None,
            menu: None,
            edit: None,
            press: None,
            problem: None,
            lua,
        };
        win.caption_colors();
        win.client = client_size(hwnd);
        win.comp = Some(win.make_comp()?);
        win.build();
        win.draw()?;
        Ok(win)
    }

    fn show(&mut self, started: Instant) {
        unsafe {
            // A test window comes without the keyboard (see `create_window`)
            let test = testing();
            let _ = ShowWindow(self.hwnd, if test { SW_SHOWNOACTIVATE } else { SW_SHOW });
            if !test {
                let _ = SetForegroundWindow(self.hwnd);
            }
        }
        log!("window shown after {} ms", started.elapsed().as_millis());
    }

    /// The title bar in the page's colors (Windows 11), dark or light.
    fn caption_colors(&self) {
        let dark = BOOL::from(!self.look.light);
        let colorref = |c: Rgba| {
            let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
            b(c.0) | b(c.1) << 8 | b(c.2) << 16
        };
        unsafe {
            let set = |attr: i32, value: *const c_void, size: usize| {
                let _ = DwmSetWindowAttribute(self.hwnd, DWMWINDOWATTRIBUTE(attr), value, size as u32);
            };
            // DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR
            set(20, &dark as *const _ as *const c_void, size_of::<BOOL>());
            let caption = colorref(self.look.bg);
            let text = colorref(self.look.text);
            set(35, &caption as *const _ as *const c_void, 4);
            set(36, &text as *const _ as *const c_void, 4);
        }
    }

    fn make_comp(&self) -> Result<Comp> {
        let g = &self.gfx;
        unsafe {
            // Each visual goes over the ones added before it: with no reference visual,
            // DirectComposition puts a visual added with insertAbove = false above its siblings
            // (and one with true below them)
            let target = g.dcomp.CreateTargetForHwnd(self.hwnd, true)?;
            let root = g.dcomp.CreateVisual()?;
            target.SetRoot(&root)?;
            // The background: one pixel of its color, stretched over the window
            let bg = g.dcomp.CreateVisual()?;
            let bg_surface = g.surface(1, 1)?;
            bg.SetContent(&bg_surface)?;
            root.AddVisual(&bg, false, None)?;
            let side = g.dcomp.CreateVisual()?;
            root.AddVisual(&side, false, None)?;
            let clip = g.dcomp.CreateVisual()?;
            root.AddVisual(&clip, false, None)?;
            let page = g.dcomp.CreateVisual()?;
            clip.AddVisual(&page, false, None)?;
            let thumb = g.dcomp.CreateVisual()?;
            root.AddVisual(&thumb, false, None)?;
            let menu = g.dcomp.CreateVisual()?;
            let hover = g.dcomp.CreateVisual()?;
            let ghost = g.dcomp.CreateVisual()?;
            let marker = g.dcomp.CreateVisual()?;
            Ok(Comp {
                _target: target,
                root,
                bg,
                bg_surface,
                side,
                side_surface: None,
                side_size: (0, 0),
                clip,
                page,
                page_surface: None,
                page_size: (0, 0),
                thumb,
                thumb_surface: None,
                menu,
                menu_shown: false,
                hover,
                hover_surface: None,
                hover_key: (0, 0, 0, 0),
                hover_parent: None,
                ghost,
                ghost_surface: None,
                marker,
                marker_surface: None,
                overlays_shown: false,
            })
        }
    }

    /// The client area in DIPs.
    fn size(&self) -> (f32, f32) {
        (self.client.0 as f32 / self.scale, self.client.1 as f32 / self.scale)
    }

    /// The page's left room and its content's width, for the window's width.
    fn page_frame(&self) -> (f32, f32) {
        let view = self.size().0 - SIDE_W;
        (PAD, (view - 2.0 * PAD).clamp(360.0, MAX_W))
    }

    fn max_scroll(&self) -> f32 {
        (self.page.scene.height - self.size().1).max(0.0)
    }

    /// Builds the sidebar and the page again from the config.
    fn build(&mut self) {
        let (x, width) = self.page_frame();
        let ctx = pages::Ctx { lua: self.lua.clone(), problem: self.problem.clone() };
        let mut f = form::Form::new(&self.fonts, &self.theme, x, width, self.scale);
        pages::page(&self.page_id, &mut f, &ctx);
        self.page = f.finish();
        let (side, tall) = pages::sidebar(&self.look, &self.page_id, SIDE_W, self.size().1, self.scale, self.side_scroll);
        self.side = side;
        self.side_tall = tall;
        let most = (tall - self.size().1).max(0.0);
        if self.side_scroll > most {
            self.side_scroll = most;
            let (side, _) = pages::sidebar(&self.look, &self.page_id, SIDE_W, self.size().1, self.scale, most);
            self.side = side;
        }
        let max = self.max_scroll();
        if self.scroll > max {
            self.scroll = max;
            self.glide = None;
        }
    }

    /// Brings the surfaces up to the scenes, drawing only where they differ.
    fn draw(&mut self) -> Result<()> {
        let s = self.scale;
        let (w, h) = self.size();
        let bg = self.look.bg;
        let Some(comp) = self.comp.as_mut() else { return Ok(()) };
        unsafe {
            gfx::draw(&comp.bg_surface, 1.0, |ctx, brush| {
                brush.SetColor(&gfx::color(bg));
                ctx.FillRectangle(&gfx::rect(0.0, 0.0, 1.0, 1.0), brush);
            })?;
            comp.bg.SetTransform2(&Matrix3x2::scale(self.client.0.max(1) as f32, self.client.1.max(1) as f32))?;
        }
        // The sidebar
        let side_size = ((SIDE_W * s).ceil() as u32, self.client.1.max(1));
        if comp.side_size != side_size || comp.side_surface.is_none() {
            let surface = self.gfx.surface(side_size.0, side_size.1)?;
            unsafe { comp.side.SetContent(&surface)? };
            comp.side_surface = Some(surface);
            comp.side_size = side_size;
            self.side_drawn = None;
        }
        if let Some(surface) = &comp.side_surface {
            paint_diff(surface, s, side_size, bg, self.side_drawn.as_ref(), &self.side, &self.fonts)?;
        }
        self.side_drawn = Some(self.side.clone());
        // The page, under its clip
        let page_h = self.page.scene.height.max(h);
        let page_size = ((self.page.scene.width * s).ceil() as u32, (page_h * s).ceil() as u32);
        if comp.page_size != page_size || comp.page_surface.is_none() {
            let surface = self.gfx.surface(page_size.0, page_size.1)?;
            unsafe { comp.page.SetContent(&surface)? };
            comp.page_surface = Some(surface);
            comp.page_size = page_size;
            self.page_drawn = None;
        }
        if let Some(surface) = &comp.page_surface {
            paint_diff(surface, s, page_size, bg, self.page_drawn.as_ref(), &self.page.scene, &self.fonts)?;
        }
        self.page_drawn = Some(self.page.scene.clone());
        unsafe {
            comp.clip.SetOffsetX2((SIDE_W * s).round())?;
            comp.clip.SetClip2(&D2D_RECT_F { left: 0.0, top: 0.0, right: ((w - SIDE_W) * s).max(0.0), bottom: self.client.1 as f32 })?;
            if self.glide.is_none() {
                comp.page.SetOffsetY2(-(self.scroll * s).round())?;
            }
        }
        self.thumb(false)?;
        if self.menu.is_some() {
            self.draw_menu()?;
        }
        self.update_hover()?;
        self.gfx.commit()
    }

    /// The scroll bar's thumb, at the right edge, where the page is.
    fn thumb(&mut self, animate: bool) -> Result<()> {
        let s = self.scale;
        let (w, h) = self.size();
        let total = self.page.scene.height;
        let max = self.max_scroll();
        let look = self.look;
        let Some(comp) = self.comp.as_mut() else { return Ok(()) };
        if max <= 0.0 {
            if comp.thumb_surface.take().is_some() {
                unsafe { comp.thumb.SetContent(None::<&windows::core::IUnknown>)? };
            }
            return Ok(());
        }
        let thumb_h = (h * h / total).max(32.0);
        let height_px = (thumb_h * s).round() as u32;
        let width_px = (3.0 * s).round().max(2.0) as u32;
        if comp.thumb_surface.as_ref().is_none_or(|(_, hp)| *hp != height_px) {
            let surface = self.gfx.surface(width_px, height_px)?;
            let (tw, th) = (width_px as f32 / s, height_px as f32 / s);
            gfx::draw(&surface, s, |ctx, brush| unsafe {
                brush.SetColor(&gfx::color(look.text.alpha(0.45)));
                let r = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT { rect: gfx::rect(0.0, 0.0, tw, th), radiusX: tw / 2.0, radiusY: tw / 2.0 };
                ctx.FillRoundedRectangle(&r, brush);
            })?;
            unsafe { comp.thumb.SetContent(&surface)? };
            comp.thumb_surface = Some((surface, height_px));
        }
        let at = |scroll: f32| ((scroll / max) * (h - thumb_h) * s).round();
        unsafe {
            comp.thumb.SetOffsetX2(((w - 6.0) * s).round())?;
            match (animate, self.glide) {
                (true, Some((from, to, _))) => comp.thumb.SetOffsetY(&self.gfx.ease_out(at(from), at(to), GLIDE)?)?,
                _ => comp.thumb.SetOffsetY2(at(self.scroll))?,
            }
        }
        Ok(())
    }

    /// Where the page is now, in the middle of a glide too.
    fn scroll_now(&self) -> f32 {
        match self.glide {
            Some((from, to, start)) => {
                let t = (start.elapsed().as_secs_f64() / GLIDE).min(1.0) as f32;
                from + (to - from) * (1.0 - (1.0 - t).powi(3))
            }
            None => self.scroll,
        }
    }

    /// Scrolls the page to `target` (DIPs from its top), gliding with `animate`.
    fn scroll_to(&mut self, target: f32, animate: bool) {
        let target = target.clamp(0.0, self.max_scroll()).round();
        let from = self.scroll_now();
        if (target - self.scroll).abs() < 0.5 && self.glide.is_none() {
            return;
        }
        self.close_edit(true);
        self.scroll = target;
        let s = self.scale;
        self.glide = animate.then(|| (from, target, Instant::now()));
        if let Some(comp) = &self.comp {
            let r = unsafe {
                if animate {
                    self.gfx.ease_out(-(from * s).round(), -(target * s).round(), GLIDE).and_then(|a| comp.page.SetOffsetY(&a))
                } else {
                    comp.page.SetOffsetY2(-(target * s).round())
                }
            };
            if let Err(e) = r {
                log!("scroll: {e}");
            }
        }
        let _ = self.thumb(animate);
        let _ = self.update_hover();
        let _ = self.gfx.commit();
    }

    /// The zone under a point of the window (DIPs): its layer, index, and rectangle in the
    /// layer's own coordinates.
    fn hit(&self, x: f32, y: f32) -> Option<(Layer, usize)> {
        if let Some(menu) = &self.menu {
            let r = menu.rect;
            return r.contains(x, y).then(|| menu.scene.zone_at(x - r.x, y - r.y).map(|i| (Layer::Menu, i))).flatten();
        }
        if x < SIDE_W {
            return self.side.zone_at(x, y).map(|i| (Layer::Side, i));
        }
        let (px, py) = (x - SIDE_W, y + self.scroll);
        self.page.scene.zone_at(px, py).map(|i| (Layer::Page, i))
    }

    fn zone(&self, layer: Layer, i: usize) -> &crate::ui::Zone {
        match layer {
            Layer::Side => &self.side.zones[i],
            Layer::Page => &self.page.scene.zones[i],
            Layer::Menu => &self.menu.as_ref().expect("a menu is open").scene.zones[i],
        }
    }

    /// The hover tint under the mouse's zone, in the visual of the zone's layer (the page's
    /// scrolls with it).
    fn update_hover(&mut self) -> Result<()> {
        let hover = if self.press.is_some() {
            None
        } else {
            self.mouse.and_then(|(x, y)| self.hit(x, y)).map(|(layer, i)| (layer, self.zone(layer, i))).filter(|(_, z)| !z.slider).map(|(l, z)| (l, z.rect, z.radius))
        };
        IBEAM.set(
            self.mouse
                .and_then(|(x, y)| self.hit(x, y))
                .filter(|(l, _)| *l == Layer::Page)
                .and_then(|(_, i)| self.page.action(&self.page.scene.zones[i].action))
                .is_some_and(|d| matches!(d, Do::Field { .. })),
        );
        if hover == self.hover {
            return Ok(());
        }
        self.hover = hover;
        let s = self.scale;
        let text = self.look.text;
        let Some(comp) = self.comp.as_mut() else { return Ok(()) };
        unsafe {
            if let Some(parent) = comp.hover_parent.take() {
                let visual = match parent {
                    Layer::Side => &comp.side,
                    Layer::Page => &comp.page,
                    Layer::Menu => &comp.menu,
                };
                visual.RemoveVisual(&comp.hover)?;
            }
            let Some((layer, rect, radius)) = hover else { return self.gfx.commit() };
            let size = ((rect.w * s).round().max(1.0) as u32, (rect.h * s).round().max(1.0) as u32);
            let tint = text.alpha(TINT);
            let key = (size.0, size.1, radius.to_bits(), tint.bits());
            if comp.hover_key != key || comp.hover_surface.is_none() {
                let surface = self.gfx.surface(size.0, size.1)?;
                let (w, h) = (size.0 as f32 / s, size.1 as f32 / s);
                gfx::draw(&surface, s, |ctx, brush| {
                    brush.SetColor(&gfx::color(tint));
                    let r = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT { rect: gfx::rect(0.0, 0.0, w, h), radiusX: radius, radiusY: radius };
                    ctx.FillRoundedRectangle(&r, brush);
                })?;
                comp.hover.SetContent(&surface)?;
                comp.hover_surface = Some(surface);
                comp.hover_key = key;
            }
            comp.hover.SetOffsetX2((rect.x * s).round())?;
            comp.hover.SetOffsetY2((rect.y * s).round())?;
            let visual = match layer {
                Layer::Side => &comp.side,
                Layer::Page => &comp.page,
                Layer::Menu => &comp.menu,
            };
            visual.AddVisual(&comp.hover, false, None)?;
            comp.hover_parent = Some(layer);
        }
        self.gfx.commit()
    }

    fn handle(&mut self, ev: Ev) {
        let r = match ev {
            Ev::Size => self.resized(),
            Ev::Dpi(dpi, rect) => {
                self.scale = dpi as f32 / 96.0;
                unsafe {
                    let _ = SetWindowPos(self.hwnd, None, rect.left, rect.top, rect.right - rect.left, rect.bottom - rect.top, SWP_NOZORDER | SWP_NOACTIVATE);
                }
                self.close_edit(true);
                self.side_drawn = None;
                self.page_drawn = None;
                self.resized()
            }
            Ev::Mouse(x, y) => self.mouse_moved(x as f32 / self.scale, y as f32 / self.scale),
            Ev::Leave => {
                self.mouse = None;
                self.update_hover()
            }
            Ev::Down(x, y) => self.pressed(x as f32 / self.scale, y as f32 / self.scale),
            Ev::Up => self.released(),
            Ev::Wheel(delta) => self.wheel(delta),
            Ev::Key(key) => self.key(key),
            Ev::Char(c) => self.typed(c),
            Ev::EditDone { host, keep } => {
                if self.edit.as_ref().is_some_and(|(e, _, _)| e.host == host) {
                    self.close_edit(keep);
                }
                Ok(())
            }
            Ev::Page(page) => {
                unsafe {
                    if IsIconic(self.hwnd).as_bool() {
                        let _ = ShowWindow(self.hwnd, SW_RESTORE);
                    }
                    if !testing() {
                        let _ = SetForegroundWindow(self.hwnd);
                    }
                }
                if !page.is_empty() {
                    let page = pages::known(&page);
                    self.go(&page);
                }
                self.rebuild()
            }
            Ev::Theme => {
                if self.theme.follows_windows {
                    self.theme_changed(true);
                }
                Ok(())
            }
            Ev::Refresh => self.rebuild(),
            Ev::Moved => {
                self.place_edit();
                Ok(())
            }
            Ev::Close => {
                self.close_edit(true);
                unsafe {
                    let _ = DestroyWindow(self.hwnd);
                }
                Ok(())
            }
        };
        if let Err(e) = r {
            log!("settings: {e}");
        }
    }

    fn resized(&mut self) -> Result<()> {
        let size = client_size(self.hwnd);
        // Minimized: nothing to show
        if size.0 == 0 || size.1 == 0 {
            return Ok(());
        }
        let width_changed = size.0 != self.client.0;
        self.client = size;
        if width_changed {
            self.close_edit(true);
            self.close_menu();
        }
        self.build();
        self.draw()
    }

    /// The config changed: the theme may have too; the page is built again and drawn where it
    /// differs.
    fn rebuild(&mut self) -> Result<()> {
        self.build();
        self.draw()
    }

    /// The theme the window shows follows the bar's: read again after every change.
    fn theme_changed(&mut self, force: bool) {
        let config = theme_config();
        if !force && config == self.theme_config {
            return;
        }
        self.theme_config = config;
        self.theme = Theme::from_config(&self.theme_config);
        self.look = Look::of(&self.theme);
        self.caption_colors();
        let _ = self.rebuild();
    }

    /// What follows a change: requests to see to, the file's problems, the theme, the page again.
    fn changed(&mut self) -> Result<()> {
        for request in form::take_requests() {
            match request {
                form::Request::Go(page) => self.go(&page),
                form::Request::Open(target) => crate::process::open(&target),
            }
        }
        self.problem = store::take_problem();
        if store::take_written() {
            self.theme_changed(false);
        }
        self.rebuild()
    }

    fn go(&mut self, page: &str) {
        if page == self.page_id {
            return;
        }
        form::forget_confirm();
        self.close_edit(true);
        self.close_menu();
        self.page_id = page.to_string();
        self.problem = None;
        self.scroll = 0.0;
        self.glide = None;
    }

    fn mouse_moved(&mut self, x: f32, y: f32) -> Result<()> {
        self.mouse = Some((x, y));
        let (_, h) = self.size();
        let (total, max) = (self.page.scene.height, self.max_scroll());
        match &mut self.press {
            Some(Press::Slide { key, num, zone, value, .. }) => {
                let f = ((x - SIDE_W - zone.x) / zone.w.max(1.0)).clamp(0.0, 1.0) as f64;
                let v = num.clamp((num.min + (num.max - num.min) * f) / num.step * num.step);
                let v = num.clamp((v / num.step).round() * num.step);
                if v != *value {
                    *value = v;
                    form::set_preview(Some((key.clone(), v)));
                    return self.rebuild();
                }
                return Ok(());
            }
            Some(Press::Thumb { grab }) => {
                let thumb_h = (h * h / total).max(32.0);
                let room = (h - thumb_h).max(1.0);
                let target = (y - *grab) / room * max;
                self.scroll_to(target, false);
                return Ok(());
            }
            Some(Press::Chip { at, dragging, .. }) => {
                if !*dragging && ((x - at.0).powi(2) + (y - at.1).powi(2)).sqrt() > 4.0 {
                    *dragging = true;
                    self.start_drag()?;
                }
                if matches!(self.press, Some(Press::Chip { dragging: true, .. })) {
                    return self.drag_to(x, y);
                }
                return Ok(());
            }
            None => {}
        }
        self.update_hover()
    }

    fn pressed(&mut self, x: f32, y: f32) -> Result<()> {
        self.mouse = Some((x, y));
        // Out of the menu, a click closes it and does nothing else
        if self.menu.is_some() {
            if let Some((Layer::Menu, i)) = self.hit(x, y) {
                let action = self.zone(Layer::Menu, i).action.clone();
                return self.pick(&action);
            }
            self.close_menu();
            return self.update_hover();
        }
        // The scroll bar's thumb, at the right edge
        if self.max_scroll() > 0.0 && x >= self.size().0 - 14.0 {
            let (_, h) = self.size();
            let thumb_h = (h * h / self.page.scene.height).max(32.0);
            let top = self.scroll / self.max_scroll() * (h - thumb_h);
            let grab = if y >= top && y <= top + thumb_h { y - top } else { thumb_h / 2.0 };
            self.press = Some(Press::Thumb { grab });
            unsafe { SetCapture(self.hwnd) };
            return self.mouse_moved(x, y);
        }
        let Some((layer, i)) = self.hit(x, y) else { return Ok(()) };
        let action = self.zone(layer, i).action.clone();
        if layer == Layer::Side {
            if let Some(page) = action.strip_prefix("go:") {
                let page = page.to_string();
                self.go(&page);
                return self.rebuild();
            }
            return Ok(());
        }
        let zone_rect = self.zone(layer, i).rect;
        // A click that isn't a second one forgets a first one waiting
        let Some(d) = self.page.action(&action) else { return Ok(()) };
        match d {
            Do::Run(f) => {
                let f = f.clone();
                f();
                self.changed()
            }
            Do::Menu { rect, options, current, on } => {
                let (rect, options, current, on) = (*rect, options.clone(), current.clone(), on.clone());
                self.open_menu(rect, options, current, on)
            }
            Do::Field { rect, value, secret, on } => {
                let (rect, value, secret, on) = (*rect, value.clone(), *secret, on.clone());
                self.open_edit(rect, &value, secret, on);
                Ok(())
            }
            Do::Slide { key, num, on } => {
                let (key, num, on) = (key.clone(), *num, on.clone());
                unsafe { SetCapture(self.hwnd) };
                self.press = Some(Press::Slide { key, num, on, zone: zone_rect, value: f64::NAN });
                self.mouse_moved(x, y)
            }
            Do::Chip { side, index } => {
                let (side, index) = (*side, *index);
                unsafe { SetCapture(self.hwnd) };
                self.press = Some(Press::Chip { side, index, at: (x, y), dragging: false, target: None });
                self.update_hover()
            }
        }
    }

    fn released(&mut self) -> Result<()> {
        let Some(press) = self.press.take() else { return Ok(()) };
        unsafe {
            let _ = ReleaseCapture();
        }
        match press {
            Press::Slide { on, value, .. } => {
                form::set_preview(None);
                if value.is_finite() {
                    on(value);
                }
                self.changed()
            }
            Press::Chip { side, index, dragging, target, .. } => {
                self.end_drag()?;
                let Some(columns) = &self.page.columns else { return Ok(()) };
                if !dragging {
                    let open = columns.on_open.clone();
                    open(side, index);
                } else if let Some((to_side, to_index)) = target {
                    let on_move = columns.on_move.clone();
                    on_move(side, index, to_side, to_index);
                }
                self.changed()
            }
            Press::Thumb { .. } => self.update_hover(),
        }
    }

    fn wheel(&mut self, delta: i32) -> Result<()> {
        if let Some(menu) = &mut self.menu {
            let rows = menu.options.len();
            let shown = rows.min(MENU_ROWS);
            let step = if delta > 0 { -3 } else { 3 };
            let first = (menu.first as i32 + step).clamp(0, (rows - shown) as i32) as usize;
            if first != menu.first {
                menu.first = first;
                self.draw_menu()?;
                return self.update_hover();
            }
            return Ok(());
        }
        let lines = delta as f32 / 120.0;
        // Over the sidebar, its list scrolls when the window is too short for it
        if self.mouse.is_some_and(|(x, _)| x < SIDE_W) {
            let most = (self.side_tall - self.size().1).max(0.0);
            let next = (self.side_scroll - lines * 76.0).clamp(0.0, most);
            if next != self.side_scroll {
                self.side_scroll = next;
                return self.rebuild();
            }
            return Ok(());
        }
        let target = self.scroll - lines * 96.0;
        self.scroll_to(target, true);
        Ok(())
    }

    fn key(&mut self, key: u16) -> Result<()> {
        if self.menu.is_some() {
            return self.menu_key(key);
        }
        let (_, h) = self.size();
        let target = match key {
            k if k == VK_UP.0 => self.scroll - 48.0,
            k if k == VK_DOWN.0 => self.scroll + 48.0,
            k if k == VK_PRIOR.0 => self.scroll - h * 0.85,
            k if k == VK_NEXT.0 => self.scroll + h * 0.85,
            k if k == VK_HOME.0 => 0.0,
            k if k == VK_END.0 => self.max_scroll(),
            // Ctrl+W closes the window, as a page of a browser
            0x57 if unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0 => {
                push(Ev::Close);
                return Ok(());
            }
            _ => return Ok(()),
        };
        self.scroll_to(target, true);
        Ok(())
    }

    // ---- the menu ------------------------------------------------------------------------

    fn open_menu(&mut self, control: Rect, options: Rc<Vec<Opt>>, current: String, on: Rc<dyn Fn(String)>) -> Result<()> {
        self.close_edit(true);
        let (w, h) = self.size();
        let shown = options.len().min(MENU_ROWS);
        let width = form::choice_width(&self.fonts, &options).max(control.w);
        let height = shown as f32 * MENU_ROW + 8.0;
        // Under the control, or over it when there's no room below
        let x = (SIDE_W + control.x).min(w - width - 8.0).max(8.0);
        let below = control.y - self.scroll + control.h + 4.0;
        let y = if below + height <= h - 8.0 { below } else { (control.y - self.scroll - 4.0 - height).max(8.0) };
        let index = options.iter().position(|o| o.value == current);
        let first = index.map_or(0, |i| i.saturating_sub(shown / 2)).min(options.len() - shown);
        self.menu = Some(Menu { options, current, on, rect: Rect::new(x, y, width, height), first, key: index, scene: Scene::default(), surface: None, size: (0, 0) });
        self.draw_menu()?;
        self.update_hover()?;
        self.gfx.commit()
    }

    fn close_menu(&mut self) {
        if self.menu.take().is_some()
            && let Some(comp) = &mut self.comp
        {
            unsafe {
                if comp.hover_parent == Some(Layer::Menu) {
                    let _ = comp.menu.RemoveVisual(&comp.hover);
                    comp.hover_parent = None;
                    self.hover = None;
                }
                if comp.menu_shown {
                    let _ = comp.root.RemoveVisual(&comp.menu);
                    comp.menu_shown = false;
                }
                let _ = comp.menu.SetContent(None::<&windows::core::IUnknown>);
            }
            let _ = self.gfx.commit();
        }
    }

    fn draw_menu(&mut self) -> Result<()> {
        let s = self.scale;
        let look = self.look;
        let Some(menu) = self.menu.as_mut() else { return Ok(()) };
        let r = menu.rect;
        let mut scene = Scene::new(r.w, r.h);
        // A flyout: a border, its fill, the rows
        scene.fill(Rect::new(0.0, 0.0, r.w, r.h), 8.0, look.control_border.alpha(0.3).mix(look.text.alpha(0.18), 0.5));
        scene.fill(Rect::new(1.0 / s, 1.0 / s, r.w - 2.0 / s, r.h - 2.0 / s), 8.0 - 1.0 / s, if look.light { look.card } else { look.bg.mix(look.text, 0.08) });
        let shown = menu.options.len().min(MENU_ROWS);
        for (n, opt) in menu.options.iter().enumerate().skip(menu.first).take(shown) {
            let y = 4.0 + (n - menu.first) as f32 * MENU_ROW;
            let row = Rect::new(4.0, y, r.w - 8.0, MENU_ROW - 2.0);
            if opt.value == menu.current {
                scene.fill(row, 4.0, look.selected);
                scene.fill(Rect::new(4.0, y + (MENU_ROW - 2.0 - 16.0) / 2.0, 3.0, 16.0), 1.5, look.accent);
            }
            if menu.key == Some(n) {
                scene.fill(row, 4.0, look.text.alpha(0.05));
            }
            let mut tx = 16.0;
            if let Some(c) = opt.swatch {
                scene.fill(Rect::new(tx, y + (MENU_ROW - 2.0 - 14.0) / 2.0, 14.0, 14.0), 7.0, c);
                tx += 22.0;
            }
            scene.text(Rect::new(tx, y, r.w - tx - 12.0, MENU_ROW - 2.0), opt.label.clone(), form::body(), look.text, Align::Start);
            scene.zone(row, 4.0, format!("pick:{n}"));
        }
        // How far down the list the rows shown are
        if menu.options.len() > shown {
            let track = r.h - 16.0;
            let bar = (track * shown as f32 / menu.options.len() as f32).max(16.0);
            let top = 8.0 + (track - bar) * menu.first as f32 / (menu.options.len() - shown) as f32;
            scene.fill(Rect::new(r.w - 6.0, top, 2.0, bar), 1.0, look.text.alpha(0.35));
        }
        let size = ((r.w * s).ceil() as u32, (r.h * s).ceil() as u32);
        let Some(comp) = self.comp.as_mut() else { return Ok(()) };
        if menu.surface.is_none() || menu.size != size {
            let surface = self.gfx.surface(size.0, size.1)?;
            unsafe { comp.menu.SetContent(&surface)? };
            menu.surface = Some(surface);
            menu.size = size;
        }
        if let Some(surface) = &menu.surface {
            let fonts = &self.fonts;
            gfx::draw(surface, s, |ctx, brush| crate::popup::paint_shapes(ctx, brush, s, &scene.shapes, fonts))?;
        }
        menu.scene = scene;
        unsafe {
            comp.menu.SetOffsetX2((r.x * s).round())?;
            comp.menu.SetOffsetY2((r.y * s).round())?;
            if !comp.menu_shown {
                comp.root.AddVisual(&comp.menu, false, None)?;
                comp.menu_shown = true;
            }
        }
        self.gfx.commit()
    }

    fn pick(&mut self, action: &str) -> Result<()> {
        let Some(n) = action.strip_prefix("pick:").and_then(|n| n.parse::<usize>().ok()) else { return Ok(()) };
        let Some(menu) = &self.menu else { return Ok(()) };
        let (value, on) = (menu.options[n].value.clone(), menu.on.clone());
        self.close_menu();
        on(value);
        self.changed()
    }

    fn menu_key(&mut self, key: u16) -> Result<()> {
        let Some(menu) = self.menu.as_mut() else { return Ok(()) };
        let n = menu.options.len();
        let at = menu.key.unwrap_or(0);
        let next = match key {
            k if k == VK_ESCAPE.0 => {
                self.close_menu();
                return Ok(());
            }
            k if k == VK_RETURN.0 => return self.pick(&format!("pick:{at}")),
            k if k == VK_UP.0 => at.saturating_sub(1),
            k if k == VK_DOWN.0 => (at + 1).min(n - 1),
            k if k == VK_PRIOR.0 => at.saturating_sub(MENU_ROWS),
            k if k == VK_NEXT.0 => (at + MENU_ROWS).min(n - 1),
            k if k == VK_HOME.0 => 0,
            k if k == VK_END.0 => n - 1,
            _ => return Ok(()),
        };
        self.menu_select(next)
    }

    fn menu_select(&mut self, next: usize) -> Result<()> {
        let Some(menu) = self.menu.as_mut() else { return Ok(()) };
        let shown = menu.options.len().min(MENU_ROWS);
        menu.key = Some(next);
        if next < menu.first {
            menu.first = next;
        } else if next >= menu.first + shown {
            menu.first = next + 1 - shown;
        }
        self.draw_menu()
    }

    /// A letter typed in an open menu goes to the next choice that starts with it (a long list
    /// of fonts).
    fn typed(&mut self, c: u16) -> Result<()> {
        let Some(menu) = &self.menu else { return Ok(()) };
        let Some(ch) = char::from_u32(c as u32).filter(|c| c.is_alphanumeric()) else { return Ok(()) };
        let ch = ch.to_lowercase().next().unwrap_or(ch);
        let n = menu.options.len();
        let start = menu.key.map_or(0, |k| k + 1);
        let found = (0..n).map(|i| (start + i) % n).find(|&i| menu.options[i].label.to_lowercase().starts_with(ch));
        match found {
            Some(i) => self.menu_select(i),
            None => Ok(()),
        }
    }

    // ---- the text box --------------------------------------------------------------------

    /// The field's inside in screen pixels, where the page is now.
    fn field_on_screen(&self, rect: Rect) -> RECT {
        let s = self.scale;
        let px = 1.0;
        let x = ((SIDE_W + rect.x) * s).round() as i32 + px as i32;
        let y = ((rect.y - self.scroll) * s).round() as i32 + px as i32;
        let w = (rect.w * s).round() as i32 - 2;
        let h = (rect.h * s).round() as i32 - 3;
        let mut origin = POINT { x, y };
        unsafe {
            let _ = ClientToScreen(self.hwnd, &mut origin);
        }
        RECT { left: origin.x, top: origin.y, right: origin.x + w, bottom: origin.y + h }
    }

    fn open_edit(&mut self, rect: Rect, value: &str, secret: bool, on: Rc<dyn Fn(String)>) {
        self.close_edit(true);
        self.close_menu();
        // The whole field in view first
        let (_, h) = self.size();
        if rect.y < self.scroll {
            self.scroll_to(rect.y - 16.0, false);
        } else if rect.y + rect.h > self.scroll + h {
            self.scroll_to(rect.y + rect.h + 16.0 - h, false);
        }
        let screen = self.field_on_screen(rect);
        let s = self.scale;
        let style = edit::Style { text: self.look.text, back: self.look.control, font_px: (form::BODY * s).round() as i32, pad_px: (9.0 * s).round() as i32 };
        match edit::Edit::open(self.hwnd, screen, value, secret, &style) {
            Ok(e) => {
                EDITING.set(true);
                self.edit = Some((e, rect, on));
            }
            Err(e) => log!("text box: {e}"),
        }
    }

    fn place_edit(&self) {
        if let Some((e, rect, _)) = &self.edit {
            e.place(self.field_on_screen(*rect));
        }
    }

    /// Closes the text box, keeping what was typed with `keep`.
    fn close_edit(&mut self, keep: bool) {
        let Some((e, _, on)) = self.edit.take() else { return };
        let text = e.text();
        let changed = keep && text != e.original;
        let front = unsafe { GetForegroundWindow() } == e.host;
        drop(e);
        EDITING.set(false);
        // The box had the keyboard: it goes back to the window, unless the user went elsewhere
        if front {
            unsafe {
                let _ = SetForegroundWindow(self.hwnd);
            }
        } else if unsafe { GetForegroundWindow() } != self.hwnd {
            unsafe {
                windows::Win32::UI::WindowsAndMessaging::SendMessageW(self.hwnd, WM_NCACTIVATE, Some(WPARAM(0)), Some(LPARAM(0)));
            }
        }
        if changed {
            on(text);
            let _ = self.changed();
        }
    }

    // ---- dragging a chip of the layout ---------------------------------------------------

    fn start_drag(&mut self) -> Result<()> {
        let Some(Press::Chip { side, index, .. }) = &self.press else { return Ok(()) };
        let Some(columns) = &self.page.columns else { return Ok(()) };
        let (Some(rect), Some(name)) = (columns.chips[*side].get(*index), columns.names[*side].get(*index)) else { return Ok(()) };
        let s = self.scale;
        let look = self.look;
        let mut scene = Scene::new(rect.w, rect.h);
        form::button_box(&mut scene, &look, Rect::new(0.0, 0.0, rect.w, rect.h), 1.0 / s);
        scene.text(Rect::new(12.0, 0.0, rect.w - 24.0, rect.h), name.clone(), form::body(), look.text, Align::Start);
        let size = ((rect.w * s).ceil() as u32, (rect.h * s).ceil() as u32);
        let marker_w = ((columns.areas[0].w - 16.0) * s).round().max(1.0) as u32;
        let Some(comp) = self.comp.as_mut() else { return Ok(()) };
        let ghost = self.gfx.surface(size.0, size.1)?;
        let fonts = &self.fonts;
        gfx::draw(&ghost, s, |ctx, brush| crate::popup::paint_shapes(ctx, brush, s, &scene.shapes, fonts))?;
        let marker = self.gfx.surface(marker_w, (2.0 * s).round().max(2.0) as u32)?;
        gfx::draw(&marker, s, |ctx, brush| unsafe {
            brush.SetColor(&gfx::color(look.accent));
            ctx.FillRectangle(&gfx::rect(0.0, 0.0, marker_w as f32 / s, 2.0), brush);
        })?;
        unsafe {
            comp.ghost.SetContent(&ghost)?;
            comp.marker.SetContent(&marker)?;
            comp.ghost.cast::<windows::Win32::Graphics::DirectComposition::IDCompositionVisual3>()?.SetOpacity2(0.9)?;
            comp.root.AddVisual(&comp.marker, false, None)?;
            comp.root.AddVisual(&comp.ghost, false, None)?;
            comp.overlays_shown = true;
        }
        comp.ghost_surface = Some(ghost);
        comp.marker_surface = Some((marker, marker_w));
        self.update_hover()
    }

    /// The chip follows the mouse; the line shows where it would go.
    fn drag_to(&mut self, x: f32, y: f32) -> Result<()> {
        let s = self.scale;
        let scroll = self.scroll;
        let Some(Press::Chip { side, index, target, .. }) = &mut self.press else { return Ok(()) };
        let Some(columns) = &self.page.columns else { return Ok(()) };
        let (px, py) = (x - SIDE_W, y + scroll);
        // The nearest column, and the place between its chips
        let col = (0..3)
            .min_by(|&a, &b| {
                let d = |c: usize| {
                    let r = columns.areas[c];
                    if px < r.x { r.x - px } else if px > r.x + r.w { px - r.x - r.w } else { 0.0 }
                };
                d(a).total_cmp(&d(b))
            })
            .unwrap_or(0);
        let chips: Vec<(usize, Rect)> = columns.chips[col].iter().copied().enumerate().filter(|(i, _)| !(col == *side && *i == *index)).collect();
        let place = chips.iter().filter(|(_, r)| py > r.y + r.h / 2.0).count();
        *target = Some((col, place));
        let area = columns.areas[col];
        let line_y = match (place, chips.len()) {
            (0, 0) => area.y + 4.0,
            (0, _) => chips[0].1.y - 3.0,
            (p, _) => chips[p - 1].1.y + chips[p - 1].1.h + 2.0,
        };
        let rect = columns.chips[*side].get(*index).copied().unwrap_or_default();
        let Some(comp) = &self.comp else { return Ok(()) };
        unsafe {
            comp.ghost.SetOffsetX2(((x - rect.w / 2.0) * s).round())?;
            comp.ghost.SetOffsetY2(((y - rect.h / 2.0) * s).round())?;
            comp.marker.SetOffsetX2(((SIDE_W + area.x + 8.0) * s).round())?;
            comp.marker.SetOffsetY2(((line_y - scroll) * s).round())?;
        }
        self.gfx.commit()
    }

    fn end_drag(&mut self) -> Result<()> {
        let Some(comp) = self.comp.as_mut() else { return Ok(()) };
        if comp.overlays_shown {
            unsafe {
                comp.root.RemoveVisual(&comp.ghost)?;
                comp.root.RemoveVisual(&comp.marker)?;
                comp.ghost.SetContent(None::<&windows::core::IUnknown>)?;
                comp.marker.SetContent(None::<&windows::core::IUnknown>)?;
            }
            comp.overlays_shown = false;
            comp.ghost_surface = None;
            comp.marker_surface = None;
        }
        self.gfx.commit()
    }
}

/// Draws `scene` into `surface` (`size` pixels at scale `s`, `bg` under it) where it differs
/// from `drawn`, what the surface shows now; everything without `drawn`.
fn paint_diff(surface: &IDCompositionSurface, s: f32, size: (u32, u32), bg: Rgba, drawn: Option<&Scene>, scene: &Scene, fonts: &Fonts) -> Result<()> {
    let region = match drawn {
        Some(old) if old.width == scene.width && old.height == scene.height => match changed_region(old, scene) {
            Some(r) => Some(r),
            None => return Ok(()),
        },
        _ => None,
    };
    let full = Rect::new(0.0, 0.0, size.0 as f32 / s, size.1 as f32 / s);
    let region = region.map(|r| {
        // Whole pixels, inside the surface
        let l = (r.x * s).floor().max(0.0);
        let t = (r.y * s).floor().max(0.0);
        let rr = ((r.x + r.w) * s).ceil().min(size.0 as f32);
        let b = ((r.y + r.h) * s).ceil().min(size.1 as f32);
        RECT { left: l as i32, top: t as i32, right: rr.max(l + 1.0) as i32, bottom: b.max(t + 1.0) as i32 }
    });
    unsafe {
        let mut offset = POINT::default();
        let ctx: ID2D1DeviceContext = surface.BeginDraw(region.as_ref().map(|r| r as *const RECT), &mut offset)?;
        ctx.SetDpi(96.0 * s, 96.0 * s);
        let (ox, oy) = match region {
            Some(r) => ((offset.x - r.left) as f32 / s, (offset.y - r.top) as f32 / s),
            None => (offset.x as f32 / s, offset.y as f32 / s),
        };
        ctx.SetTransform(&Matrix3x2::translation(ox, oy));
        ctx.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        let area = match region {
            Some(r) => Rect::new(r.left as f32 / s, r.top as f32 / s, (r.right - r.left) as f32 / s, (r.bottom - r.top) as f32 / s),
            None => full,
        };
        ctx.PushAxisAlignedClip(&gfx::rect(area.x, area.y, area.w, area.h), D2D1_ANTIALIAS_MODE_ALIASED);
        let brush = ctx.CreateSolidColorBrush(&gfx::color(bg), None)?;
        ctx.FillRectangle(&gfx::rect(area.x, area.y, area.w, area.h), &brush);
        let shapes: Vec<Shape> = scene.shapes.iter().filter(|sh| region.is_none() || overlaps(bounds(sh), area)).cloned().collect();
        crate::popup::paint_shapes(&ctx, &brush, s, &shapes, fonts);
        ctx.PopAxisAlignedClip();
        surface.EndDraw()
    }
}

fn bounds(shape: &Shape) -> Rect {
    match shape {
        Shape::Fill { rect, .. } | Shape::Image { rect, .. } | Shape::Para { rect, .. } => *rect,
        // Glyphs reach a little past their box
        Shape::Text { rect, .. } => Rect::new(rect.x - 2.0, rect.y - 2.0, rect.w + 4.0, rect.h + 4.0),
    }
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

fn union(a: Option<Rect>, b: Rect) -> Rect {
    match a {
        None => b,
        Some(a) => {
            let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
            let (x1, y1) = ((a.x + a.w).max(b.x + b.w), (a.y + a.h).max(b.y + b.h));
            Rect::new(x0, y0, x1 - x0, y1 - y0)
        }
    }
}

/// Where two scenes of the same size draw something different: the box around the shapes that
/// changed, before and after; None when they draw the same. When shapes came or went, the box
/// covers from the first difference on (everything after it may have moved).
fn changed_region(old: &Scene, new: &Scene) -> Option<Rect> {
    let mut region: Option<Rect> = None;
    let n = old.shapes.len().min(new.shapes.len());
    for i in 0..n {
        if old.shapes[i] != new.shapes[i] {
            region = Some(union(region, bounds(&old.shapes[i])));
            region = Some(union(region, bounds(&new.shapes[i])));
        }
    }
    for sh in old.shapes.iter().skip(n).chain(new.shapes.iter().skip(n)) {
        region = Some(union(region, bounds(sh)));
    }
    region
}

fn client_size(hwnd: HWND) -> (u32, u32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
    }
    ((r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32)
}

/// The window, 1040 × 760 DIPs at the scale of the screen with the mouse, in its middle; a test
/// window (`RUSTYBAR_SETTINGS_TEST`) goes off every screen.
fn create_window() -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&class);
        let mut title = t("Impostazioni di RustyBar", "RustyBar Settings").to_string();
        if crate::mock::dir().is_some() {
            title.push_str(t(" (dati finti)", " (made-up data)"));
        } else if crate::config::dir() != crate::config::default_dir() {
            title.push_str(&format!(" — {}", crate::config::dir().display()));
        }
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let (mut x, mut y, mut w, mut h) = (CW_USEDEFAULT, CW_USEDEFAULT, 1040, 760);
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let mut dpi = (96u32, 96u32);
            let _ = windows::Win32::UI::HiDpi::GetDpiForMonitor(monitor, windows::Win32::UI::HiDpi::MDT_EFFECTIVE_DPI, &mut dpi.0, &mut dpi.1);
            let s = dpi.0 as f32 / 96.0;
            let work = info.rcWork;
            w = ((1040.0 * s) as i32).min(work.right - work.left);
            h = ((760.0 * s) as i32).min(work.bottom - work.top);
            x = work.left + (work.right - work.left - w) / 2;
            y = work.top + (work.bottom - work.top - h) / 2;
        }
        if testing() {
            x = -4 * GetSystemMetrics(SM_CXSCREEN).max(4000);
            y = 0;
        }
        let hwnd = CreateWindowExW(WS_EX_NOREDIRECTIONBITMAP, CLASS, &HSTRING::from(title), WS_OVERLAPPEDWINDOW, x, y, w, h, None, None, Some(instance.into()), None)?;
        let _ = SetPropW(hwnd, DIR_PROP, Some(windows::Win32::Foundation::HANDLE(dir_tag() as *mut c_void)));
        Ok(hwnd)
    }
}

/// `rustybar settings --shot <page> <file.png> [scale] [width]`: a page as the window draws it,
/// sidebar included, away from the screen (never a capture of it), `width` DIPs wide (1040) and
/// as tall as the page.
fn shot(args: &[String]) -> i32 {
    use std::io::Write;
    let (Some(page), Some(file)) = (args.first(), args.get(1)) else {
        let _ = writeln!(std::io::stderr(), "usage: rustybar settings --shot <page> <file.png> [scale] [width]");
        return 2;
    };
    let scale = args.get(2).and_then(|s| s.parse::<f32>().ok()).unwrap_or(1.0).clamp(0.5, 4.0);
    let width = args.get(3).and_then(|s| s.parse::<f32>().ok()).unwrap_or(1040.0).clamp(760.0, 2400.0);
    let path = std::path::absolute(file).unwrap_or_else(|_| std::path::PathBuf::from(file));
    let _ = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED) };
    let result = (|| -> std::result::Result<(u32, u32), String> {
        let theme = Theme::from_config(&theme_config());
        let look = Look::of(&theme);
        let fonts = ui_fonts(&theme).map_err(|e| e.to_string())?;
        let page_id = pages::known(page);
        let page_id = page_id.as_str();
        let content = (width - SIDE_W - 2.0 * PAD).clamp(360.0, MAX_W);
        let ctx = pages::Ctx { lua: Rc::new(pages::lua_names()), problem: None };
        let mut f = form::Form::new(&fonts, &theme, PAD, content, scale);
        pages::page(page_id, &mut f, &ctx);
        let built = f.finish();
        let height = built.scene.height.max(560.0);
        let (side, _) = pages::sidebar(&look, page_id, SIDE_W, height, scale, 0.0);
        let gfx = Gfx::new().map_err(|e| e.to_string())?;
        let size = ((width * scale).ceil() as u32, (height * scale).ceil() as u32);
        gfx.png(size.0, size.1, scale, &path, |ctx, brush| unsafe {
            brush.SetColor(&gfx::color(look.bg));
            ctx.FillRectangle(&gfx::rect(0.0, 0.0, width, height), brush);
            crate::popup::paint_shapes(ctx, brush, scale, &side.shapes, &fonts);
            ctx.SetTransform(&Matrix3x2::translation(SIDE_W, 0.0));
            crate::popup::paint_shapes(ctx, brush, scale, &built.scene.shapes, &fonts);
            ctx.SetTransform(&Matrix3x2::identity());
        })
        .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(size)
    })();
    match result {
        Ok((w, h)) => {
            let _ = writeln!(std::io::stdout(), "ok {w}x{h}");
            0
        }
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "{e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Rgba;

    #[test]
    fn only_what_changed_is_drawn_again() {
        let mut a = Scene::new(100.0, 100.0);
        a.fill(Rect::new(0.0, 0.0, 10.0, 10.0), 0.0, Rgba(1.0, 0.0, 0.0, 1.0));
        a.fill(Rect::new(50.0, 50.0, 10.0, 10.0), 0.0, Rgba(1.0, 0.0, 0.0, 1.0));
        let mut b = a.clone();
        assert_eq!(changed_region(&a, &b), None);
        b.shapes[1] = Shape::Fill { rect: Rect::new(50.0, 50.0, 10.0, 10.0), radius: 0.0, color: Rgba(0.0, 1.0, 0.0, 1.0) };
        assert_eq!(changed_region(&a, &b), Some(Rect::new(50.0, 50.0, 10.0, 10.0)));
        b.fill(Rect::new(80.0, 0.0, 5.0, 5.0), 0.0, Rgba(0.0, 0.0, 1.0, 1.0));
        assert_eq!(changed_region(&a, &b), Some(Rect::new(50.0, 0.0, 35.0, 60.0)));
    }
}
