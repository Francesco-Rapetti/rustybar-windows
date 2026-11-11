//! Today's next event, from calendars in iCal format at their secret addresses (Google Calendar:
//! the calendar's settings, «Secret address in iCal format»): Windows has no calendar an app
//! outside a package can read. As the macOS RustyBar's: the start time, colored by how close it is
//! (yellow from `soon`, 30 minutes, peach from `sooner`, 15, red from `imminent`, 5, the accent
//! while it's on), then the title with a countdown, or «until» while it's on. With no timed event
//! left, the all-day ones; with nothing left today, nothing.
//!
//! The popup is a dashboard. On top, two tiles wide, the event the pill shows, tinted with the
//! color of its countdown: when it is, the title, its calendar and place, how many guests
//! accepted, how much of it is gone while it's on, and «Partecipa a Google Meet» for its call.
//! Then the all-day events as chips, and all of today's timed ones, each with its calendar's
//! color, the past ones dimmed and a red line at the time it is now; a click on one joins its
//! call or shows the day in Google Calendar (`open`, as `clock`'s). «Apri Google Calendar» last.
//!
//! `calendars`: a name and a color each (`[{ name = "Lavoro", color = "sky" }]`); the address of
//! each is a secret in Credential Manager, `events-<name>` (`Get-Clipboard | rustybar secret set
//! events-Lavoro`), or `url` for a public one. They're downloaded on a worker thread every 15
//! minutes (`interval`), at start, on wake and at midnight, but not while no bar shows or a listed
//! game is in front; the countdowns are worked out every minute from what's already read, and
//! only while there's an event to count down to.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{HANDLE, SYSTEMTIME};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Time::SystemTimeToTzSpecificLocalTime;

use super::{Input, Reply, Signal};
use crate::config::Options;
use crate::i18n::t;
use crate::ical::{self, Event};
use crate::log;
use crate::theme::{Rgba, Theme};
use crate::ui::{Align, Font, Fonts, Item, Pill, Rect, Scene, Weight};
use crate::wake::{Timer, Wake};

// Segoe Fluent Icons
const CALENDAR: &str = "\u{E787}";
const VIDEO: &str = "\u{E714}";
const PEOPLE: &str = "\u{E716}";

// The popup's layout, in DIPs.
const MARGIN: f32 = 8.0;
const WIDE: f32 = 308.0;
const WIDTH: f32 = WIDE + 2.0 * MARGIN;
const PAD: f32 = 12.0;
const GAP: f32 = 8.0;
const ROW: f32 = 40.0;
const CHIP_H: f32 = 28.0;
const BUTTON_H: f32 = 32.0;

const GOOGLE_DAY: &str = "https://calendar.google.com/calendar/r/day/{year}/{month}/{day}";

