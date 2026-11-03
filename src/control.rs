//! Cross-process overlay control state.
//!
//! The overlay process is a bare, frameless, always-on-top surface, so once
//! input is disabled the user cannot reach it with the mouse. The tray
//! (StatusNotifierItem), the portal global-shortcut handlers and the
//! `anima-overlay-ctl` CLI therefore talk to it through this small JSON file:
//! writers bump `generation` and change fields, and the overlay applies the
//! newest generation.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::paths;

/// File name, inside [`paths::user_data_dir`].
pub const CONTROL_FILE: &str = "overlay_control.json";

/// Smallest accepted display size. Explicit downscales below this are refused.
pub const MIN_SCALE_PERCENT: u32 = 10;

/// 100% is the native frame size. The overlay only ever shrinks, so no
/// upscaling (and no resampling) is needed.
pub const MAX_SCALE_PERCENT: u32 = 100;

fn default_scale_percent() -> u32 {
    MAX_SCALE_PERCENT
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// How the overlay surface treats pointer input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputMode {
    /// The whole surface receives input.
    Full,
    /// Only opaque pixels receive input; transparent ones pass clicks through
    /// to whatever is below.
    #[default]
    Opaque,
    /// The whole surface passes clicks through.
    PassThrough,
}

/// Desired overlay state, written by the tray/portal/CLI and read by the overlay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlState {
    /// Input handling for the surface.
    #[serde(default)]
    pub input_mode: InputMode,
    /// Display size as a percentage of the native frame size (10-100).
    #[serde(default = "default_scale_percent")]
    pub scale_percent: u32,
    /// Whether dragging the overlay is disabled.
    #[serde(default)]
    pub locked: bool,
    /// One-shot: put the window back at its start position and native size.
    #[serde(default)]
    pub reset: bool,
    /// One-shot: stop the overlay process.
    #[serde(default)]
    pub quit: bool,
    /// Marker bumped on every write; the overlay only applies newer ones.
    #[serde(default)]
    pub generation: u64,
}

impl Default for ControlState {
    fn default() -> Self {
        Self {
            input_mode: InputMode::default(),
            scale_percent: MAX_SCALE_PERCENT,
            locked: false,
            reset: false,
            quit: false,
            generation: 0,
        }
    }
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

    /// Select an input mode.
    #[must_use]
    pub fn set_input_mode(mut self, mode: InputMode) -> Self {
        self.input_mode = mode;
        self.touched()
    }

    /// Set the display size, clamped to `MIN..=MAX` percent.
    #[must_use]
    pub fn set_scale_percent(mut self, percent: u32) -> Self {
        self.scale_percent = percent.clamp(MIN_SCALE_PERCENT, MAX_SCALE_PERCENT);
        self.touched()
    }

    /// Step the display size by `delta` percent, clamped.
    #[must_use]
    pub fn nudge_scale(self, delta: i32) -> Self {
        let next = (i64::from(self.scale_percent) + i64::from(delta))
            .clamp(i64::from(MIN_SCALE_PERCENT), i64::from(MAX_SCALE_PERCENT))
            as u32;
        self.set_scale_percent(next)
    }

    /// Enable or disable dragging.
    #[must_use]
    pub fn set_locked(mut self, locked: bool) -> Self {
        self.locked = locked;
        self.touched()
    }

    /// Flip the drag lock.
    #[must_use]
    pub fn toggle_locked(self) -> Self {
        let locked = !self.locked;
        self.set_locked(locked)
    }

    /// Toggle between per-pixel input (`Opaque`) and full click-through.
    #[must_use]
    pub fn toggle_click_through(self) -> Self {
        let mode = match self.input_mode {
            InputMode::PassThrough => InputMode::Opaque,
            _ => InputMode::PassThrough,
        };
        self.set_input_mode(mode)
    }

    /// Ask the overlay to restore its initial position, native size and per-pixel
    /// input.
    #[must_use]
    pub fn request_reset(mut self) -> Self {
        self.input_mode = InputMode::Opaque;
        self.scale_percent = MAX_SCALE_PERCENT;
        self.locked = false;
        self.reset = true;
        self.quit = false;
        self.bump();
        self
    }

    /// Ask the overlay to exit.
    #[must_use]
    pub fn request_quit(mut self) -> Self {
        self.reset = false;
        self.quit = true;
        self.bump();
        self
    }

    /// Clear the one-shot flags and mark a new generation.
    fn touched(mut self) -> Self {
        self.reset = false;
        self.quit = false;
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
        assert_eq!(ControlState::load(&path).input_mode, InputMode::Opaque);
        assert_eq!(ControlState::load(&path).scale_percent, 100);
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
            input_mode: InputMode::PassThrough,
            scale_percent: 50,
            locked: true,
            reset: false,
            quit: false,
            generation: 7,
        };
        state.save(&path).unwrap();
        assert_eq!(ControlState::load(&path), state);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn input_mode_serializes_snake_case() {
        let json = serde_json::to_string(&InputMode::PassThrough).unwrap();
        assert_eq!(json, "\"pass_through\"");
        assert_eq!(
            serde_json::from_str::<InputMode>("\"opaque\"").unwrap(),
            InputMode::Opaque
        );
    }

    #[test]
    fn scale_is_clamped_and_nudged() {
        let state = ControlState::fresh();
        assert_eq!(state.scale_percent, 100);

        assert_eq!(state.clone().set_scale_percent(0).scale_percent, 10);
        assert_eq!(state.clone().set_scale_percent(250).scale_percent, 100);
        assert_eq!(state.clone().set_scale_percent(50).scale_percent, 50);
        assert_eq!(state.clone().nudge_scale(-40).scale_percent, 60);
        assert_eq!(state.clone().nudge_scale(40).scale_percent, 100);

        // reset restores the native size
        let small = state.clone().set_scale_percent(25);
        assert_eq!(small.request_reset().scale_percent, 100);
    }

    #[test]
    fn transitions_bump_generation_and_clear_flags() {
        let start = ControlState::fresh();

        let through = start.clone().toggle_click_through();
        assert_eq!(through.input_mode, InputMode::PassThrough);
        assert!(through.generation > start.generation);

        let back = through.clone().toggle_click_through();
        assert_eq!(back.input_mode, InputMode::Opaque);

        let reset = back.clone().request_reset();
        assert!(reset.reset);
        assert_eq!(reset.input_mode, InputMode::Opaque);
        assert!(reset.generation > back.generation);

        let quit = reset.clone().request_quit();
        assert!(quit.quit);
        assert!(!quit.reset);
        assert!(quit.generation > reset.generation);

        let full = quit.clone().set_input_mode(InputMode::Full);
        assert_eq!(full.input_mode, InputMode::Full);
        assert!(!full.reset);
        assert!(!full.quit);

        // a scale change clears the one-shot flags too
        let scaled = reset.clone().set_scale_percent(75);
        assert_eq!(scaled.scale_percent, 75);
        assert!(!scaled.reset);

        // the drag lock round-trips and reset clears it
        let locked = start.clone().toggle_locked();
        assert!(locked.locked);
        assert!(locked.generation > start.generation);
        assert!(!locked.request_reset().locked);
    }
}
