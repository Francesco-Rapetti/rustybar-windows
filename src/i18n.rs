//! Texts in Italian or English, as the language of Windows is.

use std::sync::OnceLock;

use windows::Win32::Globalization::GetUserDefaultUILanguage;

/// Whether Windows speaks Italian (any region).
pub fn italian() -> bool {
    static ITALIAN: OnceLock<bool> = OnceLock::new();
    // The primary language is the low 10 bits of the LANGID; 0x10 is Italian
    *ITALIAN.get_or_init(|| unsafe { GetUserDefaultUILanguage() } & 0x3ff == 0x10)
}

/// `t("italiano", "english")`
pub fn t<'a>(it: &'a str, en: &'a str) -> &'a str {
    if italian() { it } else { en }
}
