//! Claude: how much of the plan's limits the Claude account has used. `✻ 42% · 31% · 17:07`: the
//! spark in the account's color (`color`, Claude's orange by default), the 5-hour session and the
//! week, yellow from `warning` (75), red from `critical` (90), all gray while the numbers are stale
//! (the last reading failed), and when the session starts again. Another account is another
//! widget of the same type with its own Claude Code config dir: `[widgets.claude_work]
//! type = "claude"`, `dir = "~/.claude-work"`, `color = "#…"`.
//!
//! The popup: a header with the spark, the name (or the email), the plan, the email and the
//! organization (not the personal one, named after the email), how old the numbers are (yellow
//! when the last reading failed, `Aggiornamento…` while it reads) and ↻; two tiles, the session and
//! the week, with the percentage, a bar, when it resets and how long until then (the times follow
//! the clock while the popup is open); `Esci`, which asks for a second click; the usage page on
//! claude.ai. Logged out: `Nessun account`, the config dir and `Accedi` (`claude auth login`, which
//! opens the browser); a login without plan limits (an API key) says so, and so does a PC without
//! Claude Code.
//!
//! The numbers come from Claude Code itself, the only way to them: `claude -p` answering the
//! `get_usage` control request (the data of `/usage`) without sending a message, so without using
//! the plan, and `claude auth status --json` for who is logged in (at the start, when the popup
//! opens and after a failed reading). It's the bar's only child process, and a heavy one (~0.4 s of
//! CPU, ~170 MB while it runs, under half a second): every 5 minutes while a bar shows and no
//! listed game is in front, and when the popup opens if the numbers are more than a minute old.
//! It runs without a window, at idle priority, with EcoQoS and low memory priority, in a job that
//! ends it with the bar, from a worker thread that waits for it (with a timeout) and then wakes
//! the loop. Nothing of the account's setup is loaded (`--safe-mode`, `--strict-mcp-config`) and no
//! session is saved; the variables a parent Claude Code passes on (`CLAUDE_*`, `ANTHROPIC_*`) are
//! removed, so it's always the plain CLI of the config dir. `claude.exe` is found in `path`, on the
//! PATH, in `~\.local\bin`, or else the newest one the Claude desktop app carries.
//!
//! With made-up data (see `mock`) the account is the one of `claude.json` whose `dir` is the
//! widget's (`~/.claude` by default): nothing runs, and the buttons do nothing.

use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{FILETIME, HANDLE, SYSTEMTIME, WAIT_TIMEOUT};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows::core::{HSTRING, PCWSTR};

use super::{Input, Reply, Signal};
use crate::config::Options;
use crate::i18n::t;
use crate::json::Json;
use crate::log;
use crate::process;
use crate::theme::{Rgba, Theme};
use crate::ui::{Align, Font, Fonts, Item, Pill, Rect, Scene, Weight};
use crate::wake::{Timer, Wake};

const ORANGE: &str = "#d97757";
const USAGE: &str = "https://claude.ai/settings/usage";
const SPARK: &str = "\u{273B}";
/// How often the numbers are read, in ms; the first reading a few seconds after the start, so it
/// doesn't weigh on it.
const EVERY: i32 = 5 * 60 * 1000;
const FIRST: i64 = 3000;
/// Numbers younger than this, in seconds, aren't read again when the popup opens.
const FRESH: i64 = 60;
/// How often the times in the open popup follow the clock, in ms.
const TICK: i32 = 30_000;
/// Flags of `CreateProcess`: no console window, idle priority.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const IDLE_PRIORITY_CLASS: u32 = 0x0000_0040;

// The popup's layout, in DIPs, as `system`'s.
const MARGIN: f32 = 8.0;
const TILE: f32 = 160.0;
const GAP: f32 = 8.0;
const WIDE: f32 = TILE * 2.0 + GAP;
const WIDTH: f32 = WIDE + 2.0 * MARGIN;
const PAD: f32 = 10.0;
const RADIUS: f32 = 6.0;
const HEADER_H: f32 = 52.0;
const HEAD: f32 = 14.0;
const VALUE: f32 = 22.0;
const BAR: f32 = 6.0;
const LINE: f32 = 16.0;
const TILE_H: f32 = PAD + HEAD + 4.0 + VALUE + 6.0 + BAR + 8.0 + 2.0 * LINE + PAD;
const NOTE_H: f32 = 64.0;
const BUTTON: f32 = 30.0;

