//! How a callback on another thread wakes the loop: Core Audio and the media sessions call their
//! callbacks on threads of their own, which only set an event that the widget hands to the loop
//! (`Widget::handles`); the widget reads what changed on the main thread. Shared through an `Arc`,
//! so a late callback never sets a closed handle.

use std::sync::Arc;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{CreateEventW, ResetEvent, SetEvent};
use windows::Win32::System::Threading::{CancelWaitableTimer, CreateWaitableTimerExW, SetWaitableTimerEx, TIMER_ALL_ACCESS};
use windows::core::PCWSTR;

use crate::log;

/// An auto-reset event.
pub struct Wake(HANDLE);

// A kernel handle can be signaled from any thread
unsafe impl Send for Wake {}
unsafe impl Sync for Wake {}

impl Wake {
    pub fn new() -> Arc<Wake> {
        let handle = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }.unwrap_or_else(|e| {
            log!("event: {e}");
            HANDLE::default()
        });
        Arc::new(Wake(handle))
    }

    pub fn handle(&self) -> Option<HANDLE> {
        (!self.0.is_invalid() && !self.0.0.is_null()).then_some(self.0)
    }

    pub fn set(&self) {
        if let Some(h) = self.handle() {
            unsafe {
                let _ = SetEvent(h);
            }
        }
    }

    /// Forgets a signal nobody waited for.
    pub fn reset(&self) {
        if let Some(h) = self.handle() {
            unsafe {
                let _ = ResetEvent(h);
            }
        }
    }
}

impl Drop for Wake {
    fn drop(&mut self) {
        if let Some(h) = self.handle() {
            unsafe {
                let _ = CloseHandle(h);
            }
        }
    }
}

/// A waitable timer that runs only while someone needs it: `start` (again) with a period, `stop`.
pub struct Timer {
    handle: HANDLE,
    period: Option<i32>,
}

impl Timer {
    pub fn new() -> Timer {
        let handle = unsafe { CreateWaitableTimerExW(None, PCWSTR::null(), 0, TIMER_ALL_ACCESS.0) }.unwrap_or_else(|e| {
            log!("timer: {e}");
            HANDLE::default()
        });
        Timer { handle, period: None }
    }

    fn valid(&self) -> bool {
        !self.handle.is_invalid() && !self.handle.0.is_null()
    }

    /// The handle while the timer runs.
    pub fn handle(&self) -> Option<HANDLE> {
        self.period.and(self.valid().then_some(self.handle))
    }

    /// Fires after `first` ms, then every `period` ms, each up to a tenth of the period late so
    /// Windows can fold the wake-up into others. Nothing changes if it already runs with that
    /// period.
    pub fn start(&mut self, first: i64, period: i32) {
        if self.period == Some(period) || !self.valid() {
            return;
        }
        let due = -(first.max(1) * 10_000);
        // The slack holds for the first time too: never more than half of it
        let slack = (period / 10).clamp(10, 30_000).min((first / 2).max(10) as i32) as u32;
        match unsafe { SetWaitableTimerEx(self.handle, &due, period, None, None, None, slack) } {
            Ok(()) => self.period = Some(period),
            Err(e) => log!("timer: {e}"),
        }
    }

    pub fn stop(&mut self) {
        if self.period.take().is_some() && self.valid() {
            unsafe {
                let _ = CancelWaitableTimer(self.handle);
            }
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        self.stop();
        if self.valid() {
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}
