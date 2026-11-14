//! The pages of the settings window, and its sidebar: the bar and the games, the theme, the
//! layout (three columns of chips to drag), one page for each widget, and About. Each is built
//! anew from the config (`store`) whenever something changes.

use std::cell::OnceCell;
use std::rc::Rc;

use super::form::{self, Columns, Control, Do, Form, Look, Num, Opt};
use super::store::{self, Spot, Value, rb};
use crate::config::Config;
use crate::i18n::t;
use crate::ui::{Align, Rect, Scene};

const SIDES: [&str; 3] = ["left", "center", "right"];
const CHIP_H: f32 = 36.0;

/// What a page needs beyond the config.
pub struct Ctx {
    /// The widgets written in Lua in `widgets\`.
    pub lua: Rc<Vec<String>>,
    /// What the last change couldn't do.
    pub problem: Option<String>,
}

/// The pages by name, for `rustybar settings <page>`: an unknown one is the bar's.
pub fn known(page: &str) -> String {
    match page {
        "bar" | "theme" | "layout" | "about" => page.to_string(),
        // A widget's page by its name in [layout] (`rustybar settings clock`)
        _ => {
            let name = page.strip_prefix("w:").unwrap_or(page);
            if sides().iter().flatten().any(|n| n == name) { format!("w:{name}") } else { "bar".into() }
        }
    }
}

/// The names of the Lua widgets in the config dir's `widgets\`, loading them once.
pub fn lua_names() -> Vec<String> {
    let theme = crate::theme::Theme::from_config(&Default::default());
    crate::lua::check(&crate::config::dir(), &theme).map(|(names, _)| names).unwrap_or_default()
}

/// The name, icon and description of a kind of widget.
pub fn info(kind: &str) -> (&'static str, &'static str, &'static str) {
    match kind {
        "power" => (t("Menu di Windows", "Windows menu"), "\u{E7E8}", t("Il logo di Windows, coi comandi del menu Start e questa finestra.", "The Windows logo, with the Start menu's commands and this window.")),
        "workspaces" => (t("Desktop", "Desktops"), "\u{E7C4}", t("I desktop virtuali di Windows con le icone delle loro finestre: un clic sul numero ci passa, sull'icona porta davanti la finestra.", "Windows' virtual desktops with the icons of their windows: a click on a number goes there, on an icon brings the window in front.")),
        "front_app" => (t("App attiva", "Front app"), "\u{E71D}", t("Il nome dell'app in primo piano.", "The name of the app in front.")),
        "media" => (t("Musica", "Media"), "\u{EC4F}", t("Cosa suona, con un player nel popup.", "What's playing, with a player in the popup.")),
        "network" => (t("Rete", "Network"), "\u{E839}", t("La connessione, con traffico, dettagli e Wi-Fi nel popup.", "The connection, with traffic, details and Wi-Fi in the popup.")),
        "audio" => ("Audio", "\u{E995}", t("L'uscita e l'ingresso, coi volumi e i dispositivi.", "The output and the input, with their volumes and devices.")),
        "system" => (t("Sistema", "System"), "\u{E9D9}", t("CPU, GPU e memoria; nel popup anche dischi e app.", "CPU, GPU and memory; disks and apps in the popup too.")),
        "clock" => (t("Orologio", "Clock"), "\u{E823}", t("Data e ora, con un calendario nel popup.", "Date and time, with a calendar in the popup.")),
        "claude" => ("Claude", "\u{E734}", t("Quanto dei limiti del piano ha usato un account Claude.", "How much of the plan's limits a Claude account has used.")),
        "home" => ("Home Assistant", "\u{E80F}", t("I dispositivi di casa, in diretta, e una plancia per comandarli.", "The devices of the house, live, and a dashboard to control them.")),
        "events" => (t("Eventi", "Events"), "\u{E787}", t("Il prossimo evento di oggi dai calendari iCal.", "Today's next event from iCal calendars.")),
        "docker" => ("Docker", "\u{E7B8}", t("I container, coi pulsanti per gestirli, e lo spazio da pulire.", "The containers, with buttons to manage them, and the space to clean up.")),
        "display" => (t("Schermi", "Displays"), "\u{E7F4}", t("La frequenza dello schermo; nel popup frequenze, HDR e luminosità.", "The screen's refresh rate; rates, HDR and brightness in the popup.")),
        "devices" => (t("Batterie", "Batteries"), "\u{E702}", t("Le batterie di mouse, tastiere e controller, anche dietro un ricevitore o un cavo, e dei dispositivi Bluetooth: nella barra i tipi scelti, gli altri quando si scaricano.", "The batteries of mice, keyboards and controllers, behind a receiver or a cable too, and of the Bluetooth devices: the kinds chosen in the bar, the others when they run low.")),
        "tray" => (t("Icone nascoste", "Hidden icons"), "\u{E70E}", t("La freccia delle icone nascoste della barra delle applicazioni: un clic apre la tendina di Windows, dove le icone funzionano come sempre.", "The taskbar's hidden icons chevron: a click opens Windows' flyout, where the icons work as always.")),
        _ => ("", "\u{E943}", t("Un widget scritto in Lua, in widgets\\ accanto alla config.", "A widget written in Lua, in widgets\\ next to the config.")),
    }
}

