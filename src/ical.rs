//! Calendars in the iCalendar format (RFC 5545), as Google Calendar gives them at a calendar's
//! secret address: the events of one day, with their times in UTC. Enough of the format for what
//! calendars hold: folded lines, escaped text, `DTSTART`/`DTEND`/`DURATION` with a `TZID` (an IANA
//! zone, `Europe/Rome`, or one of Windows', read with Windows' own rules for each year), in UTC or
//! floating, all-day dates, and repeating events (`RRULE` with `FREQ`, `INTERVAL`, `COUNT`, `UNTIL`,
//! `BYDAY`, `BYMONTHDAY`, `BYMONTH`), with `EXDATE` and the occurrences changed or cancelled on
//! their own (`RECURRENCE-ID`). Run on a worker thread: a calendar of years is a few MB of text.

use std::collections::{HashMap, HashSet};

use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::System::Time::{DYNAMIC_TIME_ZONE_INFORMATION, TzSpecificLocalTimeToSystemTime, TzSpecificLocalTimeToSystemTimeEx};

/// An event of the day: times in seconds since 1970, UTC (an all-day one from the local midnight
/// it starts on to the one it ends on).
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub title: String,
    pub location: String,
    pub meet: Option<String>,
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    /// Guests, and how many of them accepted.
    pub guests: Option<(u32, u32)>,
}

/// Days since 1970-01-01 of a date of the proleptic Gregorian calendar (Howard Hinnant's).
pub fn days(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a day number.
pub fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { yoe + era * 400 + 1 } else { yoe + era * 400 }, m, d)
}

/// 0 for Monday, 6 for Sunday.
fn weekday(day: i64) -> u32 {
    (day + 3).rem_euclid(7) as u32
}

fn month_length(y: i64, m: u32) -> u32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    (days(ny, nm, 1) - days(y, m, 1)) as u32
}

/// Where a time is: in UTC, in the PC's zone (floating), or in a named one.
#[derive(Clone, Debug, PartialEq)]
enum Zone {
    Utc,
    Floating,
    Named(String),
}

/// A `DTSTART`-like value: a day, and the seconds into it unless it's a date.
#[derive(Clone, Debug, PartialEq)]
struct When {
    day: i64,
    secs: Option<i64>,
    zone: Zone,
}

/// The zones met so far, read once each.
#[derive(Default)]
struct Zones(HashMap<String, Option<Box<DYNAMIC_TIME_ZONE_INFORMATION>>>);

fn system_time(day: i64, secs: i64) -> SYSTEMTIME {
    let (y, m, d) = civil(day);
    SYSTEMTIME {
        wYear: y as u16,
        wMonth: m as u16,
        wDay: d as u16,
        wHour: (secs / 3600) as u16,
        wMinute: (secs / 60 % 60) as u16,
        wSecond: (secs % 60) as u16,
        ..Default::default()
    }
}

fn from_system_time(t: &SYSTEMTIME) -> i64 {
    days(t.wYear as i64, t.wMonth as u32, t.wDay as u32) * 86_400 + t.wHour as i64 * 3600 + t.wMinute as i64 * 60 + t.wSecond as i64
}

impl Zones {
    /// A local time of a zone as seconds since 1970, UTC.
    fn utc(&mut self, day: i64, secs: i64, zone: &Zone) -> i64 {
        let local = system_time(day, secs);
        let mut out = SYSTEMTIME::default();
        let converted = match zone {
            Zone::Utc => return day * 86_400 + secs,
            Zone::Floating => unsafe { TzSpecificLocalTimeToSystemTime(None, &local, &mut out) },
            Zone::Named(name) => {
                let tz = self.0.entry(name.clone()).or_insert_with(|| crate::widgets::clock::time_zone(name));
                match tz {
                    Some(tz) => unsafe { TzSpecificLocalTimeToSystemTimeEx(Some(&**tz), &local, &mut out) },
                    // A zone Windows doesn't know: as if it were the PC's
                    None => unsafe { TzSpecificLocalTimeToSystemTime(None, &local, &mut out) },
                }
            }
        };
        match converted {
            Ok(()) => from_system_time(&out),
            Err(_) => day * 86_400 + secs,
        }
    }

