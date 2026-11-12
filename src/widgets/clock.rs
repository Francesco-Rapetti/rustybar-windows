//! The date and time, in the language and formats of Windows, and a calendar in the popup. It
//! wakes up once a minute, at the start of the minute (a waitable timer with some slack, so
//! Windows can fold the wake-up into others), and not at all while no bar shows.
//!
//! The calendar shows a month at a time: today in a dot of the primary color, the week starting
//! on the region's first day, the weekend dimmer. ‹ ›, the wheel and the arrow keys go through the
//! months, "Today" back to this one; a click on a day opens it in the calendar of `open`. What the
//! popup needs (the month shown, the names of the days) is made when it opens and dropped when it
//! closes.
//!
//! Options: `date`, a Windows date picture (`"ddd dd/MM"` by default: `Mar 10/11`), or `""` for
//! the time alone; `open`, the page a click on a day opens, with `{year}`, `{month}` and `{day}`
//! in it (Google Calendar's day view by default), or `""` for days that can't be clicked;
//! `timezone`, for a clock of another place: a time zone of Windows by its key name
//! (`Tokyo Standard Time`) or by its IANA name (`Asia/Tokyo`, `UTC`), through a table of CLDR's
//! mapping that the binary carries.

use std::ffi::c_void;

use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, SYSTEMTIME};
use windows::Win32::Globalization::{
    DATE_LONGDATE, ENUM_DATE_FORMATS_FLAGS, GetDateFormatEx, GetLocaleInfoEx, GetTimeFormatEx, LOCALE_IFIRSTDAYOFWEEK,
    LOCALE_SLONGDATE, LOCALE_SSHORTESTDAYNAME1, TIME_NOSECONDS,
};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_BINARY, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
use windows::Win32::System::Threading::{CancelWaitableTimer, CreateWaitableTimerExW, SetWaitableTimerEx, TIMER_ALL_ACCESS};
use windows::Win32::System::Time::{DYNAMIC_TIME_ZONE_INFORMATION, SystemTimeToTzSpecificLocalTimeEx};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_HOME, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RIGHT, VK_UP};
use windows::core::{HSTRING, PCWSTR, w};

use super::{Input, Reply};
use crate::config::Options;
use crate::i18n::t;
use crate::log;
use crate::process;
use crate::theme::Theme;
use crate::ui::{Align, Font, Fonts, Item, Pill, Rect, Scene, Weight};

/// How late the minute may come, in ms, so that Windows can coalesce the wake-up.
const SLACK_MS: u32 = 250;

/// Google Calendar's view of one day.
const OPEN: &str = "https://calendar.google.com/calendar/r/day/{year}/{month}/{day}";

// The calendar's layout, in DIPs: a top row with the date and "Today", the month between ‹ and ›,
// the days of the week, and always six weeks, so the popup keeps its size from month to month.
const PAD: f32 = 12.0;
const CELL_W: f32 = 32.0;
const CELL_H: f32 = 28.0;
const GAP: f32 = 2.0;
const GRID_W: f32 = 7.0 * CELL_W + 6.0 * GAP;
const WIDTH: f32 = GRID_W + 2.0 * PAD;
const ROW_H: f32 = 28.0;
const TOP_Y: f32 = PAD;
const MONTH_Y: f32 = TOP_Y + ROW_H + 4.0;
const DAYS_Y: f32 = MONTH_Y + ROW_H + 6.0;
const DAYS_H: f32 = 20.0;
const GRID_Y: f32 = DAYS_Y + DAYS_H + 4.0;
const HEIGHT: f32 = GRID_Y + 6.0 * CELL_H + 5.0 * GAP + PAD;
/// Segoe Fluent Icons' chevrons.
const PREVIOUS: &str = "\u{E76B}";
const NEXT: &str = "\u{E76C}";

