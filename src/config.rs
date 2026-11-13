//! `rustybar.toml` in the config dir (`%APPDATA%\rustybar`, or `RUSTYBAR_CONFIG_DIR`). Every key
//! has a default, so a missing file or a missing key is fine; the bar reloads by itself when the
//! file is saved. `rustybar.local.toml` beside it, if there is one, goes over it: its tables merge
//! key by key, any other value replaces the one of `rustybar.toml` (what to keep out of a shared
//! dotfiles repo, such as a server or the accounts).

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
    /// Which screens get a bar: `all` (each its own) or `primary` (the main one alone).
    pub screens: String,
    /// Widgets drawn in their compact form on every screen, room or not.
    pub always_compact: Vec<String>,
    /// From the bottom of the bar to the top of a popup, in DIPs.
    pub popup_gap: f32,
}

impl Default for BarConfig {
    fn default() -> Self {
        BarConfig { height: 40.0, margin: 8.0, reserve: true, screens: "all".into(), always_compact: Vec::new(), popup_gap: 6.0 }
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
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

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Layout {
    pub left: Vec<String>,
    pub center: Vec<String>,
    pub right: Vec<String>,
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            left: vec!["power".into(), "workspaces".into(), "front_app".into()],
            center: vec!["media".into()],
            right: vec!["network".into(), "audio".into(), "system".into(), "clock".into()],
        }
    }
}

#[derive(Deserialize, Debug, Clone, Default, PartialEq)]
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

/// What to keep out of a shared `rustybar.toml` (a server, accounts), merged over it.
pub fn local_file() -> PathBuf {
    dir().join("rustybar.local.toml")
}

/// The config, or the defaults with the reason the files couldn't be used.
pub fn load() -> (Config, Option<String>) {
    match read() {
        Ok(config) => (config, None),
        Err(e) => (Config::default(), Some(e)),
    }
}

/// A file's text, or None if there's no such file.
fn text_of(path: &std::path::Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// `rustybar.toml` with `rustybar.local.toml` over it, if there is one.
fn read() -> Result<Config, String> {
    let (path, local) = (file(), local_file());
    let text = text_of(&path)?.unwrap_or_default();
    let Some(more) = text_of(&local)? else {
        return toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()));
    };
    let mut table: toml::Table = text.parse().map_err(|e| format!("{}: {e}", path.display()))?;
    merge(&mut table, more.parse().map_err(|e| format!("{}: {e}", local.display()))?);
    toml::Value::Table(table).try_into().map_err(|e: toml::de::Error| format!("{} + {}: {e}", path.display(), local.display()))
}

/// Puts `more` over `base`: tables merge key by key, anything else (arrays too) is replaced.
pub fn merge(base: &mut toml::Table, more: toml::Table) {
    for (key, value) in more {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(b)), toml::Value::Table(m)) => merge(b, m),
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

/// What `rustybar check` finds wrong (the config doesn't parse, its theme doesn't exist, a Lua
/// widget doesn't load, a name in `[layout]` is no widget), and the names of the Lua widgets, if
/// there are any.
pub fn problems() -> (Vec<String>, Option<Vec<String>>) {
    let (config, error) = load();
    let mut problems: Vec<String> = error.into_iter().collect();
    if !crate::theme::known(&config.theme.preset) {
        problems.push(format!("unknown theme preset {:?}", config.theme.preset));
    }
    if !matches!(config.bar.screens.as_str(), "all" | "primary") {
        problems.push(format!("[bar] screens is {:?}: all or primary", config.bar.screens));
    }
    let lua = crate::lua::check(&dir(), &crate::theme::Theme::from_config(&config.theme));
    let lua_names = match &lua {
        Some((names, errors)) => {
            problems.extend(errors.iter().cloned());
            names.clone()
        }
        None => Vec::new(),
    };
    let mut known: Vec<&str> = crate::widgets::KINDS.to_vec();
    known.extend(lua_names.iter().map(String::as_str));
    for name in config.layout.left.iter().chain(&config.layout.center).chain(&config.layout.right) {
        let kind = widget_kind(&config, name);
        if !crate::widgets::KINDS.contains(&kind.as_str()) && !lua_names.contains(&kind) {
            let hint = closest(&kind, known.iter().copied()).map(|n| format!(" (maybe {n:?}?)")).unwrap_or_default();
            problems.push(format!("unknown widget {name:?} in [layout]{hint}"));
        }
    }
    (problems, lua.map(|(names, _)| names))
}

/// `rustybar check`: says what `problems` finds, or that the config is fine.
pub fn check() -> i32 {
    let (problems, lua) = problems();
    if problems.is_empty() {
        let local = local_file();
        let lua = match &lua {
            Some(names) if !names.is_empty() => format!(" (Lua: {})", names.join(", ")),
            _ => String::new(),
        };
        if local.exists() {
            println!("{} + {}: ok{lua}", file().display(), local.display());
        } else {
            println!("{}: ok{lua}", file().display());
        }
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

/// The name most likely meant by one that isn't a widget (`sistem`: `system`, `notification`:
/// `notifications`), among `names`: the nearest by edits, if it's near enough for a typo.
pub fn closest<'a>(wrong: &str, names: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let wrong = wrong.to_lowercase();
    let near = (wrong.chars().count() / 3).max(1);
    names
        .into_iter()
        .filter(|n| !n.eq_ignore_ascii_case(&wrong))
        .map(|n| (edits(&wrong, &n.to_lowercase()), n))
        .filter(|(d, n)| *d <= near || (wrong.len() >= 4 && n.to_lowercase().starts_with(&wrong)))
        .min_by_key(|(d, _)| *d)
        .map(|(_, n)| n.to_string())
}

/// Levenshtein's distance: the letters to add, remove or change to make `b` of `a`.
fn edits(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(ca != *cb)).min(row[j] + 1).min(above + 1);
            diagonal = above;
        }
    }
    row[b.len()]
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

    /// A list of texts (what isn't a text in it is left out).
    pub fn strings(&self, key: &str) -> Option<Vec<String>> {
        self.0.get(key).and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|v| v.as_str()).map(str::to_string).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_goes_over() {
        let mut base: toml::Table = "[bar]\nheight = 36\n[layout]\nright = [\"clock\"]\n[widgets.claude]\ncolor = \"red\"\nwarning = 70".parse().unwrap();
        merge(&mut base, "[layout]\nright = [\"claude\", \"clock\"]\n[widgets.claude]\ndir = \"~/.claude-work\"".parse().unwrap());
        let config: Config = toml::Value::Table(base).try_into().unwrap();
        assert_eq!(config.bar.height, 36.0);
        assert_eq!(config.layout.right, ["claude", "clock"]);
        let claude = Options(config.widgets["claude"].clone());
        assert_eq!((claude.str("color"), claude.str("dir"), claude.f64("warning")), (Some("red"), Some("~/.claude-work"), Some(70.0)));
    }

    #[test]
    fn the_widget_most_likely_meant() {
        let kinds = crate::widgets::KINDS.iter().copied();
        assert_eq!(closest("sistem", kinds.clone()).as_deref(), Some("system"));
        assert_eq!(closest("Clok", kinds.clone()).as_deref(), Some("clock"));
        assert_eq!(closest("front", kinds.clone()).as_deref(), Some("front_app"));
        assert_eq!(closest("weather", kinds.clone()), None);
        assert_eq!(closest("pomodor", ["pomodoro", "power"]).as_deref(), Some("pomodoro"));
    }
}
