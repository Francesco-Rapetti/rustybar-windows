//! Home Assistant: the devices of the house, live, and a dashboard of tiles to control them, as the
//! macOS RustyBar's.
//!
//! - Pill: Home Assistant's logo in its blue with how many devices of the popup are on (lights,
//!   switches, fans, the climate in any mode but off, the vacuum while it cleans), gray and
//!   without the count while Home Assistant can't be reached. The logo is the server's own mask
//!   icon (`/static/icons/mask-icon.svg`), fetched once after the first connection and kept in
//!   `%LOCALAPPDATA%\rustybar`: a trademark, so it isn't in the repo, and until then there's a
//!   house of Segoe Fluent Icons. While the 3D printer prints, a printer and the progress in a
//!   part of their own on the left (yellow while paused), a line between, as two pills on macOS.
//! - Popup: the groups of `groups`, each a title and its tiles, two in a row; a tile is the icon,
//!   the name (Home Assistant's, or the tile's own) and the state, in the color of the state, on
//!   a lighter background while on. The icons are the ones Home Assistant shows (Material Design
//!   Icons, `home_icons.txt`, drawn by `svg.rs`), where macOS has its symbols; a tile's `icon` is
//!   one of them (`fan`, `mdi:fan`), a symbol of macOS as the macOS RustyBar's config names it
//!   (`lightbulb`, `powerplug.fill`) or a glyph of Segoe Fluent Icons. A light toggles at a
//!   click, a bulb lit only while on, and has a slider when dimmable (a bar filled to the
//!   brightness with the percentage on it, as on macOS; set when released, 0 turns it off); a
//!   switch, fan or input_boolean toggles; a cover toggles and has a slider with its position; a
//!   climate is two tiles wide, with the icon of its mode (a flame peach for heat, a snowflake
//!   sky for cool, a drop teal for dry, a fan teal, a thermometer otherwise), the room's
//!   temperature, the modes as icons in a row and − target + (sent a second after the last
//!   click); a vacuum cleans or goes back; the printer's plug (`printer.plug`) is two tiles wide
//!   with the progress, and turns off only at a second click, through `printer.off`; scripts and
//!   scenes are chips after the tiles of their group. Unavailable: gray, no actions. A service
//!   that fails: `Non riuscito` in red until the entity changes. Last, «Apri Home Assistant»,
//!   which opens `dashboard`.
//! - Data: the WebSocket API (`net::Socket`), `subscribe_entities` for the tiles and the
//!   printer's sensors only, open while a bar shows: Home Assistant says what changed, and the bar
//!   does nothing in between (WinHTTP keeps the connection alive on its own threads). A dropped
//!   connection is tried again after 1 s, then every 10 s (gray only from the second failure, so
//!   a blip doesn't dim the bar), and at once on wake. Actions are `POST
//!   /api/services/<domain>/<service>` on a worker thread. The token, a long-lived access token,
//!   is in Credential Manager (`Get-Clipboard | rustybar secret set home-assistant`).

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use windows::Win32::Foundation::HANDLE;

use super::{Input, Reply, Signal};
use crate::config::Options;
use crate::i18n::t;
use crate::json::Json;
use crate::log;
use crate::net::{self, Socket};
use crate::theme::{Rgba, Theme};
use crate::ui::{Align, Font, Fonts, Item, Outline, Pill, Rect, Scene, Weight};
use crate::wake::{Timer, Wake};

// Segoe Fluent Icons: the house until the server's logo is in, − and +. The rest are the icons
// Home Assistant shows (`home_icons.txt`): Segoe has no plug, fan, flame, thermometer or blind
const HOUSE: &str = "\u{E80F}";
const PLUS: &str = "\u{E710}";
const MINUS: &str = "\u{E738}";

/// Material Design Icons by name, as `home_icons.txt` has them.
const ICONS: &str = include_str!("home_icons.txt");

thread_local! {
    static OUTLINES: RefCell<HashMap<String, Option<Arc<Outline>>>> = RefCell::new(HashMap::new());
}

/// An icon of `home_icons.txt`, read the first time it's drawn.
fn icon(name: &str) -> Option<Arc<Outline>> {
    OUTLINES.with(|o| {
        o.borrow_mut()
            .entry(name.to_string())
            .or_insert_with(|| {
                let d = ICONS.lines().find_map(|l| l.strip_prefix(name)?.strip_prefix('|'))?;
                Outline::new(d, [0.0, 0.0, 24.0, 24.0]).map(Arc::new)
            })
            .clone()
    })
}

/// The icon a name means: one of `home_icons.txt` (`fan`, or `mdi:fan` as in Home Assistant), or
/// a symbol of macOS (`lightbulb`, `powerplug.fill`), as the macOS RustyBar's config names them.
fn named(name: &str) -> Option<&'static str> {
    let name = name.trim();
    let mdi = name.strip_prefix("mdi:").unwrap_or(name);
    if let Some(line) = ICONS.lines().filter(|l| !l.starts_with('#')).find(|l| l.split('|').next() == Some(mdi)) {
        return line.split('|').next();
    }
    Some(match name.trim_end_matches(".fill") {
        "lightbulb" | "lightbulb.led" | "lightbulb.2" | "light.max" | "light.min" | "light.panel" | "light.strip.leftthird.rightthird" => "lightbulb",
        "powerplug" | "poweroutlet.type.f" | "poweroutlet.type.c" | "poweroutlet.strip" => "power-plug",
        "fan" | "fan.desk" | "fan.ceiling" | "fan.floor" | "wind" => "fan",
        "power" | "togglepower" | "poweron" | "switch.2" => "power",
        "roller.shade.open" | "blinds.horizontal.open" | "blinds.vertical.open" | "curtains.open" | "window.shade.open" => "roller-shade",
        "roller.shade.closed" | "blinds.horizontal.closed" | "blinds.vertical.closed" | "curtains.closed" | "window.shade.closed" => "roller-shade-closed",
        "robotic.vacuum" => "robot-vacuum",
        "printer" | "printer.dotmatrix" => "printer",
        "snowflake" => "snowflake",
        "flame" => "fire",
        "humidity" | "drop.degreesign" | "humidifier" | "dehumidifier" => "water-percent",
        "drop" | "drop.circle" | "drop.triangle" => "water",
        "thermometer" | "thermometer.medium" | "thermometer.low" | "thermometer.high" | "thermometer.sun" | "thermometer.snowflake" => "thermometer",
        "house" | "homekit" | "house.circle" => "home",
        "tv" | "tv.inset" | "appletv" | "play.tv" => "television",
        "lamp.desk" => "desk-lamp",
        "lamp.floor" => "floor-lamp",
        "lamp.table" | "lamp.ceiling" => "lamp",
        "washer" => "washing-machine",
        "dishwasher" => "dishwasher",
        "refrigerator" => "fridge",
        "speaker" | "hifispeaker" | "homepod" | "speaker.wave.2" => "speaker",
        "sofa" | "chair.lounge" => "sofa",
        "bed.double" | "bed.single" => "bed",
        "air.conditioner.horizontal" | "air.conditioner.vertical" | "air.purifier" => "air-conditioner",
        "heater.vertical" | "fireplace" | "heat.waves" => "radiator",
        "door.garage.closed" | "door.garage.open" | "car.garage" => "garage",
        "lock" | "lock.open" | "lock.shield" => "lock",
        "door.left.hand.closed" | "door.left.hand.open" | "door.right.hand.closed" | "door.right.hand.open" | "door.french.closed" | "door.french.open" => "door",
        "play" | "play.circle" => "play",
        "hourglass" => "timer-sand",
        _ => return None,
    })
}

/// What a tile shows as its icon.
#[derive(Clone, Debug, PartialEq)]
enum Look {
    /// A glyph of Segoe Fluent Icons.
    Glyph(String),
    /// An icon of `home_icons.txt`, by name.
    Icon(&'static str),
}

impl Look {
    /// Drawn in `r`, in `color`.
    fn draw(&self, scene: &mut Scene, r: Rect, color: Rgba) {
        match self {
            Look::Glyph(g) => scene.text(r, g.as_str(), Font::icons(r.h - 4.0), color, Align::Start),
            Look::Icon(name) => {
                if let Some(o) = icon(name) {
                    scene.outline(r, o, color);
                }
            }
        }
    }
}

/// Home Assistant's blue.
const BRAND: Rgba = Rgba(0x18 as f32 / 255.0, 0xbc as f32 / 255.0, 0xf2 as f32 / 255.0, 1.0);

// The popup's layout, in DIPs.
const MARGIN: f32 = 8.0;
const TILE: f32 = 150.0;
const GAP: f32 = 8.0;
const WIDE: f32 = TILE * 2.0 + GAP;
const WIDTH: f32 = WIDE + 2.0 * MARGIN;
const PAD: f32 = 10.0;
const TILE_H: f32 = 78.0;
const SLIDER_H: f32 = 22.0;
/// A climate's mode as an icon to click, as on macOS.
const MODE_W: f32 = 30.0;
const TOP_H: f32 = 52.0;
const LINE_H: f32 = 24.0;
const CHIP_H: f32 = 28.0;
const TITLE_H: f32 = 24.0;
const BUTTON_H: f32 = 32.0;

/// How long a value set from the popup shows before Home Assistant has it.
const PENDING: Duration = Duration::from_secs(15);
const PRINTING: [&str; 4] = ["running", "pause", "prepare", "slicing"];

#[derive(Clone, Debug, Default, PartialEq)]
struct Entity {
    state: String,
    attributes: BTreeMap<String, Json>,
}

impl Entity {
    fn num(&self, key: &str) -> Option<f64> {
        self.attributes.get(key).and_then(Json::f64)
    }

