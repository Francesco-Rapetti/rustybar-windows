//! When the bar fails: the reason goes in the log, and the bar starts again, as the LaunchAgent
//! does on macOS. At most 3 times in 10 minutes, so a bar that fails at every start doesn't loop.
//!
//! A panic aborts the process (`panic = "abort"`), after the hook: the hook logs it and starts the
//! new bar. A fault inside a Windows call (an access violation) goes through the unhandled
//! exception filter instead. The new bar waits for the old one to be gone before it takes the
//! config dir's mutex. Not `RegisterApplicationRestart`: Windows starts the app again only after
//! 60 seconds of life, and Windows Error Reporting may show a window that takes the focus from a
//! game.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{CloseHandle, EXCEPTION_STACK_OVERFLOW};
use windows::Win32::System::Diagnostics::Debug::{EXCEPTION_POINTERS, SetUnhandledExceptionFilter};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};

use crate::log;

/// The times (Unix seconds) the bar started again, comma separated, handed to the new bar.
const RESTARTS: &str = "RUSTYBAR_RESTARTS";
/// The process id of the bar that failed, for the new one to wait for.
const AFTER: &str = "RUSTYBAR_AFTER";
const MAX: usize = 3;
const WINDOW_S: u64 = 600;

static RESTART: AtomicBool = AtomicBool::new(false);
static FAILED: AtomicBool = AtomicBool::new(false);

/// Logs panics and faults; with `restart`, starts the bar again after them (the bar's process,
/// not the settings window's or a command's).
pub fn install(restart: bool) {
    RESTART.store(restart, Ordering::Relaxed);
    std::panic::set_hook(Box::new(|info| {
        let place = info.location().map(|l| format!(" at {}:{}", l.file(), l.line())).unwrap_or_default();
        let payload = info.payload();
        let what = payload.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| payload.downcast_ref::<String>().cloned()).unwrap_or_default();
        let thread = std::thread::current().name().map(|n| format!(" in thread {n}")).unwrap_or_default();
        log::write_failing(&format!("panic{thread}{place}: {what}"));
        failed();
    }));
    unsafe {
        SetUnhandledExceptionFilter(Some(filter));
    }
}

unsafe extern "system" fn filter(info: *const EXCEPTION_POINTERS) -> i32 {
    // EXCEPTION_CONTINUE_SEARCH: Windows ends the process as it would without the filter
    const CONTINUE_SEARCH: i32 = 0;
    let Some(record) = (unsafe { info.as_ref() }).and_then(|i| unsafe { i.ExceptionRecord.as_ref() }) else { return CONTINUE_SEARCH };
    let address = record.ExceptionAddress as usize;
    let base = unsafe { GetModuleHandleW(None) }.map(|m| m.0 as usize).unwrap_or(0);
    let offset = if base != 0 && address > base { format!(" (rustybar+{:#x})", address - base) } else { String::new() };
    log::write_failing(&format!("fault {:#010x} at {address:#x}{offset}", record.ExceptionCode.0 as u32));
    // A stack overflow leaves too little stack to start a process from
    if record.ExceptionCode != EXCEPTION_STACK_OVERFLOW {
        failed();
    }
    CONTINUE_SEARCH
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The restarts still inside the window, from the variable.
fn recent(value: &str, now: u64) -> Vec<u64> {
    value.split(',').filter_map(|t| t.trim().parse::<u64>().ok()).filter(|t| now.saturating_sub(*t) < WINDOW_S).collect()
}

/// Starts the bar again, unless it already did `MAX` times in the window. Once per process.
fn failed() {
    if !RESTART.load(Ordering::Relaxed) || FAILED.swap(true, Ordering::Relaxed) {
        return;
    }
    let now = now();
    let mut times = recent(&std::env::var(RESTARTS).unwrap_or_default(), now);
    if times.len() >= MAX {
        log::write_failing(&format!("failed {MAX} times in 10 minutes: not starting again"));
        return;
    }
    times.push(now);
    let list = times.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let Ok(exe) = std::env::current_exe() else { return };
    let started = std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .env(RESTARTS, list)
        .env(AFTER, std::process::id().to_string())
        .spawn();
    match started {
        Ok(_) => log::write_failing("starting again"),
        Err(e) => log::write_failing(&format!("can't start again: {e}")),
    }
}

/// In a bar started again after a failure: waits (up to 5 s) for the old one to be gone, so its
/// mutex is free, and says so in the log. Called at the start, while the process has one thread.
pub fn after_failure() {
    let Some(pid) = std::env::var_os(AFTER) else { return };
    // Not for what this bar starts in turn (the settings window, Claude Code)
    unsafe { std::env::remove_var(AFTER) };
    let Some(pid) = pid.to_str().and_then(|p| p.parse::<u32>().ok()) else { return };
    if let Ok(process) = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
        unsafe {
            WaitForSingleObject(process, 5000);
            let _ = CloseHandle(process);
        }
    }
    let count = recent(&std::env::var(RESTARTS).unwrap_or_default(), now()).len();
    log!("started again after a failure ({count} in the last 10 minutes)");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_restarts_of_the_last_ten_minutes_count() {
        let now = 10_000;
        assert_eq!(recent("", now), Vec::<u64>::new());
        assert_eq!(recent("9000,9500, 9999,x", now), vec![9500, 9999]);
        assert_eq!(recent("9401", now).len(), 1);
        assert_eq!(recent("9400", now).len(), 0);
    }
}
