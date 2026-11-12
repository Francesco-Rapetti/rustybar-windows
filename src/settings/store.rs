//! The two files the settings window edits, `rustybar.toml` and `rustybar.local.toml`, with
//! toml_edit, which keeps their comments and layout: only the value that changes is written again.
//! Each edit reads the file again first if it changed on disk, so an edit made elsewhere while the
//! window is open isn't lost, and writes it in place: the bar's watcher sees it and the bar
//! reloads by itself.
//!
//! The keys are read from `rustybar.local.toml` over `rustybar.toml`, as the bar merges them, and
//! written where they are: a key, or a table, of the local file stays there; anything new goes to
//! `rustybar.toml`. A key set back to its default goes away, so that the file keeps only what
//! differs. A file that doesn't parse is shown and never written until it's fixed.
//!
//! With made-up data (`RUSTYBAR_MOCK`, see `mock`) the window edits the copies in
//! `<mock>/settings/`, which no bar reads.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

pub use toml_edit::Value;
use toml_edit::{ArrayOfTables, DocumentMut, InlineTable, Item, Table};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum File {
    Main,
    Local,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seg {
    K(String),
    I(usize),
}

/// A place in the config: `widgets.home.groups[2].title`.
#[derive(Clone, Debug, PartialEq)]
pub struct Spot {
    pub path: Vec<Seg>,
}

impl Spot {
    /// Keys split at the dots: for names without one.
    pub fn new(path: &str) -> Spot {
        Spot { path: path.split('.').filter(|s| !s.is_empty()).map(|s| Seg::K(s.to_string())).collect() }
    }

    pub fn k(&self, key: &str) -> Spot {
        let mut s = self.clone();
        s.path.push(Seg::K(key.to_string()));
        s
    }

    pub fn i(&self, index: usize) -> Spot {
        let mut s = self.clone();
        s.path.push(Seg::I(index));
        s
    }
}

/// `rb("bar.height")`: a spot by its dotted path.
pub fn rb(path: &str) -> Spot {
    Spot::new(path)
}

/// The folder of made-up configs with `RUSTYBAR_MOCK` (see `mock`): the window edits those, and
/// applies nothing.
pub fn mock_dir() -> Option<PathBuf> {
    crate::mock::dir().map(|d| d.join("settings"))
}

#[cfg(test)]
thread_local! {
    /// The folder of the files, for a test: never the real config.
    static TEST_DIR: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

pub fn path(file: File) -> PathBuf {
    #[cfg(test)]
    let dir = TEST_DIR.with(|d| d.borrow().clone()).or_else(mock_dir);
    #[cfg(not(test))]
    let dir = mock_dir();
    match (dir, file) {
        (Some(d), File::Main) => d.join("rustybar.toml"),
        (Some(d), File::Local) => d.join("rustybar.local.toml"),
        (None, File::Main) => crate::config::file(),
        (None, File::Local) => crate::config::local_file(),
    }
}

struct Doc {
    path: PathBuf,
    text: String,
    doc: DocumentMut,
    mtime: Option<SystemTime>,
    /// A file that doesn't parse is never written.
    error: Option<String>,
}

#[derive(Default)]
struct Store {
    docs: HashMap<File, Doc>,
}

thread_local! {
    static STORE: RefCell<Store> = RefCell::new(Store::default());
    /// What the last edit couldn't do, for the window to show.
    static PROBLEM: RefCell<Option<String>> = const { RefCell::new(None) };
    /// A file was written since the window last asked.
    static WRITTEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn with<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    STORE.with(|s| f(&mut s.borrow_mut()))
}

/// What the last edit couldn't do, once.
pub fn take_problem() -> Option<String> {
    PROBLEM.with(|p| p.borrow_mut().take())
}

/// Whether a file was written since the last call.
pub fn take_written() -> bool {
    WRITTEN.with(|w| w.replace(false))
}

impl Store {
    fn load(&mut self, file: File) -> &mut Doc {
        let path = path(file);
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let stale = self.docs.get(&file).is_none_or(|d| d.mtime != mtime || d.path != path);
        if stale {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let (doc, error) = match text.parse::<DocumentMut>() {
                Ok(d) => (d, None),
                Err(e) => (DocumentMut::new(), Some(format!("{}: {}", path.display(), e.to_string().trim()))),
            };
            self.docs.insert(file, Doc { path, text, doc, mtime, error });
        }
        self.docs.get_mut(&file).expect("loaded")
    }

    fn find(&mut self, file: File, path: &[Seg]) -> Option<toml::Value> {
        find(self.load(file).doc.as_table(), path).and_then(owned)
    }

    fn exists(&mut self, file: File, path: &[Seg]) -> bool {
        find(self.load(file).doc.as_table(), path).is_some()
    }

    /// The file an edit of `spot` goes to (see the top of the module).
    fn target(&mut self, spot: &Spot) -> File {
        let p = &spot.path;
        if self.exists(File::Local, p) {
            return File::Local;
        }
        if self.exists(File::Main, p) {
            return File::Main;
        }
        // A table of the local file, such as [widgets.home], keeps what's new in it
        for n in (2..p.len()).rev() {
            if self.exists(File::Local, &p[..n]) {
                return File::Local;
            }
        }
        File::Main
    }

    fn edit<R>(&mut self, spot: &Spot, f: impl FnOnce(&mut DocumentMut, usize) -> R) -> Result<R, String> {
        let file = self.target(spot);
        let headers = headers(spot);
        let d = self.load(file);
        if let Some(e) = &d.error {
            return Err(e.clone());
        }
        let r = f(&mut d.doc, headers);
        let text = d.doc.to_string();
        if text != d.text {
            if let Some(dir) = d.path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            write_in_place(&d.path, &text).map_err(|e| format!("{}: {e}", d.path.display()))?;
            d.text = text;
            d.mtime = std::fs::metadata(&d.path).and_then(|m| m.modified()).ok();
            WRITTEN.with(|w| w.set(true));
        }
        Ok(r)
    }
}

/// Writes `text` over the file itself, never through an empty one: the bar may read it at any
/// moment (it waits for the writes to settle, see `app`). The same file, not a new one renamed
/// over it, so a link to a dotfiles repo stays a link.
fn write_in_place(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(false).open(path)?;
    file.write_all(text.as_bytes())?;
    file.set_len(text.len() as u64)?;
    file.flush()
}

/// How many of the first keys of a path are `[headers]` when they have to be made; deeper ones are
/// dotted keys.
fn headers(spot: &Spot) -> usize {
    let key = |i: usize| match spot.path.get(i) {
        Some(Seg::K(k)) => k.as_str(),
        _ => "",
    };
    match key(0) {
        "widgets" if key(2) == "printer" => 3,
        "widgets" => 2,
        "theme" if key(1) == "colors" => 2,
        _ => 1,
    }
}

// ---- reading -----------------------------------------------------------------------------------

/// A step through the document: tables hold items, arrays and inline tables hold values.
enum Got<'a> {
    I(&'a Item),
    V(&'a Value),
    T(&'a Table),
}

fn step<'a>(g: Got<'a>, seg: &Seg) -> Option<Got<'a>> {
    match (g, seg) {
        (Got::I(Item::Table(t)), _) => step(Got::T(t), seg),
        (Got::I(Item::Value(v)), _) => step(Got::V(v), seg),
        (Got::I(Item::ArrayOfTables(a)), Seg::I(i)) => a.get(*i).map(Got::T),
        (Got::T(t), Seg::K(k)) => t.get(k).filter(|i| !i.is_none()).map(Got::I),
        (Got::V(Value::InlineTable(t)), Seg::K(k)) => t.get(k).map(Got::V),
        (Got::V(Value::Array(a)), Seg::I(i)) => a.get(*i).map(Got::V),
        _ => None,
    }
}

fn find<'a>(root: &'a Table, path: &[Seg]) -> Option<Got<'a>> {
    let mut g = Got::T(root);
    for seg in path {
        g = step(g, seg)?;
    }
    Some(g)
}

