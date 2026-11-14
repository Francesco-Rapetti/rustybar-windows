//! A widget written in Lua (the state they share, and `rb`, are in `crate::lua`):
//!
//! ```lua
//! local rb = require("rustybar")
//! rb.widget {
//!   name = "weather",
//!   every = 600,                                   -- seconds between update() calls
//!   update = function(self)
//!     rb.http("https://wttr.in/?format=%t", function(body, status)
//!       if status == 200 then self.temp = body end   -- the bar draws again after a callback
//!     end)
//!   end,
//!   bar = function(self, ctx)
//!     return rb.ui.pill { rb.ui.icon { "E9CA" }, rb.ui.text { self.temp or "…" } }
//!   end,
//!   popup = function(self, ctx)
//!     return rb.ui.column { rb.ui.section { "Weather", right = self.temp },
//!                           rb.ui.button { "Open wttr.in", on_click = "https://wttr.in" } }
//!   end,
//! }
//! ```
//!
//! `self` is the widget's own table (its `name` and its `options` from `[widgets.<name>]`), over
//! the definition: two widgets of the same kind keep apart what they store. `init` runs once,
//! then `update`, again every `every` seconds (at least 1) while a bar shows and no listed game is
//! in front; `event(self, name, args)` gets the events named in `on` (`wake`, `foreground`,
//! `time`, `game`, `shown`, or a trigger's name). The pill comes from `bar(self, ctx)` (and
//! `compact`, or `bar` with `ctx.compact`), the popup from `popup(self, ctx)`: a column, top to
//! bottom, of the pieces the built-in popups are made of (`rb.ui.text`, `section`, `row`,
//! `detail`, `button`, `slider`, `progress`, `tile`, `tiles`, `divider`, `spacer`, `image`). What
//! fails shows: the pill as `⚠ name` in red with the error in its popup, and in the log once.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use mlua::{Function, MultiValue, Table, Value};
use windows::Win32::Foundation::HANDLE;

use super::{Input, Reply, Signal};
use crate::config::Options;
use crate::log;
use crate::lua::{self, Channel, Done, Host};
use crate::theme::{Rgba, Theme};
use crate::ui::{Align, Font, Fonts, Image, Item, Pill, Rect, Scene, Weight};
use crate::wake::Timer;

/// Segoe Fluent Icons' warning sign.
const WARNING: &str = "\u{E7BA}";

// The popup's layout, in DIPs, as the built-in popups'.
const WIDTH: f32 = 320.0;
const MARGIN: f32 = 8.0;
const PAD: f32 = 10.0;
const RADIUS: f32 = 6.0;
const GAP: f32 = 4.0;
const TILE_GAP: f32 = 8.0;
const SECTION_H: f32 = 26.0;
const ROW_H: f32 = 30.0;
const DETAIL_H: f32 = 24.0;
const SLIDER_H: f32 = 28.0;
const PROGRESS_H: f32 = 14.0;
const BAR: f32 = 6.0;

/// What a click does.
#[derive(Clone)]
enum Action {
    Call(Function),
    Open(String),
    Popup,
}

impl Action {
    fn of(v: &Value) -> Option<Action> {
        match v {
            Value::Function(f) => Some(Action::Call(f.clone())),
            Value::String(s) if s.to_string_lossy() == "popup" => Some(Action::Popup),
            Value::String(s) => Some(Action::Open(s.to_string_lossy())),
            _ => None,
        }
    }
}

/// Pictures decoded, by file and size in pixels (None: the file couldn't be read).
type Pictures = HashMap<(String, u32, u32), Option<Arc<Image>>>;

#[derive(Clone, Copy, PartialEq)]
enum Slot {
    Wake,
    Timer,
}

pub struct LuaWidget {
    host: Option<Host>,
    name: String,
    /// The widget's `self`; None when its definition isn't there.
    this: Option<Table>,
    channel: Rc<Channel>,
    /// `every`, in ms.
    every: Option<i32>,
    on: Vec<String>,
    popup_fn: bool,
    timer: Timer,
    shown: bool,
    game: bool,
    open: bool,
    /// Why the pill shows `⚠`: the definition is missing, or `bar` failed.
    broken: RefCell<Option<String>>,
    /// The last error written to the log, so a failing `bar` isn't logged at every frame.
    logged: RefCell<String>,
    pill_action: RefCell<Option<Action>>,
    item_actions: RefCell<Vec<Option<Action>>>,
    popup_actions: RefCell<Vec<Action>>,
    sliders: RefCell<Vec<Function>>,
    /// The toggles of the open popup: their `on_change`, and the value a click gives.
    toggles: RefCell<Vec<(Function, bool)>>,
    /// A slider being dragged: its index and where the mouse is along it.
    sliding: Option<(usize, f32)>,
    /// The pictures of the open popup, decoded once, by file and size in pixels.
    images: RefCell<Pictures>,
}

impl LuaWidget {
    /// The widget `name` of `[layout]`, made from the definition called `kind`; `problem` says
    /// why it may be missing (a file that didn't load).
    pub fn new(host: Option<&Host>, name: &str, kind: &str, options: &Options, problem: Option<String>) -> LuaWidget {
        let mut w = LuaWidget {
            host: host.cloned(),
            name: name.to_string(),
            this: None,
            channel: Channel::new(),
            every: None,
            on: Vec::new(),
            popup_fn: false,
            timer: Timer::new(),
            shown: true,
            game: false,
            open: false,
            broken: RefCell::new(None),
            logged: RefCell::new(String::new()),
            pill_action: RefCell::new(None),
            item_actions: RefCell::new(Vec::new()),
            popup_actions: RefCell::new(Vec::new()),
            sliders: RefCell::new(Vec::new()),
            toggles: RefCell::new(Vec::new()),
            sliding: None,
            images: RefCell::new(HashMap::new()),
        };
        let Some((host, def)) = host.and_then(|h| Some((h, h.def(kind)?))) else {
            let why = problem.unwrap_or_else(|| format!("no widget is called {kind:?}"));
            w.fail(&why);
            *w.broken.get_mut() = Some(why);
            return w;
        };
        match make_self(host, name, &def, options) {
            Ok(this) => w.this = Some(this),
            Err(e) => {
                let why = lua::short(&e);
                w.fail(&why);
                *w.broken.get_mut() = Some(why);
                return w;
            }
        }
        w.on = def.get::<Option<Vec<String>>>("on").ok().flatten().unwrap_or_default();
        w.popup_fn = matches!(def.get::<Value>("popup"), Ok(Value::Function(_)));
        w.run("init", Vec::new());
        w.run("update", Vec::new());
        w.schedule();
        w
    }