    fn instant(&mut self, w: &When) -> i64 {
        self.utc(w.day, w.secs.unwrap_or(0), if w.secs.is_some() { &w.zone } else { &Zone::Floating })
    }
}

/// A content line: its name, parameters and value.
struct Line {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

impl Line {
    fn param(&self, key: &str) -> Option<&str> {
        self.params.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.as_str())
    }
}

/// The lines of the text, with the folded ones joined back.
fn lines(text: &str) -> Vec<Line> {
    let mut joined: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        match raw.strip_prefix([' ', '\t']) {
            Some(rest) if !joined.is_empty() => joined.last_mut().expect("not empty").push_str(rest),
            _ => joined.push(raw.to_string()),
        }
    }
    joined.into_iter().filter_map(|l| parse_line(&l)).collect()
}

fn parse_line(l: &str) -> Option<Line> {
    // The value starts at the first colon outside quotes
    let mut quoted = false;
    let colon = l.char_indices().find(|&(_, c)| {
        if c == '"' {
            quoted = !quoted;
        }
        c == ':' && !quoted
    })?;
    let (head, value) = (&l[..colon.0], &l[colon.0 + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?.trim().to_ascii_uppercase();
    let params = parts.filter_map(|p| p.split_once('=')).map(|(k, v)| (k.trim().to_ascii_uppercase(), v.trim_matches('"').to_string())).collect();
    Some(Line { name, params, value: value.to_string() })
}

/// Text values come with `\n`, `\,`, `\;` and `\\` escaped.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(c) => out.push(c),
            None => {}
        }
    }
    out
}

/// `20261111`, `20261111T090000`, `20261111T090000Z`, with the line's `TZID`.
fn when(value: &str, tzid: Option<&str>) -> Option<When> {
    let v = value.trim();
    let date = v.get(..8)?;
    let (y, m, d) = (date.get(..4)?.parse().ok()?, date.get(4..6)?.parse().ok()?, date.get(6..8)?.parse().ok()?);
    let day = days(y, m, d);
    let Some(time) = v.get(8..).and_then(|t| t.strip_prefix('T')) else { return Some(When { day, secs: None, zone: Zone::Floating }) };
    let (time, utc) = match time.strip_suffix('Z') {
        Some(t) => (t, true),
        None => (time, false),
    };
    let secs = time.get(..2)?.parse::<i64>().ok()? * 3600 + time.get(2..4)?.parse::<i64>().ok()? * 60 + time.get(4..6).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    let zone = match (utc, tzid) {
        (true, _) => Zone::Utc,
        (false, Some(z)) => Zone::Named(z.trim_start_matches('/').to_string()),
        (false, None) => Zone::Floating,
    };
    Some(When { day, secs: Some(secs), zone })
}