/// The kind of the widget called `name`: its `type`, or the name itself.
pub fn kind_of(name: &str) -> String {
    store::get_str(&rb("widgets").k(name).k("type")).unwrap_or_else(|| name.to_string())
}

/// A widget's title: its kind's name, with its own name when it's another of a kind.
pub fn title_of(name: &str) -> String {
    let kind = kind_of(name);
    let (title, _, _) = info(&kind);
    if title.is_empty() {
        name.to_string()
    } else if name != kind {
        format!("{title} ({name})")
    } else {
        title.to_string()
    }
}

/// A widget's name in the small bar: its kind's alone.
fn short_of(name: &str) -> String {
    let (title, _, _) = info(&kind_of(name));
    if title.is_empty() { name.to_string() } else { title.to_string() }
}

/// The three sides of `[layout]`, as the bar reads them.
pub fn sides() -> [Vec<String>; 3] {
    let d = Config::default().layout;
    let get = |key: &str, d: Vec<String>| store::get_strings(&rb(&format!("layout.{key}"))).unwrap_or(d);
    [get("left", d.left), get("center", d.center), get("right", d.right)]
}

/// Writes a side of `[layout]`; the default one goes from the file.
pub fn put_side(side: usize, names: &[String]) {
    let d = Config::default().layout;
    let default = [d.left, d.center, d.right][side].clone();
    let default = toml::Value::Array(default.into_iter().map(toml::Value::String).collect());
    store::put(&rb(&format!("layout.{}", SIDES[side])), store::strings(names), Some(&default));
}

// ---- the sidebar -------------------------------------------------------------------------------

/// The sidebar: the app's name, then the pages, the current one marked as in Windows' Settings.
/// The sidebar: the app's name, the pages of the bar, then one page for each widget of the
/// layout, About last; the current one marked as in Windows' Settings. `scroll` moves the list
/// up when the window is too short for it. Also returns how tall the list is.
pub fn sidebar(look: &Look, current: &str, width: f32, height: f32, scale: f32, scroll: f32) -> (Scene, f32) {
    let mut scene = Scene::new(width, height);
    let mut entries: Vec<(String, String, String)> = vec![
        ("bar".into(), "\u{E7F4}".into(), t("Barra", "Bar").into()),
        ("theme".into(), "\u{E790}".into(), t("Tema", "Theme").into()),
        ("layout".into(), "\u{E8A9}".into(), "Layout".into()),
    ];
    let widgets: Vec<String> = sides().concat();
    let first_widget = entries.len();
    for name in &widgets {
        let (_, icon, _) = info(&kind_of(name));
        entries.push((format!("w:{name}"), icon.into(), title_of(name)));
    }
    let about = entries.len();
    entries.push(("about".into(), "\u{E946}".into(), t("Informazioni", "About").into()));
    let mut y = 72.0 - scroll;
    for (n, (id, icon, label)) in entries.into_iter().enumerate() {
        if n == first_widget && !widgets.is_empty() {
            scene.text(Rect::new(24.0, y + 6.0, width - 40.0, 24.0), "Widget", form::caption(), look.sub, Align::Start);
            y += 32.0;
        }
        if n == about {
            y += 8.0;
        }
        let row = Rect::new(8.0, y, width - 16.0, 36.0);
        if id == current {
            scene.fill(row, 4.0, look.selected);
            scene.fill(Rect::new(8.0, y + 10.0, 3.0, 16.0), 1.5, look.accent);
        }
        scene.text(Rect::new(24.0, y, 24.0, 36.0), icon, form::icons(16.0), look.text, Align::Start);
        scene.text(Rect::new(56.0, y, width - 72.0, 36.0), label, form::body(), look.text, Align::Start);
        // Rows under the header can't be clicked through it
        if y >= 60.0 {
            scene.zone(row, 4.0, format!("go:{id}"));
        }
        y += 38.0;
    }
    let tall = y + scroll + 16.0;
    // The name over the list, which scrolls under it: the logo, four squares in the accent
    scene.fill(Rect::new(0.0, 0.0, width, 64.0), 0.0, look.bg);
    for r in crate::ui::logo(20.0, 20.0, 20.0, 28.0, 16.0, scale) {
        scene.fill(r, 0.0, look.accent);
    }
    scene.text(Rect::new(52.0, 18.0, width - 60.0, 32.0), "RustyBar", crate::ui::Font::new(18.0, crate::ui::Weight::SemiBold), look.text, Align::Start);
    (scene, tall)
}

// ---- the pages ---------------------------------------------------------------------------------

