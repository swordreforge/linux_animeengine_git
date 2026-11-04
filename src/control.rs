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
use sha2::{Digest, Sha256};

use crate::error::Result;
use crate::paths;

/// Directory (inside [`paths::user_data_dir`]) holding one control file per
/// overlay instance.
pub const CONTROL_DIR: &str = "overlay_control";

/// Stable, filesystem-safe slug for an instance key: 16 hex chars of its SHA-256.
pub fn instance_slug(key: &str) -> String {
    use std::fmt::Write as _;

    let digest = Sha256::digest(key.as_bytes());
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Smallest accepted display size. Explicit downscales below this are refused.
pub const MIN_SCALE_PERCENT: u32 = 10;

/// 100% is the native frame size. The overlay only ever shrinks, so no
/// upscaling (and no resampling) is needed.
pub const MAX_SCALE_PERCENT: u32 = 100;

fn default_scale_percent() -> u32 {
    MAX_SCALE_PERCENT
}

/// Default input-region sampling policy (`off`, `auto`, or a fixed grid).
pub const DEFAULT_REGION_STEP: &str = "off";
/// Default soft rectangle budget for `auto`.
pub const DEFAULT_REGION_SOFT_RECTS: usize = 768;
/// Default hard rectangle cap.
pub const DEFAULT_REGION_MAX_RECTS: usize = 4096;

fn default_region_step() -> String {
    DEFAULT_REGION_STEP.to_string()
}

fn default_region_soft() -> usize {
    DEFAULT_REGION_SOFT_RECTS
}

fn default_region_max() -> usize {
    DEFAULT_REGION_MAX_RECTS
}

/// Normalize a region-step string to `off`, `auto`, or `1..=32`.
pub fn normalize_region_step(value: &str) -> String {
    match value {
        "auto" => "auto".to_string(),
        "off" => "off".to_string(),
        other => other
            .parse::<u32>()
            .map(|step| step.clamp(1, 32).to_string())
            .unwrap_or_else(|_| "off".to_string()),
    }
}

/// Normalize a rotation to the nearest quarter turn (0/90/180/270).
pub fn normalize_rotation(degrees: i32) -> i32 {
    let normalized = degrees.rem_euclid(360);
    ((normalized + 45) / 90 % 4) * 90
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
    /// Whether the overlay is hidden (kept alive as a 1x1 surface).
    #[serde(default)]
    pub hidden: bool,
    /// Clockwise rotation in degrees, normalized to 0/90/180/270.
    #[serde(default)]
    pub rotation: i32,
    /// Mirror horizontally.
    #[serde(default)]
    pub flip_h: bool,
    /// Mirror vertically.
    #[serde(default)]
    pub flip_v: bool,
    /// Input-region sampling policy: `off`, `auto`, or a fixed grid `1..=32`.
    #[serde(default = "default_region_step")]
    pub region_step: String,
    /// Soft rectangle budget for `auto`.
    #[serde(default = "default_region_soft")]
    pub region_soft_rects: usize,
    /// Hard rectangle cap.
    #[serde(default = "default_region_max")]
    pub region_max_rects: usize,
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
            hidden: false,
            rotation: 0,
            flip_h: false,
            flip_v: false,
            region_step: default_region_step(),
            region_soft_rects: DEFAULT_REGION_SOFT_RECTS,
            region_max_rects: DEFAULT_REGION_MAX_RECTS,
            reset: false,
            quit: false,
            generation: 0,
        }
    }
}

impl ControlState {
    /// Control file for one instance (`key` = the animation's canonical path),
    /// or the key-less `default.json` used by the generated test pattern.
    pub fn path_for(key: Option<&str>) -> PathBuf {
        let dir = paths::user_data_dir().join(CONTROL_DIR);
        match key {
            Some(key) => dir.join(format!("{}.json", instance_slug(key))),
            None => dir.join("default.json"),
        }
    }

    /// Key-less control file (test pattern, and the fallback when no instance
    /// files exist yet).
    pub fn path() -> PathBuf {
        Self::path_for(None)
    }

