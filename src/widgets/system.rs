//! CPU, GPU and memory: `CPU 12%  GPU 34% 62°  RAM 43%`, each number as wide as `100%` so the pill
//! keeps its size and only its text is drawn again; yellow from `warning`, red from `critical`
//! (the temperature from the one the driver calls high). Read every `interval` seconds (5) on a
//! waitable timer with slack, so Windows can fold the wake-up into others; redrawn only when a
//! number changes. The timer stops while no bar shows, and while a game of `[games] apps` is in
//! front (the numbers stay, dimmed). See `stats` for where each number comes from.
//!
//! The popup is a dashboard of tiles, read every 2 seconds while it's open:
//!
//! - CPU and GPU side by side: the use now, the cores and the GPU's temperature, and the last 30
//!   readings as bars (kept from the start, so the graphs are full when the popup opens).
//! - Memory: in use of the total, what's committed of the limit (the RAM plus the page file).
//! - Video memory: in use of the dedicated, with the GPU's name.
//! - A tile for each fixed disk: used of the total and what's free, yellow from
//!   `disk_warning` (90%), red from `disk_critical` (95%); a click opens it in File Explorer.
//!   Read when the popup opens and every 10 seconds.
//! - The apps that use the most CPU or memory, with a switch: every process of an executable
//!   together, under the name of its version information, as `front_app` has it. All the
//!   processes come in one call of a few ms, only while the popup is open.
//! - Task Manager and the storage settings.
//!
//! What only the popup needs (the processes, the disks, the apps' names) is made when it opens
//! and dropped when it closes.
//!
//! With made-up data (see `mock`) everything comes from `system.json` (sizes in GB, shares in
//! percent): nothing is read, no timer runs, the switch works and the buttons do nothing.

use std::collections::{HashMap, VecDeque};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{CancelWaitableTimer, CreateWaitableTimerExW, SetWaitableTimerEx, TIMER_ALL_ACCESS};
use windows::core::PCWSTR;

use super::{Input, Reply, Signal, front_app};
use crate::config::Options;
use crate::i18n::{number, size, t};
use crate::json::Json;
use crate::log;
use crate::process;
use crate::stats::{self, App, CpuShare, CpuTimes, Disk, Gpu, Memory, Processes};
use crate::theme::{Rgba, Theme};
use crate::ui::{Align, Font, Fonts, Item, Pill, Rect, Scene, Weight};

/// How often the popup reads, in ms, and how soon after it opens.
const OPEN_PERIOD: i32 = 2000;
const OPEN_FIRST: i64 = 500;
/// While the popup is open the disks are read again every this many readings: 10 seconds.
const DISKS_EVERY: u32 = 5;
/// Readings in the graphs.
const BARS: usize = 30;
/// Apps in a list.
const TOP: usize = 5;

// The popup's layout, in DIPs: tiles two side by side, or one two wide.
const MARGIN: f32 = 8.0;
const TILE: f32 = 160.0;
const GAP: f32 = 8.0;
const WIDE: f32 = TILE * 2.0 + GAP;
const WIDTH: f32 = WIDE + 2.0 * MARGIN;
/// Inside a tile.
const PAD: f32 = 10.0;
const RADIUS: f32 = 6.0;
const HEAD: f32 = 14.0;
const VALUE: f32 = 22.0;
const GRAPH: f32 = 24.0;
const BAR: f32 = 6.0;
const GAUGE_H: f32 = PAD + HEAD + 4.0 + VALUE + 4.0 + GRAPH + PAD;
const METER_H: f32 = PAD + HEAD + 4.0 + VALUE + 6.0 + BAR + PAD;
const SWITCH: f32 = 26.0;
const LINE: f32 = 16.0;
const ROW: f32 = 22.0;
const APPS_H: f32 = PAD + SWITCH + 6.0 + LINE + 4.0 + TOP as f32 * ROW + 6.0;
const BUTTON: f32 = 30.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum List {
    Cpu,
    Memory,
}

/// What the open popup needs.
struct Open {
    processes: Processes,
    /// Every app at the last reading; the CPU is known from the second one.
    apps: Vec<App>,
    cpu_known: bool,
    disks: Vec<Disk>,
    video_memory: u64,
    /// The apps' names, by executable.
    names: HashMap<String, String>,
    readings: u32,
}

