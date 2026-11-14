//! The virtual desktops of Windows, each with the icons of its windows, as the macOS bar shows
//! AeroSpace's workspaces: one capsule per desktop with its number (or its name, when it has one)
//! and an icon per window, in the order the windows came; the current desktop under a highlight
//! that slides from one to the next, the window in front on a darker tile. A click on a number
//! goes to that desktop, on an icon brings that window in front (Windows goes to its desktop).
//! Compact, when the bar has no room for the rest, only the current desktop has its icons.
//!
//! Where it all comes from, without a timer:
//!
//! - The desktops: Explorer keeps the list and the current one in the registry
//!   (`Explorer\VirtualDesktops`: `VirtualDesktopIDs`, `CurrentVirtualDesktop`,
//!   `Desktops\{id}\Name`), and the registry tells when they change. With a single desktop
//!   Explorer writes neither, and the widget hides (`single = true` keeps it).
//! - The windows: those the taskbar would show (visible, not tool windows, without an owner, not
//!   hidden by their app, with a title), listed again when the shell says one came, went or was
//!   replaced (a shell hook on a message-only window, as the taskbar has; ~30 messages a minute
//!   while the PC is used, most of them ignored at once). Which desktop each is on comes from
//!   `IVirtualDesktopManager::GetWindowDesktopId` (documented: not the COM interfaces that change
//!   between builds), ~70 µs a window since Explorer answers it, asked only for new windows, and
//!   for all of them when the desktops change or Explorer leaves the front (the Task View, where
//!   windows move from one desktop to another). Listing them costs ~0.1 ms.
//! - The icons: see `app_icons`. Read once per app and dropped when its last window goes.
//!
//! Without visible bars the hook is gone and nothing is listed; the first listing waits for the
//! first frame. Options: `icons = false` shows the numbers alone, `max_icons` (8) the icons per
//! desktop before a `+N`.
//!
//! With made-up data (see `mock`) the desktops are the `desktops` of `workspaces.json`,
//! `current` (from 0) the one in front, and `windows` their windows (`desktop` from 0, a `glyph`
//! of Segoe Fluent Icons as the icon and a `color` of the palette), `front` the one in front;
//! nothing of the PC is read and the clicks move the highlight and the tile without sending
//! keys.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Once};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, KEY_READ, REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_CHANGE_NAME, RRF_RT_REG_BINARY,
    RRF_RT_REG_SZ, RegCloseKey, RegGetValueW, RegNotifyChangeKeyValue, RegOpenKeyExW,
};
use windows::Win32::System::Threading::{CreateEventW, GetCurrentProcessId};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_LCONTROL,
    VK_LEFT, VK_LWIN, VK_RIGHT,
};
use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DeregisterShellHookWindow, DestroyWindow, EnumWindows, GW_OWNER, GWL_EXSTYLE, GWLP_USERDATA, GetShellWindow,
    GetWindow, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowThreadProcessId, HWND_MESSAGE, IsIconic, IsWindowVisible, RegisterClassExW,
    RegisterShellHookWindow, RegisterWindowMessageW, SW_RESTORE, SetForegroundWindow, SetWindowLongPtrW, ShowWindowAsync, WINDOW_EX_STYLE,
    WINDOW_STYLE, WNDCLASSEXW, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, GUID, HSTRING, PCWSTR, w};

use super::app_icons;
use super::front_app;
use crate::config::Options;
use crate::log;
use crate::theme::Theme;
use crate::ui::{Image, Item, Pill};
use crate::wake::{Timer, Wake};

const KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\VirtualDesktops");

/// How big an icon is in the pill, in DIPs.
const ICON: f32 = 16.0;

/// How many times a window without a desktop is asked for it before it's left alone.
const TRIES: u8 = 4;

pub struct Workspaces {
    key: Option<HKEY>,
    event: HANDLE,
    single: bool,
    /// The icons of the windows (`icons`), and how many a desktop shows before `+N`.
    icons: bool,
    max_icons: usize,
    /// Each desktop's label: its name, or its number.
    desktops: Vec<String>,
    /// The desktops' ids, as the registry lists them; empty with a single desktop.
    ids: Vec<GUID>,
    current: Option<usize>,
    /// A listed game is in front: clicks on a number send no keys.
    game: bool,
    mock: bool,
    /// The windows, in the order they came.
    windows: Vec<Win>,
    /// Windows the taskbar would show that are on no desktop (a UWP app left in the background)
    /// or that the manager doesn't know yet (Explorer gives a new window its desktop a moment
    /// after the shell tells), and how many times they were asked: again every 300 ms (`retry`)
    /// up to `TRIES`, then once when the desktops change.
    ignored: HashMap<isize, u8>,
    retry: Timer,
    /// The windows were listed once: until then (the first frame) nothing lists them.
    listed: bool,
    /// The window in front, and whether it was Explorer's (the desktop, the taskbar, the Task
    /// View).
    front: HWND,
    front_is_shell: bool,
    manager: Option<IVirtualDesktopManager>,
    hook: Option<ShellHook>,
    /// Set by the hook: the windows are to list again.
    wake: Arc<Wake>,
    /// The icons, by app (an executable's path, or `app:` and a packaged app's id).
    cache: HashMap<String, Icon>,
    logos: Logos,
    /// An icon's side in pixels: `ICON` at the scale of the densest screen.
    px: u32,
    /// The theme is light (for the logos of packaged apps), as the last pill was drawn.
    light: Cell<bool>,
    pid: u32,
}