pub struct Clock {
    timer: HANDLE,
    date_format: Option<HSTRING>,
    date: String,
    time: String,
    running: bool,
    open: String,
    /// The time zone of `timezone`; None for Windows' own.
    zone: Option<Box<DYNAMIC_TIME_ZONE_INFORMATION>>,
    /// While the popup is open.
    calendar: Option<Calendar>,
}

/// What the open popup needs.
struct Calendar {
    /// The month shown, as year * 12 + month - 1.
    month: i32,
    /// The first day of the week, as the region has it: 0 = Monday … 6 = Sunday.
    first_weekday: u32,
    /// The shortest names of the days, Monday first.
    names: [String; 7],
}

impl Calendar {
    fn new(now: &SYSTEMTIME) -> Calendar {
        Calendar { month: month_index(now), first_weekday: first_weekday(), names: std::array::from_fn(|i| day_name(i as u32)) }
    }
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
        let open = options.str("open").unwrap_or(OPEN).to_string();
        let zone = options.str("timezone").filter(|name| !name.is_empty()).and_then(|name| {
            let zone = time_zone(name);
            if zone.is_none() {
                log!("clock: unknown time zone {name:?}: the clock shows Windows' own");
            }
            zone
        });
        let mut clock = Clock { timer, date_format, date: String::new(), time: String::new(), running: false, open, zone, calendar: None };
        clock.refresh();
        clock.start();
        clock
    }

    /// The time now, in the clock's time zone.
    fn now(&self) -> SYSTEMTIME {
        let Some(zone) = &self.zone else { return unsafe { GetLocalTime() } };
        let utc = unsafe { GetSystemTime() };
        let mut local = SYSTEMTIME::default();
        match unsafe { SystemTimeToTzSpecificLocalTimeEx(Some(&**zone), &utc, &mut local) } {
            Ok(()) => local,
            Err(_) => unsafe { GetLocalTime() },
        }
    }

    /// Reads the time again. True if what the pill shows changed.
    fn refresh(&mut self) -> bool {
        let now = self.now();
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

    /// The time alone.
    fn compact(&self, theme: &Theme) -> Option<Pill> {
        Some(Pill::new(vec![Item::text(&self.time, theme.text)]))
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
            // The bar still shows with a game in front: the clock goes on
            super::Signal::Foreground(_) | super::Signal::Game(_) | super::Signal::Trigger { .. } => false,
        }
    }

    fn has_popup(&self) -> bool {
        true
    }

    fn popup_shown(&mut self, open: bool) {
        // Read again at every opening: the region may have changed since
        self.calendar = open.then(|| Calendar::new(&self.now()));
    }

    fn popup(&self, theme: &Theme, fonts: &Fonts) -> Option<Scene> {
        let cal = self.calendar.as_ref()?;
        let now = self.now();
        let today = (now.wYear as i32, now.wMonth as u32, now.wDay as u32);
        let (year, month) = month_of(cal.month);
        let bold = Font::new(theme.font_size, Weight::SemiBold);
        let small = Font::new(theme.small_size, Weight::SemiBold);
        let mut scene = Scene::new(WIDTH, HEIGHT);

        // The whole date, and "Today"
        let label = t("Oggi", "Today");
        let button_w = fonts.width(label, small) + 16.0;
        let button = Rect::new(WIDTH - PAD - button_w, TOP_Y + 2.0, button_w, ROW_H - 4.0);
        let room = button.x - PAD - 12.0;
        let mut date = capitalize(&long_date(&now));
        // Without the year when it's too long (`Wednesday, November 11, 2026`): the month's title
        // just below has it
        if fonts.width(&date, bold) > room
            && let Some(short) = long_date_without_year(&now)
        {
            date = capitalize(&short);
        }
        scene.text(Rect::new(PAD + 4.0, TOP_Y, room, ROW_H), date, bold, theme.text, Align::Start);
        scene.text(button, label, small, theme.primary, Align::Center);
        scene.zone(button, theme.row_radius, "today");

        // The month between ‹ and ›
        let previous = Rect::new(PAD, MONTH_Y, ROW_H, ROW_H);
        let next = Rect::new(WIDTH - PAD - ROW_H, MONTH_Y, ROW_H, ROW_H);
        let title = Rect::new(PAD + ROW_H, MONTH_Y, GRID_W - 2.0 * ROW_H, ROW_H);
        scene.text(title, month_title(year, month), bold, theme.text, Align::Center);
        scene.text(previous, PREVIOUS, Font::icons(10.0), theme.text, Align::Center);
        scene.text(next, NEXT, Font::icons(10.0), theme.text, Align::Center);
        scene.zone(previous, theme.row_radius, "previous");
        scene.zone(next, theme.row_radius, "next");

        // The days of the week, from the region's first
        let column = |i: u32| PAD + i as f32 * (CELL_W + GAP);
        let header = Font::new(theme.small_size, Weight::Medium);
        for i in 0..7 {
            let day = (cal.first_weekday + i) % 7;
            let color = if day >= 5 { theme.overlay } else { theme.subtext };
            scene.text(Rect::new(column(i), DAYS_Y, CELL_W, DAYS_H), cal.names[day as usize].as_str(), header, color, Align::Center);
        }

        // The days: today in a dot, the weekend dimmer
        let number = Font::new(theme.font_size - 1.0, Weight::Medium);
        let lead = (weekday(year, month, 1) + 7 - cal.first_weekday) % 7;
        for d in 1..=days_in_month(year, month) {
            let i = lead + d - 1;
            let cell = Rect::new(column(i % 7), GRID_Y + (i / 7) as f32 * (CELL_H + GAP), CELL_W, CELL_H);
            let dot = Rect::new(cell.x + (CELL_W - CELL_H) / 2.0, cell.y, CELL_H, CELL_H);
            let is_today = (year, month, d) == today;
            let color = if is_today {
                scene.fill(dot, CELL_H / 2.0, theme.primary);
                theme.on_primary
            } else if weekday(year, month, d) >= 5 {
                theme.overlay
            } else {
                theme.text
            };
            scene.text(cell, d.to_string(), if is_today { bold } else { number }, color, Align::Center);
            if !self.open.is_empty() {
                scene.zone(dot, CELL_H / 2.0, format!("day:{year}-{month}-{d}"));
            }
        }
        Some(scene)
    }

    fn popup_input(&mut self, input: &Input) -> Reply {
        let this_month = month_index(&self.now());
        let Some(cal) = &mut self.calendar else { return Reply::Nothing };
        let step = match input {
            Input::Action("previous") => -1,
            Input::Action("next") => 1,
            Input::Action("today") => this_month - cal.month,
            Input::Action(action) => {
                let Some((year, month, day)) = action.strip_prefix("day:").and_then(parse_day) else { return Reply::Nothing };
                process::open(&day_url(&self.open, year, month, day));
                return Reply::Opened;
            }
            // Away from the user goes back in time, as scrolling up a list of months would
            Input::Wheel(notches) => -notches,
            Input::Key(key) => match *key {
                k if k == VK_LEFT.0 || k == VK_UP.0 || k == VK_PRIOR.0 => -1,
                k if k == VK_RIGHT.0 || k == VK_DOWN.0 || k == VK_NEXT.0 => 1,
                k if k == VK_HOME.0 => this_month - cal.month,
                _ => 0,
            },
            Input::Slide(..) | Input::Release(_) => 0,
        };
        // Windows' dates go from 1601 to 30827; the calendar stays well inside them
        let month = (cal.month + step).clamp(1601 * 12, 9999 * 12 + 11);
        if month == cal.month {
            return Reply::Nothing;
        }
        cal.month = month;
        Reply::Redraw
    }
}

