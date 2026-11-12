//! The screens: the main one's refresh rate in the pill, and `HDR` while it's on; in the popup, for
//! each screen, its name and resolution, the refresh rates it has at that resolution (a click
//! switches), HDR on and off, and the brightness of a monitor that takes it over DDC/CI.
//!
//! - Data: the display configuration (`QueryDisplayConfig`, the targets' names and their advanced
//!   color state with `DisplayConfigGetDeviceInfo`, in well under a millisecond), read at the start
//!   and when Windows says the screens changed (`WM_DISPLAYCHANGE`); the rates
//!   (`EnumDisplaySettingsW`, ~15 ms for some 180 modes) only when the popup opens. HDR uses the
//!   HDR state of Windows 11 24H2 where it's there (`GET_ADVANCED_COLOR_INFO_2`, `SET_HDR_STATE`),
//!   the advanced color state before; a rate goes through `ChangeDisplaySettingsExW`, as
//!   Settings does it.
//! - The brightness goes over the monitor's cable (DDC/CI, `GetMonitorBrightness`): tens of ms a
//!   monitor, so on a worker thread, only when the popup opens (`brightness = false` leaves it
//!   out), and set when the slider is let go.
//!
//! Nothing runs at rest: no timer, no thread. With made-up data (see `mock`) the screens are those
//! of `display.json` and the controls move the made-up values alone.

use std::sync::{Arc, Mutex};

use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE, DISPLAYCONFIG_DEVICE_INFO_TYPE,
    DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, DestroyPhysicalMonitors,
    DisplayConfigGetDeviceInfo, DisplayConfigSetDeviceInfo, GetDisplayConfigBufferSizes, GetMonitorBrightness, GetNumberOfPhysicalMonitorsFromHMONITOR,
    GetPhysicalMonitorsFromHMONITOR, PHYSICAL_MONITOR, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig, SetMonitorBrightness,
};
use windows::Win32::Foundation::{HANDLE, LPARAM, LUID, RECT};
use windows::Win32::Graphics::Gdi::{
    CDS_UPDATEREGISTRY, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH,
    ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_MODE, EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
    MONITORINFOEXW,
};
use windows::core::{BOOL, HSTRING};

use super::{Input, Reply, Signal};
use crate::config::Options;
use crate::i18n::t;
use crate::log;
use crate::process;
use crate::theme::Theme;
use crate::ui::{Align, Font, Fonts, Item, Pill, Rect, Scene, Weight};
use crate::wake::Wake;

const MONITOR: &str = "\u{E7F4}";
const SUN: &str = "\u{E706}";

/// Windows 11 24H2's HDR state, which the `windows` crate doesn't have yet.
const GET_ADVANCED_COLOR_INFO_2: DISPLAYCONFIG_DEVICE_INFO_TYPE = DISPLAYCONFIG_DEVICE_INFO_TYPE(15);
const SET_HDR_STATE: DISPLAYCONFIG_DEVICE_INFO_TYPE = DISPLAYCONFIG_DEVICE_INFO_TYPE(16);

#[repr(C)]
#[derive(Default)]
struct AdvancedColorInfo2 {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    /// Bits: 0 advanced color supported, 1 active, 3 limited by policy, 4 HDR supported, 5 HDR
    /// on, 6 wide color supported, 7 on.
    value: u32,
    color_encoding: u32,
    bits_per_color_channel: u32,
    active_color_mode: u32,
}

#[repr(C)]
#[derive(Default)]
struct SetHdrState {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    /// Bit 0: HDR on.
    value: u32,
}

// The popup's layout, in DIPs.
const MARGIN: f32 = 8.0;
const INNER: f32 = 304.0;
const WIDTH: f32 = INNER + 2.0 * MARGIN;
const PAD: f32 = 12.0;
const GAP: f32 = 8.0;
const RADIUS: f32 = 6.0;
const HEAD: f32 = 40.0;
const LABEL: f32 = 22.0;
const CHIP: f32 = 30.0;
const ROW: f32 = 36.0;
const BUTTON: f32 = 30.0;

