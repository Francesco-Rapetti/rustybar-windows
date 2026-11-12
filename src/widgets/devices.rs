//! The batteries of the Bluetooth devices, as Settings > Bluetooth & devices shows them: a phone,
//! earbuds, a keyboard, a mouse, a controller. The pill shows only while a connected one runs low
//! (at `low`, 20%; `always = true` keeps it): its kind and its level, yellow, red at 10%. The popup
//! lists every paired device that has a battery, the connected ones first, the others dimmed with
//! the last level Windows had, and the Bluetooth settings.
//!
//! The level is the property Settings shows, on one of the device's Bluetooth nodes (Configuration
//! Manager, as `audio` reads its headphones'); the name, the kind (`Input.Keyboard`,
//! `Communication.Phone`…) and whether it's connected (`DN_DEVICE_DISCONNECTED`) are on its device
//! node, in the same container. Windows doesn't announce a new level: the known nodes are read
//! again every 5 minutes while a bar shows without a game of the list in front (well under a
//! millisecond); everything is read again (~10 ms for some 75 nodes) a second after Windows says
//! devices changed (`WM_DEVICECHANGE`), as one does when it connects, and when the popup opens.
//!
//! With made-up data (see `mock`) the devices are those of `devices.json`.

use windows::Win32::Devices::Properties::{DEVPKEY_Device_ContainerId, DEVPKEY_Device_DevNodeStatus, DEVPKEY_Device_FriendlyName};
use windows::Win32::Foundation::{DEVPROPKEY, HANDLE};
use windows::core::GUID;

use super::audio::{BATTERY, battery_color, battery_icon, device_ids, locate, property};
use super::{Input, Reply, Signal};
use crate::config::Options;
use crate::i18n::t;
use crate::process;
use crate::theme::Theme;
use crate::ui::{Align, Font, Fonts, Item, Pill, Rect, Scene, Weight};
use crate::wake::Timer;

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

// The popup's layout, in DIPs.
const MARGIN: f32 = 8.0;
const INNER: f32 = 284.0;
const WIDTH: f32 = INNER + 2.0 * MARGIN;
const PAD: f32 = 10.0;
const ROW: f32 = 36.0;
const HEADER: f32 = 28.0;
const BUTTON: f32 = 30.0;

/// A device with a battery.
#[derive(Clone, Debug, PartialEq)]
struct Device {
    name: String,
    icon: &'static str,
    battery: u8,
    connected: bool,
    /// The node with the level, and the device's own node (with its name and its state).
    battery_node: u32,
    device_node: Option<u32>,
}

impl Device {
    /// Reads the level and the state again. True if either changed.
    fn read(&mut self) -> bool {
        let before = (self.battery, self.connected);
        if let Some(b) = property(self.battery_node, &BATTERY).and_then(|b| b.first().copied()).filter(|b| *b <= 100) {
            self.battery = b;
        }
        if let Some(node) = self.device_node {
            self.connected = connected(node);
        }
        before != (self.battery, self.connected)
    }
}

pub struct Devices {
    devices: Vec<Device>,
    timer: Timer,
    settle: Timer,
    low: u8,
    always: bool,
    shown: bool,
    game: bool,
    mock: bool,
}

impl Devices {
    pub fn new(options: &Options) -> Devices {
        let low = options.f64("low").unwrap_or(20.0).clamp(0.0, 100.0) as u8;
        let always = options.bool("always").unwrap_or(false);
        let mut d = Devices { devices: Vec::new(), timer: Timer::new(), settle: Timer::new(), low, always, shown: true, game: false, mock: false };
        if let Some(m) = crate::mock::load("devices") {
            d.mock = true;
            d.devices = m["devices"]
                .array()
                .iter()
                .map(|v| Device {
                    name: v["name"].str().unwrap_or_default().to_string(),
                    icon: icon(v["category"].str().unwrap_or_default()),
                    battery: v["battery"].f64().unwrap_or(0.0).clamp(0.0, 100.0) as u8,
                    connected: v["connected"].bool().unwrap_or(false),
                    battery_node: 0,
                    device_node: None,
                })
                .collect();
            sort(&mut d.devices);
            return d;
        }
        d.scan();
        d.schedule();
        d
    }

