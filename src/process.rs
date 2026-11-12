//! The process makes itself small before anything else: below normal priority, EcoQoS (on a
//! hybrid CPU the scheduler keeps it on the efficient cores), low memory priority, and per-monitor
//! DPI so that every screen draws at its own scale. One bar per config dir.

use std::ffi::c_void;

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, LPARAM, WAIT_TIMEOUT, WPARAM};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::System::Memory::{HeapOptimizeResources, HeapSetInformation};
use windows::Win32::System::Threading::{
    BELOW_NORMAL_PRIORITY_CLASS, CreateMutexW, GetCurrentProcess, MEMORY_PRIORITY_INFORMATION, MEMORY_PRIORITY_LOW,
    PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
    ProcessMemoryPriority, ProcessPowerThrottling, SetPriorityClass, SetProcessInformation, WaitForSingleObject,
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
    quit(&crate::config::dir(), 0);
}

/// Asks the bar of config dir `dir` to close and waits up to `wait_ms` for its window to go.
/// True if a bar was running.
pub fn quit(dir: &std::path::Path, wait_ms: u64) -> bool {
    let title = HSTRING::from(dir.to_string_lossy().as_ref());
    unsafe {
        let Ok(hwnd) = FindWindowW(CONTROL_CLASS, &title) else { return false };
        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        let start = std::time::Instant::now();
        while start.elapsed().as_millis() < wait_ms as u128 && FindWindowW(CONTROL_CLASS, &title).is_ok() {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    true
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

/// What a child printed, and whether it ended well.
pub struct Output {
    pub ok: bool,
    /// The exit code, -1 when there's none (it couldn't start, or it was ended).
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Flags of `CreateProcess`: no console window, idle priority.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
pub const IDLE_PRIORITY_CLASS: u32 = 0x0000_0040;

/// The job every child goes in: it ends them if the bar ends first.
fn job() -> Option<HANDLE> {
    struct Job(HANDLE);
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}
    static JOB: std::sync::OnceLock<Option<Job>> = std::sync::OnceLock::new();
    JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(None, PCWSTR::null()).ok()?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(job, JobObjectExtendedLimitInformation, &limits as *const _ as *const _, size_of_val(&limits) as u32).ok()?;
        Some(Job(job))
    })
    .as_ref()
    .map(|j| j.0)
}

/// Runs `command` as small as it can be: no window, idle priority, EcoQoS, low memory priority, in
/// the job that ends it with the bar, `input` on its stdin, at most `timeout` seconds; its pipes
/// are read while it runs, so a long answer never blocks it. For a worker thread: it waits.
/// `label` names it in the log.
pub fn run(mut command: std::process::Command, input: Option<&str>, timeout: u32, label: &str) -> Output {
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    command
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW | IDLE_PRIORITY_CLASS);
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => return Output { ok: false, code: -1, stdout: String::new(), stderr: e.to_string() },
    };
    let handle = HANDLE(child.as_raw_handle());
    make_small(handle);
    if let Some(job) = job() {
        unsafe {
            let _ = AssignProcessToJobObject(job, handle);
        }
    }
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        use std::io::Write;
        let _ = stdin.write_all(input.as_bytes());
    }
    let read = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::Builder::new().stack_size(64 * 1024).spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut bytes);
            }
            String::from_utf8_lossy(&bytes).into_owned()
        })
    };
    let stdout = read(child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>));
    let stderr = read(child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>));
    if unsafe { WaitForSingleObject(handle, timeout.saturating_mul(1000)) } == WAIT_TIMEOUT {
        log!("{label}: took more than {timeout} s");
        let _ = child.kill();
    }
    let status = child.wait().ok();
    let text = |t: std::io::Result<std::thread::JoinHandle<String>>| t.ok().and_then(|h| h.join().ok()).unwrap_or_default();
    Output { ok: status.is_some_and(|s| s.success()), code: status.and_then(|s| s.code()).unwrap_or(-1), stdout: text(stdout), stderr: text(stderr) }
}

/// Prints go to the terminal that started `rustybar check`, even though the binary is a GUI one.
pub fn attach_console() {
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