pub fn page(id: &str, f: &mut Form, ctx: &Ctx) {
    if let Some(problem) = &ctx.problem {
        f.banner(problem, true);
    }
    for e in store::errors() {
        f.banner(&format!("{}{e}", t("Il file non si legge e non viene scritto finché non è a posto: ", "The file doesn't read, and isn't written until it's fixed: ")), true);
    }
    match id {
        "theme" => theme(f),
        "layout" => layout(f, ctx),
        "about" => about(f),
        _ => match id.strip_prefix("w:") {
            Some(name) => super::schema::widget(f, name, ctx),
            None => bar(f),
        },
    }
}

pub fn number(f: &mut Form, title: &str, desc: Option<&str>, spot: Spot, default: f64, num: Num) {
    let value = store::get_f64(&spot).unwrap_or(default);
    let def = toml::Value::Float(default);
    f.row(title, desc, Control::Number { value, num, run: Rc::new(move |v| store::put(&spot, store::number(v, num.decimals), Some(&def))) });
}

pub fn toggle(f: &mut Form, title: &str, desc: Option<&str>, spot: Spot, default: bool) {
    let on = store::get_bool(&spot).unwrap_or(default);
    f.row(title, desc, Control::Toggle { on, run: Rc::new(move |v| store::put(&spot, Value::from(v), Some(&toml::Value::Boolean(default)))) });
}

pub fn choice(f: &mut Form, title: &str, desc: Option<&str>, spot: Spot, options: Vec<Opt>, default: &str) {
    let current = store::get_str(&spot).unwrap_or_else(|| default.to_string());
    let def = toml::Value::String(default.to_string());
    f.row(title, desc, Control::Choice { options, current, run: Rc::new(move |v| store::put(&spot, Value::from(v), Some(&def))) });
}

pub fn color(f: &mut Form, title: &str, desc: Option<&str>, spot: Spot, default: &str) {
    let value = store::get_str(&spot).unwrap_or_default();
    let def = toml::Value::String(default.to_string());
    let shown = if value == default { String::new() } else { value };
    f.row(
        title,
        desc,
        Control::Color {
            value: shown,
            default: default.to_string(),
            run: Rc::new(move |v: String| {
                if v.is_empty() {
                    store::remove(&spot);
                } else {
                    store::put(&spot, Value::from(v), Some(&def));
                }
            }),
        },
    );
}

fn bar(f: &mut Form) {
    let d = Config::default().bar;
    f.title(t("Barra", "Bar"));
    f.intro(t(
        "La barra in cima a ogni schermo. Ogni modifica si scrive subito nella config e la barra la prende da sola, rifacendo solo quello che è cambiato.",
        "The bar at the top of every screen. Every change is written to the config at once, and the bar takes it by itself, making again only what changed.",
    ));
    f.group(None);
    number(f, t("Altezza", "Height"), Some(t("La barra, in DIP; le pill stanno al centro", "The bar, in DIPs; the pills are centered in it")), rb("bar.height"), d.height as f64, Num::new(16.0, 120.0, 1.0, 0));
    number(f, t("Margine", "Margin"), Some(t("Dai bordi dello schermo alla prima e all'ultima pill", "From the screen edges to the first and last pill")), rb("bar.margin"), d.margin as f64, Num::new(0.0, 2000.0, 1.0, 0));
    number(f, t("Distanza dei popup", "Popup gap"), Some(t("Dal fondo della barra all'inizio di un popup", "From the bottom of the bar to the top of a popup")), rb("bar.popup_gap"), d.popup_gap as f64, Num::new(0.0, 60.0, 1.0, 0));
    toggle(
        f,
        t("Riserva lo spazio", "Reserve the space"),
        Some(t("Come la barra delle applicazioni: le finestre ingrandite si fermano sotto la barra", "As the taskbar does: maximized windows stop under the bar")),
        rb("bar.reserve"),
        d.reserve,
    );
    let opt = |value: &str, label: &str| Opt { value: value.into(), label: label.into(), swatch: None, family: None };
    choice(
        f,
        t("Schermi", "Screens"),
        Some(t("Una barra su ogni schermo, o sul principale soltanto", "A bar on every screen, or on the main one alone")),
        rb("bar.screens"),
        vec![opt("all", t("Tutti", "All")), opt("primary", t("Solo il principale", "The main one alone"))],
        &d.screens,
    );
    let spot = rb("bar.always_compact");
    let names = store::get_strings(&spot).unwrap_or_default();
    let (add, remove) = (spot.clone(), spot.clone());
    let list = Rc::new(names.clone());
    f.tokens(
        t("Sempre compatti", "Always compact"),
        Some(t("Widget nella forma compatta su ogni schermo, anche quando c'è posto (system: la sola RAM)", "Widgets in their compact form on every screen, even with room to spare (system: the memory alone)")),
        &names,
        t("Aggiungi, es. system", "Add, e.g. system"),
        Rc::new(move |name: String| {
            let name = name.trim().to_string();
            if !name.is_empty() && !store::get_strings(&add).unwrap_or_default().contains(&name) {
                store::push(&add, Value::from(name));
            }
        }),
        Rc::new(move |i: usize| {
            let mut names = (*list).clone();
            if i < names.len() {
                names.remove(i);
                store::put(&remove, store::strings(&names), Some(&toml::Value::Array(Vec::new())));
            }
        }),
    );

    f.group(Some(t("Giochi", "Games")));
    let spot = rb("games.apps");
    let apps = store::get_strings(&spot).unwrap_or_default();
    let (a, r) = (spot.clone(), spot.clone());
    let list = Rc::new(apps.clone());
    let list2 = list.clone();
    f.tokens(
        t("Giochi in finestra", "Games in a window"),
        Some(t(
            "Eseguibili che contano come gioco quando sono in primo piano, anche in finestra: niente animazioni, e i widget smettono di leggere (l'orologio no)",
            "Executables that count as a game while in front, even in a window: no animations, and the widgets stop reading (not the clock)",
        )),
        &apps,
        t("Aggiungi, es. wow.exe", "Add, e.g. wow.exe"),
        Rc::new(move |name: String| {
            let mut name = name.trim().trim_matches('"').to_string();
            if name.is_empty() {
                return;
            }
            if !name.to_lowercase().ends_with(".exe") {
                name.push_str(".exe");
            }
            if !list.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                store::push(&a, Value::from(name));
            }
        }),
        Rc::new(move |i: usize| {
            let mut apps = (*list2).clone();
            if i < apps.len() {
                apps.remove(i);
                store::put(&r, store::strings(&apps), Some(&toml::Value::Array(Vec::new())));
            }
        }),
    );
    f.footer(t(
        "Un'app che copre uno schermo senza barra del titolo (un gioco, un video, F11) nasconde comunque la barra di quello schermo, e la barra smette di lavorare finché non esce.",
        "An app that covers a screen without a title bar (a game, a video, F11) hides that screen's bar anyway, and the bar stops working until it leaves.",
    ));
}

