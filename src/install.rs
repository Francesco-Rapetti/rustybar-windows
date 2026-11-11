//! `rustybar install` and `rustybar uninstall`, for the user alone and without administrator
//! rights. Install copies the running executable to `%LOCALAPPDATA%\Programs\RustyBar` (closing
//! the installed bar first, so it also updates it), writes the example config to
//! `%APPDATA%\rustybar` if there's none, starts the bar with Windows (the `Run` key: Settings >
//! Apps > Startup shows it, with its switch), lists it in Settings > Apps (whose Uninstall runs
//! `rustybar uninstall`), adds a Start menu shortcut, and starts it. Uninstall undoes all of it
//! and leaves the config and the logs.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, IPersistFile};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW,
    RegDeleteTreeW, RegSetValueExW,
};
use windows::Win32::UI::Shell::{FOLDERID_Programs, IShellLinkW, KF_FLAG_DEFAULT, SHGetKnownFolderPath, ShellLink};
use windows::core::{HSTRING, Interface, PCWSTR, w};

use crate::config;
use crate::i18n::t;
use crate::process;

/// A line for the terminal that ran the command; nothing if it went away (a closed pipe would
/// make `println!` panic, and the bar would stay half installed).
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

const NAME: &str = "RustyBar";
const RUN: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const UNINSTALL: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\RustyBar");
const URL: &str = "https://github.com/Francesco-Rapetti/rustybar-windows";
/// The example config, written for a first install.
const EXAMPLE: &str = include_str!("../config/rustybar.toml");
/// `CreateProcess`: the bar starts on its own, without the terminal that ran the command.
const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

fn install_dir() -> PathBuf {
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    local.join("Programs").join(NAME)
}

fn installed_exe() -> PathBuf {
    install_dir().join("rustybar.exe")
}

/// `%APPDATA%\Microsoft\Windows\Start Menu\Programs\RustyBar.lnk`.
fn shortcut() -> Option<PathBuf> {
    unsafe {
        let path = SHGetKnownFolderPath(&FOLDERID_Programs, KF_FLAG_DEFAULT, None).ok()?;
        let s = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as *const _));
        s.map(|s| PathBuf::from(s).join(format!("{NAME}.lnk")))
    }
}

pub fn install() -> i32 {
    let source = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return fail(&format!("{}: {e}", t("Non trovo l'eseguibile", "Can't find the executable"))),
    };
    clean_parked();
    let dir = install_dir();
    let exe = installed_exe();
    // The installed bar goes first: its file can't be replaced while it runs
    if process::quit(&config::default_dir(), 5000) {
        say!("{}", t("La barra installata è stata chiusa", "The installed bar was closed"));
    }
    if !same_file(&source, &exe) {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return fail(&format!("{}: {e}", dir.display()));
        }
        if let Err(e) = copy_retrying(&source, &exe) {
            return fail(&format!("{} {}: {e}", t("Non riesco a copiare in", "Can't copy to"), exe.display()));
        }
    }
    say!("{} {}", t("Installata in", "Installed in"), exe.display());

    let config_file = config::default_dir().join("rustybar.toml");
    if !config_file.exists() {
        let _ = std::fs::create_dir_all(config::default_dir());
        match std::fs::write(&config_file, EXAMPLE) {
            Ok(()) => say!("{} {}", t("Config di esempio in", "Example config in"), config_file.display()),
            Err(e) => say!("{} {}: {e}", t("Config non scritta:", "Config not written:"), config_file.display()),
        }
    }

    let quoted = format!("\"{}\"", exe.display());
    if set_string(HKEY_CURRENT_USER, RUN, NAME, &quoted) {
        say!("{}", t("Parte con Windows (Impostazioni > App > Avvio)", "Starts with Windows (Settings > Apps > Startup)"));
    } else {
        say!("{}", t("Non riesco a registrare l'avvio con Windows", "Can't register the start with Windows"));
    }
    if register_app(&dir, &exe) {
        say!("{}", t("Elencata in Impostazioni > App", "Listed in Settings > Apps"));
    }
    match make_shortcut(&exe) {
        Some(link) => say!("{} {}", t("Collegamento nel menu Start:", "Start menu shortcut:"), link.display()),
        None => say!("{}", t("Non riesco a creare il collegamento nel menu Start", "Can't make the Start menu shortcut")),
    }

    match start(&exe) {
        Ok(()) => say!("{}", t("Barra avviata", "Bar started")),
        Err(e) => return fail(&format!("{}: {e}", t("Non riesco ad avviare la barra", "Can't start the bar"))),
    }
    0
}

pub fn uninstall() -> i32 {
    clean_parked();
    let dir = install_dir();
    let exe = installed_exe();
    if process::quit(&config::default_dir(), 5000) {
        say!("{}", t("La barra è stata chiusa", "The bar was closed"));
    }
    unsafe {
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN, &HSTRING::from(NAME));
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, UNINSTALL);
    }
    say!("{}", t("Avvio con Windows e voce in Impostazioni > App tolti", "Start with Windows and the Settings > Apps entry removed"));
    if let Some(link) = shortcut()
        && std::fs::remove_file(&link).is_ok()
    {
        say!("{}", t("Collegamento nel menu Start tolto", "Start menu shortcut removed"));
    }
    // Run from the installed copy (Settings' Uninstall does that), the file is in use: a running
    // executable can't be deleted but can be moved, out of the folder, to the temporary files
    if let Ok(me) = std::env::current_exe()
        && same_file(&me, &exe)
    {
        let parked = std::env::temp_dir().join(format!("rustybar-uninstalled-{}.exe", std::process::id()));
        let _ = std::fs::rename(&exe, &parked);
    }
    let _ = std::fs::remove_file(&exe);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => say!("{} {}", t("Tolta", "Removed"), dir.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => say!("{} {}: {e}", t("Non riesco a togliere", "Can't remove"), dir.display()),
    }
    say!("{} {}", t("Config e log restano in", "The config and the logs stay in"), config::default_dir().display());
    0
}