fn owned(g: Got) -> Option<toml::Value> {
    match g {
        Got::I(i) => owned_item(i),
        Got::V(v) => Some(owned_value(v)),
        Got::T(t) => Some(owned_table(t)),
    }
}

fn owned_item(i: &Item) -> Option<toml::Value> {
    match i {
        Item::None => None,
        Item::Value(v) => Some(owned_value(v)),
        Item::Table(t) => Some(owned_table(t)),
        Item::ArrayOfTables(a) => Some(toml::Value::Array(a.iter().map(owned_table).collect())),
    }
}

fn owned_table(t: &Table) -> toml::Value {
    toml::Value::Table(t.iter().filter_map(|(k, i)| owned_item(i).map(|v| (k.to_string(), v))).collect())
}

fn owned_value(v: &Value) -> toml::Value {
    match v {
        Value::String(s) => toml::Value::String(s.value().clone()),
        Value::Integer(i) => toml::Value::Integer(*i.value()),
        Value::Float(f) => toml::Value::Float(*f.value()),
        Value::Boolean(b) => toml::Value::Boolean(*b.value()),
        Value::Datetime(d) => toml::Value::String(d.value().to_string()),
        Value::Array(a) => toml::Value::Array(a.iter().map(owned_value).collect()),
        Value::InlineTable(t) => toml::Value::Table(t.iter().map(|(k, v)| (k.to_string(), owned_value(v))).collect()),
    }
}

