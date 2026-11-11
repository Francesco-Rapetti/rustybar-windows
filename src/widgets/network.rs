//! The connection: in the pill its icon and name (`Ethernet`, the Wi-Fi network), yellow with
//! `Nessun Internet` when the network has no internet, red with `Disconnesso` without one, `VPN`
//! in front while a VPN is up. The popup is a dashboard: the connection (state, adapter, link
//! speed; on Wi-Fi signal, band, generation and rate), download and upload (the speed now, the
//! last 30 seconds as bars, the total of the adapter), the VPN, the nearby Wi-Fi networks (a click
//! joins a known or open one; a secured new one opens Windows' own list, which asks for the
//! password), and the details: local IP, router with its ping, DNS, MAC and IPv6, each copied with
//! a click.
//!
//! Nothing is read on a timer at rest: Windows tells when connectivity changes
//! (`NotifyNetworkConnectivityHintChange`: internet, local only, none), when an interface or an
//! address does (`NotifyIpInterfaceChange`, `NotifyUnicastIpAddressChange`), and the Wi-Fi service
//! when it connects, disconnects, finishes a scan or the signal changes
//! (`WlanRegisterNotification`). Their callbacks, on threads of their own, only set an event; the
//! widget reads the adapters (`GetAdaptersAddresses`) on the main thread. Only while the popup is
//! open: the adapter's counters every second (`GetIfEntry2`) and a ping to the router every
//! 5 seconds (ICMP in-process, `IcmpSendEcho2` with an event: no privileges, nothing waits). While
//! no bar shows every callback is cancelled.
//!
//! Since Windows 11 24H2 the Wi-Fi network's name and the list of networks are given only to
//! desktop apps that may use the location: without it the pill says `Wi-Fi` and the popup says
//! what's missing, with the way to the setting.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_IO_PENDING, GlobalFree, HANDLE, HGLOBAL, NO_ERROR};
use windows::Win32::NetworkManagement::IpHelper::{
    CancelMibChangeNotify2, GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, GetIfEntry2,
    GetNetworkConnectivityHint, ICMP_ECHO_REPLY, IP_ADAPTER_ADDRESSES_LH, IcmpCloseHandle, IcmpCreateFile, IcmpParseReplies, IcmpSendEcho2,
    MIB_IF_ROW2, MIB_IPINTERFACE_ROW, MIB_NOTIFICATION_TYPE, MIB_UNICASTIPADDRESS_ROW, NotifyIpInterfaceChange, NotifyNetworkConnectivityHintChange,
    NotifyUnicastIpAddressChange,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::NetworkManagement::WiFi::{
    DOT11_SSID, L2_NOTIFICATION_DATA, WLAN_AVAILABLE_NETWORK, WLAN_AVAILABLE_NETWORK_CONNECTED, WLAN_AVAILABLE_NETWORK_HAS_PROFILE,
    WLAN_AVAILABLE_NETWORK_LIST, WLAN_BSS_ENTRY, WLAN_BSS_LIST, WLAN_CONNECTION_ATTRIBUTES, WLAN_CONNECTION_PARAMETERS, WLAN_INTERFACE_INFO,
    WLAN_INTERFACE_INFO_LIST, WLAN_NOTIFICATION_SOURCE_ACM, WLAN_NOTIFICATION_SOURCE_MSM, WLAN_NOTIFICATION_SOURCE_NONE, WlanCloseHandle,
    WlanConnect, WlanEnumInterfaces, WlanFreeMemory, WlanGetAvailableNetworkList, WlanGetNetworkBssList, WlanOpenHandle, WlanQueryInterface,
    WlanRegisterNotification, WlanScan, dot11_BSS_type_any, dot11_BSS_type_infrastructure, wlan_connection_mode_profile,
    wlan_connection_mode_temporary_profile, wlan_interface_state_connected, wlan_intf_opcode_current_connection,
};
use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6, AF_UNSPEC, NL_NETWORK_CONNECTIVITY_HINT, SOCKADDR_IN, SOCKADDR_IN6, SOCKET_ADDRESS};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::core::{GUID, HSTRING, PCWSTR};

use super::{Input, Reply, Signal};
use crate::i18n::{number, size, t};
use crate::log;
use crate::process;
use crate::theme::{Rgba, Theme};
use crate::ui::{Align, Font, Fonts, Item, Pill, Rect, Scene, Weight};
use crate::wake::{Timer, Wake};

// Segoe Fluent Icons
const ETHERNET: &str = "\u{E839}";
const ETHERNET_ERROR: &str = "\u{EB55}";
const ETHERNET_WARNING: &str = "\u{EB56}";
const WIFI: [&str; 4] = ["\u{E872}", "\u{E873}", "\u{E874}", "\u{E701}"];
const OFFLINE: &str = "\u{F384}";
const LOCK: &str = "\u{E72E}";
const DOWNLOAD: &str = "\u{E896}";
const UPLOAD: &str = "\u{E898}";
const REFRESH: &str = "\u{E72C}";

/// `NL_NETWORK_CONNECTIVITY_LEVEL_HINT`: 1 none, 2 the local network only, 3 internet, 4 internet
/// behind a sign-in page or a limit.
const LEVEL_NONE: i32 = 1;
const LEVEL_LOCAL: i32 = 2;

/// Interface types: Ethernet, Wi-Fi, loopback, and those of VPNs (PPP, tunnels, the virtual ones
/// WireGuard and the like make).
const IF_ETHERNET: u32 = 6;
const IF_WIFI: u32 = 71;
const IF_LOOPBACK: u32 = 24;
const IF_VPN: [u32; 3] = [23, 131, 53];

/// The clipboard's format for UTF-16 text (in `Win32_System_Ole`, a big part of the crate).
const CF_UNICODETEXT: u32 = 13;

/// While the popup is open: counters every second, a ping every this many seconds.
const PING_EVERY: u32 = 5;
const SECONDS: usize = 30;
/// Networks in the popup's list, at most.
const NETWORKS: usize = 8;

// The popup's layout, in DIPs, as `system`'s.
const MARGIN: f32 = 8.0;
const TILE: f32 = 160.0;
const GAP: f32 = 8.0;
const WIDE: f32 = TILE * 2.0 + GAP;
const WIDTH: f32 = WIDE + 2.0 * MARGIN;
const PAD: f32 = 10.0;
const RADIUS: f32 = 6.0;
const HEAD: f32 = 14.0;
const VALUE: f32 = 22.0;
const LINE: f32 = 16.0;
const GRAPH: f32 = 24.0;
const CONNECTION_H: f32 = PAD + HEAD + 4.0 + VALUE + 2.0 + LINE + PAD;
const GAUGE_H: f32 = PAD + HEAD + 4.0 + VALUE + 4.0 + GRAPH + PAD;
const ROW: f32 = 26.0;
const NET_ROW: f32 = 30.0;
const HEADER: f32 = 26.0;
const BUTTON: f32 = 30.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Kind {
    Ethernet,
    Wifi,
    #[default]
    Other,
}

