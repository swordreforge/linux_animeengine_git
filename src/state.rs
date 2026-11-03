//! Secondary overlay instance state (`secondary_instances.json`).
//!
//! Ported from the `_json_*` helpers in `monoModule.py`: the file tracks one
//! entry per running animation so positions and interaction toggles survive
//! restarts.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::paths::{self, atomic_write_json};

fn default_true() -> bool {
    true
}

fn default_fps() -> u32 {
    15
}

fn default_scale() -> u32 {
    100
}

/// File name inside [`paths::user_data_dir`].
pub const STATE_FILE: &str = "secondary_instances.json";

/// A single animation instance persisted between runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Instance {
    pub id: String,
    #[serde(default)]
    pub path: String,
    #[serde(default = "default_fps")]
    pub fps: u32,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default = "default_true")]
    pub always_on_top: bool,
    #[serde(default = "default_scale")]
    pub scale_percent: u32,
    #[serde(default)]
    pub rotation: i32,
    #[serde(default)]
    pub flip_h: bool,
    #[serde(default)]
    pub flip_v: bool,
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub click_through: bool,
    #[serde(default)]
    pub hover_hide: bool,
}

impl Instance {
    /// New instance at a position with sensible defaults.
    pub fn new(id: impl Into<String>, path: impl Into<String>, x: i32, y: i32) -> Self {
        Self {
            id: id.into(),
            path: path.into(),
            fps: default_fps(),
            x,
            y,
            always_on_top: true,
            scale_percent: default_scale(),
            rotation: 0,
            flip_h: false,
            flip_v: false,
            locked: false,
            click_through: false,
            hover_hide: false,
        }
    }
}

/// Root document of `secondary_instances.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecondaryState {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub instances: Vec<Instance>,
}

impl Default for SecondaryState {
    fn default() -> Self {
        Self {
            enabled: true,
            instances: Vec::new(),
        }
    }
}

impl SecondaryState {
    /// Load from disk; a missing or malformed file yields defaults (matching the
    /// forgiving behaviour of `_json_load`).
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Persist atomically.
    pub fn save(&self, path: &Path) -> Result<()> {
        atomic_write_json(path, self)
    }

    /// Default location: `<XDG data>/AnimaEngine/secondary_instances.json`,
    /// overridable with `ANIMAENGINE_STATE_FILE`.
    pub fn user_path() -> PathBuf {
        if let Some(path) = std::env::var_os("ANIMAENGINE_STATE_FILE") {
            return PathBuf::from(path);
        }
        paths::user_data_dir().join(STATE_FILE)
    }

    /// Find an instance by id, falling back to its stored path.
    pub fn find_by_key(&self, key: &str) -> Option<&Instance> {
        self.instances
            .iter()
            .find(|i| i.id == key || (!i.path.is_empty() && i.path == key))
    }

    /// Insert or update the geometry (position + display scale) of `key`,
    /// preserving every other field. Returns whether the document changed.
    pub fn upsert_geometry(&mut self, key: &str, x: i32, y: i32, scale_percent: u32) -> bool {
        if let Some(instance) = self
            .instances
            .iter_mut()
            .find(|i| i.id == key || (!i.path.is_empty() && i.path == key))
        {
            if instance.x == x && instance.y == y && instance.scale_percent == scale_percent {
                return false;
            }
            instance.x = x;
            instance.y = y;
            instance.scale_percent = scale_percent;
            return true;
        }

        let mut instance = Instance::new(key, key, x, y);
        instance.scale_percent = scale_percent;
        self.instances.push(instance);
        true
    }

    pub fn find(&self, id: &str) -> Option<&Instance> {
        self.instances.iter().find(|i| i.id == id)
    }

    /// Insert `instance`, or replace the existing entry with the same id.
    pub fn upsert(&mut self, instance: Instance) {
        match self.instances.iter_mut().find(|i| i.id == instance.id) {
            Some(existing) => *existing = instance,
            None => self.instances.push(instance),
        }
    }

    /// Remove by id; returns whether anything was removed.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.instances.len();
        self.instances.retain(|i| i.id != id);
        self.instances.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let state = SecondaryState::load(&dir.path().join("absent.json"));
        assert!(state.enabled);
        assert!(state.instances.is_empty());
    }

    #[test]
    fn roundtrip_and_upsert_remove() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secondary_instances.json");

        let mut state = SecondaryState::default();
        state.upsert(Instance::new("a", "miku.apng", 10, 20));
        state.save(&path).unwrap();

        let mut loaded = SecondaryState::load(&path);
        assert_eq!(loaded.instances.len(), 1);

        // upsert replaces, not appends
        let mut mv = loaded.find("a").unwrap().clone();
        mv.x = 99;
        loaded.upsert(mv);
        assert_eq!(loaded.instances.len(), 1);
        assert_eq!(loaded.find("a").unwrap().x, 99);

        assert!(loaded.remove("a"));
        assert!(!loaded.remove("a"));
    }

    #[test]
    fn parses_python_written_shape() {
        let raw = r#"{
            "enabled": true,
            "instances": [
                {"id":"x","path":"S:\\w\\miku.apng","fps":15,"x":659,"y":399,
                 "always_on_top":true,"scale_percent":100,"rotation":0,
                 "flip_h":false,"flip_v":false}
            ]
        }"#;
        let state: SecondaryState = serde_json::from_str(raw).unwrap();
        let inst = state.find("x").unwrap();
        assert_eq!(inst.x, 659);
        assert_eq!(inst.fps, 15);
        assert!(inst.always_on_top);
    }

    #[test]
    fn upsert_geometry_creates_and_preserves() {
        let mut state = SecondaryState::default();
        assert!(state.upsert_geometry("/gifs/a.apng", 10, 20, 80));
        let inst = state.find_by_key("/gifs/a.apng").unwrap();
        assert_eq!((inst.x, inst.y, inst.scale_percent), (10, 20, 80));
        assert_eq!(inst.path, "/gifs/a.apng");

        // identical geometry is a no-op
        assert!(!state.upsert_geometry("/gifs/a.apng", 10, 20, 80));

        // other fields survive a geometry update
        state.instances[0].always_on_top = false;
        state.instances[0].fps = 30;
        assert!(state.upsert_geometry("/gifs/a.apng", 5, 6, 50));
        let inst = state.find_by_key("/gifs/a.apng").unwrap();
        assert_eq!((inst.x, inst.y, inst.scale_percent), (5, 6, 50));
        assert!(!inst.always_on_top);
        assert_eq!(inst.fps, 30);
        assert_eq!(state.instances.len(), 1);

        // matches by path when the id differs
        state.instances[0].id = "generated-id".into();
        assert!(state.upsert_geometry("/gifs/a.apng", 7, 8, 25));
        assert_eq!(state.instances.len(), 1);
        assert_eq!(state.find_by_key("/gifs/a.apng").unwrap().scale_percent, 25);
    }

    #[test]
    fn user_path_falls_back_to_state_file() {
        if std::env::var_os("ANIMAENGINE_STATE_FILE").is_none() {
            assert!(SecondaryState::user_path().ends_with(STATE_FILE));
        }
    }
}
