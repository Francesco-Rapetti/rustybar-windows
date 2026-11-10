//! The date and time, in the language and formats of Windows. It wakes up once a minute, at the
//! start of the minute (a waitable timer with some slack, so Windows can fold the wake-up into
//! others), and not at all while no bar shows.
//!
//! Options: `date`, a Windows date picture (`"ddd dd/MM"` by default: `Mar 10/11`), or `""` for
//! the time alone.

use windows::Win32::Foundation::{CloseHandle, HANDLE, SYSTEMTIME};
use windows::Win32::Globalization::{ENUM_DATE_FORMATS_FLAGS, GetDateFormatEx, GetTimeFormatEx, TIME_NOSECONDS};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{CancelWaitableTimer, CreateWaitableTimerExW, SetWaitableTimerEx, TIMER_ALL_ACCESS};
use windows::core::{HSTRING, PCWSTR};

use crate::config::Options;
use crate::log;
use crate::theme::Theme;
use crate::ui::{Item, Pill};

/// How late the minute may come, in ms, so that Windows can coalesce the wake-up.
const SLACK_MS: u32 = 250;

pub struct Clock {
    timer: HANDLE,
    date_format: Option<HSTRING>,
    date: String,
    time: String,
    running: bool,
}

impl Clock {
    pub fn new(options: &Options) -> Clock {
        let timer = unsafe { CreateWaitableTimerExW(None, PCWSTR::null(), 0, TIMER_ALL_ACCESS.0) }.unwrap_or_else(|e| {
            log!("clock timer: {e}");
            HANDLE::default()
        });
        let date_format = match options.str("date") {
            Some("") => None,
            Some(f) => Some(HSTRING::from(f)),
            None => Some(HSTRING::from("ddd dd/MM")),
        };
        let mut clock = Clock { timer, date_format, date: String::new(), time: String::new(), running: false };
        clock.refresh();
        clock.start();
        clock
    }

    /// Reads the time again. True if what the pill shows changed.
    fn refresh(&mut self) -> bool {
        let now = unsafe { GetLocalTime() };
        let date = self.date_format.as_ref().map(|f| capitalize(&format_date(&now, f))).unwrap_or_default();
        let time = format_time(&now);
        let changed = date != self.date || time != self.time;
        self.date = date;
        self.time = time;
        changed
    }

    /// Arms the timer for the start of the next minute.
    fn start(&mut self) {
        if self.timer.is_invalid() || self.timer.0.is_null() {
            return;
        }
        let now = unsafe { GetLocalTime() };
        let into_minute = now.wSecond as i64 * 1000 + now.wMilliseconds as i64;
        // Relative times are negative, in 100 ns; a few ms past the minute, never just before it
        let due = -((60_000 - into_minute + 5) * 10_000);
        match unsafe { SetWaitableTimerEx(self.timer, &due, 0, None, None, None, SLACK_MS) } {
            Ok(()) => self.running = true,
            Err(e) => log!("clock timer: {e}"),
        }
    }

    fn stop(&mut self) {
        if self.running {
            unsafe {
                let _ = CancelWaitableTimer(self.timer);
            }
            self.running = false;
        }
    }
}

impl Drop for Clock {
    fn drop(&mut self) {
        if !self.timer.0.is_null() {
            unsafe {
                let _ = CloseHandle(self.timer);
            }
        }
    }
}

impl super::Widget for Clock {
    fn pill(&self, theme: &Theme) -> Option<Pill> {
        let mut items = Vec::new();
        if !self.date.is_empty() {
            items.push(Item::text(&self.date, theme.text));
        }
        items.push(Item::text(&self.time, theme.text));
        Some(Pill::new(items).gap(10.0))
    }

    fn handle(&self) -> Option<HANDLE> {
        self.running.then_some(self.timer)
    }

    fn signaled(&mut self) -> bool {
        let changed = self.refresh();
        self.start();
        changed
    }

    fn event(&mut self, signal: &super::Signal) -> bool {
        match signal {
            super::Signal::Resume | super::Signal::TimeChanged => {
                let changed = self.refresh();
                if self.running {
                    self.start();
                }
                changed
            }
            super::Signal::Shown(true) => {
                let changed = self.refresh();
                self.start();
                changed
            }
            super::Signal::Shown(false) => {
                self.stop();
                false
            }
            super::Signal::Foreground(_) => false,
        }
    }
}

fn format_date(t: &SYSTEMTIME, picture: &HSTRING) -> String {
    let mut buf = [0u16; 80];
    let n = unsafe { GetDateFormatEx(PCWSTR::null(), ENUM_DATE_FORMATS_FLAGS(0), Some(t), picture, Some(&mut buf), PCWSTR::null()) };
    // The count includes the terminating null
    String::from_utf16_lossy(&buf[..(n.max(1) - 1) as usize]).replace('.', "")
}

fn format_time(t: &SYSTEMTIME) -> String {
    let mut buf = [0u16; 40];
    let n = unsafe { GetTimeFormatEx(PCWSTR::null(), TIME_NOSECONDS, Some(t), PCWSTR::null(), Some(&mut buf)) };
    String::from_utf16_lossy(&buf[..(n.max(1) - 1) as usize])
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn capitalize() {
        assert_eq!(super::capitalize("mar 10/11"), "Mar 10/11");
        assert_eq!(super::capitalize(""), "");
    }
}
