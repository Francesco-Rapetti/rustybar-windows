//! A small JSON reader, for what Claude Code answers (a few lines) and its `.claude.json`: values
//! as a tree, numbers as `f64`. A dependency would do the same with more than this needs.

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

static NULL: Json = Json::Null;

impl Json {
    pub fn parse(text: &str) -> Option<Json> {
        let mut p = Parser { s: text.as_bytes(), i: 0 };
        let value = p.value()?;
        p.space();
        (p.i == p.s.len()).then_some(value)
    }

    /// The member `key` of an object, or null.
    pub fn get(&self, key: &str) -> &Json {
        match self {
            Json::Object(m) => m.get(key).unwrap_or(&NULL),
            _ => &NULL,
        }
    }

    pub fn str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn f64(&self) -> Option<f64> {
        match self {
            Json::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    pub fn object(&self) -> Option<&BTreeMap<String, Json>> {
        match self {
            Json::Object(m) => Some(m),
            _ => None,
        }
    }

    pub fn array(&self) -> &[Json] {
        match self {
            Json::Array(a) => a,
            _ => &[],
        }
    }

    /// An object of these members, for writing.
    pub fn of<'a>(members: impl IntoIterator<Item = (&'a str, Json)>) -> Json {
        Json::Object(members.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }
}

impl From<&str> for Json {
    fn from(s: &str) -> Json {
        Json::String(s.to_string())
    }
}

impl From<f64> for Json {
    fn from(n: f64) -> Json {
        Json::Number(n)
    }
}

impl From<bool> for Json {
    fn from(b: bool) -> Json {
        Json::Bool(b)
    }
}

/// The value as JSON text: whole numbers without a fraction, strings escaped.
impl std::fmt::Display for Json {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Json::Null => f.write_str("null"),
            Json::Bool(b) => write!(f, "{b}"),
            Json::Number(n) if n.fract() == 0.0 && n.abs() < 1e15 => write!(f, "{}", *n as i64),
            Json::Number(n) if n.is_finite() => write!(f, "{n}"),
            Json::Number(_) => f.write_str("null"),
            Json::String(s) => {
                f.write_str("\"")?;
                for c in s.chars() {
                    match c {
                        '"' => f.write_str("\\\"")?,
                        '\\' => f.write_str("\\\\")?,
                        '\n' => f.write_str("\\n")?,
                        '\r' => f.write_str("\\r")?,
                        '\t' => f.write_str("\\t")?,
                        c if (c as u32) < 0x20 => write!(f, "\\u{:04x}", c as u32)?,
                        c => write!(f, "{c}")?,
                    }
                }
                f.write_str("\"")
            }
            Json::Array(a) => {
                f.write_str("[")?;
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str("]")
            }
            Json::Object(m) => {
                f.write_str("{")?;
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{}:{v}", Json::String(k.clone()))?;
                }
                f.write_str("}")
            }
        }
    }
}

impl std::ops::Index<&str> for Json {
    type Output = Json;

    fn index(&self, key: &str) -> &Json {
        self.get(key)
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn space(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn eat(&mut self, b: u8) -> Option<()> {
        self.space();
        (self.s.get(self.i) == Some(&b)).then(|| self.i += 1)
    }

    fn word(&mut self, w: &str, v: Json) -> Option<Json> {
        self.s[self.i..].starts_with(w.as_bytes()).then(|| {
            self.i += w.len();
            v
        })
    }

    fn value(&mut self) -> Option<Json> {
        self.space();
        match *self.s.get(self.i)? {
            b'n' => self.word("null", Json::Null),
            b't' => self.word("true", Json::Bool(true)),
            b'f' => self.word("false", Json::Bool(false)),
            b'"' => self.string().map(Json::String),
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                if self.eat(b']').is_some() {
                    return Some(Json::Array(items));
                }
                loop {
                    items.push(self.value()?);
                    if self.eat(b',').is_none() {
                        self.eat(b']')?;
                        return Some(Json::Array(items));
                    }
                }
            }
            b'{' => {
                self.i += 1;
                let mut members = BTreeMap::new();
                if self.eat(b'}').is_some() {
                    return Some(Json::Object(members));
                }
                loop {
                    self.space();
                    let key = self.string()?;
                    self.eat(b':')?;
                    members.insert(key, self.value()?);
                    if self.eat(b',').is_none() {
                        self.eat(b'}')?;
                        return Some(Json::Object(members));
                    }
                }
            }
            _ => self.number(),
        }
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.i;
        while self.i < self.s.len() && matches!(self.s[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
            self.i += 1;
        }
        std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok().map(Json::Number)
    }

    fn hex4(&mut self) -> Option<u32> {
        let h = std::str::from_utf8(self.s.get(self.i..self.i + 4)?).ok()?;
        self.i += 4;
        u32::from_str_radix(h, 16).ok()
    }

    fn string(&mut self) -> Option<String> {
        if self.s.get(self.i) != Some(&b'"') {
            return None;
        }
        self.i += 1;
        let mut out = String::new();
        loop {
            let start = self.i;
            while self.i < self.s.len() && self.s[self.i] != b'"' && self.s[self.i] != b'\\' {
                self.i += 1;
            }
            out.push_str(std::str::from_utf8(&self.s[start..self.i]).ok()?);
            match *self.s.get(self.i)? {
                b'"' => {
                    self.i += 1;
                    return Some(out);
                }
                _ => {
                    self.i += 1;
                    let c = *self.s.get(self.i)?;
                    self.i += 1;
                    match c {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let mut code = self.hex4()?;
                            // A pair of surrogates for a character outside the basic plane
                            if (0xd800..0xdc00).contains(&code) && self.s[self.i..].starts_with(b"\\u") {
                                self.i += 2;
                                let low = self.hex4()?;
                                code = 0x10000 + ((code - 0xd800) << 10) + (low.wrapping_sub(0xdc00) & 0x3ff);
                            }
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        _ => return None,
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values() {
        let j = Json::parse(r#" {"a": [1, 2.5, -3e2], "b": {"c": "x\"yè😀"}, "d": true, "e": null} "#).unwrap();
        assert_eq!(j["a"], Json::Array(vec![Json::Number(1.0), Json::Number(2.5), Json::Number(-300.0)]));
        assert_eq!(j["b"]["c"].str(), Some("x\"yè😀"));
        assert_eq!(j["d"].bool(), Some(true));
        assert!(j["e"].is_null());
        assert!(j["missing"]["deeper"].is_null());
        assert_eq!(Json::parse("[]"), Some(Json::Array(vec![])));
        assert_eq!(Json::parse("{\"a\":1} x"), None);
        assert_eq!(Json::parse("{\"a\":}"), None);
    }

    #[test]
    fn written_and_read_back() {
        let j = Json::of([("type", "auth".into()), ("id", 3.0.into()), ("ids", Json::Array(vec!["a\"b\n".into()])), ("t", 21.5.into())]);
        let text = j.to_string();
        assert_eq!(text, r#"{"id":3,"ids":["a\"b\n"],"t":21.5,"type":"auth"}"#);
        assert_eq!(Json::parse(&text), Some(j));
    }
}
