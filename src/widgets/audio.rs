//! The sound: in the pill the output's icon (by its kind and its volume, crossed out when muted)
//! and its volume, and the battery of a Bluetooth output. The popup has a tile for the output and
//! one for the input, each with its name, its volume and a slider (a click or a drag sets it, a
//! click on the icon mutes), then the outputs and the inputs to switch to (the one in use lit,
//! the battery of the Bluetooth ones), the sound settings and the volume mixer.
//!
//! Core Audio tells when a device comes, goes or becomes the default (`IMMNotificationClient`)
//! and when a volume changes (`IAudioEndpointVolumeCallback`, from the keys, another app or this
//! one): its callbacks, on threads of their own, only set an event; the widget reads the rest on
//! the main thread. Nothing is read on a timer but the battery, which Windows doesn't announce:
//! every 5 minutes while the output or the input is Bluetooth and a bar shows without a game in
//! front, from the property Settings shows (Configuration Manager: the Bluetooth device in the
//! endpoint's container). While no bar shows the callbacks are unregistered, so a game full screen
//! wakes nothing; they come back, and everything is read again, when a bar does.
//!
//! Windows has no public way to change the default device: a click on a device uses
//! `IPolicyConfig`, the undocumented interface that Windows' own sound settings use (as do
//! EarTrumpet and SoundSwitch), stable since Vista. If it ever fails, the log says so.

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GETIDLIST_FILTER_ENUMERATOR, CM_GETIDLIST_FILTER_PRESENT, CM_Get_DevNode_PropertyW, CM_Get_Device_ID_List_SizeW,
    CM_Get_Device_ID_ListW, CM_LOCATE_DEVNODE_NORMAL, CM_Locate_DevNodeW, CR_SUCCESS,
};
use windows::Win32::Devices::FunctionDiscovery::{PKEY_Device_EnumeratorName, PKEY_Device_FriendlyName};
use windows::Win32::Devices::Properties::{DEVPKEY_Device_ContainerId, DEVPROPTYPE};
use windows::Win32::Foundation::{DEVPROPKEY, HANDLE, PROPERTYKEY};
use windows::Win32::Media::Audio::Endpoints::{IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl};
use windows::Win32::Media::Audio::{
    AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE, DEVICE_STATE_ACTIVE, EDataFlow, ERole, IMMDevice, IMMDeviceEnumerator, IMMNotificationClient,
    IMMNotificationClient_Impl, MMDeviceEnumerator, PKEY_AudioEndpoint_FormFactor, eCapture, eCommunications, eConsole, eMultimedia, eRender,
};
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantToStringAlloc};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ};
use windows::Win32::System::Variant::VT_UI4;
use windows::core::{GUID, HSTRING, PCWSTR, implement};

use super::{Input, Reply, Signal};
use crate::log;
use crate::process;
use crate::theme::{Rgba, Theme};
use crate::ui::{Align, Font, Fonts, Item, Pill, Rect, Scene, Weight};
use crate::wake::{Timer, Wake};
use crate::i18n::t;

/// How often the battery of a Bluetooth device is read again, in ms.
const BATTERY_EVERY: i32 = 5 * 60 * 1000;

/// What this widget passes when it sets a volume, so its own changes can be told apart.
const CONTEXT: GUID = GUID::from_u128(0x5d1f0f3c_6a64_4f8e_9a37_52b7c3a1e7a1);

/// The battery level Windows shows for a Bluetooth device, on its Bluetooth device node.
const BATTERY: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0x104ea319_6ee2_4701_bd47_8ddbf425bbe5), pid: 2 };

/// The enumerators of Bluetooth device nodes: classic, hands-free, Low Energy.
const BLUETOOTH: [&str; 4] = ["BTHENUM", "BTHHFENUM", "BTHLE", "BTHLEDEVICE"];

// Segoe Fluent Icons
const SPEAKER: [&str; 4] = ["\u{E992}", "\u{E993}", "\u{E994}", "\u{E995}"];
const MUTED: &str = "\u{E74F}";
const HEADPHONES: &str = "\u{E7F6}";
const HEADSET: &str = "\u{E95B}";
const MONITOR: &str = "\u{E7F4}";
const SPEAKERS: &str = "\u{E7F5}";
const MICROPHONE: &str = "\u{E720}";
const MICROPHONE_OFF: &str = "\u{EC54}";

