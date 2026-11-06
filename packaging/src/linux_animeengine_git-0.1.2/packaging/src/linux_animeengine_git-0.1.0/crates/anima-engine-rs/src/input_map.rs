//! Input -> function bindings (`configTeclas.json`) and the `funciones` catalog.
//!
//! Ported from `deteccionDeTeclas.py` + `funciones.py`. `configTeclas.json` maps
//! an event string to a bound function:
//!
//! ```json
//! { "a": { "function": "hide", "activation_mode": "hold" } }
//! ```
//!
//! Event strings follow the Python convention: a bare character (`"a"`), a
//! special key (`"Key.f1"`), a mouse button (`"Button.left"`), or a gamepad
//! control (`"gamepad_BTN_SOUTH"`).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::paths::atomic_write_json;

pub const CONFIG_FILE: &str = "configTeclas.json";

/// Prefix used for gamepad event names.
pub const GAMEPAD_PREFIX: &str = "gamepad_";
/// Prefix used for mouse button event names (`pynput.mouse.Button`).
pub const MOUSE_BUTTON_PREFIX: &str = "Button.";

/// Functions exposed by `funciones.py`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Function {
    /// `hide` — toggle the animation window.
    Hide,
    /// `HideAudioGame` — hide and stop audio, or show and (re)play it.
    HideAudioGame,
}

/// Callables in `funciones.py`, in editor order.
pub const FUNCTION_NAMES: &[&str] = &["hide", "HideAudioGame"];

impl Function {
    pub fn name(self) -> &'static str {
        match self {
            Function::Hide => "hide",
            Function::HideAudioGame => "HideAudioGame",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "hide" => Some(Function::Hide),
            "HideAudioGame" => Some(Function::HideAudioGame),
            _ => None,
        }
    }
}

/// When the bound function runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivationMode {
    /// Fire on press.
    #[default]
    Press,
    /// Fire on press and again on release.
    Hold,
    /// An unrecognized mode; the binding never fires (matches Python's else-branch).
    Unknown,
}

impl<'de> Deserialize<'de> for ActivationMode {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(match raw.to_ascii_lowercase().as_str() {
            "press" => ActivationMode::Press,
            "hold" => ActivationMode::Hold,
            _ => ActivationMode::Unknown,
        })
    }
}

/// A single event binding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    pub function: String,
    #[serde(default)]
    pub activation_mode: ActivationMode,
}

/// Classifies an event string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Keyboard,
    Mouse,
    Gamepad,
}

/// Classify an event string the same way the Python listeners produce them.
pub fn classify_event(event: &str) -> EventKind {
    if event.starts_with(GAMEPAD_PREFIX) {
        EventKind::Gamepad
    } else if event.starts_with(MOUSE_BUTTON_PREFIX) {
        EventKind::Mouse
    } else {
        EventKind::Keyboard
    }
}

/// Build the canonical gamepad event name for a raw `inputs` code.
pub fn gamepad_event(code: &str) -> String {
    format!("{GAMEPAD_PREFIX}{code}")
}

/// The full event -> binding map.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Bindings(BTreeMap<String, Binding>);

impl Bindings {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load `configTeclas.json`. Missing or malformed entries are skipped rather
    /// than failing the whole file (matching the forgiving Python loader).
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let raw: BTreeMap<String, serde_json::Value> =
            serde_json::from_str(&text).unwrap_or_default();