struct Win {
    hwnd: HWND,
    desktop: GUID,
    /// Its app's key in the cache.
    app: String,
}

enum Icon {
    Image(Arc<Image>),
    /// The first letter of the app's name, for an app without an icon, and while its logo loads.
    Letter(String),
    /// A glyph of Segoe Fluent Icons in a color of the palette (made-up data).
    Glyph(String, String),
}

/// The logos read, by app, from the worker thread.
type Read = Arc<Mutex<Vec<(String, Option<Image>)>>>;

/// The logos of packaged apps, read on a worker thread one batch at a time.
struct Logos {
    wake: Arc<Wake>,
    done: Read,
    queue: Vec<(String, String)>,
    busy: bool,
}

/// Where an item of the pill belongs: a desktop's number, one of its windows, the `+N` of those
/// that don't fit.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Slot {
    Desktop(usize),
    Window(usize, usize),
    More(usize, usize),
}

impl Slot {
    fn desktop(self) -> usize {
        match self {
            Slot::Desktop(d) | Slot::Window(d, _) | Slot::More(d, _) => d,
        }
    }
}

impl Workspaces {
    pub fn new(options: &Options) -> Workspaces {
        let mut w = Workspaces::blank(options);
        if let Some(m) = crate::mock::load("workspaces") {
            w.mock_data(&m);
            return w;
        }
        let mut key = HKEY::default();
        let opened = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, KEY, Some(0), KEY_READ | KEY_NOTIFY, &mut key) };
        w.key = opened.is_ok().then_some(key);
        if w.key.is_none() {
            log!("workspaces: no VirtualDesktops key ({opened:?})");
        }
        w.event = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }.unwrap_or_default();
        w.read();
        w.watch();
        if w.icons {
            // The windows are listed once the loop runs: after the first frame, which doesn't
            // wait for Explorer
            w.attach();
            w.wake.set();
        }
        w
    }

    /// The widget with its options, before it reads anything.
    fn blank(options: &Options) -> Workspaces {
        Workspaces {
            key: None,
            event: HANDLE::default(),
            single: options.bool("single").unwrap_or(false),
            icons: options.bool("icons").unwrap_or(true),
            max_icons: options.f64("max_icons").map(|n| n.clamp(1.0, 50.0) as usize).unwrap_or(8),
            desktops: Vec::new(),
            ids: Vec::new(),
            current: None,
            game: false,
            mock: false,
            windows: Vec::new(),
            ignored: HashMap::new(),
            retry: Timer::new(),
            listed: false,
            front: HWND::default(),
            front_is_shell: false,
            manager: None,
            hook: None,
            wake: Wake::new(),
            cache: HashMap::new(),
            logos: Logos { wake: Wake::new(), done: Arc::new(Mutex::new(Vec::new())), queue: Vec::new(), busy: false },
            px: icon_px(),
            light: Cell::new(false),
            pid: unsafe { GetCurrentProcessId() },
        }
    }

    /// The made-up desktops and windows of `workspaces.json` (see the module's doc).
    fn mock_data(&mut self, m: &crate::json::Json) {
        self.mock = true;
        self.desktops = m["desktops"].array().iter().filter_map(|d| d.str().map(str::to_string)).collect();
        self.ids = (0..self.desktops.len()).map(|i| GUID::from_u128(i as u128 + 1)).collect();
        self.current = m["current"].f64().map(|c| c as usize).filter(|c| *c < self.desktops.len());
        for (i, win) in m["windows"].array().iter().enumerate() {
            let desktop = win["desktop"].f64().unwrap_or(0.0) as u128;
            let app = format!("mock:{i}");
            let glyph = win["glyph"].str().and_then(|g| u32::from_str_radix(g, 16).ok()).and_then(char::from_u32).map(String::from);
            let icon = match glyph {
                Some(g) => Icon::Glyph(g, win["color"].str().unwrap_or("text").to_string()),
                None => Icon::Letter(app_icons::letter(win["name"].str().unwrap_or("?"))),
            };
            self.cache.insert(app.clone(), icon);
            self.windows.push(Win { hwnd: HWND((i + 1) as *mut _), desktop: GUID::from_u128(desktop + 1), app });
        }
        self.front = HWND(m["front"].f64().map(|f| f as usize + 1).unwrap_or(0) as *mut _);
    }

    /// Asks the registry to signal the event at the next change under the key: once, so again
    /// after every change.
    fn watch(&self) {
        if let Some(key) = self.key {
            let filter = REG_NOTIFY_CHANGE_LAST_SET | REG_NOTIFY_CHANGE_NAME;
            let result = unsafe { RegNotifyChangeKeyValue(key, true, filter, Some(self.event), true) };
            if result.is_err() {
                log!("workspaces: can't watch the registry ({result:?})");
            }
        }
    }

    /// Reads the desktops again. True if the pill changed.
    fn read(&mut self) -> bool {
        let Some(key) = self.key else { return false };
        let raw = binary(key, None, w!("VirtualDesktopIDs")).unwrap_or_default();
        let raw: Vec<[u8; 16]> = raw.as_chunks::<16>().0.to_vec();
        let current = binary(key, None, w!("CurrentVirtualDesktop")).and_then(|c| <[u8; 16]>::try_from(c.as_slice()).ok());
        let desktops: Vec<String> = raw
            .iter()
            .enumerate()
            .map(|(i, id)| {
                let sub = HSTRING::from(format!("Desktops\\{}", guid(id)));
                string(key, &sub, w!("Name")).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| (i + 1).to_string())
            })
            .collect();
        let current = current.and_then(|c| raw.iter().position(|id| *id == c));
        let ids: Vec<GUID> = raw.iter().map(guid_of).collect();
        let changed = desktops != self.desktops || current != self.current || ids != self.ids;
        self.desktops = desktops;
        self.current = current;
        self.ids = ids;
        changed
    }

    /// Starts following the windows: the manager of the desktops and the shell hook.
    fn attach(&mut self) {
        if self.mock || !self.icons {
            return;
        }
        if self.manager.is_none() {
            match unsafe { CoCreateInstance::<_, IVirtualDesktopManager>(&VirtualDesktopManager, None, CLSCTX_ALL) } {
                Ok(m) => self.manager = Some(m),
                Err(e) => log!("workspaces: no desktop manager ({e})"),
            }
        }
        if self.hook.is_none() && self.manager.is_some() {
            self.hook = ShellHook::new(self.wake.clone());
        }
    }

    /// Stops following them, while no bar shows.
    fn detach(&mut self) {
        self.hook = None;
        self.manager = None;
        self.retry.stop();
    }

    /// Lists the windows again: those that went go, the new ones come after the others (asked
    /// for their desktop and their icon), and with `everything` every window is asked for its
    /// desktop again. True if the pill changed.
    fn refresh(&mut self, everything: bool) -> bool {
        let Some(manager) = self.manager.clone() else { return false };
        self.listed = true;
        let before: Vec<(isize, GUID)> = self.windows.iter().map(|w| (w.hwnd.0 as isize, w.desktop)).collect();
        let found = taskbar_windows(self.pid);
        let here: HashSet<isize> = found.iter().map(|h| h.0 as isize).collect();
        self.windows.retain(|w| here.contains(&(w.hwnd.0 as isize)));
        self.ignored.retain(|h, _| here.contains(h));
        if everything {
            // Those left alone are asked once more, without asking again later
            for tries in self.ignored.values_mut() {
                *tries = TRIES - 1;
            }
            let mut lost = Vec::new();
            for w in &mut self.windows {
                match desktop_of(&manager, w.hwnd) {
                    Some(d) => w.desktop = d,
                    None => lost.push(w.hwnd.0 as isize),
                }
            }
            self.windows.retain(|w| !lost.contains(&(w.hwnd.0 as isize)));
            self.ignored.extend(lost.into_iter().map(|h| (h, TRIES)));
        }
        let mut again = false;
        // Lowest first: a window long left behind came before the ones used since
        for &hwnd in found.iter().rev() {
            let id = hwnd.0 as isize;
            let tries = self.ignored.get(&id).copied().unwrap_or(0);
            if tries >= TRIES || self.windows.iter().any(|w| w.hwnd == hwnd) {
                continue;
            }
            match desktop_of(&manager, hwnd) {
                Some(desktop) => {
                    self.ignored.remove(&id);
                    let app = self.app_of(hwnd);
                    self.windows.push(Win { hwnd, desktop, app });
                }
                None => {
                    self.ignored.insert(id, tries + 1);
                    again |= tries + 1 < TRIES;
                }
            }
        }
        if again {
            self.retry.start(300, 300);
        } else {
            self.retry.stop();
        }
        // Icons of apps with no window left go
        let used: HashSet<&str> = self.windows.iter().map(|w| w.app.as_str()).collect();
        self.cache.retain(|k, _| used.contains(k.as_str()));
        if let Some(hook) = &self.hook {
            *hook.state.known.borrow_mut() = self.windows.iter().map(|w| w.hwnd.0 as isize).chain(self.ignored.keys().copied()).collect();
        }
        let after: Vec<(isize, GUID)> = self.windows.iter().map(|w| (w.hwnd.0 as isize, w.desktop)).collect();
        before != after
    }

    /// The key of a window's app in the cache, its icon read (or asked for) if it isn't there.
    fn app_of(&mut self, hwnd: HWND) -> String {
        let pid = window_pid(hwnd);
        let path = pid.and_then(front_app::process_path).unwrap_or_default();
        if file_name(&path).eq_ignore_ascii_case("ApplicationFrameHost.exe")
            && let Some(id) = app_icons::frame_app_id(hwnd)
        {
            // A UWP app: its frame belongs to a host, the app is in the frame's properties
            let key = format!("app:{id}");
            if !self.cache.contains_key(&key) {
                let name = id.split_once('!').map(|(f, _)| f).unwrap_or(&id);
                let name = name.split('_').next().unwrap_or(name).rsplit('.').next().unwrap_or(name);
                self.request_logo(&key, &id, app_icons::letter(name));
            }
            return key;
        }
        let key = if path.is_empty() { format!("window:{}", hwnd.0 as isize) } else { path.to_lowercase() };
        if !self.cache.contains_key(&key) {
            let icon = (!path.is_empty()).then(|| app_icons::from_exe(&path, self.px)).flatten().or_else(|| app_icons::from_class(hwnd, self.px));
            let letter = || app_icons::letter(&if path.is_empty() { String::new() } else { front_app::describe(&path) });
            match icon {
                Some(image) => {
                    self.cache.insert(key.clone(), Icon::Image(Arc::new(image)));
                }
                None => match pid.and_then(app_icons::process_app_id) {
                    // A packaged app without icons in its executable: its package's logo
                    Some(id) => {
                        let l = letter();
                        self.request_logo(&key, &id, l);
                    }
                    None => {
                        self.cache.insert(key.clone(), Icon::Letter(letter()));
                    }
                },
            }
        }
        key
    }

    /// Reads the logo of the packaged app `id` for the app `key` on a worker thread; its letter
    /// meanwhile.
    fn request_logo(&mut self, key: &str, id: &str, letter: String) {
        self.cache.insert(key.to_string(), Icon::Letter(letter));
        self.logos.queue.push((key.to_string(), id.to_string()));
        self.start_logos();
    }

    fn start_logos(&mut self) {
        if self.logos.busy || self.logos.queue.is_empty() {
            return;
        }
        self.logos.busy = true;
        let list = std::mem::take(&mut self.logos.queue);
        let (done, wake, px, light) = (self.logos.done.clone(), self.logos.wake.clone(), self.px, self.light.get());
        let spawned = std::thread::Builder::new().name("logos".into()).stack_size(512 * 1024).spawn(move || {
            let logos: Vec<(String, Option<Image>)> = list.into_iter().map(|(key, id)| {
                let logo = app_icons::logo(&id, px, light);
                (key, logo)
            }).collect();
            if let Ok(mut d) = done.lock() {
                d.extend(logos);
            }
            wake.set();
        });
        if let Err(e) = spawned {
            log!("workspaces: no thread for the logos ({e})");
            self.logos.busy = false;
        }
    }

    /// The logos read: in the cache of the apps that still have a window. True if the pill changed.
    fn logos_read(&mut self) -> bool {
        self.logos.busy = false;
        let done: Vec<(String, Option<Image>)> = self.logos.done.lock().map(|mut d| std::mem::take(&mut *d)).unwrap_or_default();
        let mut changed = false;
        for (key, logo) in done {
            if let (Some(entry), Some(logo)) = (self.cache.get_mut(&key), logo) {
                *entry = Icon::Image(Arc::new(logo));
                changed = true;
            }
        }
        self.start_logos();
        changed
    }

    /// The screens changed: icons of another size if the densest one did.
    fn rescale(&mut self) -> bool {
        let px = icon_px();
        if px == self.px || self.mock {
            return false;
        }
        self.px = px;
        self.cache.clear();
        for i in 0..self.windows.len() {
            let app = self.app_of(self.windows[i].hwnd);
            self.windows[i].app = app;
        }
        true
    }

    /// Another window is in front: the tile follows it. A new one is listed; when Explorer leaves
    /// the front (the Task View closing, where windows move) every desktop is asked again.
    fn front_changed(&mut self, hwnd: HWND) -> bool {
        if self.mock {
            // The made-up window in front stays, whatever is in front of the PC
            return false;
        }
        let shell = window_pid(hwnd).is_some() && window_pid(hwnd) == window_pid(unsafe { GetShellWindow() });
        let left_shell = self.front_is_shell && !shell;
        self.front_is_shell = shell;
        let old = std::mem::replace(&mut self.front, hwnd);
        if !self.listed {
            // Before the first frame: the first listing comes after it
            return false;
        }
        let known = |h: HWND| self.windows.iter().any(|w| w.hwnd == h);
        let mut changed = old != hwnd && (known(old) || known(hwnd));
        if left_shell || (!known(hwnd) && !self.ignored.contains_key(&(hwnd.0 as isize))) {
            changed |= self.refresh(left_shell);
        }
        changed
    }

    /// Which desktop a window is on, by index; with a single desktop (Explorer lists none) the
    /// first one.
    fn desktop_index(&self, w: &Win) -> Option<usize> {
        if self.ids.len() < 2 {
            return Some(0);
        }
        self.ids.iter().position(|id| *id == w.desktop)
    }

    /// The pill's items, by what they are: each desktop's number, then its windows (all of them
    /// or, `compact`, only the current desktop's), then `+N` for those over `max_icons`.
    fn slots(&self, compact: bool) -> Vec<Slot> {
        let current = self.current.unwrap_or(0);
        let mut out = Vec::new();
        for d in 0..self.desktops.len().max(1) {
            out.push(Slot::Desktop(d));
            if !self.icons || (compact && d != current) {
                continue;
            }
            let mine: Vec<usize> = (0..self.windows.len()).filter(|&i| self.desktop_index(&self.windows[i]) == Some(d)).collect();
            out.extend(mine.iter().take(self.max_icons).map(|&i| Slot::Window(d, i)));
            if mine.len() > self.max_icons {
                out.push(Slot::More(d, mine.len() - self.max_icons));
            }
        }
        out
    }

    fn pill_of(&self, theme: &Theme, compact: bool) -> Option<Pill> {
        if self.desktops.len() < 2 && !self.single {
            return None;
        }
        let b = theme.base;
        self.light.set(0.2126 * b.0 + 0.7152 * b.1 + 0.0722 * b.2 > 0.5);
        let labels: Vec<String> = if self.desktops.is_empty() { vec!["1".into()] } else { self.desktops.clone() };
        let current = self.current.unwrap_or(0);
        let slots = self.slots(compact);
        let mut items = Vec::with_capacity(slots.len());
        for (n, slot) in slots.iter().enumerate() {
            let on = slot.desktop() == current;
            let color = if on { theme.on_primary } else { theme.subtext };
            // The icons close to their number and to each other; the desktops the pill's gap apart
            let space = if matches!(slots.get(n.wrapping_sub(1)), Some(Slot::Desktop(_))) { 6.0 } else { 4.0 };
            let item = match *slot {
                Slot::Desktop(d) => Item::text(labels.get(d).cloned().unwrap_or_else(|| (d + 1).to_string()), color).min_width(14.0).max_width(120.0),
                Slot::More(_, more) => Item::text(format!("+{more}"), color).small().space(space),
                Slot::Window(_, i) => {
                    let w = &self.windows[i];
                    let item = match self.cache.get(&w.app) {
                        Some(Icon::Image(image)) => Item::app_icon(image.clone(), ICON),
                        Some(Icon::Glyph(glyph, name)) => Item::icon(glyph, theme.color(name).unwrap_or(theme.text)).min_width(ICON),
                        Some(Icon::Letter(l)) => Item::text(l.as_str(), if on { color } else { theme.text }).min_width(ICON),
                        None => Item::text("?", color).min_width(ICON),
                    };
                    let item = item.space(space);
                    if on && w.hwnd == self.front { item.tile(theme.base.alpha(0.35)) } else { item }
                }
            };
            items.push(item);
        }
        let first = slots.iter().position(|s| s.desktop() == current).unwrap_or(0);
        let last = slots.iter().rposition(|s| s.desktop() == current).unwrap_or(first);
        Some(Pill::new(items).gap(10.0).highlight_span(first, last))
    }

    /// Goes to the desktop `target` with Windows' own shortcut: Ctrl+Win+← or → as many times as
    /// there are desktops in between, sent in one go so the user's typing can't get in the
    /// middle. Nothing while a listed game is in front: the keys are simulated, and an anti-cheat
    /// may watch them. The registry says when the desktop changed, and the highlight slides there.
    fn go_to(&mut self, target: usize) -> bool {
        let Some(current) = self.current else { return false };
        if target == current || target >= self.desktops.len() || self.game {
            return false;
        }
        if self.mock {
            self.current = Some(target);
            return true;
        }
        let keys = switch_keys(current, target);
        let sent = unsafe { SendInput(&keys, size_of::<INPUT>() as i32) };
        if sent as usize != keys.len() {
            log!("workspaces: Windows took {sent} of {} keys", keys.len());
        }
        false
    }

    /// Brings a window in front, restored if it was minimized. On another desktop, Windows goes
    /// there to show it. The bar may: it just got the click.
    fn activate(&mut self, i: usize) -> bool {
        let Some(w) = self.windows.get(i) else { return false };
        if self.mock {
            self.current = self.desktop_index(w).or(self.current);
            self.front = w.hwnd;
            return true;
        }
        unsafe {
            if IsIconic(w.hwnd).as_bool() {
                // Async: a window of another app that hangs doesn't hang the bar
                let _ = ShowWindowAsync(w.hwnd, SW_RESTORE);
            }
            if !SetForegroundWindow(w.hwnd).as_bool() {
                log!("workspaces: Windows didn't bring the window in front");
            }
        }
        false
    }
}