// The popup's layout, in DIPs.
const MARGIN: f32 = 8.0;
const INNER: f32 = 304.0;
const WIDTH: f32 = INNER + 2.0 * MARGIN;
const GAP: f32 = 8.0;
const PAD: f32 = 10.0;
const RADIUS: f32 = 6.0;
const TILE_H: f32 = 76.0;
const HEADER: f32 = 26.0;
const ROW: f32 = 32.0;
const BUTTON: f32 = 30.0;

/// The interface behind Windows' sound settings, with Windows' names: only `SetDefaultEndpoint`
/// is called, the others keep the order of the slots.
#[allow(non_snake_case)]
mod policy {
    use std::ffi::c_void;

    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::Media::Audio::ERole;
    use windows::core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, PCWSTR, interface};

    #[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
    pub unsafe trait IPolicyConfig: IUnknown {
        unsafe fn GetMixFormat(&self, id: PCWSTR, format: *mut *mut c_void) -> HRESULT;
        unsafe fn GetDeviceFormat(&self, id: PCWSTR, default: i32, format: *mut *mut c_void) -> HRESULT;
        unsafe fn ResetDeviceFormat(&self, id: PCWSTR) -> HRESULT;
        unsafe fn SetDeviceFormat(&self, id: PCWSTR, endpoint: *mut c_void, mix: *mut c_void) -> HRESULT;
        unsafe fn GetProcessingPeriod(&self, id: PCWSTR, default: i32, period: *mut i64, minimum: *mut i64) -> HRESULT;
        unsafe fn SetProcessingPeriod(&self, id: PCWSTR, period: *mut i64) -> HRESULT;
        unsafe fn GetShareMode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
        unsafe fn SetShareMode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
        unsafe fn GetPropertyValue(&self, id: PCWSTR, key: *const PROPERTYKEY, value: *mut c_void) -> HRESULT;
        unsafe fn SetPropertyValue(&self, id: PCWSTR, key: *const PROPERTYKEY, value: *mut c_void) -> HRESULT;
        pub unsafe fn SetDefaultEndpoint(&self, id: PCWSTR, role: ERole) -> HRESULT;
        unsafe fn SetEndpointVisibility(&self, id: PCWSTR, visible: i32) -> HRESULT;
    }

    pub const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);
}

use policy::{CLSID_POLICY_CONFIG, IPolicyConfig};

/// Core Audio's word that devices changed: a flag and the event.
#[implement(IMMNotificationClient)]
struct Devices {
    changed: Arc<AtomicBool>,
    wake: Arc<Wake>,
}

impl Devices {
    fn poke(&self) {
        self.changed.store(true, Ordering::Release);
        self.wake.set();
    }
}

impl IMMNotificationClient_Impl for Devices_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> windows::core::Result<()> {
        self.poke();
        Ok(())
    }

    fn OnDeviceAdded(&self, _: &PCWSTR) -> windows::core::Result<()> {
        self.poke();
        Ok(())
    }

    fn OnDeviceRemoved(&self, _: &PCWSTR) -> windows::core::Result<()> {
        self.poke();
        Ok(())
    }

    fn OnDefaultDeviceChanged(&self, _: EDataFlow, _: ERole, _: &PCWSTR) -> windows::core::Result<()> {
        self.poke();
        Ok(())
    }

    // Endpoints change properties all the time; only a new name matters here
    fn OnPropertyValueChanged(&self, _: &PCWSTR, key: &PROPERTYKEY) -> windows::core::Result<()> {
        if *key == PKEY_Device_FriendlyName {
            self.poke();
        }
        Ok(())
    }
}

/// Core Audio's word that a volume changed.
#[implement(IAudioEndpointVolumeCallback)]
struct VolumeChanged {
    wake: Arc<Wake>,
}