/// A window of the plan: how much of it is used, and when it resets (unix seconds; None before it
/// starts, which happens with the next message).
#[derive(Clone, Debug, Default, PartialEq)]
struct Window {
    percent: f64,
    reset: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Account {
    logged_in: bool,
    email: String,
    org: String,
    plan: String,
    name: String,
    session: Option<Window>,
    week: Option<Window>,
    /// A login without plan limits, as an API key.
    no_limits: bool,
    /// The last reading failed: these numbers are those of the one before.
    stale: bool,
    /// When the numbers were read, unix seconds; 0 without numbers.
    at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Job {
    /// The numbers, and with `status` who is logged in.
    Read { status: bool },
    Login,
    Logout,
}

enum Outcome {
    Read(Account),
    Login(bool),
    Logout,
}

/// What the worker thread leaves for the main thread.
struct Shared {
    wake: Arc<Wake>,
    outcome: Mutex<Option<Outcome>>,
}

pub struct Claude {
    path: Option<PathBuf>,
    /// The Claude Code config dir; None for `~\.claude`.
    dir: Option<PathBuf>,
    color: String,
    warning: f64,
    critical: f64,
    account: Option<Account>,
    busy: Option<Job>,
    /// The next reading asks who is logged in too.
    needs_status: bool,
    login_failed: bool,
    confirm_logout: bool,
    missing: bool,
    shared: Arc<Shared>,
    timer: Timer,
    tick: Timer,
    shown: bool,
    game: bool,
    open: bool,
    /// With made-up data (see `mock`), the config dir as written in the config, shown in place of
    /// the real path: nothing is read or run.
    mock: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Slot {
    Wake,
    Refresh,
    Tick,
}

impl Claude {
    pub fn new(options: &Options) -> Claude {
        let mut c = Claude {
            path: options.str("path").filter(|p| !p.is_empty()).map(expand),
            dir: options.str("dir").filter(|d| !d.is_empty()).map(expand),
            color: options.str("color").unwrap_or(ORANGE).to_string(),
            warning: options.f64("warning").unwrap_or(75.0),
            critical: options.f64("critical").unwrap_or(90.0),
            account: None,
            busy: None,
            needs_status: true,
            login_failed: false,
            confirm_logout: false,
            missing: false,
            shared: Arc::new(Shared { wake: Wake::new(), outcome: Mutex::new(None) }),
            timer: Timer::new(),
            tick: Timer::new(),
            shown: true,
            game: false,
            open: false,
            mock: None,
        };
        if let Some(m) = crate::mock::load("claude") {
            let dir = options.str("dir").filter(|d| !d.is_empty()).unwrap_or("~/.claude");
            // An instance without its account in the file is logged out
            c.account = Some(m["accounts"].array().iter().find(|a| a["dir"].str().unwrap_or("~/.claude") == dir).map(mock_account).unwrap_or_default());
            c.mock = Some(dir.replace('/', "\\"));
            return c;
        }
        c.schedule(true);
        c
    }

    /// The refresh timer runs while a bar shows without a listed game, and while there's an
    /// account to read (before the first reading, or logged in): logged out, or without Claude
    /// Code, nothing runs until the popup opens. `soon` starts it again with the first reading in
    /// a few seconds.
    fn schedule(&mut self, soon: bool) {
        let account = !self.missing && self.account.as_ref().is_none_or(|a| a.logged_in);
        if self.shown && !self.game && account && self.mock.is_none() {
            if soon {
                self.timer.stop();
                self.timer.start(FIRST, EVERY);
            } else {
                self.timer.start(EVERY as i64, EVERY);
            }
        } else {
            self.timer.stop();
        }
    }

    fn slots(&self) -> Vec<(Slot, HANDLE)> {
        let mut slots: Vec<(Slot, HANDLE)> = self.shared.wake.handle().map(|h| (Slot::Wake, h)).into_iter().collect();
        slots.extend(self.timer.handle().map(|h| (Slot::Refresh, h)));
        slots.extend(self.tick.handle().map(|h| (Slot::Tick, h)));
        slots
    }

    /// Starts a job on a worker thread, unless one runs.
    fn start(&mut self, job: Job) {
        if self.busy.is_some() || self.mock.is_some() {
            return;
        }
        if !self.path.as_ref().is_some_and(|p| p.is_file()) {
            self.path = find_claude();
        }
        let Some(path) = self.path.clone() else {
            if !self.missing {
                log!("claude: no Claude Code found");
            }
            self.missing = true;
            self.schedule(false);
            return;
        };
        self.missing = false;
        let job = match job {
            Job::Read { status } => Job::Read { status: status || self.needs_status || !self.account.as_ref().is_some_and(|a| a.logged_in) },
            other => other,
        };
        self.busy = Some(job);
        let shared = self.shared.clone();
        let dir = self.dir.clone();
        let previous = self.account.clone();
        let spawned = std::thread::Builder::new().name("claude".into()).stack_size(256 * 1024).spawn(move || {
            let outcome = match job {
                Job::Read { status } => Outcome::Read(read(&path, &dir, status, previous)),
                Job::Login => {
                    let out = claude(&path, &dir, &["auth", "login"], None, 300);
                    if !out.ok {
                        let file = crate::log::path().with_file_name("claude-login.log");
                        let _ = std::fs::write(&file, format!("{}\n{}", out.stdout, out.stderr));
                    }
                    Outcome::Login(out.ok)
                }
                Job::Logout => {
                    claude(&path, &dir, &["auth", "logout"], None, 20);
                    Outcome::Logout
                }
            };
            *lock(&shared.outcome) = Some(outcome);
            shared.wake.set();
        });
        if let Err(e) = spawned {
            log!("claude: no worker thread ({e})");
            self.busy = None;
        }
    }

    /// Takes the worker's outcome. True if what the widget shows changed.
    fn finish(&mut self) -> bool {
        let Some(outcome) = lock(&self.shared.outcome).take() else { return false };
        let job = self.busy.take();
        match outcome {
            Outcome::Read(account) => {
                if let Some(Job::Read { status: true }) = job {
                    self.needs_status = false;
                }
                // A reading that failed: who is logged in is asked again next time
                if account.stale || (account.logged_in && account.session.is_none() && !account.no_limits) {
                    self.needs_status = true;
                }
                self.account = Some(account);
            }
            Outcome::Login(ok) => {
                self.login_failed = !ok;
                self.needs_status = true;
                self.start(Job::Read { status: true });
            }
            Outcome::Logout => {
                self.account = Some(Account::default());
                self.needs_status = true;
            }
        }
        self.schedule(false);
        true
    }

    fn level(&self, theme: &Theme, percent: f64, normal: Rgba) -> Rgba {
        theme.level(percent, self.warning, self.critical, normal)
    }

    fn spark_color(&self, theme: &Theme) -> Rgba {
        theme.color(&self.color).unwrap_or(theme.primary)
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl super::Widget for Claude {
    fn pill(&self, theme: &Theme) -> Option<Pill> {
        let a = self.account.as_ref();
        let numbers = a.and_then(|a| Some((a.session.as_ref()?, a.week.as_ref()?, a.stale)));
        let Some((session, week, stale)) = numbers else {
            let text = match a {
                Some(a) if !a.logged_in => t("Accedi", "Sign in"),
                _ => "–",
            };
            return Some(Pill::new(vec![Item::text(SPARK, theme.subtext), Item::text(text, theme.subtext).small()]).gap(6.0));
        };
        let color = |p: f64| if stale { theme.subtext } else { self.level(theme, p, theme.text) };
        // No fixed widths: the numbers change at most every 5 minutes
        let mut items = vec![
            Item::text(SPARK, if stale { theme.subtext } else { self.spark_color(theme) }),
            Item::text(format!("{}%", session.percent), color(session.percent)).space(6.0),
            Item::text("·", theme.subtext),
            Item::text(format!("{}%", week.percent), color(week.percent)),
        ];
        if let Some(reset) = session.reset {
            items.push(Item::text("·", theme.subtext));
            items.push(Item::text(clock(reset), theme.subtext));
        }
        Some(Pill::new(items).gap(4.0))
    }

    /// The session's share alone.
    fn compact(&self, theme: &Theme) -> Option<Pill> {
        let a = self.account.as_ref()?;
        let session = a.session.as_ref()?;
        let color = if a.stale { theme.subtext } else { self.level(theme, session.percent, theme.text) };
        let spark = if a.stale { theme.subtext } else { self.spark_color(theme) };
        Some(Pill::new(vec![Item::text(SPARK, spark), Item::text(format!("{}%", session.percent), color).space(6.0)]))
    }

    fn handles(&self) -> Vec<HANDLE> {
        self.slots().into_iter().map(|(_, h)| h).collect()
    }

    fn signaled_at(&mut self, index: usize) -> bool {
        match self.slots().get(index).map(|(slot, _)| *slot) {
            Some(Slot::Wake) => self.finish(),
            Some(Slot::Refresh) => {
                self.start(Job::Read { status: false });
                // Shows the popup's `Aggiornamento…`
                self.open
            }
            // The times follow the clock
            Some(Slot::Tick) => self.open,
            None => false,
        }
    }

    fn event(&mut self, signal: &Signal) -> bool {
        match signal {
            Signal::Shown(shown) if *shown != self.shown => {
                self.shown = *shown;
                self.schedule(false);
                false
            }
            Signal::Game(game) => {
                self.game = *game;
                self.schedule(false);
                false
            }
            // A login may have expired during the sleep
            Signal::Resume => {
                self.needs_status = true;
                self.schedule(true);
                false
            }
            _ => false,
        }
    }

    fn has_popup(&self) -> bool {
        true
    }

    fn popup_shown(&mut self, open: bool) {
        self.open = open;
        self.confirm_logout = false;
        if open {
            let old = self.account.as_ref().is_none_or(|a| now() - a.at > FRESH);
            if old {
                self.start(Job::Read { status: true });
            }
            self.tick.start(TICK as i64, TICK);
        } else {
            self.tick.stop();
        }
    }

    fn popup(&self, theme: &Theme, fonts: &Fonts) -> Option<Scene> {
        let a = self.account.as_ref();
        let reading = self.busy.is_some();
        let logged_in = a.is_some_and(|a| a.logged_in);
        let has_numbers = a.is_some_and(|a| a.session.is_some() && a.week.is_some());
        let body = if self.missing || !logged_in || !has_numbers { NOTE_H } else { TILE_H };
        let header = if logged_in { HEADER_H + GAP } else { 0.0 };
        let links = if logged_in { 2.0 } else { 1.0 };
        let height = MARGIN + header + body + GAP + links * BUTTON + 6.0;
        let mut scene = Scene::new(WIDTH, height);
        let mut y = MARGIN;
        let small = Font::new(theme.small_size, Weight::Medium);
        let regular = Font::new(theme.font_size, Weight::Regular);

        // Who: the spark, the name, the plan and the email; how old the numbers are, and ↻
        if let Some(a) = a.filter(|a| a.logged_in) {
            let spark = Rect::new(MARGIN + 4.0, y + 8.0, 34.0, 34.0);
            scene.fill(spark, 17.0, self.spark_color(theme).alpha(0.18));
            scene.text(spark, SPARK, Font::new(18.0, Weight::SemiBold), self.spark_color(theme), Align::Center);
            let refresh = Rect::new(MARGIN + WIDE - 30.0, y + 12.0, 28.0, 26.0);
            scene.text(refresh, "\u{E72C}", Font::icons(12.0), theme.text, Align::Center);
            if !reading {
                scene.zone(refresh, theme.row_radius, "refresh");
            }
            let (age_text, age_color) = if reading {
                (t("Aggiornamento…", "Updating…").to_string(), theme.subtext)
            } else if a.at == 0 {
                (String::new(), theme.subtext)
            } else {
                (age(a.at), if a.stale { theme.color("yellow").unwrap_or(theme.subtext) } else { theme.subtext })
            };
            let age_w = fonts.width(&age_text, small).ceil();
            scene.text(Rect::new(refresh.x - 4.0 - age_w, y, age_w, HEADER_H), age_text, small, age_color, Align::End);
            let x = spark.x + spark.w + 10.0;
            let w = refresh.x - 12.0 - age_w - x;
            let title = if a.name.is_empty() { a.email.as_str() } else { a.name.as_str() };
            let plan = plan_name(&a.plan);
            let title_font = Font::new(theme.font_size + 1.0, Weight::SemiBold);
            let plan_w = if plan.is_empty() { 0.0 } else { fonts.width(&plan, small).ceil() + 14.0 };
            let title_w = fonts.width(title, title_font).ceil().min(w - plan_w);
            scene.text(Rect::new(x, y + 7.0, title_w, 20.0), title, title_font, theme.text, Align::Start);
            if !plan.is_empty() {
                let badge = Rect::new(x + title_w + 6.0, y + 9.0, plan_w - 6.0, 16.0);
                scene.fill(badge, 8.0, self.spark_color(theme).alpha(0.18));
                scene.text(badge, plan, small, self.spark_color(theme), Align::Center);
            }
            let line = if self.confirm_logout {
                (t("Fai clic di nuovo su Esci per uscire", "Click Sign out again to sign out").to_string(), theme.color("red").unwrap_or(theme.text))
            } else {
                (details(a), theme.subtext)
            };
            scene.text(Rect::new(x, y + 28.0, w, 16.0), line.0, small, line.1, Align::Start);
            y += HEADER_H + GAP;
        }

        // The numbers, or why there are none
        let red = theme.color("red").unwrap_or(theme.text);
        let yellow = theme.color("yellow").unwrap_or(theme.text);
        let note = |scene: &mut Scene, y: f32, color: Rgba, text: &str, detail: &str| {
            let r = Rect::new(MARGIN, y, WIDE, NOTE_H);
            scene.fill(r, RADIUS, theme.tile(color, true));
            scene.text(Rect::new(r.x + PAD, r.y + 12.0, r.w - 2.0 * PAD, 20.0), text, Font::new(theme.font_size, Weight::SemiBold), color, Align::Start);
            scene.text(Rect::new(r.x + PAD, r.y + 34.0, r.w - 2.0 * PAD, 18.0), detail, small, theme.subtext, Align::Start);
        };
        let dir = match &self.mock {
            Some(dir) => dir.clone(),
            None => self.dir.clone().unwrap_or_else(|| home().join(".claude")).display().to_string(),
        };
        if self.missing {
            note(&mut scene, y, red, t("Claude Code non trovato", "Claude Code not found"), t("Installalo, o indica path nella config", "Install it, or set path in the config"));
        } else if a.is_none() {
            note(&mut scene, y, theme.primary, t("Lettura…", "Reading…"), &dir);
        } else if !logged_in {
            let (text, color) = if self.busy == Some(Job::Login) {
                (t("Accedi nel browser…", "Sign in in the browser…"), theme.primary)
            } else if self.login_failed {
                (t("Accesso non riuscito: riprova", "Sign-in failed: try again"), red)
            } else {
                (t("Nessun account", "No account"), theme.subtext)
            };
            note(&mut scene, y, color, text, &dir);
        } else if a.is_some_and(|a| a.no_limits) {
            note(&mut scene, y, yellow, t("Questo accesso non ha i limiti di un piano", "This login has no plan limits"), t("Una chiave API, o un'organizzazione senza limiti", "An API key, or an organization without limits"));
        } else if let Some((session, week, stale)) = a.and_then(|a| Some((a.session.as_ref()?, a.week.as_ref()?, a.stale))) {
            for (i, (label, w)) in [(t("Sessione", "Session"), session), (t("Settimana", "Week"), week)].into_iter().enumerate() {
                let r = Rect::new(MARGIN + i as f32 * (TILE + GAP), y, TILE, TILE_H);
                let color = if stale { theme.subtext } else { self.level(theme, w.percent, self.spark_color(theme)) };
                scene.fill(r, RADIUS, theme.tile(color, !stale && w.percent >= self.warning));
                scene.text(Rect::new(r.x + PAD, r.y + PAD, r.w - 2.0 * PAD, HEAD), label, Font::new(theme.small_size, Weight::SemiBold), theme.subtext, Align::Start);
                let value_color = if stale { theme.subtext } else { self.level(theme, w.percent, theme.text) };
                let vy = r.y + PAD + HEAD + 4.0;
                scene.text(Rect::new(r.x + PAD, vy, r.w - 2.0 * PAD, VALUE), format!("{}%", w.percent), Font::new(17.0, Weight::SemiBold), value_color, Align::Start);
                let track = Rect::new(r.x + PAD, vy + VALUE + 6.0, r.w - 2.0 * PAD, BAR);
                scene.fill(track, BAR / 2.0, theme.text.alpha(0.10));
                let filled = (track.w * (w.percent / 100.0).clamp(0.0, 1.0) as f32).round();
                if filled > 0.0 {
                    scene.fill(Rect::new(track.x, track.y, filled.max(BAR), BAR), BAR / 2.0, color);
                }
                let ly = track.y + BAR + 8.0;
                let when = match w.reset {
                    Some(reset) => format!("{} {}", t("Si azzera", "Resets"), reset_clock(reset)),
                    None => String::new(),
                };
                scene.text(Rect::new(r.x + PAD, ly, r.w - 2.0 * PAD, LINE), when, small, theme.text, Align::Start);
                scene.text(Rect::new(r.x + PAD, ly + LINE, r.w - 2.0 * PAD, LINE), reset_in(w.reset), small, theme.subtext, Align::Start);
            }
        }
        y += body + GAP;

        // Sign in or out, and the usage page
        if !logged_in {
            if !self.missing && a.is_some() && self.busy != Some(Job::Login) {
                scene.text(Rect::new(MARGIN + PAD, y, WIDE - 2.0 * PAD, BUTTON), t("Accedi", "Sign in"), regular, theme.primary, Align::Start);
                scene.zone(Rect::new(MARGIN, y, WIDE, BUTTON), theme.row_radius, "login");
            }
        } else {
            scene.text(Rect::new(MARGIN + PAD, y, WIDE - 2.0 * PAD, BUTTON), t("Utilizzo su claude.ai", "Usage on claude.ai"), regular, theme.primary, Align::Start);
            scene.zone(Rect::new(MARGIN, y, WIDE, BUTTON), theme.row_radius, "usage");
            y += BUTTON;
            let color = if self.confirm_logout { red } else { theme.subtext };
            scene.text(Rect::new(MARGIN + PAD, y, WIDE - 2.0 * PAD, BUTTON), t("Esci", "Sign out"), regular, color, Align::Start);
            if !reading {
                scene.zone(Rect::new(MARGIN, y, WIDE, BUTTON), theme.row_radius, "logout");
            }
        }
        Some(scene)
    }

    fn popup_input(&mut self, input: &Input) -> Reply {
        let Input::Action(action) = input else { return Reply::Nothing };
        if self.mock.is_some() {
            return Reply::Nothing;
        }
        match *action {
            "refresh" => {
                self.start(Job::Read { status: true });
                Reply::Redraw
            }
            "usage" => {
                process::open(USAGE);
                Reply::Opened
            }
            "login" => {
                self.login_failed = false;
                self.start(Job::Login);
                Reply::Redraw
            }
            "logout" => {
                if self.confirm_logout {
                    self.confirm_logout = false;
                    self.start(Job::Logout);
                } else {
                    self.confirm_logout = true;
                }
                Reply::Redraw
            }
            _ => Reply::Nothing,
        }
    }
}

// ---- Claude Code ---------------------------------------------------------------------------------

struct Output {
    ok: bool,
    stdout: String,
    stderr: String,
}

/// The job every child goes in: it ends them if the bar ends first.
fn job() -> Option<HANDLE> {
    struct Job(HANDLE);
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}
    static JOB: OnceLock<Option<Job>> = OnceLock::new();
    JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(None, PCWSTR::null()).ok()?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(job, JobObjectExtendedLimitInformation, &limits as *const _ as *const _, size_of_val(&limits) as u32).ok()?;
        Some(Job(job))
    })
    .as_ref()
    .map(|j| j.0)
}

/// Runs Claude Code with `args` for the account of `dir`, `input` on its stdin, at most `timeout`
/// seconds: no window, idle priority, EcoQoS, in the job; the variables of a parent Claude Code
/// removed.
fn claude(path: &Path, dir: &Option<PathBuf>, args: &[&str], input: Option<&str>, timeout: u32) -> Output {
    let mut command = Command::new(path);
    command
        .args(args)
        .current_dir(home())
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW | IDLE_PRIORITY_CLASS);
    for (name, _) in std::env::vars_os() {
        let upper = name.to_string_lossy().to_uppercase();
        if upper.starts_with("CLAUDE") || upper.starts_with("ANTHROPIC") {
            command.env_remove(&name);
        }
    }
    command.env("DISABLE_AUTOUPDATER", "1").env("DISABLE_TELEMETRY", "1").env("DISABLE_ERROR_REPORTING", "1");
    if let Some(dir) = dir {
        command.env("CLAUDE_CONFIG_DIR", dir);
    }
    let failed = |e: String| Output { ok: false, stdout: String::new(), stderr: e };
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => return failed(e.to_string()),
    };
    let handle = HANDLE(child.as_raw_handle());
    process::make_small(handle);
    if let Some(job) = job() {
        unsafe {
            let _ = AssignProcessToJobObject(job, handle);
        }
    }
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        use std::io::Write;
        let _ = stdin.write_all(input.as_bytes());
    }
    // The pipes are read while it runs, so a long answer never blocks it
    let read = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::Builder::new().stack_size(64 * 1024).spawn(move || {
            let mut s = String::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_string(&mut s);
            }
            s
        })
    };
    let stdout = read(child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>));
    let stderr = read(child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>));
    if unsafe { WaitForSingleObject(handle, timeout * 1000) } == WAIT_TIMEOUT {
        log!("claude: {} took more than {timeout} s", args.join(" "));
        let _ = child.kill();
    }
    let ok = child.wait().is_ok_and(|s| s.success());
    let text = |t: std::io::Result<std::thread::JoinHandle<String>>| t.ok().and_then(|h| h.join().ok()).unwrap_or_default();
    Output { ok, stdout: text(stdout), stderr: text(stderr) }
}