#[derive(Clone, Debug, PartialEq)]
struct Screen {
    /// Its GDI name (`\\.\DISPLAY1`), what the modes and the monitors go by.
    gdi: String,
    name: String,
    width: u32,
    height: u32,
    hz: u32,
    /// The refresh rates at this resolution, read when the popup opens.
    rates: Vec<u32>,
    /// None if the screen can't do HDR.
    hdr: Option<bool>,
    primary: bool,
    /// The brightness, 0–100, of a monitor that answers over DDC/CI.
    brightness: Option<u32>,
    adapter: LUID,
    target: u32,
    /// Windows has the HDR state of 24H2.
    hdr_state: bool,
}

/// What the worker thread of DDC/CI brings back: each monitor's brightness by its GDI name.
type Brightness = Arc<Mutex<Option<Vec<(String, u32)>>>>;

pub struct Display {
    screens: Vec<Screen>,
    open: bool,
    ddc: bool,
    wake: Arc<Wake>,
    brightness: Brightness,
    reading: bool,
    /// A slider being dragged: its screen and where.
    sliding: Option<(usize, f32)>,
    mock: bool,
}

impl Display {
    pub fn new(options: &Options) -> Display {
        let mut d = Display {
            screens: Vec::new(),
            open: false,
            ddc: options.bool("brightness").unwrap_or(true),
            wake: Wake::new(),
            brightness: Arc::new(Mutex::new(None)),
            reading: false,
            sliding: None,
            mock: false,
        };
        if let Some(m) = crate::mock::load("display") {
            d.mock = true;
            d.screens = m["screens"]
                .array()
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let n = |k: &str| s[k].f64().unwrap_or(0.0) as u32;
                    Screen {
                        gdi: format!("\\\\.\\DISPLAY{}", i + 1),
                        name: s["name"].str().unwrap_or_default().to_string(),
                        width: n("width"),
                        height: n("height"),
                        hz: n("hz"),
                        rates: s["rates"].array().iter().filter_map(|r| r.f64()).map(|r| r as u32).collect(),
                        hdr: s["hdr"].bool(),
                        primary: i == 0,
                        brightness: s["brightness"].f64().map(|b| b as u32),
                        adapter: LUID::default(),
                        target: 0,
                        hdr_state: false,
                    }
                })
                .collect();
            return d;
        }
        d.screens = read_screens();
        d
    }

    fn primary(&self) -> Option<&Screen> {
        self.screens.iter().find(|s| s.primary).or(self.screens.first())
    }

    /// The screens read again, keeping what only the popup reads (rates, brightness) for the same
    /// screens. True if what's shown changed.
    fn reread(&mut self) -> bool {
        let mut screens = read_screens();
        for s in &mut screens {
            if let Some(old) = self.screens.iter().find(|o| o.gdi == s.gdi) {
                s.brightness = old.brightness;
                if self.open {
                    s.rates = rates(&s.gdi, s.width, s.height);
                }
            }
        }
        let changed = screens != self.screens;
        self.screens = screens;
        changed
    }

    /// The brightness of the monitors, over DDC/CI on a worker thread.
    fn read_brightness(&mut self) {
        if !self.ddc || self.reading {
            return;
        }
        self.reading = true;
        let (slot, wake) = (self.brightness.clone(), self.wake.clone());
        std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let mut found = Vec::new();
            for (gdi, monitor) in monitors() {
                if let Some(b) = with_physical(monitor, |handle| {
                    let (mut min, mut cur, mut max) = (0u32, 0u32, 0u32);
                    (unsafe { GetMonitorBrightness(handle, &mut min, &mut cur, &mut max) } != 0 && max > min).then(|| (cur - min) * 100 / (max - min))
                }) {
                    found.push((gdi, b));
                }
            }
            log!("display: the brightness of {} monitors read over DDC/CI in {} ms", found.len(), started.elapsed().as_millis());
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(found);
            wake.set();
        });
    }

    fn set_brightness(&mut self, i: usize, percent: u32) {
        let Some(s) = self.screens.get_mut(i) else { return };
        s.brightness = Some(percent);
        if self.mock {
            return;
        }
        let gdi = s.gdi.clone();
        std::thread::spawn(move || {
            for (name, monitor) in monitors() {
                if name == gdi {
                    with_physical(monitor, |handle| {
                        let (mut min, mut cur, mut max) = (0u32, 0u32, 0u32);
                        if unsafe { GetMonitorBrightness(handle, &mut min, &mut cur, &mut max) } != 0 && max > min {
                            let value = min + (max - min) * percent / 100;
                            if unsafe { SetMonitorBrightness(handle, value) } == 0 {
                                log!("display: the monitor refused the brightness");
                            }
                        }
                        Some(())
                    });
                }
            }
        });
    }

    fn set_hdr(&mut self, i: usize) {
        let Some(s) = self.screens.get_mut(i) else { return };
        let Some(on) = s.hdr else { return };
        s.hdr = Some(!on);
        if self.mock {
            return;
        }
        let header = |kind, size| DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: kind, size, adapterId: s.adapter, id: s.target };
        let result = if s.hdr_state {
            let set = SetHdrState { header: header(SET_HDR_STATE, size_of::<SetHdrState>() as u32), value: u32::from(!on) };
            unsafe { DisplayConfigSetDeviceInfo(&set.header) }
        } else {
            let mut set = DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE {
                header: header(DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE, size_of::<DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE>() as u32),
                ..Default::default()
            };
            set.Anonymous.value = u32::from(!on);
            unsafe { DisplayConfigSetDeviceInfo(&set.header) }
        };
        if result != 0 {
            log!("display: HDR {}: error {result}", if on { "off" } else { "on" });
            s.hdr = Some(on);
        }
    }

    fn set_rate(&mut self, i: usize, hz: u32) {
        let Some(s) = self.screens.get_mut(i) else { return };
        if s.hz == hz {
            return;
        }
        if self.mock {
            s.hz = hz;
            return;
        }
        let mut mode = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
        mode.dmFields = DM_DISPLAYFREQUENCY | DM_PELSWIDTH | DM_PELSHEIGHT;
        (mode.dmPelsWidth, mode.dmPelsHeight, mode.dmDisplayFrequency) = (s.width, s.height, hz);
        let name = HSTRING::from(s.gdi.as_str());
        let result = unsafe { ChangeDisplaySettingsExW(&name, Some(&mode), None, CDS_UPDATEREGISTRY, None) };
        if result == DISP_CHANGE_SUCCESSFUL {
            s.hz = hz;
        } else {
            log!("display: {hz} Hz on {}: {result:?}", s.gdi);
        }
    }
}

