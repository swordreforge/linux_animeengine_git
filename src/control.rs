//! Cross-process overlay control state.
//!
//! The overlay process is a bare, frameless, always-on-top surface, so once
//! click-through is enabled the user cannot reach it with the mouse. The tray
//! (StatusNotifierItem) and the portal global-shortcut handlers therefore talk
//! to it through this small JSON file: writers bump `generation` and flip
//! flags, and the overlay applies the newest generation.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::paths;

/// File name, inside [`paths::user_data_dir`].
pub const CONTROL_FILE: &str = "overlay_control.json";

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Desired overlay state, written by the tray/portal and read by the overlay.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ControlState {
    /// When true the whole surface ignores input (clicks pass through).
    #[serde(default)]
    pub click_through: bool,
    /// One-shot: turn click-through off and put the window back at its start.
    #[serde(default)]
    pub reset: bool,
    /// One-shot: stop the overlay process.
    #[serde(default)]
    pub quit: bool,
    /// Marker bumped on every write; the overlay only applies newer ones.
    #[serde(default)]
    pub generation: u64,
}

impl ControlState {
    /// Default location: `<XDG data>/AnimaEngine/overlay_control.json`.
    pub fn path() -> PathBuf {
        paths::user_data_dir().join(CONTROL_FILE)
    }

    /// Fresh state with a current generation, used at overlay startup so stale
    /// flags from a previous session are not re-applied.
    pub fn fresh() -> Self {
        let mut state = Self::default();
        state.bump();
        state
    }

    /// Read the file, tolerating a missing or corrupt one (default state).
    pub fn load(path: &Path) -> Self {
        paths::load_json(path)
    }

    /// Atomically write the file.
    pub fn save(&self, path: &Path) -> Result<()> {
        paths::atomic_write_json(path, self)
    }

    /// Flip click-through and clear the one-shot flags.
    #[must_use]
    pub fn toggle_click_through(mut self) -> Self {
        self.click_through = !self.click_through;
        self.reset = false;
        self.quit = false;
        self.bump();
        self
    }

    /// Ask the overlay to restore its initial interaction and position.
    #[must_use]
    pub fn request_reset(mut self) -> Self {
        self.click_through = false;
        self.reset = true;
        self.quit = false;
        self.bump();
        self
    }

    /// Ask the overlay to exit.
    #[must_use]
    pub fn request_quit(mut self) -> Self {
        self.quit = true;
        self.bump();
        self
    }

    fn bump(&mut self) {
        self.generation = now_millis().max(self.generation + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("anima-control-{}-{name}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn missing_file_loads_default() {
        let path = temp_path("missing");
        assert_eq!(ControlState::load(&path), ControlState::default());
    }

    #[test]
    fn corrupt_file_loads_default() {
        let path = temp_path("corrupt");
        std::fs::write(&path, b"not json").unwrap();
        assert_eq!(ControlState::load(&path), ControlState::default());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn roundtrip_save_load() {
        let path = temp_path("roundtrip");
        let state = ControlState {
            click_through: true,
            reset: false,
            quit: false,
            generation: 7,
        };
        state.save(&path).unwrap();
        assert_eq!(ControlState::load(&path), state);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn transitions_bump_generation_and_clear_flags() {
        let start = ControlState::fresh();

        let toggled = start.clone().toggle_click_through();
        assert!(toggled.click_through);
        assert!(!toggled.reset);
        assert!(toggled.generation > start.generation);

        let reset = toggled.clone().request_reset();
        assert!(!reset.click_through);
        assert!(reset.reset);
        assert!(reset.generation > toggled.generation);

        let quit = reset.clone().request_quit();
        assert!(quit.quit);
        assert!(quit.generation > reset.generation);

        let toggled_again = quit.toggle_click_through();
        assert!(toggled_again.click_through);
        assert!(!toggled_again.reset);
        assert!(!toggled_again.quit);
    }
}