    /// Every Bluetooth device with a battery, read anew.
    fn scan(&mut self) {
        let mut devices: Vec<(Vec<u8>, Device)> = Vec::new();
        let mut device_nodes: Vec<(Vec<u8>, u32, String)> = Vec::new();
        for enumerator in BLUETOOTH {
            for id in device_ids(enumerator) {
                let Some(node) = locate(&id) else { continue };
                let Some(container) = property(node, &DEVPKEY_Device_ContainerId) else { continue };
                // The device's own node (`BTHENUM\Dev_…`, `BTHLE\Dev_…`) has its name and its state
                if id.to_uppercase().contains("\\DEV_") {
                    device_nodes.push((container.clone(), node, text(property(node, &DEVPKEY_Device_FriendlyName))));
                }
                let Some(battery) = property(node, &BATTERY).and_then(|b| b.first().copied()).filter(|b| *b <= 100) else { continue };
                if devices.iter().any(|(c, _)| *c == container) {
                    continue;
                }
                let name = text(property(node, &DEVPKEY_Device_FriendlyName));
                devices.push((container, Device { name, icon: icon(""), battery, connected: true, battery_node: node, device_node: None }));
            }
        }
        for (container, device) in &mut devices {
            if let Some((_, node, name)) = device_nodes.iter().find(|(c, _, _)| c == container) {
                device.device_node = Some(*node);
                device.connected = connected(*node);
                device.icon = icon(&text(property(*node, &CATEGORY)));
                if !name.is_empty() {
                    device.name = name.clone();
                }
            }
        }
        let mut devices: Vec<Device> = devices.into_iter().map(|(_, d)| d).collect();
        sort(&mut devices);
        self.devices = devices;
    }

    /// The levels are read again every 5 minutes while a bar shows without a listed game in front.
    fn schedule(&mut self) {
        if self.shown && !self.game && !self.mock && !self.devices.is_empty() {
            self.timer.start(EVERY as i64, EVERY);
        } else {
            self.timer.stop();
        }
    }

    /// The connected devices the pill shows: those that run low, or all with `always`.
    fn in_pill(&self) -> Vec<&Device> {
        self.devices.iter().filter(|d| d.connected && (self.always || d.battery <= self.low)).collect()
    }
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

/// A string property (UTF-16, with its terminating zero).
fn text(bytes: Option<Vec<u8>>) -> String {
    let Some(bytes) = bytes else { return String::new() };
    let wide: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).take_while(|c| *c != 0).collect();
    String::from_utf16_lossy(&wide).trim().to_string()
}