    /// `update` every `every` while a bar shows and no listed game is in front.
    /// `every` as the script has it now (a script stops its timer with `self.every = false` and
    /// starts it again with a number), then the timer to match. After every call that may have
    /// changed it.
    fn schedule(&mut self) {
        let seconds = self.this.as_ref().and_then(|t| match t.get::<Value>("every").ok()? {
            Value::Integer(i) => Some(i as f64),
            Value::Number(n) => Some(n),
            _ => None,
        });
        self.every = seconds.filter(|s| *s > 0.0).map(|s| (s.max(1.0) * 1000.0).min(i32::MAX as f64) as i32);
        match self.every {
            Some(every) if self.shown && !self.game => self.timer.start(every as i64, every),
            _ => self.timer.stop(),
        }
    }

    fn slots(&self) -> Vec<(Slot, HANDLE)> {
        let mut slots: Vec<(Slot, HANDLE)> = self.channel.wake.handle().map(|h| (Slot::Wake, h)).into_iter().collect();
        slots.extend(self.timer.handle().map(|h| (Slot::Timer, h)));
        slots
    }

    /// Whether the widget defines `method`.
    fn has(&self, method: &str) -> bool {
        self.this.as_ref().is_some_and(|t| matches!(t.get::<Value>(method), Ok(Value::Function(_))))
    }

    /// Calls `self:method(args…)`: None if there's no such function.
    fn call(&self, method: &str, args: Vec<Value>) -> Result<Option<Value>, String> {
        let Some(this) = &self.this else { return Err("not loaded".into()) };
        let f = match this.get::<Value>(method) {
            Ok(Value::Function(f)) => f,
            Ok(Value::Nil) => return Ok(None),
            Ok(_) => return Err(format!("{method} isn't a function")),
            Err(e) => return Err(lua::short(&e)),
        };
        let args: MultiValue = std::iter::once(Value::Table(this.clone())).chain(args).collect();
        self.call_fn(&f, args).map(Some).map_err(|e| format!("{method}: {e}"))
    }

    fn call_fn(&self, f: &Function, args: MultiValue) -> Result<Value, String> {
        let Some(host) = &self.host else { return Err("not loaded".into()) };
        host.call(&self.channel, || f.call::<Value>(args)).map_err(|e| lua::short(&e))
    }

    /// Calls `self:method(args…)`, an error going to the log. True if it ran.
    fn run(&self, method: &str, args: Vec<Value>) -> bool {
        match self.call(method, args) {
            Ok(ran) => ran.is_some(),
            Err(e) => {
                self.fail(&e);
                true
            }
        }
    }

    /// Writes an error to the log, once until another comes.
    fn fail(&self, error: &str) {
        let mut logged = self.logged.borrow_mut();
        if *logged != error {
            log!("{}: {error}", self.name);
            *logged = error.to_string();
        }
    }

    /// `ctx`: whether the bar wants the compact form, whether the popup is open.
    fn ctx(&self, compact: bool) -> Option<Value> {
        let lua = self.host.as_ref()?.lua();
        let t = lua.create_table().ok()?;
        t.set("compact", compact).ok()?;
        t.set("popup_open", self.open).ok()?;
        Some(Value::Table(t))
    }

    /// Calls `event(self, name, args)` if the widget listens to `name`. True if it ran.
    fn deliver(&self, name: &str, args: Vec<(String, Value)>) -> bool {
        if !self.on.iter().any(|o| o == name) || !self.has("event") {
            return false;
        }
        let Some(host) = &self.host else { return false };
        let lua = host.lua();
        let Ok(table) = lua.create_table() else { return false };
        for (k, v) in args {
            let _ = table.set(k, v);
        }
        let name = match lua.create_string(name) {
            Ok(s) => Value::String(s),
            Err(_) => return false,
        };
        self.run("event", vec![name, Value::Table(table)])
    }

    fn text(&self, s: &str) -> Value {
        self.host.as_ref().and_then(|h| h.lua().create_string(s).ok()).map_or(Value::Nil, Value::String)
    }

    /// The pill a function gave, its actions kept when `record`.
    fn to_pill(&self, theme: &Theme, v: &Value, record: bool) -> Option<Pill> {
        let (mut items, mut actions) = (Vec::new(), Vec::new());
        let mut pill_action = None;
        let mut pill = match v {
            Value::Nil | Value::Boolean(false) => return None,
            Value::Table(t) if str_of(t, "_t").is_none_or(|k| k == "pill") => {
                for value in t.sequence_values::<Value>().flatten() {
                    if let Some((item, action)) = pill_item(theme, &value) {
                        items.push(item);
                        actions.push(action);
                    }
                }
                pill_action = t.get::<Value>("on_click").ok().as_ref().and_then(Action::of);
                let mut pill = Pill::new(std::mem::take(&mut items));
                if let Some(gap) = num(t, "gap") {
                    pill = pill.gap(gap.clamp(0.0, 40.0));
                }
                if let Some(h) = num(t, "highlight").filter(|h| *h >= 1.0) {
                    pill = pill.highlight(h as usize - 1);
                }
                if flag(t, "bare") {
                    pill = pill.bare();
                }
                pill
            }
            // A text, an icon, a number alone
            other => {
                let (item, action) = pill_item(theme, other)?;
                actions.push(action);
                Pill::new(vec![item])
            }
        };
        if pill.items.is_empty() {
            return None;
        }
        if record {
            *self.pill_action.borrow_mut() = pill_action;
            *self.item_actions.borrow_mut() = actions;
        }
        pill.gap = pill.gap.max(0.0);
        Some(pill)
    }

    /// The popup a function gave, laid out.
    fn layout(&self, theme: &Theme, fonts: &Fonts, root: &Table) -> Scene {
        let width = num(root, "width").unwrap_or(WIDTH).clamp(200.0, 640.0);
        let sliding = self.sliding;
        let mut lay = Lay { w: self, theme, fonts, actions: Vec::new(), sliders: Vec::new(), toggles: Vec::new(), sliding };
        let mut scene = Scene::new(width, 0.0);
        let h = lay.column(&mut scene, root, MARGIN, MARGIN, width - 2.0 * MARGIN, PAD);
        scene.height = (h + 2.0 * MARGIN).ceil();
        let Lay { actions, sliders, toggles, .. } = lay;
        *self.popup_actions.borrow_mut() = actions;
        *self.sliders.borrow_mut() = sliders;
        *self.toggles.borrow_mut() = toggles;
        scene
    }