/// The active screens, from the display configuration.
fn read_screens() -> Vec<Screen> {
    let (mut np, mut nm) = (0u32, 0u32);
    if unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) }.is_err() {
        return Vec::new();
    }
    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
    if unsafe { QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None) }.is_err() {
        return Vec::new();
    }
    let mut screens = Vec::new();
    for p in &paths[..np as usize] {
        let (adapter, target) = (p.targetInfo.adapterId, p.targetInfo.id);
        let header = |kind, size| DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: kind, size, adapterId: adapter, id: target };
        let mut name = DISPLAYCONFIG_TARGET_DEVICE_NAME { header: header(DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32), ..Default::default() };
        unsafe { DisplayConfigGetDeviceInfo(&mut name.header) };
        let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                adapterId: p.sourceInfo.adapterId,
                id: p.sourceInfo.id,
            },
            ..Default::default()
        };
        unsafe { DisplayConfigGetDeviceInfo(&mut source.header) };
        // HDR: the 24H2 state where there is one, the advanced color state before
        let mut info2 = AdvancedColorInfo2 { header: header(GET_ADVANCED_COLOR_INFO_2, size_of::<AdvancedColorInfo2>() as u32), ..Default::default() };
        let (hdr, hdr_state) = if unsafe { DisplayConfigGetDeviceInfo(&mut info2.header) } == 0 {
            ((info2.value & 1 << 4 != 0).then_some(info2.value & 1 << 5 != 0), true)
        } else {
            let mut info = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO { header: header(DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO, size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32), ..Default::default() };
            let bits = if unsafe { DisplayConfigGetDeviceInfo(&mut info.header) } == 0 { unsafe { info.Anonymous.value } } else { 0 };
            ((bits & 1 != 0).then_some(bits & 2 != 0), false)
        };
        let gdi = wide(&source.viewGdiDeviceName);
        // The source mode: the resolution, and where the screen is (the main one is at 0, 0)
        let mut current = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
        let name_w = HSTRING::from(gdi.as_str());
        unsafe {
            let _ = EnumDisplaySettingsW(&name_w, ENUM_CURRENT_SETTINGS, &mut current);
        }
        let source_mode = modes.get(unsafe { p.sourceInfo.Anonymous.modeInfoIdx } as usize).filter(|m| m.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE);
        let primary = source_mode.is_some_and(|m| {
            let position = unsafe { m.Anonymous.sourceMode.position };
            position.x == 0 && position.y == 0
        });
        let rate = p.targetInfo.refreshRate;
        let hz = if rate.Denominator > 0 { (rate.Numerator as f64 / rate.Denominator as f64).round() as u32 } else { current.dmDisplayFrequency };
        let friendly = wide(&name.monitorFriendlyDeviceName);
        screens.push(Screen {
            name: if friendly.is_empty() { gdi.trim_start_matches("\\\\.\\").to_string() } else { friendly },
            gdi,
            width: current.dmPelsWidth,
            height: current.dmPelsHeight,
            hz,
            rates: Vec::new(),
            hdr,
            primary,
            brightness: None,
            adapter,
            target,
            hdr_state,
        });
    }
    screens
}

