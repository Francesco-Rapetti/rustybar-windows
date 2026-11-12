//! A widget's popup: a window under its pill, made when it opens and destroyed when it closes,
//! that shows the widget's scene (`ui::Scene`). Unlike the bar it takes the front, as the
//! taskbar's flyouts do, so that Windows itself says when the user clicks anywhere else
//! (`WM_ACTIVATE`): no mouse hook. It slides down and fades in, and fades out, with animations DWM
//! runs by itself. Its content is drawn only when the scene changes; the hover tint is a visual
//! of its own that moves from zone to zone without drawing anything again.

use std::cell::Cell;
use std::sync::Once;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_BRUSH_PROPERTIES1, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_EXTEND_MODE_CLAMP,
    D2D1_INTERPOLATION_MODE_LINEAR, D2D1_ROUNDED_RECT, ID2D1DeviceContext, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectComposition::{IDCompositionSurface, IDCompositionTarget, IDCompositionVisual2, IDCompositionVisual3};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, HWND_TOPMOST, IDC_ARROW, KillTimer, LoadCursorW, RegisterClassExW, SW_SHOW,
    SWP_NOACTIVATE, SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow, WA_INACTIVE, WM_ACTIVATE, WM_CAPTURECHANGED, WM_CLOSE,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_TIMER, WNDCLASSEXW, WS_EX_NOREDIRECTIONBITMAP,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{Interface, PCWSTR, Result, w};
use windows_numerics::{Matrix3x2, Vector2};

use crate::app::{Event, push};
use crate::gfx::{self, Gfx};
use crate::log;
use crate::theme::{Rgba, Theme};
use crate::ui::{Align, Backdrop, Fonts, Image, Rect, Scene, Shape, Zone};

const CLASS: PCWSTR = w!("RustyBarPopup");
/// Not in the parts of the `windows` crate the bar uses (it's in `Win32_UI_Controls`).
const WM_MOUSELEAVE: u32 = 0x02a3;
const FADE_TIMER: usize = 1;
/// How far the popup slides down as it appears, in DIPs, and in how long, in seconds.
const DROP: f32 = 24.0;
const DROP_TIME: f64 = 0.25;
const FADE_IN: f64 = 0.12;
const FADE_OUT: f64 = 0.1;
/// The hover tint: the text color, this opaque.
const TINT: f32 = 0.08;

thread_local! {
    /// Windows has been asked to say when the mouse leaves the popup.
    static TRACKING: Cell<bool> = const { Cell::new(false) };
}

