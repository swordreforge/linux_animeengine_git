//! Application settings and the small auxiliary config files.
//!
//! These are the scattered `*.json` files the Python app kept next to the
//! executable:
//!
//! - `settings.json`         — the [`Settings`] document (UI/behavior flags).
//! - `anim_meta.json`        — per-animation `fps` overrides.
//! - `window_position.json`  — last overlay position.
//! - `user.json`             — the [`UserProfile`].
//! - `audioGifRegistro.json` — animation path -> audio path.
//! - `last_selected.json`    — last selection per kind.
//! - `config.json`           — enabled [`FeatureFlags`].

use std::collections::BTreeMap;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::Result;

pub const SETTINGS_FILE: &str = "settings.json";
/// The main window's own settings, kept apart from the game's [`Settings`].
pub const STUDIO_SETTINGS_FILE: &str = "studio.json";
pub const ANIM_META_FILE: &str = "anim_meta.json";
pub const WINDOW_POSITION_FILE: &str = "window_position.json";
pub const USER_FILE: &str = "user.json";
pub const AUDIO_REGISTRY_FILE: &str = "audioGifRegistro.json";
pub const LAST_SELECTED_FILE: &str = "last_selected.json";
pub const CONFIG_FILE: &str = "config.json";

/// A JSON config file: forgiving load + atomic save with sensible defaults.
pub trait ConfigFile: Serialize + DeserializeOwned + Default {
    fn load(path: &Path) -> Self {
        crate::paths::load_json(path)
    }

    fn save(&self, path: &Path) -> Result<()> {
        crate::paths::save_json(path, self)
    }
}

fn default_true() -> bool {
    true
}

fn default_sound_level() -> f32 {
    0.95
}

fn default_fps() -> String {
    "30".to_string()
}

fn default_canvas_mode() -> String {
    "primary".to_string()
}

/// `settings.json`. Unknown keys are preserved in [`Settings::extra`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub use_gif_with_audio: bool,
    #[serde(default)]
    pub play_on_touch: bool,
    #[serde(default = "default_sound_level")]
    pub sound_level: f32,
    #[serde(default)]
    pub play_if_visible: bool,
    #[serde(default, rename = "fullPantalla")]
    pub full_pantalla: bool,
    #[serde(default = "default_true")]
    pub lock_soltado: bool,
    #[serde(default = "default_true")]
    pub lock_movido: bool,
    #[serde(default = "default_true")]
    pub lock_agarrado: bool,
    #[serde(default = "default_true")]
    pub lock_estatico: bool,
    #[serde(default, rename = "seguimientoRaton")]
    pub seguimiento_raton: bool,
    #[serde(default = "default_fps", rename = "FPS")]
    pub fps: String,
    #[serde(default, rename = "ocultarAnima")]
    pub ocultar_anima: bool,
    #[serde(default)]
    pub first_run_tour_done: bool,
    #[serde(default)]
    pub tray_hint_seen: bool,
    #[serde(default)]
    pub start_minimized_to_tray: bool,
    #[serde(default)]
    pub obs_enabled: bool,
    #[serde(default = "default_true")]
    pub obs_keep_visible_when_desktop_hidden: bool,
    #[serde(default = "default_canvas_mode")]
    pub obs_canvas_mode: String,
    #[serde(default)]
    pub gif_auto_convert_apng: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            use_gif_with_audio: false,
            play_on_touch: false,
            sound_level: default_sound_level(),
            play_if_visible: false,
            full_pantalla: false,
            lock_soltado: true,
            lock_movido: true,
            lock_agarrado: true,
            lock_estatico: true,
            seguimiento_raton: false,
            fps: default_fps(),
            ocultar_anima: false,
            first_run_tour_done: false,
            tray_hint_seen: false,
            start_minimized_to_tray: false,
            obs_enabled: false,
            obs_keep_visible_when_desktop_hidden: true,
            obs_canvas_mode: default_canvas_mode(),
            gif_auto_convert_apng: false,
            extra: BTreeMap::new(),
        }
    }
}

impl ConfigFile for Settings {}

