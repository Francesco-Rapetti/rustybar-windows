//! Made-up data, for screenshots and tests: with `RUSTYBAR_MOCK` set to a folder, every widget but
//! the clock reads `<folder>/<widget>.json` instead (`examples/mock` has a set, and the config of
//! the README's screenshots), and nothing of yours reaches the bar: no server is asked, no account,
//! device or network read. What a click would do to the PC does nothing; a slider or a switch
//! moves the made-up data alone.

use crate::json::Json;
use crate::log;

/// The folder of the made-up data, when mocking is on: for the files a widget's data names (the
/// covers of `media`).
pub fn dir() -> Option<std::path::PathBuf> {
    std::env::var_os("RUSTYBAR_MOCK").map(std::path::PathBuf::from)
}

/// None when mocking is off; else the widget's file, or null when there's none.
pub fn load(name: &str) -> Option<Json> {
    let path = dir()?.join(format!("{name}.json"));
    let Ok(text) = std::fs::read_to_string(&path) else { return Some(Json::Null) };
    Some(Json::parse(&text).unwrap_or_else(|| {
        log!("{}: not JSON", path.display());
        Json::Null
    }))
}

#[cfg(test)]
mod tests {
    use crate::json::Json;

    #[test]
    fn the_made_up_data_reads() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples").join("mock");
        let mut files = 0;
        for entry in std::fs::read_dir(&dir).expect("examples/mock") {
            let path = entry.expect("an entry").path();
            if path.extension().is_some_and(|e| e == "json") {
                let text = std::fs::read_to_string(&path).expect("readable");
                assert!(Json::parse(&text).is_some(), "{} isn't JSON", path.display());
                files += 1;
            }
        }
        assert!(files >= 11, "{files} files");
        // The config the settings window edits on made-up data reads as the bar's
        let settings = std::fs::read_to_string(dir.join("settings").join("rustybar.toml")).expect("examples/mock/settings");
        assert!(toml::from_str::<crate::config::Config>(&settings).is_ok());
    }
}