/// The refresh rates a screen has at a resolution, highest last.
fn rates(gdi: &str, width: u32, height: u32) -> Vec<u32> {
    let name = HSTRING::from(gdi);
    let mut rates = Vec::new();
    for i in 0.. {
        let mut mode = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
        if !unsafe { EnumDisplaySettingsW(&name, ENUM_DISPLAY_SETTINGS_MODE(i), &mut mode) }.as_bool() {
            break;
        }
        if mode.dmPelsWidth == width && mode.dmPelsHeight == height && mode.dmDisplayFrequency > 1 && !rates.contains(&mode.dmDisplayFrequency) {
            rates.push(mode.dmDisplayFrequency);
        }
    }
    rates.sort_unstable();
    rates
}

/// The monitors, by GDI name.
fn monitors() -> Vec<(String, HMONITOR)> {
    unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let list = unsafe { &mut *(data.0 as *mut Vec<(String, HMONITOR)>) };
        let mut info = MONITORINFOEXW { monitorInfo: MONITORINFO { cbSize: size_of::<MONITORINFOEXW>() as u32, ..Default::default() }, ..Default::default() };
        if unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) }.as_bool() {
            list.push((wide(&info.szDevice), monitor));
        }
        BOOL(1)
    }
    let mut list: Vec<(String, HMONITOR)> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut list as *mut _ as isize));
    }
    list
}

/// Runs `f` on the first physical monitor of `monitor` that answers, and lets them go.
fn with_physical<R>(monitor: HMONITOR, f: impl Fn(HANDLE) -> Option<R>) -> Option<R> {
    let mut n = 0u32;
    if unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(monitor, &mut n) }.is_err() || n == 0 {
        return None;
    }
    let mut physical = vec![PHYSICAL_MONITOR::default(); n as usize];
    unsafe { GetPhysicalMonitorsFromHMONITOR(monitor, &mut physical) }.ok()?;
    let result = physical.iter().find_map(|p| f(p.hPhysicalMonitor));
    unsafe {
        let _ = DestroyPhysicalMonitors(&physical);
    }
    result
}

fn wide(s: &[u16]) -> String {
    String::from_utf16_lossy(&s[..s.iter().position(|c| *c == 0).unwrap_or(s.len())])
}

impl super::Widget for Display {
    fn pill(&self, theme: &Theme) -> Option<Pill> {
        let s = self.primary()?;
        let mut items = vec![Item::icon(MONITOR, theme.text), Item::text(format!("{} Hz", s.hz), theme.text)];
        if s.hdr == Some(true) {
            items.push(Item::text("HDR", theme.subtext).small().space(6.0));
        }
        Some(Pill::new(items).gap(6.0))
    }

    fn compact(&self, theme: &Theme) -> Option<Pill> {
        let s = self.primary()?;
        Some(Pill::new(vec![Item::text(format!("{} Hz", s.hz), theme.text)]))
    }

