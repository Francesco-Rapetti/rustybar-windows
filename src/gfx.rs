//! The devices every bar draws with, created once and dropped while every bar is hidden (a game
//! full screen): Direct3D 11 (WARP, Windows' software rasterizer, unless `RUSTYBAR_GPU=hardware`),
//! Direct2D on it, and DirectComposition for the visual tree that DWM composes and animates. The
//! bar draws into a surface only when its content changes; between changes, and during an
//! animation, the process does nothing.

use windows::Win32::Foundation::{E_FAIL, HMODULE, POINT};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_FACTORY_OPTIONS, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1Device,
    ID2D1DeviceContext, ID2D1Factory1, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_SINGLETHREADED, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice2, IDCompositionAnimation, IDCompositionDesktopDevice, IDCompositionSurface,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM};
use windows::Win32::Graphics::Dxgi::{IDXGIDevice, IDXGIDevice3};
use windows::core::{Interface, Result};
use windows_numerics::Matrix3x2;

use crate::log;
use crate::theme::Rgba;

/// How long a slide lasts, in seconds.
pub const SLIDE: f64 = 0.22;

pub struct Gfx {
    pub dcomp: IDCompositionDesktopDevice,
    pub hardware: bool,
    d2d: ID2D1Device,
    d3d: ID3D11Device,
}

fn d3d(hardware: bool) -> Result<ID3D11Device> {
    let mut device = None;
    unsafe {
        D3D11CreateDevice(
            None,
            if hardware { D3D_DRIVER_TYPE_HARDWARE } else { D3D_DRIVER_TYPE_WARP },
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_SINGLETHREADED,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
    }
    device.ok_or_else(|| E_FAIL.into())
}

impl Gfx {
    pub fn new() -> Result<Gfx> {
        let want_hardware = std::env::var("RUSTYBAR_GPU").is_ok_and(|v| v.eq_ignore_ascii_case("hardware"));
        let (d3d, hardware) = match d3d(want_hardware) {
            Ok(device) => (device, want_hardware),
            Err(e) => {
                log!("Direct3D ({}): {e}, trying the other", if want_hardware { "hardware" } else { "WARP" });
                (d3d(!want_hardware)?, !want_hardware)
            }
        };
        unsafe {
            let dxgi: IDXGIDevice = d3d.cast()?;
            let options = D2D1_FACTORY_OPTIONS::default();
            let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, Some(&options))?;
            let d2d = factory.CreateDevice(&dxgi)?;
            let dcomp: IDCompositionDesktopDevice = DCompositionCreateDevice2(&d2d)?;
            Ok(Gfx { dcomp, hardware, d2d, d3d })
        }
    }

    /// Lets go of what drawing leaves cached for next time: Direct2D's resources (glyphs,
    /// gradients) and the memory Direct3D keeps to reuse. For when a popup has gone: what it drew
    /// won't be drawn again soon, and the bar's next redraw makes what it needs again.
    pub fn trim(&self) {
        unsafe {
            self.d2d.ClearResources(0);
            // Trim only frees what nothing is bound to
            if let Ok(context) = self.d3d.GetImmediateContext() {
                context.ClearState();
                context.Flush();
            }
            if let Ok(dxgi) = self.d3d.cast::<IDXGIDevice3>() {
                dxgi.Trim();
            }
        }
    }

    /// A surface for a pill part, `width` × `height` pixels.
    pub fn surface(&self, width: u32, height: u32) -> Result<IDCompositionSurface> {
        unsafe { self.dcomp.CreateSurface(width.max(1), height.max(1), DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_ALPHA_MODE_PREMULTIPLIED) }
    }

    /// A slide of a pill or a highlight from `from` to `to` (pixels).
    pub fn slide(&self, from: f32, to: f32) -> Result<IDCompositionAnimation> {
        self.ease_out(from, to, SLIDE)
    }

    /// An ease-out from `from` to `to` in `duration` seconds, run by DWM: the cubic
    /// `from + Δ·(1 − (1 − t/d)³)`, written out as DirectComposition's coefficients.
    pub fn ease_out(&self, from: f32, to: f32, duration: f64) -> Result<IDCompositionAnimation> {
        let d = duration as f32;
        let delta = to - from;
        unsafe {
            let a = self.dcomp.CreateAnimation()?;
            a.AddCubic(0.0, from, 3.0 * delta / d, -3.0 * delta / (d * d), delta / (d * d * d))?;
            a.End(duration, to)?;
            Ok(a)
        }
    }

    /// A straight line from `from` to `to` in `duration` seconds (a fade), run by DWM.
    pub fn linear(&self, from: f32, to: f32, duration: f64) -> Result<IDCompositionAnimation> {
        unsafe {
            let a = self.dcomp.CreateAnimation()?;
            a.AddCubic(0.0, from, (to - from) / duration as f32, 0.0, 0.0)?;
            a.End(duration, to)?;
            Ok(a)
        }
    }

    pub fn commit(&self) -> Result<()> {
        unsafe { self.dcomp.Commit() }
    }
}

pub fn color(c: Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.0, g: c.1, b: c.2, a: c.3 }
}

pub fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

/// Draws a whole surface, in DIPs at `scale`, starting from transparent.
pub fn draw(surface: &IDCompositionSurface, scale: f32, f: impl FnOnce(&ID2D1DeviceContext, &ID2D1SolidColorBrush)) -> Result<()> {
    unsafe {
        let mut offset = POINT::default();
        let ctx: ID2D1DeviceContext = surface.BeginDraw(None, &mut offset)?;
        // The surface may live inside a bigger atlas: draw from its offset
        ctx.SetDpi(96.0 * scale, 96.0 * scale);
        ctx.SetTransform(&Matrix3x2::translation(offset.x as f32 / scale, offset.y as f32 / scale));
        ctx.Clear(Some(&color(Rgba::TRANSPARENT)));
        ctx.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        let brush = ctx.CreateSolidColorBrush(&color(Rgba::TRANSPARENT), None)?;
        f(&ctx, &brush);
        surface.EndDraw()
    }
}
