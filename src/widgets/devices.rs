//! The batteries of the mice, keyboards and controllers, and of the other Bluetooth devices (a
//! phone, earbuds). The pill shows the connected ones that run low (at `low`, 20%), the kinds of
//! `pinned` always (`["mouse", "keyboard", "controller"]`: one each), or every connected one with
//! `always = true`: its kind and its level, yellow, red at 10%, the battery that charges while it
//! does. The popup lists them all, the connected ones first, the others dimmed with the last level
//! known, and the Bluetooth settings.
//!
//! Two sources:
//! - Bluetooth: the level Settings shows, on one of the device's Bluetooth nodes (Configuration
//!   Manager, as `audio` reads its headphones'); the name, the kind (`Input.Keyboard`,
//!   `Communication.Phone`…) and whether it's connected (`DN_DEVICE_DISCONNECTED`) are on its
//!   device node, in the same container. Read on the main thread: the known nodes in well under a
//!   millisecond, all of them in ~10 ms.
//! - Receivers and cables (`hid`, `receivers = false` leaves them out): Razer, Logitech, ROG, the
//!   Steam Controller, DualSense and DualShock 4, Xbox's XInput; a pass on a worker thread that
//!   ends with it (~3.5 ms of CPU, up to a few seconds of waiting on the devices).
//!
//! Windows doesn't announce a new level: both are read again every 5 minutes while a bar shows
//! without a game of the list in front, when the popup opens, and a second after Windows says
//! devices changed (`WM_DEVICECHANGE`), as one does when it connects (then the receivers' HID
//! collections are found again too). A device seen on both, connected on one side only, shows once:
//! the same maker and kind (a keyboard paired over Bluetooth and on its receiver now).
//!
//! With made-up data (see `mock`) the devices are those of `devices.json`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use windows::Win32::Devices::Properties::{DEVPKEY_Device_ContainerId, DEVPKEY_Device_DevNodeStatus, DEVPKEY_Device_FriendlyName};
use windows::Win32::Foundation::{DEVPROPKEY, HANDLE};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::core::GUID;

use super::audio::{BATTERY, device_ids, locate, property};
use super::hid::{self, Found, Kind, Link, Maker};
use super::{Input, Reply, Signal};
use crate::config::Options;
use crate::i18n::t;
use crate::theme::{Rgba, Theme};
use crate::ui::{Align, Font, Fonts, Item, Pill, Rect, Scene, Weight};
use crate::wake::{Timer, Wake};
use crate::{log, process};

/// The enumerators of Bluetooth device nodes: classic, hands-free, Low Energy.
const BLUETOOTH: [&str; 4] = ["BTHENUM", "BTHHFENUM", "BTHLE", "BTHLEDEVICE"];
/// What kind of device it is, as Settings groups it (`DEVPKEY_DeviceDisplay_Category`).
const CATEGORY: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0x78c34fc8_104a_4aca_9ea4_524d52996e57), pid: 90 };
/// The device's function driver says it isn't connected (out of range, off, on another link).
const DN_DEVICE_DISCONNECTED: u32 = 0x0200_0000;

/// The levels are read again this often, in ms.
const EVERY: i32 = 5 * 60 * 1000;
/// Windows says devices changed several times in a row: everything is read again this long after
/// the last time, in ms.
const SETTLE: i64 = 1000;

// The popup's layout, in DIPs: tiles as wide as the popup, then short rows.
const MARGIN: f32 = 8.0;
const INNER: f32 = 308.0;
const WIDTH: f32 = INNER + 2.0 * MARGIN;
/// Inside a tile.
const PAD: f32 = 10.0;
const RADIUS: f32 = 6.0;
const BADGE: f32 = 34.0;
const BAR: f32 = 6.0;
/// A half of a split keyboard: its line, with a label, a bar and the number.
const HALF: f32 = 15.0;
const GAP: f32 = 6.0;
const HEADER: f32 = 32.0;
const SECTION: f32 = 24.0;
const ROW: f32 = 26.0;
const BUTTON: f32 = 30.0;

fn tile_height(d: &Device) -> f32 {
    let bars = if d.second.is_some() { 2.0 * HALF + 3.0 } else { BAR };
    PAD + BADGE + 9.0 + bars + PAD
}

/// A device with a battery, as the pill and the popup show it.
#[derive(Clone, Debug, PartialEq)]
struct Device {
    name: String,
    kind: Kind,
    /// The glyph of the kind (a Bluetooth device's category has more: a phone, earbuds).
    icon: &'static str,
    maker: Maker,
    link: Link,
    /// The last level known; `known` is false for a device paired to a receiver that has slept
    /// since the bar started.
    battery: u8,
    known: bool,
    /// The other half of a split keyboard.
    second: Option<u8>,
    charging: bool,
    /// One of XInput's four levels, not a percentage.
    approx: bool,
    connected: bool,
    /// Behind a receiver or a cable, not answering now (asleep, off) but read since the bar
    /// started: the pill keeps it, dimmed, with that level, and the popup says when it was read.
    asleep: bool,
    read_at: Option<String>,
}

impl Device {
    /// The lower of its levels (a split keyboard's halves).
    fn lowest(&self) -> u8 {
        self.second.map_or(self.battery, |s| s.min(self.battery))
    }
}

/// A Bluetooth device and its nodes: the one with the level, and the device's own (its name and
/// its state).
#[derive(Clone, Debug, PartialEq)]
struct Paired {
    device: Device,
    battery_node: u32,
    device_node: Option<u32>,
}

impl Paired {
    /// Reads the level and the state again.
    fn read(&mut self) {
        if let Some(b) = property(self.battery_node, &BATTERY).and_then(|b| b.first().copied()).filter(|b| *b <= 100) {
            self.device.battery = b;
        }
        if let Some(node) = self.device_node {
            self.device.connected = connected(node);
        }
    }
}