/// A month as year * 12 + month - 1.
fn month_index(t: &SYSTEMTIME) -> i32 {
    t.wYear as i32 * 12 + t.wMonth as i32 - 1
}

/// The IANA names of time zones with the key name of Windows' zone for each, one a line
/// (`Asia/Tokyo Tokyo Standard Time`): CLDR's mapping, as the ICU of Windows has it (see the
/// ignored test `print_zones`, which writes it).
const ZONES: &str = include_str!("zones.txt");

/// Where Windows keeps its time zones, a key each.
const ZONES_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Time Zones";

/// A time zone of Windows by its key name (`Tokyo Standard Time`), or by its IANA name
/// (`Asia/Tokyo`) through `ZONES`. Read straight from its key: going through Windows' list of
/// zones (`EnumDynamicTimeZoneInformation`) took 65 ms, and ICU for the name 90 more.
pub fn time_zone(name: &str) -> Option<Box<DYNAMIC_TIME_ZONE_INFORMATION>> {
    zone_from_registry(name).or_else(|| {
        let key = ZONES.lines().find_map(|line| line.split_once(' ').filter(|(iana, _)| iana.eq_ignore_ascii_case(name)).map(|(_, key)| key))?;
        zone_from_registry(key)
    })
}

/// The zone of key `key` under `ZONES_KEY`: its rules (`TZI`) and names. The key name goes with
/// it, so that Windows finds the rules of each year too (`Dynamic DST`).
fn zone_from_registry(key: &str) -> Option<Box<DYNAMIC_TIME_ZONE_INFORMATION>> {
    // REG_TZI_FORMAT
    #[repr(C)]
    #[derive(Default)]
    struct Tzi {
        bias: i32,
        standard_bias: i32,
        daylight_bias: i32,
        standard_date: SYSTEMTIME,
        daylight_date: SYSTEMTIME,
    }
    if key.is_empty() || key.contains('\\') || key.encode_utf16().count() >= 128 {
        return None;
    }
    let subkey = HSTRING::from(format!("{ZONES_KEY}\\{key}"));
    let mut tzi = Tzi::default();
    let mut size = size_of::<Tzi>() as u32;
    let read = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, &subkey, w!("TZI"), RRF_RT_REG_BINARY, None, Some(&mut tzi as *mut Tzi as *mut c_void), Some(&mut size)) };
    if read != ERROR_SUCCESS || size != size_of::<Tzi>() as u32 {
        return None;
    }
    let mut zone = Box::new(DYNAMIC_TIME_ZONE_INFORMATION {
        Bias: tzi.bias,
        StandardBias: tzi.standard_bias,
        DaylightBias: tzi.daylight_bias,
        StandardDate: tzi.standard_date,
        DaylightDate: tzi.daylight_date,
        ..Default::default()
    });
    let text = |value: PCWSTR, into: &mut [u16]| {
        let mut size = (into.len() * 2) as u32;
        // A name it can't read stays empty: only the rules count
        let _ = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, &subkey, value, RRF_RT_REG_SZ, None, Some(into.as_mut_ptr() as *mut c_void), Some(&mut size)) };
    };
    text(w!("Std"), &mut zone.StandardName);
    text(w!("Dlt"), &mut zone.DaylightName);
    for (to, from) in zone.TimeZoneKeyName.iter_mut().zip(key.encode_utf16()) {
        *to = from;
    }
    Some(zone)
}