/// Segoe Fluent Icons' glyph for a kind of device (`Input.Keyboard`, `Communication.Phone`…).
fn icon(category: &str) -> &'static str {
    let c = category.to_lowercase();
    if c.contains("keyboard") {
        "\u{E765}"
    } else if c.contains("mouse") {
        "\u{E962}"
    } else if c.contains("gam") || c.contains("joystick") {
        "\u{E7FC}"
    } else if c.contains("phone") && !c.contains("headphone") {
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

impl super::Widget for Devices {
    fn pill(&self, theme: &Theme) -> Option<Pill> {
        let shown = self.in_pill();
        if shown.is_empty() {
            return None;
        }
        let mut items = Vec::new();
        for (i, d) in shown.iter().enumerate() {
            let color = battery_color(theme, d.battery, theme.text);
            let icon = Item::icon(d.icon, theme.text);
            items.push(if i == 0 { icon } else { icon.space(12.0) });
            items.push(Item::icon(battery_icon(d.battery), color).space(4.0));
            items.push(Item::text(format!("{}%", d.battery), color).small().space(2.0));
        }
        Some(Pill::new(items).gap(6.0))
    }

    /// The kinds and their batteries, without the numbers.
    fn compact(&self, theme: &Theme) -> Option<Pill> {
        let shown = self.in_pill();
        if shown.is_empty() {
            return None;
        }
        let items = shown.iter().flat_map(|d| [Item::icon(d.icon, theme.text), Item::icon(battery_icon(d.battery), battery_color(theme, d.battery, theme.text))]).collect();
        Some(Pill::new(items).gap(4.0))
    }

    fn handles(&self) -> Vec<HANDLE> {
        self.timer.handle().into_iter().chain(self.settle.handle()).collect()
    }

    fn signaled_at(&mut self, index: usize) -> bool {
        let Some(handle) = self.handles().get(index).copied() else { return false };
        let before = self.devices.clone();
        if Some(handle) == self.settle.handle() {
            self.settle.stop();
            self.scan();
            self.schedule();
        } else {
            for d in &mut self.devices {
                d.read();
            }
            sort(&mut self.devices);
        }
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
                    let before = self.devices.clone();
                    self.scan();
                    self.schedule();
                    return before != self.devices;
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
            Signal::Resume => {
                let before = self.devices.clone();
                self.scan();
                before != self.devices
            }
            _ => false,
        }
    }

    fn has_popup(&self) -> bool {
        true
    }

    fn popup_shown(&mut self, open: bool) {
        if open && !self.mock {
            self.scan();
        }
    }

    fn popup(&self, theme: &Theme, _fonts: &Fonts) -> Option<Scene> {
        let rows = self.devices.len().max(1);
        let height = MARGIN + HEADER + rows as f32 * ROW + 6.0 + BUTTON + 6.0;
        let mut scene = Scene::new(WIDTH, height);
        let mut y = MARGIN;
        scene.text(Rect::new(MARGIN + PAD, y, INNER - 2.0 * PAD, HEADER), t("Dispositivi Bluetooth", "Bluetooth devices"), Font::new(theme.small_size, Weight::SemiBold), theme.subtext, Align::Start);
        y += HEADER;
        if self.devices.is_empty() {
            let none = t("Nessun dispositivo con la batteria", "No device with a battery");
            scene.text(Rect::new(MARGIN + PAD, y, INNER - 2.0 * PAD, ROW), none, Font::new(theme.font_size, Weight::Regular), theme.subtext, Align::Start);
            y += ROW;
        }
        let small = Font::new(theme.small_size, Weight::Medium);
        for d in &self.devices {
            let color = if d.connected { theme.text } else { theme.subtext };
            scene.text(Rect::new(MARGIN + PAD, y, 20.0, ROW), d.icon, Font::icons(15.0), color, Align::Center);
            let name_x = MARGIN + PAD + 30.0;
            let level_w = 92.0;
            let name_font = Font::new(theme.font_size, if d.connected { Weight::SemiBold } else { Weight::Regular });
            let name_w = MARGIN + INNER - PAD - level_w - name_x;
            let level = if d.connected { battery_color(theme, d.battery, theme.text) } else { theme.subtext };
            let right = MARGIN + INNER - PAD;
            if d.connected {
                scene.text(Rect::new(name_x, y, name_w, ROW), d.name.as_str(), name_font, color, Align::Start);
                scene.text(Rect::new(right - level_w, y, level_w - 46.0, ROW), battery_icon(d.battery), Font::icons(14.0), level, Align::End);
            } else {
                // The level Windows had last, under a note that it may be old
                scene.text(Rect::new(name_x, y + 2.0, name_w, 18.0), d.name.as_str(), name_font, color, Align::Start);
                scene.text(Rect::new(name_x, y + 19.0, name_w, 14.0), t("Non collegato", "Not connected"), small, theme.subtext, Align::Start);
            }
            scene.text(Rect::new(right - 40.0, y, 40.0, ROW), format!("{}%", d.battery), Font::new(theme.font_size, Weight::SemiBold), level, Align::End);
            y += ROW;
        }
        y += 6.0;
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

    #[test]
    fn kinds_of_devices() {
        assert_eq!(icon("Input.Keyboard"), "\u{E765}");
        assert_eq!(icon("Input.Mouse"), "\u{E962}");
        assert_eq!(icon("Communication.Phone"), "\u{E8EA}");
        assert_eq!(icon("Communication.Headset.Bluetooth"), "\u{E7F6}");
        assert_eq!(icon("Multimedia.Headphone"), "\u{E7F6}");
        assert_eq!(icon("Input.Gaming"), "\u{E7FC}");
        assert_eq!(icon(""), "\u{E702}");
    }

    #[test]
    fn strings_of_properties() {
        let bytes: Vec<u8> = "Keyboard\0".encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        assert_eq!(text(Some(bytes)), "Keyboard");
        assert_eq!(text(None), "");
    }
}