/// What the worker thread keeps between passes (the collections it found) and brings back.
#[derive(Default)]
struct Work {
    known: Mutex<hid::Known>,
    result: Mutex<Option<(Vec<Found>, bool)>>,
}

pub struct Devices {
    /// What the pill and the popup show, made of the two below.
    devices: Vec<Device>,
    bluetooth: Vec<Paired>,
    wired: Vec<Found>,
    /// The last level of each receiver's device, by its key, and the time it was read, for while
    /// it sleeps.
    last: HashMap<String, (u8, Option<u8>, String)>,
    /// Names of the user's for the pill, by the full name (lowercase): `names = { "Razer Naga V2
    /// Pro" = "Gioco" }`.
    names: HashMap<String, String>,
    /// The pass on the worker thread: running, another one asked meanwhile (finding the
    /// collections again or not), and whether the receivers have anything to read.
    work: Arc<Work>,
    wake: Arc<Wake>,
    reading: bool,
    again: Option<bool>,
    any_wired: bool,
    timer: Timer,
    settle: Timer,
    low: u8,
    always: bool,
    pinned: Vec<Kind>,
    receivers: bool,
    shown: bool,
    game: bool,
    mock: bool,
}

impl Devices {
    pub fn new(options: &Options) -> Devices {
        let low = options.f64("low").unwrap_or(20.0).clamp(0.0, 100.0) as u8;
        let always = options.bool("always").unwrap_or(false);
        let pinned = options.strings("pinned").unwrap_or_default().iter().filter_map(|k| kind_named(k)).collect();
        let receivers = options.bool("receivers").unwrap_or(true);
        let names = options
            .0
            .get("names")
            .and_then(|v| v.as_table())
            .map(|t| t.iter().filter_map(|(k, v)| Some((k.to_lowercase(), v.as_str()?.trim().to_string()))).filter(|(_, v)| !v.is_empty()).collect())
            .unwrap_or_default();
        let mut d = Devices {
            devices: Vec::new(),
            bluetooth: Vec::new(),
            wired: Vec::new(),
            last: HashMap::new(),
            names,
            work: Arc::new(Work::default()),
            wake: Wake::new(),
            reading: false,
            again: None,
            any_wired: false,
            timer: Timer::new(),
            settle: Timer::new(),
            low,
            always,
            pinned,
            receivers,
            shown: true,
            game: false,
            mock: false,
        };
        if let Some(m) = crate::mock::load("devices") {
            d.mock = true;
            d.devices = m["devices"]
                .array()
                .iter()
                .map(|v| {
                    let category = v["category"].str().unwrap_or_default();
                    let battery = v["battery"].f64();
                    Device {
                        name: v["name"].str().unwrap_or_default().to_string(),
                        kind: kind_of(category),
                        icon: icon(category),
                        maker: Maker::Other,
                        link: match v["link"].str().unwrap_or("bluetooth") {
                            "receiver" => Link::Receiver,
                            "cable" => Link::Cable,
                            "bluetooth" => Link::Bluetooth,
                            _ => Link::Unknown,
                        },
                        battery: battery.unwrap_or(0.0).clamp(0.0, 100.0) as u8,
                        known: battery.is_some(),
                        second: v["second"].f64().map(|s| s.clamp(0.0, 100.0) as u8),
                        charging: v["charging"].bool().unwrap_or(false),
                        approx: v["approx"].bool().unwrap_or(false),
                        connected: v["connected"].bool().unwrap_or(false),
                        asleep: v["asleep"].bool().unwrap_or(false),
                        read_at: v["read"].str().map(str::to_string),
                    }
                })
                .collect();
            sort(&mut d.devices);
            return d;
        }
        d.scan();
        d.rebuild();
        d.start(true);
        d.schedule();
        d
    }

    /// Every Bluetooth device with a battery, read anew.
    fn scan(&mut self) {
        let mut paired: Vec<(Vec<u8>, Paired)> = Vec::new();
        let mut device_nodes: Vec<(Vec<u8>, u32, String)> = Vec::new();
        let mut makers: Vec<(Vec<u8>, Maker)> = Vec::new();
        for enumerator in BLUETOOTH {
            for id in device_ids(enumerator) {
                let Some(node) = locate(&id) else { continue };
                let Some(container) = property(node, &DEVPKEY_Device_ContainerId) else { continue };
                let upper = id.to_uppercase();
                // The device's own node (`BTHENUM\Dev_…`, `BTHLE\Dev_…`) has its name and its state
                if upper.contains("\\DEV_") {
                    device_nodes.push((container.clone(), node, text(property(node, &DEVPKEY_Device_FriendlyName))));
                }
                // Its HID node says who made it (`…_VID&02068e_PID&…`)
                if let Some(vid) = vendor_id(&upper) {
                    makers.push((container.clone(), hid::maker(vid)));
                }
                let Some(battery) = property(node, &BATTERY).and_then(|b| b.first().copied()).filter(|b| *b <= 100) else { continue };
                if paired.iter().any(|(c, _)| *c == container) {
                    continue;
                }
                let name = text(property(node, &DEVPKEY_Device_FriendlyName));
                let device = Device { name, kind: Kind::Other, icon: icon(""), maker: Maker::Other, link: Link::Bluetooth, battery, known: true, second: None, charging: false, approx: false, connected: true, asleep: false, read_at: None };
                paired.push((container, Paired { device, battery_node: node, device_node: None }));
            }
        }
        for (container, p) in &mut paired {
            if let Some((_, node, name)) = device_nodes.iter().find(|(c, _, _)| c == container) {
                p.device_node = Some(*node);
                p.device.connected = connected(*node);
                let category = text(property(*node, &CATEGORY));
                p.device.icon = icon(&category);
                p.device.kind = kind_of(&category);
                if !name.is_empty() {
                    p.device.name = name.clone();
                }
            }
            if let Some((_, maker)) = makers.iter().find(|(c, _)| c == container) {
                p.device.maker = *maker;
            }
        }
        self.bluetooth = paired.into_iter().map(|(_, p)| p).collect();
    }