/// The font families of Windows, sorted, read once.
fn families() -> Vec<String> {
    thread_local! {
        static FAMILIES: OnceCell<Vec<String>> = const { OnceCell::new() };
    }
    FAMILIES.with(|f| f.get_or_init(read_families).clone())
}

fn read_families() -> Vec<String> {
    use windows::Win32::Graphics::DirectWrite::{DWRITE_FACTORY_TYPE_SHARED, DWriteCreateFactory, IDWriteFactory};
    use windows::core::BOOL;
    let mut out = Vec::new();
    unsafe {
        let Ok(factory) = DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED) else { return out };
        let mut collection = None;
        if factory.GetSystemFontCollection(&mut collection, false).is_err() {
            return out;
        }
        let Some(collection) = collection else { return out };
        for i in 0..collection.GetFontFamilyCount() {
            let Ok(family) = collection.GetFontFamily(i) else { continue };
            let Ok(names) = family.GetFamilyNames() else { continue };
            // The English name, as the config writes it
            let (mut index, mut exists) = (0u32, BOOL(0));
            let _ = names.FindLocaleName(windows::core::w!("en-us"), &mut index, &mut exists);
            if !exists.as_bool() {
                index = 0;
            }
            let len = names.GetStringLength(index).unwrap_or(0);
            let mut buf = vec![0u16; len as usize + 1];
            if names.GetString(index, &mut buf).is_ok() {
                let name = String::from_utf16_lossy(&buf[..len as usize]);
                // Symbols and fonts for programs only
                if !name.starts_with('@') {
                    out.push(name);
                }
            }
        }
    }
    out.sort_by_key(|n| n.to_lowercase());
    out.dedup();
    out
}

