//! Widgets written in Lua: every `<config dir>\widgets\*.lua` file can define some, with
//! `rb.widget { … }` (see `widgets::lua` for what a widget does). Lua 5.4 runs inside the bar, on
//! the main thread, in one state for all the widgets, made only when the layout names a widget no
//! built-in has and there are files: without them, nothing of Lua is in memory.
//!
//! What keeps it light and the bar safe:
//! - every call into Lua has 250 ms (a hook every few thousand instructions looks at the clock),
//!   then it stops with an error; the state has 16 MB at most;
//! - no `os.execute`, `os.exit`, `io.popen`, nor C modules (`package.cpath` is empty): what's slow
//!   goes through `rb.http` (WinHTTP on a worker thread) or `rb.exec` (a small child process, the
//!   only way a script costs a process), whose answers come back on the main thread to the
//!   callback, through the widget's own event;
//! - `print` and `rb.log` go to the bar's log.
//!
//! `require("rustybar")` (or the global `rb`) is the API, with `rb.ui` (the builders of the
//! prelude below); `require("name")` loads `widgets\lib\name.lua`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mlua::{Function, HookTriggers, Lua, Table, Value, VmState};

use crate::log;
use crate::theme::{Rgba, Theme};
use crate::wake::Wake;

/// The most a single call into Lua may take.
pub const BUDGET: Duration = Duration::from_millis(250);
/// The most memory the state may use.
const MEMORY: usize = 16 * 1024 * 1024;
/// Requests and commands a widget may have going at once.
const PENDING: usize = 8;

/// The builders of `rb.ui`: each tags its table with what it is; the rest of the table is its
/// options and, in the array part, its text or its children.
const PRELUDE: &str = r#"
local rb = require("rustybar")
rb.ui = {}
local function kind(t)
  return function(spec)
    if type(spec) ~= "table" then spec = { spec } end
    spec._t = t
    return spec
  end
end
for _, t in ipairs({ "pill", "text", "icon", "column", "section", "row", "detail", "button", "slider",
                     "progress", "tile", "tiles", "image", "spacer" }) do
  rb.ui[t] = kind(t)
end
function rb.ui.divider() return { _t = "divider" } end
"#;

/// What a request or a command in the background brings back.
pub enum Done {
    Http { status: u16, body: String, error: Option<String> },
    Exec { code: i32, stdout: String, stderr: String },
}

/// Where the background work of one widget comes back: the worker leaves what it brought and
/// sets the widget's event; the widget then calls the callbacks on the main thread.
pub struct Channel {
    pub wake: Arc<Wake>,
    done: Arc<Mutex<Vec<(u64, Done)>>>,
    callbacks: RefCell<HashMap<u64, Function>>,
    next: Cell<u64>,
}

impl Channel {
    pub fn new() -> Rc<Channel> {
        Rc::new(Channel { wake: Wake::new(), done: Arc::new(Mutex::new(Vec::new())), callbacks: RefCell::new(HashMap::new()), next: Cell::new(1) })
    }

    /// The callbacks whose work came back, with what it brought.
    pub fn take(&self) -> Vec<(Function, Done)> {
        let done: Vec<(u64, Done)> = lock(&self.done).drain(..).collect();
        let mut callbacks = self.callbacks.borrow_mut();
        done.into_iter().filter_map(|(id, d)| Some((callbacks.remove(&id)?, d))).collect()
    }

    /// Keeps `callback` for the work about to start; its id, or an error when too much is going.
    fn register(&self, callback: Function) -> mlua::Result<u64> {
        if self.callbacks.borrow().len() >= PENDING {
            return Err(mlua::Error::runtime(format!("more than {PENDING} requests at once")));
        }
        let id = self.next.get();
        self.next.set(id + 1);
        self.callbacks.borrow_mut().insert(id, callback);
        Ok(id)
    }

