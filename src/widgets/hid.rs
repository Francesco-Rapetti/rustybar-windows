//! The batteries behind a receiver or a cable, for `devices`: the mice, keyboards and controllers
//! that Windows doesn't show (Settings has only Bluetooth's). None of them has the standard HID
//! battery (usage page 0x06, usage 0x20): each maker has its own way, and all of what's sent here
//! are questions, never a setting:
//!
//! - Razer: a 90-byte feature report, as Synapse sends; `07 80` is the battery (0–255), `07 84`
//!   whether it charges. A HyperSpeed receiver without its device answers "timed out".
//! - Logitech: HID++ 2.0 through the receiver (Bolt, Unifying, Lightspeed): the root feature finds
//!   the unified battery (`0x1004`) or the older battery status (`0x1000`). The receiver knows the
//!   name and the kind of what's paired even while it sleeps.
//! - ASUS ROG: the 64-byte vendor reports of the SpeedNova and Omni receivers: `12 01` on the
//!   keyboard's channel (report 2) has its level, the two halves of a split one; `12 07` on the
//!   mouse's (report 3, or report 0 on a mouse's own receiver) as G-Helper reads it. `FF AA` is
//!   "nothing there".
//! - Valve: a Steam Controller (2026) sends its battery (input report 0x43) every ~3.5 s among
//!   ~260 reports a second; the input buffer is made big, the thread naps 1.5 s and takes what
//!   piled up in one go: two wake-ups instead of hundreds.
//! - Sony: DualSense and DualShock 4 have their level in every input report (as SDL reads it): one
//!   report is enough. Over Bluetooth only once something (Steam) asked for the full reports.
//! - Xbox: XInput's four levels.
//!
//! Everything runs on a worker thread (`pass`), with `hid.dll` and `xinput1_4.dll` loaded for that
//! pass alone: the bar doesn't load them otherwise. Only the collections of these makers are opened,
//! picked from their paths first: opening every HID collection costs ~330 ms of waits in drivers.

use std::ffi::c_void;
use std::time::{Duration, Instant};

use windows::Win32::Devices::DeviceAndDriverInstallation::{CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_Device_Interface_List_SizeW, CM_Get_Device_Interface_ListW, CM_Get_Device_Interface_PropertyW, CR_SUCCESS};
use windows::Win32::Devices::HumanInterfaceDevice::{HIDD_ATTRIBUTES, HIDP_BUTTON_CAPS, HIDP_CAPS, HIDP_REPORT_TYPE, HIDP_VALUE_CAPS, HidP_Output};
use windows::Win32::Devices::Properties::{DEVPKEY_Device_ContainerId, DEVPKEY_Device_InstanceId, DEVPROPTYPE};
use windows::Win32::Foundation::{CloseHandle, ERROR_IO_PENDING, FreeLibrary, GENERIC_READ, GENERIC_WRITE, HANDLE, HMODULE, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAG_OVERLAPPED, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile, WriteFile};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows::Win32::System::Threading::{CreateEventW, WaitForMultipleObjects, WaitForSingleObject};
use windows::core::{GUID, HSTRING, PCSTR, PCWSTR, s, w};

use super::audio::{locate, property};

/// What a device is, as the pill groups them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Mouse,
    Keyboard,
    Controller,
    Other,
}

impl Kind {
    pub fn icon(self) -> &'static str {
        match self {
            Kind::Mouse => "\u{E962}",
            Kind::Keyboard => "\u{E765}",
            Kind::Controller => "\u{E7FC}",
            Kind::Other => "\u{E702}",
        }
    }
}

/// How it reaches the PC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Bluetooth,
    Receiver,
    Cable,
    Unknown,
}

/// Who made it: a device seen on two links (Bluetooth and a receiver) is told apart by its maker
/// and kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Maker {
    Razer,
    Logitech,
    Asus,
    Valve,
    Sony,
    Microsoft,
    Other,
}

/// The maker of a USB vendor id, or of the one a Bluetooth device gives (Razer's are 0x068E there).
pub fn maker(vid: u16) -> Maker {
    match vid {
        0x1532 | 0x068E => Maker::Razer,
        0x046D => Maker::Logitech,
        0x0B05 => Maker::Asus,
        0x28DE => Maker::Valve,
        0x054C => Maker::Sony,
        0x045E => Maker::Microsoft,
        _ => Maker::Other,
    }
}

/// A device with a battery, as one pass found it.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    /// The same device in the next pass, to keep its last level while it sleeps.
    pub key: String,
    /// Empty when only its receiver is known (a ROG receiver): `devices` names it.
    pub name: String,
    pub kind: Kind,
    pub maker: Maker,
    pub link: Link,
    /// None while it isn't connected (asleep, off, its receiver alone).
    pub level: Option<u8>,
    /// The other half of a split keyboard.
    pub second: Option<u8>,
    pub charging: bool,
    /// One of four levels (XInput), not a percentage.
    pub approx: bool,
}

impl Found {
    fn new(key: String, name: String, kind: Kind, maker: Maker, link: Link) -> Found {
        Found { key, name, kind, maker, link, level: None, second: None, charging: false, approx: false }
    }
}

/// A HID collection of one of the makers above, as the discovery found it (no handle is kept).
#[derive(Clone, Debug)]
pub struct Collection {
    path: String,
    lower: String,
    vid: u16,
    pid: u16,
    page: u16,
    usage: u16,
    input: usize,
    output: usize,
    feature: usize,
    /// The report ID of its output reports, when it has one.
    out_id: u8,
    product: String,
    /// The device container: the collections of one USB device share it.
    container: Vec<u8>,
}

/// The collections found last, kept between passes until devices change.
pub type Known = Option<Vec<Collection>>;

/// The paths of the makers this module reads, picked before opening anything.
const MAKERS: [&str; 6] = ["vid_1532", "vid_046d", "vid_0b05", "vid_28de", "vid_054c", "&0002054c"];

