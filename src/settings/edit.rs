//! The text box over a field of the settings window while it's being edited: Windows' own EDIT
//! control, so typing works as everywhere (IME, clipboard, undo, its context menu). The window
//! draws with DirectComposition and has no GDI surface of its own, where a child control would be
//! invisible, so the box lives in a small window of its own owned by it, over the field, in the
//! field's colors. Enter, Tab and losing the keyboard keep the text; Esc puts it back.

use std::cell::Cell;
use std::sync::Once;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET, DeleteObject, FF_DONTCARE, FW_NORMAL, HBRUSH, HDC,
    HFONT, OUT_DEFAULT_PRECIS, SetBkColor, SetTextColor,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE, VK_RETURN, VK_TAB};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_WNDPROC, GetWindowTextLengthW, GetWindowTextW, IDC_ARROW, LoadCursorW,
    RegisterClassExW, SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW, SetWindowLongPtrW, SetWindowPos, ShowWindow, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_CHAR, WM_COMMAND, WM_CTLCOLOREDIT, WM_KEYDOWN, WM_MOUSEWHEEL, WM_SETFONT, WNDCLASSEXW, WNDPROC, WS_CHILD, WS_POPUP, WS_VISIBLE,
};
use windows::core::{HSTRING, PCWSTR, w};

use super::{Ev, push};
use crate::theme::Rgba;

const HOST_CLASS: PCWSTR = w!("RustyBarSettingsEdit");
// The EDIT control's messages and styles (in `Win32_UI_Controls`, which the bar doesn't take)
const ES_PASSWORD: u32 = 0x0020;
const ES_AUTOHSCROLL: u32 = 0x0080;
const EM_SETSEL: u32 = 0x00B1;
const EM_SETMARGINS: u32 = 0x00D3;
const EC_LEFTMARGIN: usize = 1;
const EC_RIGHTMARGIN: usize = 2;
const EN_KILLFOCUS: u32 = 0x0200;

thread_local! {
    /// The EDIT control's own window procedure, which the subclass hands everything else to.
    static EDIT_PROC: Cell<isize> = const { Cell::new(0) };
    /// The colors of the open box: text, background, and the brush of the background.
    static COLORS: Cell<(u32, u32, isize)> = const { Cell::new((0, 0, 0)) };
}

fn colorref(c: Rgba) -> COLORREF {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    COLORREF(b(c.0) | b(c.1) << 8 | b(c.2) << 16)
}

/// The box the user types in.
pub struct Edit {
    pub host: HWND,
    edit: HWND,
    font: HFONT,
    brush: HBRUSH,
    /// The text it opened with.
    pub original: String,
}

unsafe extern "system" fn host_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CTLCOLOREDIT => {
            let (text, back, brush) = COLORS.get();
            let dc = HDC(wparam.0 as *mut _);
            unsafe {
                SetTextColor(dc, COLORREF(text));
                SetBkColor(dc, COLORREF(back));
            }
            return LRESULT(brush);
        }
        WM_COMMAND if (wparam.0 >> 16) as u32 == EN_KILLFOCUS => {
            push(Ev::EditDone { host: hwnd, keep: true });
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// The EDIT control, subclassed for the keys that end the editing and the wheel.
unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let host = unsafe { windows::Win32::UI::WindowsAndMessaging::GetParent(hwnd) }.unwrap_or_default();
    match msg {
        WM_KEYDOWN if wparam.0 as u16 == VK_RETURN.0 || wparam.0 as u16 == VK_TAB.0 => {
            push(Ev::EditDone { host, keep: true });
            return LRESULT(0);
        }
        WM_KEYDOWN if wparam.0 as u16 == VK_ESCAPE.0 => {
            push(Ev::EditDone { host, keep: false });
            return LRESULT(0);
        }
        // Without this the control beeps at Enter, Tab and Esc
        WM_CHAR if matches!(wparam.0 as u16, 13 | 9 | 27) => return LRESULT(0),
        // The page scrolls under it: the text is kept and the box goes
        WM_MOUSEWHEEL => {
            push(Ev::EditDone { host, keep: true });
            push(Ev::Wheel(((wparam.0 >> 16) & 0xffff) as i16 as i32));
            return LRESULT(0);
        }
        _ => {}
    }
    let previous: WNDPROC = unsafe { std::mem::transmute(EDIT_PROC.get()) };
    unsafe { CallWindowProcW(previous, hwnd, msg, wparam, lparam) }
}

