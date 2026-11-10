//! One bar per screen: a window at the top that reserves its strip as an AppBar (maximized windows
//! stop under it, as with the taskbar) and hides itself while an app is full screen there. What it
//! shows is a DirectComposition tree: per pill a visual whose content is the background, with the
//! text in a child visual, so that a new minute redraws only the text; a pill that moves, and the
//! highlight inside one, slide to their place with an animation DWM runs by itself.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Once;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Direct2D::{D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ROUNDED_RECT};
use windows::Win32::Graphics::DirectComposition::{IDCompositionSurface, IDCompositionTarget, IDCompositionVisual2};
use windows::Win32::Graphics::Gdi::{HMONITOR, MONITOR_DEFAULTTONULL, MonitorFromWindow};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    ABE_TOP, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, APPBARDATA, QUNS_RUNNING_D3D_FULL_SCREEN, SHAppBarMessage,
    SHQueryUserNotificationState,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWL_STYLE, GetClassNameW, GetForegroundWindow, GetWindowLongW, GetWindowRect,
    HWND_TOPMOST, IDC_ARROW, LoadCursorW, MA_NOACTIVATE, RegisterClassExW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
    SetWindowPos, ShowWindow, WM_APP, WM_DPICHANGED, WM_LBUTTONDOWN, WM_MOUSEACTIVATE, WNDCLASSEXW, WS_CAPTION, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{Result, w};
use windows_numerics::Vector2;

use crate::app::{Event, push};
use crate::gfx::{self, Gfx};
use crate::log;
use crate::theme::Theme;
use crate::ui::Measured;

/// The message the shell sends AppBar notifications with.
pub const WM_APPBAR: u32 = WM_APP + 1;

/// How far the highlight reaches past its item, in DIPs, sideways and up and down.
const HIGHLIGHT_X: f32 = 5.0;
const HIGHLIGHT_Y: f32 = 4.0;

/// The pills of the three groups, measured once for every screen.
pub struct Sides {
    pub left: Vec<Measured>,
    pub center: Vec<Measured>,
    pub right: Vec<Measured>,
}

struct Comp {
    // Holds the tree on the window
    _target: IDCompositionTarget,
    root: IDCompositionVisual2,
}

struct Highlight {
    visual: IDCompositionVisual2,
    surface: IDCompositionSurface,
    size: (u32, u32),
    color: u32,
    x: f32,
}

struct PillVisual {
    visual: IDCompositionVisual2,
    background: IDCompositionSurface,
    text_visual: IDCompositionVisual2,
    text: IDCompositionSurface,
    size: (u32, u32),
    background_sig: u64,
    text_sig: u64,
    x: f32,
    highlight: Option<Highlight>,
}

pub struct Bar {
    pub hwnd: HWND,
    pub monitor: HMONITOR,
    /// The whole screen, in pixels.
    pub screen: RECT,
    pub scale: f32,
    height: i32,
    registered: bool,
    /// The strip is still to reserve (see `reserve`).
    unreserved: bool,
    pub visible: bool,
    comp: Option<Comp>,
    pills: HashMap<String, PillVisual>,
}

