//! Texts in Italian or English, as the language of Windows is, and numbers as the region writes
//! them. `RUSTYBAR_LOCALE` (a locale name, `en-US`) sets both, for screenshots in another
//! language.

use std::sync::OnceLock;

use windows::Win32::Globalization::{GetLocaleInfoEx, GetUserDefaultUILanguage, LOCALE_SDECIMAL};
use windows::core::{HSTRING, PCWSTR};

fn chosen() -> Option<&'static HSTRING> {
    static CHOSEN: OnceLock<Option<HSTRING>> = OnceLock::new();
    CHOSEN.get_or_init(|| std::env::var("RUSTYBAR_LOCALE").ok().filter(|l| !l.is_empty()).map(HSTRING::from)).as_ref()
}

/// The locale that dates, times and numbers follow, for the `*Ex` functions of Windows: the
/// user's (null), or `RUSTYBAR_LOCALE`.
pub fn locale() -> PCWSTR {
    chosen().map_or(PCWSTR::null(), |l| PCWSTR(l.as_ptr()))
}

/// Whether Windows speaks Italian (any region), or `RUSTYBAR_LOCALE` does.
pub fn italian() -> bool {
    static ITALIAN: OnceLock<bool> = OnceLock::new();
    *ITALIAN.get_or_init(|| match chosen() {
        Some(l) => l.to_string().to_lowercase().starts_with("it"),
        // The primary language is the low 10 bits of the LANGID; 0x10 is Italian
        None => (unsafe { GetUserDefaultUILanguage() } & 0x3ff) == 0x10,
    })
}

/// `t("italiano", "english")`
pub fn t<'a>(it: &'a str, en: &'a str) -> &'a str {
    if italian() { it } else { en }
}

/// The region's decimal separator (`,` in Italy).
fn separator() -> &'static str {
    static SEPARATOR: OnceLock<String> = OnceLock::new();
    SEPARATOR.get_or_init(|| {
        let mut buf = [0u16; 8];
        let n = unsafe { GetLocaleInfoEx(locale(), LOCALE_SDECIMAL, Some(&mut buf)) };
        if n > 1 { String::from_utf16_lossy(&buf[..n as usize - 1]) } else { ".".into() }
    })
}

/// `v` with `digits` decimals, as the region writes it: `13,8`.
pub fn number(v: f64, digits: usize) -> String {
    let s = format!("{v:.digits$}");
    if digits == 0 { s } else { s.replace('.', separator()) }
}

/// Bytes as File Explorer counts them (by 1024): `820 MB`, `13,8 GB`, `380 GB`, `1,8 TB`.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["MB", "GB", "TB", "PB"];
    let mut v = bytes as f64 / (1024.0 * 1024.0);
    let mut unit = 0;
    while v >= 1000.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    // A decimal only where it says something: under 100 of a unit, and never for MB
    let digits = if unit > 0 && v < 99.95 { 1 } else { 0 };
    format!("{} {}", number(v, digits), UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        let sep = separator();
        assert_eq!(size(820 * 1024 * 1024), "820 MB");
        assert_eq!(size(14_817_637_171), format!("13{sep}8 GB"));
        assert_eq!(size(380 * 1024 * 1024 * 1024), "380 GB");
        assert_eq!(size(2_000_000_000_000), format!("1{sep}8 TB"));
        assert_eq!(number(3.0, 0), "3");
    }
}