/// HID's interface class (`HidD_GetHidGuid`).
const HID_GUID: GUID = GUID::from_u128(0x4d1e55b2_f16f_11cf_88cb_001111000030);

/// One pass: the collections found again if asked (or never found), then every battery read. On
/// the worker thread: it waits on devices for up to a few seconds.
pub fn pass(known: &mut Known, rediscover: bool) -> Vec<Found> {
    let mut found = Vec::new();
    if let Some(api) = Api::load() {
        if rediscover || known.is_none() {
            *known = Some(discover(&api));
        }
        let colls = known.as_deref().unwrap_or_default();
        razer(&api, colls, &mut found);
        logitech(colls, &mut found);
        asus(colls, &mut found);
        sony(colls, &mut found);
        valve(&api, colls, &mut found);
    }
    xinput(&mut found);
    merge(found)
}

/// One device per key: the one that's connected, charging first (the cable of a device that has a
/// receiver too).
pub fn merge(found: Vec<Found>) -> Vec<Found> {
    let mut out: Vec<Found> = Vec::new();
    for f in found {
        match out.iter_mut().find(|o| o.key == f.key) {
            Some(o) => {
                let better = |x: &Found| (x.level.is_some(), x.charging);
                if better(&f) > better(o) {
                    *o = f;
                }
            }
            None => out.push(f),
        }
    }
    out
}

// ---- hid.dll, loaded for a pass ----------------------------------------------------------------

type Attributes = unsafe extern "system" fn(HANDLE, *mut HIDD_ATTRIBUTES) -> u8;
type Preparsed = unsafe extern "system" fn(HANDLE, *mut isize) -> u8;
type FreePreparsed = unsafe extern "system" fn(isize) -> u8;
type Caps = unsafe extern "system" fn(isize, *mut HIDP_CAPS) -> i32;
type ValueCaps = unsafe extern "system" fn(HIDP_REPORT_TYPE, *mut HIDP_VALUE_CAPS, *mut u16, isize) -> i32;
type ButtonCaps = unsafe extern "system" fn(HIDP_REPORT_TYPE, *mut HIDP_BUTTON_CAPS, *mut u16, isize) -> i32;
type GetFeature = unsafe extern "system" fn(HANDLE, *mut c_void, u32) -> u8;
type SetFeature = unsafe extern "system" fn(HANDLE, *const c_void, u32) -> u8;
type Product = unsafe extern "system" fn(HANDLE, *mut c_void, u32) -> u8;
type Buffers = unsafe extern "system" fn(HANDLE, u32) -> u8;

struct Api {
    lib: HMODULE,
    attributes: Attributes,
    preparsed: Preparsed,
    free_preparsed: FreePreparsed,
    caps: Caps,
    value_caps: ValueCaps,
    button_caps: ButtonCaps,
    get_feature: GetFeature,
    set_feature: SetFeature,
    product: Product,
    buffers: Buffers,
}

