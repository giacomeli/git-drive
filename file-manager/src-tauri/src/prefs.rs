//! The accent color, shared with the Control Center: the `accent` key of
//! `~/.config/git-drive/control-center.json`. Either app may change it; a
//! save here leaves the file's other keys as they are.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// The palette lives in the frontend (`src/ui/accent.ts`); unknown ids fall back there too.
pub const DEFAULT_ACCENT: &str = "multicolor";

pub fn path() -> PathBuf {
    git_drive_state::paths::config_dir().join("control-center.json")
}

/// The saved accent id; the default when the file is missing, unreadable or has none.
pub fn load_accent(path: &Path) -> String {
    read(path)
        .get("accent")
        .and_then(Value::as_str)
        .map_or_else(|| DEFAULT_ACCENT.to_string(), str::to_string)
}

/// The preferences object; empty when the file is missing or unreadable.
fn read(path: &Path) -> Map<String, Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|v| match v {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default()
}

/// Saves `accent`, keeping the other preferences; atomic (temp file + rename).
pub fn save_accent(path: &Path, accent: &str) -> io::Result<()> {
    let mut prefs = read(path);
    prefs.insert("accent".into(), Value::String(accent.into()));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&prefs)? + "\n")?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_accent_defaults_when_missing_or_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control-center.json");
        assert_eq!(load_accent(&path), DEFAULT_ACCENT, "missing file");
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(load_accent(&path), DEFAULT_ACCENT, "unreadable file");
        std::fs::write(&path, r#"{"other": 1}"#).unwrap();
        assert_eq!(load_accent(&path), DEFAULT_ACCENT, "no accent key");
    }

    #[test]
    fn saving_keeps_the_control_centers_other_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("git-drive").join("control-center.json");
        save_accent(&path, "green").unwrap();
        assert_eq!(load_accent(&path), "green", "the folder is created");

        std::fs::write(&path, r#"{"accent": "green", "page": "logs"}"#).unwrap();
        save_accent(&path, "blue").unwrap();
        assert_eq!(load_accent(&path), "blue");
        let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["page"], "logs", "a key this app does not know survives");
        assert!(!path.with_extension("json.tmp").exists(), "no temp file left");
    }

    #[test]
    fn an_unreadable_file_is_replaced_on_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control-center.json");
        std::fs::write(&path, "{not json").unwrap();
        save_accent(&path, "pink").unwrap();
        assert_eq!(load_accent(&path), "pink");
    }
}
