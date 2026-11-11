//! Made-up data, for screenshots and tests: with `RUSTYBAR_MOCK` set to a folder, the widgets that
//! show personal data read `<folder>/<widget>.json` instead (`examples/mock` has a set), and
//! nothing of yours reaches the bar: no server is asked, no account read.

use crate::json::Json;
use crate::log;

/// None when mocking is off; else the widget's file, or null when there's none.
pub fn load(name: &str) -> Option<Json> {
    let dir = std::env::var_os("RUSTYBAR_MOCK")?;
    let path = std::path::Path::new(&dir).join(format!("{name}.json"));
    let Ok(text) = std::fs::read_to_string(&path) else { return Some(Json::Null) };
    Some(Json::parse(&text).unwrap_or_else(|| {
        log!("{}: not JSON", path.display());
        Json::Null
    }))
}