pub struct System {
    timer: HANDLE,
    /// The timer's period now, in ms; None while it's stopped.
    period: Option<i32>,
    interval: i32,
    warning: f64,
    critical: f64,
    disk_warning: f64,
    disk_critical: f64,
    logical: u32,
    cores: u32,
    gpu: Option<Gpu>,
    times: Option<CpuTimes>,
    share: Option<CpuShare>,
    memory: Option<Memory>,
    gpu_use: Option<f64>,
    temperature: Option<f32>,
    /// The last readings, numbered: CPU and GPU.
    history: VecDeque<(u64, u32, Option<u32>)>,
    readings: u64,
    shown: bool,
    game: bool,
    list: List,
    open: Option<Open>,
    mock: Option<Mock>,
}

/// What the made-up popup shows (see `mock`).
struct Mock {
    apps: Vec<App>,
    names: HashMap<String, String>,
    disks: Vec<Disk>,
    video_memory: u64,
}

impl System {
    pub fn new(options: &Options) -> System {
        if let Some(m) = crate::mock::load("system") {
            return System::mock(options, &m);
        }
        let timer = unsafe { CreateWaitableTimerExW(None, PCWSTR::null(), 0, TIMER_ALL_ACCESS.0) }.unwrap_or_else(|e| {
            log!("system timer: {e}");
            HANDLE::default()
        });
        let (logical, cores) = stats::processors();
        let interval = (options.f64("interval").unwrap_or(5.0).clamp(1.0, 600.0) * 1000.0) as i32;
        let mut s = System {
            timer,
            period: None,
            interval,
            warning: options.f64("warning").unwrap_or(75.0),
            critical: options.f64("critical").unwrap_or(90.0),
            disk_warning: options.f64("disk_warning").unwrap_or(90.0),
            disk_critical: options.f64("disk_critical").unwrap_or(95.0),
            logical,
            cores,
            gpu: if options.bool("gpu").unwrap_or(true) { Gpu::open() } else { None },
            times: None,
            share: None,
            memory: Memory::read(),
            gpu_use: None,
            temperature: None,
            history: VecDeque::new(),
            readings: 0,
            shown: true,
            game: false,
            list: List::Cpu,
            open: None,
            mock: None,
        };
        s.temperature = s.gpu.as_ref().and_then(Gpu::temperature);
        s.schedule();
        s
    }