/// `studio.json` — settings for the main window (`anima-studio`), as opposed to the game's
/// [`Settings`].
///
/// The Steam Web API key lives here rather than only in the environment so the window works when
/// it is started from a desktop entry or a Flatpak, where no shell has exported anything. The
/// environment still wins when it is set: it is the deliberate, temporary override.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StudioSettings {
    /// Get one at <https://steamcommunity.com/dev/apikey>; only browsing the Workshop needs it.
    #[serde(default)]
    pub steam_api_key: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl ConfigFile for StudioSettings {}

/// One entry of `anim_meta.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AnimMetaEntry {
    #[serde(default)]
    pub fps: u32,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// `anim_meta.json` — animation path -> per-path overrides.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AnimMeta(pub BTreeMap<String, AnimMetaEntry>);

impl AnimMeta {
    /// The configured `fps` for `path`, if any.
    pub fn fps_for(&self, path: &str) -> Option<u32> {
        self.0.get(path).map(|entry| entry.fps)
    }

    pub fn set_fps(&mut self, path: impl Into<String>, fps: u32) {
        self.0.entry(path.into()).or_default().fps = fps;
    }
}

impl ConfigFile for AnimMeta {}

/// `window_position.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowPosition {
    pub x: i32,
    pub y: i32,
}

impl Default for WindowPosition {
    fn default() -> Self {
        Self { x: 500, y: 500 }
    }
}

impl ConfigFile for WindowPosition {}

/// `user.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UserProfile {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub vali: bool,
}

impl ConfigFile for UserProfile {}

/// `audioGifRegistro.json` — animation path -> audio path.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AudioRegistry(pub BTreeMap<String, String>);

impl AudioRegistry {
    pub fn audio_for(&self, animation: &str) -> Option<&str> {
        self.0.get(animation).map(String::as_str)
    }
}

impl ConfigFile for AudioRegistry {}

/// `last_selected.json` — kind (`"gif"`, `"sprite"`, …) -> last path.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LastSelected(pub BTreeMap<String, String>);

impl LastSelected {
    pub fn get(&self, kind: &str) -> Option<&str> {
        self.0.get(kind).map(String::as_str)
    }

    pub fn set(&mut self, kind: impl Into<String>, path: impl Into<String>) {
        self.0.insert(kind.into(), path.into());
    }
}

impl ConfigFile for LastSelected {}

/// `config.json` — the list of enabled feature names.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FeatureFlags(pub Vec<String>);

impl FeatureFlags {
    pub fn has(&self, flag: &str) -> bool {
        self.0.iter().any(|name| name == flag)
    }
}