impl Drop for Workspaces {
    fn drop(&mut self) {
        self.detach();
        unsafe {
            if let Some(key) = self.key {
                let _ = RegCloseKey(key);
            }
            if !self.event.0.is_null() {
                let _ = CloseHandle(self.event);
            }
        }
    }
}

impl super::Widget for Workspaces {
    fn pill(&self, theme: &Theme) -> Option<Pill> {
        self.pill_of(theme, false)
    }

    /// The current desktop's icons alone.
    fn compact(&self, theme: &Theme) -> Option<Pill> {
        if !self.icons || self.windows.is_empty() {
            return None;
        }
        self.pill_of(theme, true)
    }

    fn handles(&self) -> Vec<HANDLE> {
        let registry = self.key.map(|_| self.event);
        let hook = self.hook.as_ref().and_then(|_| self.wake.handle());
        registry.into_iter().chain(hook).chain(self.retry.handle()).chain(self.logos.wake.handle()).collect()
    }

    fn signaled_at(&mut self, index: usize) -> bool {
        let Some(handle) = self.handles().get(index).copied() else { return false };
        if Some(handle) == self.logos.wake.handle() {
            return self.logos_read();
        }
        if Some(handle) == self.wake.handle() || Some(handle) == self.retry.handle() {
            return self.refresh(false);
        }
        let changed = self.read();
        self.watch();
        // A desktop that goes takes its windows to another: every window is asked again
        self.refresh(true) || changed
    }