/// A calendar of the config.
#[derive(Clone, Debug, serde::Deserialize)]
struct Calendar {
    name: String,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

/// An event of today, with its calendar's name and color.
#[derive(Clone, Debug, PartialEq)]
struct Ev {
    e: Event,
    calendar: String,
    color: Option<String>,
}

/// What the worker thread leaves: the day, and each calendar's events (None: it failed).
type Fetched = (i64, Vec<Option<Vec<Event>>>);

pub struct Events {
    calendars: Vec<Calendar>,
    soon: i64,
    sooner: i64,
    imminent: i64,
    interval: Duration,
    open: String,
    events: Vec<Ev>,
    /// What each calendar had last time, for when a download fails.
    kept: Vec<Option<Vec<Event>>>,
    /// The day the events are of.
    day: i64,
    fetched: Option<Instant>,
    busy: bool,
    result: Arc<Mutex<Option<Fetched>>>,
    wake: Arc<Wake>,
    timer: Timer,
    /// The timer's period: every minute or every `interval`.
    ticking: Option<i32>,
    shown: bool,
    game: bool,
    mock: bool,
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Today, in days since 1970, as the PC's clock has it.
fn today() -> i64 {
    let t = unsafe { GetLocalTime() };
    ical::days(t.wYear as i64, t.wMonth as u32, t.wDay as u32)
}

/// Minutes to `t`, rounded up: at 9:29 an event at 9:30 is 1 min away.
fn minutes_to(t: i64, now: i64) -> i64 {
    (t - now + 59).div_euclid(60)
}

/// `20 min`, `1 h`, `1 h 20 min`.
fn left(minutes: i64) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// One line of text, or «Senza titolo».
fn line(title: &str) -> String {
    let one: String = title.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let one = one.trim();
    if one.is_empty() { t("Senza titolo", "Untitled").to_string() } else { one.to_string() }
}

/// The time of `t` (seconds since 1970, UTC) on the PC's clock, in the region's format.
fn time(t: i64) -> String {
    let (y, m, d) = ical::civil(t.div_euclid(86_400));
    let s = t.rem_euclid(86_400);
    let utc = SYSTEMTIME { wYear: y as u16, wMonth: m as u16, wDay: d as u16, wHour: (s / 3600) as u16, wMinute: (s / 60 % 60) as u16, ..Default::default() };
    let mut local = SYSTEMTIME::default();
    if unsafe { SystemTimeToTzSpecificLocalTime(None, &utc, &mut local) }.is_err() {
        local = utc;
    }
    super::clock::format_time(&local)
}

impl Events {
    pub fn new(options: &Options) -> Events {
        let calendars: Vec<Calendar> = options.0.get("calendars").cloned().and_then(|v| v.try_into().map_err(|e| log!("events: calendars: {e}")).ok()).unwrap_or_default();
        let minutes = |k: &str, d: i64| options.f64(k).map(|v| v as i64).unwrap_or(d);
        let mut w = Events {
            soon: minutes("soon", 30),
            sooner: minutes("sooner", 15),
            imminent: minutes("imminent", 5),
            interval: Duration::from_secs(minutes("interval", 15).max(1) as u64 * 60),
            open: options.str("open").unwrap_or(GOOGLE_DAY).to_string(),
            kept: vec![None; calendars.len()],
            calendars,
            events: Vec::new(),
            day: today(),
            fetched: None,
            busy: false,
            result: Arc::new(Mutex::new(None)),
            wake: Wake::new(),
            timer: Timer::new(),
            ticking: None,
            shown: true,
            game: false,
            mock: false,
        };
        if let Some(m) = crate::mock::load("events") {
            w.mock = true;
            let now = now();
            let (from, to) = ical::day_bounds(w.day);
            for e in m.array() {
                let all_day = e["all_day"].bool().unwrap_or(false);
                let at = |k: &str| now + e[k].f64().unwrap_or(0.0) as i64 * 60;
                let guests = e["guests"].f64().map(|n| (n as u32, e["accepted"].f64().unwrap_or(0.0) as u32));
                w.events.push(Ev {
                    e: Event {
                        title: e["title"].str().unwrap_or_default().into(),
                        location: e["location"].str().unwrap_or_default().into(),
                        meet: e["meet"].bool().unwrap_or(false).then(|| "https://meet.google.com/".into()),
                        start: if all_day { from } else { at("start") },
                        end: if all_day { to } else { at("end") },
                        all_day,
                        guests,
                    },
                    calendar: e["calendar"].str().unwrap_or_default().into(),
                    color: e["color"].str().map(String::from),
                });
            }
            w.events.sort_by_key(|e| e.e.start);
        }
        w.fetch();
        w.schedule();
        w
    }

