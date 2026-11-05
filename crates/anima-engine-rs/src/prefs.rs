//! Shared overlay preferences and the cross-process visibility flag.
//!
//! Ported from `anima_overlay_prefs.py`. Two files under the user data dir:
//!
//! - `config.json` — menu style, access warnings, toggle hotkey.
//! - `overlay_visibility.json` — a lightweight hide/show signal watched by every
//!   animation process (generation integer + hidden bool).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::paths::{atomic_write_json, ensure_dir, user_data_dir};

pub const DEFAULT_TOGGLE_HOTKEY: &str = "Ctrl+Shift+Space";

/// Which animation control popup to present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MenuStyle {
    #[default]
    Modern,
    Compact,
}

impl MenuStyle {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "compact" => MenuStyle::Compact,
            _ => MenuStyle::Modern,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            MenuStyle::Modern => "modern",
            MenuStyle::Compact => "compact",
        }
    }
}

/// User-tunable overlay preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlayPreferences {
    #[serde(default)]
    pub overlay_menu_style: MenuStyle,
    #[serde(default = "default_true")]
    pub overlay_access_warnings: bool,
    #[serde(default = "default_hotkey")]
    pub overlay_toggle_hotkey: String,
}

fn default_true() -> bool {
    true
}

fn default_hotkey() -> String {
    DEFAULT_TOGGLE_HOTKEY.to_string()
}

impl Default for OverlayPreferences {
    fn default() -> Self {
        Self {
            overlay_menu_style: MenuStyle::default(),
            overlay_access_warnings: true,
            overlay_toggle_hotkey: default_hotkey(),
        }
    }
}

impl OverlayPreferences {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        atomic_write_json(path, self)
    }
}

/// Cross-process hide/show signal written to `overlay_visibility.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GlobalVisibility {
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub generation: u64,
}

impl GlobalVisibility {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        atomic_write_json(path, self)
    }

    /// Flip `hidden`, stamping a fresh generation so watchers notice the change.
    pub fn toggled(mut self) -> Self {
        self.hidden = !self.hidden;
        self.generation = now_nanos();
        self
    }
}

fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Resolved on-disk locations for the shared preference files.
#[derive(Debug, Clone)]
pub struct PrefsPaths {
    pub config: PathBuf,
    pub visibility: PathBuf,
}

impl PrefsPaths {
    /// Derive paths from `$XDG_DATA_HOME` (or the default user data dir).
    pub fn user_default() -> Self {
        let root = user_data_dir();
        Self {
            config: root.join("config.json"),
            visibility: root.join("overlay_visibility.json"),
        }
    }

    /// Ensure the parent directory exists.
    pub fn ensure(&self) -> Result<()> {
        if let Some(parent) = self.config.parent() {
            ensure_dir(parent)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_style_parsing() {
        assert_eq!(MenuStyle::parse("Compact"), MenuStyle::Compact);
        assert_eq!(MenuStyle::parse(" MODERN "), MenuStyle::Modern);
        assert_eq!(MenuStyle::parse("bogus"), MenuStyle::Modern);
    }

    #[test]
    fn prefs_roundtrip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");

        assert_eq!(
            OverlayPreferences::load(&path),
            OverlayPreferences::default()
        );

        let prefs = OverlayPreferences {
            overlay_menu_style: MenuStyle::Compact,
            overlay_access_warnings: false,
            overlay_toggle_hotkey: "Ctrl+Shift+Space".into(),
        };
        prefs.save(&path).unwrap();
        assert_eq!(OverlayPreferences::load(&path), prefs);
    }

    #[test]
    fn visibility_toggle_bumps_generation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overlay_visibility.json");

        let v0 = GlobalVisibility::load(&path);
        assert!(!v0.hidden);

        let v1 = v0.toggled();
        v1.save(&path).unwrap();
        let back = GlobalVisibility::load(&path);
        assert!(back.hidden);
        assert!(back.generation > v0.generation);
    }
}
