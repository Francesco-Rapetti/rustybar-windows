//! The power menu: the Windows logo in the bar (its four squares, drawn: see `ui::logo`), and in
//! its popup what the Start menu's power button and the Win+X menu do, with the same words, as the
//! Apple menu of the macOS version. About
//! this PC; Task Manager and Settings; Sleep, Restart and Shut down; Lock and Sign out (with the
//! account's name); last the config of RustyBar.
//!
//! Restart, Shut down and Sign out ask for a second click, the row in red meanwhile: a slip of the
//! mouse doesn't end a game. Shut down uses fast startup when Windows has it on, and restart and
//! shut down bring the apps back after sign-in when Windows is set to, as Start does. Everything
//! runs in-process (`ExitWindowsEx`, `LockWorkStation`, `SetSuspendState`), with no timer and no
//! data to follow: the widget costs nothing at rest. The account's full name is read when the
//! popup opens, with `netapi32.dll` loaded for that alone.
//!
//! With made-up data (see `mock`) the name is the `user` of `power.json` and the commands do
//! nothing, the second click included.

use std::ffi::c_void;

use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, FreeLibrary, HANDLE, LUID};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED, SE_SHUTDOWN_NAME, TOKEN_ADJUST_PRIVILEGES,
    TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows::Win32::System::Power::SetSuspendState;
use windows::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::System::Shutdown::{
    EWX_HYBRID_SHUTDOWN, EWX_LOGOFF, EWX_POWEROFF, EWX_REBOOT, EWX_RESTARTAPPS, EWX_SHUTDOWN, EXIT_WINDOWS_FLAGS, ExitWindowsEx,
    LockWorkStation, SHTDN_REASON_FLAG_PLANNED, SHTDN_REASON_MAJOR_OTHER, SHTDN_REASON_MINOR_OTHER,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::System::WindowsProgramming::GetUserNameW;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow, SW_SHOWNORMAL};
use windows::core::{HSTRING, PCWSTR, PWSTR, s, w};

use super::{Input, Reply};
use crate::config;
use crate::i18n::t;
use crate::log;
use crate::process;
use crate::theme::Theme;
use crate::ui::{Align, Font, Fonts, Item, Pill, Rect, Scene, Weight};

/// Segoe Fluent Icons' power button.
const POWER: &str = "\u{E7E8}";
const MARGIN: f32 = 4.0;
const ROW_H: f32 = 32.0;
const DIVIDER_H: f32 = 9.0;
const PAD: f32 = 12.0;
const ICON_W: f32 = 16.0;
const MIN_WIDTH: f32 = 240.0;

/// A line of the menu: a command, or a divider.
enum Line {
    Row { action: &'static str, icon: &'static str, text: String, shortcut: &'static str },
    Divider,
}

#[derive(Default)]
pub struct Power {
    /// The account's name, read when the popup opens.
    user: Option<String>,
    /// The command waiting for its second click.
    confirm: Option<&'static str>,
    /// The command that Windows refused.
    failed: Option<&'static str>,
    /// The made-up name, with made-up data (see `mock`).
    mock: Option<String>,
}

impl Power {
    pub fn new() -> Power {
        let mock = crate::mock::load("power").map(|m| m["user"].str().unwrap_or_default().to_string());
        Power { mock, ..Power::default() }
    }

    fn lines(&self) -> Vec<Line> {
        let row = |action, icon, text: &str, shortcut| Line::Row { action, icon, text: text.to_string(), shortcut };
        let user = self.user.as_deref().unwrap_or_default();
        let sign_out = if user.is_empty() { t("Disconnetti", "Sign out").to_string() } else { format!("{} {user}", t("Disconnetti", "Sign out")) };
        vec![
            row("about", "\u{E946}", t("Informazioni su questo PC", "About this PC"), ""),
            Line::Divider,
            row("taskmgr", "\u{F120}", t("Gestione attività", "Task Manager"), t("Ctrl+Maiusc+Esc", "Ctrl+Shift+Esc")),
            row("settings", "\u{E713}", t("Impostazioni", "Settings"), "Win+I"),
            Line::Divider,
            row("sleep", "\u{E708}", t("Sospendi", "Sleep"), ""),
            row("restart", "\u{E777}", t("Riavvia il sistema", "Restart"), ""),
            row("shutdown", POWER, t("Arresta il sistema", "Shut down"), ""),
            Line::Divider,
            row("lock", "\u{E72E}", t("Blocca", "Lock"), "Win+L"),
            Line::Row { action: "signout", icon: "\u{F3B1}", text: sign_out, shortcut: "" },
            Line::Divider,
            row("config", "", t("Config di RustyBar…", "RustyBar config…"), ""),
        ]
    }

