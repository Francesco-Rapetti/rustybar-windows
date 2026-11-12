//! A small log file, `%LOCALAPPDATA%\rustybar\rustybar.log` (one per config dir name when
//! `RUSTYBAR_CONFIG_DIR` is set), started over when it passes 1 MB. The settings window writes
//! its own, `rustybar-settings.log` (`rustybar-settings-<dir>.log`).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use windows::Win32::System::SystemInformation::GetLocalTime;

static FILE: Mutex<Option<File>> = Mutex::new(None);

/// The bar's log.
pub fn path() -> PathBuf {
    path_of("")
}

/// The log of a part of RustyBar: `rustybar-<part>[-<dir>].log`, or the bar's with `""`.
pub fn path_of(part: &str) -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let mut name = String::from("rustybar");
    if !part.is_empty() {
        name.push('-');
        name.push_str(part);
    }
    if let Some(dir) = std::env::var_os("RUSTYBAR_CONFIG_DIR") {
        name.push('-');
        name.push_str(&PathBuf::from(dir).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    }
    base.join("rustybar").join(format!("{name}.log"))
}

pub fn init() {
    open(path());
}

/// The log of another part (the settings window), see `path_of`.
pub fn init_named(part: &str) {
    open(path_of(part));
}

fn open(path: PathBuf) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let fresh = std::fs::metadata(&path).map(|m| m.len() > 1 << 20).unwrap_or(true);
    let file = OpenOptions::new().create(true).append(!fresh).write(true).truncate(fresh).open(&path).ok();
    *FILE.lock().unwrap_or_else(|e| e.into_inner()) = file;
}

fn line(message: &str) -> String {
    let t = unsafe { GetLocalTime() };
    format!("[{:02}:{:02}:{:02}.{:03}] {message}\n", t.wHour, t.wMinute, t.wSecond, t.wMilliseconds)
}

pub fn write(message: &str) {
    let line = line(message);
    if let Some(file) = FILE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        let _ = file.write_all(line.as_bytes());
    }
}

/// For a panic or a fault: never waits for the lock, which the failing thread may hold itself.
pub fn write_failing(message: &str) {
    let line = line(message);
    let mut guard = match FILE.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return,
    };
    if let Some(file) = guard.as_mut() {
        let _ = file.write_all(line.as_bytes());
    }
}

/// `log!("format {}", args)`: a line in the log file, with the time.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::log::write(&format!($($arg)*)) };
}