/// The value at `spot`, from the local file over the main one, tables merged as the bar merges
/// them.
pub fn get(spot: &Spot) -> Option<toml::Value> {
    with(|s| {
        let main = s.find(File::Main, &spot.path);
        let local = s.find(File::Local, &spot.path);
        match (main, local) {
            (Some(toml::Value::Table(mut m)), Some(toml::Value::Table(l))) => {
                crate::config::merge(&mut m, l);
                Some(toml::Value::Table(m))
            }
            (m, l) => l.or(m),
        }
    })
}

pub fn get_str(spot: &Spot) -> Option<String> {
    get(spot).and_then(|v| v.as_str().map(String::from))
}

pub fn get_f64(spot: &Spot) -> Option<f64> {
    get(spot).and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
}

pub fn get_bool(spot: &Spot) -> Option<bool> {
    get(spot).and_then(|v| v.as_bool())
}

/// A list of strings; a single string counts as a list of one.
pub fn get_strings(spot: &Spot) -> Option<Vec<String>> {
    match get(spot)? {
        toml::Value::String(s) => Some(vec![s]),
        toml::Value::Array(a) => Some(a.iter().filter_map(|v| v.as_str().map(String::from)).collect()),
        _ => None,
    }
}

/// The keys of a table, in the files' order (the main one first).
pub fn keys(spot: &Spot) -> Vec<String> {
    with(|s| {
        let mut out: Vec<String> = Vec::new();
        for f in [File::Main, File::Local] {
            let keys: Vec<String> = match find(s.load(f).doc.as_table(), &spot.path) {
                Some(Got::I(Item::Table(t))) | Some(Got::T(t)) => t.iter().filter(|(_, i)| !i.is_none()).map(|(k, _)| k.to_string()).collect(),
                Some(Got::I(Item::Value(Value::InlineTable(t)))) | Some(Got::V(Value::InlineTable(t))) => t.iter().map(|(k, _)| k.to_string()).collect(),
                _ => Vec::new(),
            };
            for k in keys {
                if !out.contains(&k) {
                    out.push(k);
                }
            }
        }
        out
    })
}

/// The errors of the files that don't parse, which the window won't write.
pub fn errors() -> Vec<String> {
    with(|s| [File::Main, File::Local].iter().filter_map(|&f| s.load(f).error.clone()).collect())
}


// ---- writing -----------------------------------------------------------------------------------