fn register() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(host_proc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: HOST_CLASS,
            ..Default::default()
        };
        RegisterClassExW(&class);
    });
}

/// How the box looks: the field's colors, the text's height and the room on its sides, in pixels.
#[derive(Clone, Copy)]
pub struct Style {
    pub text: Rgba,
    pub back: Rgba,
    pub font_px: i32,
    pub pad_px: i32,
}

impl Edit {
    /// A box over `screen` (the field inside its border, in screen pixels), owned by `owner`,
    /// with `value` selected; `secret` hides what's typed.
    pub fn open(owner: HWND, screen: RECT, value: &str, secret: bool, look: &Style) -> windows::core::Result<Edit> {
        let Style { text, back, font_px, pad_px } = *look;
        register();
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let (w, h) = (screen.right - screen.left, screen.bottom - screen.top);
            let host = CreateWindowExW(WINDOW_EX_STYLE(0), HOST_CLASS, PCWSTR::null(), WS_POPUP, screen.left, screen.top, w, h, Some(owner), None, Some(instance.into()), None)?;
            let style = WS_CHILD | WS_VISIBLE | WINDOW_STYLE(ES_AUTOHSCROLL | if secret { ES_PASSWORD } else { 0 });
            // One line of text, centered up and down in the field
            let line = (font_px as f32 * 1.35).round() as i32;
            let edit = CreateWindowExW(WINDOW_EX_STYLE(0), w!("EDIT"), &HSTRING::from(value), style, 0, (h - line) / 2, w, line, Some(host), None, Some(instance.into()), None)?;
            let font = CreateFontW(
                -font_px,
                0,
                0,
                0,
                FW_NORMAL.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                FF_DONTCARE.0 as u32,
                w!("Segoe UI Variable Text"),
            );
            SendMessageW(edit, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(0)));
            SendMessageW(edit, EM_SETMARGINS, Some(WPARAM(EC_LEFTMARGIN | EC_RIGHTMARGIN)), Some(LPARAM((pad_px & 0xffff | (pad_px << 16)) as isize)));
            SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
            let brush = CreateSolidBrush(colorref(back));
            COLORS.set((colorref(text).0, colorref(back).0, brush.0 as isize));
            let previous = SetWindowLongPtrW(edit, GWLP_WNDPROC, edit_proc as *const () as isize);
            EDIT_PROC.set(previous);
            let _ = ShowWindow(host, SW_SHOW);
            let _ = SetFocus(Some(edit));
            Ok(Edit { host, edit, font, brush, original: value.to_string() })
        }
    }

    pub fn text(&self) -> String {
        unsafe {
            let n = GetWindowTextLengthW(self.edit);
            let mut buf = vec![0u16; n as usize + 1];
            let got = GetWindowTextW(self.edit, &mut buf);
            String::from_utf16_lossy(&buf[..got.max(0) as usize])
        }
    }

    /// Follows the field when the window moves.
    pub fn place(&self, screen: RECT) {
        unsafe {
            let _ = SetWindowPos(self.host, None, screen.left, screen.top, 0, 0, SWP_NOACTIVATE | SWP_NOZORDER | windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE);
        }
    }
}

impl Drop for Edit {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.host);
            let _ = DeleteObject(self.font.into());
            let _ = DeleteObject(self.brush.into());
        }
    }
}
