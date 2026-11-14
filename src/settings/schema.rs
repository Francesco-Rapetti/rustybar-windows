//! The page of each widget of the layout: what it does, where it sits in the bar, and its
//! options, from a schema for each built-in kind (a switch, a number with its limits and step, a
//! text, a color, a slider for a percentage, a list). The secrets (Home Assistant's token, the
//! secret addresses of the calendars) go to Windows' Credential Manager from a hidden field
//! (see `secret`), never to the files. A widget written in Lua gets a control for each key it has
//! in the config, by the type of its value, and «Add an option»; so do the keys of a built-in
//! widget that its schema doesn't know.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use super::form::{self, Control, Form, Num, Opt};
use super::pages::{self, Ctx, color, number, toggle};
use super::store::{self, Spot, Value, rb};
use crate::i18n::t;

/// Text keys: shown, or the default when there's none; the default written back goes away.
fn text(f: &mut Form, title: &str, desc: Option<&str>, spot: Spot, default: &str, placeholder: &str, width: f32) {
    let value = store::get_str(&spot).unwrap_or_else(|| default.to_string());
    let def = toml::Value::String(default.to_string());
    f.row(title, desc, Control::Text { value, placeholder: placeholder.into(), width, run: Rc::new(move |v: String| store::put(&spot, Value::from(v.trim()), Some(&def))) });
}

/// A percentage on a slider, written when it's let go.
fn percent(f: &mut Form, title: &str, desc: Option<&str>, spot: Spot, default: f64) {
    let value = store::get_f64(&spot).unwrap_or(default);
    let def = toml::Value::Float(default);
    let key = format!("{:?}", spot.path);
    f.row(title, desc, Control::Slider { key, value, num: Num::new(0.0, 100.0, 1.0, 0), run: Rc::new(move |v| store::put(&spot, store::number(v, 0), Some(&def))) });
}

/// A switch for one kind in a list (`devices`' `pinned`): on puts it in, off takes it out, and an
/// empty list goes away.
fn pinned(f: &mut Form, title: &str, spot: Spot, kind: &'static str) {
    let on = store::get_strings(&spot).unwrap_or_default().iter().any(|k| k == kind);
    f.row(
        title,
        None,
        Control::Toggle {
            on,
            run: Rc::new(move |v| {
                let mut kinds = store::get_strings(&spot).unwrap_or_default();
                kinds.retain(|k| k != kind);
                if v {
                    kinds.push(kind.to_string());
                }
                store::put(&spot, store::strings(&kinds), Some(&toml::Value::Array(Vec::new())));
            }),
        },
    );
}

/// The user's own names for a pill (`devices`, `audio`), by the full name the popup shows: a
/// field for each (empty takes it away, as the bin does), and one that adds `full name = short
/// name`; `about` says what the pill shows without one, `example` is a line to add.
fn names(f: &mut Form, spot: Spot, about: &'static str, example: &str) {
    f.group(Some(t("Nomi nella barra", "Names in the bar")));
    for full in store::keys(&spot) {
        let at = spot.k(&full);
        let value = store::get_str(&at).unwrap_or_default();
        let (edit, bin) = (at.clone(), at);
        let field = Control::Text {
            value,
            placeholder: String::new(),
            width: 180.0,
            run: Rc::new(move |v: String| {
                let v = v.trim();
                if v.is_empty() { store::remove(&edit) } else { store::set(&edit, Value::from(v)) }
            }),
        };
        let remove = Control::Icon { glyph: "\u{E74D}", run: Rc::new(move || store::remove(&bin)) };
        f.row_controls(None, &full, None, vec![field, remove]);
    }
    f.row(
        t("Aggiungi un nome", "Add a name"),
        Some(about),
        Control::Text {
            value: String::new(),
            placeholder: example.into(),
            width: 260.0,
            run: Rc::new(move |line: String| {
                if let Some((full, short)) = line.split_once('=')
                    && !full.trim().is_empty()
                    && !short.trim().is_empty()
                {
                    store::set(&spot.k(full.trim()), Value::from(short.trim()));
                }
            }),
        },
    );
}

// ---- secrets -----------------------------------------------------------------------------------

thread_local! {
    /// With made-up data the secrets are made up too, and never reach Credential Manager.
    static FAKE: RefCell<HashSet<String>> = RefCell::new(["home-assistant".to_string(), "events-Work".to_string()].into_iter().collect());
}

fn mock() -> bool {
    store::mock_dir().is_some()
}

fn saved(name: &str) -> bool {
    if mock() { FAKE.with(|f| f.borrow().contains(name)) } else { crate::secret::get(name).is_some() }
}

fn save(name: &str, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    if mock() {
        FAKE.with(|f| f.borrow_mut().insert(name.to_string()));
        crate::log!("{name}: made-up data, the secret stays out of Credential Manager");
    } else if let Err(e) = crate::secret::set(name, value) {
        crate::log!("secret {name}: {e}");
    }
}

fn forget(name: &str) {
    if mock() {
        FAKE.with(|f| f.borrow_mut().remove(name));
    } else {
        let _ = crate::secret::delete(name);
    }
}

/// Moves a secret to a new name (a calendar renamed).
fn rename_secret(old: &str, new: &str) {
    if mock() {
        FAKE.with(|f| {
            let mut f = f.borrow_mut();
            if f.remove(old) {
                f.insert(new.to_string());
            }
        });
    } else if let Some(value) = crate::secret::get(old)
        && crate::secret::set(new, &value).is_ok()
    {
        let _ = crate::secret::delete(old);
    }
}