    /// The popup of a widget that failed: what went wrong.
    fn error_scene(&self, theme: &Theme, fonts: &Fonts, error: &str) -> Scene {
        let red = theme.color("red").unwrap_or(theme.text);
        let mut scene = Scene::new(WIDTH, 0.0);
        let title = Font::new(theme.font_size, Weight::SemiBold);
        scene.text(Rect::new(MARGIN + PAD, MARGIN, WIDTH - 2.0 * (MARGIN + PAD), ROW_H), WARNING, Font::icons(14.0), red, Align::Start);
        scene.text(Rect::new(MARGIN + PAD + 22.0, MARGIN, WIDTH - 2.0 * (MARGIN + PAD) - 22.0, ROW_H), self.name.as_str(), title, red, Align::Start);
        let small = Font::new(theme.small_size, Weight::Medium);
        let mut y = MARGIN + ROW_H;
        for line in wrap(fonts, error, small, WIDTH - 2.0 * (MARGIN + PAD)) {
            scene.text(Rect::new(MARGIN + PAD, y, WIDTH - 2.0 * (MARGIN + PAD), 18.0), line, small, theme.subtext, Align::Start);
            y += 18.0;
        }
        scene.height = (y + MARGIN + 4.0).ceil();
        scene
    }

    fn picture(&self, file: &str, width: u32, height: u32) -> Option<Arc<Image>> {
        let key = (file.to_string(), width, height);
        if let Some(image) = self.images.borrow().get(&key) {
            return image.clone();
        }
        let path = std::path::Path::new(file);
        let path = if path.is_absolute() { path.to_path_buf() } else { crate::config::dir().join("widgets").join(path) };
        let image = super::media::decode_file(&path, width, height).map(Arc::new);
        self.images.borrow_mut().insert(key, image.clone());
        image
    }
}

/// `self`: a table of its own (`name`, `options`) that finds the rest in the definition.
fn make_self(host: &Host, name: &str, def: &Table, options: &Options) -> mlua::Result<Table> {
    let lua = host.lua();
    let this = lua.create_table()?;
    this.set("name", name)?;
    this.set("options", lua::from_toml(lua, &toml::Value::Table(options.0.clone()))?)?;
    let meta = lua.create_table()?;
    meta.set("__index", def.clone())?;
    this.set_metatable(Some(meta))?;
    Ok(this)
}

fn error_pill(theme: &Theme, name: &str) -> Pill {
    let red = theme.color("red").unwrap_or(theme.text);
    Pill::new(vec![Item::icon(WARNING, red), Item::text(name, red)])
}

/// An item of a pill: a text or an icon, with what a click on it does.
fn pill_item(theme: &Theme, v: &Value) -> Option<(Item, Option<Action>)> {
    match v {
        Value::String(_) | Value::Integer(_) | Value::Number(_) => Some((Item::text(lua::display(v), theme.text), None)),
        Value::Table(t) => {
            let first = t.get::<Value>(1).map(|v| lua::display(&v)).unwrap_or_default();
            let color = str_of(t, "color").and_then(|c| lua::color(theme, &c)).unwrap_or(theme.text);
            let mut item = match str_of(t, "_t").as_deref() {
                Some("icon") => Item::icon(&glyph(&first), color),
                _ => Item::text(first, color),
            };
            if flag(t, "small") {
                item = item.small();
            }
            if let Some(template) = str_of(t, "fixed").and_then(|f| intern(&f)) {
                item = item.fixed(template);
            }
            if let Some(w) = num(t, "max_width") {
                item = item.max_width(w.max(8.0));
            }
            if let Some(w) = num(t, "min_width") {
                item = item.min_width(w.max(0.0));
            }
            if let Some(s) = num(t, "space") {
                item = item.space(s.clamp(0.0, 40.0));
            }
            Some((item, t.get::<Value>("on_click").ok().as_ref().and_then(Action::of)))
        }
        _ => None,
    }
}

/// An icon by its code point (`"E9CA"`, `"U+E9CA"`), or the glyph itself.
fn glyph(s: &str) -> String {
    let code = s.trim().trim_start_matches("U+").trim_start_matches("\\u");
    match u32::from_str_radix(code, 16).ok().and_then(char::from_u32) {
        Some(c) if (4..=5).contains(&code.len()) => c.to_string(),
        _ => s.to_string(),
    }
}

/// `Item::fixed` takes a `&'static str`: each template a script uses is kept for good, at most a
/// few hundred of them.
fn intern(s: &str) -> Option<&'static str> {
    static KEPT: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(k) = kept.iter().find(|k| **k == s) {
        return Some(k);
    }
    if kept.len() >= 256 || s.len() > 64 {
        return None;
    }
    let k: &'static str = Box::leak(s.to_string().into_boxed_str());
    kept.push(k);
    Some(k)
}

fn str_of(t: &Table, key: &str) -> Option<String> {
    match t.get::<Value>(key).ok()? {
        Value::String(s) => Some(s.to_string_lossy()),
        Value::Integer(i) => Some(i.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn num(t: &Table, key: &str) -> Option<f32> {
    match t.get::<Value>(key).ok()? {
        Value::Integer(i) => Some(i as f32),
        Value::Number(n) if n.is_finite() => Some(n as f32),
        _ => None,
    }
}

fn flag(t: &Table, key: &str) -> bool {
    matches!(t.get::<Value>(key), Ok(Value::Boolean(true)))
}

/// `text` in lines no wider than `width`, broken between words.
fn wrap(fonts: &Fonts, text: &str, font: Font, width: f32) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split(' ') {
            let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if line.is_empty() || fonts.width(&candidate, font) <= width {
                line = candidate;
            } else {
                lines.push(std::mem::replace(&mut line, word.to_string()));
            }
        }
        lines.push(line);
    }
    lines.truncate(20);
    lines
}

/// Lays out a popup: the actions and sliders it meets, numbered in the order of their zones.
struct Lay<'a> {
    w: &'a LuaWidget,
    theme: &'a Theme,
    fonts: &'a Fonts,
    actions: Vec<Action>,
    sliders: Vec<Function>,
    toggles: Vec<(Function, bool)>,
    sliding: Option<(usize, f32)>,
}

/// How a text looks: its font, color and alignment.
struct Look {
    font: Font,
    color: Rgba,
    align: Align,
    line: f32,
}