    /// The widget with the made-up data of `system.json`, and no timer.
    fn mock(options: &Options, m: &Json) -> System {
        const GB: f64 = (1u64 << 30) as f64;
        let bytes = |v: &Json| (v.f64().unwrap_or(0.0) * GB) as u64;
        let share = |k: &str| m["cpu"][k].f64().unwrap_or(0.0) / 100.0;
        let (kernel, user) = (share("system"), share("user"));
        let memory = Memory { total: bytes(&m["memory"]["total"]), available: bytes(&m["memory"]["total"]).saturating_sub(bytes(&m["memory"]["used"])), committed: bytes(&m["memory"]["committed"]), commit_limit: bytes(&m["memory"]["limit"]) };
        let g = &m["gpu"];
        let gpu = (!g.is_null() && options.bool("gpu").unwrap_or(true)).then(|| Gpu::mock(g["name"].str().unwrap_or_default().to_string(), bytes(&g["dedicated"]), g["hot"].f64().map(|t| t as f32)));
        let history = |k: &str| m[k].array().iter().map(|v| percent(v.f64().unwrap_or(0.0))).collect::<Vec<u32>>();
        let (cpu, gpu_history) = (history("cpu_history"), history("gpu_history"));
        let history: VecDeque<(u64, u32, Option<u32>)> = cpu.iter().enumerate().map(|(i, c)| (i as u64, *c, gpu_history.get(i).copied())).collect();
        let mut names = HashMap::new();
        let apps = m["apps"]
            .array()
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let exe = a["exe"].str().unwrap_or_default().to_lowercase();
                names.insert(exe.clone(), a["name"].str().unwrap_or_default().to_string());
                App { exe, pid: i as u32, cpu: a["cpu"].f64().unwrap_or(0.0), memory: bytes(&a["memory"]) }
            })
            .collect();
        let disks = m["disks"]
            .array()
            .iter()
            .map(|d| Disk { root: d["root"].str().unwrap_or_default().to_string(), label: d["label"].str().unwrap_or_default().to_string(), total: bytes(&d["total"]), free: bytes(&d["free"]) })
            .collect();
        System {
            timer: HANDLE::default(),
            period: None,
            interval: 5000,
            warning: options.f64("warning").unwrap_or(75.0),
            critical: options.f64("critical").unwrap_or(90.0),
            disk_warning: options.f64("disk_warning").unwrap_or(90.0),
            disk_critical: options.f64("disk_critical").unwrap_or(95.0),
            logical: m["logical"].f64().unwrap_or(16.0) as u32,
            cores: m["cores"].f64().unwrap_or(8.0) as u32,
            gpu,
            times: None,
            share: Some(CpuShare { kernel, user, idle: (1.0 - kernel - user).max(0.0) }),
            memory: Some(memory),
            gpu_use: g["use"].f64(),
            temperature: g["temperature"].f64().map(|t| t as f32),
            readings: history.len() as u64,
            history,
            shown: true,
            game: false,
            list: List::Cpu,
            open: None,
            mock: Some(Mock { apps, names, disks, video_memory: bytes(&g["used"]) }),
        }
    }

    fn timer_ok(&self) -> bool {
        !self.timer.is_invalid() && !self.timer.0.is_null()
    }

    /// Starts, changes or stops the timer for what's going on: every 2 seconds with the popup
    /// open, every `interval` while a bar shows without a game in front, else not at all.
    fn schedule(&mut self) {
        let want = if self.open.is_some() {
            Some(OPEN_PERIOD)
        } else if self.shown && !self.game {
            Some(self.interval)
        } else {
            None
        };
        if want == self.period || !self.timer_ok() {
            return;
        }
        match want {
            Some(period) => {
                // From a stop, the readings start again: the time stopped isn't the last interval
                if self.period.is_none() {
                    self.baseline();
                }
                let first = if self.open.is_some() || self.period.is_none() { OPEN_FIRST } else { period as i64 };
                let due = -(first * 10_000);
                // A tenth of the period of slack, for Windows to fold the wake-up into others
                let slack = (period / 10).clamp(50, 1000) as u32;
                match unsafe { SetWaitableTimerEx(self.timer, &due, period, None, None, None, slack) } {
                    Ok(()) => self.period = Some(period),
                    Err(e) => log!("system timer: {e}"),
                }
            }
            None => {
                unsafe {
                    let _ = CancelWaitableTimer(self.timer);
                }
                self.period = None;
            }
        }
    }

    /// The first readings, for the next ones to compare with.
    fn baseline(&mut self) {
        self.times = CpuTimes::read();
        if let Some(gpu) = &mut self.gpu {
            gpu.usage();
        }
    }

    /// Reads the CPU, the memory and the GPU, and adds the reading to the graphs.
    fn read(&mut self) {
        let times = CpuTimes::read();
        if let (Some(old), Some(new)) = (self.times, times) {
            self.share = CpuShare::between(old, new).or(self.share);
        }
        self.times = times.or(self.times);
        self.memory = Memory::read().or(self.memory);
        if let Some(gpu) = &mut self.gpu {
            self.gpu_use = gpu.usage().or(self.gpu_use);
            self.temperature = gpu.temperature();
        }
        if let Some(share) = self.share {
            self.readings += 1;
            self.history.push_back((self.readings, percent(share.busy() * 100.0), self.gpu_use.map(percent)));
            while self.history.len() > BARS {
                self.history.pop_front();
            }
        }
    }

    /// The popup's readings: the processes, the video memory, and now and then the disks.
    fn read_open(&mut self) {
        let gpu_memory = self.gpu.as_ref().map(Gpu::memory_used);
        let Some(open) = &mut self.open else { return };
        open.readings += 1;
        if let Some(apps) = open.processes.read() {
            open.apps = apps;
            open.cpu_known = true;
        }
        if let Some(used) = gpu_memory {
            open.video_memory = used;
        }
        if open.readings.is_multiple_of(DISKS_EVERY) {
            open.disks = stats::disks();
        }
        name_apps(open);
    }

    fn cpu(&self) -> Option<u32> {
        self.share.map(|s| percent(s.busy() * 100.0))
    }

    fn ram(&self) -> Option<u32> {
        self.memory.map(|m| percent(m.percent()))
    }

    /// What the pill shows, to tell whether a reading changed it.
    fn shown_values(&self) -> (Option<u32>, Option<u32>, Option<u32>, Option<u32>, bool) {
        (self.cpu(), self.gpu_use.map(percent), self.temperature.map(|t| t.round() as u32), self.ram(), self.paused())
    }

    /// The numbers don't move: a listed game is in front, and the popup isn't open.
    fn paused(&self) -> bool {
        self.period.is_none() && self.mock.is_none()
    }

    fn level(&self, theme: &Theme, v: u32, normal: Rgba) -> Rgba {
        theme.level(v as f64, self.warning, self.critical, normal)
    }

    fn temperature_color(&self, theme: &Theme, t: f32, normal: Rgba) -> Rgba {
        match self.gpu.as_ref().and_then(|g| g.hot) {
            Some(hot) if t >= hot => theme.color("red").unwrap_or(normal),
            Some(hot) if t >= hot - 10.0 => theme.color("yellow").unwrap_or(normal),
            _ => normal,
        }
    }
}

