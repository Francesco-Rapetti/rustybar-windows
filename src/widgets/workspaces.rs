//! The virtual desktops of Windows: one number (or the desktop's name, when it has one) each, the
//! current one under a highlight that slides from one to the next. Explorer keeps the list and the
//! current desktop in the registry (`Explorer\VirtualDesktops`: `VirtualDesktopIDs`,
//! `CurrentVirtualDesktop`, `Desktops\{id}\Name`), and the registry tells when they change, so the
//! widget reads them only then. With a single desktop Explorer writes neither, and the widget
//! hides (`single = true` keeps it).

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, KEY_READ, REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_CHANGE_NAME, RRF_RT_REG_BINARY,
    RRF_RT_REG_SZ, RegCloseKey, RegGetValueW, RegNotifyChangeKeyValue, RegOpenKeyExW,
};
use windows::Win32::System::Threading::CreateEventW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_LCONTROL,
    VK_LEFT, VK_LWIN, VK_RIGHT,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::config::Options;
use crate::log;
use crate::theme::Theme;
use crate::ui::{Item, Pill};

const KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\VirtualDesktops");

pub struct Workspaces {
    key: Option<HKEY>,
    event: HANDLE,
    single: bool,
    /// Each desktop's label: its name, or its number.
    desktops: Vec<String>,
    current: Option<usize>,
    /// A listed game is in front: clicks send no keys.
    game: bool,
}

impl Workspaces {
    pub fn new(options: &Options) -> Workspaces {
        let mut key = HKEY::default();
        let opened = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, KEY, Some(0), KEY_READ | KEY_NOTIFY, &mut key) };
        let key = opened.is_ok().then_some(key);
        if key.is_none() {
            log!("workspaces: no VirtualDesktops key ({opened:?})");
        }
        let event = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }.unwrap_or_default();
        let mut w = Workspaces { key, event, single: options.bool("single").unwrap_or(false), desktops: Vec::new(), current: None, game: false };
        w.read();
        w.watch();
        w
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
        let ids = binary(key, None, w!("VirtualDesktopIDs")).unwrap_or_default();
        let ids: Vec<[u8; 16]> = ids.as_chunks::<16>().0.to_vec();
        let current = binary(key, None, w!("CurrentVirtualDesktop")).and_then(|c| <[u8; 16]>::try_from(c.as_slice()).ok());
        let desktops: Vec<String> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| {
                let sub = HSTRING::from(format!("Desktops\\{}", guid(id)));
                string(key, &sub, w!("Name")).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| (i + 1).to_string())
            })
            .collect();
        let current = current.and_then(|c| ids.iter().position(|id| *id == c));
        let changed = desktops != self.desktops || current != self.current;
        self.desktops = desktops;
        self.current = current;
        changed
    }
}

impl Drop for Workspaces {
    fn drop(&mut self) {
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
        if self.desktops.len() < 2 && !self.single {
            return None;
        }
        let labels: Vec<String> = if self.desktops.is_empty() { vec!["1".into()] } else { self.desktops.clone() };
        let current = self.current.unwrap_or(0);
        let items = labels
            .iter()
            .enumerate()
            .map(|(i, label)| Item::text(label, if i == current { theme.on_primary } else { theme.subtext }).min_width(14.0).max_width(120.0))
            .collect();
        Some(Pill::new(items).gap(10.0).highlight(current))
    }

    fn handle(&self) -> Option<HANDLE> {
        self.key.map(|_| self.event)
    }

    /// A click on a desktop goes there, with Windows' own shortcut: Ctrl+Win+← or → as many
    /// times as there are desktops in between, sent in one go so the user's typing can't get in
    /// the middle. Nothing while a listed game is in front: the keys are simulated, and an
    /// anti-cheat may watch them. The registry says when the desktop changed, and the highlight
    /// slides there.
    fn click(&mut self, item: Option<usize>) -> bool {
        let (Some(target), Some(current)) = (item, self.current) else { return false };
        if target == current || target >= self.desktops.len() || self.game {
            return false;
        }
        let keys = switch_keys(current, target);
        let sent = unsafe { SendInput(&keys, size_of::<INPUT>() as i32) };
        if sent as usize != keys.len() {
            log!("workspaces: Windows took {sent} of {} keys", keys.len());
        }
        false
    }

    fn event(&mut self, signal: &super::Signal) -> bool {
        if let super::Signal::Game(game) = signal {
            self.game = *game;
        }
        false
    }

    fn signaled(&mut self) -> bool {
        let changed = self.read();
        self.watch();
        changed
    }
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
    #[test]
    fn guid_as_in_the_registry() {
        let bytes = [0x77, 0x4F, 0xAC, 0x07, 0xC2, 0xAA, 0x87, 0x42, 0xB9, 0xAA, 0x2A, 0x2F, 0x70, 0x86, 0xCC, 0x6A];
        assert_eq!(super::guid(&bytes), "{07AC4F77-AAC2-4287-B9AA-2A2F7086CC6A}");
    }

    #[test]
    fn keys_to_switch_desktops() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{KEYEVENTF_KEYUP, VK_LCONTROL, VK_LEFT, VK_LWIN, VK_RIGHT};
        let keys = super::switch_keys(0, 2);
        let vk: Vec<u16> = keys.iter().map(|k| unsafe { k.Anonymous.ki.wVk.0 }).collect();
        assert_eq!(vk, [VK_LCONTROL.0, VK_LWIN.0, VK_RIGHT.0, VK_RIGHT.0, VK_RIGHT.0, VK_RIGHT.0, VK_LWIN.0, VK_LCONTROL.0]);
        // Every key that goes down comes up
        let ups = keys.iter().filter(|k| unsafe { k.Anonymous.ki.dwFlags.contains(KEYEVENTF_KEYUP) }).count();
        assert_eq!(ups * 2, keys.len());
        assert_eq!(unsafe { super::switch_keys(3, 2)[2].Anonymous.ki.wVk }, VK_LEFT);
    }
}