impl IAudioEndpointVolumeCallback_Impl for VolumeChanged_Impl {
    fn OnNotify(&self, _: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> windows::core::Result<()> {
        self.wake.set();
        Ok(())
    }
}

/// A device, as the lists show it.
#[derive(Clone, Debug, PartialEq)]
struct Device {
    id: String,
    name: String,
    /// `EndpointFormFactor`: 1 speakers, 2 line, 3 headphones, 4 microphone, 5 headset, 8 S/PDIF,
    /// 9 a display (HDMI, DisplayPort).
    form: u32,
    bluetooth: bool,
}

/// The default output or input, with its volume.
struct Endpoint {
    device: Device,
    volume: Option<IAudioEndpointVolume>,
    callback: Option<IAudioEndpointVolumeCallback>,
    level: f32,
    muted: bool,
    battery: Option<u8>,
}

impl Endpoint {
    fn open(device: &IMMDevice, wake: &Arc<Wake>) -> Option<Endpoint> {
        let info = describe(device)?;
        let volume: Option<IAudioEndpointVolume> = unsafe { device.Activate(CLSCTX_ALL, None) }.ok();
        let callback: Option<IAudioEndpointVolumeCallback> = volume.as_ref().and_then(|v| {
            let callback: IAudioEndpointVolumeCallback = VolumeChanged { wake: wake.clone() }.into();
            unsafe { v.RegisterControlChangeNotify(&callback) }.ok().map(|()| callback)
        });
        let mut e = Endpoint { battery: None, device: info, volume, callback, level: 0.0, muted: false };
        e.read();
        e.read_battery();
        Some(e)
    }

    fn read(&mut self) -> bool {
        let Some(v) = &self.volume else { return false };
        let level = unsafe { v.GetMasterVolumeLevelScalar() }.unwrap_or(self.level);
        let muted = unsafe { v.GetMute() }.map(|m| m.as_bool()).unwrap_or(self.muted);
        let changed = level != self.level || muted != self.muted;
        self.level = level;
        self.muted = muted;
        changed
    }

    fn read_battery(&mut self) -> bool {
        let battery = if self.device.bluetooth { bluetooth_battery(&self.device.id) } else { None };
        let changed = battery != self.battery;
        self.battery = battery;
        changed
    }

    fn percent(&self) -> u32 {
        (self.level * 100.0).round() as u32
    }

    fn set_level(&mut self, level: f32) {
        let level = level.clamp(0.0, 1.0);
        if let Some(v) = &self.volume {
            unsafe {
                let _ = v.SetMasterVolumeLevelScalar(level, &CONTEXT);
                // Dragging the volume up unmutes, as Windows' own slider does
                if self.muted && level > 0.0 {
                    let _ = v.SetMute(false, &CONTEXT);
                    self.muted = false;
                }
            }
            self.level = level;
        }
    }

    fn toggle_mute(&mut self) {
        if let Some(v) = &self.volume {
            self.muted = !self.muted;
            unsafe {
                let _ = v.SetMute(self.muted, &CONTEXT);
            }
        }
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        if let (Some(v), Some(c)) = (&self.volume, &self.callback) {
            unsafe {
                let _ = v.UnregisterControlChangeNotify(c);
            }
        }
    }
}

pub struct Audio {
    enumerator: Option<IMMDeviceEnumerator>,
    wake: Arc<Wake>,
    changed: Arc<AtomicBool>,
    /// Registered with Core Audio while a bar shows.
    client: Option<IMMNotificationClient>,
    output: Option<Endpoint>,
    input: Option<Endpoint>,
    battery_timer: Timer,
    shown: bool,
    game: bool,
    /// While the popup is open: the devices to switch to.
    lists: Option<(Vec<Device>, Vec<Device>)>,
}

impl Audio {
    pub fn new() -> Audio {
        let enumerator: Option<IMMDeviceEnumerator> = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .map_err(|e| log!("audio: no Core Audio ({e})"))
            .ok();
        let mut a = Audio {
            enumerator,
            wake: Wake::new(),
            changed: Arc::new(AtomicBool::new(false)),
            client: None,
            output: None,
            input: None,
            battery_timer: Timer::new(),
            shown: true,
            game: false,
            lists: None,
        };
        a.attach();
        a
    }

    /// Registers with Core Audio and reads everything.
    fn attach(&mut self) {
        let Some(enumerator) = &self.enumerator else { return };
        if self.client.is_none() {
            let client: IMMNotificationClient = Devices { changed: self.changed.clone(), wake: self.wake.clone() }.into();
            match unsafe { enumerator.RegisterEndpointNotificationCallback(&client) } {
                Ok(()) => self.client = Some(client),
                Err(e) => log!("audio: can't watch the devices ({e})"),
            }
        }
        self.defaults();
        self.schedule();
    }