    /// Existing per-instance control files, sorted for stable output.
    pub fn instances() -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(paths::user_data_dir().join(CONTROL_DIR)) else {
            return Vec::new();
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension().is_some_and(|ext| ext == "json")
                    && path
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .is_some_and(|stem| {
                            stem == "default"
                                || (stem.len() == 16 && stem.bytes().all(|b| b.is_ascii_hexdigit()))
                        })
            })
            .collect();
        paths.sort();
        paths
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

    /// Show or hide the overlay (a hidden overlay stays alive at 1x1).
    #[must_use]
    pub fn set_hidden(mut self, hidden: bool) -> Self {
        self.hidden = hidden;
        self.touched()
    }

    /// Flip the hidden flag.
    #[must_use]
    pub fn toggle_hidden(self) -> Self {
        let hidden = !self.hidden;
        self.set_hidden(hidden)
    }

    /// Set the rotation, normalized to 0/90/180/270.
    #[must_use]
    pub fn set_rotation(mut self, degrees: i32) -> Self {
        self.rotation = normalize_rotation(degrees);
        self.touched()
    }

    /// Rotate clockwise by `delta` degrees (typically ±90).
    #[must_use]
    pub fn rotate_by(self, delta: i32) -> Self {
        let degrees = self.rotation + delta;
        self.set_rotation(degrees)
    }

    /// Flip horizontally.
    #[must_use]
    pub fn toggle_flip_h(mut self) -> Self {
        self.flip_h = !self.flip_h;
        self.touched()
    }

    /// Flip vertically.
    #[must_use]
    pub fn toggle_flip_v(mut self) -> Self {
        self.flip_v = !self.flip_v;
        self.touched()
    }

    /// Set the input-region sampling policy (`off`, `auto`, or `1..=32`).
    #[must_use]
    pub fn set_region_step(mut self, step: &str) -> Self {
        self.region_step = normalize_region_step(step);
        self.touched()
    }

    /// Set the `auto` soft budget (clamped, kept at or below the hard cap).
    #[must_use]
    pub fn set_region_soft_rects(mut self, soft: usize) -> Self {
        self.region_soft_rects = soft.clamp(16, 65_536).min(self.region_max_rects);
        self.touched()
    }

    /// Set the hard rectangle cap (clamped; the soft budget follows it down).
    #[must_use]
    pub fn set_region_max_rects(mut self, max: usize) -> Self {
        self.region_max_rects = max.clamp(16, 1_000_000);
        self.region_soft_rects = self.region_soft_rects.min(self.region_max_rects);
        self.touched()
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
            hidden: true,
            rotation: 90,
            flip_h: true,
            flip_v: false,
            region_step: "auto".to_string(),
            region_soft_rects: 512,
            region_max_rects: 2048,
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
    fn rotation_and_flips_normalize() {
        let state = ControlState::fresh();
        assert_eq!(state.rotation, 0);
        assert!(!state.flip_h && !state.flip_v);

        assert_eq!(state.clone().set_rotation(90).rotation, 90);
        assert_eq!(state.clone().set_rotation(-90).rotation, 270);
        assert_eq!(state.clone().set_rotation(350).rotation, 0);
        assert_eq!(state.clone().rotate_by(90).rotation, 90);
        assert_eq!(state.clone().rotate_by(-90).rotation, 270);

        let flipped = state.clone().toggle_flip_h().toggle_flip_v();
        assert!(flipped.flip_h && flipped.flip_v);
    }

    #[test]
    fn region_tuning_is_normalized_and_clamped() {
        let state = ControlState::fresh();
        assert_eq!(state.region_step, "off");
        assert_eq!(state.region_soft_rects, 768);
        assert_eq!(state.region_max_rects, 4096);

        assert_eq!(state.clone().set_region_step("auto").region_step, "auto");
        assert_eq!(state.clone().set_region_step("4").region_step, "4");
        assert_eq!(state.clone().set_region_step("99").region_step, "32");
        assert_eq!(state.clone().set_region_step("junk").region_step, "off");

        assert_eq!(state.clone().set_region_soft_rects(0).region_soft_rects, 16);
        assert_eq!(
            state.clone().set_region_soft_rects(5000).region_soft_rects,
            4096
        );
        let capped = state.clone().set_region_max_rects(100);
        assert_eq!(capped.region_max_rects, 100);
        assert_eq!(capped.region_soft_rects, 100);
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
    fn instance_paths_are_distinct_and_stable() {
        let slug = instance_slug("/gifs/a.apng");
        assert_eq!(slug.len(), 16);
        assert!(slug.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(slug, instance_slug("/gifs/a.apng"));

        let a = ControlState::path_for(Some("/gifs/a.apng"));
        let b = ControlState::path_for(Some("/gifs/b.apng"));
        let default = ControlState::path_for(None);
        assert_ne!(a, b);
        assert_ne!(a, default);
        assert!(a.ends_with(format!("{slug}.json")));
        assert!(default.ends_with("default.json"));
        assert_eq!(ControlState::path(), default);
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

        // hide/show round-trips
        let hidden = start.clone().toggle_hidden();
        assert!(hidden.hidden);
        assert!(!hidden.clone().toggle_hidden().hidden);
    }
}
