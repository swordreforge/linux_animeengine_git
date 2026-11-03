//! Secondary overlay instance state (`secondary_instances.json`).
//!
//! Ported from the `_json_*` helpers in `monoModule.py`: the file tracks one
//! entry per running animation so positions and interaction toggles survive
//! restarts.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::paths::atomic_write_json;

fn default_true() -> bool {
    true
}

fn default_fps() -> u32 {
    15
}

fn default_scale() -> u32 {
    100
}

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
}