/// The account and its numbers; with `status` who is logged in too. A failed reading keeps the
/// numbers of `previous`, stale.
fn read(path: &Path, dir: &Option<PathBuf>, status: bool, previous: Option<Account>) -> Account {
    let mut account = match (&previous, status) {
        (Some(p), false) if p.logged_in => Account { session: None, week: None, no_limits: false, stale: false, at: 0, ..p.clone() },
        _ => {
            let out = claude(path, dir, &["auth", "status", "--json"], None, 20);
            let Some(status) = Json::parse(out.stdout.trim()) else {
                // No answer at all: the numbers of before, stale
                return previous.map(|p| Account { stale: true, ..p }).unwrap_or_default();
            };
            if status["loggedIn"].bool() != Some(true) {
                return Account::default();
            }
            let text = |k: &str| status[k].str().unwrap_or_default().to_string();
            Account { logged_in: true, email: text("email"), org: text("orgName"), plan: text("subscriptionType"), name: display_name(dir), ..Default::default() }
        }
    };
    let request = r#"{"type":"control_request","request_id":"usage","request":{"subtype":"get_usage","skip_behaviors":true}}"#;
    let args = ["-p", "--safe-mode", "--strict-mcp-config", "--no-session-persistence", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose"];
    let out = claude(path, dir, &args, Some(&format!("{request}\n")), 30);
    match parse_usage(&out.stdout) {
        Some(Ok((session, week))) => (account.session, account.week, account.at) = (Some(session), Some(week), now()),
        Some(Err(())) => account.no_limits = true,
        None => {
            if let Some(p) = previous.filter(|p| p.email == account.email && p.session.is_some()) {
                (account.session, account.week, account.at, account.stale) = (p.session, p.week, p.at, true);
            }
        }
    }
    account
}

/// The session and the week from the answer to `get_usage`, or Err for a login without limits.
fn parse_usage(stdout: &str) -> Option<Result<(Window, Window), ()>> {
    let window = |w: &Json| Window { percent: w["utilization"].f64().unwrap_or(0.0).round(), reset: w["resets_at"].str().and_then(iso8601) };
    stdout.lines().filter_map(|l| Json::parse(l.trim())).find_map(|v| {
        if v["type"].str() != Some("control_response") || v["response"]["request_id"].str() != Some("usage") {
            return None;
        }
        let r = &v["response"]["response"];
        if r["rate_limits_available"].bool() == Some(false) {
            return Some(Err(()));
        }
        let limits = &r["rate_limits"];
        if limits.is_null() {
            return None;
        }
        Some(Ok((window(&limits["five_hour"]), window(&limits["seven_day"]))))
    })
}

/// A made-up account of `claude.json` (see `mock`): `at` is how many minutes ago it was read, a
/// window's `reset` in how many minutes it resets.
fn mock_account(m: &Json) -> Account {
    let text = |k: &str| m[k].str().unwrap_or_default().to_string();
    let window = |w: &Json| {
        (!w.is_null()).then(|| Window { percent: w["percent"].f64().unwrap_or(0.0).round(), reset: w["reset"].f64().map(|min| now() + (min * 60.0) as i64) })
    };
    Account {
        logged_in: m["logged_in"].bool().unwrap_or(true),
        email: text("email"),
        org: text("org"),
        plan: text("plan"),
        name: text("name"),
        session: window(&m["session"]),
        week: window(&m["week"]),
        no_limits: m["no_limits"].bool().unwrap_or(false),
        stale: m["stale"].bool().unwrap_or(false),
        at: now() - (m["at"].f64().unwrap_or(0.0) * 60.0) as i64,
    }
}

/// The account's name, from Claude Code's `.claude.json`.
fn display_name(dir: &Option<PathBuf>) -> String {
    let file = match dir {
        Some(d) => d.join(".claude.json"),
        None => home().join(".claude.json"),
    };
    let Some(config) = std::fs::read_to_string(file).ok().and_then(|s| Json::parse(&s)) else { return String::new() };
    let account = &config["oauthAccount"];
    account["displayName"].str().or(account["fullName"].str()).unwrap_or_default().to_string()
}

/// `claude.exe`: on the PATH, in `~\.local\bin`, or the newest the Claude desktop app carries in
/// its data (`claude-code\<version>\<build>\claude.exe`).
fn find_claude() -> Option<PathBuf> {
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let exe = dir.join("claude.exe");
            if exe.is_file() {
                return Some(exe);
            }
        }
    }
    let local = home().join(".local").join("bin").join("claude.exe");
    if local.is_file() {
        return Some(local);
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(l) = std::env::var_os("LOCALAPPDATA") {
        // The packaged app keeps one folder per installation
        if let Ok(entries) = std::fs::read_dir(PathBuf::from(&l).join("Claude-Data")) {
            roots.extend(entries.flatten().map(|e| e.path().join("claude-code")));
        }
    }
    if let Some(a) = std::env::var_os("APPDATA") {
        roots.push(PathBuf::from(a).join("Claude").join("claude-code"));
    }
    let mut found: Vec<(Vec<u32>, PathBuf)> = Vec::new();
    for root in roots {
        let Ok(versions) = std::fs::read_dir(&root) else { continue };
        for version in versions.flatten() {
            let number: Vec<u32> = version.file_name().to_string_lossy().split('.').map(|p| p.parse().unwrap_or(0)).collect();
            let Ok(builds) = std::fs::read_dir(version.path()) else { continue };
            for build in builds.flatten() {
                let exe = build.path().join("claude.exe");
                if exe.is_file() {
                    found.push((number.clone(), exe));
                }
            }
        }
    }
    found.sort();
    found.pop().map(|(_, exe)| exe)
}