fn secret(f: &mut Form, title: &str, desc: Option<&str>, name: &str) {
    let (a, b) = (name.to_string(), name.to_string());
    f.row(title, desc, Control::Secret { saved: saved(name), run: Rc::new(move |v: String| save(&a, &v)), clear: Rc::new(move || forget(&b)) });
}

// ---- the page ----------------------------------------------------------------------------------

pub fn widget(f: &mut Form, name: &str, ctx: &Ctx) {
    let kind = pages::kind_of(name);
    let (_, icon, desc) = pages::info(&kind);
    let lua = !crate::widgets::KINDS.contains(&kind.as_str());
    f.title(&pages::title_of(name));
    f.intro(desc);
    let table = rb("widgets").k(name);

    f.group(None);
    where_it_is(f, name, icon);
    if name != kind {
        let title = if lua { kind.clone() } else { pages::info(&kind).0.to_string() };
        let desc = format!("{} [widgets.{name}]", t("Un altro dello stesso tipo, con le opzioni sue in", "Another one of a kind, with options of its own in"));
        f.row(t("Tipo", "Kind"), Some(&desc), Control::Label(title));
    }

    let known: &[&str] = match kind.as_str() {
        "clock" => {
            f.group(None);
            text(
                f,
                t("Formato della data", "Date format"),
                Some(t("Un formato di Windows: ddd dd/MM fa «Mar 10/11»; vuoto per la sola ora; un testo fra apici per distinguere due orologi ('Tokyo')", "A Windows date picture: ddd dd/MM makes «Tue 10/11»; empty for the time alone; text between quotes to tell two clocks apart ('Tokyo')")),
                table.k("date"),
                "ddd dd/MM",
                "ddd dd/MM",
                220.0,
            );
            text(f, t("Fuso orario", "Time zone"), Some(t("L'ora di un altro posto: un nome IANA o di Windows; vuoto per quella di questo PC", "Another place's time: an IANA or Windows name; empty for this PC's")), table.k("timezone"), "", "Asia/Tokyo, UTC…", 220.0);
            text(f, t("Clic su un giorno", "Click on a day"), Some(t("La pagina che si apre, con {year} {month} {day}; vuoto per giorni che non si cliccano", "The page that opens, with {year} {month} {day}; empty for days that can't be clicked")), table.k("open"), crate::widgets::clock::OPEN, "https://…", 320.0);
            &["date", "timezone", "open"]
        }
        "audio" => {
            f.group(None);
            toggle(f, t("Ingresso nella pill", "Input in the pill"), Some(t("Una parte per il microfono, col suo nome e il suo volume, accanto a quella dell'uscita", "A part for the microphone, with its name and its volume, beside the output's")), table.k("input"), true);
            toggle(f, t("Microfono e fotocamera in uso", "Microphone and camera in use"), Some(t("Un'icona nella pill quando un'app li usa, e quali nel popup (dal registro di Windows, a eventi)", "An icon in the pill while an app uses them, and which in the popup (from Windows' registry, on events)")), table.k("privacy"), true);
            toggle(f, t("Mixer", "Mixer"), Some(t("Il volume di ogni app nel popup, letto solo mentre è aperto", "Each app's volume in the popup, read only while it's open")), table.k("mixer"), true);
            names(
                f,
                table.k("names"),
                t(
                    "Il nome completo come nel popup (o quello della pill), «=», e quello da mostrare; senza, il dispositivo fra parentesi (Aurora Buds), o cos'è se lì c'è la scheda audio (Speakers)",
                    "The full name as in the popup (or the pill's), «=», and the one to show; without one, the device in brackets (Aurora Buds), or what it is when that's the sound card (Speakers)",
                ),
                "Headphones (Aurora Buds) = Buds",
            );
            &["input", "privacy", "mixer", "names"]
        }
        "display" => {
            f.group(None);
            toggle(f, t("Luminosità", "Brightness"), Some(t("Dei monitor che la accettano via DDC/CI, letta all'apertura del popup su un thread di lavoro", "Of the monitors that take it over DDC/CI, read when the popup opens, on a worker thread")), table.k("brightness"), true);
            &["brightness"]
        }
        "devices" => {
            f.group(None);
            percent(f, t("Nella barra da", "In the bar from"), Some(t("La pill compare quando un dispositivo collegato scende fin qui", "The pill shows when a connected device gets this low")), table.k("low"), 20.0);
            toggle(f, t("Sempre nella barra", "Always in the bar"), Some(t("Tutti i dispositivi collegati, anche carichi", "Every connected device, charged ones too")), table.k("always"), false);
            f.group(Some(t("Sempre nella barra, per tipo", "Always in the bar, by kind")));
            for (kind, title) in [("mouse", t("Mouse", "Mice")), ("keyboard", t("Tastiere", "Keyboards")), ("controller", t("Controller", "Controllers"))] {
                pinned(f, title, table.k("pinned"), kind);
            }
            names(
                f,
                table.k("names"),
                t(
                    "Il nome completo come nel popup, «=», e quello da mostrare nella barra; senza, il modello senza il produttore (Naga, Falcata)",
                    "The full name as in the popup, «=», and the one to show in the bar; without one, the model without its maker (Naga, Falcata)",
                ),
                "Razer Naga V2 Pro = Naga",
            );
            f.group(Some(t("Ricevitori e cavi", "Receivers and cables")));
            toggle(
                f,
                t("Mouse, tastiere e controller non Bluetooth", "Mice, keyboards and controllers off Bluetooth"),
                Some(t(
                    "Razer, Logitech, ROG, Steam Controller, DualSense, DualShock 4 e Xbox, letti su un thread di lavoro ogni 5 minuti (~3,5 ms di CPU); spento: il solo Bluetooth",
                    "Razer, Logitech, ROG, Steam Controller, DualSense, DualShock 4 and Xbox, read on a worker thread every 5 minutes (~3.5 ms of CPU); off: Bluetooth alone",
                )),
                table.k("receivers"),
                true,
            );
            &["low", "always", "pinned", "names", "receivers"]
        }
        "network" => {
            f.group(None);
            text(f, t("Ping verso", "Ping to"), Some(t("Un indirizzo su Internet (il server di un gioco, 1.1.1.1) da misurare a popup aperto, ogni 5 s; vuoto per nessuno: senza, il widget non chiede niente fuori dalla rete locale", "A host on the Internet (a game's server, 1.1.1.1) to measure while the popup is open, every 5 s; empty for none: without it the widget asks nothing outside the local network")), table.k("ping"), "", "1.1.1.1", 220.0);
            &["ping"]
        }
        "workspaces" => {
            f.group(None);
            toggle(f, t("Anche con un solo desktop", "With a single desktop too"), Some(t("Altrimenti la pill si nasconde finché c'è un desktop solo", "Otherwise the pill hides while there's one desktop")), table.k("single"), false);
            toggle(f, t("Icone delle app", "App icons"), Some(t("Un'icona per finestra accanto al numero del suo desktop; spento, solo i numeri (e la barra non segue le finestre)", "An icon per window next to its desktop's number; off, the numbers alone (and the bar doesn't follow the windows)")), table.k("icons"), true);
            number(f, t("Icone per desktop", "Icons per desktop"), Some(t("Le altre finestre come «+N»", "The other windows as “+N”")), table.k("max_icons"), 8.0, Num::new(1.0, 50.0, 1.0, 0));
            &["single", "icons", "max_icons"]
        }
        "media" => {
            f.group(None);
            toggle(f, t("Brano in pausa", "Paused track"), Some(t("La pill resta, in grigio; spento la nasconde", "The pill stays, dimmed; off hides it")), table.k("paused"), true);
            toggle(f, t("Copertina", "Cover"), Some(t("Nel popup; spento non la legge mai", "In the popup; off never reads it")), table.k("cover"), true);
            toggle(f, t("Colori della copertina", "Cover colors"), Some(t("Il popup nei colori della copertina; spento tiene quelli del tema", "The popup in the cover's colors; off keeps the theme's")), table.k("cover_colors"), true);
            toggle(f, t("Copertina nella pill", "Cover in the pill"), Some(t("Al posto della nota; letta a ogni brano (~0,2 MB in più)", "In place of the note; read at every track (~0.2 MB more)")), table.k("pill_cover"), false);
            &["paused", "cover", "cover_colors", "pill_cover"]
        }
        "system" => {
            f.group(None);
            number(f, t("Ogni quanto legge", "Reading interval"), Some(t("Secondi fra una lettura e l'altra (ogni 2 a popup aperto)", "Seconds between readings (every 2 while the popup is open)")), table.k("interval"), 5.0, Num::new(1.0, 600.0, 1.0, 0));
            toggle(f, "GPU", Some(t("Spento: niente GPU nella pill e nel popup", "Off: no GPU in the pill and the popup")), table.k("gpu"), true);
            f.group(Some(t("Soglie", "Thresholds")));
            percent(f, t("Giallo da", "Yellow from"), Some(t("CPU, GPU e memoria", "CPU, GPU and memory")), table.k("warning"), 75.0);
            percent(f, t("Rosso da", "Red from"), None, table.k("critical"), 90.0);
            percent(f, t("Disco giallo da", "Disk yellow from"), Some(t("Nel popup, in percentuale usata", "In the popup, as a percentage used")), table.k("disk_warning"), 90.0);
            percent(f, t("Disco rosso da", "Disk red from"), None, table.k("disk_critical"), 95.0);
            let degrees = Num::new(40.0, 120.0, 1.0, 0);
            number(f, t("Temperatura della GPU gialla da", "GPU temperature yellow from"), Some(t("In °C; senza, 10° sotto quella che il driver dice alta, o 85", "In °C; without it, 10° under the one the driver calls high, or 85")), table.k("temp_warning"), 85.0, degrees);
            number(f, t("Temperatura della GPU rossa da", "GPU temperature red from"), Some(t("In °C; senza, quella che il driver dice alta, o 95", "In °C; without it, the one the driver calls high, or 95")), table.k("temp_critical"), 95.0, degrees);
            &["interval", "gpu", "warning", "critical", "disk_warning", "disk_critical", "temp_warning", "temp_critical"]
        }
        "claude" => {
            f.group(None);
            text(f, t("Cartella di Claude Code", "Claude Code's folder"), Some(t("Un account per cartella (CLAUDE_CONFIG_DIR); vuoto per ~\\.claude", "One account per folder (CLAUDE_CONFIG_DIR); empty for ~\\.claude")), table.k("dir"), "", "~/.claude", 260.0);
            color(f, t("Colore", "Color"), Some(t("La stellina della pill", "The pill's spark")), table.k("color"), crate::widgets::claude::ORANGE);
            text(f, "claude.exe", Some(t("Vuoto: la trova da sola (PATH, ~\\.local\\bin, l'app Claude)", "Empty: found by itself (PATH, ~\\.local\\bin, the Claude app)")), table.k("path"), "", t("trovata da sola", "found by itself"), 260.0);
            f.group(Some(t("Soglie", "Thresholds")));
            percent(f, t("Giallo da", "Yellow from"), None, table.k("warning"), 75.0);
            percent(f, t("Rosso da", "Red from"), None, table.k("critical"), 90.0);
            f.footer(t(
                "Ogni 5 minuti lancia Claude Code per meno di un secondo (~0,4 s di CPU, ~190 MB mentre gira): per questo non è nel layout predefinito.",
                "Every 5 minutes it runs Claude Code for under a second (~0.4 s of CPU, ~190 MB while it runs): that's why it isn't in the default layout.",
            ));
            &["dir", "color", "path", "warning", "critical"]
        }
        "home" => {
            home(f, &table);
            &["server", "dashboard", "groups", "printer", "watch"]
        }
        "events" => {
            events(f, &table);
            &["calendars", "soon", "sooner", "imminent", "interval", "open"]
        }
        "docker" => {
            f.group(None);
            f.row(
                t("Motore", "Engine"),
                Some(t("La named pipe di Docker Desktop; un'altra con la variabile DOCKER_HOST (npipe:////./pipe/<nome>)", "Docker Desktop's named pipe; another with the DOCKER_HOST variable (npipe:////./pipe/<name>)")),
                Control::Label(r"\\.\pipe\docker_engine".into()),
            );
            &[]
        }
        _ if lua => &[],
        _ => {
            f.footer(t("Questo widget non ha opzioni.", "This widget has no options."));
            &[]
        }
    };
    other_keys(f, name, known, lua, ctx);
}