/// An adapter that's up, as the popup shows it.
#[derive(Clone, Debug, Default, PartialEq)]
struct Adapter {
    kind: Kind,
    name: String,
    description: String,
    /// Link speed, in bits per second.
    speed: u64,
    luid: u64,
    ipv4: Option<(Ipv4Addr, u8)>,
    gateway: Option<IpAddr>,
    dns: Vec<IpAddr>,
    mac: String,
    ipv6: Option<Ipv6Addr>,
}

/// The Wi-Fi connection now.
#[derive(Clone, Debug, Default, PartialEq)]
struct WifiLink {
    /// Empty without the location permission.
    ssid: String,
    /// 0 to 100.
    signal: u32,
    /// In kbit/s.
    rate: u32,
    phy: i32,
    band: Option<&'static str>,
}

/// A network nearby.
#[derive(Clone, Debug, PartialEq)]
struct Nearby {
    ssid: String,
    profile: String,
    signal: u32,
    secured: bool,
    connected: bool,
    band: Option<&'static str>,
}

/// What the callbacks, on threads of their own, leave for the main thread.
struct Shared {
    wake: Arc<Wake>,
    changed: AtomicBool,
    wifi: AtomicBool,
    scanned: AtomicBool,
}

/// The Wi-Fi service, when the PC has an adapter for it.
struct Wlan {
    handle: HANDLE,
    interface: GUID,
    registered: bool,
}

impl Wlan {
    fn open() -> Option<Wlan> {
        let (mut version, mut handle) = (0u32, HANDLE::default());
        // Fails when the service doesn't run: a PC without Wi-Fi
        if unsafe { WlanOpenHandle(2, None, &mut version, &mut handle) } != 0 {
            return None;
        }
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        let interface = unsafe {
            if WlanEnumInterfaces(handle, None, &mut list) != 0 || list.is_null() {
                None
            } else {
                let n = (*list).dwNumberOfItems as usize;
                let first = (n > 0).then(|| (*(&raw const (*list).InterfaceInfo as *const WLAN_INTERFACE_INFO)).InterfaceGuid);
                WlanFreeMemory(list as *const c_void);
                first
            }
        };
        match interface {
            Some(interface) => Some(Wlan { handle, interface, registered: false }),
            None => {
                unsafe { WlanCloseHandle(handle, None) };
                None
            }
        }
    }

    /// The connection now, or None when not connected.
    fn current(&self) -> Option<WifiLink> {
        let (mut size, mut data) = (0u32, std::ptr::null_mut());
        unsafe {
            if WlanQueryInterface(self.handle, &self.interface, wlan_intf_opcode_current_connection, None, &mut size, &mut data, None) != 0
                || data.is_null()
            {
                return None;
            }
            let c = &*(data as *const WLAN_CONNECTION_ATTRIBUTES);
            let link = (c.isState == wlan_interface_state_connected).then(|| {
                let a = &c.wlanAssociationAttributes;
                let band = self.bss().into_iter().find(|b| b.dot11Bssid == a.dot11Bssid).and_then(|b| band(b.ulChCenterFrequency));
                WifiLink { ssid: ssid(&a.dot11Ssid), signal: a.wlanSignalQuality, rate: a.ulRxRate.max(a.ulTxRate), phy: a.dot11PhyType.0, band }
            });
            WlanFreeMemory(data);
            link
        }
    }

    /// The access points the last scan saw.
    fn bss(&self) -> Vec<WLAN_BSS_ENTRY> {
        let mut list: *mut WLAN_BSS_LIST = std::ptr::null_mut();
        unsafe {
            if WlanGetNetworkBssList(self.handle, &self.interface, None, dot11_BSS_type_any, false, None, &mut list) != 0 || list.is_null() {
                return Vec::new();
            }
            let n = (*list).dwNumberOfItems as usize;
            let entries = std::slice::from_raw_parts(&raw const (*list).wlanBssEntries as *const WLAN_BSS_ENTRY, n).to_vec();
            WlanFreeMemory(list as *const c_void);
            entries
        }
    }

    /// The networks nearby, one per name: the connected one first, then the known ones, then by
    /// signal. Err with the error when Windows refuses (no location permission).
    fn nearby(&self) -> Result<Vec<Nearby>, u32> {
        let mut list: *mut WLAN_AVAILABLE_NETWORK_LIST = std::ptr::null_mut();
        let bss = self.bss();
        let mut out: Vec<Nearby> = Vec::new();
        unsafe {
            let e = WlanGetAvailableNetworkList(self.handle, &self.interface, 0, None, &mut list);
            if e != 0 || list.is_null() {
                return Err(e);
            }
            let n = (*list).dwNumberOfItems as usize;
            let items = std::slice::from_raw_parts(&raw const (*list).Network as *const WLAN_AVAILABLE_NETWORK, n);
            for item in items {
                let name = ssid(&item.dot11Ssid);
                // Hidden networks have no name to show
                if name.is_empty() {
                    continue;
                }
                let band = bss.iter().filter(|b| ssid(&b.dot11Ssid) == name).max_by_key(|b| b.uLinkQuality).and_then(|b| band(b.ulChCenterFrequency));
                let profile = if item.dwFlags & WLAN_AVAILABLE_NETWORK_HAS_PROFILE != 0 { wide(&item.strProfileName) } else { String::new() };
                let entry = Nearby {
                    ssid: name.clone(),
                    profile,
                    signal: item.wlanSignalQuality,
                    secured: item.bSecurityEnabled.as_bool(),
                    connected: item.dwFlags & WLAN_AVAILABLE_NETWORK_CONNECTED != 0,
                    band,
                };
                // The same network once with its profile and once without: one row, the best of both
                match out.iter_mut().find(|n| n.ssid == name) {
                    Some(existing) => {
                        existing.connected |= entry.connected;
                        existing.signal = existing.signal.max(entry.signal);
                        if existing.profile.is_empty() {
                            existing.profile = entry.profile;
                        }
                    }
                    None => out.push(entry),
                }
            }
            WlanFreeMemory(list as *const c_void);
        }
        out.sort_by_key(|n| std::cmp::Reverse((n.connected, !n.profile.is_empty(), n.signal)));
        Ok(out)
    }

    fn scan(&self) {
        unsafe {
            let _ = WlanScan(self.handle, &self.interface, None, None, None);
        }
    }

    /// Joins a network with its profile, or an open one with a profile made for the occasion.
    fn connect(&self, network: &Nearby) -> bool {
        let xml;
        let (mode, profile) = if !network.profile.is_empty() {
            (wlan_connection_mode_profile, HSTRING::from(network.profile.as_str()))
        } else {
            xml = HSTRING::from(open_profile(&network.ssid));
            (wlan_connection_mode_temporary_profile, xml.clone())
        };
        let parameters = WLAN_CONNECTION_PARAMETERS {
            wlanConnectionMode: mode,
            strProfile: PCWSTR(profile.as_ptr()),
            pDot11Ssid: std::ptr::null_mut(),
            pDesiredBssidList: std::ptr::null_mut(),
            dot11BssType: dot11_BSS_type_infrastructure,
            dwFlags: 0,
        };
        let e = unsafe { WlanConnect(self.handle, &self.interface, &parameters, None) };
        if e != 0 {
            log!("network: can't join a Wi-Fi network (error {e})");
        }
        e == 0
    }