fn month_of(index: i32) -> (i32, u32) {
    (index.div_euclid(12), index.rem_euclid(12) as u32 + 1)
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(y) => 29,
        _ => 28,
    }
}

/// The day of the week, as Windows counts them: 0 = Monday … 6 = Sunday (Sakamoto's method).
fn weekday(y: i32, m: u32, d: u32) -> u32 {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    let from_sunday = (y + y / 4 - y / 100 + y / 400 + T[m as usize - 1] + d as i32).rem_euclid(7);
    ((from_sunday + 6) % 7) as u32
}

/// The region's first day of the week, 0 = Monday … 6 = Sunday.
fn first_weekday() -> u32 {
    let mut buf = [0u16; 4];
    let n = unsafe { GetLocaleInfoEx(crate::i18n::locale(), LOCALE_IFIRSTDAYOFWEEK, Some(&mut buf)) };
    if n < 2 {
        return 0;
    }
    char::from_u32(buf[0] as u32).and_then(|c| c.to_digit(10)).filter(|d| *d < 7).unwrap_or(0)
}

/// The shortest name of a day (0 = Monday) in the region's language: `lu`, `Mo`.
fn day_name(day: u32) -> String {
    let mut buf = [0u16; 16];
    let n = unsafe { GetLocaleInfoEx(crate::i18n::locale(), LOCALE_SSHORTESTDAYNAME1 + day, Some(&mut buf)) };
    String::from_utf16_lossy(&buf[..(n.max(1) - 1) as usize])
}

