//! `rustybar msg <command>`: commands for the running bar, for scripts and keyboard shortcuts
//! (`rustybar msg popup clock`). They go as a `WM_COPYDATA` to the bar's hidden control window, the
//! one `rustybar quit` closes, so the bar has nothing to listen on and nothing to wake it: a
//! command is a window message like any other. The answer comes back the same way, to a
//! message-only window of the `msg` process, which waits for it up to 5 seconds.

use std::cell::RefCell;
use std::ffi::c_void;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    FindWindowW, GetClassNameW, GetWindowThreadProcessId, HWND_MESSAGE, MSG, MSGFLT_ALLOW, MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW,
    QS_ALLINPUT, RegisterClassExW, SMTO_ABORTIFHUNG, SendMessageTimeoutW, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_COPYDATA,
    WNDCLASSEXW,
};
use windows::core::{PCWSTR, w};

use crate::i18n::t;
use crate::process;

/// Marks RustyBar's own `WM_COPYDATA` ("RBM1"), so that other data sent to the window is ignored.
pub const MAGIC: usize = 0x5242_4d31;
/// The longest command or answer taken, in bytes.
const LIMIT: u32 = 1 << 20;
/// The class of the `msg` process's window that gets the answer.
const REPLY_CLASS: PCWSTR = w!("RustyBarMsg");

thread_local! {
    static ANSWER: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The text of a RustyBar `WM_COPYDATA`, or None for anything else.
///
/// # Safety
/// `lparam` must be the `LPARAM` of a `WM_COPYDATA` being handled.
pub unsafe fn read(lparam: LPARAM) -> Option<String> {
    let data = unsafe { (lparam.0 as *const COPYDATASTRUCT).as_ref()? };
    if data.dwData != MAGIC || data.cbData > LIMIT || (data.cbData > 0 && data.lpData.is_null()) {
        return None;
    }
    if data.cbData == 0 {
        return Some(String::new());
    }
    // Valid only while the message is handled: copied at once
    let bytes = unsafe { std::slice::from_raw_parts(data.lpData as *const u8, data.cbData as usize) };
    Some(String::from_utf8_lossy(bytes).into_owned())
}

fn copy_data(to: HWND, from: HWND, text: &str, timeout_ms: u32) -> bool {
    let bytes = text.as_bytes();
    let data = COPYDATASTRUCT { dwData: MAGIC, cbData: bytes.len().min(LIMIT as usize) as u32, lpData: bytes.as_ptr() as *mut c_void };
    let mut result = 0usize;
    let sent = unsafe {
        SendMessageTimeoutW(to, WM_COPYDATA, WPARAM(from.0 as usize), LPARAM(&data as *const _ as isize), SMTO_ABORTIFHUNG, timeout_ms, Some(&mut result))
    };
    sent.0 != 0 && result != 0
}

/// The bar answers a command: only to a window of a `msg` process, whatever handle it was given.
pub fn answer(to: HWND, from: HWND, text: &str) {
    let mut buf = [0u16; 32];
    let n = unsafe { GetClassNameW(to, &mut buf) };
    if String::from_utf16_lossy(&buf[..n.max(0) as usize]) != "RustyBarMsg" {
        return;
    }
    copy_data(to, from, text, 2000);
}

unsafe extern "system" fn reply_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_COPYDATA
        && let Some(text) = unsafe { read(lparam) }
    {
        ANSWER.with(|a| *a.borrow_mut() = Some(text));
        return LRESULT(1);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// `rustybar msg <words>`: sends them to the bar of this config dir and returns its answer.
pub fn send(words: &[String]) -> Result<String, String> {
    let not_running = || t("RustyBar non è in esecuzione (per questa cartella di config)", "RustyBar is not running (for this config dir)").to_string();
    unsafe {
        let target = FindWindowW(process::CONTROL_CLASS, &process::instance_title()).map_err(|_| not_running())?;
        // A popup takes the front: the bar may, if this process may (started from the app in front)
        let mut pid = 0u32;
        GetWindowThreadProcessId(target, Some(&mut pid));
        if AllowSetForegroundWindow(pid).is_err() {
            let _ = AllowSetForegroundWindow(ASFW_ANY);
        }
        let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = WNDCLASSEXW { cbSize: size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(reply_proc), hInstance: instance.into(), lpszClassName: REPLY_CLASS, ..Default::default() };
        RegisterClassExW(&class);
        let me = CreateWindowExW(WINDOW_EX_STYLE(0), REPLY_CLASS, PCWSTR::null(), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(instance.into()), None)
            .map_err(|e| e.to_string())?;
        // From an elevated terminal the answer comes from below: let it in
        let _ = ChangeWindowMessageFilterEx(me, WM_COPYDATA, MSGFLT_ALLOW, None);
        let result = exchange(target, me, &words.join("\n"));
        let _ = DestroyWindow(me);
        result
    }
}

fn exchange(target: HWND, me: HWND, text: &str) -> Result<String, String> {
    if !copy_data(target, me, text, 5000) {
        return Err(t("la barra non ha risposto", "the bar didn't answer").into());
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(answer) = ANSWER.with(|a| a.borrow_mut().take()) {
            return Ok(answer);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(t("nessuna risposta dalla barra entro 5 s", "no answer from the bar within 5 s").into());
        }
        unsafe {
            MsgWaitForMultipleObjects(None, false, left.as_millis() as u32, QS_ALLINPUT);
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}