const CLASS: windows::core::PCWSTR = w!("RustyBarBar");

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_APPBAR => {
            push(Event::AppBar { hwnd, code: wparam.0 as u32, on: lparam.0 != 0 });
            LRESULT(0)
        }
        WM_DPICHANGED => {
            push(Event::Dpi { hwnd, dpi: (wparam.0 & 0xffff) as u32 });
            LRESULT(0)
        }
        // A click on the bar leaves the window in front where it is
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONDOWN => {
            push(Event::Click { hwnd, x: (lparam.0 & 0xffff) as i16 as i32 });
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn register_class() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&class);
    });
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// The desktop and the taskbars: in front, but never full screen apps.
pub fn is_shell(hwnd: HWND) -> bool {
    matches!(class_name(hwnd).as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
}

impl Bar {
    pub fn new(monitor: HMONITOR, screen: RECT, dpi: u32, height_dip: f32, reserve: bool) -> Result<Bar> {
        register_class();
        let scale = dpi as f32 / 96.0;
        let height = (height_dip * scale).round() as i32;
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_NOREDIRECTIONBITMAP,
                CLASS,
                w!("RustyBar"),
                WS_POPUP,
                screen.left,
                screen.top,
                screen.right - screen.left,
                height,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                None,
            )?
        };
        let mut bar = Bar {
            hwnd,
            monitor,
            screen,
            scale,
            height,
            registered: false,
            unreserved: reserve,
            visible: false,
            comp: None,
            pills: HashMap::new(),
        };
        bar.place();
        Ok(bar)
    }

    /// Registers the AppBar and reserves the strip, the first time it's called. The app calls it
    /// after the first frame: the shell takes a while to move the windows out of the way, and the
    /// bar shows meanwhile.
    pub fn reserve(&mut self) {
        if !self.unreserved {
            return;
        }
        self.unreserved = false;
        let mut data = self.appbar_data();
        self.registered = unsafe { SHAppBarMessage(ABM_NEW, &mut data) } != 0;
        if self.registered {
            self.place();
        } else {
            log!("the shell refused the AppBar of a screen: the bar won't reserve its strip there");
        }
    }

    fn appbar_data(&self) -> APPBARDATA {
        APPBARDATA {
            cbSize: size_of::<APPBARDATA>() as u32,
            hWnd: self.hwnd,
            uCallbackMessage: WM_APPBAR,
            uEdge: ABE_TOP,
            ..Default::default()
        }
    }

    /// Takes the top strip of the screen: asks the shell where it may go (another AppBar may be
    /// there already), reserves it, and moves the window there.
    pub fn place(&mut self) {
        let mut rect = RECT { left: self.screen.left, top: self.screen.top, right: self.screen.right, bottom: self.screen.top + self.height };
        if self.registered {
            let mut data = self.appbar_data();
            data.rc = rect;
            unsafe { SHAppBarMessage(ABM_QUERYPOS, &mut data) };
            data.rc.bottom = data.rc.top + self.height;
            unsafe { SHAppBarMessage(ABM_SETPOS, &mut data) };
            rect = data.rc;
        }
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOACTIVATE,
            );
        }
    }

    pub fn set_dpi(&mut self, dpi: u32, height_dip: f32) {
        self.scale = dpi as f32 / 96.0;
        self.height = (height_dip * self.scale).round() as i32;
        self.place();
        // Every surface has to be drawn again at the new scale
        if let Some(comp) = &self.comp {
            unsafe {
                let _ = comp.root.RemoveAllVisuals();
            }
        }
        self.pills.clear();
    }

    /// Whether the app in front covers this whole screen without a title bar (a game, a video, a
    /// browser in F11), or Windows says a Direct3D app holds the screen in exclusive mode.
    pub fn fullscreen_here(&self) -> bool {
        unsafe {
            let front = GetForegroundWindow();
            if front.0.is_null() || is_shell(front) || MonitorFromWindow(front, MONITOR_DEFAULTTONULL) != self.monitor {
                return false;
            }
            let mut r = RECT::default();
            if GetWindowRect(front, &mut r).is_err() {
                return false;
            }
            let s = self.screen;
            let covers = r.left <= s.left && r.top <= s.top && r.right >= s.right && r.bottom >= s.bottom;
            // A maximized window can cover the screen too, with its invisible borders: it has a title bar
            let titled = GetWindowLongW(front, GWL_STYLE) as u32 & WS_CAPTION.0 == WS_CAPTION.0;
            (covers && !titled) || SHQueryUserNotificationState().is_ok_and(|s| s == QUNS_RUNNING_D3D_FULL_SCREEN)
        }
    }

    pub fn set_visible(&mut self, visible: bool) {
        if visible != self.visible {
            self.visible = visible;
            unsafe {
                let _ = ShowWindow(self.hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
            }
        }
    }

    /// Connects the window to the composition device.
    pub fn attach(&mut self, gfx: &Gfx) -> Result<()> {
        unsafe {
            let target = gfx.dcomp.CreateTargetForHwnd(self.hwnd, true)?;
            let root = gfx.dcomp.CreateVisual()?;
            target.SetRoot(&root)?;
            self.comp = Some(Comp { _target: target, root });
        }
        self.pills.clear();
        Ok(())
    }

    /// Lets go of every surface and of the composition target, so the device can go.
    pub fn release(&mut self) {
        self.pills.clear();
        self.comp = None;
    }

    /// Lays out the three groups on this screen and brings the visual tree up to date: a pill
    /// whose size or colors changed gets its background drawn again, one whose text changed its
    /// text, and one that moved slides (with `animate`) or jumps to its place.
    pub fn render(&mut self, gfx: &Gfx, theme: &Theme, sides: &Sides, margin: f32, animate: bool) -> Result<()> {
        if !self.visible {
            return Ok(());
        }
        let Some(comp) = &self.comp else { return Ok(()) };
        let s = self.scale;
        let width = (self.screen.right - self.screen.left) as f32 / s;
        let height = self.height as f32 / s;
        let gap = theme.item_gap;
        let total = |pills: &[Measured]| pills.iter().map(|p| p.width).sum::<f32>() + gap * pills.len().saturating_sub(1) as f32;

        let mut places: Vec<(&Measured, f32)> = Vec::new();
        let mut x = margin;
        for p in &sides.left {
            places.push((p, x));
            x += p.width + gap;
        }
        let mut x = width - margin - total(&sides.right);
        for p in &sides.right {
            places.push((p, x));
            x += p.width + gap;
        }
        let mut x = (width - total(&sides.center)) / 2.0;
        for p in &sides.center {
            places.push((p, x));
            x += p.width + gap;
        }

        let y = ((height - theme.pill_height) / 2.0 * s).round();
        let mut background = DefaultHasher::new();
        (theme.base.bits(), theme.surface.bits(), theme.pill_radius.to_bits(), theme.pill_border.to_bits()).hash(&mut background);
        let background = background.finish();

        let mut seen: Vec<&str> = Vec::with_capacity(places.len());
        for (m, x_dip) in places {
            seen.push(&m.key);
            let left = (x_dip * s).round();
            let size = (((x_dip + m.width) * s).round() - left) as u32;
            let size = (size, (theme.pill_height * s).round() as u32);
            let background_sig = {
                let mut h = DefaultHasher::new();
                (background, size).hash(&mut h);
                h.finish()
            };
            let text_sig = {
                let mut h = DefaultHasher::new();
                (m.text_sig, size).hash(&mut h);
                h.finish()
            };
            let fresh = !self.pills.contains_key(&m.key);
            if fresh {
                unsafe {
                    let visual = gfx.dcomp.CreateVisual()?;
                    let text_visual = gfx.dcomp.CreateVisual()?;
                    let background = gfx.surface(size.0, size.1)?;
                    let text = gfx.surface(size.0, size.1)?;
                    visual.SetContent(&background)?;
                    text_visual.SetContent(&text)?;
                    visual.AddVisual(&text_visual, true, None)?;
                    visual.SetOffsetX2(left)?;
                    visual.SetOffsetY2(y)?;
                    comp.root.AddVisual(&visual, true, None)?;
                    self.pills.insert(
                        m.key.clone(),
                        PillVisual { visual, background, text_visual, text, size, background_sig: 0, text_sig: 0, x: left, highlight: None },
                    );
                }
            }
            let pv = self.pills.get_mut(&m.key).expect("just inserted");
            if pv.size != size {
                pv.background = gfx.surface(size.0, size.1)?;
                pv.text = gfx.surface(size.0, size.1)?;
                unsafe {
                    pv.visual.SetContent(&pv.background)?;
                    pv.text_visual.SetContent(&pv.text)?;
                }
                pv.size = size;
                pv.background_sig = 0;
                pv.text_sig = 0;
            }
            if pv.background_sig != background_sig {
                draw_background(&pv.background, s, theme, size)?;
                pv.background_sig = background_sig;
            }
            if pv.text_sig != text_sig {
                draw_text(&pv.text, s, theme, m)?;
                pv.text_sig = text_sig;
            }
            highlight(gfx, pv, m, theme, s, animate && !fresh)?;
            if pv.x != left {
                unsafe {
                    if animate {
                        pv.visual.SetOffsetX(&gfx.slide(pv.x, left)?)?;
                    } else {
                        pv.visual.SetOffsetX2(left)?;
                    }
                    pv.visual.SetOffsetY2(y)?;
                }
                pv.x = left;
            }
        }
        let gone: Vec<String> = self.pills.keys().filter(|k| !seen.contains(&k.as_str())).cloned().collect();
        for key in gone {
            if let Some(pv) = self.pills.remove(&key) {
                unsafe {
                    let _ = comp.root.RemoveVisual(&pv.visual);
                }
            }
        }
        Ok(())
    }

    /// The pill under `x` (client pixels), by its widget's name. The bar's whole height counts,
    /// up to the screen's edge, where the mouse stops by itself.
    pub fn pill_at(&self, x: i32) -> Option<String> {
        let x = x as f32;
        self.pills.iter().find(|(_, pv)| x >= pv.x && x < pv.x + pv.size.0 as f32).map(|(key, _)| key.clone())
    }

    /// A pill's left and right edges, and the bar's bottom, in screen pixels.
    pub fn pill_span(&self, key: &str) -> Option<(i32, i32, i32)> {
        let pv = self.pills.get(key)?;
        let mut r = RECT::default();
        unsafe { GetWindowRect(self.hwnd, &mut r).ok()? };
        let left = r.left + pv.x as i32;
        Some((left, left + pv.size.0 as i32, r.bottom))
    }

    fn unregister(&mut self) {
        if self.registered {
            let mut data = self.appbar_data();
            unsafe { SHAppBarMessage(ABM_REMOVE, &mut data) };
            self.registered = false;
        }
    }
}