/// `Novembre 2026`.
fn month_title(year: i32, month: u32) -> String {
    let first = SYSTEMTIME { wYear: year as u16, wMonth: month as u16, wDay: 1, ..Default::default() };
    capitalize(&format_date(&first, &HSTRING::from("MMMM yyyy")))
}

/// `martedì 10 novembre 2026`: the region's long date.
fn long_date(t: &SYSTEMTIME) -> String {
    let mut buf = [0u16; 80];
    let n = unsafe { GetDateFormatEx(crate::i18n::locale(), DATE_LONGDATE, Some(t), PCWSTR::null(), Some(&mut buf), PCWSTR::null()) };
    String::from_utf16_lossy(&buf[..(n.max(1) - 1) as usize])
}

/// The region's long date with the year taken out of its pattern (`dddd, MMMM d, yyyy` →
/// `dddd, MMMM d`); None for a pattern with quoted text, where the year may sit inside words.
fn long_date_without_year(t: &SYSTEMTIME) -> Option<String> {
    let mut buf = [0u16; 80];
    let n = unsafe { GetLocaleInfoEx(crate::i18n::locale(), LOCALE_SLONGDATE, Some(&mut buf)) };
    let pattern = String::from_utf16_lossy(&buf[..(n.max(1) - 1) as usize]);
    if pattern.is_empty() || pattern.contains('\'') {
        return None;
    }
    let picture = pattern.replace('y', "");
    let picture = picture.trim_matches(|c: char| c == ' ' || c == ',');
    let mut out = [0u16; 80];
    let n = unsafe { GetDateFormatEx(crate::i18n::locale(), ENUM_DATE_FORMATS_FLAGS(0), Some(t), &HSTRING::from(picture), Some(&mut out), PCWSTR::null()) };
    (n > 1).then(|| String::from_utf16_lossy(&out[..n as usize - 1]))
}

fn parse_day(s: &str) -> Option<(i32, u32, u32)> {
    let mut parts = s.split('-');
    let day = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
    parts.next().is_none().then_some(day)
}

fn day_url(template: &str, year: i32, month: u32, day: u32) -> String {
    template.replace("{year}", &year.to_string()).replace("{month}", &month.to_string()).replace("{day}", &day.to_string())
}

pub fn format_date(t: &SYSTEMTIME, picture: &HSTRING) -> String {
    let mut buf = [0u16; 80];
    let n = unsafe { GetDateFormatEx(crate::i18n::locale(), ENUM_DATE_FORMATS_FLAGS(0), Some(t), picture, Some(&mut buf), PCWSTR::null()) };
    // The count includes the terminating null
    String::from_utf16_lossy(&buf[..(n.max(1) - 1) as usize]).replace('.', "")
}