impl Lay<'_> {
    fn color(&self, t: &Table, key: &str, normal: Rgba) -> Rgba {
        str_of(t, key).and_then(|c| lua::color(self.theme, &c)).unwrap_or(normal)
    }

    fn action(&mut self, t: &Table) -> Option<String> {
        let action = t.get::<Value>("on_click").ok().as_ref().and_then(Action::of)?;
        self.actions.push(action);
        Some(format!("a:{}", self.actions.len() - 1))
    }

    /// How a text node looks, `normal` its color without one of its own.
    /// A plain string (not a `text` table) is small with `small`: the right end of a section or a
    /// row, as in the built-in popups.
    fn look(&self, v: &Value, normal: Rgba, small: bool) -> (String, Look) {
        let theme = self.theme;
        let Value::Table(t) = v else {
            let (size, weight) = if small { (theme.small_size, Weight::Medium) } else { (theme.font_size, Weight::Regular) };
            return (lua::display(v), Look { font: Font::new(size, weight), color: normal, align: Align::Start, line: line(size) });
        };
        let text = t.get::<Value>(1).map(|v| lua::display(&v)).unwrap_or_default();
        let small = flag(t, "small");
        let icon = str_of(t, "_t").as_deref() == Some("icon");
        let size = num(t, "size").unwrap_or(if icon { 16.0 } else if small { theme.small_size } else { theme.font_size }).clamp(6.0, 48.0);
        let weight = match str_of(t, "weight").as_deref() {
            Some("medium") => Weight::Medium,
            Some("semibold" | "bold") => Weight::SemiBold,
            _ if small => Weight::Medium,
            _ => Weight::Regular,
        };
        let font = if icon { Font::icons(size) } else { Font::new(size, weight) };
        let align = match str_of(t, "align").as_deref() {
            Some("center") => Align::Center,
            Some("end" | "right") => Align::End,
            _ => Align::Start,
        };
        let color = self.color(t, "color", if small { theme.subtext } else { normal });
        (if icon { glyph(&text) } else { text }, Look { font, color, align, line: line(size) })
    }

    /// The children of `t` one under the other, `gap` apart; their height.
    fn column(&mut self, scene: &mut Scene, t: &Table, x: f32, y: f32, w: f32, inset: f32) -> f32 {
        let gap = num(t, "gap").unwrap_or(GAP).clamp(0.0, 40.0);
        let mut used = 0.0;
        let mut any = false;
        for child in t.sequence_values::<Value>().flatten() {
            let h = self.node(scene, &child, x, y + used, w, inset);
            if h > 0.0 {
                used += h + gap;
                any = true;
            }
        }
        if any { used - gap } else { 0.0 }
    }

    fn node(&mut self, scene: &mut Scene, v: &Value, x: f32, y: f32, w: f32, inset: f32) -> f32 {
        let theme = self.theme;
        let t = match v {
            Value::Table(t) => t.clone(),
            Value::String(_) | Value::Integer(_) | Value::Number(_) => {
                let (text, look) = self.look(v, theme.text, false);
                scene.text(Rect::new(x + inset, y, w - 2.0 * inset, look.line), text, look.font, look.color, look.align);
                return look.line;
            }
            _ => return 0.0,
        };
        let (ix, iw) = (x + inset, w - 2.0 * inset);
        match str_of(&t, "_t").as_deref().unwrap_or("column") {
            "text" | "icon" => {
                let (text, look) = self.look(v, theme.text, false);
                let lines = if flag(&t, "wrap") { wrap(self.fonts, &text, look.font, iw) } else { vec![text] };
                for (i, l) in lines.iter().enumerate() {
                    scene.text(Rect::new(ix, y + i as f32 * look.line, iw, look.line), l.as_str(), look.font, look.color, look.align);
                }
                lines.len() as f32 * look.line
            }
            "section" => {
                let title = t.get::<Value>(1).map(|v| lua::display(&v)).unwrap_or_default();
                let font = Font::new(theme.small_size, Weight::SemiBold);
                let right = t.get::<Value>("right").ok().filter(|v| !v.is_nil());
                let right_w = right.as_ref().map_or(0.0, |r| {
                    let (text, look) = self.look(r, theme.subtext, true);
                    let rw = self.fonts.width(&text, look.font).ceil().min(iw / 2.0);
                    scene.text(Rect::new(ix + iw - rw, y, rw, SECTION_H), text, look.font, look.color, Align::End);
                    rw + 8.0
                });
                scene.text(Rect::new(ix, y, iw - right_w, SECTION_H), title, font, theme.subtext, Align::Start);
                SECTION_H
            }
            "row" => {
                let h = num(&t, "height").unwrap_or(ROW_H).clamp(16.0, 80.0);
                if let Some(action) = self.action(&t) {
                    scene.zone(Rect::new(x, y, w, h), theme.row_radius, action);
                }
                let right = t.get::<Value>("right").ok().filter(|v| !v.is_nil());
                let right_w = right.as_ref().map_or(0.0, |r| {
                    let (text, look) = self.look(r, theme.subtext, true);
                    let rw = self.fonts.width(&text, look.font).ceil().min(iw / 2.0);
                    scene.text(Rect::new(ix + iw - rw, y, rw, h), text, look.font, look.color, Align::End);
                    rw + 8.0
                });
                let mut cx = ix;
                let end = ix + iw - right_w;
                for child in t.sequence_values::<Value>().flatten() {
                    if cx >= end {
                        break;
                    }
                    let (text, look) = self.look(&child, theme.text, false);
                    let cw = (self.fonts.width(&text, look.font).ceil() + 1.0).min(end - cx);
                    scene.text(Rect::new(cx, y, cw, h), text, look.font, look.color, Align::Start);
                    cx += cw + 6.0;
                }
                h
            }
            "detail" => {
                if let Some(action) = self.action(&t) {
                    scene.zone(Rect::new(x, y, w, DETAIL_H), theme.row_radius, action);
                }
                let label = t.get::<Value>(1).map(|v| lua::display(&v)).unwrap_or_default();
                let value = t.get::<Value>(2).map(|v| if v.is_nil() { String::new() } else { lua::display(&v) }).unwrap_or_default();
                let small = Font::new(theme.small_size, Weight::Medium);
                let medium = Font::new(theme.font_size, Weight::Medium);
                let value_w = self.fonts.width(&value, medium).ceil().min(iw * 0.65);
                scene.text(Rect::new(ix, y, iw - value_w - 8.0, DETAIL_H), label, small, theme.subtext, Align::Start);
                scene.text(Rect::new(ix + iw - value_w - 1.0, y, value_w + 1.0, DETAIL_H), value, medium, self.color(&t, "color", theme.text), Align::End);
                DETAIL_H
            }
            "button" => {
                if let Some(action) = self.action(&t) {
                    scene.zone(Rect::new(x, y, w, ROW_H), theme.row_radius, action);
                }
                let label = t.get::<Value>(1).map(|v| lua::display(&v)).unwrap_or_default();
                let color = self.color(&t, "color", theme.primary);
                let mut tx = ix;
                if let Some(icon) = str_of(&t, "icon") {
                    scene.text(Rect::new(ix, y, 18.0, ROW_H), glyph(&icon), Font::icons(13.0), color, Align::Center);
                    tx += 26.0;
                }
                scene.text(Rect::new(tx, y, ix + iw - tx, ROW_H), label, Font::new(theme.font_size, Weight::Regular), color, Align::Start);
                ROW_H
            }
            "slider" => {
                let index = self.sliders.len();
                let value = match self.sliding {
                    Some((i, f)) if i == index => f,
                    _ => num(&t, "value").unwrap_or(0.0).clamp(0.0, 1.0),
                };
                let track = Rect::new(ix + 4.0, y + 3.0, iw - 8.0, SLIDER_H - 6.0);
                super::audio::slider(scene, theme, track, value, !flag(&t, "disabled"));
                if let Ok(Value::Function(f)) = t.get::<Value>("on_change") {
                    self.sliders.push(f);
                    scene.slider(Rect::new(track.x - 8.0, track.y, track.w + 16.0, track.h), format!("s:{index}"));
                }
                SLIDER_H
            }
            // A label and a switch as Windows 11 draws it; a click on the row calls `on_change(on)`
            "toggle" => {
                let on = flag(&t, "on");
                let h = num(&t, "height").unwrap_or(ROW_H).clamp(24.0, 80.0);
                let switch = Rect::new(ix + iw - 40.0, y + (h - 20.0) / 2.0, 40.0, 20.0);
                let label = t.get::<Value>(1).ok().filter(|v| !v.is_nil());
                if let Some(label) = &label {
                    let (text, look) = self.look(label, theme.text, false);
                    scene.text(Rect::new(ix, y, iw - 52.0, h), text, look.font, look.color, Align::Start);
                }
                let color = self.color(&t, "color", theme.primary);
                if on {
                    scene.fill(switch, 10.0, color);
                    scene.fill(Rect::new(switch.x + 24.0, switch.y + 4.0, 12.0, 12.0), 6.0, theme.on_primary);
                } else {
                    scene.fill(switch, 10.0, theme.subtext);
                    scene.fill(Rect::new(switch.x + 1.0, switch.y + 1.0, 38.0, 18.0), 9.0, theme.base);
                    scene.fill(Rect::new(switch.x + 5.0, switch.y + 5.0, 10.0, 10.0), 5.0, theme.subtext);
                }
                if let Ok(Value::Function(f)) = t.get::<Value>("on_change") {
                    self.toggles.push((f, !on));
                    scene.zone(Rect::new(x, y, w, h), theme.row_radius, format!("t:{}", self.toggles.len() - 1));
                }
                h
            }
            "progress" => {
                let value = num(&t, "value").unwrap_or(0.0).clamp(0.0, 1.0);
                let track = Rect::new(ix, y + (PROGRESS_H - BAR) / 2.0, iw, BAR);
                scene.fill(track, BAR / 2.0, theme.text.alpha(0.10));
                let filled = (track.w * value).round();
                if filled > 0.0 {
                    scene.fill(Rect::new(track.x, track.y, filled.max(BAR), BAR), BAR / 2.0, self.color(&t, "color", theme.primary));
                }
                PROGRESS_H
            }
            "tile" => {
                let (inner, h) = self.tile_inside(&t, w);
                self.tile_place(scene, &t, inner, x, y, w, h);
                h
            }
            "tiles" => {
                let columns = num(&t, "columns").unwrap_or(2.0).clamp(1.0, 4.0) as usize;
                let tile_w = (w - TILE_GAP * (columns - 1) as f32) / columns as f32;
                let children: Vec<Table> = t
                    .sequence_values::<Value>()
                    .flatten()
                    .filter_map(|v| match v {
                        Value::Table(c) => Some(c),
                        _ => None,
                    })
                    .collect();
                let mut used = 0.0;
                for (r, row) in children.chunks(columns).enumerate() {
                    let parts: Vec<(Scene, f32)> = row.iter().map(|c| self.tile_inside(c, tile_w)).collect();
                    let h = parts.iter().map(|(_, h)| *h).fold(0.0, f32::max);
                    if r > 0 {
                        used += TILE_GAP;
                    }
                    for (i, (c, (inner, _))) in row.iter().zip(parts).enumerate() {
                        self.tile_place(scene, c, inner, x + i as f32 * (tile_w + TILE_GAP), y + used, tile_w, h);
                    }
                    used += h;
                }
                used
            }
            "divider" => {
                scene.fill(Rect::new(ix, y + 4.0, iw, 1.0), 0.0, theme.text.alpha(0.08));
                9.0
            }
            "spacer" => num(&t, "height").unwrap_or(8.0).clamp(0.0, 200.0),
            "image" => {
                let file = t.get::<Value>(1).map(|v| lua::display(&v)).unwrap_or_default();
                let width = num(&t, "width").unwrap_or(64.0).clamp(4.0, iw);
                let height = num(&t, "height").unwrap_or(width).clamp(4.0, 600.0);
                let radius = num(&t, "radius").unwrap_or(RADIUS).max(0.0);
                let px = |v: f32| (v * 2.0).round() as u32;
                let left = match str_of(&t, "align").as_deref() {
                    Some("center") => ix + (iw - width) / 2.0,
                    Some("end" | "right") => ix + iw - width,
                    _ => ix,
                };
                match self.w.picture(&file, px(width), px(height)) {
                    Some(image) => scene.image(Rect::new(left, y, width, height), radius, image),
                    None => scene.fill(Rect::new(left, y, width, height), radius, theme.text.alpha(0.08)),
                }
                height
            }
            _ => self.column(scene, &t, x, y, w, inset),
        }
    }

    /// What a tile holds, laid out on its own (at 0, 0) to know its height first.
    fn tile_inside(&mut self, t: &Table, w: f32) -> (Scene, f32) {
        let mut inner = Scene::new(w, 0.0);
        let h = self.column(&mut inner, t, PAD, PAD, w - 2.0 * PAD, 0.0);
        (inner, h + 2.0 * PAD)
    }

    /// A tile's background (tinted with `color` when `alert`), its zone, then what it holds.
    #[allow(clippy::too_many_arguments)]
    fn tile_place(&mut self, scene: &mut Scene, t: &Table, inner: Scene, x: f32, y: f32, w: f32, h: f32) {
        let color = self.color(t, "color", self.theme.primary);
        scene.fill(Rect::new(x, y, w, h), RADIUS, self.theme.tile(color, flag(t, "alert")));
        // Under the zones inside it, which come after and win
        if let Some(action) = self.action(t) {
            scene.zone(Rect::new(x, y, w, h), RADIUS, action);
        }
        scene.append(inner, x, y);
    }
}