    fn register(&mut self, shared: &Arc<Shared>) {
        if self.registered {
            return;
        }
        let e = unsafe {
            WlanRegisterNotification(
                self.handle,
                WLAN_NOTIFICATION_SOURCE_ACM | WLAN_NOTIFICATION_SOURCE_MSM,
                true,
                Some(on_wlan),
                Some(Arc::as_ptr(shared) as *const c_void),
                None,
                None,
            )
        };
        self.registered = e == 0;
    }

    fn unregister(&mut self) {
        if self.registered {
            // Waits for the callbacks under way
            unsafe { WlanRegisterNotification(self.handle, WLAN_NOTIFICATION_SOURCE_NONE, true, None, None, None, None) };
            self.registered = false;
        }
    }
}

impl Drop for Wlan {
    fn drop(&mut self) {
        self.unregister();
        unsafe { WlanCloseHandle(self.handle, None) };
    }
}

/// A ping to the router while the popup is open: one in flight at a time, its reply signals an
/// event the loop waits on. The buffer belongs to the widget, so a reply never lands in freed
/// memory.
struct Ping {
    icmp: HANDLE,
    done: Arc<Wake>,
    buffer: Box<[u8; 128]>,
    pending: bool,
    sent: Option<Instant>,
}

impl Ping {
    fn new() -> Option<Ping> {
        let icmp = unsafe { IcmpCreateFile() }.ok()?;
        Some(Ping { icmp, done: Wake::new(), buffer: Box::new([0; 128]), pending: false, sent: None })
    }

    fn send(&mut self, to: Ipv4Addr) {
        // One at a time; one sent before the popup closed is over by now (it times out at 1 s)
        if self.pending && self.sent.is_some_and(|s| s.elapsed().as_secs() < 2) {
            return;
        }
        self.done.reset();
        self.sent = Some(Instant::now());
        let payload = *b"rustybar";
        let r = unsafe {
            IcmpSendEcho2(
                self.icmp,
                self.done.handle(),
                None,
                None,
                u32::from_ne_bytes(to.octets()),
                payload.as_ptr() as *const c_void,
                payload.len() as u16,
                None,
                self.buffer.as_mut_ptr() as *mut c_void,
                self.buffer.len() as u32,
                1000,
            )
        };
        // Answered at once (rare) or on its way
        let error = unsafe { windows::Win32::Foundation::GetLastError() };
        self.pending = r != 0 || error == ERROR_IO_PENDING;
        if r != 0 {
            self.done.set();
        }
    }

    /// The round trip of the reply that came, in ms.
    fn reply(&mut self) -> Option<u32> {
        self.pending = false;
        let n = unsafe { IcmpParseReplies(self.buffer.as_mut_ptr() as *mut c_void, self.buffer.len() as u32) };
        if n == 0 {
            return None;
        }
        let reply = unsafe { &*(self.buffer.as_ptr() as *const ICMP_ECHO_REPLY) };
        (reply.Status == 0).then_some(reply.RoundTripTime)
    }
}

impl Drop for Ping {
    fn drop(&mut self) {
        unsafe {
            let _ = IcmpCloseHandle(self.icmp);
        }
    }
}

/// What the open popup needs.
struct Open {
    /// Bytes per second down and up, the last `SECONDS`.
    history: VecDeque<(f64, f64)>,
    last: Option<(u64, u64, Instant)>,
    totals: (u64, u64),
    ticks: u32,
    ping: Option<u32>,
    nearby: Option<Result<Vec<Nearby>, u32>>,
    scanning: bool,
    /// The detail just copied, and when.
    copied: Option<(String, Instant)>,
}

pub struct Network {
    shared: Arc<Shared>,
    /// The handles of the IP Helper notifications, while a bar shows.
    notifications: Vec<HANDLE>,
    wlan: Option<Wlan>,
    level: i32,
    adapter: Option<Adapter>,
    vpn: Option<Adapter>,
    wifi: Option<WifiLink>,
    timer: Timer,
    ping: Option<Ping>,
    shown: bool,
    open: Option<Open>,
}

impl Network {
    pub fn new() -> Network {
        let shared = Arc::new(Shared { wake: Wake::new(), changed: AtomicBool::new(false), wifi: AtomicBool::new(false), scanned: AtomicBool::new(false) });
        let mut n = Network {
            shared,
            notifications: Vec::new(),
            wlan: Wlan::open(),
            level: 0,
            adapter: None,
            vpn: None,
            wifi: None,
            timer: Timer::new(),
            ping: None,
            shown: true,
            open: None,
        };
        n.attach();
        n
    }

    /// Asks Windows to say when the network changes, and reads it.
    fn attach(&mut self) {
        if self.notifications.is_empty() {
            let context = Some(Arc::as_ptr(&self.shared) as *const c_void);
            let mut handle = HANDLE::default();
            unsafe {
                if NotifyNetworkConnectivityHintChange(Some(on_hint), context, false, &mut handle) == NO_ERROR {
                    self.notifications.push(handle);
                }
                if NotifyIpInterfaceChange(AF_UNSPEC, Some(on_interface), context, false, &mut handle) == NO_ERROR {
                    self.notifications.push(handle);
                }
                if NotifyUnicastIpAddressChange(AF_UNSPEC, Some(on_address), context, false, &mut handle) == NO_ERROR {
                    self.notifications.push(handle);
                }
            }
        }
        if let Some(w) = &mut self.wlan {
            w.register(&self.shared);
        }
        self.read();
    }

    fn detach(&mut self) {
        for handle in self.notifications.drain(..) {
            // Waits for the callbacks under way
            unsafe {
                let _ = CancelMibChangeNotify2(handle);
            }
        }
        if let Some(w) = &mut self.wlan {
            w.unregister();
        }
    }

    /// Reads the connectivity, the adapters and the Wi-Fi connection. True if anything shown
    /// changed.
    fn read(&mut self) -> bool {
        let before = (self.level, self.adapter.clone(), self.vpn.clone(), self.wifi.clone());
        let mut hint = NL_NETWORK_CONNECTIVITY_HINT::default();
        if unsafe { GetNetworkConnectivityHint(&mut hint) } == NO_ERROR {
            self.level = hint.ConnectivityLevel.0;
        }
        let adapters = adapters();
        // The connection: the adapter up with a router and the lowest metric, else one up without
        // (a network with no way out); a VPN apart
        self.vpn = adapters.iter().find(|(a, kind)| *kind == IfKind::Vpn && a.ipv4.is_some()).map(|(a, _)| a.clone());
        let physical: Vec<Adapter> = adapters.into_iter().filter(|(_, kind)| *kind == IfKind::Physical).map(|(a, _)| a).collect();
        self.adapter = physical.iter().find(|a| a.gateway.is_some()).or(physical.first()).cloned();
        self.wifi = match (&self.wlan, &self.adapter) {
            (Some(w), Some(a)) if a.kind == Kind::Wifi => w.current(),
            _ => None,
        };
        before != (self.level, self.adapter.clone(), self.vpn.clone(), self.wifi.clone())
    }