impl Drop for System {
    fn drop(&mut self) {
        if self.timer_ok() {
            unsafe {
                let _ = CancelWaitableTimer(self.timer);
                let _ = CloseHandle(self.timer);
            }
        }
    }
}

impl super::Widget for System {
    fn pill(&self, theme: &Theme) -> Option<Pill> {
        let paused = self.paused();
        let label = |text: &str, space: Option<f32>| {
            let item = Item::text(text, theme.subtext).small();
            match space {
                Some(s) => item.space(s),
                None => item,
            }
        };
        let value = |v: Option<u32>| {
            let color = match v {
                _ if paused => theme.subtext,
                Some(v) => self.level(theme, v, theme.text),
                None => theme.subtext,
            };
            Item::text(v.map_or("–".to_string(), |v| format!("{v}%")), color).fixed("100%")
        };
        let mut items = vec![label("CPU", None), value(self.cpu())];
        if self.gpu.is_some() {
            items.push(label("GPU", Some(12.0)));
            items.push(value(self.gpu_use.map(percent)));
            if let Some(t) = self.temperature {
                let color = if paused { theme.subtext } else { self.temperature_color(theme, t, theme.text) };
                items.push(Item::text(format!("{}°", t.round()), color).fixed("100°"));
            }
        }
        items.push(label("RAM", Some(12.0)));
        items.push(value(self.ram()));
        Some(Pill::new(items).gap(4.0))
    }

    /// The memory alone, as the macOS RustyBar's compact form.
    fn compact(&self, theme: &Theme) -> Option<Pill> {
        let ram = self.ram();
        let color = match ram {
            _ if self.paused() => theme.subtext,
            Some(v) => self.level(theme, v, theme.text),
            None => theme.subtext,
        };
        let value = Item::text(ram.map_or("–".to_string(), |v| format!("{v}%")), color).fixed("100%");
        Some(Pill::new(vec![Item::text("RAM", theme.subtext).small(), value]).gap(4.0))
    }

    fn handle(&self) -> Option<HANDLE> {
        self.period.map(|_| self.timer)
    }

    fn signaled(&mut self) -> bool {
        let before = self.shown_values();
        self.read();
        if self.open.is_some() {
            self.read_open();
            return true;
        }
        self.shown_values() != before
    }

    fn event(&mut self, signal: &Signal) -> bool {
        if self.mock.is_some() {
            return false;
        }
        let before = self.shown_values();
        match signal {
            Signal::Shown(shown) => self.shown = *shown,
            Signal::Game(game) => self.game = *game,
            // The graphs start again: the minute before the sleep isn't the last one
            Signal::Resume => {
                self.history.clear();
                self.share = None;
                self.gpu_use = None;
                if let Some(gpu) = &mut self.gpu {
                    gpu.reset();
                }
                // Stopped and started again below, from new readings
                if self.period.is_some() && self.timer_ok() {
                    unsafe {
                        let _ = CancelWaitableTimer(self.timer);
                    }
                    self.period = None;
                }
            }
            Signal::Foreground(_) | Signal::TimeChanged | Signal::Trigger { .. } => return false,
        }
        self.schedule();
        self.shown_values() != before
    }