    fn name(&self) -> Option<&str> {
        self.attributes.get("friendly_name").and_then(Json::str)
    }
}

/// An entity of a group: its id, or a table with a name and an icon of its own (see `named`, or a
/// glyph of Segoe Fluent Icons, as the character or its code: `"E7F4"`).
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum TileDef {
    Id(String),
    Full {
        id: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        icon: Option<String>,
    },
}

impl TileDef {
    fn id(&self) -> &str {
        match self {
            TileDef::Id(id) | TileDef::Full { id, .. } => id,
        }
    }

    fn name(&self) -> Option<&str> {
        match self {
            TileDef::Full { name, .. } => name.as_deref(),
            TileDef::Id(_) => None,
        }
    }

    fn icon(&self) -> Option<Look> {
        let TileDef::Full { icon: Some(icon), .. } = self else { return None };
        if let Some(name) = named(icon) {
            return Some(Look::Icon(name));
        }
        let code = icon.trim_start_matches("\\u").trim_start_matches("U+");
        match u32::from_str_radix(code, 16).ok().and_then(char::from_u32) {
            Some(c) if code.len() == 4 => Some(Look::Glyph(c.to_string())),
            // A glyph itself; a name nothing here knows means nothing
            _ => (icon.chars().count() <= 2).then(|| Look::Glyph(icon.clone())),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Group {
    title: String,
    entities: Vec<TileDef>,
}

/// A 3D printer, e.g. of the Bambu Lab integration: its plug among the tiles, the script that
/// turns it off, and the sensors of its status, progress (%) and time left (hours).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Printer {
    plug: String,
    off: String,
    status: String,
    progress: String,
    remaining: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Setup {
    server: String,
    /// A path on the server or an address.
    dashboard: String,
    groups: Vec<Group>,
    /// Entities for the Lua widgets (`rb.home`), followed besides the tiles.
    watch: Vec<String>,
    printer: Printer,
}

/// What a tile says for a moment, over its state.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Note {
    Failed,
    ConfirmOff,
    TurningOff,
    Printing,
    Running,
}

fn domain(id: &str) -> &str {
    id.split('.').next().unwrap_or("")
}

/// `26,5°` in Italian, `26.5°` in English; whole degrees without decimals.
fn degrees(v: f64) -> String {
    let r = (v * 10.0).round() / 10.0;
    format!("{}°", crate::i18n::number(r, if r.fract() == 0.0 { 0 } else { 1 }))
}

/// A climate mode: its icon, color (by palette name) and name, as the macOS RustyBar has them.
fn mode_look(mode: &str) -> (&'static str, &'static str, String) {
    let (icon, color, name) = match mode {
        "off" => ("thermometer", "subtext0", t("Spento", "Off")),
        "heat" => ("fire", "peach", t("Caldo", "Heat")),
        "cool" => ("snowflake", "sky", t("Freddo", "Cool")),
        "dry" => ("water-percent", "teal", t("Deumidifica", "Dry")),
        "fan_only" => ("fan", "teal", t("Ventola", "Fan")),
        "heat_cool" => ("thermometer", "text", t("Caldo/freddo", "Heat/Cool")),
        "auto" => ("alpha-a-circle-outline", "text", t("Automatico", "Auto")),
        other => return ("thermometer", "text", other.to_string()),
    };
    (icon, color, name.to_string())
}

/// A tile's background: lighter, with a hint of the state's color, while it's on (as on macOS).
fn tile_color(theme: &Theme, color: Rgba, lit: bool) -> Rgba {
    if lit { theme.text.mix(color, 0.25).alpha(0.11) } else { theme.tile(color, false) }
}

/// A slider as the macOS RustyBar's: a rounded bar filled up to the value, with the percentage
/// on the fill, or just after it when the fill is too short.
fn level_slider(scene: &mut Scene, theme: &Theme, fonts: &Fonts, r: Rect, value: f32) {
    let v = value.clamp(0.0, 1.0);
    // The track darker than the tile, as on macOS
    scene.fill(r, r.h / 2.0, theme.color("crust").unwrap_or(theme.surface).alpha(0.7));
    let edge = if v > 0.0 { (r.w * v).max(r.h) } else { 0.0 };
    if edge > 0.0 {
        scene.fill(Rect::new(r.x, r.y, edge, r.h), r.h / 2.0, theme.primary);
    }
    let text = format!("{:.0}%", v * 100.0);
    let font = Font::new(theme.small_size, Weight::SemiBold);
    let tw = fonts.width(&text, font).ceil();
    let (inset, gap) = (8.0, 6.0);
    let (x, on_fill) = if edge - inset - tw >= inset {
        (edge - inset - tw, true)
    } else if (edge + gap).max(inset) + tw <= r.w - inset {
        ((edge + gap).max(inset), false)
    } else {
        ((r.w - tw) / 2.0, edge > r.w / 2.0)
    };
    scene.text(Rect::new(r.x + x, r.y, tw + 2.0, r.h), text, font, if on_fill { theme.on_primary } else { theme.text }, Align::Start);
}

/// Where the server's logo is kept: `%LOCALAPPDATA%\rustybar\home-assistant-logo.svg`, by the log.
fn logo_path() -> std::path::PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("rustybar").join("home-assistant-logo.svg")
}

/// `1 h 20 min`, `45 min`.
fn span(minutes: f64) -> String {
    let m = minutes.max(0.0).round() as u64;
    if m >= 60 { format!("{} h {} min", m / 60, m % 60) } else { format!("{m} min") }
}

/// JSON (the made-up data) as TOML, to read the setup the same way as the config.
fn to_toml(j: &Json) -> Option<toml::Value> {
    Some(match j {
        Json::Null => return None,
        Json::Bool(b) => toml::Value::Boolean(*b),
        Json::Number(n) if n.fract() == 0.0 => toml::Value::Integer(*n as i64),
        Json::Number(n) => toml::Value::Float(*n),
        Json::String(s) => toml::Value::String(s.clone()),
        Json::Array(a) => toml::Value::Array(a.iter().filter_map(to_toml).collect()),
        Json::Object(m) => toml::Value::Table(m.iter().filter_map(|(k, v)| Some((k.clone(), to_toml(v)?))).collect()),
    })
}

/// What a worker thread leaves: the entity a service was for, and whether it went through.
type Results = Arc<Mutex<Vec<(String, bool)>>>;

pub struct Home {
    setup: Setup,
    /// None until the first connection or failure.
    online: Option<bool>,
    no_token: bool,
    states: HashMap<String, Entity>,
    token: Option<String>,
    socket: Option<Socket>,
    /// Set by the socket and by the worker threads of the services.
    wake: Arc<Wake>,
    results: Results,
    next_id: u64,
    /// In a row.
    failures: u32,
    retry_at: Option<Instant>,
    notes: HashMap<String, (Note, Option<Instant>)>,
    /// Brightness, position or target temperature set from the popup, until Home Assistant has it.
    pending: HashMap<String, (f64, Instant)>,
    pending_mode: HashMap<String, String>,
    /// A climate's target, sent when the clicks on − and + stop.
    nudge: Option<(String, f64, Instant)>,
    /// The slider being dragged in the popup, and where.
    drag: Option<(String, f32)>,
    /// Home Assistant's logo as its server has it, for the pill (see `fetch_logo`).
    logo: Option<Arc<Outline>>,
    logo_asked: bool,
    /// The logo's document, from the worker thread that fetched it.
    logo_svg: Arc<Mutex<Option<String>>>,
    timer: Timer,
    shown: bool,
    mock: bool,
}

impl Home {
    pub fn new(options: &Options) -> Home {
        let mut setup: Setup = toml::Value::Table(options.0.clone()).try_into().unwrap_or_else(|e| {
            log!("home: {e}");
            Setup::default()
        });
        let mut w = Home {
            setup: Setup::default(),
            online: None,
            no_token: false,
            states: HashMap::new(),
            token: None,
            socket: None,
            wake: Wake::new(),
            results: Arc::new(Mutex::new(Vec::new())),
            next_id: 1,
            failures: 0,
            retry_at: None,
            notes: HashMap::new(),
            pending: HashMap::new(),
            pending_mode: HashMap::new(),
            nudge: None,
            drag: None,
            logo: None,
            logo_asked: false,
            logo_svg: Arc::new(Mutex::new(None)),
            timer: Timer::new(),
            shown: true,
            mock: false,
        };
        if let Some(m) = crate::mock::load("home") {
            w.mock = true;
            if setup.groups.is_empty() {
                let mock: Setup = to_toml(&m).and_then(|v| v.try_into().ok()).unwrap_or_default();
                setup.groups = mock.groups;
                setup.printer = mock.printer;
            }
            w.online = Some(m["online"].bool().unwrap_or(true));
            for (id, e) in m["states"].object().into_iter().flatten() {
                w.states.insert(id.clone(), Entity { state: e["state"].str().unwrap_or_default().into(), attributes: e["attributes"].object().cloned().unwrap_or_default() });
            }
        }
        w.setup = setup;
        w.publish(&w.states.keys().cloned().collect::<Vec<_>>(), false);
        if !w.mock && w.configured() {
            w.logo = std::fs::read_to_string(logo_path()).ok().and_then(|s| Outline::from_svg(&s)).map(Arc::new);
            w.connect();
        }
        w
    }