/// The height of a line of text of `size`.
fn line(size: f32) -> f32 {
    (size * 1.5).ceil()
}

impl super::Widget for LuaWidget {
    fn pill(&self, theme: &Theme) -> Option<Pill> {
        if self.this.is_none() {
            return Some(error_pill(theme, &self.name));
        }
        let ctx = self.ctx(false)?;
        match self.call("bar", vec![ctx]) {
            Ok(Some(v)) => {
                *self.broken.borrow_mut() = None;
                self.to_pill(theme, &v, true)
            }
            Ok(None) => None,
            Err(e) => {
                self.fail(&e);
                *self.broken.borrow_mut() = Some(e);
                Some(error_pill(theme, &self.name))
            }
        }
    }

    /// `compact(self, ctx)`, or `bar` with `ctx.compact`: kept only if narrower.
    fn compact(&self, theme: &Theme) -> Option<Pill> {
        if self.this.is_none() || self.broken.borrow().is_some() {
            return None;
        }
        let method = if self.has("compact") { "compact" } else { "bar" };
        match self.call(method, vec![self.ctx(true)?]) {
            Ok(Some(v)) => self.to_pill(theme, &v, false),
            Ok(None) => None,
            Err(e) => {
                self.fail(&e);
                None
            }
        }
    }