    fn has_popup(&self) -> bool {
        true
    }

    fn popup_shown(&mut self, open: bool) {
        if let Some(m) = &self.mock {
            self.open = open.then(|| Open {
                processes: Processes::new(self.logical),
                apps: m.apps.clone(),
                cpu_known: true,
                disks: m.disks.clone(),
                video_memory: m.video_memory,
                names: m.names.clone(),
                readings: 0,
            });
            return;
        }
        if open {
            let mut processes = Processes::new(self.logical);
            let apps = processes.read().unwrap_or_default();
            let mut open = Open {
                processes,
                apps,
                cpu_known: false,
                disks: stats::disks(),
                video_memory: self.gpu.as_ref().map_or(0, Gpu::memory_used),
                names: HashMap::new(),
                readings: 0,
            };
            name_apps(&mut open);
            self.open = Some(open);
        } else {
            self.open = None;
        }
        self.schedule();
    }

    fn popup(&self, theme: &Theme, fonts: &Fonts) -> Option<Scene> {
        let open = self.open.as_ref()?;
        let gpu = self.gpu.as_ref();
        let video = gpu.filter(|g| g.dedicated > 0);
        let buttons = [("taskmgr", t("Apri Gestione attività", "Open Task Manager")), ("storage", t("Impostazioni di archiviazione…", "Storage settings…"))];
        let meters = self.memory.iter().count() + video.iter().count() + open.disks.len();
        let height = MARGIN + GAUGE_H + GAP + meters as f32 * (METER_H + GAP) + APPS_H + GAP + buttons.len() as f32 * BUTTON + 6.0;
        let mut scene = Scene::new(WIDTH, height);
        let mut y = MARGIN;

        // CPU and GPU
        let cores = format!("{} core", self.cores);
        let width = if gpu.is_some() { TILE } else { WIDE };
        self.gauge(&mut scene, theme, fonts, Rect::new(MARGIN, y, width, GAUGE_H), "CPU", (cores, theme.subtext), self.cpu(), false);
        if gpu.is_some() {
            let note = match self.temperature {
                Some(t) => (format!("{} °C", t.round()), self.temperature_color(theme, t, theme.subtext)),
                None => (String::new(), theme.subtext),
            };
            self.gauge(&mut scene, theme, fonts, Rect::new(MARGIN + TILE + GAP, y, TILE, GAUGE_H), "GPU", note, self.gpu_use.map(percent), true);
        }
        y += GAUGE_H + GAP;

        // Memory
        if let Some(m) = self.memory {
            let ram = percent(m.percent());
            let committed = format!("{} {} {} {}", t("Impegnata", "Committed"), size(m.committed), t("di", "of"), size(m.commit_limit));
            let meter = Meter {
                label: t("Memoria", "Memory").into(),
                note: (committed, theme.subtext),
                value: size(m.used()),
                of: format!("{} {}", t("di", "of"), size(m.total)),
                value_color: self.level(theme, ram, theme.text),
                fraction: m.used() as f64 / m.total.max(1) as f64,
                color: self.level(theme, ram, theme.primary),
                alert: ram as f64 >= self.warning,
            };
            meter.draw(&mut scene, theme, fonts, Rect::new(MARGIN, y, WIDE, METER_H));
            y += METER_H + GAP;
        }

        // Video memory
        if let Some(gpu) = video {
            let meter = Meter {
                label: t("Memoria video", "Video memory").into(),
                note: (gpu.name.clone(), theme.subtext),
                value: size(open.video_memory),
                of: format!("{} {}", t("di", "of"), size(gpu.dedicated)),
                value_color: theme.text,
                fraction: open.video_memory as f64 / gpu.dedicated as f64,
                color: theme.primary,
                alert: false,
            };
            meter.draw(&mut scene, theme, fonts, Rect::new(MARGIN, y, WIDE, METER_H));
            y += METER_H + GAP;
        }

        // The disks
        for (i, disk) in open.disks.iter().enumerate() {
            let share = disk.percent();
            let level = |normal| theme.level(share, self.disk_warning, self.disk_critical, normal);
            let letter = disk.root.trim_end_matches('\\');
            let name = if disk.label.is_empty() { t("Disco locale", "Local Disk") } else { disk.label.as_str() };
            let meter = Meter {
                label: format!("{name} ({letter})"),
                note: (format!("{} {}", size(disk.free), t("liberi", "free")), theme.subtext),
                value: size(disk.used()),
                of: format!("{} {}", t("di", "of"), size(disk.total)),
                value_color: level(theme.text),
                fraction: share / 100.0,
                color: level(theme.primary),
                alert: share >= self.disk_warning,
            };
            let rect = Rect::new(MARGIN, y, WIDE, METER_H);
            meter.draw(&mut scene, theme, fonts, rect);
            scene.zone(rect, RADIUS, format!("disk:{i}"));
            y += METER_H + GAP;
        }

        // The apps, and the buttons
        self.apps(&mut scene, theme, fonts, open, y);
        y += APPS_H + GAP;
        for (action, label) in buttons {
            let rect = Rect::new(MARGIN, y, WIDE, BUTTON);
            scene.text(Rect::new(MARGIN + PAD, y, WIDE - 2.0 * PAD, BUTTON), label, Font::new(theme.font_size, Weight::Regular), theme.primary, Align::Start);
            scene.zone(rect, theme.row_radius, action);
            y += BUTTON;
        }
        Some(scene)
    }

