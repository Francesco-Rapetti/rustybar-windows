//! The icons of apps, for `workspaces`: drawn at the size they're shown, in premultiplied BGRA.
//! Where they come from, cheapest first, and none sends a message to the app (a game's window
//! took 77 ms to answer `WM_GETICON`):
//!
//! - the executable's own icon, at the exact size in pixels (`PrivateExtractIconsW`, ~0.4 ms: it
//!   reads the file's resources);
//! - the icon of the window's class (`GetClassLongPtrW`, a few µs), for an executable without
//!   icons (most apps of the Store);
//! - the logo in the package of a UWP or packaged app: its manifest names it, the file of the
//!   right size and look is next to it, WIC decodes it (1–15 ms, +0.2 MB the first time, on a
//!   worker thread: `logo`). The Apps folder of Start would know it too, but it cost 109 ms and
//!   4.2 MB that stayed.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{CloseHandle, HWND, PROPERTYKEY};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDIBits, HBITMAP, HDC,
};
use windows::Win32::Storage::Packaging::Appx::{GetApplicationUserModelId, GetPackagePathByFullName, GetPackagesByPackageFamily};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, GCLP_HICON, GCLP_HICONSM, GetClassLongPtrW, GetIconInfo, HICON, ICONINFO, PrivateExtractIconsW,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

use crate::ui::{Image, image_id};

/// The property with a window's app user model id (`PKEY_AppUserModel_ID`).
const APP_ID: PROPERTYKEY = PROPERTYKEY { fmtid: windows::core::GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3), pid: 5 };

/// The icon of the executable at `path`, `px` pixels on a side.
pub fn from_exe(path: &str, px: u32) -> Option<Image> {
    let wide: Vec<u16> = path.encode_utf16().collect();
    if wide.len() >= 260 {
        return None;
    }
    let mut name = [0u16; 260];
    name[..wide.len()].copy_from_slice(&wide);
    let mut icons = [HICON::default()];
    let n = unsafe { PrivateExtractIconsW(&name, 0, px as i32, px as i32, Some(&mut icons), None, 0) };
    if n == 0 || n == u32::MAX || icons[0].is_invalid() {
        return None;
    }
    let image = pixels(icons[0]);
    unsafe {
        let _ = DestroyIcon(icons[0]);
    }
    image
}

/// The icon of the window's class: the small one for small sizes, else the big one. The icon is
/// the app's, shared: it's read, never destroyed.
pub fn from_class(hwnd: HWND, px: u32) -> Option<Image> {
    let order = if px <= 20 { [GCLP_HICONSM, GCLP_HICON] } else { [GCLP_HICON, GCLP_HICONSM] };
    order.into_iter().find_map(|which| {
        let icon = unsafe { GetClassLongPtrW(hwnd, which) };
        (icon != 0).then(|| pixels(HICON(icon as *mut _))).flatten()
    })
}

/// An icon's pixels, premultiplied. An icon without an alpha channel takes its transparency
/// from its mask.
fn pixels(icon: HICON) -> Option<Image> {
    unsafe {
        let mut info = ICONINFO::default();
        GetIconInfo(icon, &mut info).ok()?;
        let dc = CreateCompatibleDC(None);
        let color = (!info.hbmColor.is_invalid()).then(|| bits(dc, info.hbmColor)).flatten();
        let mask = (!info.hbmMask.is_invalid()).then(|| bits(dc, info.hbmMask)).flatten();
        let _ = DeleteDC(dc);
        for b in [info.hbmColor, info.hbmMask] {
            if !b.is_invalid() {
                let _ = DeleteObject(b.into());
            }
        }
        let (width, height, mut px) = color?;
        if !px.as_chunks::<4>().0.iter().any(|p| p[3] != 0) {
            // No alpha: where the mask is white the icon is transparent
            let (mw, _, mask) = mask?;
            if mw != width {
                return None;
            }
            for (p, m) in px.as_chunks_mut::<4>().0.iter_mut().zip(mask.as_chunks::<4>().0) {
                p[3] = if m[0] == 0 { 255 } else { 0 };
            }
        }
        for p in px.as_chunks_mut::<4>().0 {
            let a = p[3] as u32;
            for c in &mut p[..3] {
                *c = ((*c as u32 * a + 127) / 255) as u8;
            }
        }
        (width > 0 && height > 0).then(|| Image { id: image_id(), width, height, pixels: px })
    }
}