impl ConfigFile for FeatureFlags {}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, contents: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    const SHIPPED_SETTINGS: &str = r#"{
      "use_gif_with_audio": false,
      "play_on_touch": false,
      "sound_level": 0.95,
      "play_if_visible": false,
      "fullPantalla": false,
      "lock_soltado": true,
      "lock_movido": true,
      "lock_agarrado": true,
      "lock_estatico": true,
      "seguimientoRaton": false,
      "FPS": "30",
      "ocultarAnima": false,
      "first_run_tour_done": true,
      "tray_hint_seen": false,
      "start_minimized_to_tray": false,
      "obs_enabled": false,
      "obs_keep_visible_when_desktop_hidden": true,
      "obs_canvas_mode": "primary",
      "gif_auto_convert_apng": false
    }"#;

    #[test]
    fn parses_shipped_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), SETTINGS_FILE, SHIPPED_SETTINGS);
        let settings = Settings::load(&path);

        assert_eq!(settings.sound_level, 0.95);
        assert_eq!(settings.fps, "30");
        assert!(settings.lock_movido);
        assert!(settings.obs_keep_visible_when_desktop_hidden);
        assert_eq!(settings.obs_canvas_mode, "primary");
        assert!(settings.first_run_tour_done);
        assert!(!settings.obs_enabled);
        assert!(settings.extra.is_empty());
    }

    #[test]
    fn settings_defaults_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings::load(&dir.path().join("nope.json"));
        assert_eq!(settings, Settings::default());
        assert_eq!(settings.fps, "30");
        assert_eq!(settings.sound_level, 0.95);
        assert!(settings.lock_estatico);
        // Fresh install has not seen the tour yet.
        assert!(!settings.first_run_tour_done);
    }

    #[test]
    fn settings_preserve_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            SETTINGS_FILE,
            r#"{"FPS":"60","future_flag":true,"nested":{"a":1}}"#,
        );
        let settings = Settings::load(&path);
        assert_eq!(settings.fps, "60");
        assert_eq!(settings.extra["future_flag"], serde_json::json!(true));

        settings.save(&path).unwrap();
        let reloaded = Settings::load(&path);
        assert_eq!(reloaded.extra["nested"]["a"], 1);
        assert_eq!(reloaded.fps, "60");
    }

    #[test]
    fn studio_settings_roundtrip_and_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STUDIO_SETTINGS_FILE);

        // No file yet: defaults, and no key.
        assert_eq!(StudioSettings::load(&path).steam_api_key, "");

        let settings = StudioSettings {
            steam_api_key: "ABC123".to_string(),
            ..StudioSettings::default()
        };
        settings.save(&path).unwrap();
        assert_eq!(StudioSettings::load(&path).steam_api_key, "ABC123");

        // Unknown keys survive a load/save cycle, like `settings.json` does.
        let path = write(
            dir.path(),
            STUDIO_SETTINGS_FILE,
            r#"{"steam_api_key":"K","later":7}"#,
        );
        let loaded = StudioSettings::load(&path);
        assert_eq!(loaded.steam_api_key, "K");
        loaded.save(&path).unwrap();
        assert_eq!(
            StudioSettings::load(&path).extra["later"],
            serde_json::json!(7)
        );
    }

    #[test]
    fn parses_anim_meta() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            ANIM_META_FILE,
            r#"{"miku.apng":{"fps":30},"teto.apng":{"fps":15}}"#,
        );
        let mut meta = AnimMeta::load(&path);
        assert_eq!(meta.fps_for("miku.apng"), Some(30));
        assert_eq!(meta.fps_for("absent.apng"), None);

        meta.set_fps("new.apng", 24);
        meta.save(&path).unwrap();
        assert_eq!(AnimMeta::load(&path).fps_for("new.apng"), Some(24));
    }

    #[test]
    fn window_position_default_and_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(WINDOW_POSITION_FILE);
        assert_eq!(
            WindowPosition::load(&path),
            WindowPosition { x: 500, y: 500 }
        );

        WindowPosition { x: 10, y: 20 }.save(&path).unwrap();
        assert_eq!(WindowPosition::load(&path), WindowPosition { x: 10, y: 20 });
    }

    #[test]
    fn user_profile_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            USER_FILE,
            r#"{"username":"marroindustries","vali":true}"#,
        );
        let profile = UserProfile::load(&path);
        assert_eq!(profile.username, "marroindustries");
        assert!(profile.vali);
    }

    #[test]
    fn audio_and_last_selected_maps() {
        let dir = tempfile::tempdir().unwrap();

        let audio_path = write(dir.path(), AUDIO_REGISTRY_FILE, r#"{"a.gif":"song.mp3"}"#);
        let audio = AudioRegistry::load(&audio_path);
        assert_eq!(audio.audio_for("a.gif"), Some("song.mp3"));

        let mut selected = LastSelected::default();
        selected.set("gif", "miku.apng");
        assert_eq!(selected.get("gif"), Some("miku.apng"));
        let sel_path = dir.path().join(LAST_SELECTED_FILE);
        selected.save(&sel_path).unwrap();
        assert_eq!(LastSelected::load(&sel_path).get("gif"), Some("miku.apng"));
    }

    #[test]
    fn feature_flags_from_shipped_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            CONFIG_FILE,
            r#"["iniciar","lineal","automatica","cache","memory"]"#,
        );
        let flags = FeatureFlags::load(&path);
        assert!(flags.has("iniciar"));
        assert!(flags.has("memory"));
        assert!(!flags.has("absent"));
    }
}