    /// Home Assistant's logo, the mask icon its server has for browsers, once: saved next to the
    /// log so the bar has it at once from then on (a trademark: it isn't in the repo, as on macOS).
    fn fetch_logo(&mut self) {
        if self.logo.is_some() || self.logo_asked || self.mock {
            return;
        }
        self.logo_asked = true;
        let url = format!("{}/static/icons/mask-icon.svg", self.base());
        let (out, wake) = (self.logo_svg.clone(), self.wake.clone());
        std::thread::spawn(move || {
            let Ok(r) = net::request("GET", &url, &[], None, 10_000) else { return };
            let svg = String::from_utf8_lossy(&r.body).into_owned();
            if r.status != 200 || Outline::from_svg(&svg).is_none() {
                return;
            }
            let path = logo_path();
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, &svg);
            if let Ok(mut o) = out.lock() {
                *o = Some(svg);
            }
            wake.set();
        });
    }

    /// The logo the worker thread fetched. True if the pill changes.
    fn take_logo(&mut self) -> bool {
        let Some(svg) = self.logo_svg.lock().ok().and_then(|mut o| o.take()) else { return false };
        self.logo = Outline::from_svg(&svg).map(Arc::new);
        self.logo.is_some()
    }

    fn configured(&self) -> bool {
        !self.setup.server.trim().is_empty()
    }

    fn base(&self) -> String {
        self.setup.server.trim().trim_end_matches('/').to_string()
    }

    fn is_online(&self) -> bool {
        self.online == Some(true)
    }