    /// Downloads the calendars on a worker thread, unless one is out already.
    fn fetch(&mut self) {
        if self.mock || self.calendars.is_empty() || self.busy || !self.shown || self.game {
            return;
        }
        self.busy = true;
        self.fetched = Some(Instant::now());
        let (calendars, day, result, wake) = (self.calendars.clone(), today(), self.result.clone(), self.wake.clone());
        std::thread::spawn(move || {
            let lists = calendars
                .iter()
                .map(|c| {
                    let Some(url) = c.url.clone().or_else(|| crate::secret::get(&format!("events-{}", c.name))) else {
                        log!("events: no address for {:?}: Get-Clipboard | rustybar secret set events-{}", c.name, c.name);
                        return None;
                    };
                    match crate::net::request("GET", &url, &[], None, 30_000) {
                        Ok(r) if r.status == 200 => Some(ical::day(&String::from_utf8_lossy(&r.body), day)),
                        // Never the address in the log: it's a secret
                        Ok(r) => {
                            log!("events: {:?}: status {}", c.name, r.status);
                            None
                        }
                        Err(e) => {
                            log!("events: {:?}: {}", c.name, e.split(" - ").next().unwrap_or(&e));
                            None
                        }
                    }
                })
                .collect();
            *result.lock().unwrap_or_else(|e| e.into_inner()) = Some((day, lists));
            wake.set();
        });
    }

    /// Takes what the worker left. True if the events changed.
    fn take(&mut self) -> bool {
        let Some((day, lists)) = self.result.lock().unwrap_or_else(|e| e.into_inner()).take() else { return false };
        self.busy = false;
        let same_day = day == self.day;
        self.day = day;
        let mut events = Vec::new();
        for (i, list) in lists.into_iter().enumerate() {
            // A calendar that failed keeps what it had, if of the same day
            let list = list.or_else(|| self.kept.get(i).cloned().flatten().filter(|_| same_day));
            if let Some(slot) = self.kept.get_mut(i) {
                slot.clone_from(&list);
            }
            let c = &self.calendars[i];
            events.extend(list.into_iter().flatten().map(|e| Ev { e, calendar: c.name.clone(), color: c.color.clone() }));
        }
        events.sort_by_key(|e| (e.e.start, e.e.end));
        let changed = events != self.events;
        self.events = events;
        self.schedule();
        changed
    }

    /// Every minute (on the minute) while there's an event to count down to, else every
    /// `interval`; nothing while no bar shows.
    fn schedule(&mut self) {
        let now = now();
        let counting = self.events.iter().any(|e| !e.e.all_day && e.e.end > now);
        let period = match () {
            _ if !self.shown => None,
            _ if counting => Some(60_000),
            _ => Some(self.interval.as_millis() as i32),
        };
        if period == self.ticking {
            return;
        }
        self.timer.stop();
        self.ticking = period;
        if let Some(p) = period {
            let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
            let first = if p == 60_000 { 60_000 - ms % 60_000 + 50 } else { p as i64 };
            self.timer.start(first, p);
        }
    }

    /// A tick: a new day or an old download means another; the countdowns move.
    fn tick(&mut self) -> bool {
        if today() != self.day {
            self.day = today();
            self.events.clear();
            self.kept.iter_mut().for_each(|k| *k = None);
            self.fetch();
        } else if self.fetched.is_none_or(|f| f.elapsed() >= self.interval) {
            self.fetch();
        }
        self.schedule();
        true
    }

    /// The event the pill shows, and whether it's on: of those going on the one that ends first,
    /// until it ends or the next one starts; else the next one.
    fn featured(&self, now: i64) -> Option<(usize, bool)> {
        let timed = || self.events.iter().enumerate().filter(|(_, e)| !e.e.all_day && e.e.end > now);
        let next = timed().find(|(_, e)| e.e.start > now);
        let ongoing = timed().filter(|(_, e)| e.e.start <= now).min_by_key(|(_, e)| e.e.end);
        if let Some((i, on)) = ongoing
            && next.is_none_or(|(_, n)| on.e.end <= n.e.start)
        {
            return Some((i, true));
        }
        next.map(|(i, _)| (i, false))
    }

    fn time_color(&self, theme: &Theme, minutes: i64) -> Rgba {
        let c = |n: &str| theme.color(n).unwrap_or(theme.text);
        if minutes <= self.imminent {
            c("red")
        } else if minutes <= self.sooner {
            c("peach")
        } else if minutes <= self.soon {
            c("yellow")
        } else {
            theme.text
        }
    }