/// Where the widget sits: a side of the bar, or out of it; another of a kind can be deleted.
fn where_it_is(f: &mut Form, name: &str, icon: &str) {
    let sides = pages::sides();
    let side = sides.iter().position(|s| s.iter().any(|n| n == name));
    let current = match side {
        Some(0) => "left",
        Some(1) => "center",
        Some(2) => "right",
        _ => "none",
    };
    let options = vec![
        Opt::new("left", t("A sinistra", "Left")),
        Opt::new("center", t("Al centro", "Center")),
        Opt::new("right", t("A destra", "Right")),
        Opt::new("none", t("Non nella barra", "Not in the bar")),
    ];
    let place = side.and_then(|s| sides[s].iter().position(|n| n == name)).map(|i| i + 1);
    let desc = match place {
        Some(n) => format!("{} {}", t(&format!("{n}° da quel lato;"), &format!("Number {n} on that side;")), t("l'ordine si cambia nel Layout", "the order changes in Layout")),
        None => t("L'ordine si cambia nel Layout", "The order changes in Layout").to_string(),
    };
    let n = name.to_string();
    let mut controls = vec![Control::Choice {
        options,
        current: current.into(),
        run: Rc::new(move |to: String| {
            let mut sides = pages::sides();
            let from = sides.iter().position(|s| s.contains(&n));
            let to = ["left", "center", "right"].iter().position(|s| *s == to);
            if from == to {
                return;
            }
            if let Some(from) = from {
                sides[from].retain(|x| *x != n);
                pages::put_side(from, &sides[from]);
            }
            if let Some(to) = to {
                sides[to].push(n.clone());
                pages::put_side(to, &sides[to]);
            }
        }),
    }];
    let kind = pages::kind_of(name);
    if name != kind {
        let n = name.to_string();
        let key = format!("delete:{name}");
        let asking = form::confirming(&key);
        controls.push(Control::Button {
            label: if asking { t("Fai clic di nuovo per eliminare", "Click again to delete").into() } else { t("Elimina", "Delete").into() },
            accent: asking,
            run: Rc::new(move || {
                if !form::confirmed(&key) {
                    return;
                }
                let mut sides = pages::sides();
                for (i, side) in sides.iter_mut().enumerate() {
                    if side.contains(&n) {
                        side.retain(|x| *x != n);
                        pages::put_side(i, side);
                    }
                }
                store::remove(&rb("widgets").k(&n));
                form::go("layout");
            }),
        });
    }
    f.row_controls(Some(icon), t("Nella barra", "In the bar"), Some(&desc), controls);
}