    fn popup_input(&mut self, input: &Input) -> Reply {
        let Input::Action(action) = input else { return Reply::Nothing };
        match *action {
            "list:cpu" | "list:memory" => {
                let list = if *action == "list:cpu" { List::Cpu } else { List::Memory };
                if list == self.list {
                    return Reply::Nothing;
                }
                self.list = list;
                Reply::Redraw
            }
            _ if self.mock.is_some() => Reply::Nothing,
            "taskmgr" => {
                process::open("taskmgr.exe");
                Reply::Opened
            }
            "storage" => {
                process::open("ms-settings:storagesense");
                Reply::Opened
            }
            _ => {
                let disk = action.strip_prefix("disk:").and_then(|i| i.parse::<usize>().ok());
                let Some(root) = disk.and_then(|i| self.open.as_ref()?.disks.get(i)).map(|d| d.root.clone()) else { return Reply::Nothing };
                process::open(&root);
                Reply::Opened
            }
        }
    }
}

impl System {
    /// CPU or GPU: the use now and the last readings as bars, the newest on the right.
    #[allow(clippy::too_many_arguments)]
    fn gauge(&self, scene: &mut Scene, theme: &Theme, fonts: &Fonts, r: Rect, label: &str, note: (String, Rgba), value: Option<u32>, gpu: bool) {
        let v = value.unwrap_or(0);
        let color = self.level(theme, v, theme.primary);
        scene.fill(r, RADIUS, theme.tile(color, v as f64 >= self.warning));
        heading(scene, theme, fonts, r, label, note);
        let value_text = value.map_or("–".to_string(), |v| format!("{v}%"));
        scene.text(Rect::new(r.x + PAD, r.y + PAD + HEAD + 4.0, r.w - 2.0 * PAD, VALUE), value_text, big(), self.level(theme, v, theme.text), Align::Start);
        // The bars, right to left from the newest
        let inner = r.w - 2.0 * PAD;
        let step = inner / BARS as f32;
        let bottom = r.y + r.h - PAD;
        let readings: Vec<u32> = self.history.iter().filter_map(|&(_, cpu, g)| if gpu { g } else { Some(cpu) }).collect();
        for (i, v) in readings.iter().rev().enumerate() {
            let right = (r.x + r.w - PAD - i as f32 * step).round();
            let left = (right - step + 1.5).round().min(right - 1.0);
            let h = (2.0 + (GRAPH - 2.0) * (*v).min(100) as f32 / 100.0).round();
            scene.fill(Rect::new(left, bottom - h, right - left, h), 1.0, self.level(theme, *v, theme.primary));
        }
    }