    fn clickable(&self) -> bool {
        true
    }

    fn click(&mut self, item: Option<usize>, compact: bool) -> bool {
        match item.and_then(|i| self.slots(compact).get(i).copied()) {
            Some(Slot::Desktop(d) | Slot::More(d, _)) => self.go_to(d),
            Some(Slot::Window(_, i)) => self.activate(i),
            None => false,
        }
    }

    fn event(&mut self, signal: &super::Signal) -> bool {
        match signal {
            super::Signal::Game(game) => {
                self.game = *game;
                false
            }
            super::Signal::FrontWindow(hwnd) => self.front_changed(*hwnd),
            super::Signal::Shown(true) => {
                self.attach();
                // At the start too, before the first frame: the first listing comes after it
                self.listed && self.refresh(true)
            }
            super::Signal::Shown(false) => {
                self.detach();
                false
            }
            super::Signal::Resume => self.refresh(true),
            super::Signal::Displays => self.rescale(),
            _ => false,
        }
    }
}

/// The windows the taskbar would show, in Windows' order (the one in front first): visible, not
/// tool windows, without an owner (or app windows), not hidden by their app (hidden by the shell
/// is on another desktop: kept), with a title; none of this process.
fn taskbar_windows(own: u32) -> Vec<HWND> {
    unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
        let (own, list) = unsafe { &mut *(data.0 as *mut (u32, Vec<HWND>)) };
        if shown_in_taskbar(hwnd) && window_pid(hwnd) != Some(*own) {
            list.push(hwnd);
        }
        BOOL(1)
    }
    let mut data = (own, Vec::new());
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut data as *mut _ as isize));
    }
    data.1
}