/// A bitmap's pixels as 32-bit BGRA rows from the top.
fn bits(dc: HDC, bitmap: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        if GetDIBits(dc, bitmap, 0, 0, None, &mut bmi, DIB_RGB_COLORS) == 0 {
            return None;
        }
        let (w, h) = (bmi.bmiHeader.biWidth, bmi.bmiHeader.biHeight.abs());
        if w <= 0 || h <= 0 || w > 512 || h > 1024 {
            return None;
        }
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;
        bmi.bmiHeader.biHeight = -h;
        let mut px = vec![0u8; (w * h * 4) as usize];
        if GetDIBits(dc, bitmap, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &mut bmi, DIB_RGB_COLORS) == 0 {
            return None;
        }
        Some((w as u32, h as u32, px))
    }
}

/// The app user model id of a UWP app's frame (`Family!App`), from the window's properties.
pub fn frame_app_id(hwnd: HWND) -> Option<String> {
    unsafe {
        let store: IPropertyStore = SHGetPropertyStoreForWindow(hwnd).ok()?;
        let value = store.GetValue(&APP_ID).ok()?;
        let text = PropVariantToStringAlloc(&value).ok()?;
        let id = text.to_string().ok();
        CoTaskMemFree(Some(text.0 as *const _));
        id.filter(|s| s.contains('!'))
    }
}

/// The app user model id of a packaged process, or None for an app that isn't packaged.
pub fn process_app_id(pid: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let result = GetApplicationUserModelId(process, &mut len, Some(PWSTR(buf.as_mut_ptr())));
        let _ = CloseHandle(process);
        result.ok().ok()?;
        let id = String::from_utf16_lossy(&buf[..(len as usize).saturating_sub(1)]);
        id.contains('!').then_some(id)
    }
}

/// The logo of the packaged app `app_id`, `px` pixels on a side; `light` for a light theme (the
/// logo drawn for light backgrounds, when the package has one). On a worker thread: WIC decodes.
pub fn logo(app_id: &str, px: u32, light: bool) -> Option<Image> {
    let (family, app) = app_id.split_once('!')?;
    let dir = package_dir(family)?;
    let manifest = std::fs::read_to_string(dir.join("AppxManifest.xml")).ok()?;
    let file = logo_file(&dir, &manifest_logo(&manifest, app)?, px, light)?;
    super::media::decode_file(&file, px, px)
}

/// Where the package of a family is installed (its first package).
fn package_dir(family: &str) -> Option<PathBuf> {
    unsafe {
        let family = HSTRING::from(family);
        let (mut count, mut len) = (0u32, 0u32);
        let _ = GetPackagesByPackageFamily(&family, &mut count, None, &mut len, None);
        if count == 0 || len == 0 {
            return None;
        }
        let mut names = vec![PWSTR::null(); count as usize];
        let mut buf = vec![0u16; len as usize];
        GetPackagesByPackageFamily(&family, &mut count, Some(names.as_mut_ptr()), &mut len, Some(PWSTR(buf.as_mut_ptr()))).ok().ok()?;
        let full = PCWSTR(names.first()?.0);
        let mut plen = 0u32;
        let _ = GetPackagePathByFullName(full, &mut plen, None);
        if plen == 0 {
            return None;
        }
        let mut path = vec![0u16; plen as usize];
        GetPackagePathByFullName(full, &mut plen, Some(PWSTR(path.as_mut_ptr()))).ok().ok()?;
        Some(PathBuf::from(String::from_utf16_lossy(&path[..(plen as usize).saturating_sub(1)])))
    }
}

/// The `Square44x44Logo` of the application `app` in a package's manifest (`Assets\Logo.png`):
/// the small logo the taskbar and Start show.
fn manifest_logo(manifest: &str, app: &str) -> Option<String> {
    let mut rest = manifest;
    while let Some(at) = rest.find("<Application ") {
        rest = &rest[at + 1..];
        let tag = &rest[..rest.find('>').unwrap_or(rest.len())];
        if attribute(tag, "Id").is_some_and(|id| id.eq_ignore_ascii_case(app)) {
            // The logo is an attribute of VisualElements, inside this application
            let end = rest.find("</Application>").unwrap_or(rest.len());
            let inside = &rest[..end];
            let key = "Square44x44Logo=\"";
            let i = inside.find(key)? + key.len();
            return Some(inside[i..i + inside[i..].find('"')?].to_string());
        }
    }
    None
}

/// An attribute's value in a tag's text.
fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!(" {name}=\"");
    let i = tag.find(&key)? + key.len();
    Some(&tag[i..i + tag[i..].find('"')?])
}