    /// A pass over the receivers and cables on a worker thread; one at a time, a second asked
    /// meanwhile runs after it.
    fn start(&mut self, rediscover: bool) {
        if !self.receivers || self.mock {
            return;
        }
        if self.reading {
            self.again = Some(self.again.unwrap_or(false) || rediscover);
            return;
        }
        let (work, wake) = (self.work.clone(), self.wake.clone());
        let spawned = std::thread::Builder::new().name("devices".into()).stack_size(256 * 1024).spawn(move || {
            let started = std::time::Instant::now();
            let mut known = work.known.lock().unwrap_or_else(|e| e.into_inner());
            let found = hid::pass(&mut known, rediscover);
            let any = known.as_ref().is_some_and(|k| !k.is_empty()) || !found.is_empty();
            drop(known);
            if rediscover {
                log!("devices: {} behind receivers or cables, read in {} ms", found.len(), started.elapsed().as_millis());
            }
            *work.result.lock().unwrap_or_else(|e| e.into_inner()) = Some((found, any));
            wake.set();
        });
        match spawned {
            Ok(_) => self.reading = true,
            Err(e) => log!("devices: {e}"),
        }
    }

    /// The pass's devices, with the last level of those that sleep now.
    fn take(&mut self) {
        let Some((found, any)) = self.work.result.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        self.reading = false;
        self.any_wired = any;
        let now = super::clock::format_time(&unsafe { GetLocalTime() });
        for f in &found {
            if let Some(level) = f.level {
                self.last.insert(f.key.clone(), (level, f.second, now.clone()));
            }
        }
        self.wired = found;
        if let Some(rediscover) = self.again.take() {
            self.start(rediscover);
        }
        self.schedule();
    }

    /// What the pill and the popup show: both sources, each device once.
    fn rebuild(&mut self) {
        let mut wired: Vec<Device> = self
            .wired
            .iter()
            .map(|f| {
                let last = self.last.get(&f.key);
                let (battery, second) = match (f.level, last) {
                    (Some(l), _) => (Some(l), f.second),
                    (None, Some((l, s, _))) => (Some(*l), *s),
                    (None, None) => (None, None),
                };
                Device {
                    name: f.name.clone(),
                    kind: f.kind,
                    icon: f.kind.icon(),
                    maker: f.maker,
                    link: f.link,
                    battery: battery.unwrap_or(0),
                    known: battery.is_some(),
                    second,
                    charging: f.charging && f.level.is_some(),
                    approx: f.approx,
                    connected: f.level.is_some(),
                    asleep: f.level.is_none() && last.is_some(),
                    read_at: last.filter(|_| f.level.is_none()).map(|(_, _, at)| at.clone()),
                }
            })
            .collect();
        let bluetooth: Vec<Device> = self.bluetooth.iter().map(|p| p.device.clone()).collect();
        self.devices = combine(bluetooth, &mut wired);
    }

    /// The levels are read again every 5 minutes while a bar shows without a listed game in front,
    /// if there's something to read.
    fn schedule(&mut self) {
        if self.shown && !self.game && !self.mock && (!self.bluetooth.is_empty() || self.any_wired) {
            self.timer.start(EVERY as i64, EVERY);
        } else {
            self.timer.stop();
        }
    }

    /// The connected devices the pill shows (and those asleep behind a receiver, dimmed): the kinds
    /// pinned, those that run low, or all with
    /// `always`.
    fn in_pill(&self) -> Vec<&Device> {
        self.devices.iter().filter(|d| (d.connected || d.asleep) && d.known && (self.always || self.pinned.contains(&d.kind) || d.lowest() <= self.low)).collect()
    }

    /// Everything read again: Bluetooth now, the receivers on the worker thread. True if what's
    /// shown changed.
    fn refresh(&mut self, rediscover: bool) -> bool {
        let before = self.devices.clone();
        self.scan();
        self.rebuild();
        self.start(rediscover);
        before != self.devices
    }
}