    fn handles(&self) -> Vec<HANDLE> {
        self.slots().into_iter().map(|(_, h)| h).collect()
    }

    fn signaled_at(&mut self, index: usize) -> bool {
        let changed = match self.slots().get(index).map(|(slot, _)| *slot) {
            Some(Slot::Wake) => {
                for (f, done) in self.channel.take() {
                    let args: MultiValue = match done {
                        Done::Http { status, body, error } => {
                            [self.text(&body), Value::Integer(status as i64), error.map_or(Value::Nil, |e| self.text(&e))].into_iter().collect()
                        }
                        Done::Exec { code, stdout, stderr } => [self.text(&stdout), Value::Integer(code as i64), self.text(&stderr)].into_iter().collect(),
                    };
                    if let Err(e) = self.call_fn(&f, args) {
                        self.fail(&format!("callback: {e}"));
                    }
                }
                true
            }
            Some(Slot::Timer) => self.run("update", Vec::new()),
            None => false,
        };
        self.schedule();
        changed
    }

    fn event(&mut self, signal: &Signal) -> bool {
        let changed = match signal {
            Signal::Shown(shown) => {
                let changed = *shown != self.shown;
                self.shown = *shown;
                self.schedule();
                changed && self.deliver("shown", vec![("shown".into(), Value::Boolean(*shown))])
            }
            Signal::Game(game) => {
                self.game = *game;
                self.schedule();
                self.deliver("game", vec![("game".into(), Value::Boolean(*game))])
            }
            // After a sleep what `update` shows is old
            Signal::Resume => {
                let heard = self.deliver("wake", Vec::new());
                let updated = self.every.is_some() && self.run("update", Vec::new());
                heard || updated
            }
            Signal::TimeChanged => self.deliver("time", Vec::new()),
            Signal::DevicesChanged | Signal::Displays | Signal::FrontWindow(_) => false,
            Signal::Foreground(front) => {
                if !self.on.iter().any(|o| o == "foreground") {
                    return false;
                }
                let args = vec![("app".into(), self.text(&front.name)), ("exe".into(), self.text(&front.exe))];
                self.deliver("foreground", args)
            }
            Signal::Trigger { name, args } => {
                if !self.on.iter().any(|o| o == name) {
                    return false;
                }
                let args = args.iter().map(|(k, v)| (k.clone(), self.text(v))).collect();
                self.deliver(name, args)
            }
        };
        self.schedule();
        changed
    }

    /// A popup function, or an error to show.
    fn has_popup(&self) -> bool {
        self.popup_fn || self.broken.borrow().is_some()
    }

    fn clickable(&self) -> bool {
        !self.has_popup()
            && (self.pill_action.borrow().as_ref().is_some_and(|a| !matches!(a, Action::Popup))
                || self.item_actions.borrow().iter().any(|a| a.as_ref().is_some_and(|a| !matches!(a, Action::Popup))))
    }

    fn click(&mut self, item: Option<usize>, _compact: bool) -> bool {
        let action = item.and_then(|i| self.item_actions.borrow().get(i).cloned().flatten()).or_else(|| self.pill_action.borrow().clone());
        match action {
            Some(Action::Call(f)) => {
                if let Err(e) = self.call_fn(&f, MultiValue::new()) {
                    self.fail(&format!("on_click: {e}"));
                }
                self.schedule();
                true
            }
            Some(Action::Open(target)) => {
                crate::process::open(&target);
                false
            }
            _ => false,
        }
    }

    fn popup_shown(&mut self, open: bool) {
        self.open = open;
        if !open {
            self.popup_actions.borrow_mut().clear();
            self.sliders.borrow_mut().clear();
            self.toggles.borrow_mut().clear();
            self.images.borrow_mut().clear();
            self.sliding = None;
            if let Some(host) = &self.host {
                host.collect();
            }
        }
    }

    fn popup(&self, theme: &Theme, fonts: &Fonts) -> Option<Scene> {
        if let Some(error) = self.broken.borrow().clone() {
            return Some(self.error_scene(theme, fonts, &error));
        }
        match self.call("popup", vec![self.ctx(false)?]) {
            Ok(Some(Value::Table(t))) => Some(self.layout(theme, fonts, &t)),
            Ok(_) => None,
            Err(e) => {
                self.fail(&e);
                Some(self.error_scene(theme, fonts, &e))
            }
        }
    }