    fn color(&self, theme: &Theme, e: &Ev) -> Rgba {
        e.color.as_deref().and_then(|c| theme.color(c)).unwrap_or(theme.primary)
    }

    /// The day in Google Calendar (`open`).
    fn day_url(&self) -> String {
        let (y, m, d) = ical::civil(self.day);
        self.open.replace("{year}", &y.to_string()).replace("{month}", &m.to_string()).replace("{day}", &d.to_string())
    }

    /// The calendar, then the place.
    fn about(e: &Ev) -> String {
        [e.calendar.as_str(), e.e.location.as_str()].into_iter().filter(|s| !s.is_empty() && !s.contains("://")).map(line).collect::<Vec<_>>().join(" · ")
    }

    /// The tile of the featured event, in the color of its countdown; its height.
    fn header(&self, scene: Option<&mut Scene>, theme: &Theme, i: usize, ongoing: bool, now: i64) -> f32 {
        let e = &self.events[i];
        let about = Self::about(e);
        let mut h = PAD + 18.0 + 24.0;
        if !about.is_empty() {
            h += 18.0;
        }
        if e.e.guests.is_some() {
            h += 18.0;
        }
        if ongoing {
            h += 12.0;
        }
        if e.e.meet.is_some() {
            h += 38.0;
        }
        h += PAD - 2.0;
        let Some(scene) = scene else { return h };
        let (when, color) = if ongoing {
            let l = left(minutes_to(e.e.end, now));
            (if crate::i18n::italian() { format!("In corso · ancora {l}") } else { format!("Now · {l} left") }, theme.primary)
        } else {
            let m = minutes_to(e.e.start, now);
            let l = left(m);
            let c = self.time_color(theme, m);
            (if crate::i18n::italian() { format!("Prossimo · tra {l}") } else { format!("Next · in {l}") }, if c == theme.text { theme.subtext } else { c })
        };
        let lit = color != theme.subtext;
        let r = Rect::new(MARGIN, MARGIN, WIDE, h);
        scene.fill(r, theme.row_radius.max(8.0), theme.tile(color, lit));
        scene.zone(r, theme.row_radius.max(8.0), "day");
        let (x, w) = (r.x + PAD, WIDE - 2.0 * PAD);
        let small = Font::new(theme.small_size, Weight::Medium);
        let mut y = r.y + PAD;
        scene.text(Rect::new(x, y, w, 16.0), when, Font::new(theme.small_size, Weight::SemiBold), color, Align::Start);
        scene.text(Rect::new(x, y, w, 16.0), format!("{} – {}", time(e.e.start), time(e.e.end)), small, theme.subtext, Align::End);
        y += 18.0;
        scene.text(Rect::new(x, y, w, 24.0), line(&e.e.title), Font::new(16.0, Weight::SemiBold), theme.text, Align::Start);
        y += 24.0;
        if !about.is_empty() {
            scene.fill(Rect::new(x + 1.0, y + 5.0, 8.0, 8.0), 4.0, self.color(theme, e));
            scene.text(Rect::new(x + 16.0, y, w - 16.0, 18.0), about, small, theme.subtext, Align::Start);
            y += 18.0;
        }
        if let Some((n, yes)) = e.e.guests {
            let accepted = if crate::i18n::italian() {
                format!("{yes} di {n} {}", if yes == 1 { "ha accettato" } else { "hanno accettato" })
            } else {
                format!("{yes} of {n} accepted")
            };
            scene.text(Rect::new(x - 1.0, y, 12.0, 18.0), PEOPLE, Font::icons(10.0), theme.subtext, Align::Center);
            scene.text(Rect::new(x + 16.0, y, w - 16.0, 18.0), accepted, small, theme.subtext, Align::Start);
            y += 18.0;
        }
        if ongoing {
            let gone = ((now - e.e.start) as f32 / (e.e.end - e.e.start).max(1) as f32).clamp(0.0, 1.0);
            let bar = Rect::new(x, y + 4.0, w, 4.0);
            scene.fill(bar, 2.0, theme.surface);
            scene.fill(Rect::new(x, bar.y, w * gone, 4.0), 2.0, theme.primary);
            y += 12.0;
        }
        if e.e.meet.is_some() {
            let join = Rect::new(x, y + 8.0, w, 30.0);
            scene.fill(join, 15.0, theme.surface.alpha(0.7));
            let label = t("Partecipa a Google Meet", "Join Google Meet");
            scene.text(Rect::new(x, join.y, w / 2.0 - 60.0, 30.0), VIDEO, Font::icons(13.0), theme.color("green").unwrap_or(theme.text), Align::End);
            scene.text(Rect::new(x + w / 2.0 - 52.0, join.y, w / 2.0 + 40.0, 30.0), label, Font::new(theme.small_size + 1.0, Weight::SemiBold), theme.text, Align::Start);
            scene.zone(join, 15.0, format!("meet:{i}"));
        }
        h
    }