    /// How the connection is: 0 fine, 1 no internet, 2 none.
    fn trouble(&self) -> u8 {
        match (&self.adapter, self.level) {
            (None, _) | (_, LEVEL_NONE) => 2,
            (_, LEVEL_LOCAL) => 1,
            _ => 0,
        }
    }

    fn status(&self, theme: &Theme) -> (&'static str, Rgba) {
        match self.trouble() {
            2 => (t("Disconnesso", "Disconnected"), theme.color("red").unwrap_or(theme.text)),
            1 => (t("Nessun Internet", "No internet"), theme.color("yellow").unwrap_or(theme.text)),
            _ => (t("Connesso a Internet", "Connected to the internet"), theme.color("green").unwrap_or(theme.text)),
        }
    }

    fn icon(&self) -> &'static str {
        let wifi = self.adapter.as_ref().is_some_and(|a| a.kind == Kind::Wifi);
        let ethernet = self.adapter.as_ref().is_some_and(|a| a.kind == Kind::Ethernet);
        match (self.trouble(), wifi) {
            (2, _) if ethernet => ETHERNET_ERROR,
            (2, _) => OFFLINE,
            (1, true) => WIFI[3],
            (1, false) => ETHERNET_WARNING,
            (_, true) => WIFI[signal_bars(self.wifi.as_ref().map_or(100, |w| w.signal))],
            _ => ETHERNET,
        }
    }

    fn name(&self) -> String {
        match (&self.adapter, &self.wifi) {
            (Some(a), Some(w)) if a.kind == Kind::Wifi => if w.ssid.is_empty() { "Wi-Fi".into() } else { w.ssid.clone() },
            (Some(a), _) => a.name.clone(),
            (None, _) => String::new(),
        }
    }

    /// The popup's second: counters, a ping now and then, the copied note going away.
    fn tick(&mut self) {
        let luid = self.adapter.as_ref().map(|a| a.luid);
        let gateway = self.adapter.as_ref().and_then(|a| match a.gateway {
            Some(IpAddr::V4(g)) => Some(g),
            _ => None,
        });
        let Some(open) = &mut self.open else { return };
        open.ticks += 1;
        if let Some(luid) = luid
            && let Some((down, up)) = counters(luid)
        {
            let now = Instant::now();
            if let Some((d0, u0, at)) = open.last {
                let s = now.duration_since(at).as_secs_f64().max(0.001);
                open.history.push_back((down.saturating_sub(d0) as f64 / s, up.saturating_sub(u0) as f64 / s));
                while open.history.len() > SECONDS {
                    open.history.pop_front();
                }
            }
            open.last = Some((down, up, now));
            open.totals = (down, up);
        }
        if open.ticks.is_multiple_of(PING_EVERY)
            && let (Some(g), Some(p)) = (gateway, &mut self.ping)
        {
            p.send(g);
        }
        if open.copied.as_ref().is_some_and(|(_, at)| at.elapsed().as_secs_f32() > 2.0) {
            open.copied = None;
        }
    }
}

impl Drop for Network {
    fn drop(&mut self) {
        self.detach();
    }
}

impl super::Widget for Network {
    fn pill(&self, theme: &Theme) -> Option<Pill> {
        let (status, color) = self.status(theme);
        let mut items = Vec::new();
        if self.vpn.is_some() {
            items.push(Item::icon(LOCK, theme.text));
            items.push(Item::text("VPN", theme.subtext).small().space(4.0));
        }
        let trouble = self.trouble();
        let icon = Item::icon(self.icon(), if trouble == 0 { theme.text } else { color });
        items.push(if items.is_empty() { icon } else { icon.space(10.0) });
        let text = if trouble == 0 { Item::text(self.name(), theme.text) } else { Item::text(status, color) };
        items.push(text.max_width(200.0));
        Some(Pill::new(items).gap(6.0))
    }

    /// The icons alone: the lock of a VPN, the connection's (colored when it's in trouble).
    fn compact(&self, theme: &Theme) -> Option<Pill> {
        let (_, color) = self.status(theme);
        let mut items = Vec::new();
        if self.vpn.is_some() {
            items.push(Item::icon(LOCK, theme.text));
        }
        items.push(Item::icon(self.icon(), if self.trouble() == 0 { theme.text } else { color }));
        Some(Pill::new(items).gap(8.0))
    }

    fn handles(&self) -> Vec<HANDLE> {
        let mut handles: Vec<HANDLE> = self.shared.wake.handle().into_iter().collect();
        // The positions matter: 1 the timer, 2 the ping
        if let Some(t) = self.timer.handle() {
            handles.push(t);
            if let Some(p) = self.ping.as_ref().filter(|p| p.pending).and_then(|p| p.done.handle()) {
                handles.push(p);
            }
        }
        handles
    }

    fn signaled_at(&mut self, index: usize) -> bool {
        match index {
            1 => {
                self.tick();
                true
            }
            2 => {
                let rtt = self.ping.as_mut().and_then(Ping::reply);
                if let Some(open) = &mut self.open {
                    open.ping = rtt;
                }
                self.open.is_some()
            }
            _ => {
                let mut changed = false;
                if self.shared.changed.swap(false, Ordering::AcqRel) | self.shared.wifi.swap(false, Ordering::AcqRel) {
                    changed |= self.read();
                }
                if self.shared.scanned.swap(false, Ordering::AcqRel)
                    && let (Some(open), Some(w)) = (&mut self.open, &self.wlan)
                {
                    open.nearby = Some(w.nearby());
                    open.scanning = false;
                    changed = true;
                }
                changed
            }
        }
    }

    fn event(&mut self, signal: &Signal) -> bool {
        match signal {
            Signal::Shown(shown) if *shown != self.shown => {
                self.shown = *shown;
                if *shown {
                    self.attach();
                    true
                } else {
                    self.detach();
                    false
                }
            }
            Signal::Resume => self.read(),
            _ => false,
        }
    }

    fn has_popup(&self) -> bool {
        true
    }