    fn handle(&self) -> Option<HANDLE> {
        self.wake.handle()
    }

    /// The brightness came from the worker thread.
    fn signaled(&mut self) -> bool {
        let Some(found) = self.brightness.lock().unwrap_or_else(|e| e.into_inner()).take() else { return false };
        self.reading = false;
        for s in &mut self.screens {
            s.brightness = found.iter().find(|(gdi, _)| *gdi == s.gdi).map(|(_, b)| *b);
        }
        self.open
    }

    fn event(&mut self, signal: &Signal) -> bool {
        match signal {
            Signal::Displays | Signal::Resume if !self.mock => self.reread(),
            _ => false,
        }
    }

    fn has_popup(&self) -> bool {
        true
    }

    fn popup_shown(&mut self, open: bool) {
        self.open = open;
        self.sliding = None;
        if !open || self.mock {
            return;
        }
        self.reread();
        for s in &mut self.screens {
            s.rates = rates(&s.gdi, s.width, s.height);
        }
        self.read_brightness();
    }

    fn popup(&self, theme: &Theme, fonts: &Fonts) -> Option<Scene> {
        let per_row = 4usize;
        let tile_h = |s: &Screen| {
            let rows = s.rates.len().max(1).div_ceil(per_row);
            let hdr = if s.hdr.is_some() { ROW } else { 0.0 };
            let light = if self.ddc && (s.brightness.is_some() || self.reading) { ROW } else { 0.0 };
            PAD + HEAD + LABEL + rows as f32 * (CHIP + 6.0) + hdr + light + PAD - 6.0
        };
        let height = MARGIN + self.screens.iter().map(|s| tile_h(s) + GAP).sum::<f32>() + BUTTON + 6.0;
        let mut scene = Scene::new(WIDTH, height.max(MARGIN + BUTTON + 6.0));
        let mut y = MARGIN;
        let small = Font::new(theme.small_size, Weight::Medium);
        let body = Font::new(theme.font_size, Weight::Regular);
        for (i, s) in self.screens.iter().enumerate() {
            let r = Rect::new(MARGIN, y, INNER, tile_h(s));
            scene.fill(r, RADIUS, theme.tile(theme.primary, false));
            let (x, w) = (r.x + PAD, r.w - 2.0 * PAD);
            let mut cy = r.y + PAD;
            scene.text(Rect::new(x, cy, 20.0, 22.0), MONITOR, Font::icons(15.0), theme.text, Align::Center);
            let size = format!("{} × {}", s.width, s.height);
            let size_w = fonts.width(&size, small).ceil();
            scene.text(Rect::new(x + 28.0, cy, w - 28.0 - size_w - 8.0, 22.0), s.name.as_str(), Font::new(theme.font_size + 1.0, Weight::SemiBold), theme.text, Align::Start);
            scene.text(Rect::new(x + w - size_w, cy, size_w, 22.0), size, small, theme.subtext, Align::End);
            cy += HEAD;
            scene.text(Rect::new(x, cy - 12.0, w, LABEL), t("Frequenza di aggiornamento", "Refresh rate"), small, theme.subtext, Align::Start);
            cy += LABEL - 8.0;
            // The rates as chips, the one in use lit
            let rates = if s.rates.is_empty() { vec![s.hz] } else { s.rates.clone() };
            let chip_w = (w - 6.0 * (per_row - 1) as f32) / per_row as f32;
            for (k, hz) in rates.iter().enumerate() {
                let (col, row) = (k % per_row, k / per_row);
                let c = Rect::new(x + col as f32 * (chip_w + 6.0), cy + row as f32 * (CHIP + 6.0), chip_w, CHIP);
                let on = *hz == s.hz;
                scene.fill(c, theme.row_radius, if on { theme.primary } else { theme.text.alpha(0.06) });
                scene.text(c, format!("{hz} Hz"), Font::new(theme.font_size, if on { Weight::SemiBold } else { Weight::Regular }), if on { theme.on_primary } else { theme.text }, Align::Center);
                if !on {
                    scene.zone(c, theme.row_radius, format!("hz:{i}:{hz}"));
                }
            }
            cy += rates.len().div_ceil(per_row) as f32 * (CHIP + 6.0);
            if let Some(on) = s.hdr {
                let row = Rect::new(x, cy, w, ROW);
                scene.text(Rect::new(x, cy, w - 52.0, ROW), "HDR", body, theme.text, Align::Start);
                let switch = Rect::new(x + w - 40.0, cy + (ROW - 20.0) / 2.0, 40.0, 20.0);
                if on {
                    scene.fill(switch, 10.0, theme.primary);
                    scene.fill(Rect::new(switch.x + 24.0, switch.y + 4.0, 12.0, 12.0), 6.0, theme.on_primary);
                } else {
                    scene.fill(switch, 10.0, theme.subtext);
                    scene.fill(Rect::new(switch.x + 1.0, switch.y + 1.0, 38.0, 18.0), 9.0, theme.base);
                    scene.fill(Rect::new(switch.x + 5.0, switch.y + 5.0, 10.0, 10.0), 5.0, theme.subtext);
                }
                scene.zone(row, theme.row_radius, format!("hdr:{i}"));
                cy += ROW;
            }
            if self.ddc && (s.brightness.is_some() || self.reading) {
                scene.text(Rect::new(x, cy, 20.0, ROW), SUN, Font::icons(14.0), theme.text, Align::Center);
                let value = match self.sliding {
                    Some((k, f)) if k == i => Some((f * 100.0).round() as u32),
                    _ => s.brightness,
                };
                match value {
                    Some(b) => {
                        let percent_w = fonts.width("100%", small).ceil();
                        let track = Rect::new(x + 30.0, cy + (ROW - 22.0) / 2.0, w - 30.0 - percent_w - 12.0, 22.0);
                        super::audio::slider(&mut scene, theme, track, b as f32 / 100.0, true);
                        scene.slider(Rect::new(track.x - 8.0, track.y, track.w + 16.0, track.h), format!("light:{i}"));
                        scene.text(Rect::new(x + w - percent_w, cy, percent_w, ROW), format!("{b}%"), small, theme.text, Align::End);
                    }
                    None => scene.text(Rect::new(x + 30.0, cy, w - 30.0, ROW), t("Luminosità…", "Brightness…"), small, theme.subtext, Align::Start),
                }
            }
            y += r.h + GAP;
        }
        if self.screens.is_empty() {
            scene.text(Rect::new(MARGIN + PAD, y, INNER - 2.0 * PAD, BUTTON), t("Nessuno schermo", "No screen"), body, theme.subtext, Align::Start);
            y += BUTTON;
        }
        scene.text(Rect::new(MARGIN + PAD, y, INNER - 2.0 * PAD, BUTTON), t("Impostazioni dello schermo", "Display settings"), body, theme.primary, Align::Start);
        scene.zone(Rect::new(MARGIN, y, INNER, BUTTON), theme.row_radius, "settings");
        Some(scene)
    }

    fn popup_input(&mut self, input: &Input) -> Reply {
        match input {
            Input::Action("settings") => {
                if !self.mock {
                    process::open("ms-settings:display");
                }
                Reply::Opened
            }
            Input::Action(a) if a.starts_with("hz:") => {
                let mut parts = a[3..].split(':').filter_map(|p| p.parse::<u32>().ok());
                if let (Some(i), Some(hz)) = (parts.next(), parts.next()) {
                    self.set_rate(i as usize, hz);
                }
                Reply::Redraw
            }
            Input::Action(a) if a.starts_with("hdr:") => {
                if let Ok(i) = a[4..].parse::<usize>() {
                    self.set_hdr(i);
                }
                Reply::Redraw
            }
            Input::Slide(a, fraction) if a.starts_with("light:") => {
                if let Ok(i) = a[6..].parse::<usize>() {
                    self.sliding = Some((i, fraction.clamp(0.0, 1.0)));
                }
                Reply::Redraw
            }
            Input::Release(a) if a.starts_with("light:") => {
                if let Some((i, f)) = self.sliding.take() {
                    self.set_brightness(i, (f * 100.0).round() as u32);
                }
                Reply::Redraw
            }
            _ => Reply::Nothing,
        }
    }
}