    /// Everything in the popup from the top, drawn into `scene` if there's one; where it ends.
    fn layout(&self, mut scene: Option<&mut Scene>, theme: &Theme, fonts: &Fonts) -> f32 {
        let now = now();
        let mut y = MARGIN;
        if let Some((i, ongoing)) = self.featured(now) {
            y += self.header(scene.as_deref_mut(), theme, i, ongoing, now) + GAP;
        }
        // The all-day events as chips with their calendar's color
        let chip_font = Font::new(theme.small_size + 1.0, Weight::SemiBold);
        let mut x = MARGIN;
        let mut any = false;
        for (i, e) in self.events.iter().enumerate().filter(|(_, e)| e.e.all_day) {
            let name = line(&e.e.title);
            let w = (fonts.width(&name, chip_font).ceil() + 34.0).min(WIDE);
            if x > MARGIN && x + w > MARGIN + WIDE {
                x = MARGIN;
                y += CHIP_H + GAP;
            }
            if let Some(s) = scene.as_deref_mut() {
                let r = Rect::new(x, y, w, CHIP_H);
                s.fill(r, CHIP_H / 2.0, theme.surface.alpha(0.7));
                s.fill(Rect::new(x + 10.0, y + 10.0, 8.0, 8.0), 4.0, self.color(theme, e));
                s.text(Rect::new(x + 24.0, y, w - 32.0, CHIP_H), name, chip_font, theme.text, Align::Start);
                s.zone(r, CHIP_H / 2.0, format!("open:{i}"));
            }
            x += w + GAP;
            any = true;
        }
        if any {
            y += CHIP_H + GAP;
        }
        // Today's timed events, with a line at the time it is now
        let timed: Vec<usize> = (0..self.events.len()).filter(|&i| !self.events[i].e.all_day).collect();
        let font = Font::new(theme.font_size, Weight::SemiBold);
        let small = Font::new(theme.small_size, Weight::Medium);
        let hours = timed.iter().map(|&i| fonts.width(&time(self.events[i].e.start), font).ceil()).fold(0.0, f32::max);
        let mut line_drawn = false;
        let now_line = |scene: &mut Option<&mut Scene>, y: &mut f32| {
            if let Some(s) = scene.as_deref_mut() {
                let red = theme.color("red").unwrap_or(theme.text);
                let label = time(now);
                let lw = fonts.width(&label, small).ceil();
                s.fill(Rect::new(MARGIN + 6.0, *y + 4.0, 7.0, 7.0), 3.5, red);
                s.fill(Rect::new(MARGIN + 12.0, *y + 7.0, WIDE - 26.0 - lw, 1.0), 0.5, red);
                s.text(Rect::new(MARGIN, *y, WIDE - 6.0, 15.0), label, small, red, Align::End);
            }
            *y += 17.0;
        };
        for (k, &i) in timed.iter().enumerate() {
            let e = &self.events[i];
            if !line_drawn && e.e.start > now {
                if k > 0 {
                    now_line(&mut scene, &mut y);
                }
                line_drawn = true;
            }
            if let Some(s) = scene.as_deref_mut() {
                let past = e.e.end <= now;
                let color = self.color(theme, e);
                let (bar, start, title, detail) = if past {
                    (color.alpha(0.35), theme.overlay, theme.overlay, theme.overlay)
                } else if e.e.start <= now {
                    (color, theme.primary, theme.text, theme.subtext)
                } else {
                    (color, theme.text, theme.text, theme.subtext)
                };
                let r = Rect::new(MARGIN, y, WIDE, ROW);
                s.zone(r, theme.row_radius, format!("open:{i}"));
                s.fill(Rect::new(r.x + 8.0, y + 6.0, 3.0, ROW - 12.0), 1.5, bar);
                let hx = r.x + 21.0;
                s.text(Rect::new(hx, y + 3.0, hours + 4.0, 18.0), time(e.e.start), font, start, Align::Start);
                s.text(Rect::new(hx, y + 21.0, hours + 4.0, 16.0), time(e.e.end), small, detail, Align::Start);
                let tx = hx + hours + 12.0;
                let tw = r.x + WIDE - tx - if e.e.meet.is_some() { 30.0 } else { 8.0 };
                let about = Self::about(e);
                if about.is_empty() {
                    s.text(Rect::new(tx, y, tw, ROW), line(&e.e.title), font, title, Align::Start);
                } else {
                    s.text(Rect::new(tx, y + 3.0, tw, 18.0), line(&e.e.title), font, title, Align::Start);
                    s.text(Rect::new(tx, y + 21.0, tw, 16.0), about, small, detail, Align::Start);
                }
                if e.e.meet.is_some() {
                    let green = theme.color("green").unwrap_or(theme.text);
                    s.text(Rect::new(r.x + WIDE - 28.0, y, 20.0, ROW), VIDEO, Font::icons(14.0), if past { green.alpha(0.4) } else { green }, Align::Center);
                }
            }
            y += ROW + 2.0;
        }
        // Everything started already
        if !timed.is_empty() && !line_drawn {
            now_line(&mut scene, &mut y);
        }
        let r = Rect::new(MARGIN, y + 2.0, WIDE, BUTTON_H);
        if let Some(s) = scene {
            s.text(Rect::new(r.x + PAD - 2.0, r.y, r.w - 2.0 * PAD, r.h), t("Apri Google Calendar", "Open Google Calendar"), Font::new(theme.font_size, Weight::Regular), theme.primary, Align::Start);
            s.zone(r, theme.row_radius, "calendar");
        }
        r.y + BUTTON_H + MARGIN - 2.0
    }
}

impl super::Widget for Events {
    fn pill(&self, theme: &Theme) -> Option<Pill> {
        let now = now();
        let (color, start, title) = match self.featured(now) {
            Some((i, true)) => {
                let on = &self.events[i];
                let l = left(minutes_to(on.e.end, now));
                let text = if crate::i18n::italian() {
                    format!("{} · fino alle {} (ancora {l})", line(&on.e.title), time(on.e.end))
                } else {
                    format!("{} · until {} ({l} left)", line(&on.e.title), time(on.e.end))
                };
                (theme.primary, Some(time(on.e.start)), text)
            }
            Some((i, false)) => {
                let n = &self.events[i];
                let m = minutes_to(n.e.start, now);
                let when = if crate::i18n::italian() { format!("tra {}", left(m)) } else { format!("in {}", left(m)) };
                (self.time_color(theme, m), Some(time(n.e.start)), format!("{} · {when}", line(&n.e.title)))
            }
            None => {
                let all_day: Vec<String> = self.events.iter().filter(|e| e.e.all_day).map(|e| line(&e.e.title)).collect();
                if all_day.is_empty() {
                    return None;
                }
                (theme.text, None, all_day.join(" · "))
            }
        };
        let mut items = vec![Item::icon(CALENDAR, color)];
        if let Some(start) = start {
            items.push(Item::text(start, color));
        }
        items.push(Item::text(title, theme.text).max_width(320.0));
        Some(Pill::new(items).gap(6.0))
    }