    fn popup_shown(&mut self, open: bool) {
        if open {
            self.read();
            let nearby = self.wlan.as_ref().map(|w| {
                w.scan();
                w.nearby()
            });
            let scanning = nearby.is_some();
            self.open = Some(Open {
                history: VecDeque::new(),
                last: None,
                totals: (0, 0),
                ticks: 0,
                ping: None,
                nearby,
                scanning,
                copied: None,
            });
            if self.ping.is_none() {
                self.ping = Ping::new();
            }
            self.tick();
            // The first ping now, then every 5 seconds
            if let (Some(IpAddr::V4(g)), Some(p)) = (self.adapter.as_ref().and_then(|a| a.gateway), &mut self.ping) {
                p.send(g);
            }
            self.timer.start(1000, 1000);
        } else {
            self.open = None;
            self.timer.stop();
        }
    }

    fn popup(&self, theme: &Theme, fonts: &Fonts) -> Option<Scene> {
        let open = self.open.as_ref()?;
        let details = self.details(open);
        let nearby_rows = match &open.nearby {
            Some(Ok(list)) => list.len().clamp(1, NETWORKS) + 1,
            Some(Err(_)) => 2,
            None => 0,
        };
        let mut height = MARGIN + CONNECTION_H + GAP;
        if self.adapter.is_some() {
            height += GAUGE_H + GAP;
        }
        if self.vpn.is_some() {
            height += CONNECTION_H + GAP;
        }
        if nearby_rows > 0 {
            height += HEADER + nearby_rows as f32 * NET_ROW + GAP;
        }
        if !details.is_empty() {
            height += PAD + HEAD + 4.0 + details.len() as f32 * ROW + PAD - 4.0 + GAP;
        }
        height += BUTTON + 6.0;
        let mut scene = Scene::new(WIDTH, height);
        let mut y = MARGIN;

        // The connection
        let (status, status_color) = self.status(theme);
        let r = Rect::new(MARGIN, y, WIDE, CONNECTION_H);
        scene.fill(r, RADIUS, theme.tile(status_color, self.trouble() > 0));
        heading(&mut scene, theme, fonts, r, t("Connessione", "Connection"), (status.to_string(), status_color));
        let name = match &self.adapter {
            Some(_) => self.name(),
            None => t("Nessuna rete", "No network").to_string(),
        };
        let vy = r.y + PAD + HEAD + 4.0;
        scene.text(Rect::new(r.x + PAD, vy, r.w - 2.0 * PAD, VALUE), name, big(), theme.text, Align::Start);
        let line = self.connection_line();
        scene.text(Rect::new(r.x + PAD, vy + VALUE + 2.0, r.w - 2.0 * PAD, LINE), line, small(theme), theme.subtext, Align::Start);
        y += CONNECTION_H + GAP;

        // Download and upload
        if self.adapter.is_some() {
            for (i, (label, icon)) in [(t("Download", "Download"), DOWNLOAD), (t("Upload", "Upload"), UPLOAD)].into_iter().enumerate() {
                let r = Rect::new(MARGIN + i as f32 * (TILE + GAP), y, TILE, GAUGE_H);
                scene.fill(r, RADIUS, theme.tile(theme.primary, false));
                let total = if i == 0 { open.totals.0 } else { open.totals.1 };
                scene.text(Rect::new(r.x + PAD - 1.0, r.y + PAD, 14.0, HEAD), icon, Font::icons(11.0), theme.subtext, Align::Start);
                let inner = Rect::new(r.x + 16.0, r.y, r.w - 16.0, r.h);
                heading(&mut scene, theme, fonts, inner, label, (size(total), theme.subtext));
                let now = open.history.back().map(|s| if i == 0 { s.0 } else { s.1 });
                let speed = now.map_or("–".to_string(), rate);
                scene.text(Rect::new(r.x + PAD, r.y + PAD + HEAD + 4.0, r.w - 2.0 * PAD, VALUE), speed, big(), theme.text, Align::Start);
                let values: Vec<f64> = open.history.iter().map(|s| if i == 0 { s.0 } else { s.1 }).collect();
                bars(&mut scene, theme, r, &values);
            }
            y += GAUGE_H + GAP;
        }

        // The VPN
        if let Some(vpn) = &self.vpn {
            let r = Rect::new(MARGIN, y, WIDE, CONNECTION_H);
            scene.fill(r, RADIUS, theme.tile(theme.primary, false));
            heading(&mut scene, theme, fonts, r, "VPN", (vpn.description.clone(), theme.subtext));
            let vy = r.y + PAD + HEAD + 4.0;
            scene.text(Rect::new(r.x + PAD, vy, r.w - 2.0 * PAD, VALUE), vpn.name.as_str(), big(), theme.text, Align::Start);
            let ip = vpn.ipv4.map(|(a, p)| format!("{a}/{p}")).unwrap_or_default();
            scene.text(Rect::new(r.x + PAD, vy + VALUE + 2.0, r.w - 2.0 * PAD, LINE), ip, small(theme), theme.subtext, Align::Start);
            y += CONNECTION_H + GAP;
        }

        // The Wi-Fi networks
        if let Some(nearby) = &open.nearby {
            let label = if open.scanning { t("Reti Wi-Fi · ricerca…", "Wi-Fi networks · scanning…") } else { t("Reti Wi-Fi", "Wi-Fi networks") };
            scene.text(Rect::new(MARGIN + PAD, y, WIDE - 2.0 * PAD - 30.0, HEADER), label, Font::new(theme.small_size, Weight::SemiBold), theme.subtext, Align::Start);
            let refresh = Rect::new(MARGIN + WIDE - 30.0, y + 1.0, 26.0, HEADER - 2.0);
            scene.text(refresh, REFRESH, Font::icons(12.0), theme.text, Align::Center);
            scene.zone(refresh, theme.row_radius, "scan");
            y += HEADER;
            match nearby {
                Ok(list) => {
                    if list.is_empty() {
                        let none = t("Nessuna rete trovata", "No networks found");
                        scene.text(Rect::new(MARGIN + PAD, y, WIDE - 2.0 * PAD, NET_ROW), none, regular(theme), theme.subtext, Align::Start);
                        y += NET_ROW;
                    }
                    for (i, n) in list.iter().take(NETWORKS).enumerate() {
                        network_row(&mut scene, theme, fonts, y, n);
                        if !n.connected {
                            scene.zone(Rect::new(MARGIN, y, WIDE, NET_ROW), theme.row_radius, format!("join:{i}"));
                        }
                        y += NET_ROW;
                    }
                    let more = t("Altre reti…", "More networks…");
                    scene.text(Rect::new(MARGIN + PAD, y, WIDE - 2.0 * PAD, NET_ROW), more, regular(theme), theme.primary, Align::Start);
                    scene.zone(Rect::new(MARGIN, y, WIDE, NET_ROW), theme.row_radius, "networks");
                    y += NET_ROW;
                }
                Err(_) => {
                    let why = t("Windows dà le reti solo alle app che possono usare la posizione", "Windows lists networks only to apps that may use the location");
                    scene.text(Rect::new(MARGIN + PAD, y, WIDE - 2.0 * PAD, NET_ROW), why, small(theme), theme.subtext, Align::Start);
                    y += NET_ROW;
                    let open_settings = t("Impostazioni di posizione…", "Location settings…");
                    scene.text(Rect::new(MARGIN + PAD, y, WIDE - 2.0 * PAD, NET_ROW), open_settings, regular(theme), theme.primary, Align::Start);
                    scene.zone(Rect::new(MARGIN, y, WIDE, NET_ROW), theme.row_radius, "location");
                    y += NET_ROW;
                }
            }
            y += GAP;
        }

        // The details, each copied with a click
        if !details.is_empty() {
            let h = PAD + HEAD + 4.0 + details.len() as f32 * ROW + PAD - 4.0;
            let r = Rect::new(MARGIN, y, WIDE, h);
            scene.fill(r, RADIUS, theme.tile(theme.primary, false));
            let note = match &open.copied {
                Some(_) => (t("Copiato", "Copied").to_string(), theme.color("green").unwrap_or(theme.primary)),
                None => (t("Clic per copiare", "Click to copy").to_string(), theme.overlay),
            };
            heading(&mut scene, theme, fonts, r, t("Dettagli", "Details"), note);
            let mut ry = r.y + PAD + HEAD + 4.0;
            for (key, label, value, extra) in &details {
                let row = Rect::new(r.x + 4.0, ry, r.w - 8.0, ROW);
                scene.text(Rect::new(r.x + PAD, ry, 90.0, ROW), *label, small(theme), theme.subtext, Align::Start);
                let extra_w = extra.as_deref().map_or(0.0, |e| fonts.width(e, small(theme)).ceil() + 8.0);
                let value_x = r.x + PAD + 92.0;
                let value_w = r.x + r.w - PAD - extra_w - value_x;
                scene.text(Rect::new(value_x, ry, value_w, ROW), value.as_str(), Font::new(theme.font_size, Weight::Medium), theme.text, Align::End);
                if let Some(e) = extra {
                    scene.text(Rect::new(r.x + r.w - PAD - extra_w + 8.0, ry, extra_w - 8.0, ROW), e.as_str(), small(theme), theme.subtext, Align::End);
                }
                scene.zone(row, theme.row_radius, format!("copy:{key}"));
                ry += ROW;
            }
            y += h + GAP;
        }

        let settings = t("Impostazioni di rete", "Network settings");
        scene.text(Rect::new(MARGIN + PAD, y, WIDE - 2.0 * PAD, BUTTON), settings, regular(theme), theme.primary, Align::Start);
        scene.zone(Rect::new(MARGIN, y, WIDE, BUTTON), theme.row_radius, "settings");
        Some(scene)
    }