fn point(l: LPARAM) -> (i32, i32) {
    ((l.0 & 0xffff) as i16 as i32, ((l.0 >> 16) & 0xffff) as i16 as i32)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // A click anywhere else, Alt+Tab, another window coming in front
        WM_ACTIVATE if (wparam.0 & 0xffff) as u32 == WA_INACTIVE => push(Event::PopupInactive(hwnd)),
        WM_MOUSEMOVE => {
            if !TRACKING.get() {
                let mut track =
                    TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                TRACKING.set(unsafe { TrackMouseEvent(&mut track) }.is_ok());
            }
            push(Event::PopupMouse { hwnd, at: Some(point(lparam)), click: false });
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            push(Event::PopupMouse { hwnd, at: Some(point(lparam)), click: true });
            return LRESULT(0);
        }
        // The end of a drag on a slider: the mouse went up, or something took it away
        WM_LBUTTONUP | WM_CAPTURECHANGED => {
            push(Event::PopupRelease(hwnd));
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            TRACKING.set(false);
            push(Event::PopupMouse { hwnd, at: None, click: false });
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            push(Event::PopupWheel { hwnd, delta: ((wparam.0 >> 16) & 0xffff) as i16 as i32 });
            return LRESULT(0);
        }
        WM_KEYDOWN => {
            push(Event::PopupKey { hwnd, key: wparam.0 as u16 });
            return LRESULT(0);
        }
        // Alt+F4 closes the popup as Esc does, instead of destroying the window under the app
        WM_CLOSE => {
            push(Event::PopupKey { hwnd, key: VK_ESCAPE.0 });
            return LRESULT(0);
        }
        WM_TIMER if wparam.0 == FADE_TIMER => {
            unsafe {
                let _ = KillTimer(Some(hwnd), FADE_TIMER);
            }
            push(Event::PopupGone(hwnd));
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
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

/// The side of the bar a pill is on: its popup lines up with the pill's edge on that side.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Side {
    Left,
    Center,
    Right,
}

/// Where a popup goes, in screen pixels.
#[derive(Clone, Copy, Debug)]
pub struct Anchor {
    /// The pill's left and right edges.
    pub left: i32,
    pub right: i32,
    /// The popup's top: the bottom of the bar, plus the gap.
    pub top: i32,
    pub side: Side,
    pub screen: RECT,
    /// How far the popup stays from the screen's edges.
    pub margin: i32,
}

impl Anchor {
    /// The top left corner of a popup `width` pixels wide: under the pill, inside the screen.
    pub fn place(&self, width: i32) -> (i32, i32) {
        let x = match self.side {
            Side::Left => self.left,
            Side::Right => self.right - width,
            Side::Center => (self.left + self.right - width) / 2,
        };
        let min = self.screen.left + self.margin;
        let max = (self.screen.right - self.margin - width).max(min);
        (x.clamp(min, max), self.top)
    }
}

struct Comp {
    // Holds the tree on the window
    _target: IDCompositionTarget,
    /// Its content is the popup; it's what slides and fades.
    root: IDCompositionVisual2,
    surface: IDCompositionSurface,
    hover: IDCompositionVisual2,
    hover_surface: Option<IDCompositionSurface>,
    /// The size, corners and color the hover tint is drawn with.
    hover_key: (u32, u32, u32, u32),
    hover_shown: bool,
}

pub struct Popup {
    /// The widget's name in `[layout]`.
    pub widget: String,
    /// The bar it opened from.
    pub bar: HWND,
    pub hwnd: HWND,
    /// The window that was in front before: it comes back when the popup closes by itself (Esc,
    /// the pill), not when the user went somewhere else.
    pub previous: HWND,
    pub fonts: Fonts,
    scale: f32,
    anchor: Anchor,
    comp: Option<Comp>,
    scene: Scene,
    sig: u64,
    size: (u32, u32),
    /// The mouse, in DIPs, while it's over the popup.
    mouse: Option<(f32, f32)>,
    hover: Option<(Rect, f32)>,
    wheel: i32,
    /// The slider being dragged, from the press until the mouse goes up.
    drag: Option<Zone>,
}

impl Popup {
    pub fn new(widget: &str, bar: HWND, previous: HWND, anchor: Anchor, scale: f32, theme: &Theme) -> Result<Popup> {
        register_class();
        let fonts = Fonts::new(theme)?;
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOREDIRECTIONBITMAP,
                CLASS,
                w!("RustyBar"),
                WS_POPUP,
                anchor.left,
                anchor.top,
                1,
                1,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                None,
            )?
        };
        Ok(Popup {
            widget: widget.to_string(),
            bar,
            hwnd,
            previous,
            fonts,
            scale,
            anchor,
            comp: None,
            scene: Scene::default(),
            sig: 0,
            size: (0, 0),
            mouse: None,
            hover: None,
            wheel: 0,
            drag: None,
        })
    }

    /// Brings the popup up to `scene`: a new size moves the window and takes a new surface, a
    /// scene that draws something else is drawn again, and the hover tint follows its zones.
    pub fn render(&mut self, gfx: &Gfx, theme: &Theme, scene: Scene) -> Result<()> {
        let s = self.scale;
        let size = ((scene.width * s).ceil() as u32, (scene.height * s).ceil() as u32);
        let sig = {
            use std::hash::{DefaultHasher, Hash, Hasher};
            let mut h = DefaultHasher::new();
            scene.signature().hash(&mut h);
            (theme.base.bits(), theme.surface.bits(), theme.popup_radius.to_bits(), theme.popup_border.to_bits()).hash(&mut h);
            h.finish()
        };
        if self.comp.is_none() {
            unsafe {
                let target = gfx.dcomp.CreateTargetForHwnd(self.hwnd, true)?;
                let root = gfx.dcomp.CreateVisual()?;
                let surface = gfx.surface(size.0, size.1)?;
                root.SetContent(&surface)?;
                target.SetRoot(&root)?;
                let hover = gfx.dcomp.CreateVisual()?;
                self.comp = Some(Comp { _target: target, root, surface, hover, hover_surface: None, hover_key: (0, 0, 0, 0), hover_shown: false });
            }
            self.size = size;
            self.sig = 0;
            self.position();
        }
        if self.size != size {
            if let Some(comp) = &mut self.comp {
                comp.surface = gfx.surface(size.0, size.1)?;
                unsafe { comp.root.SetContent(&comp.surface)? };
            }
            self.size = size;
            self.sig = 0;
            self.position();
        }
        if self.sig != sig
            && let Some(comp) = &self.comp
        {
            draw(&comp.surface, s, theme, &scene, &self.fonts)?;
            self.sig = sig;
        }
        self.scene = scene;
        self.update_hover(gfx, theme)?;
        Ok(())
    }

    fn position(&self) {
        let (w, h) = (self.size.0 as i32, self.size.1 as i32);
        let (x, y) = self.anchor.place(w);
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
        }
    }

    /// Shows the drawn popup, sliding down and fading in with `animate`, and brings it in front.
    pub fn show(&self, gfx: &Gfx, animate: bool) -> Result<()> {
        let Some(comp) = &self.comp else { return Ok(()) };
        unsafe {
            if animate {
                comp.root.SetOffsetY(&gfx.ease_out(-(DROP * self.scale).round(), 0.0, DROP_TIME)?)?;
                comp.root.cast::<IDCompositionVisual3>()?.SetOpacity(&gfx.linear(0.0, 1.0, FADE_IN)?)?;
            }
            gfx.commit()?;
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            // Allowed: the user just clicked the bar, so this process had the last input
            if !SetForegroundWindow(self.hwnd).as_bool() {
                log!("the popup couldn't come in front: a click on its pill closes it");
            }
        }
        Ok(())
    }

    /// Starts fading out; the window goes when the fade is over (`Event::PopupGone`). An error
    /// means no fade: the caller drops the popup at once.
    pub fn fade_out(&self, gfx: &Gfx) -> Result<()> {
        let Some(comp) = &self.comp else { return Err(windows::Win32::Foundation::E_FAIL.into()) };
        unsafe {
            comp.root.cast::<IDCompositionVisual3>()?.SetOpacity(&gfx.linear(1.0, 0.0, FADE_OUT)?)?;
            // No more clicks while it fades
            let _ = EnableWindow(self.hwnd, false);
            SetTimer(Some(self.hwnd), FADE_TIMER, (FADE_OUT * 1000.0) as u32 + 30, None);
        }
        Ok(())
    }

    /// The mouse moved to `at` (client pixels), or left the popup. True if the hover tint moved,
    /// for the caller to commit.
    pub fn mouse(&mut self, gfx: &Gfx, theme: &Theme, at: Option<(i32, i32)>) -> Result<bool> {
        self.mouse = at.map(|(x, y)| (x as f32 / self.scale, y as f32 / self.scale));
        self.update_hover(gfx, theme)
    }

    /// A press at `at` (client pixels): the action of the zone there, and for a slider where
    /// along it (the drag starts, and the popup keeps the mouse until it goes up).
    pub fn press(&mut self, at: (i32, i32)) -> Option<(String, Option<f32>)> {
        let (x, y) = (at.0 as f32 / self.scale, at.1 as f32 / self.scale);
        let zone = self.scene.zones[self.scene.zone_at(x, y)?].clone();
        if !zone.slider {
            return Some((zone.action, None));
        }
        unsafe {
            SetCapture(self.hwnd);
        }
        let fraction = zone.fraction(x);
        let action = zone.action.clone();
        self.drag = Some(zone);
        Some((action, Some(fraction)))
    }

    /// The mouse at `at` during a drag: the slider's action and where along it.
    pub fn drag_to(&self, at: (i32, i32)) -> Option<(String, f32)> {
        let zone = self.drag.as_ref()?;
        Some((zone.action.clone(), zone.fraction(at.0 as f32 / self.scale)))
    }

    /// The end of a drag: the slider's action, if one was dragged.
    pub fn release(&mut self) -> Option<String> {
        let zone = self.drag.take()?;
        unsafe {
            let _ = ReleaseCapture();
        }
        Some(zone.action)
    }

    /// Adds a wheel delta and returns the whole notches it makes: a touchpad sends small ones.
    pub fn wheel(&mut self, delta: i32) -> i32 {
        self.wheel += delta;
        let notches = self.wheel / 120;
        self.wheel -= notches * 120;
        notches
    }

    /// Puts the hover tint under the mouse's zone, or takes it away. True if anything changed.
    fn update_hover(&mut self, gfx: &Gfx, theme: &Theme) -> Result<bool> {
        let hover = self
            .mouse
            .and_then(|(x, y)| self.scene.zone_at(x, y))
            .map(|i| &self.scene.zones[i])
            .filter(|z| !z.slider && self.drag.is_none())
            .map(|z| (z.rect, z.radius));
        if hover == self.hover {
            return Ok(false);
        }
        self.hover = hover;
        let s = self.scale;
        let Some(comp) = &mut self.comp else { return Ok(false) };
        unsafe {
            match hover {
                None => {
                    if comp.hover_shown {
                        comp.root.RemoveVisual(&comp.hover)?;
                        comp.hover_shown = false;
                    }
                }
                Some((rect, radius)) => {
                    let size = ((rect.w * s).round() as u32, (rect.h * s).round() as u32);
                    let text = self.scene.backdrop.map_or(theme.text, |b| b.text);
                    let tint = Rgba(text.0, text.1, text.2, TINT);
                    let key = (size.0, size.1, radius.to_bits(), tint.bits());
                    if comp.hover_key != key || comp.hover_surface.is_none() {
                        let surface = gfx.surface(size.0, size.1)?;
                        let (w, h) = (size.0 as f32 / s, size.1 as f32 / s);
                        gfx::draw(&surface, s, |ctx, brush| {
                            brush.SetColor(&gfx::color(tint));
                            ctx.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: gfx::rect(0.0, 0.0, w, h), radiusX: radius, radiusY: radius }, brush);
                        })?;
                        comp.hover.SetContent(&surface)?;
                        comp.hover_surface = Some(surface);
                        comp.hover_key = key;
                    }
                    comp.hover.SetOffsetX2((rect.x * s).round())?;
                    comp.hover.SetOffsetY2((rect.y * s).round())?;
                    if !comp.hover_shown {
                        comp.root.AddVisual(&comp.hover, true, None)?;
                        comp.hover_shown = true;
                    }
                }
            }
        }
        Ok(true)
    }
}

