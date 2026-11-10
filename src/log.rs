//! A small log file, `%LOCALAPPDATA%\rustybar\rustybar.log` (one per config dir name when
//! `RUSTYBAR_CONFIG_DIR` is set), started over when it passes 1 MB.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use windows::Win32::System::SystemInformation::GetLocalTime;

static FILE: Mutex<Option<File>> = Mutex::new(None);

pub fn path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let name = match std::env::var_os("RUSTYBAR_CONFIG_DIR") {
        Some(dir) => format!(
            "rustybar-{}.log",
            PathBuf::from(dir).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        ),
        None => "rustybar.log".into(),
    };
    base.join("rustybar").join(name)
}

pub fn init() {
    let path = path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let fresh = std::fs::metadata(&path).map(|m| m.len() > 1 << 20).unwrap_or(true);
    let file = OpenOptions::new().create(true).append(!fresh).write(true).truncate(fresh).open(&path).ok();
    *FILE.lock().unwrap_or_else(|e| e.into_inner()) = file;
}

pub fn write(message: &str) {
    let t = unsafe { GetLocalTime() };
    let line = format!("[{:02}:{:02}:{:02}.{:03}] {message}\n", t.wHour, t.wMinute, t.wSecond, t.wMilliseconds);
    if let Some(file) = FILE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        let _ = file.write_all(line.as_bytes());
    }
}

/// `log!("format {}", args)`: a line in the log file, with the time.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::log::write(&format!($($arg)*)) };
}