enum GotMut<'a> {
    I(&'a mut Item),
    V(&'a mut Value),
    T(&'a mut Table),
}

/// One step down; with `make`, a missing table is made: a `[header]` while `header`, else dotted
/// (or inline, inside an inline table).
fn step_mut<'a>(g: GotMut<'a>, seg: &Seg, make: bool, header: bool) -> Option<GotMut<'a>> {
    match g {
        GotMut::I(Item::Table(t)) => step_mut(GotMut::T(t), seg, make, header),
        GotMut::I(Item::Value(v)) => step_mut(GotMut::V(v), seg, make, header),
        GotMut::I(Item::ArrayOfTables(a)) => match seg {
            Seg::I(i) => a.get_mut(*i).map(GotMut::T),
            _ => None,
        },
        GotMut::I(Item::None) => None,
        GotMut::T(t) => {
            let Seg::K(k) = seg else { return None };
            if t.get(k).is_none_or(Item::is_none) {
                if !make {
                    return None;
                }
                let mut new = Table::new();
                if header {
                    new.set_implicit(true);
                } else {
                    new.set_dotted(true);
                }
                t.insert(k, Item::Table(new));
            }
            t.get_mut(k).map(GotMut::I)
        }
        GotMut::V(Value::InlineTable(t)) => {
            let Seg::K(k) = seg else { return None };
            if !t.contains_key(k) {
                if !make {
                    return None;
                }
                t.insert(k, Value::InlineTable(InlineTable::new()));
            }
            t.get_mut(k).map(GotMut::V)
        }
        GotMut::V(Value::Array(a)) => match seg {
            Seg::I(i) => a.get_mut(*i).map(GotMut::V),
            _ => None,
        },
        GotMut::V(_) => None,
    }
}

fn find_mut<'a>(doc: &'a mut DocumentMut, path: &[Seg], make: bool, headers: usize) -> Option<GotMut<'a>> {
    let mut g = GotMut::T(doc.as_table_mut());
    for (n, seg) in path.iter().enumerate() {
        g = step_mut(g, seg, make, n < headers)?;
    }
    Some(g)
}

/// Puts `v` in place of `slot`, keeping its comments and spacing.
fn replace(slot: &mut Value, mut v: Value) {
    // A 'literal' string stays one, when the new text can be written that way
    if let (Value::String(old), Value::String(new)) = (&*slot, &v) {
        let literal = old.as_repr().and_then(|r| r.as_raw().as_str()).is_some_and(|r| r.starts_with('\'') && !r.starts_with("'''"));
        if literal
            && !new.value().contains('\'')
            && let Ok(l) = format!("'{}'", new.value()).parse::<Value>()
        {
            v = l;
        }
    }
    *v.decor_mut() = slot.decor().clone();
    *slot = v;
}

fn set_in(doc: &mut DocumentMut, path: &[Seg], headers: usize, v: Value) {
    let Some((last, parent)) = path.split_last() else { return };
    let Some(g) = find_mut(doc, parent, true, headers) else { return };
    match (g, last) {
        (GotMut::I(Item::Table(t)) | GotMut::T(t), Seg::K(k)) => match t.get_mut(k) {
            Some(Item::Value(old)) => replace(old, v),
            _ => {
                t.insert(k, Item::Value(v));
            }
        },
        (GotMut::I(Item::Value(Value::InlineTable(t))) | GotMut::V(Value::InlineTable(t)), Seg::K(k)) => match t.get_mut(k) {
            Some(old) => replace(old, v),
            None => {
                t.insert(k, v);
                // The spaces of `{ a = 1 }` around the new key
                t.fmt();
            }
        },
        (GotMut::I(Item::Value(Value::Array(a))) | GotMut::V(Value::Array(a)), Seg::I(i)) => {
            if let Some(old) = a.get_mut(*i) {
                replace(old, v);
            }
        }
        _ => {}
    }
}