    /// The tiles and the printer's sensors, once each.
    fn entities(&self) -> Vec<String> {
        let p = &self.setup.printer;
        let tiles = self.setup.groups.iter().flat_map(|g| g.entities.iter().map(|e| e.id().to_string()));
        let mut ids: Vec<String> = Vec::new();
        for id in tiles.chain([&p.status, &p.progress, &p.remaining].into_iter().cloned()).chain(self.setup.watch.iter().cloned()) {
            if !id.is_empty() && !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids
    }

    // ---- the Lua widgets (`rb.home`) ---------------------------------------------------------

    /// Tells the Lua widgets what changed: the states of `changed`, and whether Home Assistant
    /// answers; with `came` (it just came online) or a state that changed, the event
    /// `home_assistant` for the widgets that listen to it.
    fn publish(&self, changed: &[String], came: bool) {
        let online = self.is_online();
        BRIDGE.with(|b| {
            let mut b = b.borrow_mut();
            b.present = true;
            b.online = online;
            b.wake = Some(self.wake.clone());
            for id in changed {
                match self.states.get(id) {
                    Some(e) => b.states.insert(id.clone(), e.clone()),
                    None => b.states.remove(id),
                };
            }
        });
        if self.mock && !came {
            return;
        }
        for id in changed {
            let state = self.states.get(id).map(|e| e.state.clone()).unwrap_or_default();
            crate::app::push(crate::app::Event::Trigger { name: "home_assistant".into(), args: vec![("entity".into(), id.clone()), ("state".into(), state)] });
        }
        if came && changed.is_empty() {
            crate::app::push(crate::app::Event::Trigger { name: "home_assistant".into(), args: Vec::new() });
        }
    }

    /// The services the Lua widgets asked for, sent as the tiles' are.
    fn lua_calls(&mut self) {
        let calls = BRIDGE.with(|b| std::mem::take(&mut b.borrow_mut().calls));
        for (domain, service, data) in calls {
            let id = data["entity_id"].str().unwrap_or("lua").to_string();
            self.service(&id, &domain, &service, data);
        }
    }

    // ---- the connection ----------------------------------------------------------------------

    fn connect(&mut self) {
        self.socket = None;
        self.retry_at = None;
        if self.token.is_none() {
            self.token = crate::secret::get("home-assistant");
            self.no_token = self.token.is_none();
            if self.no_token {
                log!("home: no token: Get-Clipboard | rustybar secret set home-assistant");
                self.online = Some(false);
                return;
            }
        }
        let url = format!("{}/api/websocket", self.base());
        match Socket::open(&url, &[], self.wake.clone()) {
            Ok(socket) => self.socket = Some(socket),
            Err(e) => {
                log!("home: {url}: {e}");
                self.disconnected();
            }
        }
    }

    fn send(&mut self, mut message: Json) {
        if let Json::Object(m) = &mut message
            && m.get("type").and_then(Json::str) != Some("auth")
        {
            m.insert("id".into(), Json::Number(self.next_id as f64));
            self.next_id += 1;
        }
        if let Some(socket) = &self.socket {
            socket.send(&message.to_string());
        }
    }

    /// What came on the socket. True if something shown changed.
    fn received(&mut self) -> bool {
        let Some(socket) = &self.socket else { return false };
        let (messages, closed) = socket.take();
        let mut changed = false;
        for text in messages {
            let Some(message) = Json::parse(&text) else { continue };
            match message["type"].str() {
                Some("auth_required") => {
                    let token = self.token.clone().unwrap_or_default();
                    self.send(Json::of([("type", "auth".into()), ("access_token", token.as_str().into())]));
                }
                Some("auth_ok") => {
                    self.next_id = 1;
                    let ids = Json::Array(self.entities().iter().map(|id| id.as_str().into()).collect());
                    self.send(Json::of([("type", "subscribe_entities".into()), ("entity_ids", ids)]));
                    self.fetch_logo();
                }
                // A wrong token: tried again on wake, or when the popup opens, read again
                Some("auth_invalid") => {
                    log!("home: Home Assistant refused the token: Get-Clipboard | rustybar secret set home-assistant");
                    self.socket = None;
                    self.token = None;
                    self.no_token = true;
                    self.online = Some(false);
                    return true;
                }
                Some("result") if message["success"].bool() == Some(false) => {
                    log!("home: {}", message["error"]["message"].str().unwrap_or("subscription refused"));
                    self.disconnected();
                    return true;
                }
                Some("event") => {
                    let ids = self.apply(&message["event"]);
                    let first = !self.is_online();
                    self.online = Some(true);
                    self.publish(&ids, first);
                    self.no_token = false;
                    self.failures = 0;
                    self.settle(&ids);
                    changed |= first || !ids.is_empty();
                }
                _ => {}
            }
        }
        if closed {
            self.disconnected();
            changed = true;
        }
        changed
    }

    /// The compressed states of `subscribe_entities`: `a` the entities added (all of them in the
    /// first event) with their state `s` and attributes `a`; `c` what changed, `+` the new state
    /// or attributes and `-` the attributes removed; `r` the entities removed. The ids that changed.
    fn apply(&mut self, event: &Json) -> Vec<String> {
        let mut changed = Vec::new();
        for (id, v) in event["a"].object().into_iter().flatten() {
            let e = Entity { state: v["s"].str().unwrap_or_default().to_string(), attributes: v["a"].object().cloned().unwrap_or_default() };
            if self.states.get(id) != Some(&e) {
                self.states.insert(id.clone(), e);
                changed.push(id.clone());
            }
        }
        for (id, change) in event["c"].object().into_iter().flatten() {
            let Some(e) = self.states.get_mut(id) else { continue };
            let before = e.clone();
            let added = &change["+"];
            if let Some(s) = added["s"].str() {
                e.state = s.to_string();
            }
            for (k, v) in added["a"].object().into_iter().flatten() {
                e.attributes.insert(k.clone(), v.clone());
            }
            for k in change["-"]["a"].array().iter().filter_map(Json::str) {
                e.attributes.remove(k);
            }
            if *e != before {
                changed.push(id.clone());
            }
        }
        for id in event["r"].array().iter().filter_map(Json::str) {
            if self.states.remove(id).is_some() {
                changed.push(id.to_string());
            }
        }
        changed
    }

    /// What changed ends the notes on it, and the values set from the popup once it has them.
    fn settle(&mut self, changed: &[String]) {
        for id in changed {
            self.notes.remove(id);
            self.pending_mode.remove(id);
            if let Some(&(value, _)) = self.pending.get(id)
                && self.actual(id).is_some_and(|a| (a - value).abs() < 0.6)
            {
                self.pending.remove(id);
            }
        }
    }

    fn disconnected(&mut self) {
        self.socket = None;
        self.failures += 1;
        if self.failures > 1 {
            self.online = Some(false);
        }
        let delay = if self.failures > 1 { 10 } else { 1 };
        self.retry_at = Some(Instant::now() + Duration::from_secs(delay));
        self.arm();
        self.publish(&[], false);
    }

    /// The timer, for the first of what's due: a new try, a note that ends, a value set from the
    /// popup that expires, a climate's target to send. Stopped when nothing is.
    fn arm(&mut self) {
        let deadlines = self.retry_at.into_iter().chain(self.notes.values().filter_map(|(_, until)| *until)).chain(self.pending.values().map(|(_, until)| *until)).chain(self.nudge.as_ref().map(|(_, _, at)| *at));
        self.timer.stop();
        if let Some(first) = deadlines.min() {
            let ms = first.saturating_duration_since(Instant::now()).as_millis() as i64;
            self.timer.start(ms.max(1), i32::MAX);
        }
    }

    /// What's due. True if something shown changed.
    fn due(&mut self) -> bool {
        let now = Instant::now();
        let mut changed = false;
        if self.retry_at.is_some_and(|at| at <= now) && self.shown {
            self.connect();
        }
        let before = self.notes.len() + self.pending.len();
        self.notes.retain(|_, (_, until)| until.is_none_or(|u| u > now));
        self.pending.retain(|_, (_, until)| *until > now);
        changed |= self.notes.len() + self.pending.len() != before;
        if let Some((id, target, _)) = self.nudge.take_if(|(_, _, at)| *at <= now) {
            self.service(&id, "climate", "set_temperature", Json::of([("entity_id", id.as_str().into()), ("temperature", target.into())]));
        }
        self.arm();
        changed
    }

    // ---- actions -----------------------------------------------------------------------------

    fn service(&mut self, id: &str, domain: &str, service: &str, data: Json) {
        let Some(token) = self.token.clone().filter(|_| !self.mock) else { return };
        let url = format!("{}/api/services/{domain}/{service}", self.base());
        let (results, wake, id) = (self.results.clone(), self.wake.clone(), id.to_string());
        let what = format!("{domain}.{service} {data}");
        std::thread::spawn(move || {
            let auth = format!("Bearer {token}");
            let headers = [("Authorization", auth.as_str()), ("Content-Type", "application/json")];
            let ok = match net::request("POST", &url, &headers, Some(data.to_string().as_bytes()), 10_000) {
                Ok(r) if r.status == 200 => true,
                // The tile only says it failed: why goes in the log
                Ok(r) => {
                    log!("home: {what} → {} {}", r.status, String::from_utf8_lossy(&r.body).chars().take(300).collect::<String>());
                    false
                }
                Err(e) => {
                    log!("home: {what}: {e}");
                    false
                }
            };
            results.lock().unwrap_or_else(|e| e.into_inner()).push((id, ok));
            wake.set();
        });
    }

    /// The services that failed: their tiles say so, until the entity changes.
    fn take_results(&mut self) -> bool {
        let results = std::mem::take(&mut *self.results.lock().unwrap_or_else(|e| e.into_inner()));
        let mut changed = false;
        for (id, ok) in results {
            if !ok {
                self.pending.remove(&id);
                self.pending_mode.remove(&id);
                self.notes.insert(id, (Note::Failed, None));
                changed = true;
            }
        }
        changed
    }

    /// A note that goes by itself after `secs`, unless replaced.
    fn note_for(&mut self, id: &str, note: Note, secs: u64) {
        self.notes.insert(id.to_string(), (note, Some(Instant::now() + Duration::from_secs(secs))));
        self.arm();
    }

    fn note(&self, id: &str) -> Option<Note> {
        self.notes.get(id).map(|(n, _)| *n)
    }

    fn printing(&self) -> Option<&str> {
        let status = self.states.get(&self.setup.printer.status)?.state.as_str();
        PRINTING.contains(&status).then_some(status)
    }

    /// A click on a tile or a chip.
    fn tap(&mut self, id: &str) {
        let Some(state) = self.states.get(id).map(|e| e.state.clone()) else { return };
        if !self.is_online() || state == "unavailable" {
            return;
        }
        let data = Json::of([("entity_id", id.into())]);
        if id == self.setup.printer.plug && state == "on" {
            if self.printing().is_some() {
                self.note_for(id, Note::Printing, 2);
            } else if self.note(id) == Some(Note::ConfirmOff) {
                self.notes.insert(id.to_string(), (Note::TurningOff, None));
                match self.setup.printer.off.clone() {
                    off if off.is_empty() => self.service(id, domain(id), "turn_off", data),
                    off => self.service(id, "script", "turn_on", Json::of([("entity_id", off.as_str().into())])),
                }
            } else {
                self.note_for(id, Note::ConfirmOff, 4);
            }
            return;
        }
        let d = domain(id).to_string();
        let service = match (d.as_str(), state.as_str()) {
            ("climate", "off") => "turn_on",
            ("climate", _) => "turn_off",
            ("vacuum", "cleaning") => "return_to_base",
            ("vacuum", _) => "start",
            ("script" | "scene", _) => {
                if d == "script" {
                    self.note_for(id, Note::Running, 10);
                }
                "turn_on"
            }
            _ => "toggle",
        };
        self.service(id, &d, service, data);
    }

    /// A slider released at `v` (0..1): a light's brightness, a blind's position.
    fn slide(&mut self, id: &str, v: f32) {
        if !self.is_online() {
            return;
        }
        let pct = (v as f64 * 100.0).round().clamp(0.0, 100.0);
        self.pending.insert(id.to_string(), (pct, Instant::now() + PENDING));
        self.arm();
        let entity: (&str, Json) = ("entity_id", id.into());
        match domain(id) {
            "light" if pct > 0.0 => self.service(id, "light", "turn_on", Json::of([entity, ("brightness_pct", pct.into())])),
            "light" => self.service(id, "light", "turn_off", Json::of([entity])),
            "cover" => self.service(id, "cover", "set_cover_position", Json::of([entity, ("position", pct.into())])),
            _ => {}
        }
    }

    /// − or + on a climate: the target moves at once, and goes to Home Assistant a second after
    /// the last click, so that an air conditioner gets one change.
    fn nudge(&mut self, id: &str, up: bool) {
        let Some(e) = self.states.get(id) else { return };
        let step = e.num("target_temp_step").filter(|s| *s > 0.0).unwrap_or(0.5);
        let (min, max) = (e.num("min_temp").unwrap_or(7.0), e.num("max_temp").unwrap_or(35.0));
        let Some(current) = self.shown_value(id) else { return };
        let target = (((current + if up { step } else { -step }) / step).round() * step).clamp(min, max);
        self.pending.insert(id.to_string(), (target, Instant::now() + PENDING));
        self.nudge = Some((id.to_string(), target, Instant::now() + Duration::from_secs(1)));
        self.arm();
    }

    fn set_mode(&mut self, id: &str, mode: &str) {
        if self.states.get(id).is_none_or(|e| e.state == mode) {
            return;
        }
        self.pending_mode.insert(id.to_string(), mode.to_string());
        self.service(id, "climate", "set_hvac_mode", Json::of([("entity_id", id.into()), ("hvac_mode", mode.into())]));
    }

    /// The value that a pending one stands for: brightness %, position, target temperature.
    fn actual(&self, id: &str) -> Option<f64> {
        let e = self.states.get(id)?;
        match domain(id) {
            "light" => Some(if e.state == "on" { (e.num("brightness").unwrap_or(0.0) / 2.55).round() } else { 0.0 }),
            "cover" => e.num("current_position"),
            "climate" => e.num("temperature"),
            _ => None,
        }
    }

    fn shown_value(&self, id: &str) -> Option<f64> {
        match self.pending.get(id) {
            Some(&(v, until)) if until > Instant::now() => Some(v),
            _ => self.actual(id),
        }
    }

    /// How many devices are on, for the pill.
    fn on_count(&self) -> usize {
        let ids = self.setup.groups.iter().flat_map(|g| g.entities.iter().map(TileDef::id));
        ids.filter(|id| {
            let Some(e) = self.states.get(*id) else { return false };
            match domain(id) {
                "light" | "switch" | "fan" | "input_boolean" => e.state == "on",
                "climate" => !["off", "unavailable", "unknown"].contains(&e.state.as_str()),
                "vacuum" => e.state == "cleaning",
                _ => false,
            }
        })
        .count()
    }

    fn printer_progress(&self) -> (f64, Option<f64>) {
        let num = |id: &str| self.states.get(id).and_then(|e| e.state.parse::<f64>().ok());
        (num(&self.setup.printer.progress).unwrap_or(0.0).clamp(0.0, 100.0), num(&self.setup.printer.remaining).map(|h| h * 60.0))
    }

    fn entity_name(&self, def: &TileDef) -> String {
        let id = def.id();
        def.name().or_else(|| self.states.get(id).and_then(Entity::name)).unwrap_or(id).to_string()
    }

    // ---- drawing -----------------------------------------------------------------------------

    /// A tile of a light, switch, cover or vacuum, at `x`, `y`, `h` tall.
    #[allow(clippy::too_many_arguments)]
    fn tile(&self, scene: &mut Scene, theme: &Theme, fonts: &Fonts, def: &TileDef, x: f32, y: f32, h: f32) {
        let id = def.id();
        let e = self.states.get(id);
        let state = e.map(|e| e.state.as_str()).unwrap_or("unavailable");
        let on = state == "on";
        let c = |name: &str| theme.color(name).unwrap_or(theme.text);
        let (masculine, default) = match domain(id) {
            "light" => (false, "lightbulb"),
            "fan" => (true, "fan"),
            "input_boolean" => (true, "power"),
            "vacuum" => (true, "robot-vacuum"),
            "cover" => (false, if state == "closed" { "roller-shade-closed" } else { "roller-shade" }),
            _ => (false, "power-plug"),
        };
        let mut look = def.icon().unwrap_or(Look::Icon(default));
        // A bulb is lit only when on
        if look == Look::Icon("lightbulb") && !on {
            look = Look::Icon("lightbulb-outline");
        }
        let (on_text, off_text) = if masculine { (t("Acceso", "On"), t("Spento", "Off")) } else { (t("Accesa", "On"), t("Spenta", "Off")) };
        let dimmable = e.is_some_and(|e| e.attributes.get("supported_color_modes").is_some_and(|m| m.array().iter().any(|m| m.str() != Some("onoff"))));
        let mut slider = None;
        let (color, text, text_color, lit) = match domain(id) {
            "cover" => {
                slider = Some(self.shown_value(id).unwrap_or(0.0) / 100.0);
                match state {
                    "opening" => (theme.text, t("Si apre…", "Opening…"), theme.text, true),
                    "closing" => (theme.text, t("Si chiude…", "Closing…"), theme.text, true),
                    "closed" => (theme.subtext, t("Chiusa", "Closed"), theme.subtext, false),
                    _ => (theme.text, t("Aperta", "Open"), theme.text, true),
                }
            }
            "vacuum" => match state {
                "cleaning" => (c("green"), t("Pulisce", "Cleaning"), theme.text, true),
                "returning" => (theme.text, t("Torna alla base", "Returning"), theme.text, true),
                "docked" => (theme.subtext, t("Alla base", "Docked"), theme.subtext, false),
                "paused" => (theme.subtext, t("In pausa", "Paused"), theme.subtext, false),
                "error" => (c("red"), t("Errore", "Error"), c("red"), false),
                _ => (theme.subtext, t("Fermo", "Idle"), theme.subtext, false),
            },
            _ => {
                if domain(id) == "light" && dimmable {
                    slider = Some(self.shown_value(id).unwrap_or(0.0) / 100.0);
                }
                if on { (c("yellow"), on_text, theme.text, true) } else { (theme.subtext, off_text, theme.subtext, false) }
            }
        };
        let unavailable = state == "unavailable" || !self.is_online();
        let (color, text, text_color, lit) = match self.note(id) {
            _ if unavailable => (theme.overlay, t("Non disponibile", "Unavailable"), theme.overlay, false),
            Some(Note::Failed) => (color, t("Non riuscito", "Failed"), c("red"), lit),
            _ => (color, text, text_color, lit),
        };
        let r = Rect::new(x, y, TILE, h);
        scene.fill(r, theme.row_radius.max(8.0), tile_color(theme, color, lit));
        if !unavailable {
            scene.zone(r, theme.row_radius.max(8.0), format!("tap:{id}"));
        }
        let inner = TILE - 2.0 * PAD;
        look.draw(scene, Rect::new(x + PAD - 1.0, y + PAD, 22.0, 22.0), color);
        let name = self.entity_name(def);
        scene.text(Rect::new(x + PAD, y + 34.0, inner, 18.0), name, Font::new(theme.font_size, Weight::SemiBold), if unavailable { theme.overlay } else { theme.text }, Align::Start);
        scene.text(Rect::new(x + PAD, y + 52.0, inner, 16.0), text, Font::new(theme.small_size, Weight::Medium), text_color, Align::Start);
        if let Some(v) = slider.filter(|_| !unavailable) {
            let v = self.drag.as_ref().filter(|(d, _)| d == id).map_or(v as f32, |(_, f)| *f);
            let bar = Rect::new(x + PAD, y + TILE_H - 6.0, inner, 18.0);
            level_slider(scene, theme, fonts, bar, v);
            scene.slider(bar, format!("slider:{id}"));
        }
    }

    /// The top of a wide tile: the icon, the name and the state, and something on the right.
    #[allow(clippy::too_many_arguments)]
    fn wide_top(&self, scene: &mut Scene, theme: &Theme, x: f32, y: f32, icon: &'static str, color: Rgba, name: String, state: (&str, Rgba), right: (String, Rgba), unavailable: bool) {
        Look::Icon(icon).draw(scene, Rect::new(x + PAD - 1.0, y + PAD + 3.0, 22.0, 22.0), color);
        let w = WIDE - 2.0 * PAD - 32.0 - 90.0;
        scene.text(Rect::new(x + PAD + 32.0, y + 8.0, w, 18.0), name, Font::new(theme.font_size, Weight::SemiBold), if unavailable { theme.overlay } else { theme.text }, Align::Start);
        scene.text(Rect::new(x + PAD + 32.0, y + 26.0, w, 16.0), state.0, Font::new(theme.small_size, Weight::Medium), state.1, Align::Start);
        scene.text(Rect::new(x + WIDE - PAD - 90.0, y + 8.0, 90.0, 34.0), right.0, Font::new(15.0, Weight::Medium), right.1, Align::End);
    }

    /// A climate, two tiles wide; its height.
    fn climate(&self, scene: Option<&mut Scene>, theme: &Theme, def: &TileDef, x: f32, y: f32) -> f32 {
        let id = def.id();
        let e = self.states.get(id);
        let state = e.map(|e| e.state.clone()).unwrap_or_else(|| "unavailable".into());
        let mode = self.pending_mode.get(id).cloned().unwrap_or_else(|| state.clone());
        let unavailable = state == "unavailable" || !self.is_online();
        let on = !unavailable && mode != "off";
        // The modes as icons in a row, then −, the target and + at its end; on a line of their
        // own when the row has no room for them
        let modes: Vec<String> = e.map(|e| e.attributes.get("hvac_modes").map(|m| m.array().iter().filter_map(Json::str).filter(|m| *m != "off").map(String::from).collect()).unwrap_or_default()).unwrap_or_default();
        let modes = &modes[..modes.len().min(8)];
        let controls = 26.0 + 4.0 + 46.0 + 4.0 + 26.0;
        let row = modes.len() as f32 * (MODE_W + 4.0);
        let lines = if row + 8.0 + controls > WIDE - 2.0 * PAD { 2.0 } else { 1.0 };
        let height = if on { TOP_H + lines * (LINE_H + 4.0) - 4.0 + PAD } else { TOP_H };
        let Some(scene) = scene else { return height };
        let (mode_icon, color_name, mode_name) = mode_look(&mode);
        let color = if unavailable { theme.overlay } else { theme.color(color_name).unwrap_or(theme.text) };
        let r = Rect::new(x, y, WIDE, height);
        scene.fill(r, theme.row_radius.max(8.0), tile_color(theme, color, on));
        if !unavailable {
            scene.zone(r, theme.row_radius.max(8.0), format!("tap:{id}"));
        }
        let state_text = match self.note(id) {
            _ if unavailable => (t("Non disponibile", "Unavailable"), theme.overlay),
            Some(Note::Failed) => (t("Non riuscito", "Failed"), theme.color("red").unwrap_or(theme.text)),
            _ if mode == "off" => (mode_name.as_str(), theme.subtext),
            _ => (mode_name.as_str(), theme.text),
        };
        let current = e.and_then(|e| e.num("current_temperature")).map(degrees).unwrap_or_default();
        self.wide_top(scene, theme, x, y, mode_icon, color, self.entity_name(def), state_text, (current, if on { theme.text } else { theme.subtext }), unavailable);
        if !on {
            return height;
        }
        let chip_bg = theme.surface.alpha(0.7);
        for (i, m) in modes.iter().enumerate() {
            let r = Rect::new(x + PAD + i as f32 * (MODE_W + 4.0), y + TOP_H, MODE_W, LINE_H);
            let (icon, c, _) = mode_look(m);
            let c = theme.color(c).unwrap_or(theme.text);
            let selected = *m == mode;
            scene.fill(r, 6.0, if selected { c.alpha(0.22) } else { chip_bg });
            Look::Icon(icon).draw(scene, Rect::new(r.x + (r.w - 15.0) / 2.0, r.y + (r.h - 15.0) / 2.0, 15.0, 15.0), if selected { c } else { theme.subtext });
            scene.zone(r, 6.0, format!("mode:{id}:{m}"));
        }
        let by = y + TOP_H + (lines - 1.0) * (LINE_H + 4.0);
        let plus = Rect::new(x + WIDE - PAD - 26.0, by, 26.0, LINE_H);
        let target = Rect::new(plus.x - 46.0, by, 46.0, LINE_H);
        let minus = Rect::new(target.x - 26.0, by, 26.0, LINE_H);
        for (r, glyph, action) in [(minus, MINUS, format!("temp:{id}:-")), (plus, PLUS, format!("temp:{id}:+"))] {
            scene.fill(r, 6.0, chip_bg);
            scene.text(r, glyph, Font::icons(11.0), theme.text, Align::Center);
            scene.zone(r, 6.0, action);
        }
        scene.text(target, self.shown_value(id).map(degrees).unwrap_or_default(), Font::new(theme.font_size, Weight::SemiBold), theme.text, Align::Center);
        height
    }

    /// The printer's plug, two tiles wide, with the progress while it prints; its height.
    fn printer(&self, scene: Option<&mut Scene>, theme: &Theme, def: &TileDef, x: f32, y: f32) -> f32 {
        let id = def.id();
        let state = self.states.get(id).map(|e| e.state.as_str()).unwrap_or("unavailable");
        let unavailable = state == "unavailable" || !self.is_online();
        let on = state == "on";
        let printing = self.printing().filter(|_| on && !unavailable);
        let height = if printing.is_some() { TOP_H + 34.0 } else { TOP_H };
        let Some(scene) = scene else { return height };
        let c = |name: &str| theme.color(name).unwrap_or(theme.text);
        let (progress, minutes) = self.printer_progress();
        let color = if unavailable { theme.overlay } else if on { c("yellow") } else { theme.subtext };
        let state_text = match self.note(id) {
            _ if unavailable => (t("Non disponibile", "Unavailable"), theme.overlay),
            Some(Note::Failed) => (t("Non riuscito", "Failed"), c("red")),
            Some(Note::ConfirmOff) => (t("Fai clic per spegnere", "Click to turn off"), c("red")),
            Some(Note::TurningOff) => (t("Spegnimento…", "Turning off…"), c("yellow")),
            Some(Note::Printing) => (t("In stampa", "Printing"), c("red")),
            _ if printing.is_some() => (t("In stampa", "Printing"), theme.text),
            _ if on => (t("Accesa", "On"), theme.text),
            _ => (t("Spenta", "Off"), theme.subtext),
        };
        let r = Rect::new(x, y, WIDE, height);
        scene.fill(r, theme.row_radius.max(8.0), tile_color(theme, color, on && !unavailable));
        if !unavailable {
            scene.zone(r, theme.row_radius.max(8.0), format!("tap:{id}"));
        }
        let paused = printing == Some("pause");
        let percent = printing.map(|_| (format!("{progress:.0}%"), if paused { c("yellow") } else { theme.text })).unwrap_or((String::new(), theme.text));
        self.wide_top(scene, theme, x, y, "printer", color, self.entity_name(def), state_text, percent, unavailable);
        if let Some(status) = printing {
            let bar = Rect::new(x + PAD, y + TOP_H, WIDE - 2.0 * PAD, 6.0);
            scene.fill(bar, 3.0, theme.surface);
            scene.fill(Rect::new(bar.x, bar.y, bar.w * (progress / 100.0) as f32, bar.h), 3.0, if paused { c("yellow") } else { theme.primary });
            let detail = match status {
                "prepare" | "slicing" => t("Si prepara a stampare…", "Getting ready to print…").to_string(),
                "pause" => t("In pausa", "Paused").to_string(),
                _ => minutes.map(|m| if crate::i18n::italian() { format!("ancora {}", span(m)) } else { format!("{} left", span(m)) }).unwrap_or_default(),
            };
            scene.text(Rect::new(x + PAD, y + TOP_H + 10.0, WIDE - 2.0 * PAD, 16.0), detail, Font::new(theme.small_size, Weight::Medium), theme.subtext, Align::Start);
        }
        height
    }

    /// A script or a scene: its chip at `x`, `y`, `w` wide.
    fn chip(&self, scene: &mut Scene, theme: &Theme, def: &TileDef, r: Rect) {
        let id = def.id();
        let e = self.states.get(id);
        let unavailable = e.is_none_or(|e| e.state == "unavailable") || !self.is_online();
        let running = e.is_some_and(|e| e.state == "on") || self.note(id) == Some(Note::Running);
        let (icon, color) = match self.note(id) {
            _ if unavailable => ("play", theme.overlay),
            Some(Note::Failed) => ("alert-circle", theme.color("red").unwrap_or(theme.text)),
            _ if running => ("timer-sand", theme.color("yellow").unwrap_or(theme.text)),
            _ => ("play", theme.primary),
        };
        scene.fill(r, CHIP_H / 2.0, theme.surface.alpha(0.7));
        Look::Icon(icon).draw(scene, Rect::new(r.x + 9.0, r.y + (r.h - 16.0) / 2.0, 16.0, 16.0), color);
        scene.text(Rect::new(r.x + 30.0, r.y, r.w - 40.0, r.h), self.entity_name(def), Font::new(theme.small_size + 1.0, Weight::SemiBold), if unavailable { theme.overlay } else { theme.text }, Align::Start);
        if !unavailable {
            scene.zone(r, CHIP_H / 2.0, format!("tap:{id}"));
        }
    }

    /// The popup's content from `y`, drawn into `scene` if there's one; where it ends.
    fn layout(&self, mut scene: Option<&mut Scene>, theme: &Theme, fonts: &Fonts) -> f32 {
        let mut y = MARGIN;
        let message = |scene: &mut Option<&mut Scene>, y: &mut f32, text: &str, color: Rgba, font: Font| {
            if let Some(s) = scene.as_deref_mut() {
                s.text(Rect::new(MARGIN + PAD, *y, WIDE - 2.0 * PAD, 26.0), text, font, color, Align::Start);
            }
            *y += 26.0;
        };
        match self.online {
            Some(true) => {
                for group in &self.setup.groups {
                    if !group.title.is_empty() {
                        if let Some(s) = scene.as_deref_mut() {
                            s.text(Rect::new(MARGIN + 4.0, y, WIDE - 8.0, TITLE_H), group.title.as_str(), Font::new(theme.font_size, Weight::SemiBold), theme.text, Align::Start);
                        }
                        y += TITLE_H + 2.0;
                    }
                    let mut pair: Vec<&TileDef> = Vec::new();
                    let mut chips: Vec<&TileDef> = Vec::new();
                    let flush = |pair: &mut Vec<&TileDef>, scene: &mut Option<&mut Scene>, y: &mut f32| {
                        if pair.is_empty() {
                            return;
                        }
                        // As tall as the taller one: with a slider or not
                        let h = pair.iter().map(|d| if self.has_slider(d.id()) { TILE_H + SLIDER_H } else { TILE_H }).fold(0.0, f32::max);
                        if let Some(s) = scene.as_deref_mut() {
                            for (i, d) in pair.iter().enumerate() {
                                self.tile(s, theme, fonts, d, MARGIN + i as f32 * (TILE + GAP), *y, h);
                            }
                        }
                        *y += h + GAP;
                        pair.clear();
                    };
                    for def in &group.entities {
                        let id = def.id();
                        match domain(id) {
                            "script" | "scene" => chips.push(def),
                            "climate" => {
                                flush(&mut pair, &mut scene, &mut y);
                                y += self.climate(scene.as_deref_mut(), theme, def, MARGIN, y) + GAP;
                            }
                            _ if id == self.setup.printer.plug => {
                                flush(&mut pair, &mut scene, &mut y);
                                y += self.printer(scene.as_deref_mut(), theme, def, MARGIN, y) + GAP;
                            }
                            _ => {
                                pair.push(def);
                                if pair.len() == 2 {
                                    flush(&mut pair, &mut scene, &mut y);
                                }
                            }
                        }
                    }
                    flush(&mut pair, &mut scene, &mut y);
                    // The chips fill lines as wide as two tiles
                    let font = Font::new(theme.small_size + 1.0, Weight::SemiBold);
                    let mut x = MARGIN;
                    for def in chips {
                        let w = (fonts.width(&self.entity_name(def), font).ceil() + 40.0).min(WIDE);
                        if x > MARGIN && x + w > MARGIN + WIDE {
                            x = MARGIN;
                            y += CHIP_H + GAP;
                        }
                        if let Some(s) = scene.as_deref_mut() {
                            self.chip(s, theme, def, Rect::new(x, y, w, CHIP_H));
                        }
                        x += w + GAP;
                    }
                    if x > MARGIN {
                        y += CHIP_H + GAP;
                    }
                }
            }
            Some(false) if self.no_token => {
                message(&mut scene, &mut y, t("Nessun token di Home Assistant", "No Home Assistant token"), theme.subtext, Font::new(theme.font_size, Weight::Regular));
                message(&mut scene, &mut y, "Get-Clipboard | rustybar secret set home-assistant", theme.subtext, Font::new(theme.small_size, Weight::Medium));
            }
            Some(false) => message(&mut scene, &mut y, t("Home Assistant non raggiungibile", "Home Assistant can't be reached"), theme.subtext, Font::new(theme.font_size, Weight::Regular)),
            None => message(&mut scene, &mut y, t("Connessione a Home Assistant…", "Connecting to Home Assistant…"), theme.subtext, Font::new(theme.font_size, Weight::Regular)),
        }
        let r = Rect::new(MARGIN, y, WIDE, BUTTON_H);
        if let Some(s) = scene {
            s.text(Rect::new(r.x + PAD, r.y, r.w - 2.0 * PAD, r.h), t("Apri Home Assistant", "Open Home Assistant"), Font::new(theme.font_size, Weight::Regular), theme.primary, Align::Start);
            s.zone(r, theme.row_radius, "dashboard");
        }
        y + BUTTON_H + MARGIN - 2.0
    }

    fn has_slider(&self, id: &str) -> bool {
        let Some(e) = self.states.get(id).filter(|e| e.state != "unavailable") else { return false };
        match domain(id) {
            "cover" => true,
            "light" => e.attributes.get("supported_color_modes").is_some_and(|m| m.array().iter().any(|m| m.str() != Some("onoff"))),
            _ => false,
        }
    }
}

impl Home {
    /// The pill: the logo and the count, after the printer while it prints when `printer`.
    fn pill_of(&self, theme: &Theme, printer: bool) -> Option<Pill> {
        if !self.configured() && !self.mock {
            return None;
        }
        let online = self.is_online();
        let mut items = Vec::new();
        if let Some(status) = self.printing().filter(|_| online && printer) {
            let color = if status == "pause" { theme.color("yellow").unwrap_or(theme.text) } else { theme.text };
            let (progress, _) = self.printer_progress();
            if let Some(o) = icon("printer") {
                items.push(Item::outline(o, theme.font_size + 4.0, color));
            }
            items.push(Item::text(format!("{progress:.0}%"), color));
        }
        let start = items.len();
        let color = if online { BRAND } else { theme.overlay };
        let logo = match &self.logo {
            Some(o) => Item::outline(o.clone(), theme.font_size + 2.0, color),
            None => Item::icon(HOUSE, color),
        };
        // The printer and the house apart, with a line between, as two pills on macOS
        items.push(if start > 0 { logo.space(2.0 * theme.pill_padding) } else { logo });
        let count = self.on_count();
        if online && count > 0 {
            items.push(Item::text(count.to_string(), theme.text));
        }
        let pill = Pill::new(items).gap(6.0);
        Some(if start > 0 { pill.part(0, 0, theme.primary).part(start, 0, theme.primary) } else { pill })
    }
}

impl super::Widget for Home {
    fn pill(&self, theme: &Theme) -> Option<Pill> {
        self.pill_of(theme, true)
    }