impl Devices {
    /// A device's tile in the popup, tinted with its color when it runs low.
    fn tile(&self, scene: &mut Scene, theme: &Theme, fonts: &Fonts, d: &Device, r: Rect) {
        let p = paint(theme, d);
        let low = d.connected && d.known && d.lowest() <= self.low;
        scene.fill(r, RADIUS, theme.tile(p.fill, low));
        // The kind in a round badge of its color
        let badge = Rect::new(r.x + PAD, r.y + PAD, BADGE, BADGE);
        scene.fill(badge, BADGE / 2.0, if d.connected { p.fill.alpha(0.2) } else { theme.text.alpha(0.07) });
        scene.text(badge, d.icon, Font::icons(16.0), if d.connected { p.fill } else { theme.subtext }, Align::Center);
        // The level, big, with the bolt while it charges
        let right = r.x + r.w - PAD;
        let big = Font::new(theme.font_size + 7.0, Weight::SemiBold);
        let value = level_text(d);
        let value_w = fonts.width(&value, big).ceil();
        scene.text(Rect::new(right - value_w - 2.0, badge.y, value_w + 2.0, BADGE), value, big, p.level, Align::End);
        let mut end = right - value_w - 8.0;
        if d.charging {
            scene.text(Rect::new(end - 16.0, badge.y, 16.0, BADGE), BOLT, Font::icons(14.0), theme.color("yellow").unwrap_or(theme.text), Align::Center);
            end -= 20.0;
        }
        // The name, and what's known about it
        let x = badge.x + BADGE + 10.0;
        let w = (end - x).max(40.0);
        scene.text(Rect::new(x, badge.y, w, 19.0), d.name.as_str(), Font::new(theme.font_size, Weight::SemiBold), p.text, Align::Start);
        scene.text(Rect::new(x, badge.y + 19.0, w, 15.0), details(d), Font::new(theme.small_size, Weight::Medium), theme.subtext, Align::Start);
        // The bars: one, or one a half
        let bar_y = badge.y + BADGE + 9.0;
        let inner = r.w - 2.0 * PAD;
        let bar = |scene: &mut Scene, x: f32, y: f32, w: f32, level: u8, fill: Rgba| {
            scene.fill(Rect::new(x, y, w, BAR), BAR / 2.0, p.track);
            if d.known && level > 0 {
                scene.fill(Rect::new(x, y, (w * level as f32 / 100.0).round().max(BAR), BAR), BAR / 2.0, fill);
            }
        };
        match d.second {
            Some(second) => {
                let small = Font::new(theme.small_size, Weight::Medium);
                let label_w = 52.0;
                let number_w = 40.0;
                for (i, level) in [d.battery, second].into_iter().enumerate() {
                    let y = bar_y + i as f32 * (HALF + 3.0);
                    let label = format!("{} {}", t("Metà", "Half"), i + 1);
                    scene.text(Rect::new(r.x + PAD, y, label_w, HALF), label, small, theme.subtext, Align::Start);
                    bar(scene, r.x + PAD + label_w, y + (HALF - BAR) / 2.0, inner - label_w - number_w, level, if d.connected { level_colors(theme, level).0 } else { p.fill });
                    let color = if d.connected { level_colors(theme, level).1 } else { theme.subtext };
                    scene.text(Rect::new(right - number_w, y, number_w, HALF), format!("{level}%"), small, color, Align::End);
                }
            }
            None => bar(scene, r.x + PAD, bar_y, inner, if d.approx || !d.known { d.battery } else { d.lowest() }, p.fill),
        }
    }
}

/// Both sources, each device once. A maker's devices of one kind are told apart only by that:
/// while some are connected the others go (a keyboard on its receiver hides its Bluetooth pairing,
/// and the other way round); with none connected one stays, the first with a level, the
/// receiver's (fresher) before Bluetooth's. A ROG receiver's device takes its Bluetooth name;
/// XInput's controllers that are on Bluetooth too go.
fn combine(bluetooth: Vec<Device>, wired: &mut [Device]) -> Vec<Device> {
    let same = |a: &Device, b: &Device| a.maker == b.maker && a.maker != Maker::Other && a.kind == b.kind;
    for w in wired.iter_mut().filter(|w| w.name.is_empty()) {
        w.name = match bluetooth.iter().find(|b| same(b, w)) {
            Some(b) => b.name.clone(),
            None if w.kind == Kind::Mouse => t("Mouse ROG", "ROG mouse").to_string(),
            None => t("Tastiera ROG", "ROG keyboard").to_string(),
        };
    }
    // An Xbox controller over Bluetooth is in XInput too, with fewer levels
    let mut xbox_bluetooth = bluetooth.iter().filter(|b| b.connected && b.kind == Kind::Controller && b.maker == Maker::Microsoft).count();
    let mut all = bluetooth;
    for w in wired.iter() {
        if w.approx && xbox_bluetooth > 0 {
            xbox_bluetooth -= 1;
            continue;
        }
        all.push(w.clone());
    }
    let keep: Vec<bool> = (0..all.len())
        .map(|i| {
            let d = &all[i];
            if d.maker == Maker::Other {
                return true;
            }
            let group: Vec<usize> = (0..all.len()).filter(|j| same(&all[*j], d)).collect();
            if group.iter().any(|j| all[*j].connected) {
                return d.connected;
            }
            let rank = |j: usize| (all[j].known, all[j].link != Link::Bluetooth);
            let best = group.iter().copied().fold(group[0], |b, j| if rank(j) > rank(b) { j } else { b });
            best == i
        })
        .collect();
    let mut out: Vec<Device> = all.into_iter().zip(keep).filter(|(_, k)| *k).map(|(d, _)| d).collect();
    sort(&mut out);
    out
}

/// Connected first, then by name.
fn sort(devices: &mut [Device]) {
    devices.sort_by_key(|d| (!d.connected, d.name.to_lowercase()));
}

fn connected(node: u32) -> bool {
    property(node, &DEVPKEY_Device_DevNodeStatus)
        .and_then(|b| b.get(..4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]])))
        .is_some_and(|status| status & DN_DEVICE_DISCONNECTED == 0)
}

/// The vendor id in a Bluetooth HID node's id: `…_VID&02068e_PID&…` (Low Energy, its source in
/// front) or `…_VID&0002054c_PID&…` (classic).
fn vendor_id(id: &str) -> Option<u16> {
    let after = &id[id.find("VID&")? + 4..];
    let digits: String = after.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    u16::from_str_radix(digits.get(digits.len().checked_sub(4)?..)?, 16).ok()
}

/// A string property (UTF-16, with its terminating zero).
fn text(bytes: Option<Vec<u8>>) -> String {
    let Some(bytes) = bytes else { return String::new() };
    let wide: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).take_while(|c| *c != 0).collect();
    String::from_utf16_lossy(&wide).trim().to_string()
}

/// A kind of `pinned`.
fn kind_named(name: &str) -> Option<Kind> {
    match name.to_lowercase().as_str() {
        "mouse" | "mice" => Some(Kind::Mouse),
        "keyboard" | "keyboards" => Some(Kind::Keyboard),
        "controller" | "controllers" | "gamepad" => Some(Kind::Controller),
        "other" => Some(Kind::Other),
        _ => None,
    }
}

/// The kind of a Bluetooth category (`Input.Keyboard`), or of the made-up data's `mouse`.
fn kind_of(category: &str) -> Kind {
    let c = category.to_lowercase();
    if c.contains("keyboard") {
        Kind::Keyboard
    } else if c.contains("mouse") {
        Kind::Mouse
    } else if c.contains("gam") || c.contains("joystick") || c.contains("controller") {
        Kind::Controller
    } else {
        Kind::Other
    }
}

