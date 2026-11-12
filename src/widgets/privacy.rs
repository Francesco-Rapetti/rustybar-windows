//! Which apps use the microphone or the camera now, as the privacy indicator of the taskbar shows
//! it: Windows writes when each app started and stopped using one in the registry
//! (`CapabilityAccessManager\ConsentStore\<microphone|webcam>`, a key per packaged app and one per
//! executable under `NonPackaged`, `LastUsedTimeStop = 0` while it's in use). Each tree is watched
//! with `RegNotifyChangeKeyValue` on an event of its own, so the bar wakes only when an app opens
//! or closes one; a scan takes under a millisecond.

use std::collections::HashMap;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, KEY_READ, REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_CHANGE_NAME, REG_NOTIFY_THREAD_AGNOSTIC, RRF_RT_REG_QWORD,
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegNotifyChangeKeyValue, RegOpenKeyExW,
};
use windows::Win32::System::Threading::CreateEventW;
use windows::core::{HSTRING, PWSTR, w};

use super::front_app;
use crate::log;

const STORE: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore";

/// The microphone, then the camera.
pub const KINDS: [&str; 2] = ["microphone", "webcam"];

struct Watch {
    key: HKEY,
    event: HANDLE,
}

pub struct Privacy {
    watches: [Option<Watch>; 2],
    /// The apps using each now, by name.
    pub using: [Vec<String>; 2],
    names: HashMap<String, String>,
}

impl Privacy {
    pub fn new() -> Privacy {
        let watches = KINDS.map(|kind| {
            let mut key = HKEY::default();
            let path = HSTRING::from(format!(r"{STORE}\{kind}"));
            if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &path, Some(0), KEY_READ | KEY_NOTIFY, &mut key) }.is_err() {
                return None;
            }
            match unsafe { CreateEventW(None, false, false, None) } {
                Ok(event) => Some(Watch { key, event }),
                Err(_) => {
                    unsafe {
                        let _ = RegCloseKey(key);
                    }
                    None
                }
            }
        });
        let mut p = Privacy { watches, using: [Vec::new(), Vec::new()], names: HashMap::new() };
        for i in 0..2 {
            p.arm(i);
            p.scan(i);
        }
        p
    }

    /// A made-up indicator (see `mock`): nothing is watched.
    pub fn mock(microphone: Vec<String>, camera: Vec<String>) -> Privacy {
        Privacy { watches: [None, None], using: [microphone, camera], names: HashMap::new() }
    }

    /// The events to wait on: the microphone's, then the camera's.
    pub fn handles(&self) -> Vec<HANDLE> {
        self.watches.iter().flatten().map(|w| w.event).collect()
    }

    /// The watch whose event is `handle` fired: reads that one again and watches it again. True
    /// if the apps changed.
    pub fn signaled(&mut self, handle: HANDLE) -> bool {
        let Some(i) = self.watches.iter().position(|w| w.as_ref().is_some_and(|w| w.event == handle)) else { return false };
        self.arm(i);
        let before = self.using[i].clone();
        self.scan(i);
        before != self.using[i]
    }

    /// One notification for the next change of the tree (each lasts for one).
    fn arm(&self, i: usize) {
        let Some(w) = &self.watches[i] else { return };
        let filter = REG_NOTIFY_CHANGE_LAST_SET | REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_THREAD_AGNOSTIC;
        let result = unsafe { RegNotifyChangeKeyValue(w.key, true, filter, Some(w.event), true) };
        if result.is_err() {
            log!("privacy: can't watch the {} ({result:?})", KINDS[i]);
        }
    }

    fn scan(&mut self, i: usize) {
        let Some(w) = &self.watches[i] else { return };
        let mut using = Vec::new();
        for name in subkeys(w.key) {
            if name == "NonPackaged" {
                let Some(apps) = open(w.key, &name) else { continue };
                for app in subkeys(apps) {
                    if in_use(apps, &app) {
                        // The executable's path, with # for \
                        let path = app.replace('#', "\\");
                        let name = self.names.entry(path.clone()).or_insert_with(|| front_app::describe(&path)).clone();
                        using.push(name);
                    }
                }
                unsafe {
                    let _ = RegCloseKey(apps);
                }
            } else if in_use(w.key, &name) {
                using.push(package_name(&name));
            }
        }
        using.sort();
        using.dedup();
        self.using[i] = using;
    }
}

impl Drop for Privacy {
    fn drop(&mut self) {
        for w in self.watches.iter().flatten() {
            unsafe {
                // Closing the key ends its notification
                let _ = RegCloseKey(w.key);
                let _ = CloseHandle(w.event);
            }
        }
    }
}

fn open(key: HKEY, sub: &str) -> Option<HKEY> {
    let mut out = HKEY::default();
    unsafe { RegOpenKeyExW(key, &HSTRING::from(sub), Some(0), KEY_READ, &mut out) }.is_ok().then_some(out)
}

fn subkeys(key: HKEY) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0.. {
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        if unsafe { RegEnumKeyExW(key, i, Some(PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None) }.is_err() {
            break;
        }
        out.push(String::from_utf16_lossy(&buf[..len as usize]));
    }
    out
}

fn qword(key: HKEY, sub: &str, value: windows::core::PCWSTR) -> Option<u64> {
    let mut v = 0u64;
    let mut size = 8u32;
    unsafe { RegGetValueW(key, &HSTRING::from(sub), value, RRF_RT_REG_QWORD, None, Some(&mut v as *mut u64 as *mut _), Some(&mut size)) }
        .is_ok()
        .then_some(v)
}

/// Started and not stopped since.
fn in_use(key: HKEY, sub: &str) -> bool {
    qword(key, sub, w!("LastUsedTimeStart")).unwrap_or(0) > 0 && qword(key, sub, w!("LastUsedTimeStop")) == Some(0)
}

/// A packaged app's name from its family name: `Microsoft.WindowsCamera_8wekyb3d8bbwe` →
/// `WindowsCamera`.
fn package_name(family: &str) -> String {
    let name = family.split('_').next().unwrap_or(family);
    name.rsplit('.').next().unwrap_or(name).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_of_packaged_apps() {
        assert_eq!(package_name("Microsoft.WindowsCamera_8wekyb3d8bbwe"), "WindowsCamera");
        assert_eq!(package_name("5319275A.WhatsAppDesktop_cv1g1gvanyjgm"), "WhatsAppDesktop");
        assert_eq!(package_name("Plain"), "Plain");
    }
}