/// Removes the value at `path`, then the tables it leaves empty (unless they have a comment).
fn remove_in(doc: &mut DocumentMut, path: &[Seg]) {
    for n in (1..=path.len()).rev() {
        let (last, parent) = path[..n].split_last().expect("not empty");
        if n < path.len() {
            // Only an empty table goes, and never an element of an array
            let Seg::K(_) = last else { return };
            let empty = match find_mut(doc, &path[..n], false, 0) {
                Some(GotMut::I(Item::Table(t))) => t.is_empty() && !t.decor().prefix().and_then(|p| p.as_str()).unwrap_or("").contains('#'),
                Some(GotMut::I(Item::Value(Value::InlineTable(t))) | GotMut::V(Value::InlineTable(t))) => t.is_empty(),
                _ => false,
            };
            if !empty {
                return;
            }
        }
        match (find_mut(doc, parent, false, 0), last) {
            (Some(GotMut::I(Item::Table(t)) | GotMut::T(t)), Seg::K(k)) => {
                t.remove(k);
            }
            (Some(GotMut::I(Item::Value(Value::InlineTable(t))) | GotMut::V(Value::InlineTable(t))), Seg::K(k)) => {
                t.remove(k);
                t.fmt();
            }
            (Some(GotMut::I(Item::Value(Value::Array(a))) | GotMut::V(Value::Array(a))), Seg::I(i)) if *i < a.len() => {
                a.remove(*i);
                tidy(a);
            }
            (Some(GotMut::I(Item::ArrayOfTables(a))), Seg::I(i)) if *i < a.len() => {
                a.remove(*i);
            }
            _ => return,
        }
    }
}

/// An array on one line is written again as one after an element goes, so that no stray space or
/// comma stays behind.
fn tidy(a: &mut toml_edit::Array) {
    if !a.to_string().contains('\n') {
        a.fmt();
    }
}

fn report<R>(r: Result<R, String>) -> Option<R> {
    match r {
        Ok(v) => Some(v),
        Err(e) => {
            PROBLEM.with(|p| *p.borrow_mut() = Some(e));
            None
        }
    }
}

/// Writes `v` at `spot`, making the tables on the way.
pub fn set(spot: &Spot, v: Value) {
    report(with(|s| s.edit(spot, |doc, h| set_in(doc, &spot.path, h, v))));
}

/// Writes `v` at `spot`, or removes it when it's the default and no other file has it, so that the
/// file keeps only what differs.
pub fn put(spot: &Spot, v: Value, default: Option<&toml::Value>) {
    if let Some(d) = default
        && same(&owned_value(&v), d)
    {
        let other = with(|s| {
            let file = s.target(spot);
            let other = if file == File::Main { File::Local } else { File::Main };
            s.exists(other, &spot.path)
        });
        if !other {
            remove(spot);
            return;
        }
    }
    set(spot, v);
}

fn same(a: &toml::Value, b: &toml::Value) -> bool {
    let num = |v: &toml::Value| v.as_float().or_else(|| v.as_integer().map(|i| i as f64));
    match (num(a), num(b)) {
        (Some(x), Some(y)) => (x - y).abs() < 1e-9,
        _ => a == b,
    }
}

pub fn remove(spot: &Spot) {
    report(with(|s| s.edit(spot, |doc, _| remove_in(doc, &spot.path))));
}

/// Adds `v` at the end of the array at `spot`, which is made if missing.
pub fn push(spot: &Spot, v: Value) {
    report(with(|s| {
        s.edit(spot, |doc, h| {
            if find_mut(doc, &spot.path, false, 0).is_none() {
                set_in(doc, &spot.path, h, Value::Array(toml_edit::Array::new()));
            }
            if let Some(GotMut::I(Item::Value(Value::Array(a))) | GotMut::V(Value::Array(a))) = find_mut(doc, &spot.path, false, 0) {
                // A list written one per line gets one more line
                let multiline = a.iter().last().map(|l| l.decor().prefix().and_then(|p| p.as_str()).unwrap_or("").contains('\n'));
                let mut v = v;
                if multiline == Some(true) {
                    let last = a.iter().last().expect("not empty");
                    *v.decor_mut() = last.decor().clone();
                }
                a.push_formatted(v);
            }
        })
    }));
}

