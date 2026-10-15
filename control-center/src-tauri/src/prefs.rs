//! Preferences of the Control Center window itself (not of the daemon, so
//! they stay out of `config.json`): `~/.config/git-drive/control-center.json`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiPrefs {
    /// Accent color id (`multicolor`, `blue`, ...); the frontend owns the palette.
    #[serde(default = "default_accent")]
    pub accent: String,
}

fn default_accent() -> String {
    "multicolor".to_string()
}

impl Default for UiPrefs {
    fn default() -> Self {
        UiPrefs { accent: default_accent() }
    }
}

pub fn path() -> PathBuf {
    git_drive::paths::config_dir().join("control-center.json")
}

/// The saved preferences; defaults when the file is missing or unreadable.
pub fn load(path: &Path) -> UiPrefs {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Writes atomically (temp file + rename).
pub fn save(path: &Path, prefs: &UiPrefs) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(prefs)? + "\n")?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_default_when_missing_or_unreadable_and_round_trip() {
        let dir = std::env::temp_dir().join(format!("cc-prefs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("control-center.json");
        assert_eq!(load(&path).accent, "multicolor", "missing file");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(load(&path).accent, "multicolor", "unreadable file");
        save(&path, &UiPrefs { accent: "green".into() }).unwrap();
        assert_eq!(load(&path).accent, "green");
        assert!(!dir.join("control-center.json.tmp").exists(), "no temp file left");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