fn shown_in_taskbar(hwnd: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return false;
        }
        let ex = WINDOW_EX_STYLE(GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32);
        let app = ex.contains(WS_EX_APPWINDOW);
        if ex.contains(WS_EX_TOOLWINDOW) || (ex.contains(WS_EX_NOACTIVATE) && !app) || (GetWindow(hwnd, GW_OWNER).is_ok() && !app) {
            return false;
        }
        // DWM_CLOAKED_SHELL (2) alone: on another desktop
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4);
        if cloaked != 0 && cloaked != 2 {
            return false;
        }
        GetWindowTextLengthW(hwnd) > 0
    }
}

/// The desktop a window is on; None for one on none (a UWP app's frame left in the background
/// has the null id) or that the manager doesn't know.
fn desktop_of(manager: &IVirtualDesktopManager, hwnd: HWND) -> Option<GUID> {
    unsafe { manager.GetWindowDesktopId(hwnd) }.ok().filter(|id| *id != GUID::zeroed())
}

fn window_pid(hwnd: HWND) -> Option<u32> {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    (pid != 0).then_some(pid)
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// An icon's side in pixels for the densest screen.
fn icon_px() -> u32 {
    unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let dpi = unsafe { &mut *(data.0 as *mut u32) };
        let (mut x, mut y) = (96u32, 96u32);
        if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) }.is_ok() {
            *dpi = (*dpi).max(x);
        }
        BOOL(1)
    }
    let mut dpi = 96u32;
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut dpi as *mut u32 as isize));
    }
    (ICON * dpi as f32 / 96.0).round().clamp(8.0, 128.0) as u32
}