/// Deletes the copies an uninstall moved to the temporary files, once nothing runs them.
fn clean_parked() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("rustybar-uninstalled-") && name.ends_with(".exe") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn fail(message: &str) -> i32 {
    say!("{message}");
    1
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Copies, trying again for a few seconds: the bar that was just closed may still hold the file.
fn copy_retrying(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut last = None;
    for _ in 0..50 {
        match std::fs::copy(from, to) {
            Ok(_) => return Ok(()),
            Err(e) => last = Some(e),
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("copy")))
}

/// Starts the installed bar on its own, without this command's terminal, and with its own config
/// dir whatever the caller's environment says.
fn start(exe: &Path) -> std::io::Result<()> {
    let spawn = |flags: u32| {
        Command::new(exe)
            .current_dir(install_dir())
            .env_remove("RUSTYBAR_CONFIG_DIR")
            .env_remove("RUSTYBAR_GPU")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(flags)
            .spawn()
            .map(|_| ())
    };
    // Out of the terminal's job, if it has one that would end its children with it; a job that
    // doesn't allow it refuses the flag, and the bar starts inside it
    spawn(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB).or_else(|_| spawn(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP))
}

fn wide(s: &str) -> Vec<u8> {
    s.encode_utf16().chain(std::iter::once(0)).flat_map(|c| c.to_le_bytes()).collect()
}

fn set_string(root: HKEY, key: PCWSTR, name: &str, value: &str) -> bool {
    with_key(root, key, |k| unsafe { RegSetValueExW(k, &HSTRING::from(name), None, REG_SZ, Some(&wide(value))) == ERROR_SUCCESS })
}

fn with_key(root: HKEY, key: PCWSTR, f: impl FnOnce(HKEY) -> bool) -> bool {
    let mut k = HKEY::default();
    unsafe {
        if RegCreateKeyExW(root, key, None, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut k, None) != ERROR_SUCCESS {
            return false;
        }
        let ok = f(k);
        let _ = RegCloseKey(k);
        ok
    }
}

/// The entry of Settings > Apps: its name, version, size and the command that uninstalls it.
fn register_app(dir: &Path, exe: &Path) -> bool {
    let size_kb = std::fs::metadata(exe).map(|m| (m.len() / 1024) as u32).unwrap_or(0);
    let today = {
        let now = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        format!("{:04}{:02}{:02}", now.wYear, now.wMonth, now.wDay)
    };
    let strings = [
        ("DisplayName", NAME.to_string()),
        ("DisplayVersion", env!("CARGO_PKG_VERSION").to_string()),
        ("Publisher", "Francesco-Rapetti".to_string()),
        ("DisplayIcon", exe.display().to_string()),
        ("InstallLocation", dir.display().to_string()),
        ("InstallDate", today),
        ("UninstallString", format!("\"{}\" uninstall", exe.display())),
        ("QuietUninstallString", format!("\"{}\" uninstall", exe.display())),
        ("URLInfoAbout", URL.to_string()),
    ];
    let dwords = [("NoModify", 1u32), ("NoRepair", 1), ("EstimatedSize", size_kb)];
    with_key(HKEY_CURRENT_USER, UNINSTALL, |k| unsafe {
        let mut ok = true;
        for (name, value) in &strings {
            ok &= RegSetValueExW(k, &HSTRING::from(*name), None, REG_SZ, Some(&wide(value))) == ERROR_SUCCESS;
        }
        for (name, value) in dwords {
            ok &= RegSetValueExW(k, &HSTRING::from(name), None, REG_DWORD, Some(&value.to_le_bytes())) == ERROR_SUCCESS;
        }
        ok
    })
}

/// A Start menu shortcut to the installed bar.
fn make_shortcut(exe: &Path) -> Option<PathBuf> {
    let link = shortcut()?;
    unsafe {
        // S_FALSE when COM was already there: fine either way
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let shell: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        shell.SetPath(&HSTRING::from(exe.as_os_str())).ok()?;
        shell.SetWorkingDirectory(&HSTRING::from(install_dir().as_os_str())).ok()?;
        shell.SetDescription(&HSTRING::from(t("Barra di stato leggera per Windows", "A light status bar for Windows"))).ok()?;
        let file: IPersistFile = shell.cast().ok()?;
        file.Save(&HSTRING::from(link.as_os_str()), true).ok()?;
    }
    Some(link)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_values() {
        assert!(installed_exe().ends_with("Programs\\RustyBar\\rustybar.exe"));
        // UTF-16 with its terminating null, as the registry wants it
        assert_eq!(wide("A"), vec![b'A', 0, 0, 0]);
        // The example config the install writes is valid
        assert!(toml::from_str::<crate::config::Config>(EXAMPLE).is_ok());
    }
}