/// Swaps elements `a` and `b` of the array, or array of tables (`[[…]]`), at `spot`: each place in
/// the file keeps its spacing and comments, only what's there moves.
pub fn swap(spot: &Spot, a: usize, b: usize) {
    if a == b {
        return;
    }
    report(with(|s| {
        s.edit(spot, |doc, _| swap_in(doc, &spot.path, a, b))
    }));
}

fn swap_in(doc: &mut DocumentMut, path: &[Seg], a: usize, b: usize) {
    match find_mut(doc, path, false, 0) {
        Some(GotMut::I(Item::Value(Value::Array(arr))) | GotMut::V(Value::Array(arr))) if a.max(b) < arr.len() => {
            let (mut va, mut vb) = (arr.get(a).cloned().expect("checked"), arr.get(b).cloned().expect("checked"));
            let (da, db) = (va.decor().clone(), vb.decor().clone());
            *va.decor_mut() = db;
            *vb.decor_mut() = da;
            arr.replace_formatted(a, vb);
            arr.replace_formatted(b, va);
        }
        Some(GotMut::I(Item::ArrayOfTables(arr))) if a.max(b) < arr.len() => {
            let (mut ta, mut tb) = (arr.get(a).cloned().expect("checked"), arr.get(b).cloned().expect("checked"));
            let (pa, pb) = (ta.position(), tb.position());
            let (da, db) = (ta.decor().clone(), tb.decor().clone());
            if let (Some(pa), Some(pb)) = (pa, pb) {
                ta.set_position(Some(pb));
                tb.set_position(Some(pa));
            }
            *ta.decor_mut() = db;
            *tb.decor_mut() = da;
            *arr.get_mut(a).expect("checked") = tb;
            *arr.get_mut(b).expect("checked") = ta;
        }
        _ => {}
    }
}

/// Adds a table at the end of the array of tables at `spot` (`[[…]]`), which is made if missing.
pub fn push_table(spot: &Spot, t: Table) {
    report(with(|s| {
        s.edit(spot, |doc, h| {
            let Some((Seg::K(k), parent)) = spot.path.split_last() else { return };
            let Some(GotMut::I(Item::Table(p)) | GotMut::T(p)) = find_mut(doc, parent, true, h) else { return };
            // Written in line (`groups = [{ … }]`): one more in line, as the others
            if let Some(Item::Value(Value::Array(a))) = p.get_mut(k) {
                let mut inline = t.into_inline_table();
                inline.fmt();
                a.push(Value::InlineTable(inline));
                return;
            }
            if !matches!(p.get(k), Some(Item::ArrayOfTables(_))) {
                p.insert(k, Item::ArrayOfTables(ArrayOfTables::new()));
            }
            if let Some(Item::ArrayOfTables(a)) = p.get_mut(k) {
                a.push(t);
            }
        })
    }));
}


/// Renames a key of the table at `spot`, keeping its place and its comments. False if the new
/// name is taken.
pub fn rename(spot: &Spot, old: &str, new: &str) -> bool {
    if old == new {
        return true;
    }
    let done = report(with(|s| {
        s.edit(spot, |doc, _| {
            let Some(GotMut::I(Item::Table(t)) | GotMut::T(t)) = find_mut(doc, &spot.path, false, 0) else { return false };
            if t.contains_key(new) || !t.contains_key(old) {
                return false;
            }
            let entries: Vec<(toml_edit::Key, Item)> =
                t.iter().filter_map(|(k, _)| t.get_key_value(k).map(|(k, v)| (k.clone(), v.clone()))).collect();
            t.clear();
            for (key, item) in entries {
                if key.get() == old {
                    let mut renamed = toml_edit::Key::new(new);
                    *renamed.leaf_decor_mut() = key.leaf_decor().clone();
                    t.insert_formatted(&renamed, item);
                } else {
                    t.insert_formatted(&key, item);
                }
            }
            true
        })
    }));
    done == Some(true)
}

pub fn strings<S: AsRef<str>>(list: &[S]) -> Value {
    Value::Array(list.iter().map(|s| s.as_ref()).collect())
}