/// The shell's messages about windows, on a message-only window of the main thread: it only
/// sets the widget's event, and the widget lists the windows when the loop gets to it. Many
/// messages before that (an app closing its windows) make one listing.
struct ShellHook {
    hwnd: HWND,
    state: Box<HookState>,
}

struct HookState {
    message: u32,
    wake: Arc<Wake>,
    /// The windows the widget knows: a new title only matters for one it doesn't.
    known: RefCell<HashSet<isize>>,
}

const HOOK_CLASS: PCWSTR = w!("RustyBarShellHook");

impl ShellHook {
    fn new(wake: Arc<Wake>) -> Option<ShellHook> {
        static CLASS: Once = Once::new();
        let instance = unsafe { GetModuleHandleW(None) }.ok()?;
        CLASS.call_once(|| unsafe {
            let class = WNDCLASSEXW { cbSize: size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(hook_proc), hInstance: instance.into(), lpszClassName: HOOK_CLASS, ..Default::default() };
            RegisterClassExW(&class);
        });
        let message = unsafe { RegisterWindowMessageW(w!("SHELLHOOK")) };
        let state = Box::new(HookState { message, wake, known: RefCell::new(HashSet::new()) });
        unsafe {
            let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), HOOK_CLASS, PCWSTR::null(), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(instance.into()), None)
                .map_err(|e| log!("workspaces: no window for the shell's messages ({e})"))
                .ok()?;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, &*state as *const HookState as isize);
            if message == 0 || !RegisterShellHookWindow(hwnd).as_bool() {
                log!("workspaces: the shell doesn't send its messages");
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                let _ = DestroyWindow(hwnd);
                return None;
            }
            Some(ShellHook { hwnd, state })
        }
    }
}