impl Api {
    fn load() -> Option<Api> {
        unsafe {
            let lib = LoadLibraryExW(w!("hid.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
            let api = (|| {
                Some(Api {
                    lib,
                    attributes: function(lib, s!("HidD_GetAttributes"))?,
                    preparsed: function(lib, s!("HidD_GetPreparsedData"))?,
                    free_preparsed: function(lib, s!("HidD_FreePreparsedData"))?,
                    caps: function(lib, s!("HidP_GetCaps"))?,
                    value_caps: function(lib, s!("HidP_GetValueCaps"))?,
                    button_caps: function(lib, s!("HidP_GetButtonCaps"))?,
                    get_feature: function(lib, s!("HidD_GetFeature"))?,
                    set_feature: function(lib, s!("HidD_SetFeature"))?,
                    product: function(lib, s!("HidD_GetProductString"))?,
                    buffers: function(lib, s!("HidD_SetNumInputBuffers"))?,
                })
            })();
            if api.is_none() {
                let _ = FreeLibrary(lib);
            }
            api
        }
    }
}

/// A function of a library loaded here, as the type of function `T` it is.
///
/// # Safety
/// `T` must be the function's real signature (a function pointer of the same size).
unsafe fn function<T: Copy>(lib: HMODULE, name: PCSTR) -> Option<T> {
    let f = unsafe { GetProcAddress(lib, name) }?;
    debug_assert_eq!(size_of::<T>(), size_of_val(&f));
    Some(unsafe { std::mem::transmute_copy::<unsafe extern "system" fn() -> isize, T>(&f) })
}

impl Drop for Api {
    fn drop(&mut self) {
        unsafe {
            let _ = FreeLibrary(self.lib);
        }
    }
}

// ---- finding the collections --------------------------------------------------------------------

fn interfaces() -> Vec<String> {
    let mut len = 0u32;
    unsafe {
        if CM_Get_Device_Interface_List_SizeW(&mut len, &HID_GUID, PCWSTR::null(), CM_GET_DEVICE_INTERFACE_LIST_PRESENT) != CR_SUCCESS || len == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        if CM_Get_Device_Interface_ListW(&HID_GUID, PCWSTR::null(), &mut buf, CM_GET_DEVICE_INTERFACE_LIST_PRESENT) != CR_SUCCESS {
            return Vec::new();
        }
        buf.split(|c| *c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
    }
}

/// The container of the device behind an interface.
fn container(path: &str) -> Vec<u8> {
    let mut kind = DEVPROPTYPE(0);
    let mut buf = [0u8; 512];
    let mut size = buf.len() as u32;
    let ok = unsafe { CM_Get_Device_Interface_PropertyW(&HSTRING::from(path), &DEVPKEY_Device_InstanceId, &mut kind, Some(buf.as_mut_ptr()), &mut size, 0) } == CR_SUCCESS;
    if !ok {
        return Vec::new();
    }
    let wide: Vec<u16> = buf[..size as usize].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).take_while(|c| *c != 0).collect();
    locate(&String::from_utf16_lossy(&wide)).and_then(|node| property(node, &DEVPKEY_Device_ContainerId)).unwrap_or_default()
}

fn discover(api: &Api) -> Vec<Collection> {
    let started = Instant::now();
    let colls: Vec<Collection> = interfaces()
        .into_iter()
        .filter(|p| {
            let p = p.to_lowercase();
            MAKERS.iter().any(|m| p.contains(m))
        })
        .filter_map(|p| describe(api, p))
        .collect();
    crate::log!("devices: {} HID collections of the known makers in {} ms", colls.len(), started.elapsed().as_millis());
    colls
}

fn describe(api: &Api, path: String) -> Option<Collection> {
    let h = Handle::open(&path, 0, false)?;
    unsafe {
        let mut attr = HIDD_ATTRIBUTES { Size: size_of::<HIDD_ATTRIBUTES>() as u32, ..Default::default() };
        if (api.attributes)(h.0, &mut attr) == 0 {
            return None;
        }
        let mut pp = 0isize;
        if (api.preparsed)(h.0, &mut pp) == 0 {
            return None;
        }
        let mut caps = HIDP_CAPS::default();
        let ok = (api.caps)(pp, &mut caps) >= 0;
        let out_id = output_id(api, pp, &caps);
        let _ = (api.free_preparsed)(pp);
        if !ok {
            return None;
        }
        let mut name = [0u16; 128];
        let product = if (api.product)(h.0, name.as_mut_ptr() as _, 256) != 0 { String::from_utf16_lossy(&name[..name.iter().position(|c| *c == 0).unwrap_or(0)]).trim().to_string() } else { String::new() };
        drop(h);
        Some(Collection {
            lower: path.to_lowercase(),
            container: container(&path),
            path,
            vid: attr.VendorID,
            pid: attr.ProductID,
            page: caps.UsagePage,
            usage: caps.Usage,
            input: caps.InputReportByteLength as usize,
            output: caps.OutputReportByteLength as usize,
            feature: caps.FeatureReportByteLength as usize,
            out_id,
            product,
        })
    }
}

/// The report ID of a collection's output reports (0 without IDs).
fn output_id(api: &Api, pp: isize, caps: &HIDP_CAPS) -> u8 {
    unsafe {
        if caps.NumberOutputValueCaps > 0 {
            let mut v = vec![HIDP_VALUE_CAPS::default(); caps.NumberOutputValueCaps as usize];
            let mut n = caps.NumberOutputValueCaps;
            if (api.value_caps)(HidP_Output, v.as_mut_ptr(), &mut n, pp) >= 0 && n > 0 {
                return v[0].ReportID;
            }
        }
        if caps.NumberOutputButtonCaps > 0 {
            let mut b = vec![HIDP_BUTTON_CAPS::default(); caps.NumberOutputButtonCaps as usize];
            let mut n = caps.NumberOutputButtonCaps;
            if (api.button_caps)(HidP_Output, b.as_mut_ptr(), &mut n, pp) >= 0 && n > 0 {
                return b[0].ReportID;
            }
        }
    }
    0
}

/// A collection's handle, closed when dropped.
struct Handle(HANDLE);

impl Handle {
    /// With no access a collection still answers its attributes and feature reports (the system
    /// holds mice and keyboards for itself); reading and writing need the access.
    fn open(path: &str, access: u32, overlapped: bool) -> Option<Handle> {
        let flags = if overlapped { FILE_FLAG_OVERLAPPED } else { FILE_FLAGS_AND_ATTRIBUTES(0) };
        unsafe { CreateFileW(&HSTRING::from(path), access, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, flags, None) }.ok().map(Handle)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// A collection opened for reading (and writing) input and output reports, with one read waiting
/// at a time: its buffer and its `OVERLAPPED` are boxed, so they stay put while Windows fills them.
struct Port {
    handle: Handle,
    event: HANDLE,
    ov: Box<OVERLAPPED>,
    buf: Box<[u8]>,
    pending: bool,
}

impl Port {
    fn open(path: &str, len: usize, write: bool) -> Option<Port> {
        let access = if write { (GENERIC_READ | GENERIC_WRITE).0 } else { GENERIC_READ.0 };
        let handle = Handle::open(path, access, true)?;
        let event = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.ok()?;
        Some(Port { handle, event, ov: Box::new(OVERLAPPED { hEvent: event, ..Default::default() }), buf: vec![0u8; len.max(1)].into_boxed_slice(), pending: false })
    }

    /// Starts a read if none waits; the report if one was there already.
    fn arm(&mut self) -> Option<Vec<u8>> {
        if self.pending {
            return None;
        }
        *self.ov = OVERLAPPED { hEvent: self.event, ..Default::default() };
        let mut got = 0u32;
        match unsafe { ReadFile(self.handle.0, Some(&mut self.buf), None, Some(&mut *self.ov)) } {
            Ok(()) => {
                let _ = unsafe { GetOverlappedResult(self.handle.0, &*self.ov, &mut got, false) };
                Some(self.buf[..got as usize].to_vec())
            }
            Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {
                self.pending = true;
                None
            }
            Err(_) => None,
        }
    }

    /// The report of the read that finished (its event is set).
    fn take(&mut self) -> Option<Vec<u8>> {
        if !self.pending {
            return None;
        }
        let mut got = 0u32;
        let done = unsafe { GetOverlappedResult(self.handle.0, &*self.ov, &mut got, false) }.is_ok();
        if done {
            self.pending = false;
            return Some(self.buf[..got as usize].to_vec());
        }
        None
    }

    /// One report, waiting up to `ms`.
    fn read(&mut self, ms: u32) -> Option<Vec<u8>> {
        if let Some(r) = self.arm() {
            return Some(r);
        }
        if !self.pending || unsafe { WaitForSingleObject(self.event, ms) } != WAIT_OBJECT_0 {
            return None;
        }
        self.take()
    }

    /// Sends an output report, waiting up to half a second.
    fn write(&self, data: &[u8]) -> bool {
        let Ok(event) = (unsafe { CreateEventW(None, true, false, PCWSTR::null()) }) else { return false };
        let mut ov = OVERLAPPED { hEvent: event, ..Default::default() };
        let mut n = 0u32;
        let ok = match unsafe { WriteFile(self.handle.0, Some(data), None, Some(&mut ov)) } {
            Ok(()) => true,
            Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => unsafe { WaitForSingleObject(event, 500) == WAIT_OBJECT_0 && GetOverlappedResult(self.handle.0, &ov, &mut n, false).is_ok() },
            Err(_) => false,
        };
        unsafe {
            if !ok {
                let _ = CancelIoEx(self.handle.0, Some(&ov));
                let _ = GetOverlappedResult(self.handle.0, &ov, &mut n, true);
            }
            let _ = CloseHandle(event);
        }
        ok
    }
}

impl Drop for Port {
    fn drop(&mut self) {
        unsafe {
            if self.pending {
                let mut got = 0u32;
                let _ = CancelIoEx(self.handle.0, Some(&*self.ov));
                let _ = GetOverlappedResult(self.handle.0, &*self.ov, &mut got, true);
            }
            let _ = CloseHandle(self.event);
        }
    }
}

/// The collections of each device (container), with what picks them.
fn devices(colls: &[Collection], pick: impl Fn(&Collection) -> bool) -> Vec<Vec<&Collection>> {
    let mut groups: Vec<Vec<&Collection>> = Vec::new();
    for c in colls.iter().filter(|c| pick(c)) {
        match groups.iter_mut().find(|g| g[0].container == c.container && g[0].vid == c.vid && g[0].pid == c.pid) {
            Some(g) => g.push(c),
            None => groups.push(vec![c]),
        }
    }
    groups
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---- Razer --------------------------------------------------------------------------------------

/// Razer's statuses: the command went through, the device didn't answer (a receiver alone), it's
/// still working on it.
const RAZER_OK: u8 = 0x02;
const RAZER_TIMEOUT: u8 = 0x04;
const RAZER_BUSY: u8 = 0x01;

/// A Razer command with no arguments: report ID 0, status 0 (new), the transaction, the size of the
/// answer, class and id, then a CRC (the XOR of bytes 3–88 of the report).
pub(super) fn razer_request(transaction: u8, class: u8, id: u8) -> [u8; 91] {
    let mut r = [0u8; 91];
    r[2] = transaction;
    r[6] = 0x02;
    r[7] = class;
    r[8] = id;
    r[89] = r[3..89].iter().fold(0, |a, b| a ^ b);
    r
}

/// The status of an answer to `class`/`id`, and its second argument (the level, or 1 if charging).
pub(super) fn razer_answer(r: &[u8], class: u8, id: u8) -> Option<(u8, u8)> {
    (r.len() >= 11 && r[7] == class && r[8] == id).then(|| (r[1], r[10]))
}

fn razer_ask(api: &Api, h: &Handle, transaction: u8, class: u8, id: u8) -> Option<(u8, u8)> {
    let req = razer_request(transaction, class, id);
    if unsafe { (api.set_feature)(h.0, req.as_ptr() as _, 91) } == 0 {
        return None;
    }
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(5));
        let mut r = [0u8; 91];
        if unsafe { (api.get_feature)(h.0, r.as_mut_ptr() as _, 91) } == 0 {
            return None;
        }
        // Another program's answer (Synapse asks too) doesn't count
        match razer_answer(&r, class, id) {
            Some((status, _)) if status == RAZER_BUSY || status == 0 => continue,
            other => return other,
        }
    }
    None
}

/// What a Razer product id is: mice under 0x0100, keyboards up to 0x04ff; the rest (headsets,
/// docks, mats, lights, cases) isn't read.
fn razer_kind(pid: u16) -> Option<Kind> {
    match pid {
        0x0001..=0x00ff => Some(Kind::Mouse),
        0x0100..=0x04ff => Some(Kind::Keyboard),
        _ => None,
    }
}

fn razer(api: &Api, colls: &[Collection], found: &mut Vec<Found>) {
    for device in devices(colls, |c| c.vid == 0x1532 && c.feature == 91) {
        let Some(kind) = razer_kind(device[0].pid) else { continue };
        for c in device {
            let Some(h) = Handle::open(&c.path, 0, false) else { continue };
            // Most answer to 0x1F; older ones to 0x3F or 0xFF
            let answer = [0x1F, 0x3F, 0xFF].into_iter().find_map(|t| razer_ask(api, &h, t, 0x07, 0x80).filter(|(s, _)| *s == RAZER_OK || *s == RAZER_TIMEOUT).map(|a| (t, a)));
            let Some((transaction, (status, level))) = answer else { continue };
            let name = if c.product.is_empty() { "Razer".to_string() } else { c.product.clone() };
            let mut f = Found::new(format!("razer:{}", name.to_lowercase()), name, kind, Maker::Razer, Link::Unknown);
            if status == RAZER_OK {
                f.level = Some(((level as u32 * 100 + 127) / 255) as u8);
                f.charging = razer_ask(api, &h, transaction, 0x07, 0x84).is_some_and(|(s, c)| s == RAZER_OK && c == 1);
                if f.charging {
                    f.link = Link::Cable;
                }
            }
            found.push(f);
            break;
        }
    }
}

// ---- Logitech -----------------------------------------------------------------------------------

/// Our software id in HID++ 2.0 requests (the low nibble of the function byte): answers to another
/// program (Options+, G Hub) carry theirs.
const SWID: u8 = 0x0A;

/// The short and the long HID++ collections of a receiver, read both at once.
struct Hidpp {
    short: Port,
    long: Port,
}

enum Answer {
    Ok(Vec<u8>),
    /// HID++ 1.0's error (from the receiver: the device didn't connect, nothing paired there), or
    /// 2.0's.
    Error,
    Timeout,
}

impl Hidpp {
    /// Waits for the answer to `matches` (or an error about `sub`/`address`) up to `ms`.
    fn wait(&mut self, device: u8, sub: u8, address: u8, ms: u64, matches: impl Fn(&[u8]) -> bool) -> Answer {
        let deadline = Instant::now() + Duration::from_millis(ms);
        loop {
            for port in [&mut self.long, &mut self.short] {
                if let Some(r) = port.arm()
                    && let Some(a) = Self::check(&r, device, sub, address, &matches)
                {
                    return a;
                }
            }
            let left = deadline.saturating_duration_since(Instant::now()).as_millis() as u32;
            // A read that couldn't start (the device went) has nothing to wait for
            let waiting: Vec<(HANDLE, bool)> = [(self.long.event, self.long.pending, true), (self.short.event, self.short.pending, false)].into_iter().filter(|(_, p, _)| *p).map(|(e, _, long)| (e, long)).collect();
            if left == 0 || waiting.is_empty() {
                return Answer::Timeout;
            }
            let events: Vec<HANDLE> = waiting.iter().map(|(e, _)| *e).collect();
            let woke = unsafe { WaitForMultipleObjects(&events, false, left) };
            let Some((_, long)) = waiting.get(woke.0.wrapping_sub(WAIT_OBJECT_0.0) as usize) else { return Answer::Timeout };
            let port = if *long { &mut self.long } else { &mut self.short };
            if let Some(r) = port.take()
                && let Some(a) = Self::check(&r, device, sub, address, &matches)
            {
                return a;
            }
        }
    }

    fn check(r: &[u8], device: u8, sub: u8, address: u8, matches: &impl Fn(&[u8]) -> bool) -> Option<Answer> {
        if r.len() < 6 || r[1] != device {
            return None;
        }
        if (r[2] == 0x8F || r[2] == 0xFF) && r[3] == sub && r[4] == address {
            return Some(Answer::Error);
        }
        matches(r).then(|| Answer::Ok(r[4..].to_vec()))
    }

    /// A HID++ 2.0 call: function `func` of the feature at `index`.
    fn call(&mut self, device: u8, index: u8, func: u8, params: &[u8]) -> Answer {
        let mut req = [0u8; 20];
        req[0] = 0x11;
        req[1] = device;
        req[2] = index;
        req[3] = (func << 4) | SWID;
        req[4..4 + params.len()].copy_from_slice(params);
        if !self.long.write(&req) {
            return Answer::Timeout;
        }
        self.wait(device, index, req[3], 400, |r| r[0] == 0x11 && r[2] == index && r[3] == req[3])
    }

    /// The index of a feature, if the device has it (`IRoot.getFeature`).
    fn feature(&mut self, device: u8, id: u16) -> Result<Option<u8>, Answer> {
        match self.call(device, 0x00, 0x00, &[(id >> 8) as u8, id as u8]) {
            Answer::Ok(p) => Ok((p[0] != 0).then_some(p[0])),
            other => Err(other),
        }
    }

    /// A long register of the receiver (`0x83`): 16 bytes.
    fn register(&mut self, address: u8, p0: u8, p1: u8) -> Option<Vec<u8>> {
        let req = [0x10u8, 0xFF, 0x83, address, p0, p1, 0];
        if !self.short.write(&req) {
            return None;
        }
        match self.wait(0xFF, 0x83, address, 300, |r| r[0] == 0x11 && r[2] == 0x83 && r[3] == address && r[4] == p0) {
            Answer::Ok(d) => Some(d),
            _ => None,
        }
    }
}

/// HID++ 1.0's kinds of paired devices (the receiver's pairing information).
pub(super) fn logitech_kind(kind: u8) -> Kind {
    match kind & 0x0F {
        1 | 3 => Kind::Keyboard,
        2 | 8 | 9 => Kind::Mouse,
        11 | 12 => Kind::Controller,
        _ => Kind::Other,
    }
}

/// The name and kind of a receiver's slot from its registers: Bolt keeps them at 0x50 + n and
/// 0x60 + n (with a 1), Unifying and Lightspeed at 0x20 + n and 0x40 + n.
fn logitech_pairing(p: &mut Hidpp, bolt: bool, slot: u8) -> Option<(String, Kind)> {
    if bolt {
        let info = p.register(0xB5, 0x50 + slot, 0)?;
        let name = p.register(0xB5, 0x60 + slot, 1)?;
        let len = (name[2] as usize).min(name.len() - 3);
        Some((String::from_utf8_lossy(&name[3..3 + len]).trim().to_string(), logitech_kind(info[1])))
    } else {
        let info = p.register(0xB5, 0x20 + slot, 0)?;
        let name = p.register(0xB5, 0x40 + slot, 0)?;
        let len = (name[1] as usize).min(name.len() - 2);
        Some((String::from_utf8_lossy(&name[2..2 + len]).trim().to_string(), logitech_kind(info[7])))
    }
}

/// The level and whether it charges, from the unified battery's `getStatus` or the older
/// `getBatteryLevelStatus`.
pub(super) fn logitech_level(unified: bool, p: &[u8]) -> (u8, bool) {
    if unified {
        (p[0].min(100), matches!(p[2], 1..=3))
    } else {
        (p[0].min(100), matches!(p[2], 1 | 2 | 4))
    }
}

fn logitech(colls: &[Collection], found: &mut Vec<Found>) {
    for device in devices(colls, |c| c.vid == 0x046D && c.page == 0xFF00) {
        let (Some(short), Some(long)) = (device.iter().find(|c| c.usage == 1 && c.input >= 7), device.iter().find(|c| c.usage == 2 && c.input >= 20)) else { continue };
        let (Some(s), Some(l)) = (Port::open(&short.path, short.input, true), Port::open(&long.path, long.input, true)) else { continue };
        let mut p = Hidpp { short: s, long: l };
        let receiver = short.product.to_lowercase().contains("receiver");
        let bolt = short.pid == 0xC548;
        let slots: Vec<u8> = if receiver { (1..=6).collect() } else { vec![0xFF] };
        for slot in slots {
            let unified = p.feature(slot, 0x1004);
            // A device that isn't a receiver's and doesn't answer isn't one
            if !receiver && unified.is_err() {
                continue;
            }
            // The receiver knows the name and the kind of what's paired even while it sleeps (an
            // empty slot has none)
            let (name, kind) = if receiver { logitech_pairing(&mut p, bolt, slot).unwrap_or((String::new(), Kind::Other)) } else { (short.product.clone(), Kind::Mouse) };
            let level = match unified {
                Ok(Some(i)) => match p.call(slot, i, 1, &[]) {
                    Answer::Ok(r) => Some(logitech_level(true, &r)),
                    _ => None,
                },
                Ok(None) => match p.feature(slot, 0x1000) {
                    Ok(Some(i)) => match p.call(slot, i, 0, &[]) {
                        Answer::Ok(r) => Some(logitech_level(false, &r)),
                        _ => None,
                    },
                    _ => None,
                },
                Err(_) => None,
            };
            // A name is needed: one only the device would say, if it's awake
            let (name, kind) = if name.is_empty() && level.is_some() { device_name(&mut p, slot).unwrap_or(("Logitech".to_string(), Kind::Other)) } else { (name, kind) };
            if name.is_empty() {
                continue;
            }
            let key = format!("logitech:{}:{slot}:{}", hex(&short.container), name.to_lowercase());
            let mut f = Found::new(key, name, kind, Maker::Logitech, if receiver { Link::Receiver } else { Link::Cable });
            if let Some((l, charging)) = level {
                f.level = Some(l);
                f.charging = charging;
            }
            found.push(f);
        }
    }
}

/// The name and the kind a HID++ 2.0 device gives (`0x0005`), for one that's no receiver's.
fn device_name(p: &mut Hidpp, slot: u8) -> Option<(String, Kind)> {
    let i = p.feature(slot, 0x0005).ok()??;
    let Answer::Ok(count) = p.call(slot, i, 0, &[]) else { return None };
    let count = count[0] as usize;
    let mut name = Vec::new();
    while name.len() < count {
        let Answer::Ok(part) = p.call(slot, i, 1, &[name.len() as u8]) else { break };
        let take = (count - name.len()).min(part.len());
        if take == 0 {
            break;
        }
        name.extend_from_slice(&part[..take]);
    }
    let kind = match p.call(slot, i, 2, &[]) {
        Answer::Ok(t) => match t[0] {
            0 | 2 => Kind::Keyboard,
            3..=5 => Kind::Mouse,
            11 | 12 => Kind::Controller,
            _ => Kind::Other,
        },
        _ => Kind::Other,
    };
    Some((String::from_utf8_lossy(&name).trim().to_string(), kind))
}

// ---- ASUS ROG -----------------------------------------------------------------------------------

/// The level of a ROG keyboard from its receiver's answer to `12 01`, and the other half's when it
/// has two (byte 7 says 2): `None` if it doesn't answer that (`FF AA`). A level of 0 is a keyboard
/// that sleeps or is off. Read from a ROG Falcata (both halves at 53%).
pub(super) fn asus_keyboard(r: &[u8]) -> Option<(u8, Option<u8>)> {
    if r.len() < 12 || r[1] != 0x12 || r[2] != 0x01 {
        return None;
    }
    let second = (r[7] == 2 && (1..=100).contains(&r[11])).then_some(r[11]);
    Some((r[6].min(100), second))
}

/// The level of a ROG mouse and whether it charges, from the answer to `12 07` (as G-Helper reads
/// it); a level of 0 without charging is a mouse that sleeps.
pub(super) fn asus_mouse(r: &[u8]) -> Option<(u8, bool)> {
    (r.len() >= 11 && r[1] == 0x12 && r[2] == 0x07).then(|| (r[5].min(100), r[10] > 0))
}

fn asus_ask(c: &Collection, sub: u8) -> Option<Vec<u8>> {
    let mut port = Port::open(&c.path, c.input, true)?;
    let mut req = vec![0u8; c.output];
    req[0] = c.out_id;
    req[1] = 0x12;
    req[2] = sub;
    if !port.write(&req) {
        return None;
    }
    let deadline = Instant::now() + Duration::from_millis(250);
    while Instant::now() < deadline {
        let r = port.read(50)?;
        if r.len() > 2 && (r[1] == 0x12 && r[2] == sub || r[1] == 0xFF && r[2] == 0xAA) {
            return Some(r);
        }
    }
    None
}

fn asus(colls: &[Collection], found: &mut Vec<Found>) {
    for device in devices(colls, |c| c.vid == 0x0B05) {
        // A ROG keyboard, mouse or their receiver: never the board's lights or anything else of ASUS
        if !device.iter().any(|c| c.page == 1 && (c.usage == 2 || c.usage == 6)) {
            continue;
        }
        let receiver = device.iter().any(|c| c.product.to_lowercase().contains("receiver"));
        let name = if receiver { String::new() } else { device[0].product.clone() };
        for c in device.iter().filter(|c| c.page >= 0xFF00 && c.input >= 64 && c.output >= 64) {
            let id = hex(&c.container);
            if c.out_id == 2 {
                let Some((level, second)) = asus_ask(c, 0x01).as_deref().and_then(asus_keyboard) else { continue };
                let mut f = Found::new(format!("asus:{id}:keyboard"), name.clone(), Kind::Keyboard, Maker::Asus, if receiver { Link::Receiver } else { Link::Unknown });
                if level > 0 {
                    f.level = Some(level);
                    f.second = second;
                }
                found.push(f);
            } else if c.out_id == 3 || c.out_id == 0 && c.output == 65 {
                let Some((level, charging)) = asus_ask(c, 0x07).as_deref().and_then(asus_mouse) else { continue };
                let mut f = Found::new(format!("asus:{id}:mouse"), name.clone(), Kind::Mouse, Maker::Asus, if receiver { Link::Receiver } else { Link::Unknown });
                if level > 0 || charging {
                    f.level = Some(level);
                    f.charging = charging;
                }
                found.push(f);
            }
        }
    }
}

// ---- Sony ---------------------------------------------------------------------------------------

/// A DualSense's level and charging from one input report: USB's 0x01 has it at byte 53, the full
/// Bluetooth report 0x31 at 54 (the short 0x01 of Bluetooth hasn't). The low nibble is tenths, the
/// high one 0 on battery, 1 charging, 2 full.
pub(super) fn dualsense(r: &[u8], bluetooth: bool) -> Option<(u8, bool)> {
    let b = match r.first()? {
        0x01 if !bluetooth => *r.get(53)?,
        0x31 if bluetooth => *r.get(54)?,
        _ => return None,
    };
    let level = ((b & 0x0F) as u32 * 10 + 5).min(100) as u8;
    match b >> 4 {
        0 => Some((level, false)),
        1 => Some((level, true)),
        2 => Some((100, true)),
        _ => None,
    }
}

/// A DualShock 4's: USB's 0x01 at byte 30, Bluetooth's full 0x11 at 32; tenths in the low nibble
/// (up to 11 on the cable), 0x10 while the cable is in.
pub(super) fn dualshock(r: &[u8], bluetooth: bool) -> Option<(u8, bool)> {
    let b = match r.first()? {
        0x01 if !bluetooth => *r.get(30)?,
        0x11 if bluetooth => *r.get(32)?,
        _ => return None,
    };
    let tenths = (b & 0x0F) as u32;
    let cable = b & 0x10 != 0;
    if cable && tenths > 10 {
        return Some((100, true));
    }
    Some(((tenths * 10 + 5).min(100) as u8, cable))
}

/// What reads a controller's level from one of its input reports (over Bluetooth or not).
type Reader = fn(&[u8], bool) -> Option<(u8, bool)>;

fn sony(colls: &[Collection], found: &mut Vec<Found>) {
    for c in colls.iter().filter(|c| c.vid == 0x054C && c.page == 1 && c.usage == 5) {
        let bluetooth = c.lower.contains("{00001124") || c.lower.contains("&0002054c");
        let (name, read): (&str, Reader) = match c.pid {
            0x0CE6 => ("DualSense", dualsense),
            0x0DF2 => ("DualSense Edge", dualsense),
            0x05C4 | 0x09CC => ("DUALSHOCK 4", dualshock),
            _ => continue,
        };
        let Some(mut port) = Port::open(&c.path, c.input, false) else { continue };
        let Some((level, charging)) = port.read(250).and_then(|r| read(&r, bluetooth)) else { continue };
        let mut f = Found::new(format!("sony:{}", hex(&c.container)), name.to_string(), Kind::Controller, Maker::Sony, if bluetooth { Link::Bluetooth } else { Link::Cable });
        f.level = Some(level);
        f.charging = charging;
        found.push(f);
    }
}

// ---- Valve --------------------------------------------------------------------------------------

/// A Steam Controller's battery report (0x43): the charge state (1 on battery, 2 charging, 4
/// charged) and the level.
pub(super) fn steam_battery(r: &[u8]) -> Option<(u8, bool)> {
    (r.len() >= 15 && r[0] == 0x43).then(|| (r[2].min(100), matches!(r[1], 2 | 4)))
}

fn valve(api: &Api, colls: &[Collection], found: &mut Vec<Found>) {
    // Wired, Bluetooth and the puck (one interface a controller): their vendor collections
    let slots: Vec<&Collection> = colls.iter().filter(|c| c.vid == 0x28DE && matches!(c.pid, 0x1302..=0x1304) && c.page == 0xFF00 && c.usage == 1 && c.input >= 15).collect();
    let mut ports: Vec<(&Collection, Port)> = slots.into_iter().filter_map(|c| Port::open(&c.path, c.input, false).map(|p| (c, p))).collect();
    for (_, port) in &ports {
        // Room for some 2 s of reports, so a nap keeps them
        unsafe {
            let _ = (api.buffers)(port.handle.0, 512);
        }
    }
    let mut levels: Vec<Option<(u8, bool)>> = vec![None; ports.len()];
    let mut waiting: Vec<bool> = vec![true; ports.len()];
    for _ in 0..3 {
        if !waiting.iter().any(|w| *w) {
            break;
        }
        std::thread::sleep(Duration::from_millis(1500));
        for (i, (_, port)) in ports.iter_mut().enumerate() {
            if !waiting[i] {
                continue;
            }
            let mut any = false;
            while let Some(r) = port.read(0) {
                any = true;
                if let Some(level) = steam_battery(&r) {
                    levels[i] = Some(level);
                    waiting[i] = false;
                    break;
                }
            }
            // Nothing came: no controller on that slot
            if !any {
                waiting[i] = false;
            }
        }
    }
    let total = levels.iter().filter(|l| l.is_some()).count();
    let mut n = 0;
    for ((c, _), level) in ports.iter().zip(levels) {
        let Some((level, charging)) = level else { continue };
        n += 1;
        let name = if total > 1 { format!("Steam Controller {n}") } else { "Steam Controller".to_string() };
        let link = match c.pid {
            0x1302 => Link::Cable,
            0x1303 => Link::Bluetooth,
            _ => Link::Receiver,
        };
        let mut f = Found::new(format!("valve:{}:{n}", hex(&c.container)), name, Kind::Controller, Maker::Valve, link);
        f.level = Some(level);
        f.charging = charging;
        found.push(f);
    }
}

// ---- Xbox ---------------------------------------------------------------------------------------

/// XInput's levels (empty, low, medium, full) as a battery to draw.
pub(super) fn xinput_level(level: u8) -> u8 {
    [5, 30, 60, 100][level.min(3) as usize]
}

fn xinput(found: &mut Vec<Found>) {
    // DWORD XInputGetBatteryInformation(DWORD user, BYTE devType, XINPUT_BATTERY_INFORMATION *info)
    type Battery = unsafe extern "system" fn(u32, u8, *mut [u8; 2]) -> u32;
    unsafe {
        let Ok(lib) = LoadLibraryExW(w!("xinput1_4.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) else { return };
        if let Some(get) = GetProcAddress(lib, s!("XInputGetBatteryInformation")) {
            let get: Battery = std::mem::transmute(get);
            let mut pads = Vec::new();
            for user in 0..4u32 {
                // [type, level]: 0 disconnected, 1 wired (no battery), then the kinds of batteries
                let mut info = [0u8; 2];
                if get(user, 0, &mut info) == 0 && info[0] > 1 {
                    pads.push(info[1]);
                }
            }
            let total = pads.len();
            for (i, level) in pads.into_iter().enumerate() {
                let name = if total > 1 { format!("{} {}", crate::i18n::t("Controller Xbox", "Xbox controller"), i + 1) } else { crate::i18n::t("Controller Xbox", "Xbox controller").to_string() };
                let mut f = Found::new(format!("xinput:{i}"), name, Kind::Controller, Maker::Microsoft, Link::Unknown);
                f.level = Some(xinput_level(level));
                f.approx = true;
                found.push(f);
            }
        }
        let _ = FreeLibrary(lib);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn razer_reports() {
        let r = razer_request(0x1F, 0x07, 0x80);
        assert_eq!((r[0], r[1], r[2], r[6], r[7], r[8]), (0, 0, 0x1F, 0x02, 0x07, 0x80));
        assert_eq!(r[89], 0x02 ^ 0x07 ^ 0x80);
        let mut answer = r;
        answer[1] = RAZER_OK;
        answer[10] = 0xFF;
        assert_eq!(razer_answer(&answer, 0x07, 0x80), Some((RAZER_OK, 0xFF)));
        // Synapse's question about something else isn't ours
        assert_eq!(razer_answer(&answer, 0x07, 0x84), None);
        assert_eq!(razer_kind(0x00A8), Some(Kind::Mouse));
        assert_eq!(razer_kind(0x02B4), Some(Kind::Keyboard));
        assert_eq!(razer_kind(0x0F08), None);
    }

    #[test]
    fn logitech_answers() {
        // getStatus of the unified battery: 25%, low, discharging
        assert_eq!(logitech_level(true, &[0x19, 0x02, 0x00]), (25, false));
        assert_eq!(logitech_level(true, &[0x50, 0x04, 0x01]), (80, true));
        assert_eq!(logitech_level(false, &[0x64, 0x32, 0x03]), (100, false));
        assert_eq!(logitech_kind(0x42), Kind::Mouse);
        assert_eq!(logitech_kind(0x01), Kind::Keyboard);
        assert_eq!(logitech_kind(0x0B), Kind::Controller);
    }

    #[test]
    fn rog_answers() {
        // A split keyboard, both halves at 53%
        let r = [0x02, 0x12, 0x01, 0, 0, 0, 0x35, 0x02, 0x01, 0, 0x14, 0x35, 0x04, 0x0F, 0x2E, 0xD5, 0x0E];
        assert_eq!(asus_keyboard(&r), Some((53, Some(53))));
        let mut one = r;
        one[7] = 1;
        assert_eq!(asus_keyboard(&one), Some((53, None)));
        assert_eq!(asus_keyboard(&[0x02, 0xFF, 0xAA, 0, 0, 0, 0, 0, 0, 0, 0, 0]), None);
        // A mouse at 80% charging; one asleep
        let mut m = [0u8; 16];
        m[..3].copy_from_slice(&[0x03, 0x12, 0x07]);
        m[5] = 80;
        m[10] = 1;
        assert_eq!(asus_mouse(&m), Some((80, true)));
        m[5] = 0;
        m[10] = 0;
        assert_eq!(asus_mouse(&m), Some((0, false)));
    }

    #[test]
    fn sony_reports() {
        let mut usb = [0u8; 64];
        usb[0] = 0x01;
        usb[53] = 0x07; // on battery, 7 tenths
        assert_eq!(dualsense(&usb, false), Some((75, false)));
        usb[53] = 0x14;
        assert_eq!(dualsense(&usb, false), Some((45, true)));
        usb[53] = 0x2A;
        assert_eq!(dualsense(&usb, false), Some((100, true)));
        // Bluetooth's short report has no battery
        assert_eq!(dualsense(&usb, true), None);
        let mut bt = [0u8; 78];
        bt[0] = 0x31;
        bt[54] = 0x0A;
        assert_eq!(dualsense(&bt, true), Some((100, false)));
        let mut ds4 = [0u8; 64];
        ds4[0] = 0x01;
        ds4[30] = 0x1B; // on the cable, full
        assert_eq!(dualshock(&ds4, false), Some((100, true)));
        ds4[30] = 0x03;
        assert_eq!(dualshock(&ds4, false), Some((35, false)));
        let mut ds4bt = [0u8; 78];
        ds4bt[0] = 0x11;
        ds4bt[32] = 0x15;
        assert_eq!(dualshock(&ds4bt, true), Some((55, true)));
    }

    #[test]
    fn steam_and_xbox() {
        let mut r = [0u8; 54];
        r[0] = 0x43;
        r[1] = 1;
        r[2] = 91;
        assert_eq!(steam_battery(&r), Some((91, false)));
        r[1] = 2;
        assert_eq!(steam_battery(&r), Some((91, true)));
        r[0] = 0x42;
        assert_eq!(steam_battery(&r), None);
        assert_eq!((xinput_level(0), xinput_level(3), xinput_level(9)), (5, 100, 100));
    }

    #[test]
    fn one_device_per_key() {
        let mut dongle = Found::new("razer:naga".into(), "Naga".into(), Kind::Mouse, Maker::Razer, Link::Unknown);
        let mut cable = dongle.clone();
        cable.level = Some(100);
        cable.charging = true;
        let mut other = dongle.clone();
        other.key = "razer:keys".into();
        other.level = Some(94);
        dongle.level = None;
        let merged = merge(vec![dongle, cable.clone(), other.clone()]);
        assert_eq!(merged, vec![cable, other]);
        assert_eq!(maker(0x068E), Maker::Razer);
        assert_eq!(maker(0x1234), Maker::Other);
    }
}