    fn popup_input(&mut self, input: &Input) -> Reply {
        // After a function ran: its timer as it may have changed it, and the popup closed if it asked
        let close = |w: &mut LuaWidget| {
            w.schedule();
            if w.host.as_ref().is_some_and(Host::take_close) { Reply::Close } else { Reply::Redraw }
        };
        match input {
            Input::Action(a) if a.starts_with("t:") => {
                let toggle = a[2..].parse::<usize>().ok().and_then(|i| self.toggles.borrow().get(i).cloned());
                let Some((f, value)) = toggle else { return Reply::Nothing };
                if let Err(e) = self.call_fn(&f, std::iter::once(Value::Boolean(value)).collect()) {
                    self.fail(&format!("on_change: {e}"));
                }
                close(self)
            }
            Input::Action(a) => {
                let Some(action) = a.strip_prefix("a:").and_then(|i| i.parse::<usize>().ok()).and_then(|i| self.popup_actions.borrow().get(i).cloned()) else {
                    return Reply::Nothing;
                };
                match action {
                    Action::Call(f) => {
                        if let Err(e) = self.call_fn(&f, MultiValue::new()) {
                            self.fail(&format!("on_click: {e}"));
                        }
                        close(self)
                    }
                    Action::Open(target) => {
                        crate::process::open(&target);
                        Reply::Opened
                    }
                    Action::Popup => Reply::Nothing,
                }
            }
            Input::Slide(a, fraction) => match a.strip_prefix("s:").and_then(|i| i.parse::<usize>().ok()) {
                Some(i) => {
                    self.sliding = Some((i, fraction.clamp(0.0, 1.0)));
                    Reply::Redraw
                }
                None => Reply::Nothing,
            },
            // The value goes to `on_change` when the mouse goes up
            Input::Release(a) => {
                let Some((i, value)) = self.sliding.take() else { return Reply::Nothing };
                if *a != format!("s:{i}") {
                    return Reply::Redraw;
                }
                let Some(f) = self.sliders.borrow().get(i).cloned() else { return Reply::Redraw };
                if let Err(e) = self.call_fn(&f, std::iter::once(Value::Number(value as f64)).collect()) {
                    self.fail(&format!("on_change: {e}"));
                }
                close(self)
            }
            _ => Reply::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lua::tests::{dir_with, theme};
    use crate::widgets::Widget;

    fn widget(test: &str, code: &str, kind: &str, options: &str) -> LuaWidget {
        let dir = dir_with(test, &[("w.lua", code)]);
        let (host, errors) = Host::load(&dir, &theme()).unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        LuaWidget::new(Some(&host), "mine", kind, &Options(toml::from_str(options).unwrap()), None)
    }

    fn texts(p: &Pill) -> Vec<String> {
        p.items.iter().map(|i| i.text.clone()).collect()
    }

    #[test]
    fn a_pill_from_lua() {
        let w = widget(
            "pill",
            r#"rb.widget { name = "weather",
                 init = function(self) self.n = 0 end,
                 update = function(self) self.n = self.n + 1 end,
                 bar = function(self, ctx)
                   if ctx.compact then return rb.ui.pill { rb.ui.text { "c" } } end
                   return rb.ui.pill { rb.ui.icon { "E9CA", color = "yellow" }, rb.ui.text { self.options.city .. " " .. self.n, small = true, fixed = "Rome 00" },
                                       highlight = 2, bare = true, on_click = function() self.n = 10 end }
                 end }"#,
            "weather",
            "city = \"Rome\"",
        );
        let t = theme();
        let p = w.pill(&t).unwrap();
        assert_eq!(texts(&p), ["\u{E9CA}", "Rome 1"]);
        assert!(p.items[0].icon && p.items[1].small && p.bare);
        assert_eq!(p.items[0].color, t.color("yellow").unwrap());
        assert_eq!(p.items[1].fixed, Some("Rome 00"));
        assert_eq!(p.highlight, Some(1));
        assert_eq!(texts(&w.compact(&t).unwrap()), ["c"]);
        assert!(!w.has_popup() && w.clickable());
        let mut w = w;
        assert!(w.click(None, false));
        assert_eq!(texts(&w.pill(&t).unwrap())[1], "Rome 10");
    }

    #[test]
    fn a_failing_bar_shows_and_is_logged_once() {
        let w = widget("fail", "rb.widget { name = 'x', bar = function() error('boom') end }", "x", "");
        let p = w.pill(&theme()).unwrap();
        assert_eq!(texts(&p)[1], "mine");
        assert!(w.broken.borrow().as_deref().unwrap().contains("boom"));
        assert!(w.has_popup());
        // A missing definition says so too
        let lost = widget("lost", "rb.widget { name = 'x' }", "y", "");
        assert!(lost.broken.borrow().as_deref().unwrap().contains("\"y\""));
    }

    #[test]
    fn events_and_triggers_reach_the_widgets_that_listen() {
        let mut w = widget(
            "events",
            r#"rb.widget { name = "x", on = { "ping", "wake" },
                 event = function(self, name, args) self.last = name .. ":" .. (args.who or "") end,
                 bar = function(self) return self.last or "none" end }"#,
            "x",
            "",
        );
        let t = theme();
        let args = vec![("who".to_string(), "me".to_string())];
        assert!(w.event(&Signal::Trigger { name: "ping", args: &args }));
        assert_eq!(texts(&w.pill(&t).unwrap()), ["ping:me"]);
        assert!(!w.event(&Signal::Trigger { name: "other", args: &[] }));
        assert!(w.event(&Signal::Resume));
        assert_eq!(texts(&w.pill(&t).unwrap()), ["wake:"]);
        assert!(!w.event(&Signal::TimeChanged));
    }

    #[test]
    fn every_is_a_second_at_least_and_stops_with_a_game() {
        let mut w = widget("every", "rb.widget { name = 'x', every = 0.2, update = function() end }", "x", "");
        assert_eq!(w.every, Some(1000));
        assert!(w.timer.handle().is_some());
        w.event(&Signal::Game(true));
        assert!(w.timer.handle().is_none());
        w.event(&Signal::Game(false));
        w.event(&Signal::Shown(false));
        assert!(w.timer.handle().is_none());
    }