    /// Unregisters every callback: nothing will wake the widget.
    fn detach(&mut self) {
        if let (Some(enumerator), Some(client)) = (&self.enumerator, self.client.take()) {
            unsafe {
                let _ = enumerator.UnregisterEndpointNotificationCallback(&client);
            }
        }
        self.output = None;
        self.input = None;
        self.battery_timer.stop();
    }

    /// The default output and input, opened again if they changed.
    fn defaults(&mut self) -> bool {
        let Some(enumerator) = &self.enumerator else { return false };
        let mut changed = false;
        for flow in [eRender, eCapture] {
            let device = unsafe { enumerator.GetDefaultAudioEndpoint(flow, eConsole) }.ok();
            let id = device.as_ref().and_then(device_id);
            let slot = if flow == eRender { &mut self.output } else { &mut self.input };
            if slot.as_ref().map(|e| &e.device.id) == id.as_ref() {
                // The same device: its name may have changed
                if let (Some(e), Some(d)) = (slot.as_mut(), device.as_ref().and_then(describe))
                    && e.device != d
                {
                    e.device = d;
                    changed = true;
                }
                continue;
            }
            *slot = device.as_ref().and_then(|d| Endpoint::open(d, &self.wake));
            changed = true;
        }
        if changed && self.lists.is_some() {
            self.lists = Some(self.list_devices());
        }
        changed
    }

    fn list_devices(&self) -> (Vec<Device>, Vec<Device>) {
        let list = |flow| -> Vec<Device> {
            let Some(enumerator) = &self.enumerator else { return Vec::new() };
            let Ok(collection) = (unsafe { enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) }) else { return Vec::new() };
            let n = unsafe { collection.GetCount() }.unwrap_or(0);
            let mut devices: Vec<Device> = (0..n).filter_map(|i| unsafe { collection.Item(i) }.ok()).filter_map(|d| describe(&d)).collect();
            devices.sort_by_key(|d| d.name.to_lowercase());
            devices
        };
        (list(eRender), list(eCapture))
    }

    /// The battery timer runs while a Bluetooth device is in use and a bar shows without a game.
    fn schedule(&mut self) {
        let bluetooth = [&self.output, &self.input].iter().any(|e| e.as_ref().is_some_and(|e| e.device.bluetooth));
        if bluetooth && self.shown && !self.game {
            self.battery_timer.start(BATTERY_EVERY as i64, BATTERY_EVERY);
        } else {
            self.battery_timer.stop();
        }
    }

    /// Makes `id` the default for every role, as the sound settings do.
    fn make_default(&self, id: &str) {
        let config: windows::core::Result<IPolicyConfig> = unsafe { CoCreateInstance(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL) };
        let config = match config {
            Ok(c) => c,
            Err(e) => {
                log!("audio: can't change the default device ({e})");
                return;
            }
        };
        let id = HSTRING::from(id);
        for role in [eConsole, eMultimedia, eCommunications] {
            if let Err(e) = unsafe { config.SetDefaultEndpoint(PCWSTR(id.as_ptr()), role) }.ok() {
                log!("audio: can't change the default device ({e})");
                return;
            }
        }
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.detach();
    }
}

impl super::Widget for Audio {
    fn pill(&self, theme: &Theme) -> Option<Pill> {
        let out = self.output.as_ref()?;
        let color = if out.muted { theme.subtext } else { theme.text };
        let mut items = vec![Item::icon(output_icon(out), color), Item::text(format!("{}%", out.percent()), color).fixed("100%")];
        if let Some(b) = out.battery {
            items.push(Item::icon(battery_icon(b), battery_color(theme, b, theme.subtext)).space(10.0));
            items.push(Item::text(format!("{b}%"), battery_color(theme, b, theme.subtext)).small().space(2.0));
        }
        Some(Pill::new(items).gap(6.0))
    }

    fn handles(&self) -> Vec<HANDLE> {
        self.wake.handle().into_iter().chain(self.battery_timer.handle()).collect()
    }