    /// What a row says while it waits for its second click, or after Windows refused it.
    fn state_text(&self, action: &str) -> Option<&'static str> {
        if self.failed == Some(action) {
            return Some(t("Non riuscito", "Didn't work"));
        }
        if self.confirm != Some(action) {
            return None;
        }
        Some(match action {
            "restart" => t("Fai clic di nuovo per riavviare", "Click again to restart"),
            "shutdown" => t("Fai clic di nuovo per arrestare", "Click again to shut down"),
            _ => t("Fai clic di nuovo per disconnetterti", "Click again to sign out"),
        })
    }
}

impl super::Widget for Power {
    fn pill(&self, theme: &Theme) -> Option<Pill> {
        // No background: the logo stands on the desktop, as the apple in the macOS menu bar
        Some(Pill::new(vec![Item::windows_logo(theme.text)]).bare())
    }

    fn has_popup(&self) -> bool {
        true
    }

    fn popup_shown(&mut self, open: bool) {
        self.confirm = None;
        self.failed = None;
        self.user = open.then(|| self.mock.clone().unwrap_or_else(user_name));
    }

    fn popup(&self, theme: &Theme, fonts: &Fonts) -> Option<Scene> {
        let lines = self.lines();
        let regular = Font::new(theme.font_size, Weight::Regular);
        let small = Font::new(theme.small_size, Weight::Regular);
        // As wide as the longest row with its shortcut
        let text_x = PAD + ICON_W + 12.0;
        let needed = lines
            .iter()
            .map(|line| match line {
                Line::Row { action, text, shortcut, .. } => {
                    let text = self.state_text(action).unwrap_or(text);
                    let shortcut = if shortcut.is_empty() { 0.0 } else { fonts.width(shortcut, small) + 24.0 };
                    text_x + fonts.width(text, regular) + shortcut + PAD
                }
                Line::Divider => 0.0,
            })
            .fold(MIN_WIDTH, f32::max)
            .ceil();
        let width = needed + 2.0 * MARGIN;
        let height = 2.0 * MARGIN + lines.iter().map(|l| if matches!(l, Line::Row { .. }) { ROW_H } else { DIVIDER_H }).sum::<f32>();
        let mut scene = Scene::new(width, height);
        let red = theme.color("red").unwrap_or(theme.text);
        let mut y = MARGIN;
        for line in &lines {
            match line {
                Line::Divider => {
                    scene.fill(Rect::new(MARGIN + PAD, y + (DIVIDER_H / 2.0).floor(), needed - 2.0 * PAD, 1.0), 0.0, theme.text.alpha(0.08));
                    y += DIVIDER_H;
                }
                Line::Row { action, icon, text, shortcut } => {
                    let row = Rect::new(MARGIN, y, needed, ROW_H);
                    let state = self.state_text(action);
                    let color = if state.is_some() { red } else { theme.text };
                    if !icon.is_empty() {
                        scene.text(Rect::new(row.x + PAD, y, ICON_W, ROW_H), *icon, Font::icons(14.0), color, Align::Center);
                    }
                    scene.text(Rect::new(row.x + text_x, y, row.w - text_x - PAD, ROW_H), state.unwrap_or(text), regular, color, Align::Start);
                    if !shortcut.is_empty() && state.is_none() {
                        scene.text(Rect::new(row.x + PAD, y, row.w - 2.0 * PAD, ROW_H), *shortcut, small, theme.subtext, Align::End);
                    }
                    scene.zone(row, theme.row_radius, *action);
                    y += ROW_H;
                }
            }
        }
        Some(scene)
    }