    fn popup_input(&mut self, input: &Input) -> Reply {
        let Input::Action(action) = input else { return Reply::Nothing };
        match *action {
            "settings" => {
                process::open("ms-settings:network-status");
                Reply::Opened
            }
            "networks" => {
                // Windows' own list of networks, which asks for a password
                process::open("ms-availablenetworks:");
                Reply::Opened
            }
            "location" => {
                process::open("ms-settings:privacy-location");
                Reply::Opened
            }
            "scan" => {
                let (Some(open), Some(w)) = (&mut self.open, &self.wlan) else { return Reply::Nothing };
                w.scan();
                open.scanning = true;
                Reply::Redraw
            }
            _ => {
                if let Some(key) = action.strip_prefix("copy:") {
                    let Some(open) = &self.open else { return Reply::Nothing };
                    let Some((_, _, value, _)) = self.details(open).into_iter().find(|d| d.0 == key) else { return Reply::Nothing };
                    if copy(&value) {
                        if let Some(open) = &mut self.open {
                            open.copied = Some((key.to_string(), Instant::now()));
                        }
                        return Reply::Redraw;
                    }
                    return Reply::Nothing;
                }
                if let Some(i) = action.strip_prefix("join:").and_then(|i| i.parse::<usize>().ok()) {
                    let Some(Some(Ok(list))) = self.open.as_ref().map(|o| &o.nearby) else { return Reply::Nothing };
                    let Some(network) = list.get(i).cloned() else { return Reply::Nothing };
                    if network.profile.is_empty() && network.secured {
                        // A password is needed: Windows asks for it
                        process::open("ms-availablenetworks:");
                        return Reply::Opened;
                    }
                    if let Some(w) = &self.wlan {
                        w.connect(&network);
                    }
                }
                Reply::Nothing
            }
        }
    }
}

impl Network {
    /// The line under the connection's name: the adapter and its speed, or on Wi-Fi the signal,
    /// the band, the generation and the rate.
    fn connection_line(&self) -> String {
        let Some(a) = &self.adapter else { return t("Nessun adattatore collegato", "No adapter connected").into() };
        match &self.wifi {
            Some(w) => {
                let mut parts = vec![format!("{} {}%", t("Segnale", "Signal"), w.signal)];
                parts.extend(w.band.map(String::from));
                parts.extend(generation(w.phy).map(String::from));
                if w.rate > 0 {
                    parts.push(link_speed(w.rate as u64 * 1000));
                }
                parts.join(" · ")
            }
            None => {
                let mut parts = vec![a.description.clone()];
                if a.speed > 0 {
                    parts.push(link_speed(a.speed));
                }
                parts.join(" · ")
            }
        }
    }

    /// The details of the connection: key, label, value, and what goes after it (the ping).
    fn details(&self, open: &Open) -> Vec<(&'static str, &'static str, String, Option<String>)> {
        let Some(a) = &self.adapter else { return Vec::new() };
        let mut rows = Vec::new();
        if let Some((ip, prefix)) = a.ipv4 {
            rows.push(("ip", t("IP locale", "Local IP"), format!("{ip}/{prefix}"), None));
        }
        if let Some(g) = a.gateway {
            let ping = open.ping.map(|ms| if ms == 0 { "< 1 ms".to_string() } else { format!("{ms} ms") });
            rows.push(("router", "Router", g.to_string(), ping));
        }
        if !a.dns.is_empty() {
            rows.push(("dns", "DNS", a.dns.iter().take(2).map(|d| d.to_string()).collect::<Vec<_>>().join(", "), None));
        }
        if !a.mac.is_empty() {
            rows.push(("mac", "MAC", a.mac.clone(), None));
        }
        if let Some(v6) = a.ipv6 {
            rows.push(("ipv6", "IPv6", v6.to_string(), None));
        }
        rows
    }
}