    /// The apps that use the most of what the switch picks, as tall as `TOP` of them so the popup
    /// doesn't change size with it.
    fn apps(&self, scene: &mut Scene, theme: &Theme, fonts: &Fonts, open: &Open, y: f32) {
        let r = Rect::new(MARGIN, y, WIDE, APPS_H);
        scene.fill(r, RADIUS, theme.tile(theme.primary, false));
        let inner = WIDE - 2.0 * PAD;
        // The switch
        let track = Rect::new(r.x + PAD, r.y + PAD, inner, SWITCH);
        scene.fill(track, RADIUS + 1.0, theme.text.alpha(0.06));
        let segment = (inner - 4.0 - 2.0) / 2.0;
        let small = Font::new(theme.small_size, Weight::SemiBold);
        for (i, (list, action, label)) in [(List::Cpu, "list:cpu", "CPU"), (List::Memory, "list:memory", t("Memoria", "Memory"))].into_iter().enumerate() {
            let s = Rect::new(track.x + 2.0 + i as f32 * (segment + 2.0), track.y + 2.0, segment, SWITCH - 4.0);
            let on = list == self.list;
            if on {
                scene.fill(s, RADIUS - 1.0, theme.primary);
            }
            scene.text(s, label, small, if on { theme.on_primary } else { theme.text }, Align::Center);
            scene.zone(s, RADIUS - 1.0, action);
        }
        // A line about the list
        let line_y = track.y + SWITCH + 6.0;
        let line = match (self.list, self.share) {
            (List::Cpu, Some(s)) => format!(
                "{} {} · {} {} · {} {}",
                t("Sistema", "System"),
                pct(s.kernel),
                t("Utente", "User"),
                pct(s.user),
                t("Inattivo", "Idle"),
                pct(s.idle)
            ),
            (List::Cpu, None) => String::new(),
            (List::Memory, _) => t("Memoria privata in uso, come in Gestione attività", "Private memory in use, as in Task Manager").into(),
        };
        let small_font = Font::new(theme.small_size, Weight::Medium);
        scene.text(Rect::new(r.x + PAD + 2.0, line_y, inner - 2.0, LINE), line, small_font, theme.subtext, Align::Start);
        // The rows
        let rows_y = line_y + LINE + 4.0;
        if self.list == List::Cpu && !open.cpu_known {
            for (i, share) in [0.8, 0.6, 0.7].into_iter().enumerate() {
                let row = rows_y + i as f32 * ROW;
                scene.fill(Rect::new(r.x + PAD + 2.0, row + (ROW - 10.0) / 2.0, inner * share, 10.0), 4.0, theme.text.alpha(0.08));
            }
            return;
        }
        let apps = match self.list {
            List::Cpu => stats::top(&open.apps, TOP, 0.05, |a| a.cpu),
            List::Memory => stats::top(&open.apps, TOP, 1, |a| a.memory),
        };
        let regular = Font::new(theme.font_size, Weight::Regular);
        let medium = Font::new(theme.font_size, Weight::Medium);
        for (i, app) in apps.iter().enumerate() {
            let row = rows_y + i as f32 * ROW;
            let value = match self.list {
                List::Cpu => format!("{}%", number(app.cpu, 1)),
                List::Memory => size(app.memory),
            };
            let value_w = fonts.width(&value, medium);
            let name = open.names.get(&app.exe).cloned().unwrap_or_else(|| fallback_name(&app.exe));
            scene.text(Rect::new(r.x + PAD + 2.0, row, inner - value_w - 12.0, ROW), name, regular, theme.text, Align::Start);
            scene.text(Rect::new(r.x + r.w - PAD - 2.0 - value_w - 1.0, row, value_w + 1.0, ROW), value, medium, theme.subtext, Align::End);
        }
    }
}

/// A tile with a bar: a heading, a value with its total, and how full it is.
struct Meter {
    label: String,
    note: (String, Rgba),
    value: String,
    of: String,
    value_color: Rgba,
    fraction: f64,
    color: Rgba,
    alert: bool,
}