impl Drop for Bar {
    fn drop(&mut self) {
        self.release();
        self.unregister();
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

fn draw_background(surface: &IDCompositionSurface, s: f32, theme: &Theme, size: (u32, u32)) -> Result<()> {
    let (w, h) = (size.0 as f32 / s, size.1 as f32 / s);
    gfx::draw(surface, s, |ctx, brush| unsafe {
        let b = theme.pill_border;
        // The border inside the pill's edge, so the pill keeps its size
        let shape = D2D1_ROUNDED_RECT { rect: gfx::rect(b / 2.0, b / 2.0, w - b, h - b), radiusX: theme.pill_radius - b / 2.0, radiusY: theme.pill_radius - b / 2.0 };
        brush.SetColor(&gfx::color(theme.base));
        ctx.FillRoundedRectangle(&shape, brush);
        if b > 0.0 {
            brush.SetColor(&gfx::color(theme.surface));
            ctx.DrawRoundedRectangle(&shape, brush, b, None);
        }
    })
}

fn draw_text(surface: &IDCompositionSurface, s: f32, theme: &Theme, m: &Measured) -> Result<()> {
    let height = theme.pill_height;
    gfx::draw(surface, s, |ctx, brush| unsafe {
        for (item, placed) in m.pill.items.iter().zip(&m.items) {
            brush.SetColor(&gfx::color(item.color));
            let x = placed.x + (placed.width - placed.text_width) / 2.0;
            // Snapped to whole pixels: text between pixels looks blurred
            let y = ((height - placed.text_height) / 2.0 * s).round() / s;
            ctx.DrawTextLayout(Vector2 { X: (x * s).round() / s, Y: y }, &placed.layout, brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
    })
}

/// The tile under the highlighted item: drawn when its size or color change, slid along when the
/// highlight moves to another item.
fn highlight(gfx: &Gfx, pv: &mut PillVisual, m: &Measured, theme: &Theme, s: f32, animate: bool) -> Result<()> {
    let Some(index) = m.pill.highlight.filter(|i| *i < m.items.len()) else {
        if let Some(h) = pv.highlight.take() {
            unsafe {
                let _ = pv.visual.RemoveVisual(&h.visual);
            }
        }
        return Ok(());
    };
    let item = &m.items[index];
    let size = (((item.width + 2.0 * HIGHLIGHT_X) * s).round() as u32, ((theme.pill_height - 2.0 * HIGHLIGHT_Y) * s).round() as u32);
    let x = ((item.x - HIGHLIGHT_X) * s).round();
    let color = theme.primary.bits();
    if pv.highlight.is_none() {
        unsafe {
            let visual = gfx.dcomp.CreateVisual()?;
            let surface = gfx.surface(size.0, size.1)?;
            visual.SetContent(&surface)?;
            visual.SetOffsetX2(x)?;
            visual.SetOffsetY2((HIGHLIGHT_Y * s).round())?;
            // Over the background, under the text
            pv.visual.AddVisual(&visual, false, &pv.text_visual)?;
            pv.highlight = Some(Highlight { visual, surface, size: (0, 0), color: 0, x });
        }
    }
    let h = pv.highlight.as_mut().expect("just made");
    if h.size != size || h.color != color {
        if h.size != size {
            h.surface = gfx.surface(size.0, size.1)?;
            unsafe { h.visual.SetContent(&h.surface)? };
        }
        let (w, hh) = (size.0 as f32 / s, size.1 as f32 / s);
        let fill: D2D1_COLOR_F = gfx::color(theme.primary);
        gfx::draw(&h.surface, s, |ctx, brush| unsafe {
            brush.SetColor(&fill);
            let r = hh / 2.0;
            ctx.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: gfx::rect(0.0, 0.0, w, hh), radiusX: r, radiusY: r }, brush);
        })?;
        h.size = size;
        h.color = color;
    }
    if h.x != x {
        unsafe {
            if animate {
                h.visual.SetOffsetX(&gfx.slide(h.x, x)?)?;
            } else {
                h.visual.SetOffsetX2(x)?;
            }
        }
        h.x = x;
    }
    Ok(())
}