fn theme(f: &mut Form) {
    let d = Config::default().theme;
    f.title(t("Tema", "Theme"));
    f.intro(t(
        "I colori, il carattere e le forme della barra e dei popup. «windows» segue il tema chiaro o scuro e il colore principale di Windows; le altre sono le palette della versione macOS, tutte coi nomi dei colori di Catppuccin.",
        "The colors, the font and the shapes of the bar and the popups. «windows» follows Windows' light or dark mode and accent color; the others are the palettes of the macOS version, all with Catppuccin's color names.",
    ));

    f.group(Some(t("Colori", "Colors")));
    let primary = store::get_str(&rb("theme.primary")).unwrap_or_else(|| d.primary.clone());
    let mut presets = vec![Opt::new("windows", "Windows").swatch(crate::theme::palette("windows").and_then(|p| pick(&p, &primary)))];
    for p in crate::theme::PRESETS {
        let swatch = crate::theme::palette(p.name).and_then(|c| pick(&c, &primary));
        presets.push(Opt::new(p.name, p.label).swatch(swatch));
    }
    choice(f, t("Palette", "Palette"), None, rb("theme.preset"), presets, &d.preset);
    color(
        f,
        t("Colore principale", "Accent"),
        Some(t("L'evidenziazione del desktop corrente, oggi nel calendario, le barre e i cursori dei popup", "The highlight of the current desktop, today in the calendar, the bars and sliders of the popups")),
        rb("theme.primary"),
        &d.primary,
    );
    toggle(
        f,
        t("Animazioni", "Animations"),
        Some(t("Spente comunque con un gioco in primo piano o con le animazioni di Windows disattivate", "Off anyway with a game in front, or with Windows' animations off")),
        rb("theme.animations"),
        d.animations,
    );

    f.group(Some(t("Testo", "Text")));
    let fonts: Vec<Opt> = families().into_iter().map(|n| Opt::new(n.clone(), n)).collect();
    choice(f, t("Carattere", "Font"), Some(t("Le famiglie installate in Windows", "The families installed in Windows")), rb("theme.font"), fonts, &d.font);
    number(f, t("Dimensione", "Size"), None, rb("theme.font_size"), d.font_size as f64, Num::new(6.0, 40.0, 0.5, 1));
    number(f, t("Dimensione piccola", "Small size"), Some(t("Etichette e testi secondari", "Labels and secondary text")), rb("theme.small_size"), d.small_size as f64, Num::new(6.0, 40.0, 0.5, 1));

    f.group(Some("Pill"));
    number(f, t("Altezza", "Height"), None, rb("theme.pill_height"), d.pill_height as f64, Num::new(12.0, 100.0, 1.0, 0));
    number(f, t("Raggio degli angoli", "Corner radius"), None, rb("theme.pill_radius"), d.pill_radius as f64, Num::new(0.0, 50.0, 1.0, 0));
    number(f, t("Bordo", "Border"), None, rb("theme.pill_border"), d.pill_border as f64, Num::new(0.0, 10.0, 0.5, 1));
    number(f, t("Spazio interno", "Padding"), None, rb("theme.pill_padding"), d.pill_padding as f64, Num::new(0.0, 40.0, 1.0, 0));
    number(f, t("Spazio fra le pill", "Gap between pills"), None, rb("theme.item_gap"), d.item_gap as f64, Num::new(0.0, 40.0, 1.0, 0));

    f.group(Some("Popup"));
    number(f, t("Raggio degli angoli", "Corner radius"), Some(t("8 come i riquadri di Windows 11", "8 as Windows 11's flyouts")), rb("theme.popup_radius"), d.popup_radius as f64, Num::new(0.0, 50.0, 1.0, 0));
    number(f, t("Bordo", "Border"), None, rb("theme.popup_border"), d.popup_border as f64, Num::new(0.0, 10.0, 0.5, 1));
    number(f, t("Raggio delle righe", "Row radius"), Some(t("Ciò che si clicca in un popup: un pulsante, una riga", "What can be clicked in a popup: a button, a row")), rb("theme.row_radius"), d.row_radius as f64, Num::new(0.0, 20.0, 1.0, 0));

    f.group(Some(t("Colori della palette", "Palette colors")));
    let table = rb("theme.colors");
    for name in store::keys(&table) {
        let spot = table.k(&name);
        let value = store::get_str(&spot).unwrap_or_default();
        let (t1, old) = (table.clone(), name.clone());
        let remove = spot.clone();
        let set = spot.clone();
        // The name on the left as a field, the color on the right, × to take it away
        let y = f.y;
        let w = f.width;
        let x = f.x;
        let h = 56.0;
        f.card(y, h);
        let name_field = Rect::new(x + form::ROW_PAD, y + 12.0, 180.0, form::CONTROL_H);
        f.control(
            Control::Text {
                value: name.clone(),
                placeholder: t("nome", "name").into(),
                width: 180.0,
                run: Rc::new(move |new: String| {
                    let new = new.trim().to_string();
                    if !new.is_empty() {
                        store::rename(&t1, &old, &new);
                    }
                }),
            },
            name_field.x,
            name_field.y,
            name_field.w,
        );
        let cw = f.control_width(&Control::Color { value: String::new(), default: String::new(), run: Rc::new(|_| {}) });
        let cx = x + w - form::ROW_PAD - 40.0 - cw;
        f.control(Control::Color { value: value.clone(), default: value, run: Rc::new(move |v: String| if !v.is_empty() { store::set(&set, Value::from(v)) }) }, cx, y + 12.0, cw);
        f.control(Control::Icon { glyph: "\u{E74D}", run: Rc::new(move || store::remove(&remove)) }, x + w - form::ROW_PAD - 32.0, y + 12.0, 32.0);
        f.y += h + form::ROW_GAP;
    }
    let table2 = table.clone();
    f.row(
        t("Aggiungi un colore", "Add a color"),
        Some(t("Un nome della palette da cambiare (base, text, sky…) o uno nuovo da usare nei widget", "A palette name to change (base, text, sky…) or a new one for the widgets")),
        Control::Text {
            value: String::new(),
            placeholder: t("nome, es. base", "name, e.g. base").into(),
            width: 220.0,
            run: Rc::new(move |name: String| {
                let name = name.trim().to_string();
                if name.is_empty() || name.contains(['.', ' ', '"']) {
                    return;
                }
                let theme = crate::theme::Theme::from_config(&super::theme_config());
                let start = theme.color(&name).map(crate::lua::hex).unwrap_or_else(|| "#808080".into());
                store::set(&table2.k(&name), Value::from(start));
            }),
        },
    );
}

