//! The process makes itself small before anything else: below normal priority, EcoQoS (on a
//! hybrid CPU the scheduler keeps it on the efficient cores), low memory priority, and per-monitor
//! DPI so that every screen draws at its own scale. One bar per config dir.

use std::ffi::c_void;

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, LPARAM, WPARAM};
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::System::Memory::{HeapOptimizeResources, HeapSetInformation};
use windows::Win32::System::Threading::{
    BELOW_NORMAL_PRIORITY_CLASS, CreateMutexW, GetCurrentProcess, MEMORY_PRIORITY_INFORMATION, MEMORY_PRIORITY_LOW,
    PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
    ProcessMemoryPriority, ProcessPowerThrottling, SetPriorityClass, SetProcessInformation,
};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow, FindWindowW, PostMessageW, SW_SHOWNORMAL, WM_CLOSE};
use windows::core::{HSTRING, PCWSTR, w};

use crate::log;

/// The class of the hidden window that receives the broadcasts (display, theme, time, power).
pub const CONTROL_CLASS: windows::core::PCWSTR = windows::core::w!("RustyBarControl");

pub fn setup() {
    unsafe {
        if let Err(e) = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) {
            log!("per-monitor DPI: {e}");
        }
        let process = GetCurrentProcess();
        if let Err(e) = SetPriorityClass(process, BELOW_NORMAL_PRIORITY_CLASS) {
            log!("priority: {e}");
        }
    }
    make_small(unsafe { GetCurrentProcess() });
}

/// EcoQoS (on a hybrid CPU the scheduler keeps the process on the efficient cores, and it runs at
/// a lower clock) and low memory priority, for this process or a child of it.
pub fn make_small(process: HANDLE) {
    unsafe {
        let throttling = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        };
        if let Err(e) = SetProcessInformation(
            process,
            ProcessPowerThrottling,
            &throttling as *const _ as *const c_void,
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        ) {
            log!("EcoQoS: {e}");
        }
        let memory = MEMORY_PRIORITY_INFORMATION { MemoryPriority: MEMORY_PRIORITY_LOW };
        if let Err(e) = SetProcessInformation(
            process,
            ProcessMemoryPriority,
            &memory as *const _ as *const c_void,
            size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
        ) {
            log!("memory priority: {e}");
        }
    }
}

/// The window title that tells instances apart: the config dir.
pub fn instance_title() -> HSTRING {
    HSTRING::from(crate::config::dir().to_string_lossy().as_ref())
}

/// Holds a named mutex for this config dir while the bar runs; None if another bar has it.
pub fn single_instance() -> Option<HANDLE> {
    let dir = crate::config::dir().to_string_lossy().to_lowercase();
    // Mutex names can't hold backslashes
    let name = HSTRING::from(format!("Local\\RustyBar-{}", dir.replace(['\\', ':'], "_")));
    unsafe {
        let mutex = CreateMutexW(None, true, &name).ok()?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(mutex);
            return None;
        }
        Some(mutex)
    }
}

/// `rustybar quit`: asks the bar of this config dir to close, as a sign-out would.
pub fn quit_running() {
    unsafe {
        if let Ok(hwnd) = FindWindowW(CONTROL_CLASS, &instance_title()) {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Gives back to Windows the pages the heaps keep after a free (what a popup used, gone with
/// it): every heap of the process trims its caches and decommits what it can. Without this they
/// would count as the bar's memory until something reused them.
pub fn release_memory() {
    // HEAP_OPTIMIZE_RESOURCES_INFORMATION, which the `windows` crate doesn't have
    #[repr(C)]
    struct Optimize {
        version: u32,
        flags: u32,
    }
    let info = Optimize { version: 1, flags: 0 };
    unsafe {
        if let Err(e) = HeapSetInformation(None, HeapOptimizeResources, Some(&info as *const _ as *const c_void), size_of::<Optimize>()) {
            log!("release memory: {e}");
        }
    }
}

/// Opens a URL (a page, a `scheme:` link) as a click in Explorer would: with the app Windows has
/// for it, which may then come in front.
pub fn open(target: &str) {
    unsafe {
        // The app that opens it is another process: it may take the front from this one
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        let result = ShellExecuteW(None, w!("open"), &HSTRING::from(target), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
        // Above 32 is success
        if result.0 as isize <= 32 {
            log!("can't open {target}: error {}", result.0 as isize);
        }
    }
}

/// Prints go to the terminal that started `rustybar check`, even though the binary is a GUI one.
pub fn attach_console() {
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