    fn signaled_at(&mut self, index: usize) -> bool {
        if index == 1 {
            // The battery timer
            let mut changed = false;
            for e in [&mut self.output, &mut self.input].into_iter().flatten() {
                changed |= e.read_battery();
            }
            return changed;
        }
        let mut changed = false;
        if self.changed.swap(false, Ordering::AcqRel) {
            changed |= self.defaults();
            if self.lists.is_some() {
                let lists = self.list_devices();
                changed |= self.lists.as_ref() != Some(&lists);
                self.lists = Some(lists);
            }
            self.schedule();
        }
        for e in [&mut self.output, &mut self.input].into_iter().flatten() {
            changed |= e.read();
        }
        changed
    }

    fn event(&mut self, signal: &Signal) -> bool {
        match signal {
            Signal::Shown(shown) if *shown != self.shown => {
                self.shown = *shown;
                if *shown {
                    self.attach();
                } else {
                    self.detach();
                }
                true
            }
            Signal::Game(game) => {
                self.game = *game;
                self.schedule();
                false
            }
            Signal::Resume => {
                let mut changed = self.defaults();
                for e in [&mut self.output, &mut self.input].into_iter().flatten() {
                    changed |= e.read();
                    changed |= e.read_battery();
                }
                changed
            }
            _ => false,
        }
    }

    fn has_popup(&self) -> bool {
        true
    }

    fn popup_shown(&mut self, open: bool) {
        if open {
            self.lists = Some(self.list_devices());
            for e in [&mut self.output, &mut self.input].into_iter().flatten() {
                e.read();
                e.read_battery();
            }
        } else {
            self.lists = None;
        }
    }

    fn popup(&self, theme: &Theme, fonts: &Fonts) -> Option<Scene> {
        let (outputs, inputs) = self.lists.as_ref()?;
        let buttons = [("settings", t("Impostazioni audio", "Sound settings")), ("mixer", t("Mixer volume", "Volume mixer"))];
        let height = MARGIN
            + 2.0 * (TILE_H + GAP)
            + 2.0 * HEADER
            + (outputs.len().max(1) + inputs.len().max(1)) as f32 * ROW
            + GAP
            + buttons.len() as f32 * BUTTON
            + 6.0;
        let mut scene = Scene::new(WIDTH, height);
        let mut y = MARGIN;
        volume_tile(&mut scene, theme, fonts, y, self.output.as_ref(), true);
        y += TILE_H + GAP;
        volume_tile(&mut scene, theme, fonts, y, self.input.as_ref(), false);
        y += TILE_H + GAP;
        for (outputs_list, devices) in [(true, outputs), (false, inputs)] {
            let label = if outputs_list { t("Uscita", "Output") } else { t("Ingresso", "Input") };
            scene.text(Rect::new(MARGIN + PAD, y, INNER - 2.0 * PAD, HEADER), label, Font::new(theme.small_size, Weight::SemiBold), theme.subtext, Align::Start);
            y += HEADER;
            let current = if outputs_list { &self.output } else { &self.input };
            let current = current.as_ref().map(|e| e.device.id.as_str());
            if devices.is_empty() {
                let none = t("Nessun dispositivo", "No device");
                scene.text(Rect::new(MARGIN + PAD, y, INNER - 2.0 * PAD, ROW), none, Font::new(theme.font_size, Weight::Regular), theme.subtext, Align::Start);
                y += ROW;
            }
            for (i, device) in devices.iter().enumerate() {
                let on = current == Some(device.id.as_str());
                let battery = if on { [&self.output, &self.input].into_iter().flatten().find(|e| e.device.id == device.id).and_then(|e| e.battery) } else { None };
                device_row(&mut scene, theme, fonts, y, device, on, battery, outputs_list);
                if !on {
                    scene.zone(Rect::new(MARGIN, y, INNER, ROW), theme.row_radius, format!("{}:{i}", if outputs_list { "out" } else { "in" }));
                }
                y += ROW;
            }
        }
        y += GAP;
        for (action, label) in buttons {
            scene.text(Rect::new(MARGIN + PAD, y, INNER - 2.0 * PAD, BUTTON), label, Font::new(theme.font_size, Weight::Regular), theme.primary, Align::Start);
            scene.zone(Rect::new(MARGIN, y, INNER, BUTTON), theme.row_radius, action);
            y += BUTTON;
        }
        Some(scene)
    }