    /// The time alone (the titles of the all-day events, shorter, when there's none).
    fn compact(&self, theme: &Theme) -> Option<Pill> {
        let mut pill = self.pill(theme)?;
        if pill.items.len() == 3 {
            pill.items.pop();
        } else if let Some(title) = pill.items.get_mut(1) {
            title.max_width = Some(140.0);
        }
        Some(pill)
    }

    fn handles(&self) -> Vec<HANDLE> {
        self.wake.handle().into_iter().chain(self.timer.handle()).collect()
    }

    fn signaled_at(&mut self, index: usize) -> bool {
        if index == 1 { self.tick() } else { self.take() }
    }

    fn event(&mut self, signal: &Signal) -> bool {
        match signal {
            Signal::Shown(shown) if *shown != self.shown => {
                self.shown = *shown;
                if *shown {
                    self.tick();
                } else {
                    self.schedule();
                }
                *shown
            }
            Signal::Game(game) => {
                self.game = *game;
                false
            }
            Signal::Resume | Signal::TimeChanged => {
                self.fetched = None;
                self.tick()
            }
            _ => false,
        }
    }

    fn has_popup(&self) -> bool {
        !self.events.is_empty()
    }

    fn popup(&self, theme: &Theme, fonts: &Fonts) -> Option<Scene> {
        if self.events.is_empty() {
            return None;
        }
        let height = self.layout(None, theme, fonts);
        let mut scene = Scene::new(WIDTH, height);
        self.layout(Some(&mut scene), theme, fonts);
        Some(scene)
    }