/// A palette's color by name, or its accent.
fn pick(palette: &[(&str, crate::theme::Rgba)], name: &str) -> Option<crate::theme::Rgba> {
    palette.iter().find(|(k, _)| *k == name).or_else(|| palette.iter().find(|(k, _)| *k == "accent")).map(|(_, c)| *c)
}

fn layout(f: &mut Form, ctx: &Ctx) {
    f.title("Layout");
    f.intro(t(
        "Cosa c'è nella barra, e dove: trascina un widget per riordinarlo o spostarlo di lato, × lo toglie (le sue opzioni restano nella config), «+ Aggiungi» ne mette uno, anche un secondo dello stesso tipo.",
        "What's in the bar, and where: drag a widget to reorder it or move it to another side, × takes it away (its options stay in the config), «+ Add» puts one in, a second of a kind too.",
    ));
    let sides = sides();
    let look = f.look;
    let px = f.px();

    // The bar, small: the three groups as they sit on the screen
    let strip = Rect::new(f.x, f.y, f.width, 34.0);
    f.scene().fill(strip, 8.0, look.card_border);
    f.scene().fill(Rect::new(strip.x + px, strip.y + px, strip.w - 2.0 * px, strip.h - 2.0 * px), 8.0 - px, if look.light { look.bg.mix(look.text, 0.08) } else { look.bg.mix(crate::theme::Rgba(0.0, 0.0, 0.0, 1.0), 0.35) });
    let small = crate::ui::Font::new(11.0, crate::ui::Weight::Medium);
    let total = |ws: &[f32]| ws.iter().sum::<f32>() + 4.0 * ws.len().saturating_sub(1) as f32;
    // The names, or the icons alone when the names don't fit side by side
    let named: Vec<Vec<f32>> = sides.iter().map(|names| names.iter().map(|n| f.fonts.width(&short_of(n), small).ceil() + 16.0).collect()).collect();
    let fits = |ws: &[Vec<f32>]| total(&ws[1]) / 2.0 + total(&ws[0]).max(total(&ws[2])) + 32.0 <= strip.w / 2.0;
    let icons_only = !fits(&named);
    let widths: Vec<Vec<f32>> = if icons_only { sides.iter().map(|names| vec![26.0; names.len()]).collect() } else { named };
    let starts = [strip.x + 8.0, strip.x + (strip.w - total(&widths[1])) / 2.0, strip.x + strip.w - 8.0 - total(&widths[2])];
    for side in 0..3 {
        let mut x = starts[side];
        for (n, name) in sides[side].iter().enumerate() {
            let w = widths[side][n];
            let r = Rect::new(x, strip.y + 7.0, w, 20.0);
            f.scene().fill(r, 10.0, look.control);
            if icons_only {
                let (_, icon, _) = info(&kind_of(name));
                f.scene().text(r, icon, form::icons(11.0), look.text, Align::Center);
            } else {
                f.scene().text(r, short_of(name), small, look.text, Align::Center);
            }
            x += w + 4.0;
        }
    }
    f.y += 34.0 + 20.0;

    // The three columns
    let gap = 12.0;
    let col_w = (f.width - 2.0 * gap) / 3.0;
    let y0 = f.y;
    let most = sides.iter().map(Vec::len).max().unwrap_or(0).max(1);
    let col_h = 44.0 + most as f32 * (CHIP_H + 6.0) + 6.0 + form::CONTROL_H + 14.0;
    let titles = [t("Sinistra", "Left"), t("Centro", "Center"), t("Destra", "Right")];
    let mut areas = [Rect::default(); 3];
    let mut chips: [Vec<Rect>; 3] = Default::default();
    let used: Vec<String> = sides.concat();
    for side in 0..3 {
        let x = f.x + side as f32 * (col_w + gap);
        let area = Rect::new(x, y0, col_w, col_h);
        areas[side] = area;
        form::card(f.scene(), &look, area, px);
        f.scene().text(Rect::new(x + 16.0, y0 + 10.0, col_w - 32.0, 24.0), titles[side], form::strong(), look.text, Align::Start);
        let count = sides[side].len();
        f.scene().text(Rect::new(x + 16.0, y0 + 10.0, col_w - 32.0, 24.0), count.to_string(), form::caption(), look.sub, Align::End);
        let mut y = y0 + 44.0;
        for (i, name) in sides[side].iter().enumerate() {
            let r = Rect::new(x + 8.0, y, col_w - 16.0, CHIP_H);
            chips[side].push(r);
            form::button_box(f.scene(), &look, r, px);
            let kind = kind_of(name);
            let known = crate::widgets::KINDS.contains(&kind.as_str()) || ctx.lua.contains(&kind);
            let (_, icon, _) = info(&kind);
            f.scene().text(Rect::new(r.x + 10.0, r.y, 12.0, r.h), "\u{E76F}", form::icons(10.0), look.faint, Align::Start);
            f.scene().text(Rect::new(r.x + 28.0, r.y, 20.0, r.h), icon, form::icons(14.0), if known { look.text } else { look.red }, Align::Start);
            // A name that is no widget: the one most likely meant, and ✓ to take it
            let meant = if known { None } else { crate::config::closest(&kind, crate::widgets::KINDS.iter().copied().chain(ctx.lua.iter().map(String::as_str))) };
            let label = match (&meant, known) {
                (_, true) => title_of(name),
                (Some(_), false) => name.clone(),
                (None, false) => format!("{name} ({})", t("sconosciuto", "unknown")),
            };
            // «→ system»: about the width of its letters
            let fix_w = meant.as_ref().map_or(0.0, |m| 30.0 + 7.5 * m.chars().count() as f32);
            let room = 34.0 + fix_w;
            f.scene().text(Rect::new(r.x + 54.0, r.y, r.w - 54.0 - room, r.h), label, form::body(), if known { look.text } else { look.red }, Align::Start);
            let chip = f.add(Do::Chip { side, index: i });
            f.scene().zone(Rect::new(r.x, r.y, r.w - room, r.h), form::CONTROL_R, chip);
            if let Some(meant) = meant {
                let fix_box = Rect::new(r.x + r.w - 34.0 - fix_w, r.y + 4.0, fix_w, 28.0);
                f.scene().text(fix_box, format!("\u{2192} {meant}"), form::body(), look.accent, Align::Center);
                let names = sides[side].clone();
                let fix = f.add(Do::Run(Rc::new(move || {
                    let mut names = names.clone();
                    names[i] = meant.clone();
                    put_side(side, &names);
                })));
                f.scene().zone(fix_box, form::CONTROL_R, fix);
            }
            let x_box = Rect::new(r.x + r.w - 32.0, r.y + 4.0, 28.0, 28.0);
            f.scene().text(x_box, "\u{E711}", form::icons(10.0), look.sub, Align::Center);
            let names = sides[side].clone();
            let remove = f.add(Do::Run(Rc::new(move || {
                let mut names = names.clone();
                names.remove(i);
                put_side(side, &names);
            })));
            f.scene().zone(x_box, form::CONTROL_R, remove);
            y += CHIP_H + 6.0;
        }
        if sides[side].is_empty() {
            f.scene().text(Rect::new(x + 16.0, y, col_w - 32.0, CHIP_H), t("Vuoto", "Empty"), form::body(), look.faint, Align::Start);
        }
        // «+ Add»: what isn't in the bar yet, another of a kind, the Lua widgets
        let add = Rect::new(x + 8.0, y0 + col_h - 14.0 - form::CONTROL_H, col_w - 16.0, form::CONTROL_H);
        form::button_box(f.scene(), &look, add, px);
        f.scene().text(add, format!("+ {}", t("Aggiungi", "Add")), form::body(), look.text, Align::Center);
        let options = addable(&used, &ctx.lua);
        let names = sides[side].clone();
        let action = f.add(Do::Menu {
            rect: add,
            options: Rc::new(options),
            current: String::new(),
            on: Rc::new(move |v: String| {
                let mut names = names.clone();
                let name = match v.split_once(':') {
                    Some(("new", kind)) => {
                        let used = sides_flat();
                        let name = (2..).map(|n| format!("{kind}_{n}")).find(|n| !used.contains(n) && store::get(&rb("widgets").k(n)).is_none()).expect("a free name");
                        store::set(&rb("widgets").k(&name).k("type"), Value::from(kind));
                        name
                    }
                    Some((_, name)) => name.to_string(),
                    None => return,
                };
                names.push(name);
                put_side(side, &names);
            }),
        });
        f.scene().zone(add, form::CONTROL_R, action);
    }
    let names = sides.clone();
    let opened = sides.clone();
    f.set_columns(Columns {
        areas,
        chips,
        names: [names[0].iter().map(|n| title_of(n)).collect(), names[1].iter().map(|n| title_of(n)).collect(), names[2].iter().map(|n| title_of(n)).collect()],
        on_move: Rc::new(move |from_side, from, to_side, to| {
            let mut sides = sides.clone();
            if from >= sides[from_side].len() {
                return;
            }
            let name = sides[from_side].remove(from);
            let to = to.min(sides[to_side].len());
            sides[to_side].insert(to, name);
            put_side(from_side, &sides[from_side]);
            if to_side != from_side {
                put_side(to_side, &sides[to_side]);
            }
        }),
        // A click on a chip opens the widget's page
        on_open: Rc::new(move |side, i| {
            if let Some(name) = opened[side].get(i) {
                form::go(&format!("w:{name}"));
            }
        }),
    });
    f.y = y0 + col_h + 12.0;
    f.footer(t(
        "Quando una barra non ha spazio, i widget più lontani dal loro bordo passano alla forma compatta e poi nel popup di una pill ⋯, finché tutto entra.",
        "When a bar has no room, the widgets farthest from their edge switch to their compact form and then go in the popup of a ⋯ pill, until everything fits.",
    ));
}