    /// For a worker thread: what to hand its result to.
    fn sender(&self, id: u64) -> impl FnOnce(Done) + Send + 'static {
        let (done, wake) = (self.done.clone(), self.wake.clone());
        move |d| {
            lock(&done).push((id, d));
            wake.set();
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct Inner {
    lua: Lua,
    /// The tables given to `rb.widget`, by name.
    defs: RefCell<HashMap<String, Table>>,
    deadline: Rc<Cell<Option<Instant>>>,
    /// The widget whose function runs, for `rb.http` and `rb.exec` to answer.
    current: RefCell<Option<Rc<Channel>>>,
    /// A function asked for the popup to close.
    close_popup: Cell<bool>,
    palette: RefCell<HashMap<String, Rgba>>,
}

/// The Lua state and the widgets its files define; cloned by every Lua widget.
#[derive(Clone)]
pub struct Host(Rc<Inner>);

/// The Lua files of `dir\widgets`, in order of name.
pub fn files(dir: &Path) -> Vec<PathBuf> {
    files_in(&dir.join("widgets"))
}

/// The Lua files of `dir` itself, in order of name.
pub fn files_in(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lua"))).collect())
        .unwrap_or_default();
    files.sort();
    files
}

impl Host {
    /// Runs every `widgets\*.lua` of `dir`. None without any; else the host and the errors, one
    /// for each file that failed (the others are loaded all the same).
    pub fn load(dir: &Path, theme: &Theme) -> Option<(Host, Vec<String>)> {
        let files = files(dir);
        if files.is_empty() {
            return None;
        }
        let started = Instant::now();
        let host = Host(Rc::new(Inner {
            lua: Lua::new(),
            defs: RefCell::new(HashMap::new()),
            deadline: Rc::new(Cell::new(None)),
            current: RefCell::new(None),
            close_popup: Cell::new(false),
            palette: RefCell::new(HashMap::new()),
        }));
        if let Err(e) = host.install(dir) {
            return Some((host, vec![format!("Lua: {e}")]));
        }
        host.set_theme(theme);
        let mut errors = Vec::new();
        for file in &files {
            let name = file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let result = std::fs::read_to_string(file).map_err(|e| e.to_string()).and_then(|code| {
                host.guarded(|| host.0.lua.load(&code).set_name(format!("@widgets\\{name}")).exec()).map_err(|e| short(&e))
            });
            if let Err(e) = result {
                errors.push(format!("widgets\\{name}: {e}"));
            }
        }
        log!("Lua: {} files, {} widgets in {} ms", files.len(), host.0.defs.borrow().len(), started.elapsed().as_millis());
        Some((host, errors))
    }

    /// The sandbox, the time limit, `rb` and its builders.
    fn install(&self, dir: &Path) -> mlua::Result<()> {
        let lua = &self.0.lua;
        let g = lua.globals();
        // Nothing that blocks the bar, ends it or loads native code
        let os: Table = g.get("os")?;
        os.set("execute", Value::Nil)?;
        os.set("exit", Value::Nil)?;
        let io: Table = g.get("io")?;
        io.set("popen", Value::Nil)?;
        let package: Table = g.get("package")?;
        package.set("cpath", "")?;
        package.set("loadlib", Value::Nil)?;
        package.set("path", dir.join("widgets").join("lib").join("?.lua").to_string_lossy().to_string())?;
        lua.set_memory_limit(MEMORY)?;

        let deadline = self.0.deadline.clone();
        lua.set_hook(HookTriggers::new().every_nth_instruction(10_000), move |_, _| match deadline.get() {
            Some(d) if Instant::now() > d => Err(mlua::Error::runtime(format!("took too long (more than {} ms)", BUDGET.as_millis()))),
            _ => Ok(VmState::Continue),
        })?;

        let rb = lua.create_table()?;
        let weak = Rc::downgrade(&self.0);
        rb.set(
            "widget",
            lua.create_function(move |_, def: Table| {
                let name: String = def.get("name").map_err(|_| mlua::Error::runtime("rb.widget needs a name"))?;
                if let Some(inner) = weak.upgrade()
                    && inner.defs.borrow_mut().insert(name.clone(), def).is_some()
                {
                    log!("Lua: two widgets called {name:?}: the last one counts");
                }
                Ok(())
            })?,
        )?;
        // The bar draws again after every call anyway: kept for the scripts of the macOS version
        rb.set("redraw", lua.create_function(|_, ()| Ok(()))?)?;
        rb.set(
            "open",
            lua.create_function(|_, target: String| {
                crate::process::open(&target);
                Ok(())
            })?,
        )?;
        let weak = Rc::downgrade(&self.0);
        rb.set(
            "close_popup",
            lua.create_function(move |_, ()| {
                if let Some(inner) = weak.upgrade() {
                    inner.close_popup.set(true);
                }
                Ok(())
            })?,
        )?;
        rb.set(
            "trigger",
            lua.create_function(|_, (name, args): (String, Option<Table>)| {
                let args = args.map(|a| strings(&a)).unwrap_or_default();
                crate::app::push(crate::app::Event::Trigger { name, args });
                Ok(())
            })?,
        )?;
        let weak = Rc::downgrade(&self.0);
        rb.set(
            "color",
            lua.create_function(move |_, name: String| {
                let Some(inner) = weak.upgrade() else { return Ok(None) };
                Ok(color_in(&inner.palette.borrow(), &name).map(hex))
            })?,
        )?;
        rb.set("t", lua.create_function(|_, (it, en): (String, String)| Ok(if crate::i18n::italian() { it } else { en }))?)?;
        // For what rb.http brings: Lua has no JSON of its own
        rb.set(
            "json",
            lua.create_function(|lua, text: String| match crate::json::Json::parse(&text) {
                Some(json) => from_json(lua, &json),
                None => Ok(Value::Nil),
            })?,
        )?;
        let log_fn = lua.create_function(|_, args: mlua::Variadic<Value>| {
            let parts: Vec<String> = args.iter().map(display).collect();
            log!("lua: {}", parts.join(" "));
            Ok(())
        })?;
        rb.set("log", log_fn.clone())?;
        g.set("print", log_fn)?;

        let weak = Rc::downgrade(&self.0);
        rb.set(
            "http",
            lua.create_function(move |_, (url, a, b): (String, Value, Value)| {
                let (options, callback) = match (a, b) {
                    (Value::Function(f), _) => (None, f),
                    (Value::Table(t), Value::Function(f)) => (Some(t), f),
                    _ => return Err(mlua::Error::runtime("rb.http(url, [options], function(body, status, error) end)")),
                };
                let inner = weak.upgrade().ok_or_else(|| mlua::Error::runtime("the bar is reloading"))?;
                let channel = inner.current.borrow().clone().ok_or_else(|| mlua::Error::runtime("rb.http works inside a widget's functions"))?;
                let text = |k: &str| options.as_ref().and_then(|o| o.get::<Option<String>>(k).ok().flatten());
                let method = text("method").unwrap_or_else(|| "GET".into()).to_uppercase();
                let body = text("body");
                let headers: Vec<(String, String)> = options.as_ref().and_then(|o| o.get::<Option<Table>>("headers").ok().flatten()).map(|h| strings(&h)).unwrap_or_default();
                let timeout = options.as_ref().and_then(|o| o.get::<Option<f64>>("timeout").ok().flatten()).unwrap_or(15.0).clamp(1.0, 60.0);
                let send = channel.sender(channel.register(callback)?);
                spawn("lua-http", move || {
                    let headers: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
                    let done = match crate::net::request(&method, &url, &headers, body.as_deref().map(str::as_bytes), (timeout * 1000.0) as i32) {
                        Ok(r) => Done::Http { status: r.status, body: String::from_utf8_lossy(&r.body).into_owned(), error: None },
                        Err(e) => Done::Http { status: 0, body: String::new(), error: Some(e) },
                    };
                    send(done);
                });
                Ok(())
            })?,
        )?;
        let weak = Rc::downgrade(&self.0);
        rb.set(
            "exec",
            lua.create_function(move |_, (cmd, callback): (String, Option<Function>)| {
                let inner = weak.upgrade().ok_or_else(|| mlua::Error::runtime("the bar is reloading"))?;
                let channel = inner.current.borrow().clone().ok_or_else(|| mlua::Error::runtime("rb.exec works inside a widget's functions"))?;
                // Without a callback, the answer goes nowhere
                let callback = match callback {
                    Some(f) => f,
                    None => inner.lua.create_function(|_, _: mlua::MultiValue| Ok(()))?,
                };
                let send = channel.sender(channel.register(callback)?);
                spawn("lua-exec", move || {
                    use std::os::windows::process::CommandExt;
                    let mut command = std::process::Command::new("cmd.exe");
                    command.raw_arg(format!("/D /S /C \"{cmd}\""));
                    let out = crate::process::run(command, None, 30, "rb.exec");
                    send(Done::Exec { code: out.code, stdout: out.stdout, stderr: out.stderr });
                });
                Ok(())
            })?,
        )?;

        let loaded: Table = package.get("loaded")?;
        loaded.set("rustybar", rb.clone())?;
        g.set("rb", rb)?;
        lua.load(PRELUDE).set_name("@rustybar").exec()
    }

    pub fn lua(&self) -> &Lua {
        &self.0.lua
    }

    /// The table of the widget called `name`.
    pub fn def(&self, name: &str) -> Option<Table> {
        self.0.defs.borrow().get(name).cloned()
    }

    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.0.defs.borrow().keys().cloned().collect();
        names.sort();
        names
    }

    /// The colors and sizes the scripts see: `rb.theme`, and those `rb.color` knows.
    pub fn set_theme(&self, theme: &Theme) {
        *self.0.palette.borrow_mut() = theme.palette.clone();
        let lua = &self.0.lua;
        let made = (|| -> mlua::Result<()> {
            let t = lua.create_table()?;
            t.set("font", theme.font.as_str())?;
            t.set("font_size", theme.font_size)?;
            t.set("small_size", theme.small_size)?;
            let b = theme.base;
            t.set("dark", 0.2126 * b.0 + 0.7152 * b.1 + 0.0722 * b.2 < 0.5)?;
            let colors = lua.create_table()?;
            for (name, c) in &theme.palette {
                colors.set(name.as_str(), hex(*c))?;
            }
            for (name, c) in [("primary", theme.primary), ("on_primary", theme.on_primary), ("text", theme.text), ("subtext", theme.subtext)] {
                colors.set(name, hex(c))?;
            }
            t.set("colors", colors)?;
            let rb: Table = lua.globals().get("rb")?;
            rb.set("theme", t)
        })();
        if let Err(e) = made {
            log!("Lua: rb.theme: {e}");
        }
    }

    /// Runs `f` as a call of the widget whose background work comes back to `channel`: at most
    /// `BUDGET`, after which it stops with an error.
    pub fn call<R>(&self, channel: &Rc<Channel>, f: impl FnOnce() -> mlua::Result<R>) -> mlua::Result<R> {
        let previous = self.0.current.replace(Some(channel.clone()));
        let result = self.guarded(f);
        *self.0.current.borrow_mut() = previous;
        result
    }

    fn guarded<R>(&self, f: impl FnOnce() -> mlua::Result<R>) -> mlua::Result<R> {
        // A call inside a call keeps the outer deadline
        let outer = self.0.deadline.get();
        if outer.is_none() {
            self.0.deadline.set(Some(Instant::now() + BUDGET));
        }
        let result = f();
        self.0.deadline.set(outer);
        result
    }

    /// Whether a function asked for the popup to close since the last time.
    pub fn take_close(&self) -> bool {
        self.0.close_popup.replace(false)
    }

    /// A full collection, for when a popup has gone: its tables won't be needed again soon.
    pub fn collect(&self) {
        let _ = self.0.lua.gc_collect();
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    if let Err(e) = std::thread::Builder::new().name(name.into()).stack_size(256 * 1024).spawn(f) {
        log!("{name}: no worker thread ({e})");
    }
}

/// The string keys and values of a table, as `key=value` pairs (numbers and booleans as text).
pub fn strings(t: &Table) -> Vec<(String, String)> {
    t.pairs::<Value, Value>()
        .flatten()
        .filter_map(|(k, v)| {
            let k = match k {
                Value::String(s) => s.to_string_lossy(),
                Value::Integer(i) => i.to_string(),
                _ => return None,
            };
            matches!(v, Value::String(_) | Value::Integer(_) | Value::Number(_) | Value::Boolean(_)).then(|| (k, display(&v)))
        })
        .collect()
}

/// A value as `print` shows it.
pub fn display(v: &Value) -> String {
    match v {
        Value::Nil => "nil".into(),
        Value::Boolean(b) => b.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.to_string_lossy(),
        other => format!("<{}>", other.type_name()),
    }
}

/// An error in one line, without the traceback.
pub fn short(e: &mlua::Error) -> String {
    let text = match e {
        mlua::Error::CallbackError { cause, .. } => return short(cause),
        mlua::Error::RuntimeError(m) | mlua::Error::SyntaxError { message: m, .. } => m.clone(),
        mlua::Error::MemoryError(_) => format!("out of memory (more than {} MB)", MEMORY / (1024 * 1024)),
        other => other.to_string(),
    };
    text.lines().next().unwrap_or_default().trim().to_string()
}

/// A palette name (`"green"`), with an opacity after `@` (`"green@0.5"`), or a hex color.
pub fn color(theme: &Theme, s: &str) -> Option<Rgba> {
    match s {
        "primary" => Some(theme.primary),
        "on_primary" => Some(theme.on_primary),
        "text" => Some(theme.text),
        "subtext" => Some(theme.subtext),
        _ => None,
    }
    .or_else(|| color_in(&theme.palette, s))
    .or_else(|| {
        let (name, alpha) = s.split_once('@')?;
        Some(color(theme, name)?.alpha(alpha.trim().parse::<f32>().ok()?.clamp(0.0, 1.0)))
    })
}

fn color_in(palette: &HashMap<String, Rgba>, s: &str) -> Option<Rgba> {
    if let Some((name, alpha)) = s.split_once('@') {
        return Some(color_in(palette, name)?.alpha(alpha.trim().parse::<f32>().ok()?.clamp(0.0, 1.0)));
    }
    if let Some(hex) = s.trim().strip_prefix('#') {
        let v = u32::from_str_radix(hex, 16).ok()?;
        let c = |shift: u32| ((v >> shift) & 0xff) as f32 / 255.0;
        return match hex.len() {
            6 => Some(Rgba(c(16), c(8), c(0), 1.0)),
            8 => Some(Rgba(c(24), c(16), c(8), c(0))),
            _ => None,
        };
    }
    palette.get(s.trim()).copied()
}

/// `#rrggbb`, or `#rrggbbaa` when it's see-through.
pub fn hex(c: Rgba) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    if c.3 < 1.0 { format!("#{:02x}{:02x}{:02x}{:02x}", b(c.0), b(c.1), b(c.2), b(c.3)) } else { format!("#{:02x}{:02x}{:02x}", b(c.0), b(c.1), b(c.2)) }
}

/// A TOML value of the config as a Lua value: the options of a widget.
pub fn from_toml(lua: &Lua, v: &toml::Value) -> mlua::Result<Value> {
    Ok(match v {
        toml::Value::String(s) => Value::String(lua.create_string(s)?),
        toml::Value::Integer(i) => Value::Integer(*i),
        toml::Value::Float(f) => Value::Number(*f),
        toml::Value::Boolean(b) => Value::Boolean(*b),
        toml::Value::Datetime(d) => Value::String(lua.create_string(d.to_string())?),
        toml::Value::Array(a) => {
            let t = lua.create_table()?;
            for (i, item) in a.iter().enumerate() {
                t.set(i + 1, from_toml(lua, item)?)?;
            }
            Value::Table(t)
        }
        toml::Value::Table(m) => {
            let t = lua.create_table()?;
            for (k, item) in m {
                t.set(k.as_str(), from_toml(lua, item)?)?;
            }
            Value::Table(t)
        }
    })
}

/// JSON as Lua values: objects and arrays as tables (arrays from 1), `null` as nil.
fn from_json(lua: &Lua, v: &crate::json::Json) -> mlua::Result<Value> {
    use crate::json::Json;
    Ok(match v {
        Json::Null => Value::Nil,
        Json::Bool(b) => Value::Boolean(*b),
        Json::Number(n) if n.fract() == 0.0 && n.abs() < 9.0e15 => Value::Integer(*n as i64),
        Json::Number(n) => Value::Number(*n),
        Json::String(s) => Value::String(lua.create_string(s)?),
        Json::Array(a) => {
            let t = lua.create_table()?;
            for (i, item) in a.iter().enumerate() {
                t.set(i + 1, from_json(lua, item)?)?;
            }
            Value::Table(t)
        }
        Json::Object(m) => {
            let t = lua.create_table()?;
            for (k, item) in m {
                t.set(k.as_str(), from_json(lua, item)?)?;
            }
            Value::Table(t)
        }
    })
}

/// `rustybar check`: the Lua files of `dir` load, and the names they define. None without files.
pub fn check(dir: &Path, theme: &Theme) -> Option<(Vec<String>, Vec<String>)> {
    let (host, errors) = Host::load(dir, theme)?;
    Some((host.names(), errors))
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A config dir of its own with `widgets\<name>.lua` holding `code`, for a test.
    pub fn dir_with(test: &str, files: &[(&str, &str)]) -> PathBuf {
        // The same dir at every run, emptied first: nothing piles up in the temporary files
        let dir = std::env::temp_dir().join(format!("rustybar-test-{test}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("widgets")).unwrap();
        for (name, code) in files {
            std::fs::write(dir.join("widgets").join(name), code).unwrap();
        }
        dir
    }

    pub fn theme() -> Theme {
        Theme::from_config(&crate::config::Config::default().theme)
    }

    #[test]
    fn no_files_no_lua() {
        let dir = dir_with("none", &[]);
        assert!(Host::load(&dir, &theme()).is_none());
    }

    #[test]
    fn the_sandbox_holds() {
        let dir = dir_with("sandbox", &[("a.lua", r#"
            assert(os.execute == nil and os.exit == nil and io.popen == nil and package.loadlib == nil)
            assert(package.cpath == "")
            local rb = require("rustybar")
            assert(rb == _G.rb and rb.ui.pill and rb.ui.divider)
            rb.widget { name = "a" }
            rb.widget { name = "b" }
        "#), ("broken.lua", "this is not lua")]);
        let (host, errors) = Host::load(&dir, &theme()).unwrap();
        assert_eq!(host.names(), ["a", "b"]);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].starts_with(r"widgets\broken.lua:"), "{errors:?}");
    }

    #[test]
    fn a_call_that_takes_too_long_stops() {
        let dir = dir_with("budget", &[("a.lua", "rb.widget { name = 'a', spin = function() while true do end end }")]);
        let (host, _) = Host::load(&dir, &theme()).unwrap();
        let spin: Function = host.def("a").unwrap().get("spin").unwrap();
        let started = Instant::now();
        let e = host.call(&Channel::new(), || spin.call::<()>(())).unwrap_err();
        assert!(short(&e).contains("took too long"), "{e}");
        assert!(started.elapsed() < BUDGET * 3, "{:?}", started.elapsed());
        // And the state is still good for the next call
        assert!(host.call(&Channel::new(), || host.lua().load("return 1 + 1").eval::<i64>()).unwrap() == 2);
    }

    #[test]
    fn memory_has_a_ceiling() {
        let dir = dir_with("memory", &[("a.lua", "rb.widget { name = 'a', hog = function() local t = {} for i = 1, 1e8 do t[i] = ('x'):rep(64) .. i end end }")]);
        let (host, _) = Host::load(&dir, &theme()).unwrap();
        let hog: Function = host.def("a").unwrap().get("hog").unwrap();
        let e = host.call(&Channel::new(), || hog.call::<()>(())).unwrap_err();
        let e = short(&e);
        assert!(e.contains("out of memory") || e.contains("took too long"), "{e}");
        host.collect();
        assert!(host.lua().used_memory() < MEMORY / 2);
    }

    #[test]
    fn colors_by_name_with_opacity_or_hex() {
        let t = theme();
        assert_eq!(color(&t, "primary"), Some(t.primary));
        assert_eq!(color(&t, "green"), t.color("green"));
        assert_eq!(color(&t, "green@0.5"), t.color("green").map(|c| c.alpha(0.5)));
        assert_eq!(color(&t, "#ff0000"), Some(Rgba(1.0, 0.0, 0.0, 1.0)));
        assert_eq!(color(&t, "nope"), None);
        assert_eq!(hex(Rgba(1.0, 0.5, 0.0, 1.0)), "#ff8000");
        assert_eq!(hex(Rgba(1.0, 0.0, 0.0, 0.5)), "#ff000080");
    }

    #[test]
    fn options_reach_lua() {
        let lua = Lua::new();
        let toml: toml::Table = toml::from_str("city = \"Rome\"\ndays = [1, 2]\n[units]\ntemp = \"C\"").unwrap();
        let v = from_toml(&lua, &toml::Value::Table(toml)).unwrap();
        lua.globals().set("o", v).unwrap();
        assert!(lua.load("return o.city == 'Rome' and o.days[2] == 2 and o.units.temp == 'C'").eval::<bool>().unwrap());
    }
}