    /// The logo alone, without the printer.
    fn compact(&self, theme: &Theme) -> Option<Pill> {
        self.pill_of(theme, false)
    }

    fn handles(&self) -> Vec<HANDLE> {
        self.wake.handle().into_iter().chain(self.timer.handle()).collect()
    }

    fn signaled_at(&mut self, index: usize) -> bool {
        if index == 1 {
            return self.due();
        }
        let a = self.received();
        let b = self.take_results();
        let c = self.take_logo();
        self.lua_calls();
        a || b || c
    }

    fn event(&mut self, signal: &Signal) -> bool {
        if self.mock || !self.configured() {
            return false;
        }
        match signal {
            // No bar: no connection, as with a game full screen; back with a bar
            Signal::Shown(shown) if *shown != self.shown => {
                self.shown = *shown;
                if *shown {
                    self.failures = 0;
                    self.connect();
                } else {
                    self.socket = None;
                    self.retry_at = None;
                    self.arm();
                }
                false
            }
            Signal::Resume if self.shown => {
                self.failures = 0;
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
        self.drag = None;
        // A token saved since: tried at once
        if open && !self.mock && self.configured() && self.no_token {
            self.token = None;
            self.failures = 0;
            self.connect();
        }
    }

    fn popup(&self, theme: &Theme, fonts: &Fonts) -> Option<Scene> {
        let height = self.layout(None, theme, fonts);
        let mut scene = Scene::new(WIDTH, height);
        self.layout(Some(&mut scene), theme, fonts);
        Some(scene)
    }

    fn popup_input(&mut self, input: &Input) -> Reply {
        match input {
            Input::Action("dashboard") => {
                if !self.mock {
                    let d = self.setup.dashboard.trim();
                    let url = if d.contains("://") { d.to_string() } else { format!("{}/{}", self.base(), d.trim_start_matches('/')) };
                    crate::process::open(&url);
                }
                Reply::Opened
            }
            _ if self.mock => Reply::Nothing,
            Input::Action(a) => {
                let (kind, rest) = a.split_once(':').unwrap_or((a, ""));
                match kind {
                    "tap" => self.tap(rest),
                    "temp" => {
                        if let Some((id, dir)) = rest.rsplit_once(':') {
                            self.nudge(id, dir == "+");
                        }
                    }
                    "mode" => {
                        if let Some((id, mode)) = rest.rsplit_once(':') {
                            self.set_mode(id, mode);
                        }
                    }
                    _ => return Reply::Nothing,
                }
                Reply::Redraw
            }
            // Shown where it's dragged; sent when released
            Input::Slide(a, f) => {
                let Some(id) = a.strip_prefix("slider:") else { return Reply::Nothing };
                self.drag = Some((id.to_string(), *f));
                Reply::Redraw
            }
            Input::Release(a) => {
                let Some((id, f)) = self.drag.take().filter(|(id, _)| a.strip_prefix("slider:") == Some(id.as_str())) else { return Reply::Nothing };
                self.slide(&id, f);
                Reply::Redraw
            }
            _ => Reply::Nothing,
        }
    }
}

// ---- what the Lua widgets see (`rb.home`) ------------------------------------------------------

/// What the Lua widgets see of Home Assistant, kept by the `home` widget on the main thread: the
/// states it follows (the tiles', the printer's and `watch`), whether it answers, and the services
/// the widgets ask for, which it sends as its own. Nothing without a `home` widget.
#[derive(Default)]
struct Bridge {
    present: bool,
    online: bool,
    states: HashMap<String, Entity>,
    calls: Vec<(String, String, Json)>,
    wake: Option<Arc<Wake>>,
}

thread_local! {
    static BRIDGE: RefCell<Bridge> = RefCell::new(Bridge::default());
}

/// The state and the attributes of an entity the `home` widget follows.
pub fn state(id: &str) -> Option<(String, BTreeMap<String, Json>)> {
    BRIDGE.with(|b| b.borrow().states.get(id).map(|e| (e.state.clone(), e.attributes.clone())))
}

/// Whether the `home` widget is connected to Home Assistant.
pub fn online() -> bool {
    BRIDGE.with(|b| b.borrow().online)
}

/// Asks the `home` widget to call a service (`light`, `toggle`, `{"entity_id": …}`); false
/// without one.
pub fn call(domain: &str, service: &str, data: Json) -> bool {
    BRIDGE.with(|b| {
        let mut b = b.borrow_mut();
        if !b.present {
            return false;
        }
        b.calls.push((domain.to_string(), service.to_string(), data));
        if let Some(w) = &b.wake {
            w.set();
        }
        true
    })
}

impl Drop for Home {
    fn drop(&mut self) {
        BRIDGE.with(|b| *b.borrow_mut() = Bridge::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> Home {
        let mut w = Home::new(&Options(toml::Table::new()));
        w.setup.groups = vec![Group { title: "Lights".into(), entities: vec![TileDef::Id("light.a".into()), TileDef::Id("climate.b".into()), TileDef::Id("vacuum.c".into())] }];
        w
    }

    /// What the Lua widgets see (`rb.home`): the states the widget follows, while it's there.
    #[test]
    fn lua_widgets_see_the_states() {
        let mut w = home();
        let ids = w.apply(&Json::parse(r#"{"a": {"light.a": {"s": "on", "a": {"friendly_name": "Lamp"}}}}"#).unwrap());
        w.publish(&ids, false);
        assert_eq!(state("light.a").map(|(s, _)| s).as_deref(), Some("on"));
        assert!(call("light", "toggle", Json::of([("entity_id", "light.a".into())])));
        drop(w);
        assert!(state("light.a").is_none());
        assert!(!call("light", "toggle", Json::Null));
    }

    /// The compressed events of `subscribe_entities`: added, changed (+ and −), removed.
    #[test]
    fn states_from_the_events() {
        let mut w = home();
        let first = Json::parse(r#"{"a": {"light.a": {"s": "on", "a": {"brightness": 128, "friendly_name": "A"}}, "climate.b": {"s": "cool", "a": {"temperature": 24}}}}"#).unwrap();
        assert_eq!(w.apply(&first).len(), 2);
        assert_eq!(w.on_count(), 2);
        let change = Json::parse(r#"{"c": {"light.a": {"+": {"s": "off", "a": {"x": 1}}, "-": {"a": ["brightness"]}}}}"#).unwrap();
        assert_eq!(w.apply(&change), vec!["light.a".to_string()]);
        let a = &w.states["light.a"];
        assert_eq!((a.state.as_str(), a.num("brightness"), a.num("x")), ("off", None, Some(1.0)));
        assert_eq!(w.on_count(), 1);
        // The same again changes nothing
        assert!(w.apply(&change).is_empty());
        let removed = Json::parse(r#"{"r": ["climate.b"]}"#).unwrap();
        assert_eq!(w.apply(&removed), vec!["climate.b".to_string()]);
        assert_eq!(w.on_count(), 0);
    }

    #[test]
    fn values_set_from_the_popup() {
        let mut w = home();
        w.apply(&Json::parse(r#"{"a": {"light.a": {"s": "on", "a": {"brightness": 255}}}}"#).unwrap());
        assert_eq!(w.actual("light.a"), Some(100.0));
        w.pending.insert("light.a".into(), (40.0, Instant::now() + PENDING));
        assert_eq!(w.shown_value("light.a"), Some(40.0));
        // Home Assistant has it: the pending one goes
        let ids = w.apply(&Json::parse(r#"{"c": {"light.a": {"+": {"a": {"brightness": 102}}}}}"#).unwrap());
        w.settle(&ids);
        assert!(w.pending.is_empty());
    }

    #[test]
    fn words() {
        assert_eq!(span(45.0), "45 min");
        assert_eq!(span(80.0), "1 h 20 min");
        let tile = |icon: &str| TileDef::Full { id: "switch.tv".into(), name: None, icon: Some(icon.into()) };
        assert_eq!(tile("E7F4").icon(), Some(Look::Glyph("\u{E7F4}".into())));
        // As the macOS RustyBar's config names them, and as Home Assistant does
        assert_eq!(tile("lightbulb").icon(), Some(Look::Icon("lightbulb")));
        assert_eq!(tile("powerplug.fill").icon(), Some(Look::Icon("power-plug")));
        assert_eq!(tile("mdi:fan").icon(), Some(Look::Icon("fan")));
        assert_eq!(tile("lamp.desk").icon(), Some(Look::Icon("desk-lamp")));
        assert_eq!(tile("cup.and.saucer").icon(), None);
        let setup: Setup = to_toml(&Json::parse(r#"{"groups": [{"title": "T", "entities": ["light.a", {"id": "switch.b", "name": "B"}]}]}"#).unwrap()).unwrap().try_into().unwrap();
        assert_eq!(setup.groups[0].entities.len(), 2);
        assert_eq!(setup.groups[0].entities[1].name(), Some("B"));
    }

    /// Every icon the widget draws, and every one a symbol of macOS names, is in the file.
    #[test]
    fn icons_are_there() {
        let drawn = ["lightbulb", "lightbulb-outline", "fan", "power", "robot-vacuum", "roller-shade", "roller-shade-closed", "power-plug", "printer", "play", "alert-circle", "timer-sand"];
        let modes = ["off", "heat", "cool", "dry", "fan_only", "heat_cool", "auto", "eco"].map(|m| mode_look(m).0);
        for name in drawn.iter().chain(&modes) {
            assert!(icon(name).is_some(), "{name}");
        }
        let symbols = [
            "lightbulb.fill", "powerplug", "fan.fill", "togglepower", "roller.shade.open", "roller.shade.closed", "robotic.vacuum.fill", "printer.fill", "snowflake", "flame.fill", "humidity.fill",
            "drop.fill", "thermometer.medium", "house.fill", "tv", "lamp.desk", "lamp.floor", "lamp.table", "washer", "dishwasher", "refrigerator", "speaker", "sofa", "bed.double", "air.conditioner.horizontal",
            "heater.vertical", "door.garage.closed", "lock.fill", "door.left.hand.closed", "play.fill", "hourglass",
        ];
        for s in symbols {
            let name = named(s).unwrap_or_else(|| panic!("{s}"));
            assert!(icon(name).is_some(), "{s} → {name}");
        }
    }

    /// The percentage on the fill, after it when the fill is short, in the middle when neither fits.
    #[test]
    fn level_slider_labels() {
        let theme = Theme::from_config(&crate::config::Config::default().theme);
        let fonts = Fonts::new(&theme).unwrap();
        let label = |v: f32, w: f32| {
            let mut s = Scene::new(200.0, 40.0);
            level_slider(&mut s, &theme, &fonts, Rect::new(10.0, 0.0, w, 18.0), v);
            s.shapes.iter().find_map(|sh| match sh {
                crate::ui::Shape::Text { rect, text, color, .. } => Some((rect.x - 10.0, text.clone(), *color)),
                _ => None,
            }).unwrap()
        };
        let (x, text, color) = label(0.7, 130.0);
        assert_eq!(text, "70%");
        assert!(x < 0.7 * 130.0 && color == theme.on_primary);
        let (x, text, color) = label(0.0, 130.0);
        assert_eq!((x, text.as_str(), color), (8.0, "0%", theme.text));
        let (x, _, color) = label(0.1, 130.0);
        assert!(x >= 0.1 * 130.0 && color == theme.text);
    }

    /// While the printer prints: its part and the house's, with a line between; compact, the house.
    #[test]
    fn the_printer_in_its_own_part() {
        use crate::widgets::Widget;
        let theme = Theme::from_config(&crate::config::Config::default().theme);
        let mut w = home();
        w.setup.server = "http://ha.test:8123".into();
        w.online = Some(true);
        w.setup.printer = Printer { plug: "switch.p".into(), status: "sensor.s".into(), progress: "sensor.pr".into(), ..Printer::default() };
        w.apply(&Json::parse(r#"{"a": {"switch.p": {"s": "on"}, "sensor.s": {"s": "running"}, "sensor.pr": {"s": "45"}, "light.a": {"s": "on"}}}"#).unwrap());
        let pill = w.pill(&theme).unwrap();
        assert!(pill.items[0].outline.is_some());
        assert_eq!(pill.items[1].text, "45%");
        assert_eq!(pill.parts.iter().map(|p| p.start).collect::<Vec<_>>(), [0, 2]);
        assert_eq!(pill.items[2].text, HOUSE);
        assert_eq!(pill.items[3].text, "1");
        let compact = w.compact(&theme).unwrap();
        assert!(compact.parts.is_empty() && compact.items[0].text == HOUSE);
        // With the server's logo, the logo in place of the house
        w.logo = Outline::new("M0 0h24v24z", [0.0, 0.0, 24.0, 24.0]).map(Arc::new);
        assert!(w.compact(&theme).unwrap().items[0].outline.is_some());
    }
}
