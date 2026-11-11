//! Secrets in Windows' Credential Manager, as generic credentials named `rustybar-<name>` (Control
//! Panel > Credential Manager > Windows Credentials shows them): the token of Home Assistant, the
//! secret addresses of calendars. `rustybar secret set <name>` writes one from stdin, so it never
//! goes through the command line or a file; the widgets read theirs once, and keep it.

use windows::Win32::Foundation::ERROR_NOT_FOUND;
use windows::Win32::Security::Credentials::{
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW, CredWriteW,
};
use windows::core::{HSTRING, PWSTR};

fn target(name: &str) -> HSTRING {
    HSTRING::from(format!("rustybar-{name}"))
}

/// The secret `name`, if there's one.
pub fn get(name: &str) -> Option<String> {
    unsafe {
        let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
        CredReadW(&target(name), CRED_TYPE_GENERIC, None, &mut credential).ok()?;
        let c = &*credential;
        let bytes = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize);
        let value = String::from_utf8_lossy(bytes).into_owned();
        CredFree(credential as *const _);
        Some(value).filter(|v| !v.is_empty())
    }
}

/// Saves the secret `name`, for this user on this PC (not roamed with the profile).
pub fn set(name: &str, value: &str) -> windows::core::Result<()> {
    let mut target: Vec<u16> = format!("rustybar-{name}").encode_utf16().chain([0]).collect();
    let mut user: Vec<u16> = "rustybar".encode_utf16().chain([0]).collect();
    let mut blob = value.as_bytes().to_vec();
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target.as_mut_ptr()),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: PWSTR(user.as_mut_ptr()),
        ..Default::default()
    };
    unsafe { CredWriteW(&credential, 0) }
}

/// Removes the secret `name`; true if there was one.
pub fn delete(name: &str) -> windows::core::Result<bool> {
    match unsafe { CredDeleteW(&target(name), CRED_TYPE_GENERIC, None) } {
        Ok(()) => Ok(true),
        Err(e) if e.code() == ERROR_NOT_FOUND.to_hresult() => Ok(false),
        Err(e) => Err(e),
    }
}

/// `rustybar secret set|delete|check <name>`: the exit code.
pub fn command(args: &[String]) -> i32 {
    use std::io::{Read, Write};
    let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
    match args {
        [verb, name] if verb == "set" => {
            let mut value = String::new();
            if std::io::stdin().read_to_string(&mut value).is_err() || value.trim().is_empty() {
                let _ = writeln!(err, "nothing on stdin: pipe the secret in, e.g. Get-Clipboard | rustybar secret set {name}");
                return 1;
            }
            match set(name, value.trim()) {
                Ok(()) => {
                    let _ = writeln!(out, "saved rustybar-{name} in Credential Manager");
                    0
                }
                Err(e) => {
                    let _ = writeln!(err, "can't save it: {e}");
                    1
                }
            }
        }
        [verb, name] if verb == "delete" => match delete(name) {
            Ok(found) => {
                let _ = writeln!(out, "{}", if found { "deleted" } else { "there was none" });
                0
            }
            Err(e) => {
                let _ = writeln!(err, "{e}");
                1
            }
        },
        // Whether it's there, without showing it
        [verb, name] if verb == "check" => {
            let found = get(name).is_some();
            let _ = writeln!(out, "rustybar-{name}: {}", if found { "saved" } else { "none" });
            if found { 0 } else { 1 }
        }
        _ => {
            let _ = writeln!(
                err,
                "usage: rustybar secret set <name>     (the secret on stdin)\n       rustybar secret delete <name>\n       rustybar secret check <name>"
            );
            2
        }
    }
}