    fn popup_input(&mut self, input: &Input) -> Reply {
        match input {
            Input::Slide(action, fraction) => {
                let endpoint = match *action {
                    "out-volume" => self.output.as_mut(),
                    "in-volume" => self.input.as_mut(),
                    _ => None,
                };
                let Some(e) = endpoint else { return Reply::Nothing };
                // Whole percents, as the label shows them
                e.set_level((fraction * 100.0).round() / 100.0);
                Reply::Redraw
            }
            Input::Action("out-mute") | Input::Action("in-mute") => {
                let e = if matches!(input, Input::Action("out-mute")) { self.output.as_mut() } else { self.input.as_mut() };
                match e {
                    Some(e) => {
                        e.toggle_mute();
                        Reply::Redraw
                    }
                    None => Reply::Nothing,
                }
            }
            Input::Action("settings") => {
                process::open("ms-settings:sound");
                Reply::Opened
            }
            Input::Action("mixer") => {
                process::open("ms-settings:apps-volume");
                Reply::Opened
            }
            Input::Action(action) => {
                let Some((kind, index)) = action.split_once(':') else { return Reply::Nothing };
                let Some((outputs, inputs)) = &self.lists else { return Reply::Nothing };
                let list = if kind == "out" { outputs } else { inputs };
                let Some(device) = index.parse::<usize>().ok().and_then(|i| list.get(i)) else { return Reply::Nothing };
                self.make_default(&device.id);
                // Core Audio says so too, a moment later; the popup shows it at once
                self.changed.store(true, Ordering::Release);
                self.wake.set();
                Reply::Nothing
            }
            _ => Reply::Nothing,
        }
    }
}

/// The volume of the output or the input: the icon (a click mutes), the name, the percentage and
/// the slider.
fn volume_tile(scene: &mut Scene, theme: &Theme, fonts: &Fonts, y: f32, endpoint: Option<&Endpoint>, output: bool) {
    let r = Rect::new(MARGIN, y, INNER, TILE_H);
    scene.fill(r, RADIUS, theme.tile(theme.primary, false));
    let prefix = if output { "out" } else { "in" };
    let Some(e) = endpoint else {
        let none = if output { t("Nessuna uscita", "No output") } else { t("Nessun ingresso", "No input") };
        scene.text(Rect::new(r.x + PAD, r.y, r.w - 2.0 * PAD, r.h), none, Font::new(theme.font_size, Weight::Regular), theme.subtext, Align::Start);
        return;
    };
    let icon = if output {
        output_icon(e)
    } else if e.muted {
        MICROPHONE_OFF
    } else {
        MICROPHONE
    };
    let color = if e.muted { theme.subtext } else { theme.text };
    let button = Rect::new(r.x + 6.0, r.y + 8.0, 32.0, 32.0);
    scene.text(button, icon, Font::icons(16.0), color, Align::Center);
    scene.zone(button, theme.row_radius, format!("{prefix}-mute"));
    let percent = format!("{}%", e.percent());
    let medium = Font::new(theme.font_size, Weight::SemiBold);
    let percent_w = fonts.width("100%", medium).ceil();
    scene.text(Rect::new(r.x + r.w - PAD - percent_w, button.y, percent_w, button.h), percent, medium, color, Align::End);
    let name_x = button.x + button.w + 6.0;
    scene.text(Rect::new(name_x, button.y, r.x + r.w - PAD - percent_w - 8.0 - name_x, button.h), e.device.name.as_str(), Font::new(theme.font_size, Weight::Regular), theme.text, Align::Start);
    let track = Rect::new(r.x + PAD + 4.0, r.y + 46.0, r.w - 2.0 * PAD - 8.0, 22.0);
    slider(scene, theme, track, e.level, !e.muted);
    scene.slider(Rect::new(track.x - 8.0, track.y, track.w + 16.0, track.h), format!("{prefix}-volume"));
}

