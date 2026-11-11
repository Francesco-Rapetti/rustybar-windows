//! `rustybar.toml` in the config dir (`%APPDATA%\rustybar`, or `RUSTYBAR_CONFIG_DIR`). Every key
//! has a default, so a missing file or a missing key is fine; the bar reloads by itself when the
//! file is saved.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub bar: BarConfig,
    pub theme: ThemeConfig,
    pub layout: Layout,
    pub games: Games,
    /// Options of each widget by its name in `[layout]`: `[widgets.clock] date = "ddd dd/MM"`.
    pub widgets: HashMap<String, toml::Table>,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct BarConfig {
    /// The height of the bar, which the pills are centered in, in DIPs.
    pub height: f32,
    /// From the screen edges to the first and last pill.
    pub margin: f32,
    /// Off: the bar doesn't reserve its strip of the screen (windows go under it). For trying a
    /// second bar next to the real one.
    pub reserve: bool,
    /// From the bottom of the bar to the top of a popup, in DIPs.
    pub popup_gap: f32,
}

impl Default for BarConfig {
    fn default() -> Self {
        BarConfig { height: 40.0, margin: 8.0, reserve: true, popup_gap: 6.0 }
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeConfig {
    /// `windows` (light or dark and the accent of Windows) or one of the palettes in `theme.rs`.
    pub preset: String,
    pub primary: String,
    pub font: String,
    pub font_size: f32,
    pub small_size: f32,
    pub pill_height: f32,
    pub pill_radius: f32,
    pub pill_border: f32,
    pub pill_padding: f32,
    pub item_gap: f32,
    pub popup_radius: f32,
    pub popup_border: f32,
    pub row_radius: f32,
    pub animations: bool,
    /// Palette colors to override or add, by name or hex.
    pub colors: HashMap<String, String>,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        ThemeConfig {
            preset: "windows".into(),
            primary: "accent".into(),
            font: "Segoe UI Variable Text".into(),
            font_size: 13.0,
            small_size: 11.0,
            pill_height: 28.0,
            pill_radius: 14.0,
            pill_border: 1.0,
            pill_padding: 10.0,
            item_gap: 8.0,
            // Windows 11's: flyouts round their corners by 8, buttons and rows by 4
            popup_radius: 8.0,
            popup_border: 1.0,
            row_radius: 4.0,
            animations: true,
            colors: HashMap::new(),
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct Layout {
    pub left: Vec<String>,
    pub center: Vec<String>,
    pub right: Vec<String>,
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            left: vec!["workspaces".into(), "front_app".into()],
            center: vec!["media".into()],
            right: vec!["network".into(), "audio".into(), "system".into(), "clock".into()],
        }
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Games {
    /// Executables (`game.exe`) that count as a game while in front, even in a window: no
    /// animations and no timers but the clock's. A full screen app counts anyway.
    pub apps: Vec<String>,
}

/// The config dir: `RUSTYBAR_CONFIG_DIR`, or `%APPDATA%\rustybar`.
pub fn dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("RUSTYBAR_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    default_dir()
}

/// `%APPDATA%\rustybar`, the dir of the installed bar.
pub fn default_dir() -> PathBuf {
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    appdata.join("rustybar")
}

pub fn file() -> PathBuf {
    dir().join("rustybar.toml")
}

/// The config, or the defaults with the reason the file couldn't be used.
pub fn load() -> (Config, Option<String>) {
    let path = file();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Config::default(), None),
        Err(e) => return (Config::default(), Some(format!("{}: {e}", path.display()))),
    };
    match toml::from_str::<Config>(&text) {
        Ok(config) => (config, None),
        Err(e) => (Config::default(), Some(format!("{}: {e}", path.display()))),
    }
}

/// `rustybar check`: the config parses, its theme exists, and every name in `[layout]` is a widget.
pub fn check() -> i32 {
    let (config, error) = load();
    let mut problems: Vec<String> = error.into_iter().collect();
    if !crate::theme::known(&config.theme.preset) {
        problems.push(format!("unknown theme preset {:?}", config.theme.preset));
    }
    for name in config.layout.left.iter().chain(&config.layout.center).chain(&config.layout.right) {
        let kind = widget_kind(&config, name);
        if !crate::widgets::KINDS.contains(&kind.as_str()) {
            problems.push(format!("unknown widget {name:?} in [layout]"));
        }
    }
    if problems.is_empty() {
        println!("{}: ok", file().display());
        0
    } else {
        for p in &problems {
            println!("{p}");
        }
        1
    }
}

/// A widget's kind: its `type` option, or its name (`[widgets.clock_utc] type = "clock"`).
pub fn widget_kind(config: &Config, name: &str) -> String {
    config
        .widgets
        .get(name)
        .and_then(|t| t.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or(name)
        .to_string()
}

/// The options of one widget, as the TOML gives them.
#[derive(Clone, Debug, Default)]
pub struct Options(pub toml::Table);

impl Options {
    pub fn str(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.as_str())
    }

    pub fn f64(&self, key: &str) -> Option<f64> {
        self.0.get(key).and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        self.0.get(key).and_then(|v| v.as_bool())
    }
}