impl Drop for ShellHook {
    fn drop(&mut self) {
        unsafe {
            let _ = DeregisterShellHookWindow(self.hwnd);
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
        }
        let _ = &self.state;
    }
}

unsafe extern "system" fn hook_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let state = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const HookState;
    if let Some(state) = unsafe { state.as_ref() }
        && msg == state.message
    {
        let relevant = match wparam.0 & 0x7FFF {
            // HSHELL_WINDOWCREATED, HSHELL_WINDOWDESTROYED, HSHELL_WINDOWREPLACED
            1 | 2 | 13 => true,
            // HSHELL_REDRAW: a title changed; for a window without one until now, it may show.
            // (A call to Explorer may pump messages while the list is being made: then it counts)
            6 => state.known.try_borrow().map(|k| !k.contains(&lparam.0)).unwrap_or(true),
            _ => false,
        };
        if relevant {
            state.wake.set();
        }
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Ctrl and Win down, ← or → pressed once per desktop between `from` and `to`, Win and Ctrl up.
fn switch_keys(from: usize, to: usize) -> Vec<INPUT> {
    let key = |vk: VIRTUAL_KEY, up: bool, extended: bool| {
        let mut flags = if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) };
        if extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } } }
    };
    let arrow = if to > from { VK_RIGHT } else { VK_LEFT };
    let mut keys = vec![key(VK_LCONTROL, false, false), key(VK_LWIN, false, true)];
    for _ in 0..from.abs_diff(to) {
        keys.push(key(arrow, false, true));
        keys.push(key(arrow, true, true));
    }
    keys.push(key(VK_LWIN, true, true));
    keys.push(key(VK_LCONTROL, true, false));
    keys
}

fn binary(key: HKEY, sub: Option<&HSTRING>, value: PCWSTR) -> Option<Vec<u8>> {
    let sub = sub.map(|s| PCWSTR(s.as_ptr())).unwrap_or(PCWSTR::null());
    let mut size = 0u32;
    unsafe {
        RegGetValueW(key, sub, value, RRF_RT_REG_BINARY, None, None, Some(&mut size)).ok().ok()?;
        let mut data = vec![0u8; size as usize];
        RegGetValueW(key, sub, value, RRF_RT_REG_BINARY, None, Some(data.as_mut_ptr() as *mut _), Some(&mut size)).ok().ok()?;
        data.truncate(size as usize);
        Some(data)
    }
}

fn string(key: HKEY, sub: &HSTRING, value: PCWSTR) -> Option<String> {
    let mut buf = [0u16; 256];
    let mut size = (buf.len() * 2) as u32;
    unsafe {
        RegGetValueW(key, sub, value, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size)).ok().ok()?;
    }
    let len = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// A GUID as the registry keeps it (the bytes of the structure) to the structure.
fn guid_of(b: &[u8; 16]) -> GUID {
    GUID::from_values(
        u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        u16::from_le_bytes([b[4], b[5]]),
        u16::from_le_bytes([b[6], b[7]]),
        [b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]],
    )
}

