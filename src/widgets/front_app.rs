//! The name of the app in front, from the foreground event that Windows sends out of process (a
//! hook that injects nothing anywhere), cut at 260 DIPs. The name is the app's own, from the
//! version information of its executable (`Google Chrome`, `Esplora risorse`), read once per
//! executable; a UWP app is looked up behind the frame that hosts it. With made-up data (see
//! `mock`) the name is the `name` of `front_app.json`, whatever app is in front.

use std::collections::HashMap;

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GetWindowThreadProcessId};
use windows::core::{BOOL, HSTRING, PWSTR};

use crate::theme::Theme;
use crate::ui::{Item, Pill};

/// The app in front: what the bar shows and what the game list matches.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Foreground {
    /// The executable's file name, lowercase (`chrome.exe`).
    pub exe: String,
    pub name: String,
}

pub struct FrontApp {
    name: String,
    mock: bool,
}

impl FrontApp {
    pub fn new() -> FrontApp {
        match crate::mock::load("front_app") {
            Some(m) => FrontApp { name: m["name"].str().unwrap_or_default().to_string(), mock: true },
            None => FrontApp { name: String::new(), mock: false },
        }
    }
}

impl super::Widget for FrontApp {
    fn pill(&self, theme: &Theme) -> Option<Pill> {
        if self.name.is_empty() {
            return None;
        }
        Some(Pill::new(vec![Item::text(&self.name, theme.text).max_width(260.0)]))
    }

    /// Cut shorter.
    fn compact(&self, theme: &Theme) -> Option<Pill> {
        if self.name.is_empty() {
            return None;
        }
        Some(Pill::new(vec![Item::text(&self.name, theme.text).max_width(120.0)]))
    }

    fn event(&mut self, signal: &super::Signal) -> bool {
        if let super::Signal::Foreground(front) = signal
            && !self.mock
            && front.name != self.name
        {
            self.name = front.name.clone();
            return true;
        }
        false
    }
}

/// Names already read, by executable path.
#[derive(Default)]
pub struct Names(HashMap<String, String>);

/// The app that owns `hwnd`, or None if it can't be told (a process of another user, one that
/// just quit).
pub fn resolve(hwnd: HWND, names: &mut Names) -> Option<Foreground> {
    let mut path = process_path(window_pid(hwnd)?)?;
    if file_name(&path).eq_ignore_ascii_case("ApplicationFrameHost.exe") {
        // A UWP app: the frame belongs to a host process, the app is a child window of another one
        if let Some(inner) = hosted_app(hwnd) {
            path = inner;
        }
    }
    let name = names.0.entry(path.clone()).or_insert_with(|| describe(&path)).clone();
    Some(Foreground { exe: file_name(&path).to_lowercase(), name })
}

fn window_pid(hwnd: HWND) -> Option<u32> {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    (pid != 0).then_some(pid)
}

pub fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let result = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(process);
        result.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

fn hosted_app(frame: HWND) -> Option<String> {
    struct Search {
        frame_pid: u32,
        found: Option<u32>,
    }
    unsafe extern "system" fn each(child: HWND, lparam: LPARAM) -> BOOL {
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        match window_pid(child) {
            Some(pid) if pid != search.frame_pid => {
                search.found = Some(pid);
                BOOL(0)
            }
            _ => BOOL(1),
        }
    }
    let mut search = Search { frame_pid: window_pid(frame)?, found: None };
    unsafe {
        let _ = EnumChildWindows(Some(frame), Some(each), LPARAM(&mut search as *mut Search as isize));
    }
    process_path(search.found?)
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// The app's name from its version information: the description, else the product name, else
/// the file name without `.exe`.
pub fn describe(path: &str) -> String {
    version_string(path, "FileDescription")
        .or_else(|| version_string(path, "ProductName"))
        .unwrap_or_else(|| {
            let name = file_name(path);
            name.strip_suffix(".exe").or_else(|| name.strip_suffix(".EXE")).unwrap_or(name).to_string()
        })
}

fn version_string(path: &str, key: &str) -> Option<String> {
    let wide = HSTRING::from(path);
    unsafe {
        let size = GetFileVersionInfoSizeW(&wide, None);
        if size == 0 {
            return None;
        }
        let mut block = vec![0u8; size as usize];
        GetFileVersionInfoW(&wide, None, size, block.as_mut_ptr() as *mut _).ok()?;
        // The first language the file has strings for
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        if !VerQueryValueW(block.as_ptr() as *const _, &HSTRING::from("\\VarFileInfo\\Translation"), &mut ptr, &mut len).as_bool() || len < 4 {
            return None;
        }
        let lang = *(ptr as *const u16);
        let codepage = *(ptr as *const u16).add(1);
        let query = HSTRING::from(format!("\\StringFileInfo\\{lang:04x}{codepage:04x}\\{key}"));
        if !VerQueryValueW(block.as_ptr() as *const _, &query, &mut ptr, &mut len).as_bool() || len == 0 {
            return None;
        }
        let text = std::slice::from_raw_parts(ptr as *const u16, len as usize);
        let text = String::from_utf16_lossy(text.split(|c| *c == 0).next().unwrap_or(text));
        let text = text.trim().to_string();
        (!text.is_empty()).then_some(text)
    }
}