/// Segoe Fluent Icons' glyph for a kind of device (`Input.Keyboard`, `Communication.Phone`…).
fn icon(category: &str) -> &'static str {
    let c = category.to_lowercase();
    match kind_of(category) {
        Kind::Other => {}
        kind => return kind.icon(),
    }
    if c.contains("phone") && !c.contains("headphone") {
        "\u{E8EA}"
    } else if c.contains("headset") || c.contains("headphone") || c.contains("earbud") {
        "\u{E7F6}"
    } else if c.contains("speaker") || c.contains("audio") {
        "\u{E7F5}"
    } else if c.contains("pen") {
        "\u{EDC6}"
    } else {
        "\u{E702}"
    }
}

/// The bolt beside a level that charges (Segoe Fluent Icons' `LightningBolt`).
const BOLT: &str = "\u{E945}";

/// The maker as the popup names it.
fn maker_name(maker: Maker) -> Option<&'static str> {
    match maker {
        Maker::Razer => Some("Razer"),
        Maker::Logitech => Some("Logitech"),
        Maker::Asus => Some("ROG"),
        Maker::Valve => Some("Valve"),
        Maker::Sony => Some("Sony"),
        Maker::Microsoft => Some("Xbox"),
        Maker::Other => None,
    }
}

/// The name in the pill: the user's (`names`, by the full name), or the model's without its maker
/// and the words every device has: `Razer Naga V2 Pro` → `Naga`, `ROG FALCATA` → `Falcata`,
/// `MX Master 4` → `MX Master 4`, `Steam Controller 2` → `Steam 2`.
fn short_name(name: &str, names: &HashMap<String, String>) -> String {
    if let Some(own) = names.get(&name.to_lowercase()) {
        return own.clone();
    }
    const MAKERS: [&str; 8] = ["razer", "rog", "asus", "logitech", "logi", "sony", "microsoft", "valve"];
    const GENERIC: [&str; 6] = ["controller", "wireless", "gaming", "mouse", "keyboard", "headset"];
    let mut words: Vec<&str> = name.split_whitespace().collect();
    while words.len() > 1 && MAKERS.contains(&words[0].to_lowercase().as_str()) {
        words.remove(0);
    }
    let kept: Vec<&str> = words.iter().copied().filter(|w| !GENERIC.contains(&w.to_lowercase().as_str())).collect();
    if !kept.is_empty() {
        words = kept;
    }
    // A number at the end tells two of a kind apart (or is the model's: `MX Master 4`)
    let number = words.last().filter(|w| w.len() == 1 && w.chars().all(|c| c.is_ascii_digit())).copied();
    let mut short: Vec<&str> = vec![words[0]];
    if words[0].chars().count() <= 3 && words.len() > 1 && Some(words[1]) != number {
        short.push(words[1]);
    }
    if let Some(n) = number
        && words.len() > short.len()
    {
        short.push(n);
    }
    // `FALCATA` as `Falcata`: a name in capitals shouts in a pill
    short
        .iter()
        .map(|w| {
            if w.chars().count() > 3 && w.chars().all(|c| c.is_ascii_uppercase()) {
                let lower = w.to_lowercase();
                lower[..1].to_uppercase() + &lower[1..]
            } else {
                w.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// XInput's four levels in words.
fn approx_level(level: u8) -> &'static str {
    match level {
        0..=10 => t("Scarica", "Empty"),
        11..=40 => t("Bassa", "Low"),
        41..=80 => t("Media", "Medium"),
        _ => t("Piena", "Full"),
    }
}

/// The palette color of a level: green above half, yellow down to a quarter, peach (orange) down to
/// a tenth, red under it.
fn level_name(level: u8) -> &'static str {
    match level {
        0..=10 => "red",
        11..=25 => "peach",
        26..=50 => "yellow",
        _ => "green",
    }
}

/// The color of a level's bar, and of its number: the text's while it's green, the bar's from
/// yellow down.
fn level_colors(theme: &Theme, level: u8) -> (Rgba, Rgba) {
    let fill = theme.color(level_name(level)).unwrap_or(theme.text);
    (fill, if level > 50 { theme.text } else { fill })
}

/// A device's colors: the level's bar and its number (`level_colors`), the icon, and the bar's
/// track; all dimmed while it isn't connected (asleep behind its receiver).
struct Paint {
    fill: Rgba,
    level: Rgba,
    text: Rgba,
    track: Rgba,
}

fn paint(theme: &Theme, d: &Device) -> Paint {
    if d.connected {
        let (fill, level) = level_colors(theme, d.lowest());
        Paint { fill, level, text: theme.text, track: theme.text.alpha(0.2) }
    } else {
        Paint { fill: theme.subtext.alpha(0.75), level: theme.subtext, text: theme.subtext, track: theme.subtext.alpha(0.25) }
    }
}

/// Under its name in the popup: the maker, how it's connected, whether it charges or sleeps
/// (`Razer · cavo · in carica`, `Logitech · non risponde · letto alle 00:58`).
fn details(d: &Device) -> String {
    let mut parts: Vec<String> = maker_name(d.maker).map(str::to_string).into_iter().collect();
    if d.asleep {
        parts.push(t("non risponde", "not answering").into());
        if let Some(at) = &d.read_at {
            parts.push(format!("{} {at}", t("letto alle", "read at")));
        }
    } else if !d.connected {
        parts.push(t("non collegato", "not connected").into());
    } else {
        let link = match d.link {
            Link::Bluetooth => Some("Bluetooth"),
            Link::Receiver => Some(t("ricevitore", "receiver")),
            Link::Cable => Some(t("cavo", "cable")),
            Link::Unknown => None,
        };
        parts.extend(link.map(str::to_string));
        if d.charging {
            parts.push(t("in carica", "charging").into());
        }
    }
    let line = parts.join(" · ");
    let mut chars = line.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// Its level as a number: the lower half of a split keyboard, XInput's word, or `–` if never read.
fn level_text(d: &Device) -> String {
    if !d.known {
        "–".to_string()
    } else if d.approx {
        approx_level(d.battery).to_string()
    } else {
        format!("{}%", d.lowest())
    }
}

impl super::Widget for Devices {
    fn pill(&self, theme: &Theme) -> Option<Pill> {
        let shown = self.in_pill();
        if shown.is_empty() {
            return None;
        }
        // Each device: its kind, its short name, its level as a bar and a number, the bolt while it
        // charges; a thin line between them
        let mut items = Vec::new();
        let bolt = theme.color("yellow").unwrap_or(theme.text);
        for (i, d) in shown.iter().enumerate() {
            let p = paint(theme, d);
            if i > 0 {
                items.push(Item::rule(theme.text.alpha(0.15)).space(10.0));
            }
            let icon = Item::icon(d.icon, p.text);
            items.push(if i == 0 { icon } else { icon.space(10.0) });
            items.push(Item::text(short_name(&d.name, &self.names), theme.subtext).small().max_width(120.0).space(6.0));
            items.push(Item::gauge(d.lowest(), p.fill, p.track).space(7.0));
            if !d.approx {
                items.push(Item::text(format!("{}%", d.lowest()), p.level).space(5.0));
            }
            if d.charging {
                items.push(Item::icon(BOLT, bolt).space(3.0));
            }
        }
        Some(Pill::new(items).gap(6.0))
    }

    /// The kinds and their levels as bars, without names and numbers.
    fn compact(&self, theme: &Theme) -> Option<Pill> {
        let shown = self.in_pill();
        if shown.is_empty() {
            return None;
        }
        let mut items = Vec::new();
        for (i, d) in shown.iter().enumerate() {
            let p = paint(theme, d);
            let icon = Item::icon(d.icon, p.text);
            items.push(if i == 0 { icon } else { icon.space(10.0) });
            items.push(Item::gauge(d.lowest(), p.fill, p.track).space(5.0));
        }
        Some(Pill::new(items).gap(4.0))
    }

    fn handles(&self) -> Vec<HANDLE> {
        self.timer.handle().into_iter().chain(self.settle.handle()).chain(if self.mock { None } else { self.wake.handle() }).collect()
    }

    fn signaled_at(&mut self, index: usize) -> bool {
        let Some(handle) = self.handles().get(index).copied() else { return false };
        let before = self.devices.clone();
        if Some(handle) == self.wake.handle() {
            self.take();
        } else if Some(handle) == self.settle.handle() {
            self.settle.stop();
            self.scan();
            self.start(true);
            self.schedule();
        } else {
            for p in &mut self.bluetooth {
                p.read();
            }
            self.start(false);
        }
        self.rebuild();
        before != self.devices
    }

    fn event(&mut self, signal: &Signal) -> bool {
        if self.mock {
            return false;
        }
        match signal {
            Signal::DevicesChanged if self.shown => {
                self.settle.stop();
                self.settle.start(SETTLE, 0);
                false
            }
            Signal::Shown(shown) if *shown != self.shown => {
                self.shown = *shown;
                if *shown {
                    let changed = self.refresh(true);
                    self.schedule();
                    return changed;
                }
                self.settle.stop();
                self.schedule();
                false
            }
            Signal::Game(game) => {
                self.game = *game;
                self.schedule();
                false
            }
            Signal::Resume => self.refresh(true),
            _ => false,
        }
    }

    fn has_popup(&self) -> bool {
        true
    }

    fn popup_shown(&mut self, open: bool) {
        if open && !self.mock {
            self.refresh(false);
        }
    }

    /// A tile for each device that's connected (or asleep behind its receiver): its kind in a
    /// round badge, name, maker, link and state, the level big and as a bar (one for each half of
    /// a split keyboard); then the others in short rows.
    fn popup(&self, theme: &Theme, fonts: &Fonts) -> Option<Scene> {
        let (tiles, others): (Vec<&Device>, Vec<&Device>) = self.devices.iter().partition(|d| d.connected || d.asleep);
        let mut height = MARGIN + HEADER + tiles.iter().map(|d| tile_height(d) + GAP).sum::<f32>();
        if !others.is_empty() {
            height += SECTION + others.len() as f32 * ROW + 4.0;
        }
        if self.devices.is_empty() {
            height += ROW;
        }
        height += 2.0 + BUTTON + 6.0;
        let mut scene = Scene::new(WIDTH, height);
        let mut y = MARGIN;
        let connected = self.devices.iter().filter(|d| d.connected).count();
        scene.text(Rect::new(MARGIN + 2.0, y, INNER - 4.0, HEADER), t("Dispositivi", "Devices"), Font::new(theme.font_size, Weight::SemiBold), theme.text, Align::Start);
        if connected > 0 {
            let count = if connected == 1 { t("1 collegato", "1 connected").to_string() } else { format!("{connected} {}", t("collegati", "connected")) };
            scene.text(Rect::new(MARGIN + 2.0, y, INNER - 4.0, HEADER), count, Font::new(theme.small_size, Weight::Medium), theme.subtext, Align::End);
        }
        y += HEADER;
        if self.devices.is_empty() {
            let none = t("Nessun dispositivo con la batteria", "No device with a battery");
            scene.text(Rect::new(MARGIN + PAD, y, INNER - 2.0 * PAD, ROW), none, Font::new(theme.font_size, Weight::Regular), theme.subtext, Align::Start);
            y += ROW;
        }
        for d in tiles {
            let h = tile_height(d);
            self.tile(&mut scene, theme, fonts, d, Rect::new(MARGIN, y, INNER, h));
            y += h + GAP;
        }
        if !others.is_empty() {
            scene.text(Rect::new(MARGIN + 2.0, y, INNER - 4.0, SECTION), t("Non collegati", "Not connected"), Font::new(theme.small_size, Weight::SemiBold), theme.subtext, Align::Start);
            y += SECTION;
            let regular = Font::new(theme.font_size, Weight::Regular);
            for d in others {
                scene.text(Rect::new(MARGIN + 4.0, y, 20.0, ROW), d.icon, Font::icons(14.0), theme.subtext, Align::Center);
                let value = level_text(d);
                scene.text(Rect::new(MARGIN + INNER - 64.0, y, 60.0, ROW), value, regular, theme.subtext, Align::End);
                scene.text(Rect::new(MARGIN + 32.0, y, INNER - 104.0, ROW), d.name.as_str(), regular, theme.subtext, Align::Start);
                y += ROW;
            }
            y += 4.0;
        }
        y += 2.0;
        scene.text(Rect::new(MARGIN + PAD, y, INNER - 2.0 * PAD, BUTTON), t("Impostazioni Bluetooth", "Bluetooth settings"), Font::new(theme.font_size, Weight::Regular), theme.primary, Align::Start);
        scene.zone(Rect::new(MARGIN, y, INNER, BUTTON), theme.row_radius, "settings");
        Some(scene)
    }

    fn popup_input(&mut self, input: &Input) -> Reply {
        match input {
            Input::Action("settings") if !self.mock => {
                process::open("ms-settings:bluetooth");
                Reply::Opened
            }
            _ => Reply::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str, kind: Kind, maker: Maker, link: Link, battery: u8, connected: bool) -> Device {
        Device { name: name.into(), kind, icon: kind.icon(), maker, link, battery, known: true, second: None, charging: false, approx: false, connected, asleep: false, read_at: None }
    }

    #[test]
    fn kinds_of_devices() {
        assert_eq!(icon("Input.Keyboard"), "\u{E765}");
        assert_eq!(icon("Input.Mouse"), "\u{E962}");
        assert_eq!(icon("Communication.Phone"), "\u{E8EA}");
        assert_eq!(icon("Communication.Headset.Bluetooth"), "\u{E7F6}");
        assert_eq!(icon("Multimedia.Headphone"), "\u{E7F6}");
        assert_eq!(icon("Input.Gaming"), "\u{E7FC}");
        assert_eq!(icon("controller"), "\u{E7FC}");
        assert_eq!(icon(""), "\u{E702}");
        assert_eq!(kind_of("Input.Gaming"), Kind::Controller);
        assert_eq!(kind_named("Mouse"), Some(Kind::Mouse));
        assert_eq!(kind_named("toaster"), None);
    }

    #[test]
    fn strings_of_properties() {
        let bytes: Vec<u8> = "Keyboard\0".encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        assert_eq!(text(Some(bytes)), "Keyboard");
        assert_eq!(text(None), "");
        assert_eq!(vendor_id(r"BTHLEDEVICE\{00001812-0000-1000-8000-00805F9B34FB}_DEV_VID&02068E_PID&02B5_REV&0001_EE7A"), Some(0x068E));
        assert_eq!(vendor_id(r"BTHENUM\{00001124-0000-1000-8000-00805F9B34FB}_VID&0002054C_PID&0CE6\9&1"), Some(0x054C));
        assert_eq!(vendor_id(r"BTHLE\DEV_EE7A543ECF4E\9&2C0F"), None);
    }

    #[test]
    fn one_device_on_two_links() {
        // A keyboard paired over Bluetooth but on its receiver now, a mouse of another maker
        let bluetooth = vec![device("Tempo Keys", Kind::Keyboard, Maker::Razer, Link::Bluetooth, 80, false), device("Glide", Kind::Mouse, Maker::Logitech, Link::Bluetooth, 50, false)];
        let mut wired = vec![device("Tempo Keys 75%", Kind::Keyboard, Maker::Razer, Link::Unknown, 94, true)];
        let out = combine(bluetooth, &mut wired);
        let names: Vec<&str> = out.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["Tempo Keys 75%", "Glide"]);
        // The receiver alone while the keyboard is on Bluetooth
        let bluetooth = vec![device("Tempo Keys", Kind::Keyboard, Maker::Razer, Link::Bluetooth, 80, true)];
        let mut wired = vec![device("Tempo Keys 75%", Kind::Keyboard, Maker::Razer, Link::Unknown, 94, false)];
        assert_eq!(combine(bluetooth, &mut wired).len(), 1);
        // Asleep on both sides: the one with a level, the receiver's first
        let bluetooth = vec![device("Tempo Keys", Kind::Keyboard, Maker::Razer, Link::Bluetooth, 3, false)];
        let mut never = device("Tempo Keys 75%", Kind::Keyboard, Maker::Razer, Link::Receiver, 0, false);
        never.known = false;
        let out = combine(bluetooth.clone(), &mut [never]);
        assert_eq!((out.len(), out[0].link), (1, Link::Bluetooth));
        let out = combine(bluetooth, &mut [device("Tempo Keys 75%", Kind::Keyboard, Maker::Razer, Link::Receiver, 90, false)]);
        assert_eq!((out.len(), out[0].battery), (1, 90));
        // Two connected of a kind both stay
        let mut pads = [device("Pad 1", Kind::Controller, Maker::Valve, Link::Receiver, 50, true), device("Pad 2", Kind::Controller, Maker::Valve, Link::Receiver, 60, true)];
        assert_eq!(combine(Vec::new(), &mut pads).len(), 2);
    }

    #[test]
    fn a_rog_receiver_takes_the_bluetooth_name() {
        let bluetooth = vec![device("Split 75", Kind::Keyboard, Maker::Asus, Link::Bluetooth, 40, false)];
        let mut wired = vec![device("", Kind::Keyboard, Maker::Asus, Link::Receiver, 53, true), device("", Kind::Mouse, Maker::Asus, Link::Receiver, 70, true)];
        let out = combine(bluetooth, &mut wired);
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|d| d.name == "Split 75" && d.connected));
        assert!(out.iter().any(|d| d.kind == Kind::Mouse && !d.name.is_empty()));
    }

    #[test]
    fn xbox_on_bluetooth_once() {
        let bluetooth = vec![device("Pad", Kind::Controller, Maker::Microsoft, Link::Bluetooth, 70, true)];
        let mut pad = device("Xbox", Kind::Controller, Maker::Microsoft, Link::Unknown, 60, true);
        pad.approx = true;
        let mut wired = vec![pad];
        assert_eq!(combine(bluetooth, &mut wired).len(), 1);
    }

    #[test]
    fn what_the_pill_shows() {
        let mut d = Devices { devices: Vec::new(), bluetooth: Vec::new(), wired: Vec::new(), last: HashMap::new(), names: HashMap::new(), work: Arc::new(Work::default()), wake: Wake::new(), reading: false, again: None, any_wired: false, timer: Timer::new(), settle: Timer::new(), low: 20, always: false, pinned: vec![Kind::Mouse, Kind::Keyboard], receivers: false, shown: true, game: false, mock: true };
        let mut split = device("Split", Kind::Keyboard, Maker::Asus, Link::Receiver, 53, true);
        split.second = Some(48);
        let mut asleep = device("Asleep", Kind::Mouse, Maker::Logitech, Link::Receiver, 0, false);
        asleep.known = false;
        d.devices = vec![device("Mouse", Kind::Mouse, Maker::Razer, Link::Cable, 100, true), split, device("Phone", Kind::Other, Maker::Other, Link::Bluetooth, 64, true), device("Buds", Kind::Other, Maker::Other, Link::Bluetooth, 15, true), asleep];
        let names: Vec<&str> = d.in_pill().iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["Mouse", "Split", "Buds"]);
        assert_eq!(d.devices[1].lowest(), 48);
        assert_eq!(level_text(&d.devices[1]), "48%");
        assert_eq!(level_text(&d.devices[4]), "–");
        d.always = true;
        assert_eq!(d.in_pill().len(), 4);
    }

    #[test]
    fn a_keyboard_that_sleeps_stays_dimmed() {
        let mut d = Devices { devices: Vec::new(), bluetooth: Vec::new(), wired: Vec::new(), last: HashMap::new(), names: HashMap::new(), work: Arc::new(Work::default()), wake: Wake::new(), reading: false, again: None, any_wired: false, timer: Timer::new(), settle: Timer::new(), low: 20, always: false, pinned: vec![Kind::Keyboard], receivers: false, shown: true, game: false, mock: true };
        let mut keys = Found { key: "razer:keys".into(), name: "Keys".into(), kind: Kind::Keyboard, maker: Maker::Razer, link: Link::Unknown, level: None, second: None, charging: false, approx: false };
        // Never read since the bar started: not in the pill
        d.wired = vec![keys.clone()];
        d.rebuild();
        assert!(d.in_pill().is_empty());
        // Read once, then asleep: in the pill with that level, dimmed, and "not answering"
        d.last.insert(keys.key.clone(), (64, None, "00:58".into()));
        d.rebuild();
        let shown = d.in_pill();
        assert_eq!((shown.len(), shown[0].battery, shown[0].asleep, shown[0].connected), (1, 64, true, false));
        assert_eq!(details(shown[0]), format!("Razer · {} · {} 00:58", t("non risponde", "not answering"), t("letto alle", "read at")));
        // Awake again
        keys.level = Some(60);
        d.wired = vec![keys];
        d.rebuild();
        assert!(d.devices[0].connected && !d.devices[0].asleep);
    }

    #[test]
    fn what_a_tile_says() {
        let mut d = device("Mouse", Kind::Mouse, Maker::Razer, Link::Cable, 100, true);
        d.charging = true;
        assert_eq!(details(&d), format!("Razer · {} · {}", t("cavo", "cable"), t("in carica", "charging")));
        let phone = device("Phone", Kind::Other, Maker::Other, Link::Bluetooth, 64, true);
        assert_eq!(details(&phone), "Bluetooth");
        let mut split = device("Split", Kind::Keyboard, Maker::Asus, Link::Receiver, 53, true);
        split.second = Some(48);
        assert_eq!(tile_height(&split) - tile_height(&phone), 2.0 * HALF + 3.0 - BAR);
    }

    #[test]
    fn colors_of_levels() {
        // The MX Master 4 at 25% is nearly flat: orange, not green
        let names: Vec<&str> = [100, 51, 50, 26, 25, 11, 10, 0].into_iter().map(level_name).collect();
        assert_eq!(names, ["green", "green", "yellow", "yellow", "peach", "peach", "red", "red"]);
    }

    #[test]
    fn short_names() {
        let none = HashMap::new();
        assert_eq!(short_name("Razer Naga V2 Pro", &none), "Naga");
        assert_eq!(short_name("Razer BlackWidow V4 Pro 75%", &none), "BlackWidow");
        assert_eq!(short_name("ROG FALCATA", &none), "Falcata");
        assert_eq!(short_name("MX Master 4", &none), "MX Master 4");
        assert_eq!(short_name("Steam Controller", &none), "Steam");
        assert_eq!(short_name("Steam Controller 2", &none), "Steam 2");
        assert_eq!(short_name("Controller Xbox", &none), "Xbox");
        assert_eq!(short_name("DualSense", &none), "DualSense");
        assert_eq!(short_name("S24 Ultra", &none), "S24 Ultra");
        let own = HashMap::from([("razer naga v2 pro".to_string(), "Gioco".to_string())]);
        assert_eq!(short_name("Razer Naga V2 Pro", &own), "Gioco");
    }
}
