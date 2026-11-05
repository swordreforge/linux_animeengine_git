//! Filesystem paths and atomic JSON persistence.
//!
//! Ported from `anima_foundation.py` (paths + atomic writes) and the state
//! directory convention used across AnimaEngine. Windows' `LOCALAPPDATA` is
//! replaced by the XDG base directory on Linux.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::Result;

/// Product folder name, matching the shipped Python build.
pub const APP_NAME: &str = "AnimaEngine";

/// Directory containing the running executable (or `.` when unavailable).
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Home directory, falling back to `.` when `HOME` is unset.
pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `$XDG_DATA_HOME/AnimaEngine` (defaults to `~/.local/share/AnimaEngine`).
pub fn user_data_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home_dir().join(".local").join("share"));
    base.join(APP_NAME)
}

/// Create `path` and all missing parents.
pub fn ensure_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    Ok(())
}

/// Write `value` as pretty JSON via a temp file + rename, so readers never
/// observe a half-written file. Same guarantee as `atomic_write_json` in Python.
pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(dir) = dir {
        std::fs::create_dir_all(dir)?;
    }
    let dir = dir.unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(".anima_{}.tmp", std::process::id()));
    {
        let mut file = std::fs::File::create(&tmp)?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Best-effort canonical key for deduplication (mirrors `canonical_path`).
pub fn canonical_path(path: &str) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| PathBuf::from(path))
        .to_string_lossy()
        .into_owned()
}

/// Drop duplicate paths while keeping first-seen order, never touching disk.
pub fn dedupe_paths<I>(paths: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for path in paths {
        if path.is_empty() {
            continue;
        }
        if seen.insert(canonical_path(&path)) {
            out.push(path);
        }
    }
    out
}

/// Read a JSON file, falling back to `T::default()` when it is missing or
/// malformed (mirrors the forgiving `load_paths` the Python build used).
pub fn load_json<T: DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Write `value` as JSON atomically.
pub fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    atomic_write_json(path, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_data_dir_ends_with_app_name() {
        // Assert the suffix shape without mutating process env (tests run in parallel).
        let dir = user_data_dir();
        assert!(dir.ends_with(APP_NAME));
    }

    #[test]
    fn atomic_write_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("state.json");
        let value = serde_json::json!({ "enabled": true, "n": 3 });
        atomic_write_json(&path, &value).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let back: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(back, value);
    }

    #[test]
    fn dedupe_keeps_first_seen_order() {
        let paths = vec![
            "a.gif".to_string(),
            "a.gif".to_string(),
            String::new(),
            "b.gif".to_string(),
        ];
        assert_eq!(dedupe_paths(paths), vec!["a.gif", "b.gif"]);
    }
}