// ---- Home Assistant ----------------------------------------------------------------------------

/// The entities of the Home Assistant server (id and name), for the menus that add a tile: asked
/// on a worker thread when the page asks, and kept while the window is open.
type Entities = Option<Result<Vec<(String, String)>, String>>;
static ENTITIES: std::sync::Mutex<Entities> = std::sync::Mutex::new(None);
static LOADING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The domains a tile can show.
const TILES: [&str; 9] = ["light", "switch", "fan", "input_boolean", "cover", "climate", "vacuum", "script", "scene"];

fn entities() -> Entities {
    ENTITIES.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Asks the server for its entities (`GET /api/states`, with the token), on a worker thread; the
/// page is made again when they come. With made-up data, those of `home.json`.
fn load_entities(server: String) {
    if LOADING.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(move || {
        let result = fetch_entities(&server);
        *ENTITIES.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
        LOADING.store(false, std::sync::atomic::Ordering::Release);
        super::refresh();
    });
}

fn fetch_entities(server: &str) -> Result<Vec<(String, String)>, String> {
    use crate::json::Json;
    let list = |states: &Json| -> Vec<(String, String)> {
        let mut list: Vec<(String, String)> = states
            .array()
            .iter()
            .filter_map(|e| Some((e["entity_id"].str()?.to_string(), e["attributes"]["friendly_name"].str().unwrap_or_default().to_string())))
            .collect();
        list.sort();
        list
    };
    if mock() {
        let m = crate::mock::load("home").ok_or("home.json")?;
        let states = m["states"].object().into_iter().flatten().map(|(id, e)| Json::of([("entity_id", id.as_str().into()), ("attributes", e["attributes"].clone())]));
        return Ok(list(&Json::Array(states.collect())));
    }
    if server.trim().is_empty() {
        return Err(t("Manca l'indirizzo del server", "The server's address is missing").into());
    }
    let token = crate::secret::get("home-assistant").ok_or(t("Manca il token di accesso", "The access token is missing"))?;
    let url = format!("{}/api/states", server.trim().trim_end_matches('/'));
    let auth = format!("Bearer {token}");
    let r = crate::net::request("GET", &url, &[("Authorization", auth.as_str())], None, 10_000)?;
    if r.status != 200 {
        return Err(format!("HTTP {}", r.status));
    }
    let json = Json::parse(&String::from_utf8_lossy(&r.body)).ok_or("JSON")?;
    Ok(list(&json))
}

/// A tile's id, and its own name and icon if it has them.
fn tile_of(v: &toml::Value) -> (String, String, String) {
    match v {
        toml::Value::Table(t) => {
            let s = |k: &str| t.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
            (s("id"), s("name"), s("icon"))
        }
        other => (other.as_str().map(str::to_string).unwrap_or_else(|| store::value_of(other).to_string()), String::new(), String::new()),
    }
}

/// Writes a tile: its id alone, or a table with its name and icon.
fn put_tile(spot: &Spot, id: &str, name: &str, icon: &str) {
    if name.is_empty() && icon.is_empty() {
        store::set(spot, Value::from(id));
        return;
    }
    let mut t = toml_edit::InlineTable::new();
    t.insert("id", id.into());
    if !name.is_empty() {
        t.insert("name", name.into());
    }
    if !icon.is_empty() {
        t.insert("icon", icon.into());
    }
    t.fmt();
    store::set(spot, Value::InlineTable(t));
}

/// ↑ and ↓, for what can move up or down in a list of `count`: an empty room for the one it can't,
/// so the rows' controls line up. Nothing for a list of one.
fn moves(spot: &Spot, i: usize, count: usize) -> Vec<Control> {
    if count < 2 {
        return Vec::new();
    }
    let (up, down) = (spot.clone(), spot.clone());
    vec![
        if i > 0 { Control::Icon { glyph: "\u{E70E}", run: Rc::new(move || store::swap(&up, i, i - 1)) } } else { Control::Gap },
        if i + 1 < count { Control::Icon { glyph: "\u{E70D}", run: Rc::new(move || store::swap(&down, i, i + 1)) } } else { Control::Gap },
    ]
}

/// A menu that adds an id to a list: «From the server…», then the server's entities not in it yet
/// (only those a tile can show, with `tiles_only`).
fn entity_menu(known: &[(String, String)], ids: &[String], tiles_only: bool) -> Vec<Opt> {
    let mut options = vec![Opt { value: String::new(), label: t("Dal server…", "From the server…").into(), swatch: None, family: None }];
    options.extend(
        known
            .iter()
            .filter(|(id, _)| !ids.contains(id) && (!tiles_only || TILES.contains(&id.split('.').next().unwrap_or(""))))
            .map(|(id, name)| Opt { value: id.clone(), label: if name.is_empty() { id.clone() } else { format!("{name} ({id})") }, swatch: None, family: None }),
    );
    options
}

fn home(f: &mut Form, table: &Spot) {
    f.group(Some(t("Server", "Server")));
    text(f, t("Indirizzo", "Address"), Some(t("Il server di Home Assistant", "The Home Assistant server")), table.k("server"), "", "http://homeassistant.local:8123", 300.0);
    secret(
        f,
        t("Token di accesso", "Access token"),
        Some(t("Un token di lunga durata (il tuo profilo in Home Assistant): va nel Gestore credenziali di Windows, mai nei file", "A long-lived token (your profile in Home Assistant): it goes to Windows' Credential Manager, never to the files")),
        "home-assistant",
    );
    text(f, t("Plancia", "Dashboard"), Some(t("Cosa apre «Apri Home Assistant»: un percorso sul server o un indirizzo", "What «Open Home Assistant» opens: a path on the server or an address")), table.k("dashboard"), "", "/", 220.0);
    // The server's entities, for the menus that add a tile
    let loaded = entities();
    let state = match &loaded {
        _ if LOADING.load(std::sync::atomic::Ordering::Acquire) => t("Caricamento…", "Loading…").to_string(),
        Some(Ok(list)) => format!("{} {}", list.len(), t("entità, nei menu «Dal server…»", "entities, in the «From the server…» menus")),
        Some(Err(e)) => format!("{}: {e}", t("Non riuscito", "Didn't work")),
        None => t("Chiedile al server, per sceglierle da un elenco invece di scriverne l'id", "Ask the server for them, to pick them from a list instead of typing their ids").to_string(),
    };
    let server = store::get_str(&table.k("server")).unwrap_or_default();
    f.row(
        t("Entità del server", "The server's entities"),
        Some(&state),
        Control::Button {
            label: if loaded.is_some() { t("Ricarica", "Load again").into() } else { t("Carica", "Load").into() },
            accent: false,
            run: Rc::new(move || load_entities(server.clone())),
        },
    );
    let known: Vec<(String, String)> = match &loaded {
        Some(Ok(list)) => list.clone(),
        _ => Vec::new(),
    };

    let groups = table.k("groups");
    let count = store::get(&groups).and_then(|v| v.as_array().map(Vec::len)).unwrap_or(0);
    for i in 0..count {
        let g = groups.i(i);
        let title = store::get_str(&g.k("title")).unwrap_or_default();
        f.group(Some(&if title.is_empty() { format!("{} {}", t("Gruppo", "Group"), i + 1) } else { title.clone() }));
        let (gs, key) = (g.clone(), format!("group:{i}"));
        let asking = form::confirming(&key);
        let title_spot = g.k("title");
        let mut controls = vec![Control::Text { value: title, placeholder: t("Luci", "Lights").into(), width: 200.0, run: Rc::new(move |v: String| store::set(&title_spot, Value::from(v.trim()))) }];
        controls.extend(moves(&groups, i, count));
        controls.push(Control::Button {
            label: if asking { t("Fai clic di nuovo", "Click again").into() } else { t("Togli", "Remove").into() },
            accent: asking,
            run: Rc::new(move || {
                if form::confirmed(&key) {
                    store::remove(&gs);
                }
            }),
        });
        f.row_controls(None, t("Titolo", "Title"), None, controls);
        // The tiles, one a row: the id, its own name and icon, ↑ ↓ and the bin
        let entities = g.k("entities");
        let tiles: Vec<(String, String, String)> = match store::get(&entities) {
            Some(toml::Value::Array(a)) => a.iter().map(tile_of).collect(),
            _ => Vec::new(),
        };
        for (j, (id, name, icon)) in tiles.iter().enumerate() {
            let spot = entities.i(j);
            let (s1, s2, s3) = (spot.clone(), spot.clone(), spot.clone());
            let (id1, icon1, id2, name2) = (id.clone(), icon.clone(), id.clone(), name.clone());
            let mut controls = vec![
                Control::Text { value: name.clone(), placeholder: t("nome suo", "its own name").into(), width: 130.0, run: Rc::new(move |v: String| put_tile(&s1, &id1, v.trim(), &icon1)) },
                Control::Text { value: icon.clone(), placeholder: t("icona", "icon").into(), width: 64.0, run: Rc::new(move |v: String| put_tile(&s2, &id2, &name2, v.trim())) },
            ];
            controls.extend(moves(&entities, j, tiles.len()));
            controls.push(Control::Icon { glyph: "\u{E74D}", run: Rc::new(move || store::remove(&s3)) });
            let known_name = known.iter().find(|(k, _)| k == id).map(|(_, n)| n.as_str()).filter(|n| !n.is_empty());
            f.row_controls(None, id, known_name, controls);
        }
        // A tile more: from the server's list, or by its id
        let ids: Vec<String> = tiles.iter().map(|(id, _, _)| id.clone()).collect();
        let (add, pick) = (entities.clone(), entities.clone());
        let mut controls = Vec::new();
        if !known.is_empty() {
            controls.push(Control::Choice {
                options: entity_menu(&known, &ids, true),
                current: String::new(),
                run: Rc::new(move |id: String| {
                    if !id.is_empty() {
                        store::push(&pick, Value::from(id));
                    }
                }),
            });
        }
        controls.push(Control::Text {
            value: String::new(),
            placeholder: t("id, es. light.salotto", "id, e.g. light.living_room").into(),
            width: 200.0,
            run: Rc::new(move |id: String| {
                let id = id.trim().to_string();
                if id.contains('.') {
                    store::push(&add, Value::from(id));
                }
            }),
        });
        f.row_controls(None, t("Aggiungi una tessera", "Add a tile"), Some(t("Luci, prese, ventole, tapparelle, clima, robot; script e scene come chip", "Lights, plugs, fans, blinds, climate, vacuums; scripts and scenes as chips")), controls);
    }
    f.group(None);
    let g = groups.clone();
    f.row(
        t("Aggiungi un gruppo", "Add a group"),
        Some(t("Un titolo e le sue tessere, nel popup", "A title and its tiles, in the popup")),
        Control::Text {
            value: String::new(),
            placeholder: t("titolo, es. Luci", "title, e.g. Lights").into(),
            width: 220.0,
            run: Rc::new(move |title: String| {
                let title = title.trim();
                if title.is_empty() {
                    return;
                }
                let mut new = toml_edit::Table::new();
                new.insert("title", toml_edit::value(title));
                new.insert("entities", toml_edit::value(toml_edit::Array::new()));
                store::push_table(&g, new);
            }),
        },
    );

    f.group(Some(t("Stampante 3D", "3D printer")));
    let p = table.k("printer");
    text(f, t("Presa", "Plug"), Some(t("Una tessera larga, e la percentuale nella pill mentre stampa", "A wide tile, and the percentage in the pill while it prints")), p.k("plug"), "", "switch.stampante", 260.0);
    text(f, t("Spegnimento", "Turning off"), Some(t("Lo script che la spegne, al secondo clic", "The script that turns it off, at a second click")), p.k("off"), "", "script.spegni_stampante", 260.0);
    text(f, t("Stato", "Status"), None, p.k("status"), "", "sensor.stampante_stato", 260.0);
    text(f, t("Avanzamento", "Progress"), Some("%"), p.k("progress"), "", "sensor.stampante_avanzamento", 260.0);
    text(f, t("Tempo rimasto", "Time left"), Some(t("In ore", "In hours")), p.k("remaining"), "", "sensor.stampante_rimasto", 260.0);

    // The entities the Lua widgets read with rb.home, besides the tiles'
    f.group(Some(t("Per i widget Lua", "For the Lua widgets")));
    let watch = table.k("watch");
    let ids = store::get_strings(&watch).unwrap_or_default();
    let (add, remove, pick) = (watch.clone(), watch.clone(), watch.clone());
    let list = Rc::new(ids.clone());
    f.tokens(
        t("Entità seguite", "Entities followed"),
        Some(t("Quelle che leggono con rb.home e che non sono già tessere (sensor.…)", "Those they read with rb.home that aren't tiles already (sensor.…)")),
        &ids,
        t("Aggiungi, es. sensor.salotto", "Add, e.g. sensor.living_room"),
        Rc::new(move |id: String| {
            let id = id.trim().to_string();
            if id.contains('.') && !store::get_strings(&add).unwrap_or_default().contains(&id) {
                store::push(&add, Value::from(id));
            }
        }),
        Rc::new(move |i: usize| {
            let mut ids = (*list).clone();
            if i < ids.len() {
                ids.remove(i);
                store::put(&remove, store::strings(&ids), Some(&toml::Value::Array(Vec::new())));
            }
        }),
    );
    if !known.is_empty() {
        f.row(
            t("Segui un'entità", "Follow an entity"),
            None,
            Control::Choice {
                options: entity_menu(&known, &ids, false),
                current: String::new(),
                run: Rc::new(move |id: String| {
                    if !id.is_empty() {
                        store::push(&pick, Value::from(id));
                    }
                }),
            },
        );
    }
}

fn events(f: &mut Form, table: &Spot) {
    let calendars = table.k("calendars");
    let list: Vec<toml::Value> = store::get(&calendars).and_then(|v| v.as_array().cloned()).unwrap_or_default();
    f.group(Some(t("Calendari", "Calendars")));
    for (i, cal) in list.iter().enumerate() {
        let c = calendars.i(i);
        let name = cal.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let has_url = cal.get("url").is_some();
        let (old, name_spot) = (name.clone(), c.k("name"));
        let (key, cs) = (format!("calendar:{i}"), c.clone());
        let asking = form::confirming(&key);
        let color_value = cal.get("color").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let color_spot = c.k("color");
        let secret_name = format!("events-{name}");
        let (save_name, forget_name) = (secret_name.clone(), secret_name.clone());
        let mut controls = vec![
            Control::Text {
                value: name.clone(),
                placeholder: t("nome", "name").into(),
                width: 150.0,
                run: Rc::new(move |new: String| {
                    let new = new.trim().to_string();
                    if new.is_empty() || new == old {
                        return;
                    }
                    store::set(&name_spot, Value::from(new.as_str()));
                    // Its address goes along with the name
                    rename_secret(&format!("events-{old}"), &format!("events-{new}"));
                }),
            },
            Control::Color { value: color_value, default: "sky".into(), run: Rc::new(move |v: String| if v.is_empty() { store::remove(&color_spot) } else { store::set(&color_spot, Value::from(v)) }) },
        ];
        if !has_url {
            controls.push(Control::Secret { saved: saved(&secret_name), run: Rc::new(move |v: String| save(&save_name, &v)), clear: Rc::new(move || forget(&forget_name)) });
        }
        controls.push(Control::Icon {
            glyph: if asking { "\u{E73E}" } else { "\u{E74D}" },
            run: Rc::new(move || {
                if form::confirmed(&key) {
                    store::remove(&cs);
                }
            }),
        });
        let desc = if has_url {
            t("Con l'indirizzo nella config (url)", "With its address in the config (url)").to_string()
        } else if asking {
            t("Fai clic di nuovo sul segno per toglierlo", "Click the mark again to remove it").to_string()
        } else {
            t("L'indirizzo segreto in formato iCal, nel Gestore credenziali", "The secret address in iCal format, in Credential Manager").to_string()
        };
        f.row_stacked(Some("\u{E787}"), if name.is_empty() { "?" } else { &name }, Some(&desc), controls);
    }
    let add = calendars.clone();
    f.row(
        t("Aggiungi un calendario", "Add a calendar"),
        Some(t("Poi il suo indirizzo segreto (Google Calendar: Impostazioni del calendario > Indirizzo segreto in formato iCal)", "Then its secret address (Google Calendar: the calendar's settings > Secret address in iCal format)")),
        Control::Text {
            value: String::new(),
            placeholder: t("nome, es. Lavoro", "name, e.g. Work").into(),
            width: 220.0,
            run: Rc::new(move |name: String| {
                let name = name.trim();
                if name.is_empty() {
                    return;
                }
                let mut cal = toml_edit::InlineTable::new();
                cal.insert("name", Value::from(name));
                cal.insert("color", Value::from("sky"));
                cal.fmt();
                store::push(&add, Value::InlineTable(cal));
            }),
        },
    );

    f.group(Some(t("Colori dell'ora", "Colors of the time")));
    number(f, t("Giallo da", "Yellow from"), Some(t("Minuti all'inizio", "Minutes to the start")), table.k("soon"), 30.0, Num::new(0.0, 240.0, 1.0, 0));
    number(f, t("Pesca da", "Peach from"), None, table.k("sooner"), 15.0, Num::new(0.0, 240.0, 1.0, 0));
    number(f, t("Rosso da", "Red from"), None, table.k("imminent"), 5.0, Num::new(0.0, 240.0, 1.0, 0));
    f.group(None);
    number(f, t("Ogni quanto scarica", "Download interval"), Some(t("Minuti fra un download e l'altro (anche all'avvio, al risveglio e a mezzanotte)", "Minutes between downloads (also at start, on wake and at midnight)")), table.k("interval"), 15.0, Num::new(1.0, 1440.0, 1.0, 0));
    text(f, t("Clic su un evento", "Click on an event"), Some(t("La pagina del giorno, con {year} {month} {day}", "The day's page, with {year} {month} {day}")), table.k("open"), crate::widgets::events::GOOGLE_DAY, "https://…", 320.0);
}

/// The keys of `[widgets.<name>]` a page doesn't know (every key, for a Lua widget): a control for
/// each by the type of its value, a bin to remove it, and «Add an option».
fn other_keys(f: &mut Form, name: &str, known: &[&str], lua: bool, ctx: &Ctx) {
    let table = rb("widgets").k(name);
    let keys: Vec<String> = store::keys(&table).into_iter().filter(|k| k != "type" && !known.contains(&k.as_str())).collect();
    if lua {
        let kind = pages::kind_of(name);
        let found = ctx.lua.contains(&kind);
        f.group(Some(t("Opzioni", "Options")));
        if !found {
            f.banner(&format!("{} {kind:?}", t("Nessun widget Lua si chiama", "No Lua widget is called")), true);
        }
        f.footer(t("Il widget le legge in self.options; il tipo di ognuna è quello del valore.", "The widget reads them in self.options; each one's type is its value's."));
    } else if keys.is_empty() {
        return;
    } else {
        f.group(Some(t("Altre chiavi", "Other keys")));
    }
    for key in &keys {
        let spot = table.k(key);
        let Some(value) = store::get(&spot) else { continue };
        let remove = spot.clone();
        let bin = Control::Icon { glyph: "\u{E74D}", run: Rc::new(move || store::remove(&remove)) };
        let control = match &value {
            toml::Value::Boolean(on) => {
                let s = spot.clone();
                Control::Toggle { on: *on, run: Rc::new(move |v| store::set(&s, Value::from(v))) }
            }
            toml::Value::Integer(i) => {
                let s = spot.clone();
                Control::Number { value: *i as f64, num: Num::new(-1e9, 1e9, 1.0, 0), run: Rc::new(move |v| store::set(&s, store::number(v, 0))) }
            }
            toml::Value::Float(x) => {
                let s = spot.clone();
                Control::Number { value: *x, num: Num::new(-1e9, 1e9, 0.5, 2), run: Rc::new(move |v| store::set(&s, store::number(v, 2))) }
            }
            toml::Value::String(text) if key.contains("color") => {
                let s = spot.clone();
                Control::Color { value: text.clone(), default: text.clone(), run: Rc::new(move |v: String| if !v.is_empty() { store::set(&s, Value::from(v)) }) }
            }
            toml::Value::String(text) => {
                let s = spot.clone();
                Control::Text { value: text.clone(), placeholder: String::new(), width: 260.0, run: Rc::new(move |v: String| store::set(&s, Value::from(v))) }
            }
            other => Control::Label(short(&store::value_of(other).to_string())),
        };
        f.row_controls(None, key, None, vec![control, bin]);
    }
    if lua {
        let t2 = table.clone();
        f.row(
            t("Aggiungi un'opzione", "Add an option"),
            Some(t("chiave = valore, come in TOML: «every = 5», «city = \"Roma\"», «on = true»; un testo senza virgolette va bene", "key = value, as in TOML: «every = 5», «city = \"Rome\"», «on = true»; text without quotes is fine")),
            Control::Text {
                value: String::new(),
                placeholder: "city = Roma".into(),
                width: 240.0,
                run: Rc::new(move |line: String| {
                    if let Some((key, value)) = option_line(&line) {
                        store::set(&t2.k(&key), value);
                    }
                }),
            },
        );
    }
}

/// `key = value` as typed: the value as TOML, or as text when it isn't TOML.
fn option_line(line: &str) -> Option<(String, Value)> {
    let (key, value) = line.split_once('=')?;
    let key = key.trim().trim_matches('"');
    if key.is_empty() || key == "type" || !key.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
        return None;
    }
    let value = value.trim();
    let parsed = value.parse::<Value>().ok().filter(|v| !matches!(v, Value::InlineTable(_)));
    Some((key.to_string(), parsed.unwrap_or_else(|| Value::from(value))))
}

fn short(s: &str) -> String {
    if s.chars().count() > 48 { format!("{}…", s.chars().take(47).collect::<String>()) } else { s.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_as_typed() {
        let (k, v) = option_line("every = 5").unwrap();
        assert_eq!((k.as_str(), v.as_integer()), ("every", Some(5)));
        let (_, v) = option_line("city = Roma").unwrap();
        assert_eq!(v.as_str(), Some("Roma"));
        let (_, v) = option_line("city = \"Roma\"").unwrap();
        assert_eq!(v.as_str(), Some("Roma"));
        let (_, v) = option_line("on=true").unwrap();
        assert_eq!(v.as_bool(), Some(true));
        assert!(option_line("type = x").is_none());
        assert!(option_line("no value").is_none());
        assert!(option_line("a b = 1").is_none());
    }
}