        let mut bindings = BTreeMap::new();
        for (event, value) in raw {
            if let Ok(binding) = serde_json::from_value::<Binding>(value) {
                bindings.insert(event, binding);
            }
        }
        Self(bindings)
    }

    /// Persist atomically.
    pub fn save(&self, path: &Path) -> Result<()> {
        atomic_write_json(path, &self.0)
    }

    pub fn get(&self, event: &str) -> Option<&Binding> {
        self.0.get(event)
    }

    /// Bind `event` to `function`.
    pub fn bind(
        &mut self,
        event: impl Into<String>,
        function: impl Into<String>,
        mode: ActivationMode,
    ) {
        self.0.insert(
            event.into(),
            Binding {
                function: function.into(),
                activation_mode: mode,
            },
        );
    }

    /// Remove a binding; returns whether one existed.
    pub fn unbind(&mut self, event: &str) -> bool {
        self.0.remove(event).is_some()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Binding)> {
        self.0
            .iter()
            .map(|(event, binding)| (event.as_str(), binding))
    }

    /// Resolve an event to a known [`Function`] plus its activation mode; `None`
    /// when unbound or the function name is unknown.
    pub fn resolve(&self, event: &str) -> Option<(Function, ActivationMode)> {
        let binding = self.0.get(event)?;
        let function = Function::from_name(&binding.function)?;
        Some((function, binding.activation_mode))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, contents: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn parses_shipped_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            CONFIG_FILE,
            r#"{"a":{"function":"hide","activation_mode":"hold"}}"#,
        );
        let bindings = Bindings::load(&path);
        assert_eq!(bindings.len(), 1);
        let binding = bindings.get("a").unwrap();
        assert_eq!(binding.function, "hide");
        assert_eq!(binding.activation_mode, ActivationMode::Hold);
    }

    #[test]
    fn missing_activation_mode_defaults_to_press() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), CONFIG_FILE, r#"{"f1":{"function":"hide"}}"#);
        let bindings = Bindings::load(&path);
        assert_eq!(
            bindings.get("f1").unwrap().activation_mode,
            ActivationMode::Press
        );
    }

    #[test]
    fn unknown_activation_mode_is_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            CONFIG_FILE,
            r#"{"a":{"function":"hide","activation_mode":"squish"}}"#,
        );
        let bindings = Bindings::load(&path);
        assert_eq!(
            bindings.get("a").unwrap().activation_mode,
            ActivationMode::Unknown
        );
    }

    #[test]
    fn malformed_entries_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            CONFIG_FILE,
            r#"{"good":{"function":"hide"},"bad":"not an object","also_bad":{}}"#,
        );
        let bindings = Bindings::load(&path);
        assert_eq!(bindings.len(), 1);
        assert!(bindings.get("good").is_some());
        assert!(bindings.get("bad").is_none());
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Bindings::load(&dir.path().join("nope.json")).is_empty());
    }

    #[test]
    fn bind_unbind_and_resolve() {
        let mut bindings = Bindings::new();
        bindings.bind("Button.left", "HideAudioGame", ActivationMode::Press);
        assert_eq!(
            bindings.resolve("Button.left"),
            Some((Function::HideAudioGame, ActivationMode::Press))
        );

        // Unknown function names resolve to nothing, like Python's hasattr check.
        bindings.bind("b", "does_not_exist", ActivationMode::Press);
        assert_eq!(bindings.resolve("b"), None);
        assert_eq!(bindings.resolve("unbound"), None);

        assert!(bindings.unbind("Button.left"));
        assert!(!bindings.unbind("Button.left"));
    }

    #[test]
    fn classifies_events() {
        assert_eq!(classify_event("a"), EventKind::Keyboard);
        assert_eq!(classify_event("Key.f1"), EventKind::Keyboard);
        assert_eq!(classify_event("Button.left"), EventKind::Mouse);
        assert_eq!(
            classify_event(&gamepad_event("BTN_SOUTH")),
            EventKind::Gamepad
        );
        assert_eq!(gamepad_event("ABS_X"), "gamepad_ABS_X");
    }

    #[test]
    fn function_names_roundtrip() {
        for name in FUNCTION_NAMES {
            let function = Function::from_name(name).unwrap();
            assert_eq!(function.name(), *name);
        }
        assert_eq!(Function::from_name("nope"), None);
    }

    #[test]
    fn save_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);
        let mut bindings = Bindings::new();
        bindings.bind("a", "hide", ActivationMode::Hold);
        bindings.save(&path).unwrap();

        let loaded = Bindings::load(&path);
        assert_eq!(loaded, bindings);
    }
}