/// A network to join: its signal, name, lock, band, and `Connessa` for the one in use.
fn network_row(scene: &mut Scene, theme: &Theme, fonts: &Fonts, y: f32, n: &Nearby) {
    let color = if n.connected { theme.text } else { theme.subtext };
    scene.text(Rect::new(MARGIN + PAD, y, 20.0, NET_ROW), WIFI[signal_bars(n.signal)], Font::icons(14.0), color, Align::Center);
    let right = if n.connected { t("Connessa", "Connected").to_string() } else { n.band.unwrap_or("").to_string() };
    let right_w = fonts.width(&right, small(theme)).ceil();
    let lock_w = if n.secured { 18.0 } else { 0.0 };
    let name_x = MARGIN + PAD + 28.0;
    let name_w = MARGIN + WIDE - PAD - right_w - lock_w - 8.0 - name_x;
    let font = Font::new(theme.font_size, if n.connected { Weight::SemiBold } else { Weight::Regular });
    scene.text(Rect::new(name_x, y, name_w, NET_ROW), n.ssid.as_str(), font, color, Align::Start);
    if n.secured {
        scene.text(Rect::new(MARGIN + WIDE - PAD - right_w - lock_w - 4.0, y, lock_w, NET_ROW), LOCK, Font::icons(11.0), theme.subtext, Align::Center);
    }
    let right_color = if n.connected { theme.primary } else { theme.subtext };
    scene.text(Rect::new(MARGIN + WIDE - PAD - right_w, y, right_w, NET_ROW), right, small(theme), right_color, Align::End);
}

/// The last seconds as bars, the newest on the right, scaled to the busiest second (at least
/// 64 KB/s, so a quiet line doesn't look busy).
fn bars(scene: &mut Scene, theme: &Theme, r: Rect, values: &[f64]) {
    let top = values.iter().copied().fold(64.0 * 1024.0, f64::max);
    let inner = r.w - 2.0 * PAD;
    let step = inner / SECONDS as f32;
    let bottom = r.y + r.h - PAD;
    for (i, v) in values.iter().rev().enumerate() {
        let right = (r.x + r.w - PAD - i as f32 * step).round();
        let left = (right - step + 1.5).round().min(right - 1.0);
        let h = (2.0 + (GRAPH - 2.0) * (*v / top) as f32).round();
        scene.fill(Rect::new(left, bottom - h, right - left, h), 1.0, theme.primary);
    }
}

/// The top of a tile, as `system`'s: a label on the left, a note on the right.
fn heading(scene: &mut Scene, theme: &Theme, fonts: &Fonts, r: Rect, label: &str, (note, note_color): (String, Rgba)) {
    let font = Font::new(theme.small_size, Weight::Medium);
    let bold = Font::new(theme.small_size, Weight::SemiBold);
    let room = r.w - 2.0 * PAD - 8.0;
    let note_w = fonts.width(&note, font).min((room - fonts.width(label, bold)).max(room / 2.0)).ceil();
    scene.text(Rect::new(r.x + PAD, r.y + PAD, room - note_w, HEAD), label, bold, theme.subtext, Align::Start);
    if !note.is_empty() {
        scene.text(Rect::new(r.x + r.w - PAD - note_w, r.y + PAD, note_w, HEAD), note, font, note_color, Align::End);
    }
}

fn big() -> Font {
    Font::new(17.0, Weight::SemiBold)
}

fn small(theme: &Theme) -> Font {
    Font::new(theme.small_size, Weight::Medium)
}

fn regular(theme: &Theme) -> Font {
    Font::new(theme.font_size, Weight::Regular)
}

/// 0 to 3 bars of Wi-Fi signal.
fn signal_bars(quality: u32) -> usize {
    match quality {
        0..=24 => 0,
        25..=49 => 1,
        50..=74 => 2,
        _ => 3,
    }
}

/// `1 Gb/s`, `2,5 Gb/s`, `100 Mb/s`.
fn link_speed(bits: u64) -> String {
    if bits >= 1_000_000_000 {
        let g = bits as f64 / 1e9;
        format!("{} Gb/s", number(g, if g.fract() == 0.0 { 0 } else { 1 }))
    } else {
        format!("{} Mb/s", bits / 1_000_000)
    }
}

/// Bytes per second: `0 KB/s`, `840 KB/s`, `4,2 MB/s`.
fn rate(bytes: f64) -> String {
    if bytes >= 1024.0 * 1024.0 {
        let m = bytes / (1024.0 * 1024.0);
        format!("{} MB/s", number(m, if m < 100.0 { 1 } else { 0 }))
    } else {
        format!("{} KB/s", (bytes / 1024.0).round())
    }
}

/// The Wi-Fi generation of a PHY type.
fn generation(phy: i32) -> Option<&'static str> {
    match phy {
        7 => Some("Wi-Fi 4"),
        8 => Some("Wi-Fi 5"),
        10 => Some("Wi-Fi 6"),
        11 => Some("Wi-Fi 7"),
        _ => None,
    }
}

/// The band of a channel's center frequency, in kHz.
fn band(khz: u32) -> Option<&'static str> {
    match khz / 1000 {
        2400..=2500 => Some("2,4 GHz"),
        5150..=5925 => Some("5 GHz"),
        5926..=7125 => Some("6 GHz"),
        _ => None,
    }
}

fn ssid(s: &DOT11_SSID) -> String {
    let n = (s.uSSIDLength as usize).min(32);
    String::from_utf8_lossy(&s.ucSSID[..n]).into_owned()
}

fn wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// A profile for an open network, made for one connection.
fn open_profile(ssid: &str) -> String {
    let name: String = ssid.chars().map(|c| match c {
        '&' => "&amp;".to_string(),
        '<' => "&lt;".to_string(),
        '>' => "&gt;".to_string(),
        '"' => "&quot;".to_string(),
        '\'' => "&apos;".to_string(),
        c => c.to_string(),
    }).collect();
    format!(
        "<?xml version=\"1.0\"?><WLANProfile xmlns=\"http://www.microsoft.com/networking/WLAN/profile/v1\"><name>{name}</name>\
         <SSIDConfig><SSID><name>{name}</name></SSID></SSIDConfig><connectionType>ESS</connectionType><connectionMode>manual</connectionMode>\
         <MSM><security><authEncryption><authentication>open</authentication><encryption>none</encryption><useOneX>false</useOneX>\
         </authEncryption></security></MSM></WLANProfile>"
    )
}

#[derive(Clone, Copy, PartialEq)]
enum IfKind {
    Physical,
    Vpn,
}