impl Meter {
    fn draw(&self, scene: &mut Scene, theme: &Theme, fonts: &Fonts, r: Rect) {
        scene.fill(r, RADIUS, theme.tile(self.color, self.alert));
        heading(scene, theme, fonts, r, &self.label, self.note.clone());
        let y = r.y + PAD + HEAD + 4.0;
        let value_w = fonts.width(&self.value, big()).ceil();
        scene.text(Rect::new(r.x + PAD, y, value_w + 1.0, VALUE), self.value.as_str(), big(), self.value_color, Align::Start);
        let small = Font::new(theme.small_size, Weight::Medium);
        scene.text(Rect::new(r.x + PAD + value_w + 5.0, y + 3.0, r.w - 2.0 * PAD - value_w - 5.0, VALUE), self.of.as_str(), small, theme.subtext, Align::Start);
        let track = Rect::new(r.x + PAD, y + VALUE + 6.0, r.w - 2.0 * PAD, BAR);
        scene.fill(track, BAR / 2.0, theme.text.alpha(0.10));
        let filled = (track.w * self.fraction.clamp(0.0, 1.0) as f32).round();
        if filled > 0.0 {
            scene.fill(Rect::new(track.x, track.y, filled.max(BAR), BAR), BAR / 2.0, self.color);
        }
    }
}

/// The top of a tile: a label on the left, a note on the right; the note gets the room the label
/// leaves, but at least half.
fn heading(scene: &mut Scene, theme: &Theme, fonts: &Fonts, r: Rect, label: &str, (note, note_color): (String, Rgba)) {
    let font = Font::new(theme.small_size, Weight::Medium);
    let bold = Font::new(theme.small_size, Weight::SemiBold);
    let room = r.w - 2.0 * PAD - 8.0;
    let note_w = fonts.width(&note, font).min((room - fonts.width(label, bold)).max(room / 2.0)).ceil();
    let label_w = room - note_w;
    let y = r.y + PAD;
    scene.text(Rect::new(r.x + PAD, y, label_w, HEAD), label, bold, theme.subtext, Align::Start);
    if !note.is_empty() {
        scene.text(Rect::new(r.x + r.w - PAD - note_w, y, note_w, HEAD), note, font, note_color, Align::End);
    }
}

fn big() -> Font {
    Font::new(17.0, Weight::SemiBold)
}

/// A share as a whole percentage, for the bar.
fn percent(v: f64) -> u32 {
    v.round().clamp(0.0, 100.0) as u32
}

/// A share (0 to 1) as `9%`.
fn pct(share: f64) -> String {
    format!("{}%", (share * 100.0).round() as u32)
}

/// Looks up the names of the apps the lists may show, once per executable: from the version
/// information of the executable, as `front_app` does.
fn name_apps(open: &mut Open) {
    let wanted: Vec<App> = stats::top(&open.apps, TOP, 0.05, |a| a.cpu).into_iter().chain(stats::top(&open.apps, TOP, 1, |a| a.memory)).collect();
    for app in wanted {
        if !open.names.contains_key(&app.exe) {
            // A process of another user (Windows' own, as dwm.exe) can't be opened: its file is
            // most likely in System32
            let path = front_app::process_path(app.pid).or_else(|| {
                let root = std::env::var_os("SystemRoot")?;
                let path = std::path::Path::new(&root).join("System32").join(&app.exe);
                (app.exe.ends_with(".exe") && path.is_file()).then(|| path.to_string_lossy().into_owned())
            });
            let name = path.map(|path| front_app::describe(&path)).unwrap_or_else(|| fallback_name(&app.exe));
            open.names.insert(app.exe.clone(), name);
        }
    }
}

/// An executable's name without `.exe`, for a process whose file can't be read (the kernel's own,
/// another user's).
fn fallback_name(exe: &str) -> String {
    let name = exe.strip_suffix(".exe").unwrap_or(exe);
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_shares() {
        assert_eq!(fallback_name("memory compression"), "Memory compression");
        assert_eq!(fallback_name("svchost.exe"), "Svchost");
        assert_eq!(pct(0.094), "9%");
        assert_eq!(percent(100.4), 100);
        assert_eq!(percent(-1.0), 0);
    }
}