fn home() -> PathBuf {
    std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

/// `~\x` and `~/x` in the user's folder.
fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        Some(rest) => home().join(rest),
        None => PathBuf::from(path),
    }
}

// ---- time ---------------------------------------------------------------------------------------

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// `2026-10-30T18:30:00.000+00:00` (or `Z`) as unix seconds.
fn iso8601(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let num = |from: usize, len: usize| -> Option<i64> { s.get(from..from + len)?.parse().ok() };
    if b.len() < 19 || b[4] != b'-' || b[10] != b'T' {
        return None;
    }
    let (y, m, d) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    let (hh, mm, ss) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    // Days from 1970-01-01 to the date (Howard Hinnant's days_from_civil)
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let mut t = days * 86_400 + hh * 3600 + mm * 60 + ss;
    // The offset after the seconds and their fraction
    let rest = &s[19..];
    let rest = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    if let Some(sign) = rest.chars().next().filter(|c| *c == '+' || *c == '-') {
        let oh: i64 = rest.get(1..3)?.parse().ok()?;
        let om: i64 = rest.get(4..6).and_then(|m| m.parse().ok()).unwrap_or(0);
        let offset = oh * 3600 + om * 60;
        t -= if sign == '+' { offset } else { -offset };
    }
    Some(t)
}