/// A slider as Windows 11 draws it: a thin track, filled up to the knob in the primary color, and
/// a round knob with a dot of the primary color in it.
pub fn slider(scene: &mut Scene, theme: &Theme, r: Rect, value: f32, active: bool) {
    let value = value.clamp(0.0, 1.0);
    let mid = r.y + r.h / 2.0;
    let fill = if active { theme.primary } else { theme.subtext };
    scene.fill(Rect::new(r.x, mid - 2.0, r.w, 4.0), 2.0, theme.text.alpha(0.2));
    let x = r.x + r.w * value;
    if x > r.x {
        scene.fill(Rect::new(r.x, mid - 2.0, x - r.x, 4.0), 2.0, fill);
    }
    let knob = theme.color("surface2").unwrap_or(theme.surface);
    scene.fill(Rect::new(x - 9.0, mid - 9.0, 18.0, 18.0), 9.0, knob);
    scene.fill(Rect::new(x - 5.0, mid - 5.0, 10.0, 10.0), 5.0, fill);
}

/// A device to switch to: a radio dot, its icon, its name and, for the one in use, its battery.
#[allow(clippy::too_many_arguments)]
fn device_row(scene: &mut Scene, theme: &Theme, fonts: &Fonts, y: f32, device: &Device, on: bool, battery: Option<u8>, output: bool) {
    let dot = Rect::new(MARGIN + PAD, y + (ROW - 16.0) / 2.0, 16.0, 16.0);
    if on {
        scene.fill(dot, 8.0, theme.primary);
        scene.fill(Rect::new(dot.x + 5.0, dot.y + 5.0, 6.0, 6.0), 3.0, theme.on_primary);
    } else {
        scene.fill(dot, 8.0, theme.subtext);
        scene.fill(Rect::new(dot.x + 1.5, dot.y + 1.5, 13.0, 13.0), 6.5, theme.base);
    }
    let icon = if output { kind_icon(device.form) } else { MICROPHONE };
    let color = if on { theme.text } else { theme.subtext };
    scene.text(Rect::new(dot.x + 24.0, y, 20.0, ROW), icon, Font::icons(14.0), color, Align::Center);
    let small = Font::new(theme.small_size, Weight::Medium);
    let battery_text = battery.map(|b| format!("{b}%"));
    let battery_w = battery_text.as_deref().map_or(0.0, |b| fonts.width(b, small).ceil() + 22.0);
    let name_x = dot.x + 52.0;
    let name_font = Font::new(theme.font_size, if on { Weight::SemiBold } else { Weight::Regular });
    scene.text(Rect::new(name_x, y, MARGIN + INNER - PAD - battery_w - name_x, ROW), device.name.as_str(), name_font, color, Align::Start);
    if let (Some(b), Some(text)) = (battery, battery_text) {
        let x = MARGIN + INNER - PAD - battery_w;
        let level = battery_color(theme, b, theme.subtext);
        scene.text(Rect::new(x, y, 18.0, ROW), battery_icon(b), Font::icons(13.0), level, Align::Center);
        scene.text(Rect::new(x + 20.0, y, battery_w - 20.0, ROW), text, small, level, Align::Start);
    }
}

/// The output's icon: its kind, or for speakers how loud, crossed out when muted.
fn output_icon(e: &Endpoint) -> &'static str {
    if e.muted {
        return MUTED;
    }
    match e.device.form {
        3 | 5 | 9 | 2 | 8 => kind_icon(e.device.form),
        _ => SPEAKER[match e.percent() {
            0 => 0,
            1..=33 => 1,
            34..=66 => 2,
            _ => 3,
        }],
    }
}

fn kind_icon(form: u32) -> &'static str {
    match form {
        3 => HEADPHONES,
        5 => HEADSET,
        9 => MONITOR,
        _ => SPEAKERS,
    }
}

/// Segoe Fluent Icons' batteries: a tenth each, from empty to full.
fn battery_icon(level: u8) -> &'static str {
    const LEVELS: [&str; 11] =
        ["\u{E850}", "\u{E851}", "\u{E852}", "\u{E853}", "\u{E854}", "\u{E855}", "\u{E856}", "\u{E857}", "\u{E858}", "\u{E859}", "\u{E83F}"];
    LEVELS[(level.min(100) as usize + 5) / 10]
}

/// Red at 10% and under, yellow at 20% and under.
fn battery_color(theme: &Theme, level: u8, normal: Rgba) -> Rgba {
    theme.level(100.0 - level as f64, 80.0, 90.0, normal)
}