/// Every name in `[layout]`.
fn sides_flat() -> Vec<String> {
    sides().concat()
}

/// What «+ Add» offers: the kinds not in the bar, the widgets of `[widgets]` and the Lua ones not
/// in it, then another of each kind already there.
fn addable(used: &[String], lua: &[String]) -> Vec<Opt> {
    let mut out: Vec<Opt> = Vec::new();
    for kind in crate::widgets::KINDS {
        if !used.iter().any(|n| n == kind) {
            out.push(Opt::new(format!("name:{kind}"), info(kind).0));
        }
    }
    for name in store::keys(&rb("widgets")) {
        if !used.contains(&name) && !crate::widgets::KINDS.contains(&name.as_str()) && store::get_str(&rb("widgets").k(&name).k("type")).is_some() {
            out.push(Opt::new(format!("name:{name}"), title_of(&name)));
        }
    }
    for name in lua {
        if !used.contains(name) {
            out.push(Opt::new(format!("name:{name}"), format!("{name} (Lua)")));
        }
    }
    for kind in crate::widgets::KINDS {
        // One of a kind is enough for these
        if matches!(*kind, "power" | "workspaces" | "front_app" | "media" | "network" | "audio" | "tray") {
            continue;
        }
        if used.iter().any(|n| kind_of(n) == *kind) {
            out.push(Opt::new(format!("new:{kind}"), format!("{} {}", info(kind).0, t("(un altro)", "(another)"))));
        }
    }
    out
}