/// A GUID as the registry writes it in key names: `{07AC4F77-AAC2-4287-B9AA-2A2F7086CC6A}`.
fn guid(b: &[u8; 16]) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        u16::from_le_bytes([b[4], b[5]]),
        u16::from_le_bytes([b[6], b[7]]),
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::Widget;

    #[test]
    fn guid_as_in_the_registry() {
        let bytes = [0x77, 0x4F, 0xAC, 0x07, 0xC2, 0xAA, 0x87, 0x42, 0xB9, 0xAA, 0x2A, 0x2F, 0x70, 0x86, 0xCC, 0x6A];
        assert_eq!(guid(&bytes), "{07AC4F77-AAC2-4287-B9AA-2A2F7086CC6A}");
        assert_eq!(guid_of(&bytes), GUID::from_u128(0x07AC4F77_AAC2_4287_B9AA_2A2F7086CC6A));
    }

    #[test]
    fn keys_to_switch_desktops() {
        let keys = switch_keys(0, 2);
        let vk: Vec<u16> = keys.iter().map(|k| unsafe { k.Anonymous.ki.wVk.0 }).collect();
        assert_eq!(vk, [VK_LCONTROL.0, VK_LWIN.0, VK_RIGHT.0, VK_RIGHT.0, VK_RIGHT.0, VK_RIGHT.0, VK_LWIN.0, VK_LCONTROL.0]);
        // Every key that goes down comes up
        let ups = keys.iter().filter(|k| unsafe { k.Anonymous.ki.dwFlags.contains(KEYEVENTF_KEYUP) }).count();
        assert_eq!(ups * 2, keys.len());
        assert_eq!(unsafe { switch_keys(3, 2)[2].Anonymous.ki.wVk }, VK_LEFT);
    }

    /// Three desktops, five windows (two on the first, three on the second), one in front.
    fn made_up(max_icons: usize) -> Workspaces {
        let mut w = Workspaces::blank(&Options::default());
        w.mock = true;
        w.max_icons = max_icons;
        w.desktops = vec!["1".into(), "2".into(), "Games".into()];
        w.ids = (1..=3).map(GUID::from_u128).collect();
        w.current = Some(1);
        w.windows = [0u128, 1, 1, 0, 1]
            .iter()
            .enumerate()
            .map(|(i, d)| Win { hwnd: HWND((i + 1) as *mut _), desktop: GUID::from_u128(d + 1), app: format!("a{i}") })
            .collect();
        for i in 0..5 {
            w.cache.insert(format!("a{i}"), Icon::Letter(format!("{i}")));
        }
        w.front = HWND(3 as *mut _);
        w
    }

    #[test]
    fn each_desktop_with_its_windows() {
        let w = made_up(8);
        let full = w.slots(false);
        use Slot::*;
        assert_eq!(full, [Desktop(0), Window(0, 0), Window(0, 3), Desktop(1), Window(1, 1), Window(1, 2), Window(1, 4), Desktop(2)]);
        // Compact: the current desktop's windows alone
        assert_eq!(w.slots(true), [Desktop(0), Desktop(1), Window(1, 1), Window(1, 2), Window(1, 4), Desktop(2)]);
        // Over max_icons, the rest as +N
        assert_eq!(made_up(2).slots(false)[3..8], [Desktop(1), Window(1, 1), Window(1, 2), More(1, 1), Desktop(2)]);
    }

    #[test]
    fn the_highlight_covers_the_current_desktop_and_the_tile_the_window_in_front() {
        let w = made_up(8);
        let theme = Theme::from_config(&crate::config::ThemeConfig::default());
        let pill = w.pill(&theme).unwrap();
        assert_eq!((pill.highlight, pill.highlight_end), (Some(3), Some(6)));
        // The window in front (the third, on the current desktop) on its tile, alone
        let tiles: Vec<usize> = pill.items.iter().enumerate().filter(|(_, i)| i.tile.is_some()).map(|(n, _)| n).collect();
        assert_eq!(tiles, [5]);
        let compact = w.compact(&theme).unwrap();
        assert_eq!((compact.highlight, compact.highlight_end), (Some(1), Some(4)));
    }

    #[test]
    fn clicks_on_numbers_and_icons() {
        let mut w = made_up(8);
        // An icon on the first desktop: there, with that window in front
        assert!(w.click(Some(2), false));
        assert_eq!((w.current, w.front), (Some(0), HWND(4 as *mut _)));
        // Compact, the other desktops have no icons: the fourth item is the second desktop's
        // number, where the full pill has an icon of the first
        assert_eq!(w.slots(false)[3], Slot::Desktop(1));
        assert!(w.click(Some(2), true));
        assert_eq!((w.current, w.front), (Some(0), HWND(4 as *mut _)));
        assert!(w.click(Some(3), true));
        assert_eq!(w.current, Some(1));
        assert!(w.click(Some(7), false));
        assert_eq!(w.current, Some(2));
        assert!(!w.click(None, false));
    }

    #[test]
    fn numbers_alone_without_icons() {
        let mut w = made_up(8);
        w.icons = false;
        assert_eq!(w.slots(false), [Slot::Desktop(0), Slot::Desktop(1), Slot::Desktop(2)]);
        assert!(w.compact(&Theme::from_config(&crate::config::ThemeConfig::default())).is_none());
    }
}