pub fn format_time(t: &SYSTEMTIME) -> String {
    let mut buf = [0u16; 40];
    let n = unsafe { GetTimeFormatEx(crate::i18n::locale(), TIME_NOSECONDS, Some(t), PCWSTR::null(), Some(&mut buf)) };
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
    use super::*;

    #[test]
    fn capitalize_first_letter() {
        assert_eq!(capitalize("mar 10/11"), "Mar 10/11");
        assert_eq!(capitalize(""), "");
    }

    #[test]
    fn weekdays_and_lengths() {
        assert_eq!(weekday(2026, 11, 10), 1); // a Tuesday
        assert_eq!(weekday(2026, 11, 1), 6); // a Sunday
        assert_eq!(weekday(2024, 2, 29), 3); // a Thursday
        assert_eq!(weekday(2000, 1, 1), 5); // a Saturday
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2100, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(month_of(2026 * 12 + 1 - 1 - 1), (2025, 12));
        assert_eq!(month_of(2026 * 12 + 11 - 1), (2026, 11));
    }

    #[test]
    fn six_weeks_hold_every_month() {
        // The worst case: 31 days starting on the last day of the week
        for first_weekday in 0..7 {
            for year in 2020..2040 {
                for month in 1..=12 {
                    let lead = (weekday(year, month, 1) + 7 - first_weekday) % 7;
                    assert!(lead + days_in_month(year, month) <= 42);
                }
            }
        }
    }

    #[test]
    fn time_zones_by_either_name() {
        let key = |name: &str| {
            let zone = time_zone(name).expect("a zone");
            let k = &zone.TimeZoneKeyName;
            (String::from_utf16_lossy(&k[..k.iter().position(|c| *c == 0).unwrap()]), zone.Bias)
        };
        assert_eq!(key("Tokyo Standard Time"), ("Tokyo Standard Time".into(), -540));
        assert_eq!(key("tokyo standard time").1, -540);
        assert_eq!(key("Asia/Tokyo"), ("Tokyo Standard Time".into(), -540));
        assert_eq!(key("europe/rome"), ("W. Europe Standard Time".into(), -60));
        assert_eq!(key("Asia/Kolkata").1, -330);
        assert_eq!(key("UTC").1, 0);
        assert!(time_zone("Nowhere/Atlantis").is_none());
        assert!(time_zone(r"..\Tokyo Standard Time").is_none());
        // 12:00 UTC on 1 July is 21:00 in Tokyo, 14:00 in Rome (summer time)
        let noon = SYSTEMTIME { wYear: 2026, wMonth: 7, wDay: 1, wHour: 12, ..Default::default() };
        for (name, hour) in [("Asia/Tokyo", 21), ("Europe/Rome", 14)] {
            let mut local = SYSTEMTIME::default();
            unsafe { SystemTimeToTzSpecificLocalTimeEx(Some(&*time_zone(name).unwrap()), &noon, &mut local) }.unwrap();
            assert_eq!(local.wHour, hour, "{name}");
        }
    }

    /// Writes the table of `zones.txt` from the ICU of Windows:
    /// `cargo test --release -- --ignored print_zones --nocapture`.
    #[test]
    #[ignore = "writes the table of zones.txt"]
    fn print_zones() {
        use windows::Win32::Globalization::{UErrorCode, ucal_getWindowsTimeZoneID, ucal_openTimeZones, uenum_close, uenum_unext};
        let mut lines = Vec::new();
        unsafe {
            let mut status = UErrorCode::default();
            let all = ucal_openTimeZones(&mut status);
            loop {
                let mut len = 0;
                let id = uenum_unext(all, &mut len, &mut status);
                if id.is_null() {
                    break;
                }
                let mut out = [0u16; 128];
                let mut status = UErrorCode::default();
                let n = ucal_getWindowsTimeZoneID(id, len, out.as_mut_ptr(), out.len() as i32, &mut status);
                if status.0 <= 0 && n > 0 {
                    let iana = String::from_utf16_lossy(std::slice::from_raw_parts(id, len as usize));
                    lines.push(format!("{iana} {}", String::from_utf16_lossy(&out[..n as usize])));
                }
            }
            uenum_close(all);
        }
        println!("ZONES-BEGIN\n{}\nZONES-END", lines.join("\n"));
    }

    #[test]
    fn days_and_their_page() {
        assert_eq!(parse_day("2026-11-10"), Some((2026, 11, 10)));
        assert_eq!(parse_day("2026-11"), None);
        assert_eq!(parse_day("2026-11-10-1"), None);
        assert_eq!(day_url(OPEN, 2026, 11, 3), "https://calendar.google.com/calendar/r/day/2026/11/3");
    }
}