/// `PT1H30M`, `P1D`, `-PT15M`: in seconds.
fn duration(value: &str) -> Option<i64> {
    let (sign, v) = match value.trim().strip_prefix('-') {
        Some(v) => (-1, v),
        None => (1, value.trim().trim_start_matches('+')),
    };
    let v = v.strip_prefix('P')?;
    let (mut total, mut number, mut time) = (0i64, 0i64, false);
    for c in v.chars() {
        match c {
            '0'..='9' => number = number * 10 + c.to_digit(10)? as i64,
            'T' => time = true,
            'W' => (total, number) = (total + number * 7 * 86_400, 0),
            'D' => (total, number) = (total + number * 86_400, 0),
            'H' if time => (total, number) = (total + number * 3600, 0),
            'M' if time => (total, number) = (total + number * 60, 0),
            'S' if time => (total, number) = (total + number, 0),
            _ => return None,
        }
    }
    Some(sign * total)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

/// A repeat rule, as far as this reads it.
#[derive(Clone, Debug, PartialEq)]
struct Rule {
    freq: Freq,
    interval: i64,
    count: Option<u32>,
    /// The last day an occurrence may start on.
    until: Option<i64>,
    /// Weekdays (0 Monday), each with its place in the month or the year (0: every one).
    by_day: Vec<(i32, u32)>,
    by_month_day: Vec<i32>,
    by_month: Vec<u32>,
}

fn rule(value: &str) -> Option<Rule> {
    let mut r = Rule { freq: Freq::Daily, interval: 1, count: None, until: None, by_day: Vec::new(), by_month_day: Vec::new(), by_month: Vec::new() };
    let mut freq = None;
    for part in value.split(';') {
        let Some((k, v)) = part.split_once('=') else { continue };
        match k.to_ascii_uppercase().as_str() {
            "FREQ" => {
                freq = Some(match v.to_ascii_uppercase().as_str() {
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    "MONTHLY" => Freq::Monthly,
                    "YEARLY" => Freq::Yearly,
                    // Hourly and the like: not in calendars of people
                    _ => return None,
                })
            }
            "INTERVAL" => r.interval = v.parse().unwrap_or(1).max(1),
            "COUNT" => r.count = v.parse().ok(),
            "UNTIL" => r.until = when(v, None).map(|w| w.day),
            "BYDAY" => {
                for d in v.split(',') {
                    let d = d.trim();
                    let (n, name) = d.split_at(d.len().saturating_sub(2));
                    let wd = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"].iter().position(|x| x.eq_ignore_ascii_case(name));
                    if let Some(wd) = wd {
                        r.by_day.push((n.trim_start_matches('+').parse().unwrap_or(0), wd as u32));
                    }
                }
            }
            "BYMONTHDAY" => r.by_month_day = v.split(',').filter_map(|d| d.trim().parse().ok()).collect(),
            "BYMONTH" => r.by_month = v.split(',').filter_map(|d| d.trim().parse().ok()).collect(),
            _ => {}
        }
    }
    r.freq = freq?;
    Some(r)
}

/// The days of month `m` of year `y` that the rule picks (sorted), with `start` the first day of
/// the series for a rule that names none.
fn month_days(r: &Rule, y: i64, m: u32, start: i64) -> Vec<i64> {
    let first = days(y, m, 1);
    let len = month_length(y, m) as i64;
    let mut out: Vec<i64> = Vec::new();
    if !r.by_month_day.is_empty() {
        for &d in &r.by_month_day {
            let d = if d > 0 { d as i64 } else { len + 1 + d as i64 };
            if (1..=len).contains(&d) {
                out.push(first + d - 1);
            }
        }
        // Both: the days of the month that are also of those weekdays
        if !r.by_day.is_empty() {
            out.retain(|&day| r.by_day.iter().any(|&(_, wd)| weekday(day) == wd));
        }
    } else if !r.by_day.is_empty() {
        for &(n, wd) in &r.by_day {
            let all: Vec<i64> = (0..len).map(|i| first + i).filter(|&d| weekday(d) == wd).collect();
            match n {
                0 => out.extend(all),
                n if n > 0 => out.extend(all.get(n as usize - 1)),
                n => out.extend(all.len().checked_sub(n.unsigned_abs() as usize).and_then(|i| all.get(i))),
            }
        }
    } else {
        let (_, _, d) = civil(start);
        if (d as i64) <= len {
            out.push(first + d as i64 - 1);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// The days the series starting on `start` falls on, in order, up to `last`.
fn occurrences(r: &Rule, start: i64, last: i64) -> Vec<i64> {
    let mut out = Vec::new();
    let mut count = 0u32;
    let (sy, sm, _) = civil(start);
    let in_month = |day: i64| r.by_month.is_empty() || r.by_month.contains(&civil(day).1);
    // Whole periods, at most this many: a daily event of decades is about 10 000
    for k in 0..50_000i64 {
        let candidates: Vec<i64> = match r.freq {
            Freq::Daily => {
                let day = start + k * r.interval;
                let ok = in_month(day) && (r.by_day.is_empty() || r.by_day.iter().any(|&(_, wd)| weekday(day) == wd)) && (r.by_month_day.is_empty() || month_days(r, civil(day).0, civil(day).1, start).contains(&day));
                if ok { vec![day] } else { vec![] }
            }
            Freq::Weekly => {
                let monday = start - weekday(start) as i64 + k * 7 * r.interval;
                let mut wds: Vec<u32> = r.by_day.iter().map(|&(_, wd)| wd).collect();
                if wds.is_empty() {
                    wds.push(weekday(start));
                }
                wds.sort_unstable();
                wds.dedup();
                wds.into_iter().map(|wd| monday + wd as i64).filter(|&d| in_month(d)).collect()
            }
            Freq::Monthly => {
                let months = (sm as i64 - 1) + k * r.interval;
                let (y, m) = (sy + months.div_euclid(12), (months.rem_euclid(12) + 1) as u32);
                if in_month(days(y, m, 1)) { month_days(r, y, m, start) } else { vec![] }
            }
            Freq::Yearly => {
                let y = sy + k * r.interval;
                let months = if r.by_month.is_empty() { vec![sm] } else { r.by_month.clone() };
                let mut all: Vec<i64> = months.into_iter().filter(|m| (1..=12).contains(m)).flat_map(|m| month_days(r, y, m, start)).collect();
                all.sort_unstable();
                all
            }
        };
        let period_start = match r.freq {
            Freq::Daily => start + k * r.interval,
            Freq::Weekly => start - weekday(start) as i64 + k * 7 * r.interval,
            Freq::Monthly => {
                let months = (sm as i64 - 1) + k * r.interval;
                days(sy + months.div_euclid(12), (months.rem_euclid(12) + 1) as u32, 1)
            }
            Freq::Yearly => days(sy + k * r.interval, 1, 1),
        };
        if period_start > last || r.until.is_some_and(|u| period_start > u) {
            break;
        }
        for day in candidates.into_iter().filter(|&d| d >= start) {
            if day > last || r.until.is_some_and(|u| day > u) || r.count.is_some_and(|c| count >= c) {
                return out;
            }
            count += 1;
            out.push(day);
        }
        if r.count.is_some_and(|c| count >= c) {
            break;
        }
    }
    out
}

/// A `VEVENT`, as read.
#[derive(Default)]
struct Raw {
    uid: String,
    title: String,
    location: String,
    description: String,
    conference: String,
    start: Option<When>,
    end: Option<When>,
    duration: Option<i64>,
    rule: Option<Rule>,
    exdates: Vec<When>,
    recurrence: Option<When>,
    cancelled: bool,
    guests: u32,
    accepted: u32,
}

fn events(text: &str) -> Vec<Raw> {
    let mut out = Vec::new();
    let mut current: Option<Raw> = None;
    // Inside a VALARM or another component within the event
    let mut nested = 0;
    for l in lines(text) {
        match (l.name.as_str(), l.value.trim().to_ascii_uppercase().as_str()) {
            ("BEGIN", "VEVENT") => current = Some(Raw::default()),
            ("END", "VEVENT") => out.extend(current.take()),
            ("BEGIN", _) if current.is_some() => nested += 1,
            ("END", _) if current.is_some() => nested -= 1,
            _ => {}
        }
        let Some(e) = current.as_mut().filter(|_| nested == 0) else { continue };
        match l.name.as_str() {
            "UID" => e.uid = l.value.clone(),
            "SUMMARY" => e.title = unescape(&l.value),
            "LOCATION" => e.location = unescape(&l.value),
            "DESCRIPTION" => e.description = unescape(&l.value),
            "X-GOOGLE-CONFERENCE" | "URL" => e.conference.push_str(&format!(" {}", l.value)),
            "DTSTART" => e.start = when(&l.value, l.param("TZID")),
            "DTEND" => e.end = when(&l.value, l.param("TZID")),
            "DURATION" => e.duration = duration(&l.value),
            "RRULE" => e.rule = rule(&l.value),
            "EXDATE" => e.exdates.extend(l.value.split(',').filter_map(|v| when(v, l.param("TZID")))),
            "RECURRENCE-ID" => e.recurrence = when(&l.value, l.param("TZID")),
            "STATUS" => e.cancelled = l.value.trim().eq_ignore_ascii_case("CANCELLED"),
            "ATTENDEE" => {
                e.guests += 1;
                if l.param("PARTSTAT").is_some_and(|p| p.eq_ignore_ascii_case("ACCEPTED")) {
                    e.accepted += 1;
                }
            }
            _ => {}
        }
    }
    out
}

/// The Google Meet address in a text, without what follows it (`?authuser=…`).
pub fn meet_link(text: &str) -> Option<String> {
    const PREFIX: &str = "https://meet.google.com/";
    let lower = text.to_ascii_lowercase();
    let at = lower.find(PREFIX)?;
    let rest = &text[at + PREFIX.len()..];
    let len = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '/' || c == '-')).unwrap_or(rest.len());
    (len > 0).then(|| format!("{PREFIX}{}", &rest[..len]))
}

/// The local midnights of `day` and of the day after, as seconds since 1970, UTC.
pub fn day_bounds(day: i64) -> (i64, i64) {
    let mut zones = Zones::default();
    (zones.utc(day, 0, &Zone::Floating), zones.utc(day + 1, 0, &Zone::Floating))
}

/// The events of a calendar that are on during the local day `day` (days since 1970), in no
/// particular order.
pub fn day(text: &str, day: i64) -> Vec<Event> {
    let mut zones = Zones::default();
    let (from, to) = day_bounds(day);
    let raws = events(text);
    // The occurrences changed or cancelled on their own, by series and original start
    let mut moved: HashSet<(String, i64)> = HashSet::new();
    for r in raws.iter().filter(|r| r.recurrence.is_some()) {
        let at = zones.instant(r.recurrence.as_ref().expect("filtered"));
        moved.insert((r.uid.clone(), at));
    }
    let mut out = Vec::new();
    for r in &raws {
        let Some(start) = &r.start else { continue };
        if r.cancelled {
            continue;
        }
        let all_day = start.secs.is_none();
        // How long it lasts: in days for a date, in seconds otherwise
        let length = match (&r.end, r.duration) {
            (Some(end), _) if all_day => (end.day - start.day).max(1) * 86_400,
            (Some(end), _) => zones.instant(end) - zones.instant(start),
            (None, Some(d)) => d,
            (None, None) if all_day => 86_400,
            (None, None) => 0,
        };
        let span_days = length.div_euclid(86_400) + 1;
        let starts: Vec<i64> = match (&r.rule, &r.recurrence) {
            (Some(rule), None) => occurrences(rule, start.day, day + 1).into_iter().filter(|&d| d >= day - span_days - 1).collect(),
            _ => vec![start.day],
        };
        let excluded: Vec<i64> = r.exdates.iter().map(|w| zones.instant(&When { secs: w.secs.or(start.secs), ..w.clone() })).collect();
        let link = meet_link(&r.conference).or_else(|| meet_link(&r.description)).or_else(|| meet_link(&r.location));
        for d in starts {
            let (s, e) = if all_day {
                let s = zones.utc(d, 0, &Zone::Floating);
                (s, zones.utc(d + length / 86_400, 0, &Zone::Floating))
            } else {
                let s = zones.utc(d, start.secs.unwrap_or(0), &start.zone);
                (s, s + length.max(0))
            };
            let instance = if all_day { zones.utc(d, 0, &Zone::Floating) } else { s };
            if r.recurrence.is_none() && r.rule.is_some() && (excluded.contains(&instance) || moved.contains(&(r.uid.clone(), instance))) {
                continue;
            }
            // On during the day: a timed one of no length counts at its start
            let on = if e > s { s < to && e > from } else { s >= from && s < to };
            if !on {
                continue;
            }
            out.push(Event {
                title: r.title.clone(),
                location: r.location.clone(),
                meet: link.clone(),
                start: s,
                end: e.max(s),
                all_day,
                guests: (r.guests > 0).then_some((r.guests, r.accepted)),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(days(1970, 1, 1), 0);
        assert_eq!(days(2026, 11, 11), 20_768);
        assert_eq!(civil(20_768), (2026, 11, 11));
        assert_eq!(civil(days(2024, 2, 29)), (2024, 2, 29));
        // 11 November 2026 is a Wednesday
        assert_eq!(weekday(days(2026, 11, 11)), 2);
        assert_eq!(month_length(2024, 2), 29);
        assert_eq!(month_length(2026, 2), 28);
    }

    #[test]
    fn values() {
        assert_eq!(unescape(r"a\, b\; c\nd\\e"), "a, b; c\nd\\e");
        assert_eq!(duration("PT1H30M"), Some(5400));
        assert_eq!(duration("P1D"), Some(86_400));
        assert_eq!(duration("-PT15M"), Some(-900));
        assert_eq!(when("20261111T090000Z", None), Some(When { day: days(2026, 11, 11), secs: Some(32_400), zone: Zone::Utc }));
        assert_eq!(when("20261111", None).unwrap().secs, None);
        assert_eq!(when("20261111T090000", Some("Europe/Rome")).unwrap().zone, Zone::Named("Europe/Rome".into()));
        assert_eq!(meet_link("Join: https://meet.google.com/abc-defg-hij?authuser=0 now").as_deref(), Some("https://meet.google.com/abc-defg-hij"));
        // Folded lines and quoted parameters
        let l = lines("SUMMARY:Long\r\n  title\r\nATTENDEE;CN=\"A: B\";PARTSTAT=ACCEPTED:mailto:a@example.com\r\n");
        assert_eq!(l[0].value, "Long title");
        assert_eq!(l[1].param("CN"), Some("A: B"));
        assert_eq!(l[1].value, "mailto:a@example.com");
    }

    fn rule_days(r: &str, start: (i64, u32, u32), until: (i64, u32, u32)) -> Vec<(i64, u32, u32)> {
        occurrences(&rule(r).unwrap(), days(start.0, start.1, start.2), days(until.0, until.1, until.2)).into_iter().map(civil).collect()
    }

    #[test]
    fn repeats() {
        // Every weekday from a Monday, two weeks: ten days
        assert_eq!(rule_days("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR", (2026, 11, 2), (2026, 11, 15)).len(), 10);
        // Every other Wednesday, three times
        assert_eq!(rule_days("FREQ=WEEKLY;INTERVAL=2;COUNT=3", (2026, 11, 4), (2027, 12, 31)), vec![(2026, 11, 4), (2026, 11, 18), (2026, 12, 2)]);
        // The second Tuesday of every month
        assert_eq!(rule_days("FREQ=MONTHLY;BYDAY=2TU", (2026, 11, 10), (2027, 1, 31)), vec![(2026, 11, 10), (2026, 12, 8), (2027, 1, 12)]);
        // The last Friday
        assert_eq!(rule_days("FREQ=MONTHLY;BYDAY=-1FR", (2026, 11, 27), (2026, 12, 31)), vec![(2026, 11, 27), (2026, 12, 25)]);
        // The 31st, only in the months that have one
        assert_eq!(rule_days("FREQ=MONTHLY", (2026, 10, 31), (2027, 1, 31)), vec![(2026, 10, 31), (2026, 12, 31), (2027, 1, 31)]);
        // A birthday, until a date
        assert_eq!(rule_days("FREQ=YEARLY;UNTIL=20280101", (2024, 2, 29), (2030, 1, 1)), vec![(2024, 2, 29)]);
        assert_eq!(rule_days("FREQ=YEARLY;UNTIL=20290101T000000Z", (2025, 3, 1), (2030, 1, 1)).len(), 4);
        // Daily, every third day
        assert_eq!(rule_days("FREQ=DAILY;INTERVAL=3", (2026, 11, 1), (2026, 11, 10)), vec![(2026, 11, 1), (2026, 11, 4), (2026, 11, 7), (2026, 11, 10)]);
    }

    /// A calendar in UTC: what's on during a day, wherever the PC is, as long as the day's bounds
    /// come from the same zone.
    #[test]
    fn a_day_of_a_calendar() {
        let ics = "BEGIN:VCALENDAR\r
BEGIN:VEVENT\r
UID:standup\r
DTSTART:20261102T090000Z\r
DTEND:20261102T091500Z\r
RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR\r
EXDATE:20261112T090000Z\r
SUMMARY:Standup\r
X-GOOGLE-CONFERENCE:https://meet.google.com/abc-defg-hij\r
ATTENDEE;PARTSTAT=ACCEPTED:mailto:a@example.com\r
ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:b@example.com\r
BEGIN:VALARM\r
DESCRIPTION:Not the event's\r
END:VALARM\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:standup\r
RECURRENCE-ID:20261111T090000Z\r
DTSTART:20261111T100000Z\r
DTEND:20261111T101500Z\r
SUMMARY:Standup (moved)\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:release\r
DTSTART;VALUE=DATE:20261111\r
DTEND;VALUE=DATE:20261112\r
SUMMARY:Release day\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:gone\r
DTSTART:20261111T120000Z\r
DURATION:PT1H\r
STATUS:CANCELLED\r
SUMMARY:Cancelled\r
END:VEVENT\r
END:VCALENDAR\r
";
        let d = days(2026, 11, 11);
        let mut on = day(ics, d);
        on.sort_by_key(|e| e.start);
        let titles: Vec<&str> = on.iter().map(|e| e.title.as_str()).collect();
        assert!(titles.contains(&"Standup (moved)") && titles.contains(&"Release day"), "{titles:?}");
        assert!(!titles.contains(&"Standup") && !titles.contains(&"Cancelled"), "{titles:?}");
        let release = on.iter().find(|e| e.all_day).unwrap();
        assert_eq!((release.start, release.end), day_bounds(d));
        // The day after: the series, without the moved one; the excluded day: nothing
        let next = day(ics, d + 1);
        assert!(next.is_empty(), "{next:?}");
        let tuesday = day(ics, d - 1);
        assert_eq!(tuesday.len(), 1);
        assert_eq!(tuesday[0].start, (d - 1) * 86_400 + 9 * 3600);
        assert_eq!(tuesday[0].meet.as_deref(), Some("https://meet.google.com/abc-defg-hij"));
        assert_eq!(tuesday[0].guests, Some((2, 1)));
    }

    /// A real calendar, Google's public one of Italian holidays (nothing personal): downloaded and
    /// read, Christmas is there. Ignored: it needs the network (`cargo test -- --ignored`).
    #[test]
    #[ignore]
    fn a_calendar_of_google() {
        let url = "https://calendar.google.com/calendar/ical/it.italian%23holiday%40group.v.calendar.google.com/public/basic.ics";
        let r = crate::net::request("GET", url, &[], None, 30_000).unwrap();
        assert_eq!(r.status, 200);
        let text = String::from_utf8_lossy(&r.body);
        let christmas = day(&text, days(2026, 12, 25));
        assert!(christmas.iter().any(|e| e.all_day && e.title.contains("Natale")), "{christmas:?}");
    }

    /// A zone of the event's own, with summer time: 9:00 in Rome is 8:00 UTC in winter, 7:00 in
    /// summer.
    #[test]
    fn times_in_a_named_zone() {
        let mut zones = Zones::default();
        let rome = Zone::Named("Europe/Rome".into());
        assert_eq!(zones.utc(days(2026, 1, 15), 9 * 3600, &rome), days(2026, 1, 15) * 86_400 + 8 * 3600);
        assert_eq!(zones.utc(days(2026, 7, 15), 9 * 3600, &rome), days(2026, 7, 15) * 86_400 + 7 * 3600);
        assert_eq!(zones.utc(days(2026, 7, 15), 9 * 3600, &Zone::Named("W. Europe Standard Time".into())), days(2026, 7, 15) * 86_400 + 7 * 3600);
    }
}