fn device_id(device: &IMMDevice) -> Option<String> {
    unsafe {
        let id = device.GetId().ok()?;
        let s = id.to_string().ok();
        CoTaskMemFree(Some(id.0 as *const c_void));
        s
    }
}

/// A device's id, name, kind and whether it's Bluetooth, from its property store.
fn describe(device: &IMMDevice) -> Option<Device> {
    let id = device_id(device)?;
    let store = unsafe { device.OpenPropertyStore(STGM_READ) }.ok()?;
    let string = |key: &PROPERTYKEY| -> String {
        unsafe {
            let Ok(value) = store.GetValue(key) else { return String::new() };
            let Ok(text) = PropVariantToStringAlloc(&value) else { return String::new() };
            let s = text.to_string().unwrap_or_default();
            CoTaskMemFree(Some(text.0 as *const c_void));
            s
        }
    };
    let form = unsafe { store.GetValue(&PKEY_AudioEndpoint_FormFactor) }.ok().and_then(|v| u32_of(&v)).unwrap_or(0);
    let enumerator = string(&PKEY_Device_EnumeratorName);
    Some(Device { id, name: string(&PKEY_Device_FriendlyName), form, bluetooth: enumerator.to_uppercase().starts_with("BTH") })
}

fn u32_of(v: &PROPVARIANT) -> Option<u32> {
    unsafe {
        let inner = &v.Anonymous.Anonymous;
        (inner.vt == VT_UI4).then(|| inner.Anonymous.ulVal)
    }
}

// ---- the battery of a Bluetooth device -----------------------------------------------------------

fn locate(id: &str) -> Option<u32> {
    let mut node = 0u32;
    (unsafe { CM_Locate_DevNodeW(&mut node, &HSTRING::from(id), CM_LOCATE_DEVNODE_NORMAL) } == CR_SUCCESS).then_some(node)
}

fn property(node: u32, key: &DEVPROPKEY) -> Option<Vec<u8>> {
    let mut kind = DEVPROPTYPE(0);
    let mut size = 0u32;
    unsafe {
        let _ = CM_Get_DevNode_PropertyW(node, key, &mut kind, None, &mut size, 0);
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        (CM_Get_DevNode_PropertyW(node, key, &mut kind, Some(buf.as_mut_ptr()), &mut size, 0) == CR_SUCCESS).then_some(buf)
    }
}

/// The present device nodes of an enumerator (`BTHENUM\…`).
fn device_ids(enumerator: &str) -> Vec<String> {
    let filter = HSTRING::from(enumerator);
    let flags = CM_GETIDLIST_FILTER_ENUMERATOR | CM_GETIDLIST_FILTER_PRESENT;
    let mut len = 0u32;
    unsafe {
        if CM_Get_Device_ID_List_SizeW(&mut len, &filter, flags) != CR_SUCCESS || len == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        if CM_Get_Device_ID_ListW(&filter, &mut buf, flags) != CR_SUCCESS {
            return Vec::new();
        }
        buf.split(|c| *c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
    }
}

/// The battery of the Bluetooth device behind an audio endpoint: the endpoint's device node
/// (`SWD\MMDEVAPI\<id>`) is in the same container as the Bluetooth nodes of the device, one of
/// which has the level Windows shows in Settings.
fn bluetooth_battery(endpoint: &str) -> Option<u8> {
    let container = property(locate(&format!(r"SWD\MMDEVAPI\{endpoint}"))?, &DEVPKEY_Device_ContainerId)?;
    BLUETOOTH
        .iter()
        .flat_map(|e| device_ids(e))
        .filter_map(|id| locate(&id))
        .filter(|node| property(*node, &DEVPKEY_Device_ContainerId).as_ref() == Some(&container))
        .find_map(|node| property(node, &BATTERY))
        .and_then(|b| b.first().copied())
        .filter(|b| *b <= 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_icons_and_colors() {
        assert_eq!(battery_icon(0), "\u{E850}");
        assert_eq!(battery_icon(44), "\u{E854}");
        assert_eq!(battery_icon(100), "\u{E83F}");
        assert_eq!(battery_icon(250), "\u{E83F}");
    }
}