/// A number as the bar expects it: whole when it has no decimals.
pub fn number(v: f64, decimals: u8) -> Value {
    let p = 10f64.powi(decimals as i32);
    let v = (v * p).round() / p;
    if v.fract() == 0.0 && v.abs() < 1e15 { Value::from(v as i64) } else { Value::from(v) }
}

/// A plain value as toml_edit writes it.
pub fn value_of(v: &toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::from(s.as_str()),
        toml::Value::Integer(i) => Value::from(*i),
        toml::Value::Float(f) => Value::from(*f),
        toml::Value::Boolean(b) => Value::from(*b),
        toml::Value::Datetime(d) => Value::from(d.to_string()),
        toml::Value::Array(a) => Value::Array(a.iter().map(value_of).collect()),
        toml::Value::Table(t) => {
            let mut inline = InlineTable::new();
            for (k, v) in t {
                inline.insert(k, value_of(v));
            }
            inline.fmt();
            Value::InlineTable(inline)
        }
    }
}

/// Forgets what was read: the next read goes to the disk.
#[cfg(test)]
pub fn clear() {
    with(|s| s.docs.clear());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(s: &str) -> DocumentMut {
        s.parse().unwrap()
    }

    fn path(s: &str) -> Vec<Seg> {
        s.split('.').map(|p| p.parse::<usize>().map(Seg::I).unwrap_or_else(|_| Seg::K(p.into()))).collect()
    }

    #[test]
    fn set_keeps_comments() {
        let mut d = doc("# top\n[bar]\nheight = 40   # the bar\n");
        set_in(&mut d, &path("bar.height"), 1, Value::from(44));
        assert_eq!(d.to_string(), "# top\n[bar]\nheight = 44   # the bar\n");
        // 'Literal' strings stay literal, unless the new text has a quote
        let mut d = doc("cmd = ['a', 'b']\n");
        set_in(&mut d, &[Seg::K("cmd".into()), Seg::I(0)], 0, Value::from("it's"));
        set_in(&mut d, &[Seg::K("cmd".into()), Seg::I(1)], 0, Value::from("c d"));
        assert_eq!(d.to_string(), "cmd = [\"it's\", 'c d']\n");
        // Keys added to and removed from an inline table keep its spaces
        let mut d = doc("e = [{ id = \"a\", name = \"A\" }]\n");
        set_in(&mut d, &[Seg::K("e".into()), Seg::I(0), Seg::K("icon".into())], 0, Value::from("i"));
        assert_eq!(d.to_string(), "e = [{ id = \"a\", name = \"A\", icon = \"i\" }]\n");
        remove_in(&mut d, &[Seg::K("e".into()), Seg::I(0), Seg::K("icon".into())]);
        assert_eq!(d.to_string(), "e = [{ id = \"a\", name = \"A\" }]\n");
    }

    #[test]
    fn set_makes_headers_and_dotted_keys() {
        let mut d = doc("[layout]\nleft = []\n");
        set_in(&mut d, &path("widgets.clock.date"), headers(&Spot { path: path("widgets.clock.date") }), Value::from(""));
        set_in(&mut d, &path("widgets.home.printer.plug"), 3, Value::from("switch.p"));
        assert_eq!(d.to_string(), "[layout]\nleft = []\n\n[widgets.clock]\ndate = \"\"\n\n[widgets.home.printer]\nplug = \"switch.p\"\n");
        let mut d = doc("[theme]\nfont = \"x\"\n");
        set_in(&mut d, &path("theme.colors.base"), 2, Value::from("#000000"));
        assert_eq!(d.to_string(), "[theme]\nfont = \"x\"\n\n[theme.colors]\nbase = \"#000000\"\n");
    }

    #[test]
    fn remove_prunes_empty_tables() {
        let mut d = doc("[layout]\nleft = []\n\n[widgets.clock]\ndate = \"\"\n");
        remove_in(&mut d, &path("widgets.clock.date"));
        assert_eq!(d.to_string(), "[layout]\nleft = []\n");
        let mut d = doc("x = [\"a\", \"b\", \"c\"]\n");
        remove_in(&mut d, &path("x.0"));
        assert_eq!(d.to_string(), "x = [\"b\", \"c\"]\n");
    }

    #[test]
    fn swaps_keep_the_places_in_the_file() {
        let mut d = doc("e = [\"a\", \"b\",   \"c\"]\n");
        swap_in(&mut d, &path("e"), 0, 2);
        assert_eq!(d.to_string(), "e = [\"c\", \"b\",   \"a\"]\n");
        let mut d = doc("[[g]]\ntitle = 'a'\n\n# second\n[[g]]\ntitle = 'b'\n");
        swap_in(&mut d, &path("g"), 0, 1);
        assert_eq!(d.to_string(), "[[g]]\ntitle = 'b'\n\n# second\n[[g]]\ntitle = 'a'\n");
    }

    #[test]
    fn reads_through_arrays_of_tables() {
        let d = doc("[[widgets.home.groups]]\ntitle = 'a'\n\n[[widgets.home.groups]]\ntitle = 'b'\nentities = ['y', 'z']\n");
        let v = find(d.as_table(), &path("widgets.home.groups.1.title")).and_then(owned);
        assert_eq!(v, Some(toml::Value::String("b".into())));
        let v = find(d.as_table(), &path("widgets.home.groups.1.entities.1")).and_then(owned);
        assert_eq!(v, Some(toml::Value::String("z".into())));
    }

    /// The edits go where the keys are, the default goes away, a file that doesn't parse isn't
    /// written: on two real files in a folder of the test's own, the way the window does it.
    #[test]
    fn edits_go_where_the_keys_are() {
        let dir = std::env::temp_dir().join(format!("rustybar-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("settings")).unwrap();
        TEST_DIR.with(|d| *d.borrow_mut() = Some(dir.join("settings")));
        let main = dir.join("settings").join("rustybar.toml");
        let local = dir.join("settings").join("rustybar.local.toml");
        std::fs::write(&main, "# mine\n[bar]\nheight = 40 # the bar\n").unwrap();
        std::fs::write(&local, "[widgets.home]\nserver = \"http://a\"\n").unwrap();
        clear();
        put(&rb("bar.height"), number(44.0, 0), Some(&toml::Value::Integer(40)));
        put(&rb("widgets.home.dashboard"), Value::from("/x"), None);
        put(&rb("bar.margin"), number(12.0, 0), Some(&toml::Value::Integer(8)));
        assert_eq!(std::fs::read_to_string(&main).unwrap(), "# mine\n[bar]\nheight = 44 # the bar\nmargin = 12\n");
        assert_eq!(std::fs::read_to_string(&local).unwrap(), "[widgets.home]\nserver = \"http://a\"\ndashboard = \"/x\"\n");
        // Back to the default: the key goes
        put(&rb("bar.margin"), number(8.0, 0), Some(&toml::Value::Integer(8)));
        assert_eq!(std::fs::read_to_string(&main).unwrap(), "# mine\n[bar]\nheight = 44 # the bar\n");
        assert_eq!(get_f64(&rb("bar.height")), Some(44.0));
        assert_eq!(get_str(&rb("widgets.home.server")).as_deref(), Some("http://a"));
        // Changed from outside: read again before the next edit
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&main, "[bar]\nheight = 50\nreserve = false\n").unwrap();
        put(&rb("bar.height"), number(52.0, 0), Some(&toml::Value::Integer(40)));
        assert_eq!(std::fs::read_to_string(&main).unwrap(), "[bar]\nheight = 52\nreserve = false\n");
        // A file that doesn't parse stays as it is
        std::fs::write(&main, "[bar\nheight = 1\n").unwrap();
        put(&rb("bar.height"), number(30.0, 0), None);
        assert!(take_problem().is_some());
        assert_eq!(errors().len(), 1);
        assert_eq!(std::fs::read_to_string(&main).unwrap(), "[bar\nheight = 1\n");
        TEST_DIR.with(|d| *d.borrow_mut() = None);
        clear();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