/// The adapters that are up, with what the popup shows, by the order Windows routes through
/// them (the lowest metric first). Loopback and adapters without addresses are left out.
fn adapters() -> Vec<(Adapter, IfKind)> {
    let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST;
    let mut size = 16 * 1024u32;
    let mut buf: Vec<u64>;
    loop {
        buf = vec![0u64; (size as usize).div_ceil(8)];
        let e = unsafe { GetAdaptersAddresses(AF_UNSPEC.0 as u32, flags, None, Some(buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH), &mut size) };
        if e == ERROR_BUFFER_OVERFLOW.0 {
            continue;
        }
        if e != NO_ERROR.0 {
            return Vec::new();
        }
        break;
    }
    let mut out: Vec<(Adapter, IfKind, u32)> = Vec::new();
    let mut p = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    while !p.is_null() {
        let a = unsafe { &*p };
        p = a.Next;
        if a.OperStatus != IfOperStatusUp || a.IfType == IF_LOOPBACK {
            continue;
        }
        let kind = if IF_VPN.contains(&a.IfType) { IfKind::Vpn } else { IfKind::Physical };
        let mut adapter = Adapter {
            kind: match a.IfType {
                IF_ETHERNET => Kind::Ethernet,
                IF_WIFI => Kind::Wifi,
                _ => Kind::Other,
            },
            name: unsafe { a.FriendlyName.to_string() }.unwrap_or_default(),
            description: unsafe { a.Description.to_string() }.unwrap_or_default(),
            speed: a.TransmitLinkSpeed.max(a.ReceiveLinkSpeed),
            luid: unsafe { a.Luid.Value },
            ..Default::default()
        };
        if a.PhysicalAddressLength > 0 {
            let n = (a.PhysicalAddressLength as usize).min(8);
            adapter.mac = a.PhysicalAddress[..n].iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join("-");
        }
        let mut u = a.FirstUnicastAddress;
        while !u.is_null() {
            let unicast = unsafe { &*u };
            match address(&unicast.Address) {
                Some(IpAddr::V4(v4)) if adapter.ipv4.is_none() => adapter.ipv4 = Some((v4, unicast.OnLinkPrefixLength)),
                // A global address, not the link-local one every adapter has
                Some(IpAddr::V6(v6)) if adapter.ipv6.is_none() && (v6.segments()[0] & 0xe000) == 0x2000 => adapter.ipv6 = Some(v6),
                _ => {}
            }
            u = unicast.Next;
        }
        let mut g = a.FirstGatewayAddress;
        while !g.is_null() {
            let gateway = unsafe { &*g };
            if let Some(ip) = address(&gateway.Address)
                && (adapter.gateway.is_none() || ip.is_ipv4())
            {
                adapter.gateway = Some(ip);
            }
            g = gateway.Next;
        }
        let mut d = a.FirstDnsServerAddress;
        while !d.is_null() {
            let dns = unsafe { &*d };
            if let Some(ip) = address(&dns.Address)
                && !matches!(ip, IpAddr::V6(v6) if v6.segments()[0] == 0xfec0)
            {
                adapter.dns.push(ip);
            }
            d = dns.Next;
        }
        // IPv4 first: shorter, and what most people look for
        adapter.dns.sort_by_key(|ip| ip.is_ipv6());
        if adapter.ipv4.is_none() && adapter.ipv6.is_none() {
            continue;
        }
        out.push((adapter, kind, a.Ipv4Metric));
    }
    out.sort_by_key(|(_, _, metric)| *metric);
    out.into_iter().map(|(a, k, _)| (a, k)).collect()
}

fn address(s: &SOCKET_ADDRESS) -> Option<IpAddr> {
    if s.lpSockaddr.is_null() {
        return None;
    }
    unsafe {
        match (*s.lpSockaddr).sa_family {
            AF_INET => {
                let v4 = &*(s.lpSockaddr as *const SOCKADDR_IN);
                Some(IpAddr::V4(Ipv4Addr::from(v4.sin_addr.S_un.S_addr.to_ne_bytes())))
            }
            AF_INET6 => {
                let v6 = &*(s.lpSockaddr as *const SOCKADDR_IN6);
                Some(IpAddr::V6(Ipv6Addr::from(v6.sin6_addr.u.Byte)))
            }
            _ => None,
        }
    }
}

/// Bytes received and sent by the adapter since it came up.
fn counters(luid: u64) -> Option<(u64, u64)> {
    let mut row = MIB_IF_ROW2::default();
    row.InterfaceLuid.Value = luid;
    (unsafe { GetIfEntry2(&mut row) } == NO_ERROR).then_some((row.InOctets, row.OutOctets))
}

/// Puts `text` on the clipboard.
fn copy(text: &str) -> bool {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        if OpenClipboard(None).is_err() {
            return false;
        }
        let _ = EmptyClipboard();
        let ok = (|| -> windows::core::Result<()> {
            let memory: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2)?;
            let ptr = GlobalLock(memory) as *mut u16;
            if ptr.is_null() {
                let _ = GlobalFree(Some(memory));
                return Err(windows::Win32::Foundation::E_FAIL.into());
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
            let _ = GlobalUnlock(memory);
            // The clipboard owns the memory from here
            if let Err(e) = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(memory.0))) {
                let _ = GlobalFree(Some(memory));
                return Err(e);
            }
            Ok(())
        })()
        .is_ok();
        let _ = CloseClipboard();
        ok
    }
}

// ---- the callbacks, on threads of Windows' -----------------------------------------------------

unsafe fn poke(context: *const c_void, flag: fn(&Shared) -> &AtomicBool) {
    if context.is_null() {
        return;
    }
    let shared = unsafe { &*(context as *const Shared) };
    flag(shared).store(true, Ordering::Release);
    shared.wake.set();
}

unsafe extern "system" fn on_hint(context: *const c_void, _: NL_NETWORK_CONNECTIVITY_HINT) {
    unsafe { poke(context, |s| &s.changed) }
}

unsafe extern "system" fn on_interface(context: *const c_void, _: *const MIB_IPINTERFACE_ROW, _: MIB_NOTIFICATION_TYPE) {
    unsafe { poke(context, |s| &s.changed) }
}

unsafe extern "system" fn on_address(context: *const c_void, _: *const MIB_UNICASTIPADDRESS_ROW, _: MIB_NOTIFICATION_TYPE) {
    unsafe { poke(context, |s| &s.changed) }
}

unsafe extern "system" fn on_wlan(data: *mut L2_NOTIFICATION_DATA, context: *mut c_void) {
    if data.is_null() {
        return;
    }
    let code = unsafe { (*data).NotificationCode };
    let source = unsafe { (*data).NotificationSource };
    // The service's: scan complete (7), scan failed (8), list refreshed (26)
    if source == WLAN_NOTIFICATION_SOURCE_ACM && matches!(code, 7 | 8 | 26) {
        unsafe { poke(context, |s| &s.scanned) }
    } else {
        unsafe { poke(context, |s| &s.wifi) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speeds_bands_and_profiles() {
        assert_eq!(link_speed(1_000_000_000), "1 Gb/s");
        assert_eq!(link_speed(100_000_000), "100 Mb/s");
        assert!(link_speed(2_500_000_000).starts_with("2"));
        assert_eq!(rate(0.0), "0 KB/s");
        assert_eq!(rate(840.0 * 1024.0), "840 KB/s");
        assert_eq!(band(2_437_000), Some("2,4 GHz"));
        assert_eq!(band(5_180_000), Some("5 GHz"));
        assert_eq!(band(5_955_000), Some("6 GHz"));
        assert_eq!(signal_bars(10), 0);
        assert_eq!(signal_bars(90), 3);
        assert!(open_profile("A&B").contains("<name>A&amp;B</name>"));
    }

    #[test]
    fn the_adapters_can_be_read() {
        // Whatever the PC has: the read doesn't fail, and what it gives holds together
        for (a, _) in adapters() {
            assert!(a.ipv4.is_some() || a.ipv6.is_some());
        }
    }
}