impl Drop for Popup {
    fn drop(&mut self) {
        self.comp = None;
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Draws the popup: its background and border, then the scene's shapes in order.
fn draw(surface: &IDCompositionSurface, s: f32, theme: &Theme, scene: &Scene, fonts: &Fonts) -> Result<()> {
    gfx::draw(surface, s, |ctx, brush| paint(ctx, brush, s, theme, scene, fonts))
}

/// A popup's scene as a PNG at `scale`, drawn away from the screen (`rustybar msg shot`).
pub fn shot(gfx: &Gfx, theme: &Theme, scene: &Scene, fonts: &Fonts, scale: f32, path: &std::path::Path) -> Result<(u32, u32)> {
    let size = ((scene.width * scale).ceil() as u32, (scene.height * scale).ceil() as u32);
    gfx.png(size.0, size.1, scale, path, |ctx, brush| paint(ctx, brush, scale, theme, scene, fonts))?;
    Ok(size)
}

/// The popup: its background and border, then the scene's shapes, in DIPs at scale `s`.
fn paint(ctx: &ID2D1DeviceContext, brush: &ID2D1SolidColorBrush, s: f32, theme: &Theme, scene: &Scene, fonts: &Fonts) {
    let (w, h) = (scene.width, scene.height);
    let colors = scene.backdrop.unwrap_or_else(|| Backdrop::of(theme));
    unsafe {
        let b = theme.popup_border;
        let r = (theme.popup_radius - b / 2.0).max(0.0);
        // The border inside the edge, so the popup keeps its size
        let outline = D2D1_ROUNDED_RECT { rect: gfx::rect(b / 2.0, b / 2.0, w - b, h - b), radiusX: r, radiusY: r };
        brush.SetColor(&gfx::color(colors.base));
        ctx.FillRoundedRectangle(&outline, brush);
        if b > 0.0 {
            brush.SetColor(&gfx::color(colors.border));
            ctx.DrawRoundedRectangle(&outline, brush, b, None);
        }
    }
    paint_shapes(ctx, brush, s, &scene.shapes, fonts);
}

/// A scene's shapes, in order, in DIPs at scale `s`: what a popup draws inside its frame, and the
/// settings window everywhere.
pub fn paint_shapes(ctx: &ID2D1DeviceContext, brush: &ID2D1SolidColorBrush, s: f32, shapes: &[Shape], fonts: &Fonts) {
    unsafe {
        for shape in shapes {
            match shape {
                Shape::Fill { rect, radius, color } => {
                    brush.SetColor(&gfx::color(*color));
                    let shape = D2D1_ROUNDED_RECT { rect: gfx::rect(rect.x, rect.y, rect.w, rect.h), radiusX: *radius, radiusY: *radius };
                    ctx.FillRoundedRectangle(&shape, brush);
                }
                Shape::Text { rect, text, font, color, align } => {
                    let Ok((layout, tw, th)) = fonts.layout(text, *font, rect.w) else { continue };
                    let x = match align {
                        Align::Start => rect.x,
                        Align::Center => rect.x + (rect.w - tw) / 2.0,
                        Align::End => rect.x + rect.w - tw,
                    };
                    let y = rect.y + (rect.h - th) / 2.0;
                    brush.SetColor(&gfx::color(*color));
                    // Snapped to whole pixels: text between pixels looks blurred
                    let at = Vector2 { X: (x * s).round() / s, Y: (y * s).round() / s };
                    ctx.DrawTextLayout(at, &layout, brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
                }
                Shape::Image { rect, radius, image } => {
                    if let Err(e) = draw_image(ctx, *rect, *radius, image) {
                        log!("popup image: {e}");
                    }
                }
                Shape::Para { rect, text, font, color } => {
                    let Ok((layout, _)) = fonts.wrapped(text, *font, rect.w) else { continue };
                    brush.SetColor(&gfx::color(*color));
                    let at = Vector2 { X: (rect.x * s).round() / s, Y: (rect.y * s).round() / s };
                    ctx.DrawTextLayout(at, &layout, brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
                }
            }
        }
    }
}

/// A picture stretched over `rect` with rounded corners: a bitmap brush filling the shape.
pub fn draw_image(ctx: &ID2D1DeviceContext, rect: Rect, radius: f32, image: &Image) -> Result<()> {
    if image.width == 0 || image.height == 0 || image.pixels.len() < (image.width * image.height * 4) as usize {
        return Ok(());
    }
    unsafe {
        let properties = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        let size = D2D_SIZE_U { width: image.width, height: image.height };
        let bitmap = ctx.CreateBitmap(size, Some(image.pixels.as_ptr() as *const _), image.width * 4, &properties)?;
        let brush_properties = D2D1_BITMAP_BRUSH_PROPERTIES1 {
            extendModeX: D2D1_EXTEND_MODE_CLAMP,
            extendModeY: D2D1_EXTEND_MODE_CLAMP,
            // Linear: the cubic modes make Direct2D compile a shader (d3dcompiler_47.dll, 4.6 MB)
            interpolationMode: D2D1_INTERPOLATION_MODE_LINEAR,
        };
        let brush = ctx.CreateBitmapBrush(&bitmap, Some(&brush_properties), None)?;
        // At 96 DPI the bitmap is as many DIPs as pixels: stretched to the rectangle, moved onto it
        let scale = Matrix3x2::scale(rect.w / image.width as f32, rect.h / image.height as f32);
        brush.SetTransform(&(scale * Matrix3x2::translation(rect.x, rect.y)));
        let shape = D2D1_ROUNDED_RECT { rect: gfx::rect(rect.x, rect.y, rect.w, rect.h), radiusX: radius, radiusY: radius };
        ctx.FillRoundedRectangle(&shape, &brush);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popups_line_up_with_their_pill_inside_the_screen() {
        let screen = RECT { left: 0, top: 0, right: 1000, bottom: 800 };
        let anchor = Anchor { left: 100, right: 200, top: 46, side: Side::Left, screen, margin: 8 };
        assert_eq!(anchor.place(300), (100, 46));
        assert_eq!(Anchor { side: Side::Right, ..anchor }.place(300), (8, 46));
        assert_eq!(Anchor { side: Side::Center, ..anchor }.place(50), (125, 46));
        // A pill at the right edge: the popup stops at the margin
        let right = Anchor { left: 900, right: 995, side: Side::Left, ..anchor };
        assert_eq!(right.place(300), (692, 46));
        // On a second screen to the left of the first
        let left_screen = Anchor { left: -500, right: -400, screen: RECT { left: -1920, top: 0, right: 0, bottom: 1080 }, ..anchor };
        assert_eq!(left_screen.place(300), (-500, 46));
    }
}
