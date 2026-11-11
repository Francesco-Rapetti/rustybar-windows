//! Docker: the containers, from the Engine API on Docker Desktop's named pipe, with no `docker`
//! CLI, and a dashboard to start, stop, restart and remove them and to clean up what they leave
//! behind, as the macOS RustyBar's.
//!
//! - Pill: a box in Docker's blue with how many containers run (paused ones too, as `docker ps`),
//!   gray when none, red when one fails its health check or keeps restarting. Hidden while Docker
//!   isn't running.
//! - Popup: the engine, two tiles wide (`Docker Desktop`, `Engine 29.0.1 · 8 CPU · 7,7 GB`, and
//!   the CPU and memory of all the containers); the containers, those of each Compose project
//!   under its name with how many run and ▶ ■ ↻ for all of them, then the others under
//!   `Container`, two tiles in a row: an icon in the color of the state (green running, yellow
//!   paused or starting its health check, red failing it, restarting or exited with an error, gray
//!   stopped), the CPU and memory it uses, the name, the image, the published ports (links to
//!   `http://localhost:<port>`) and how long it's been up or its state; ▶ (resumes a paused one)
//!   ■ ↻ and the bin, which removes it at a second click. While a request is out the state says
//!   so, and `Non riuscito` in red if it fails (Docker's reason goes in the log). Then
//!   `Spazio su disco`: images, containers, volumes and build cache with their size and what a
//!   clean-up frees, cleaned at a second click (`docker image prune -a`, `container prune`,
//!   `volume prune`: anonymous volumes only, `builder prune -a`), `Reti inutilizzate` and
//!   `Pulisci tutto` (as `docker system prune -a`). `Apri Docker Desktop` last.
//! - Data: `/info` and `/containers/json?all=1` (and each one up, for when it started), read on a
//!   worker thread again a second after the last of a burst of `/events`, which a thread of its
//!   own follows with overlapped reads on the pipe (it costs nothing while nothing happens), and
//!   at once after an action of the popup. While Docker is off its pipe is tried every 30 s, not
//!   while no bar shows or a listed game is in front. CPU and memory come from
//!   `/containers/<id>/stats` only while the popup is open, every 5 s; the disk from
//!   `/system/df` when it opens and after a clean-up. `DOCKER_HOST=npipe:////./pipe/<name>` picks
//!   another pipe.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{CloseHandle, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAG_OVERLAPPED, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_NONE, OPEN_EXISTING, ReadFile, WriteFile};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects};
use windows::core::{HSTRING, PCWSTR};

use super::{Input, Reply, Signal};
use crate::config::Options;
use crate::i18n::t;
use crate::json::Json;
use crate::log;
use crate::theme::{Rgba, Theme};
use crate::ui::{Align, Font, Fonts, Item, Pill, Rect, Scene, Weight};
use crate::wake::{Timer, Wake};

// Segoe Fluent Icons
const BOX: &str = "\u{E7B8}";
const PLAY: &str = "\u{E768}";
const STOP: &str = "\u{E71A}";
const RESTART: &str = "\u{E72C}";
const BIN: &str = "\u{E74D}";
const DISK: &str = "\u{EDA2}";

/// Docker's blue.
const BRAND: Rgba = Rgba(0x1d as f32 / 255.0, 0x63 as f32 / 255.0, 0xed as f32 / 255.0, 1.0);

// The popup's layout, in DIPs.
const MARGIN: f32 = 8.0;
const TILE: f32 = 150.0;
const GAP: f32 = 8.0;
const WIDE: f32 = TILE * 2.0 + GAP;
const WIDTH: f32 = WIDE + 2.0 * MARGIN;
const PAD: f32 = 10.0;
const TILE_H: f32 = 112.0;
const HEAD_H: f32 = 26.0;
const ROW: f32 = 30.0;
/// The most container tiles the popup has: it doesn't scroll.
const MAX_TILES: usize = 16;

const RETRY: Duration = Duration::from_secs(30);
const BURST: Duration = Duration::from_secs(1);
const STATS: Duration = Duration::from_secs(5);

// ---- the Engine API on the pipe ----------------------------------------------------------------

/// The pipe: `DOCKER_HOST`'s if it's one, else Docker Desktop's.
fn pipe() -> String {
    std::env::var("DOCKER_HOST")
        .ok()
        .and_then(|h| h.strip_prefix("npipe://").map(|p| p.trim_start_matches('/').replace('/', "\\")))
        .map(|p| format!(r"\\{p}"))
        .unwrap_or_else(|| r"\\.\pipe\docker_engine".into())
}

/// The pipe, opened; None while Docker isn't running.
fn open(overlapped: bool) -> Option<HANDLE> {
    let flags = if overlapped { FILE_FLAG_OVERLAPPED } else { FILE_FLAGS_AND_ATTRIBUTES(0) };
    unsafe { CreateFileW(&HSTRING::from(pipe()), (GENERIC_READ | GENERIC_WRITE).0, FILE_SHARE_NONE, None, OPEN_EXISTING, flags, None) }.ok()
}

/// The status and the body of an HTTP answer, with chunked bodies put back together.
fn parse_response(raw: &[u8]) -> Option<(u16, Vec<u8>)> {
    let end = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&raw[..end]);
    let status = head.split_whitespace().nth(1)?.parse().ok()?;
    let mut body = raw[end + 4..].to_vec();
    let header = |name: &str| head.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_string()));
    if header("Transfer-Encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        let (mut out, mut rest) = (Vec::new(), &body[..]);
        while let Some(eol) = rest.windows(2).position(|w| w == b"\r\n") {
            let size = usize::from_str_radix(String::from_utf8_lossy(&rest[..eol]).split(';').next()?.trim(), 16).ok()?;
            if size == 0 {
                break;
            }
            let chunk = rest.get(eol + 2..eol + 2 + size)?;
            out.extend_from_slice(chunk);
            rest = rest.get(eol + 4 + size..).unwrap_or(&[]);
        }
        body = out;
    } else if let Some(len) = header("Content-Length").and_then(|v| v.parse::<usize>().ok()) {
        body.truncate(len);
    }
    Some((status, body))
}

