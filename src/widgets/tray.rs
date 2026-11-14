//! `tray`: the notification area's hidden icons, as the ^ by the clock of Windows 11's taskbar.
//! The pill is that chevron; a click opens Windows' own flyout of the hidden icons, down on the
//! taskbar, where the icons work as they always do (left and right click, menus, double click):
//! they're Windows', not drawn here.
//!
//! Why not the icons in the bar: a click on a tray icon is a message with the callback the app
//! gave `Shell_NotifyIcon`, which only the taskbar knows. Windows 11 doesn't tell it to anyone
//! else: its tray is XAML (no toolbar to read), `ITrayNotify` answers but lists nothing on this
//! build, and the registry (`Control Panel\NotifyIconSettings`) keeps pictures of the icons ever
//! seen, not the ones there now nor how to click them.
//!
//! How: UI Automation, on a worker thread, only at a click. The chevron is the taskbar's button
//! with the automation id `SystemTrayIcon` and the class `SystemTray.NormalButton` (its name is
//! translated: «Mostra icone nascoste»); `Invoke` on it does what a click does, and again closes
//! the flyout. The bar lets Explorer take the front first (the click is the bar's input). With
//! every icon on the taskbar, or the chevron turned off in Settings, there's no button: the log
//! says so. At rest: nothing, no handle, no timer, no thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    CUIAutomation8, IUIAutomation, IUIAutomation2, IUIAutomationElement, IUIAutomationInvokePattern, TreeScope_Descendants, UIA_AutomationIdPropertyId,
    UIA_ClassNamePropertyId, UIA_InvokePatternId,
};
use windows::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId};
use windows::core::{Interface, w};

use crate::log;
use crate::theme::Theme;
use crate::ui::{Item, Pill};

/// The chevron of Segoe Fluent Icons (`ChevronUp`), the glyph of the taskbar's button.
const CHEVRON: &str = "\u{E70E}";

/// How long UI Automation may wait for Explorer, in ms: a hung taskbar keeps the worker that long.
const TIMEOUT: u32 = 2000;

pub struct Tray {
    /// A click being worked on: another one meanwhile does nothing.
    busy: Arc<AtomicBool>,
    /// Made-up data: the click does nothing.
    mock: bool,
}

impl Tray {
    pub fn new() -> Tray {
        Tray { busy: Arc::new(AtomicBool::new(false)), mock: crate::mock::dir().is_some() }
    }

    /// Opens (or closes) the flyout of the hidden icons, on a worker thread.
    fn open(&self) {
        if self.mock || self.busy.swap(true, Ordering::AcqRel) {
            return;
        }
        // The flyout is Explorer's window: it may take the front, since the bar just had the
        // user's click
        let taskbar = unsafe { FindWindowW(w!("Shell_TrayWnd"), None) }.unwrap_or_default();
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(taskbar, Some(&mut pid)) };
        if pid != 0 {
            let _ = unsafe { AllowSetForegroundWindow(pid) };
        }
        let busy = self.busy.clone();
        let started = std::thread::Builder::new().name("tray".into()).stack_size(256 * 1024).spawn(move || {
            let t = Instant::now();
            let invoke = |chevron: &IUIAutomationElement| unsafe { chevron.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)?.Invoke() };
            match unsafe { with_chevron(invoke) } {
                Ok(true) => log!("tray: flyout of the hidden icons in {} ms", t.elapsed().as_millis()),
                Ok(false) => log!("tray: no hidden icons button on the taskbar ({} ms)", t.elapsed().as_millis()),
                Err(e) => log!("tray: the hidden icons didn't open: {e}"),
            }
            busy.store(false, Ordering::Release);
        });
        if started.is_err() {
            self.busy.store(false, Ordering::Release);
        }
    }
}

/// Finds the taskbar's chevron and does `what` with it, on a thread of the multithreaded
/// apartment: false when there's none.
unsafe fn with_chevron(what: impl FnOnce(&IUIAutomationElement) -> windows::core::Result<()>) -> windows::core::Result<bool> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let result = (|| {
            let taskbar = FindWindowW(w!("Shell_TrayWnd"), None)?;
            let uia: IUIAutomation = CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)?;
            if let Ok(uia2) = uia.cast::<IUIAutomation2>() {
                let _ = uia2.SetConnectionTimeout(TIMEOUT);
                let _ = uia2.SetTransactionTimeout(TIMEOUT);
            }
            let root = uia.ElementFromHandle(taskbar)?;
            let id = uia.CreatePropertyCondition(UIA_AutomationIdPropertyId, &VARIANT::from("SystemTrayIcon"))?;
            let class = uia.CreatePropertyCondition(UIA_ClassNamePropertyId, &VARIANT::from("SystemTray.NormalButton"))?;
            let both = uia.CreateAndCondition(&id, &class)?;
            // Not found is an error with a null element
            let Ok(chevron) = root.FindFirst(TreeScope_Descendants, &both) else { return Ok(false) };
            what(&chevron)?;
            Ok(true)
        })();
        CoUninitialize();
        result
    }
}

impl super::Widget for Tray {
    fn pill(&self, theme: &Theme) -> Option<Pill> {
        Some(Pill::new(vec![Item::icon(CHEVRON, theme.text)]))
    }

    fn clickable(&self) -> bool {
        true
    }

    fn click(&mut self, _item: Option<usize>, _compact: bool) -> bool {
        self.open();
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Finds the chevron on this PC's taskbar without invoking it, and says how long it took.
    #[test]
    #[ignore = "reads this PC's taskbar"]
    fn finds_the_chevron() {
        for _ in 0..3 {
            let t = Instant::now();
            let mut name = String::new();
            let found = unsafe { with_chevron(|c| c.CurrentName().map(|n| name = n.to_string())) }.unwrap();
            println!("found {found} «{name}» in {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
            assert!(found);
        }
    }
}