    fn popup_input(&mut self, input: &Input) -> Reply {
        let Input::Action(action) = input else { return Reply::Nothing };
        let action: &'static str = match *action {
            "about" => "about",
            "taskmgr" => "taskmgr",
            "settings" => "settings",
            "sleep" => "sleep",
            "restart" => "restart",
            "shutdown" => "shutdown",
            "lock" => "lock",
            "signout" => "signout",
            "config" => "config",
            _ => return Reply::Nothing,
        };
        self.failed = None;
        // The commands that end the session wait for a second click on the same row
        if matches!(action, "restart" | "shutdown" | "signout") && self.confirm != Some(action) {
            self.confirm = Some(action);
            return Reply::Redraw;
        }
        self.confirm = None;
        if self.mock.is_some() {
            return Reply::Close;
        }
        let done = match action {
            "about" => {
                process::open("ms-settings:about");
                true
            }
            "taskmgr" => {
                process::open("taskmgr.exe");
                true
            }
            "settings" => {
                process::open("ms-settings:");
                true
            }
            "config" => {
                open_config();
                true
            }
            "lock" => unsafe { LockWorkStation() }.map_err(|e| log!("lock: {e}")).is_ok(),
            "sleep" => {
                // On a thread: Windows tells every window the PC is going to sleep, this one's too,
                // and the call returns only after it wakes. A moment first, for the popup to fade.
                std::thread::spawn(|| {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    if !shutdown_privilege() || !unsafe { SetSuspendState(false, false, false) } {
                        log!("sleep: Windows refused");
                    }
                });
                true
            }
            "restart" => end_session(EWX_REBOOT | restart_apps()),
            "shutdown" => {
                let fast = if registry_dword(HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\Session Manager\Power", "HiberbootEnabled") == Some(1) {
                    EWX_HYBRID_SHUTDOWN
                } else {
                    EXIT_WINDOWS_FLAGS(0)
                };
                end_session(EWX_SHUTDOWN | EWX_POWEROFF | fast | restart_apps())
            }
            _ => end_session(EWX_LOGOFF),
        };
        if done {
            Reply::Opened
        } else {
            self.failed = Some(action);
            Reply::Redraw
        }
    }
}

/// Restarts, shuts down or signs out, as Start does: the apps are asked to close and may say
/// they're busy. True if Windows took it.
fn end_session(flags: EXIT_WINDOWS_FLAGS) -> bool {
    if flags != EWX_LOGOFF && !shutdown_privilege() {
        return false;
    }
    let reason = SHTDN_REASON_MAJOR_OTHER | SHTDN_REASON_MINOR_OTHER | SHTDN_REASON_FLAG_PLANNED;
    match unsafe { ExitWindowsEx(flags, reason) } {
        Ok(()) => true,
        Err(e) => {
            log!("end of session ({:#x}): {e}", flags.0);
            false
        }
    }
}

/// The apps registered for it come back after sign-in, when Windows is set to (Settings >
/// Accounts > Sign-in options > Restart apps).
fn restart_apps() -> EXIT_WINDOWS_FLAGS {
    if registry_dword(HKEY_CURRENT_USER, r"Software\Microsoft\Windows NT\CurrentVersion\Winlogon", "RestartApps") == Some(1) {
        EWX_RESTARTAPPS
    } else {
        EXIT_WINDOWS_FLAGS(0)
    }
}

/// Every user may shut the PC down, but the right has to be switched on in the process's token
/// first.
fn shutdown_privilege() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut luid = LUID::default();
        let ok = LookupPrivilegeValueW(PCWSTR::null(), SE_SHUTDOWN_NAME, &mut luid).is_ok() && {
            let privileges = TOKEN_PRIVILEGES { PrivilegeCount: 1, Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }] };
            AdjustTokenPrivileges(token, false, Some(&privileges), 0, None, None).is_ok()
        };
        let _ = CloseHandle(token);
        if !ok {
            log!("the shutdown privilege couldn't be switched on");
        }
        ok
    }
}