/// A request to the Engine, waiting for the whole answer: for a worker thread.
fn request(method: &str, path: &str) -> Result<(u16, Json), String> {
    let h = open(false).ok_or("Docker isn't running")?;
    let text = format!("{method} {path} HTTP/1.1\r\nHost: docker\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");
    let mut raw = Vec::new();
    unsafe {
        let mut written = 0u32;
        let sent = WriteFile(h, Some(text.as_bytes()), Some(&mut written), None);
        if sent.is_ok() {
            let mut buf = vec![0u8; 64 << 10];
            loop {
                let mut read = 0u32;
                // The end of the answer is the Engine closing the pipe
                if ReadFile(h, Some(&mut buf), Some(&mut read), None).is_err() || read == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..read as usize]);
            }
        }
        let _ = CloseHandle(h);
        sent.map_err(|e| e.to_string())?;
    }
    let (status, body) = parse_response(&raw).ok_or("no answer")?;
    Ok((status, Json::parse(&String::from_utf8_lossy(&body)).unwrap_or(Json::Null)))
}

/// `?filters=` as the Engine takes them, encoded.
fn query(json: &str) -> String {
    json.bytes().map(|b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// `2026-11-11T12:00:00.123456789Z` as seconds since 1970.
fn rfc3339(s: &str) -> Option<f64> {
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let day = crate::ical::days(d.next()??, d.next()?? as u32, d.next()?? as u32);
    let time = time.trim_end_matches('Z');
    let time = time.split(['+', '-']).next()?;
    let mut parts = time.split(':');
    let (h, m) = (parts.next()?.parse::<f64>().ok()?, parts.next()?.parse::<f64>().ok()?);
    let sec = parts.next().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
    (day > 0).then_some(day as f64 * 86_400.0 + h * 3600.0 + m * 60.0 + sec)
}

/// The thread that follows `/events`: it says when something happened, and when Docker went.
struct Stream {
    stop: HANDLE,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Stream {
    fn start(inbox: Arc<Inbox>) -> Option<Stream> {
        let h = open(true)?;
        let stop = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.ok()?;
        let (h, stop_raw) = (h.0 as usize, stop.0 as usize);
        let thread = std::thread::spawn(move || {
            let (h, stop) = (HANDLE(h as *mut _), HANDLE(stop_raw as *mut _));
            let filters = query(r#"{"type":["container"]}"#);
            let text = format!("GET /events?filters={filters} HTTP/1.1\r\nHost: docker\r\n\r\n");
            unsafe {
                let Ok(io) = CreateEventW(None, true, false, PCWSTR::null()) else { return };
                let mut ov = OVERLAPPED { hEvent: io, ..Default::default() };
                let mut n = 0u32;
                let mut ok = match WriteFile(h, Some(text.as_bytes()), None, Some(&mut ov)) {
                    Ok(()) => true,
                    Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => GetOverlappedResult(h, &ov, &mut n, true).is_ok(),
                    Err(_) => false,
                };
                let mut buf = vec![0u8; 16 << 10];
                let mut first = true;
                while ok {
                    ov = OVERLAPPED { hEvent: io, ..Default::default() };
                    let started = ReadFile(h, Some(&mut buf), None, Some(&mut ov));
                    if let Err(e) = started
                        && e.code() != ERROR_IO_PENDING.to_hresult()
                    {
                        break;
                    }
                    let waited = WaitForMultipleObjects(&[io, stop], false, INFINITE);
                    if waited != WAIT_OBJECT_0 {
                        // Asked to stop: the read goes first
                        let _ = CancelIoEx(h, Some(&ov));
                        let _ = GetOverlappedResult(h, &ov, &mut n, true);
                        let _ = CloseHandle(io);
                        let _ = CloseHandle(h);
                        return;
                    }
                    ok = GetOverlappedResult(h, &ov, &mut n, false).is_ok() && n > 0;
                    // The answer's head first: an event is anything after it
                    if ok && !first {
                        inbox.events.store(true, Ordering::Release);
                        inbox.wake.set();
                    }
                    first = false;
                }
                let _ = CloseHandle(io);
                let _ = CloseHandle(h);
            }
            // The pipe broke: Docker went, or restarts
            inbox.ended.store(true, Ordering::Release);
            inbox.wake.set();
        });
        Some(Stream { stop, thread: Some(thread) })
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        unsafe {
            let _ = SetEvent(self.stop);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        unsafe {
            let _ = CloseHandle(self.stop);
        }
    }
}

// ---- the data ----------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
struct Engine {
    os: String,
    version: String,
    cpus: u64,
    memory: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Container {
    id: String,
    name: String,
    project: String,
    image: String,
    /// created, running, paused, restarting, removing, exited or dead.
    state: String,
    /// healthy, unhealthy, starting, or empty without a health check.
    health: String,
    exit: Option<i64>,
    ports: Vec<u16>,
    /// Seconds since 1970.
    started: Option<f64>,
}

impl Container {
    fn up(&self) -> bool {
        matches!(self.state.as_str(), "running" | "paused" | "restarting")
    }

    fn can(&self, op: Op) -> bool {
        match op {
            Op::Start => matches!(self.state.as_str(), "created" | "exited" | "paused"),
            Op::Stop | Op::Restart => self.up(),
            Op::Remove => self.state != "removing",
        }
    }

    fn failing(&self) -> bool {
        self.health == "unhealthy" || self.state == "restarting" || self.state == "dead"
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Snapshot {
    engine: Engine,
    containers: Vec<Container>,
}

/// Bytes used and bytes a clean-up frees, of each kind.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Disk {
    images: (f64, f64),
    containers: (f64, f64),
    volumes: (f64, f64),
    build: (f64, f64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Start,
    Stop,
    Restart,
    Remove,
}

impl Op {
    fn key(self) -> &'static str {
        match self {
            Op::Start => "start",
            Op::Stop => "stop",
            Op::Restart => "restart",
            Op::Remove => "remove",
        }
    }

    fn of(key: &str) -> Option<Op> {
        [Op::Start, Op::Stop, Op::Restart, Op::Remove].into_iter().find(|o| o.key() == key)
    }

    fn busy(self) -> &'static str {
        match self {
            Op::Start => t("Si avvia…", "Starting…"),
            Op::Stop => t("Si ferma…", "Stopping…"),
            Op::Restart => t("Si riavvia…", "Restarting…"),
            Op::Remove => t("Si elimina…", "Removing…"),
        }
    }

    /// The request for a container in `state`: ▶ on a paused one resumes it.
    fn path(self, id: &str, state: &str) -> (&'static str, String) {
        match (self, state) {
            (Op::Start, "paused") => ("POST", format!("/containers/{id}/unpause")),
            (Op::Remove, _) => ("DELETE", format!("/containers/{id}?force=true")),
            _ => ("POST", format!("/containers/{id}/{}", self.key())),
        }
    }
}

/// The requests of a clean-up, in order: containers first, so their images and networks are
/// unused by then, as `docker system prune`.
fn prune_paths(kind: &str) -> Vec<String> {
    match kind {
        "containers" => vec!["/containers/prune".into()],
        // As `-a`: every image without a container, not only the untagged ones
        "images" => vec![format!("/images/prune?filters={}", query(r#"{"dangling":["false"]}"#))],
        // Anonymous volumes only, since Engine 23: named ones keep data
        "volumes" => vec!["/volumes/prune".into()],
        "build" => vec!["/build/prune?all=true".into()],
        "networks" => vec!["/networks/prune".into()],
        "all" => ["containers", "networks", "images", "build"].into_iter().flat_map(prune_paths).collect(),
        _ => vec![],
    }
}

/// What a tile, a button or a clean-up says for a moment, over its state.
#[derive(Clone, Debug, PartialEq)]
enum Note {
    Busy(&'static str),
    Confirm,
    Failed,
    Done(String),
}

/// CPU % and MB, by container name.
type Usage = Vec<(String, (f64, f64))>;

/// What the threads leave for the main thread.
struct Inbox {
    wake: Arc<Wake>,
    /// The stream saw an event; the stream ended.
    events: AtomicBool,
    ended: AtomicBool,
    snapshot: Mutex<Option<Option<Snapshot>>>,
    usage: Mutex<Option<Usage>>,
    disk: Mutex<Option<Option<Disk>>>,
    /// An action done: its key, and whether it went through (with what a clean-up freed).
    done: Mutex<Vec<(String, Result<f64, String>)>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The engine and the containers, from the Engine; None while it isn't running.
fn read_snapshot() -> Option<Snapshot> {
    let (_, info) = request("GET", "/info").ok()?;
    let engine = Engine {
        os: info["OperatingSystem"].str().unwrap_or("Docker").to_string(),
        version: info["ServerVersion"].str().unwrap_or_default().to_string(),
        cpus: info["NCPU"].f64().unwrap_or(0.0) as u64,
        memory: info["MemTotal"].f64().unwrap_or(0.0),
    };
    let (_, list) = request("GET", "/containers/json?all=1").ok()?;
    let mut containers: Vec<Container> = list.array().iter().map(container).collect();
    // When each one up started: only `/containers/<id>/json` says
    for c in containers.iter_mut().filter(|c| c.up()).take(MAX_TILES) {
        if let Ok((_, inspect)) = request("GET", &format!("/containers/{}/json", c.id)) {
            c.started = inspect["State"]["StartedAt"].str().and_then(rfc3339);
        }
    }
    sort(&mut containers);
    Some(Snapshot { engine, containers })
}

/// A container of `/containers/json`.
fn container(j: &Json) -> Container {
    let status = j["Status"].str().unwrap_or_default();
    let health = if status.contains("(unhealthy)") {
        "unhealthy"
    } else if status.contains("(health: starting)") {
        "starting"
    } else if status.contains("(healthy)") {
        "healthy"
    } else {
        ""
    };
    // `Exited (1) 2 hours ago`
    let exit = status.strip_prefix("Exited (").and_then(|s| s.split(')').next()).and_then(|s| s.parse().ok());
    let mut ports: Vec<u16> = j["Ports"].array().iter().filter_map(|p| p["PublicPort"].f64()).map(|p| p as u16).collect();
    ports.sort_unstable();
    ports.dedup();
    Container {
        id: j["Id"].str().unwrap_or_default().to_string(),
        name: j["Names"].array().first().and_then(Json::str).unwrap_or_default().trim_start_matches('/').to_string(),
        project: j["Labels"]["com.docker.compose.project"].str().unwrap_or_default().to_string(),
        image: j["Image"].str().unwrap_or_default().to_string(),
        state: j["State"].str().unwrap_or_default().to_string(),
        health: health.into(),
        exit,
        ports,
        started: None,
    }
}

/// By project (the projects first, by name, then the others), then by name.
fn sort(containers: &mut [Container]) {
    containers.sort_by(|a, b| (a.project.is_empty(), &a.project, &a.name).cmp(&(b.project.is_empty(), &b.project, &b.name)));
}

/// CPU % and MB of a container, from one reading of its stats.
fn usage(id: &str) -> Option<(f64, f64)> {
    let (_, s) = request("GET", &format!("/containers/{id}/stats?stream=false")).ok()?;
    let cpu = s["cpu_stats"]["cpu_usage"]["total_usage"].f64()? - s["precpu_stats"]["cpu_usage"]["total_usage"].f64().unwrap_or(0.0);
    let system = s["cpu_stats"]["system_cpu_usage"].f64().unwrap_or(0.0) - s["precpu_stats"]["system_cpu_usage"].f64().unwrap_or(0.0);
    let cpus = s["cpu_stats"]["online_cpus"].f64().unwrap_or(1.0);
    let percent = if system > 0.0 { cpu / system * cpus * 100.0 } else { 0.0 };
    let mem = s["memory_stats"]["usage"].f64().unwrap_or(0.0) - s["memory_stats"]["stats"]["inactive_file"].f64().unwrap_or(0.0);
    Some((percent.max(0.0), mem.max(0.0) / 1_048_576.0))
}

/// The disk of `/system/df`: what each kind uses and what a clean-up frees.
fn read_disk() -> Option<Disk> {
    let (_, df) = request("GET", "/system/df").ok()?;
    let sum = |list: &Json, size: &dyn Fn(&Json) -> f64, free: &dyn Fn(&Json) -> bool| {
        list.array().iter().fold((0.0, 0.0), |(u, f), x| (u + size(x), f + if free(x) { size(x) } else { 0.0 }))
    };
    Some(Disk {
        images: sum(&df["Images"], &|x| x["Size"].f64().unwrap_or(0.0), &|x| x["Containers"].f64().unwrap_or(0.0) == 0.0),
        containers: sum(&df["Containers"], &|x| x["SizeRw"].f64().unwrap_or(0.0), &|x| x["State"].str() != Some("running")),
        volumes: sum(&df["Volumes"], &|x| x["UsageData"]["Size"].f64().unwrap_or(0.0).max(0.0), &|x| x["UsageData"]["RefCount"].f64() == Some(0.0) && !x["Labels"]["com.docker.volume.anonymous"].is_null()),
        build: sum(&df["BuildCache"], &|x| x["Size"].f64().unwrap_or(0.0), &|x| x["InUse"].bool() != Some(true)),
    })
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// `da 2 h`, `since 3 min`.
fn up_for(started: f64) -> String {
    let m = ((now() - started) / 60.0).max(0.0) as u64;
    let span = match m {
        0 => t("adesso", "now").to_string(),
        m if m < 60 => format!("{m} min"),
        m if m < 48 * 60 => format!("{} h", m / 60),
        m => format!("{} {}", m / 1440, t("g", "d")),
    };
    if m == 0 { span } else { format!("{} {span}", t("da", "for")) }
}

pub struct Docker {
    snapshot: Option<Snapshot>,
    usage: HashMap<String, (f64, f64)>,
    disk: Option<Disk>,
    inbox: Arc<Inbox>,
    stream: Option<Stream>,
    reading: bool,
    measuring: bool,
    sizing: bool,
    /// By container id, project (`project:<name>`) or clean-up (`prune:<kind>`).
    notes: HashMap<String, (Note, Option<Instant>)>,
    retry_at: Option<Instant>,
    read_at: Option<Instant>,
    stats_at: Option<Instant>,
    timer: Timer,
    open: bool,
    shown: bool,
    game: bool,
    mock: bool,
}

impl Docker {
    pub fn new(_options: &Options) -> Docker {
        let inbox = Arc::new(Inbox {
            wake: Wake::new(),
            events: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            snapshot: Mutex::new(None),
            usage: Mutex::new(None),
            disk: Mutex::new(None),
            done: Mutex::new(Vec::new()),
        });
        let mut w = Docker {
            snapshot: None,
            usage: HashMap::new(),
            disk: None,
            inbox,
            stream: None,
            reading: false,
            measuring: false,
            sizing: false,
            notes: HashMap::new(),
            retry_at: None,
            read_at: None,
            stats_at: None,
            timer: Timer::new(),
            open: false,
            shown: true,
            game: false,
            mock: false,
        };
        if let Some(m) = crate::mock::load("docker") {
            w.mock = true;
            let e = &m["engine"];
            let engine = Engine {
                os: e["os"].str().unwrap_or("Docker Desktop").into(),
                version: e["version"].str().unwrap_or_default().into(),
                cpus: e["cpus"].f64().unwrap_or(0.0) as u64,
                memory: e["memory"].f64().unwrap_or(0.0),
            };
            let mut containers: Vec<Container> = m["containers"]
                .array()
                .iter()
                .map(|c| Container {
                    id: c["name"].str().unwrap_or_default().into(),
                    name: c["name"].str().unwrap_or_default().into(),
                    project: c["project"].str().unwrap_or_default().into(),
                    image: c["image"].str().unwrap_or_default().into(),
                    state: c["state"].str().unwrap_or_default().into(),
                    health: c["health"].str().unwrap_or_default().into(),
                    exit: c["exit"].f64().map(|e| e as i64),
                    ports: c["ports"].array().iter().filter_map(Json::f64).map(|p| p as u16).collect(),
                    // Seconds ago, in the made-up data
                    started: c["started"].f64().map(|ago| now() - ago),
                })
                .collect();
            sort(&mut containers);
            w.snapshot = Some(Snapshot { engine, containers });
            for (name, u) in m["usage"].object().into_iter().flatten() {
                let a = u.array();
                w.usage.insert(name.clone(), (a.first().and_then(Json::f64).unwrap_or(0.0), a.get(1).and_then(Json::f64).unwrap_or(0.0)));
            }
            let pair = |k: &str| {
                let a = m["disk"][k].array();
                (a.first().and_then(Json::f64).unwrap_or(0.0), a.get(1).and_then(Json::f64).unwrap_or(0.0))
            };
            w.disk = Some(Disk { images: pair("images"), containers: pair("containers"), volumes: pair("volumes"), build: pair("build") });
            return w;
        }
        w.connect();
        w
    }

    /// Follows Docker if it runs (the stream, a first reading), else tries again in 30 s.
    fn connect(&mut self) {
        self.retry_at = None;
        if !self.shown || self.game {
            self.stream = None;
            self.arm();
            return;
        }
        if self.stream.is_none() {
            self.inbox.ended.store(false, Ordering::Release);
            self.stream = Stream::start(self.inbox.clone());
        }
        if self.stream.is_some() {
            self.read();
        } else {
            self.snapshot = None;
            self.retry_at = Some(Instant::now() + RETRY);
        }
        self.arm();
    }

    /// Reads the engine and the containers on a worker thread, unless a reading is out.
    fn read(&mut self) {
        if self.reading || self.mock {
            return;
        }
        self.reading = true;
        let inbox = self.inbox.clone();
        std::thread::spawn(move || {
            let s = read_snapshot();
            *lock(&inbox.snapshot) = Some(s);
            inbox.wake.set();
        });
    }

    /// CPU and memory of the containers that run, a thread each: each reading takes a second or
    /// two (the Engine samples), and until then the last ones show.
    fn measure(&mut self) {
        if self.measuring || self.mock {
            return;
        }
        let ids: Vec<(String, String)> = self.snapshot.iter().flat_map(|s| &s.containers).filter(|c| c.state == "running").map(|c| (c.id.clone(), c.name.clone())).collect();
        if ids.is_empty() {
            return;
        }
        self.measuring = true;
        let inbox = self.inbox.clone();
        std::thread::spawn(move || {
            let threads: Vec<_> = ids.into_iter().map(|(id, name)| std::thread::spawn(move || usage(&id).map(|u| (name, u)))).collect();
            let all: Vec<(String, (f64, f64))> = threads.into_iter().filter_map(|t| t.join().ok().flatten()).collect();
            *lock(&inbox.usage) = Some(all);
            inbox.wake.set();
        });
    }

    fn size(&mut self) {
        if self.sizing || self.mock {
            return;
        }
        self.sizing = true;
        let inbox = self.inbox.clone();
        std::thread::spawn(move || {
            *lock(&inbox.disk) = Some(read_disk());
            inbox.wake.set();
        });
    }

    /// Sends an action on a worker thread: its key says what it was for when it's done.
    fn act(&mut self, key: String, requests: Vec<(&'static str, String)>) {
        if self.mock {
            return;
        }
        let inbox = self.inbox.clone();
        std::thread::spawn(move || {
            let mut freed = 0.0;
            let mut result = Ok(0.0);
            for (method, path) in requests {
                match request(method, &path) {
                    Ok((status, body)) if (200..300).contains(&status) => freed += body["SpaceReclaimed"].f64().unwrap_or(0.0),
                    Ok((status, body)) => {
                        let why = body["message"].str().unwrap_or_default().to_string();
                        log!("docker: {method} {path} → {status} {why}");
                        result = Err(why);
                        break;
                    }
                    Err(e) => {
                        log!("docker: {method} {path}: {e}");
                        result = Err(e);
                        break;
                    }
                }
            }
            lock(&inbox.done).push((key, result.map(|_| freed)));
            inbox.wake.set();
        });
    }

    /// Takes what the threads left. True if something shown changed.
    fn take(&mut self) -> bool {
        let mut changed = false;
        if self.inbox.ended.swap(false, Ordering::AcqRel) {
            self.stream = None;
            changed |= self.snapshot.take().is_some();
            self.retry_at = Some(Instant::now() + RETRY);
        }
        // A burst of events reads once, a second after the last
        if self.inbox.events.swap(false, Ordering::AcqRel) {
            self.read_at = Some(Instant::now() + BURST);
        }
        if let Some(s) = lock(&self.inbox.snapshot).take() {
            self.reading = false;
            if s.is_none() {
                self.stream = None;
                self.retry_at = Some(Instant::now() + RETRY);
            }
            if s != self.snapshot {
                // A container that changed ends its notes, but a confirmation
                if let (Some(old), Some(new)) = (&self.snapshot, &s) {
                    for c in &new.containers {
                        if old.containers.iter().find(|o| o.id == c.id).is_none_or(|o| o.state != c.state) {
                            self.notes.remove(&c.id);
                        }
                    }
                }
                self.snapshot = s;
                changed = true;
            }
        }
        if let Some(usage) = lock(&self.inbox.usage).take() {
            self.measuring = false;
            self.usage = usage.into_iter().collect();
            self.stats_at = self.open.then(|| Instant::now() + STATS);
            changed = true;
        }
        // A reading of the disk that failed keeps the last one
        if let Some(d) = lock(&self.inbox.disk).take() {
            self.sizing = false;
            if d.is_some() {
                self.disk = d;
                changed = true;
            }
        }
        let done = std::mem::take(&mut *lock(&self.inbox.done));
        for (key, result) in done {
            let note = match (&result, key.starts_with("prune:")) {
                (Ok(freed), true) => {
                    let text = if crate::i18n::italian() { format!("Liberati {}", crate::i18n::size(*freed as u64)) } else { format!("Freed {}", crate::i18n::size(*freed as u64)) };
                    Some((Note::Done(text), Some(Instant::now() + Duration::from_secs(6))))
                }
                (Ok(_), false) => None,
                (Err(_), _) => Some((Note::Failed, Some(Instant::now() + Duration::from_secs(6)))),
            };
            match note {
                Some(n) => {
                    self.notes.insert(key.clone(), n);
                }
                None => {
                    self.notes.remove(&key);
                }
            }
            if key.starts_with("prune:") {
                self.size();
            }
            self.read();
            changed = true;
        }
        self.arm();
        changed
    }

    /// The timer, for the first of what's due: a new try, a reading after a burst, the stats, a
    /// note that ends.
    fn arm(&mut self) {
        let notes = self.notes.values().filter_map(|(_, until)| *until);
        let first = self.retry_at.into_iter().chain(self.read_at).chain(self.stats_at).chain(notes).min();
        self.timer.stop();
        if let Some(first) = first {
            self.timer.start(first.saturating_duration_since(Instant::now()).as_millis().max(1) as i64, i32::MAX);
        }
    }

    fn due(&mut self) -> bool {
        let now = Instant::now();
        let mut changed = false;
        if self.retry_at.is_some_and(|at| at <= now) {
            self.connect();
        }
        if self.read_at.take_if(|at| *at <= now).is_some() {
            self.read();
        }
        if self.stats_at.take_if(|at| *at <= now).is_some() && self.open {
            self.measure();
        }
        let before = self.notes.len();
        self.notes.retain(|_, (_, until)| until.is_none_or(|u| u > now));
        changed |= self.notes.len() != before;
        self.arm();
        changed
    }

    fn note(&self, key: &str) -> Option<&Note> {
        self.notes.get(key).map(|(n, _)| n)
    }

    /// A button of a container, or of a project (all of its containers at once).
    fn button(&mut self, target: &str, op: Op) {
        let Some(s) = &self.snapshot else { return };
        let (key, chosen): (String, Vec<Container>) = match target.strip_prefix("project:") {
            Some(p) => (target.to_string(), s.containers.iter().filter(|c| c.project == p && c.can(op)).cloned().collect()),
            None => (target.to_string(), s.containers.iter().filter(|c| c.id == target && c.can(op)).cloned().collect()),
        };
        if chosen.is_empty() {
            return;
        }
        // Removing takes a second click
        if op == Op::Remove && self.note(&key) != Some(&Note::Confirm) {
            self.notes.insert(key, (Note::Confirm, Some(Instant::now() + Duration::from_secs(4))));
            self.arm();
            return;
        }
        self.notes.insert(key.clone(), (Note::Busy(op.busy()), None));
        let requests = chosen.iter().map(|c| op.path(&c.id, &c.state)).collect();
        self.act(key, requests);
    }

    fn prune(&mut self, kind: &str) {
        let key = format!("prune:{kind}");
        match self.note(&key) {
            Some(Note::Busy(_)) => {}
            Some(Note::Confirm) => {
                self.notes.insert(key.clone(), (Note::Busy(t("Pulizia…", "Cleaning up…")), None));
                self.act(key, prune_paths(kind).into_iter().map(|p| ("POST", p)).collect());
            }
            _ => {
                self.notes.insert(key, (Note::Confirm, Some(Instant::now() + Duration::from_secs(4))));
                self.arm();
            }
        }
    }

    // ---- drawing -----------------------------------------------------------------------------

    fn state_look(&self, theme: &Theme, c: &Container) -> (Rgba, String) {
        let col = |n: &str| theme.color(n).unwrap_or(theme.text);
        match (c.state.as_str(), c.health.as_str()) {
            ("running", "unhealthy") => (col("red"), t("Non in salute", "Unhealthy").into()),
            ("running", "starting") => (col("yellow"), t("Si avvia…", "Starting…").into()),
            ("running", _) => (col("green"), c.started.map(up_for).unwrap_or_else(|| t("In esecuzione", "Running").into())),
            ("paused", _) => (col("yellow"), t("In pausa", "Paused").into()),
            ("restarting", _) => (col("red"), t("Si riavvia…", "Restarting…").into()),
            ("exited", _) if c.exit.is_some_and(|e| e != 0) => {
                let code = c.exit.unwrap_or(0);
                (col("red"), if crate::i18n::italian() { format!("Uscito con codice {code}") } else { format!("Exited with code {code}") })
            }
            ("created", _) => (theme.subtext, t("Creato", "Created").into()),
            ("dead", _) => (col("red"), t("Morto", "Dead").into()),
            _ => (theme.subtext, t("Fermo", "Stopped").into()),
        }
    }

    /// A container's tile at `x`, `y`.
    fn tile(&self, scene: &mut Scene, theme: &Theme, fonts: &Fonts, c: &Container, x: f32, y: f32) {
        let (color, mut status) = self.state_look(theme, c);
        let mut status_color = theme.subtext;
        match self.note(&c.id) {
            Some(Note::Busy(b)) => status = b.to_string(),
            Some(Note::Confirm) => (status, status_color) = (t("Fai clic per eliminare", "Click to remove").into(), theme.color("red").unwrap_or(theme.text)),
            Some(Note::Failed) => (status, status_color) = (t("Non riuscito", "Failed").into(), theme.color("red").unwrap_or(theme.text)),
            _ => {}
        }
        // Tinted while up, and red when it failed
        scene.fill(Rect::new(x, y, TILE, TILE_H), theme.row_radius.max(8.0), theme.tile(color, color != theme.subtext));
        let small = Font::new(theme.small_size, Weight::Medium);
        let inner = TILE - 2.0 * PAD;
        scene.text(Rect::new(x + PAD, y + 8.0, 20.0, 22.0), BOX, Font::icons(16.0), color, Align::Start);
        if let Some((cpu, mb)) = self.usage.get(&c.name).filter(|_| c.state == "running") {
            scene.text(Rect::new(x + PAD + 22.0, y + 8.0, inner - 22.0, 22.0), format!("{} % · {} MB", crate::i18n::number(*cpu, 1), mb.round()), small, theme.subtext, Align::End);
        }
        let name = c.name.strip_prefix(&format!("{}-", c.project)).filter(|_| !c.project.is_empty()).unwrap_or(&c.name);
        scene.text(Rect::new(x + PAD, y + 30.0, inner, 18.0), name, Font::new(theme.font_size, Weight::SemiBold), theme.text, Align::Start);
        scene.text(Rect::new(x + PAD, y + 48.0, inner, 16.0), c.image.as_str(), small, theme.subtext, Align::Start);
        // The ports, links while it runs; else the state
        let mut px = x + PAD;
        let mut line_used = false;
        if c.state == "running" {
            for p in c.ports.iter().take(3) {
                let label = format!(":{p}");
                let w = fonts.width(&label, small).ceil() + 4.0;
                if px + w > x + TILE - PAD {
                    break;
                }
                let r = Rect::new(px, y + 64.0, w, 16.0);
                scene.text(r, label, small, theme.primary, Align::Start);
                scene.zone(r, 3.0, format!("port:{p}"));
                px += w + 6.0;
                line_used = true;
            }
        }
        let status_x = if line_used { px } else { x + PAD };
        scene.text(Rect::new(status_x, y + 64.0, x + TILE - PAD - status_x, 16.0), status, small, status_color, Align::Start);
        // ▶ ■ ↻ and the bin, dimmed where they'd do nothing
        let busy = matches!(self.note(&c.id), Some(Note::Busy(_)));
        for (k, (op, glyph)) in [(Op::Start, PLAY), (Op::Stop, STOP), (Op::Restart, RESTART), (Op::Remove, BIN)].into_iter().enumerate() {
            let r = Rect::new(x + 4.0 + k as f32 * 34.0, y + TILE_H - 32.0, 32.0, 28.0);
            let can = c.can(op) && !busy;
            let red = op == Op::Remove && self.note(&c.id) == Some(&Note::Confirm);
            let color = if red { theme.color("red").unwrap_or(theme.text) } else if can { theme.text } else { theme.overlay };
            scene.text(r, glyph, Font::icons(13.0), color, Align::Center);
            if can {
                scene.zone(r, 6.0, format!("{}:{}", op.key(), c.id));
            }
        }
    }

    /// The popup's content, drawn into `scene` if there's one; where it ends.
    fn layout(&self, mut scene: Option<&mut Scene>, theme: &Theme, fonts: &Fonts) -> f32 {
        let Some(s) = &self.snapshot else { return 0.0 };
        let small = Font::new(theme.small_size, Weight::Medium);
        let bold = Font::new(theme.font_size, Weight::SemiBold);
        let mut y = MARGIN;
        // The engine
        if let Some(sc) = scene.as_deref_mut() {
            sc.fill(Rect::new(MARGIN, y, WIDE, 56.0), theme.row_radius.max(8.0), theme.tile(BRAND, true));
            sc.text(Rect::new(MARGIN + PAD, y, 24.0, 56.0), BOX, Font::icons(20.0), BRAND, Align::Start);
            sc.text(Rect::new(MARGIN + PAD + 32.0, y + 9.0, 170.0, 18.0), s.engine.os.as_str(), bold, theme.text, Align::Start);
            let detail = format!("Engine {} · {} CPU · {}", s.engine.version, s.engine.cpus, crate::i18n::size(s.engine.memory as u64));
            sc.text(Rect::new(MARGIN + PAD + 32.0, y + 28.0, WIDE - 2.0 * PAD - 32.0, 16.0), detail, small, theme.subtext, Align::Start);
            let (cpu, mb) = self.usage.values().fold((0.0, 0.0), |(c, m), (uc, um)| (c + uc, m + um));
            if !self.usage.is_empty() {
                sc.text(Rect::new(MARGIN + WIDE - PAD - 100.0, y + 9.0, 100.0, 18.0), format!("{} %", crate::i18n::number(cpu, 1)), bold, theme.text, Align::End);
                sc.text(Rect::new(MARGIN + WIDE - PAD - 100.0, y + 28.0, 100.0, 16.0), format!("{} MB", mb.round()), small, theme.subtext, Align::End);
            }
        }
        y += 56.0 + GAP;
        // The containers, by project
        let mut groups: Vec<(&str, Vec<&Container>)> = Vec::new();
        for c in s.containers.iter().take(MAX_TILES) {
            match groups.last_mut() {
                Some((p, list)) if *p == c.project => list.push(c),
                _ => groups.push((c.project.as_str(), vec![c])),
            }
        }
        for (project, list) in groups {
            if let Some(sc) = scene.as_deref_mut() {
                let title = if project.is_empty() { "Container".to_string() } else { project.to_string() };
                let up = list.iter().filter(|c| c.up()).count();
                sc.text(Rect::new(MARGIN + 4.0, y, 200.0, HEAD_H), format!("{title}  "), bold, theme.text, Align::Start);
                let tw = fonts.width(&title, bold).ceil();
                sc.text(Rect::new(MARGIN + 10.0 + tw, y, 60.0, HEAD_H), format!("{up}/{}", list.len()), small, theme.subtext, Align::Start);
                if !project.is_empty() {
                    let key = format!("project:{project}");
                    let busy = matches!(self.note(&key), Some(Note::Busy(_)));
                    for (k, (op, glyph)) in [(Op::Start, PLAY), (Op::Stop, STOP), (Op::Restart, RESTART)].into_iter().enumerate() {
                        let r = Rect::new(MARGIN + WIDE - (3 - k) as f32 * 32.0, y, 30.0, HEAD_H);
                        let can = !busy && list.iter().any(|c| c.can(op));
                        sc.text(r, glyph, Font::icons(12.0), if can { theme.text } else { theme.overlay }, Align::Center);
                        if can {
                            sc.zone(r, 6.0, format!("{}:{key}", op.key()));
                        }
                    }
                    if let Some(Note::Failed) = self.note(&key) {
                        sc.text(Rect::new(MARGIN + 60.0 + tw, y, 120.0, HEAD_H), t("Non riuscito", "Failed"), small, theme.color("red").unwrap_or(theme.text), Align::Start);
                    }
                }
            }
            y += HEAD_H + 2.0;
            for pair in list.chunks(2) {
                if let Some(sc) = scene.as_deref_mut() {
                    for (k, c) in pair.iter().enumerate() {
                        self.tile(sc, theme, fonts, c, MARGIN + k as f32 * (TILE + GAP), y);
                    }
                }
                y += TILE_H + GAP;
            }
        }
        let more = s.containers.len().saturating_sub(MAX_TILES);
        if more > 0 {
            if let Some(sc) = scene.as_deref_mut() {
                let text = if crate::i18n::italian() { format!("e altri {more}") } else { format!("and {more} more") };
                sc.text(Rect::new(MARGIN + 4.0, y, WIDE, 20.0), text, small, theme.subtext, Align::Start);
            }
            y += 22.0;
        }
        // The disk
        if let Some(sc) = scene.as_deref_mut() {
            sc.text(Rect::new(MARGIN + 4.0, y, 22.0, HEAD_H), DISK, Font::icons(13.0), theme.text, Align::Start);
            sc.text(Rect::new(MARGIN + 26.0, y, 200.0, HEAD_H), t("Spazio su disco", "Disk space"), bold, theme.text, Align::Start);
        }
        y += HEAD_H + 2.0;
        let kinds = [
            ("images", t("Immagini", "Images"), self.disk.map(|d| d.images)),
            ("containers", t("Container fermi", "Stopped containers"), self.disk.map(|d| d.containers)),
            ("volumes", t("Volumi anonimi", "Anonymous volumes"), self.disk.map(|d| d.volumes)),
            ("build", t("Cache di build", "Build cache"), self.disk.map(|d| d.build)),
            ("networks", t("Reti inutilizzate", "Unused networks"), None),
            ("all", t("Pulisci tutto", "Clean up everything"), None),
        ];
        for (kind, label, sizes) in kinds {
            if let Some(sc) = scene.as_deref_mut() {
                let key = format!("prune:{kind}");
                let r = Rect::new(MARGIN, y, WIDE, ROW);
                let red = theme.color("red").unwrap_or(theme.text);
                let (right, color) = match self.note(&key) {
                    Some(Note::Confirm) => (t("Fai clic per pulire", "Click to clean up").to_string(), red),
                    Some(Note::Busy(b)) => (b.to_string(), theme.subtext),
                    Some(Note::Failed) => (t("Non riuscito", "Failed").to_string(), red),
                    Some(Note::Done(d)) => (d.clone(), theme.color("green").unwrap_or(theme.text)),
                    None => match sizes {
                        Some((used, free)) => {
                            let free = if free > 0.0 { format!(" · {} {}", crate::i18n::size(free as u64), t("liberabili", "to free")) } else { String::new() };
                            (format!("{}{free}", crate::i18n::size(used as u64)), theme.subtext)
                        }
                        None => (String::new(), theme.subtext),
                    },
                };
                let primary = kind == "all";
                sc.text(Rect::new(r.x + PAD, y, 150.0, ROW), label, Font::new(theme.font_size, Weight::Regular), if primary { theme.primary } else { theme.text }, Align::Start);
                sc.text(Rect::new(r.x + 150.0, y, WIDE - 150.0 - PAD, ROW), right, small, color, Align::End);
                sc.zone(r, theme.row_radius, format!("prune:{kind}"));
            }
            y += ROW;
        }
        let r = Rect::new(MARGIN, y + 4.0, WIDE, ROW);
        if let Some(sc) = scene {
            sc.text(Rect::new(r.x + PAD, r.y, r.w - 2.0 * PAD, r.h), t("Apri Docker Desktop", "Open Docker Desktop"), Font::new(theme.font_size, Weight::Regular), theme.primary, Align::Start);
            sc.zone(r, theme.row_radius, "desktop");
        }
        r.y + ROW + MARGIN - 2.0
    }
}

impl super::Widget for Docker {
    fn pill(&self, theme: &Theme) -> Option<Pill> {
        let s = self.snapshot.as_ref()?;
        let up = s.containers.iter().filter(|c| c.up()).count();
        let failing = s.containers.iter().any(Container::failing);
        let color = if failing {
            theme.color("red").unwrap_or(theme.text)
        } else if up == 0 {
            theme.overlay
        } else {
            BRAND
        };
        let mut items = vec![Item::icon(BOX, color)];
        if up > 0 {
            items.push(Item::text(up.to_string(), if failing { color } else { theme.text }));
        }
        Some(Pill::new(items).gap(6.0))
    }

    fn handles(&self) -> Vec<HANDLE> {
        self.inbox.wake.handle().into_iter().chain(self.timer.handle()).collect()
    }

    fn signaled_at(&mut self, index: usize) -> bool {
        if index == 1 { self.due() } else { self.take() }
    }

    fn event(&mut self, signal: &Signal) -> bool {
        if self.mock {
            return false;
        }
        match signal {
            Signal::Shown(shown) if *shown != self.shown => {
                self.shown = *shown;
                self.connect();
                false
            }
            Signal::Game(game) if *game != self.game => {
                self.game = *game;
                self.connect();
                false
            }
            Signal::Resume => {
                self.stream = None;
                self.connect();
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
        if open {
            self.measure();
            self.size();
        } else {
            self.stats_at = None;
            self.notes.retain(|_, (n, _)| matches!(n, Note::Busy(_)));
            if !self.mock {
                self.usage.clear();
            }
            self.arm();
        }
    }

    fn popup(&self, theme: &Theme, fonts: &Fonts) -> Option<Scene> {
        self.snapshot.as_ref()?;
        let height = self.layout(None, theme, fonts);
        let mut scene = Scene::new(WIDTH, height);
        self.layout(Some(&mut scene), theme, fonts);
        Some(scene)
    }

    fn popup_input(&mut self, input: &Input) -> Reply {
        let Input::Action(action) = input else { return Reply::Nothing };
        let (what, rest) = action.split_once(':').unwrap_or((action, ""));
        match what {
            "desktop" => {
                let app = r"C:\Program Files\Docker\Docker\Docker Desktop.exe";
                if !self.mock && std::path::Path::new(app).exists() {
                    crate::process::open(app);
                }
                Reply::Opened
            }
            "port" => {
                if !self.mock {
                    crate::process::open(&format!("http://localhost:{rest}"));
                }
                Reply::Opened
            }
            "prune" => {
                if !self.mock {
                    self.prune(rest);
                }
                Reply::Redraw
            }
            op => match Op::of(op) {
                Some(op) if !self.mock => {
                    self.button(rest, op);
                    Reply::Redraw
                }
                _ => Reply::Nothing,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_of_the_engine() {
        let plain = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 7\r\n\r\n{\"a\":1}extra";
        assert_eq!(parse_response(plain), Some((200, b"{\"a\":1}".to_vec())));
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n";
        assert_eq!(parse_response(chunked), Some((200, b"{\"a\":1}".to_vec())));
        assert_eq!(parse_response(b"HTTP/1.1 404 Not Found\r\n\r\n"), Some((404, vec![])));
        assert_eq!(parse_response(b"garbage"), None);
        assert_eq!(query(r#"{"type":["container"]}"#), "%7B%22type%22%3A%5B%22container%22%5D%7D");
        assert_eq!(rfc3339("1970-01-02T00:00:01.5Z"), Some(86_401.5));
    }

    #[test]
    fn containers_of_the_list() {
        let j = Json::parse(r#"{"Id": "abc", "Names": ["/shop-db-1"], "Image": "postgres:16", "State": "running", "Status": "Up 2 hours (health: starting)", "Labels": {"com.docker.compose.project": "shop"}, "Ports": [{"PublicPort": 5432}, {"PrivatePort": 1}, {"PublicPort": 5432}]}"#).unwrap();
        let c = container(&j);
        assert_eq!((c.name.as_str(), c.project.as_str(), c.health.as_str(), c.ports.clone()), ("shop-db-1", "shop", "starting", vec![5432]));
        let j = Json::parse(r#"{"Id": "x", "Names": ["/w"], "State": "exited", "Status": "Exited (1) 3 minutes ago"}"#).unwrap();
        let c = container(&j);
        assert_eq!(c.exit, Some(1));
        assert!(c.can(Op::Start) && !c.can(Op::Stop) && c.can(Op::Remove));
        assert_eq!(Op::Start.path("p", "paused"), ("POST", "/containers/p/unpause".into()));
        assert_eq!(prune_paths("all").len(), 4);
    }

    #[test]
    fn the_pipe() {
        // Without DOCKER_HOST, Docker Desktop's
        if std::env::var("DOCKER_HOST").is_err() {
            assert_eq!(pipe(), r"\\.\pipe\docker_engine");
        }
    }
}