/// The file of a logo to draw at `px` pixels: the manifest names `Assets\Logo.png`, the package
/// has `Logo.targetsize-16_altform-unplated.png`, `Logo.scale-100.png` and so on. The best one:
/// a target size (without the tile's plate: the taskbar's look; for a light theme the light one
/// if there is one), as small as is at least `px`, else the largest; else a scale, the plated
/// tile; never one for high contrast.
fn logo_file(dir: &Path, logo: &str, px: u32, light: bool) -> Option<PathBuf> {
    let path = dir.join(logo.replace('/', "\\"));
    if path.is_file() && !logo.contains('.') {
        return Some(path);
    }
    let folder = path.parent()?;
    let stem = path.file_stem()?.to_string_lossy().to_lowercase();
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    let mut best: Option<(i64, PathBuf)> = None;
    for entry in std::fs::read_dir(folder).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        let Some(qualifiers) = name.strip_prefix(&format!("{stem}.")).and_then(|r| r.strip_suffix(&format!(".{ext}"))) else {
            if name == format!("{stem}.{ext}") && best.is_none() {
                best = Some((i64::MIN + 1, entry.path()));
            }
            continue;
        };
        if let Some(score) = logo_score(qualifiers, px, light)
            && best.as_ref().is_none_or(|(s, _)| score > *s)
        {
            best = Some((score, entry.path()));
        }
    }
    best.map(|(_, p)| p)
}

/// How good a logo with these qualifiers (`targetsize-16_altform-unplated`) is for `px` pixels;
/// None for one never to use.
fn logo_score(qualifiers: &str, px: u32, light: bool) -> Option<i64> {
    let (mut size, mut scale, mut unplated, mut lightlook) = (None, None, false, false);
    for q in qualifiers.split('_') {
        let (k, v) = q.split_once('-').unwrap_or((q, ""));
        match k {
            "targetsize" => size = v.parse::<i64>().ok(),
            "scale" => scale = v.parse::<i64>().ok(),
            "altform" => match v {
                "unplated" => unplated = true,
                "lightunplated" => (unplated, lightlook) = (true, true),
                _ => return None,
            },
            "contrast" => return None,
            _ => {}
        }
    }
    let px = px as i64;
    if let Some(size) = size {
        // At least the size wanted, the nearest; smaller ones after, the largest first
        let fit = if size >= px { 10_000 - (size - px) } else { size };
        let look = if !unplated { 0 } else if lightlook == light { 40_000 } else { 20_000 };
        return Some(100_000 + look + fit);
    }
    // A scale of the tile: 100 is 44 px; the nearest above what's wanted
    let scale = scale.unwrap_or(100);
    let side = 44 * scale / 100;
    Some(if side >= px { 10_000 - (side - px) } else { side })
}

/// The first letter of an app's name, for an app without an icon.
pub fn letter(name: &str) -> String {
    name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "?".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_logo_of_an_application_in_a_manifest() {
        let manifest = r#"<Package><Applications>
            <Application Id="Other" Executable="o.exe"><uap:VisualElements Square44x44Logo="Assets\Other.png"/></Application>
            <Application Id="App" Executable="a.exe">
              <uap:VisualElements DisplayName="A" Square150x150Logo="Assets\Big.png" Square44x44Logo="Assets\Small.png"/>
            </Application></Applications></Package>"#;
        assert_eq!(manifest_logo(manifest, "App").as_deref(), Some("Assets\\Small.png"));
        assert_eq!(manifest_logo(manifest, "other").as_deref(), Some("Assets\\Other.png"));
        assert_eq!(manifest_logo(manifest, "None"), None);
    }

    #[test]
    fn the_best_file_of_a_logo() {
        let best = |names: &[&str], px, light| {
            names.iter().copied().max_by_key(|n| logo_score(n, px, light).unwrap_or(i64::MIN)).map(str::to_string)
        };
        let all = ["scale-100", "scale-200", "targetsize-16", "targetsize-24", "targetsize-16_altform-unplated", "targetsize-16_altform-lightunplated", "targetsize-16_contrast-black"];
        assert_eq!(best(&all, 16, false).as_deref(), Some("targetsize-16_altform-unplated"));
        assert_eq!(best(&all, 16, true).as_deref(), Some("targetsize-16_altform-lightunplated"));
        assert_eq!(best(&["targetsize-16", "targetsize-24", "targetsize-32"], 20, false).as_deref(), Some("targetsize-24"));
        assert_eq!(best(&["targetsize-16", "targetsize-24"], 48, false).as_deref(), Some("targetsize-24"));
        assert_eq!(best(&["scale-100", "scale-200"], 16, false).as_deref(), Some("scale-100"));
        assert_eq!(logo_score("targetsize-16_contrast-white", 16, false), None);
        // The order of the qualifiers doesn't matter
        assert_eq!(logo_score("altform-unplated_targetsize-16", 16, false), logo_score("targetsize-16_altform-unplated", 16, false));
    }

    #[test]
    fn letters_of_names() {
        assert_eq!(letter("guitar pro"), "G");
        assert_eq!(letter("(x) tool"), "X");
        assert_eq!(letter(""), "?");
    }
}