fn registry_dword(root: HKEY, key: &str, value: &str) -> Option<u32> {
    let mut data = 0u32;
    let mut size = 4u32;
    let read = unsafe {
        RegGetValueW(root, &HSTRING::from(key), &HSTRING::from(value), RRF_RT_REG_DWORD, None, Some(&mut data as *mut u32 as *mut c_void), Some(&mut size))
    };
    (read == ERROR_SUCCESS).then_some(data)
}

/// The account's full name, as Settings shows it (`Mario Rossi`), or its user name.
fn user_name() -> String {
    full_name().filter(|n| !n.trim().is_empty()).unwrap_or_else(login_name)
}

fn login_name() -> String {
    let mut buf = [0u16; 257];
    let mut len = buf.len() as u32;
    match unsafe { GetUserNameW(Some(PWSTR(buf.as_mut_ptr())), &mut len) } {
        // The count includes the terminating null
        Ok(()) => String::from_utf16_lossy(&buf[..len.saturating_sub(1) as usize]),
        Err(_) => String::new(),
    }
}

/// The account's full name from `NetUserGetInfo` (level 2), with `netapi32.dll` loaded for this
/// alone and let go of at once: the bar doesn't load it otherwise.
fn full_name() -> Option<String> {
    // NET_API_STATUS NetUserGetInfo(LPCWSTR servername, LPCWSTR username, DWORD level, LPBYTE *bufptr)
    type GetInfo = unsafe extern "system" fn(PCWSTR, PCWSTR, u32, *mut *mut u8) -> u32;
    type Free = unsafe extern "system" fn(*mut c_void) -> u32;
    let user = HSTRING::from(login_name());
    unsafe {
        let lib = LoadLibraryExW(w!("netapi32.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
        let name = (|| {
            let get: GetInfo = std::mem::transmute(GetProcAddress(lib, s!("NetUserGetInfo"))?);
            let free: Free = std::mem::transmute(GetProcAddress(lib, s!("NetApiBufferFree"))?);
            let mut info: *mut u8 = std::ptr::null_mut();
            if get(PCWSTR::null(), PCWSTR(user.as_ptr()), 2, &mut info) != 0 || info.is_null() {
                return None;
            }
            // USER_INFO_2: usri2_name, usri2_password, usri2_password_age, usri2_priv,
            // usri2_home_dir, usri2_comment, usri2_flags, usri2_script_path, usri2_auth_flags,
            // then usri2_full_name
            #[repr(C)]
            struct UserInfo2 {
                name: PWSTR,
                password: PWSTR,
                password_age: u32,
                privilege: u32,
                home_dir: PWSTR,
                comment: PWSTR,
                flags: u32,
                script_path: PWSTR,
                auth_flags: u32,
                full_name: PWSTR,
            }
            let full = (*(info as *const UserInfo2)).full_name;
            let name = if full.is_null() { None } else { full.to_string().ok() };
            free(info as *mut c_void);
            name
        })();
        let _ = FreeLibrary(lib);
        name
    }
}

/// Opens `rustybar.toml` in the app Windows has for it, or in Notepad; writes the example first if
/// there's no file yet.
fn open_config() {
    let path = config::file();
    if !path.exists() {
        let _ = std::fs::create_dir_all(config::dir());
        let _ = std::fs::write(&path, crate::install::EXAMPLE);
    }
    let file = HSTRING::from(path.to_string_lossy().as_ref());
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        // Above 32 is success; no app for .toml is 31
        if ShellExecuteW(None, w!("open"), &file, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL).0 as isize <= 32 {
            let quoted = HSTRING::from(format!("\"{}\"", path.display()));
            ShellExecuteW(None, w!("open"), w!("notepad.exe"), &quoted, PCWSTR::null(), SW_SHOWNORMAL);
        }
    }
}