    #[test]
    fn toggles_and_disclosures() {
        let mut w = widget(
            "toggle",
            r#"rb.widget { name = "x", on = false, open = false,
                 bar = function() return "x" end,
                 popup = function(self)
                   return rb.ui.column {
                     rb.ui.toggle { "Lamp", on = self.on, on_change = function(v) self.on = v end },
                     rb.ui.disclosure { "More", open = self.open, on_click = function() self.open = not self.open end,
                       rb.ui.text { "Inside" } },
                   }
                 end }"#,
            "x",
            "",
        );
        let t = theme();
        let fonts = Fonts::new(&t).unwrap();
        w.popup_shown(true);
        let closed = w.popup(&t, &fonts).unwrap();
        let actions: Vec<&str> = closed.zones.iter().map(|z| z.action.as_str()).collect();
        assert_eq!(actions, ["t:0", "a:0"]);
        // A click on the toggle gives on_change the other value; on the title, the section opens
        assert_eq!(w.popup_input(&Input::Action("t:0")), Reply::Redraw);
        assert!(w.this.as_ref().unwrap().get::<bool>("on").unwrap());
        assert_eq!(w.popup_input(&Input::Action("a:0")), Reply::Redraw);
        let open = w.popup(&t, &fonts).unwrap();
        assert!(open.height > closed.height);
        assert_eq!(w.popup_input(&Input::Action("t:0")), Reply::Redraw);
        assert!(!w.this.as_ref().unwrap().get::<bool>("on").unwrap());
    }

    #[test]
    fn home_assistant_without_the_home_widget() {
        let w = widget(
            "home",
            r#"rb.widget { name = "x",
                 bar = function() return tostring(rb.home.online()) .. " " .. tostring(rb.home.state("light.a")) .. " " .. tostring(rb.home.call("light", "toggle", { entity_id = "light.a" })) end }"#,
            "x",
            "",
        );
        let p = w.pill(&theme()).unwrap();
        assert_eq!(p.items[0].text, "false nil false");
    }

    #[test]
    fn a_popup_laid_out_with_its_actions() {
        let mut w = widget(
            "popup",
            r#"rb.widget { name = "x", level = 0.25,
                 bar = function() return "x" end,
                 popup = function(self)
                   return rb.ui.column {
                     rb.ui.section { "Title", right = "now" },
                     rb.ui.tiles { rb.ui.tile { rb.ui.text { "A" }, rb.ui.text { "B" } }, rb.ui.tile { rb.ui.text { "C" } } },
                     rb.ui.detail { "Label", "Value" },
                     rb.ui.slider { value = self.level, on_change = function(v) self.level = v end },
                     rb.ui.progress { value = 0.5 },
                     rb.ui.divider(),
                     rb.ui.button { "Go", on_click = function() self.went = true; rb.close_popup() end },
                   }
                 end }"#,
            "x",
            "",
        );
        let t = theme();
        let fonts = Fonts::new(&t).unwrap();
        assert!(w.has_popup());
        w.popup_shown(true);
        let scene = w.popup(&t, &fonts).unwrap();
        let actions: Vec<&str> = scene.zones.iter().map(|z| z.action.as_str()).collect();
        assert_eq!(actions, ["s:0", "a:0"]);
        assert!(scene.height > 150.0 && scene.width == WIDTH);
        // The two tiles of a row are as tall as each other: the taller one's height
        let tiles: Vec<&Rect> = scene.shapes.iter().filter_map(|s| match s {
            crate::ui::Shape::Fill { rect, radius, .. } if *radius == RADIUS => Some(rect),
            _ => None,
        }).collect();
        assert_eq!(tiles.len(), 2);
        assert_eq!(tiles[0].h, tiles[1].h);
        // A drag shows where it is; the value goes to on_change when the mouse goes up
        assert_eq!(w.popup_input(&Input::Slide("s:0", 0.75)), Reply::Redraw);
        assert_eq!(w.popup_input(&Input::Release("s:0")), Reply::Redraw);
        assert_eq!(w.this.as_ref().unwrap().get::<f64>("level").unwrap(), 0.75);
        // rb.close_popup in a click closes it
        assert_eq!(w.popup_input(&Input::Action("a:0")), Reply::Close);
        assert!(w.this.as_ref().unwrap().get::<bool>("went").unwrap());
        w.popup_shown(false);
        assert!(w.popup_actions.borrow().is_empty());
    }

    #[test]
    fn exec_answers_on_the_main_thread() {
        use windows::Win32::System::Threading::WaitForSingleObject;
        let mut w = widget(
            "exec",
            r#"rb.widget { name = "x",
                 update = function(self) rb.exec("echo hello", function(out, code) self.out = out:gsub("%s+$", "") .. " " .. code end) end,
                 bar = function(self) return self.out or "waiting" end }"#,
            "x",
            "",
        );
        let t = theme();
        assert_eq!(texts(&w.pill(&t).unwrap()), ["waiting"]);
        let wake = w.channel.wake.handle().unwrap();
        assert_eq!(unsafe { WaitForSingleObject(wake, 10_000) }, windows::Win32::Foundation::WAIT_OBJECT_0);
        assert!(w.signaled_at(0));
        assert_eq!(texts(&w.pill(&t).unwrap()), ["hello 0"]);
    }

    #[test]
    fn a_script_stops_and_starts_its_timer() {
        let mut w = widget(
            "every-self",
            r#"rb.widget { name = "x", every = 5, update = function() end,
                 on = { "stop", "go" },
                 event = function(self, name) if name == "stop" then self.every = false else self.every = 2 end end }"#,
            "x",
            "",
        );
        assert_eq!(w.every, Some(5000));
        w.event(&Signal::Trigger { name: "stop", args: &[] });
        assert_eq!(w.every, None);
        assert!(w.timer.handle().is_none());
        w.event(&Signal::Trigger { name: "go", args: &[] });
        assert_eq!(w.every, Some(2000));
        assert!(w.timer.handle().is_some());
    }

    #[test]
    fn json_for_what_http_brings() {
        let w = widget("json", r#"rb.widget { name = "x", bar = function() local t = rb.json('{"a":[1,2.5,"x"],"b":{"c":true},"d":null}') return t.a[1] + t.a[2] .. t.a[3] .. tostring(t.b.c) .. tostring(t.d) .. tostring(rb.json("nope")) end }"#, "x", "");
        assert_eq!(texts(&w.pill(&theme()).unwrap()), ["3.5xtruenilnil"]);
    }

    #[test]
    fn icons_by_code() {
        assert_eq!(glyph("E9CA"), "\u{E9CA}");
        assert_eq!(glyph("U+E9CA"), "\u{E9CA}");
        assert_eq!(glyph("♪"), "♪");
        assert_eq!(glyph("cloud.sun"), "cloud.sun");
    }
}