fn about(f: &mut Form) {
    f.title(t("Informazioni", "About"));
    let (problems, _) = crate::config::problems();
    if problems.is_empty() {
        f.intro(t("La config si legge e ogni widget del layout esiste: `rustybar check` non ha niente da dire.", "The config reads and every widget of the layout exists: `rustybar check` has nothing to say."));
    } else {
        for p in &problems {
            f.banner(p, true);
        }
    }
    f.group(None);
    f.row("RustyBar", Some(t("Barra di stato leggera per Windows", "A light status bar for Windows")), Control::Label(env!("CARGO_PKG_VERSION").to_string()));
    let dir = crate::config::dir();
    let d2 = dir.clone();
    f.row(t("Cartella della config", "Config folder"), Some(&dir.display().to_string()), Control::Link { run: Rc::new(move || form::open(d2.display().to_string())) });
    let main = store::path(store::File::Main);
    let m2 = main.clone();
    f.row(
        "rustybar.toml",
        Some(&main.display().to_string()),
        Control::Button {
            label: t("Apri", "Open").into(),
            accent: false,
            run: Rc::new(move || {
                if !m2.exists() {
                    if let Some(dir) = m2.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    let _ = std::fs::write(&m2, crate::install::EXAMPLE);
                }
                form::open(m2.display().to_string());
            }),
        },
    );
    let local = store::path(store::File::Local);
    if local.exists() {
        let l2 = local.clone();
        f.row(
            "rustybar.local.toml",
            Some(&local.display().to_string()),
            Control::Button { label: t("Apri", "Open").into(), accent: false, run: Rc::new(move || form::open(l2.display().to_string())) },
        );
    }
    let log = crate::log::path_of("");
    let l3 = log.clone();
    f.row(t("Log della barra", "The bar's log"), Some(&log.display().to_string()), Control::Link { run: Rc::new(move || form::open(l3.display().to_string())) });
    f.row(
        t("Sito", "Website"),
        Some("github.com/Francesco-Rapetti/rustybar-windows"),
        Control::Link { run: Rc::new(|| form::open("https://github.com/Francesco-Rapetti/rustybar-windows")) },
    );
    f.footer(t(
        "Questa finestra gira in un processo suo, che finisce quando la chiudi: la barra non ne porta niente.",
        "This window runs in a process of its own, which ends when you close it: the bar carries none of it.",
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_a_title_and_an_icon() {
        for kind in crate::widgets::KINDS {
            let (title, icon, desc) = info(kind);
            assert!(!title.is_empty() && !icon.is_empty() && !desc.is_empty(), "{kind}");
        }
        assert_eq!(known("theme"), "theme");
        assert_eq!(known("nothing"), "bar");
    }
}