    fn popup_input(&mut self, input: &Input) -> Reply {
        let Input::Action(action) = input else { return Reply::Nothing };
        let event = |i: &str| i.parse::<usize>().ok().and_then(|i| self.events.get(i));
        // A row joins the call it has, the tile on top has «Join» for that
        let target = match action.split_once(':') {
            Some(("meet", i)) => event(i).and_then(|e| e.e.meet.clone()),
            Some(("open", i)) => event(i).map(|e| e.e.meet.clone().unwrap_or_else(|| self.day_url())),
            _ => Some(self.day_url()),
        };
        match target {
            Some(url) if !self.mock && !url.is_empty() => {
                crate::process::open(&url);
                Reply::Opened
            }
            _ => Reply::Close,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(title: &str, start: i64, end: i64) -> Ev {
        Ev { e: Event { title: title.into(), location: String::new(), meet: None, start, end, all_day: false, guests: None }, calendar: String::new(), color: None }
    }

    #[test]
    fn countdowns() {
        assert_eq!(minutes_to(60, 1), 1);
        assert_eq!(minutes_to(60, 0), 1);
        assert_eq!(minutes_to(61, 0), 2);
        assert_eq!(left(5), "5 min");
        assert_eq!(left(60), "1 h");
        assert_eq!(left(80), "1 h 20 min");
        assert_eq!(line(" a\nb "), "a b");
    }

    /// The one going on until it ends or the next starts; else the next.
    #[test]
    fn the_event_the_pill_shows() {
        let mut w = Events::new(&Options(toml::Table::new()));
        w.events = vec![ev("on", 0, 1000), ev("next", 500, 600), ev("later", 2000, 3000)];
        assert_eq!(w.featured(100), Some((1, false)));
        w.events = vec![ev("on", 0, 400), ev("next", 500, 600)];
        assert_eq!(w.featured(100), Some((0, true)));
        assert_eq!(w.featured(450), Some((1, false)));
        assert_eq!(w.featured(700), None);
    }
}