/// Unix seconds as the local time of Windows.
fn local(unix: i64) -> Option<SYSTEMTIME> {
    let ticks = (unix * 10_000_000 + 116_444_736_000_000_000) as u64;
    let file = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    unsafe {
        FileTimeToSystemTime(&file, &mut utc).ok()?;
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()?;
    }
    Some(local)
}

/// `17:07`, in the region's format.
fn clock(unix: i64) -> String {
    local(unix).map(|t| super::clock::format_time(&t)).unwrap_or_default()
}

/// When a window resets: `alle 14:12`, `ven 19:10` when it's more than a day away.
fn reset_clock(unix: i64) -> String {
    if unix - now() < 86_400 {
        return format!("{} {}", t("alle", "at"), clock(unix));
    }
    let day = local(unix).map(|t| super::clock::format_date(&t, &HSTRING::from("ddd"))).unwrap_or_default();
    format!("{day} {}", clock(unix))
}

/// How long until a window resets: `tra 2 h 57 min`, or that it starts with the next message.
fn reset_in(reset: Option<i64>) -> String {
    let Some(at) = reset else { return t("al prossimo messaggio", "with the next message").into() };
    let left = at - now();
    if left < 60 {
        return t("tra meno di un minuto", "in under a minute").into();
    }
    format!("{} {}", t("tra", "in"), span(left))
}

/// How old numbers read at `at` are: `adesso`, `3 min fa`.
fn age(at: i64) -> String {
    let age = now() - at;
    if age < 60 {
        return t("adesso", "just now").into();
    }
    if crate::i18n::italian() { format!("{} fa", span(age)) } else { format!("{} ago", span(age)) }
}

/// `45 min`, `2 h 57 min`, `3 g 4 h`.
fn span(seconds: i64) -> String {
    let minutes = seconds / 60;
    let (d, h, m) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    if d > 0 {
        format!("{d} {} {h} h", t("g", "d"))
    } else if h > 0 {
        format!("{h} h {m} min")
    } else {
        format!("{m} min")
    }
}

/// Under the name: the email (unless it's the name already) and the organization, but not the
/// personal one, which Claude names after the email.
fn details(a: &Account) -> String {
    let email = if a.name.is_empty() { "" } else { a.email.as_str() };
    let org = if !a.email.is_empty() && a.org.contains(&a.email) { "" } else { a.org.as_str() };
    [email, org].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")
}

/// `max` → `Max`.
fn plan_name(plan: &str) -> String {
    let mut chars = plan.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_answers() {
        let out = "{\"type\":\"system\"}\n{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"usage\",\"response\":{\"rate_limits\":{\"five_hour\":{\"utilization\":41.6,\"resets_at\":\"2026-10-30T18:30:00.000+00:00\"},\"seven_day\":{\"utilization\":31,\"resets_at\":null}}}}}\n";
        let (session, week) = parse_usage(out).unwrap().unwrap();
        assert_eq!(session.percent, 42.0);
        assert_eq!(session.reset, Some(1_793_385_000));
        assert_eq!(week, Window { percent: 31.0, reset: None });
        let none = "{\"type\":\"control_response\",\"response\":{\"request_id\":\"usage\",\"response\":{\"rate_limits_available\":false}}}";
        assert_eq!(parse_usage(none), Some(Err(())));
        assert_eq!(parse_usage("garbage\n"), None);
    }

    #[test]
    fn times() {
        assert_eq!(iso8601("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(iso8601("2026-10-30T18:30:00.000+00:00"), Some(1_793_385_000));
        assert_eq!(iso8601("2026-10-30T20:30:00+02:00"), Some(1_793_385_000));
        assert_eq!(span(45 * 60), "45 min");
        assert_eq!(span(177 * 60), "2 h 57 min");
        assert_eq!(plan_name("max"), "Max");
        let a = Account { name: "Alex".into(), email: "alex@example.com".into(), org: "alex@example.com's Organization".into(), ..Default::default() };
        assert_eq!(details(&a), "alex@example.com");
    }
}
