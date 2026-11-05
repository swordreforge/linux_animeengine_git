//! `anima-engine-rs` — Rust rewrite of AnimaEngine.
//!
//! This crate starts with the **pure logic** core: JSON state, filesystem
//! conventions, preferences and (next) animation decoding/caching. None of the
//! modules here depend on a windowing system or GPU, so they are unit-testable
//! on any platform. The always-on-top window/render layer is added later.

// A public type a caller cannot print is a public type a caller cannot debug. `#[expect]` with a reason is
// the way to exempt one, not `#[allow]`.
#![deny(missing_debug_implementations)]

pub mod anim;
#[cfg(feature = "bc7")]
pub mod bc7;
pub mod cache;
pub mod control;
#[cfg(feature = "dbus")]
pub mod dbus_intent;
pub mod error;
pub mod ffmpeg;
pub mod i18n;
pub mod input_map;
pub mod keying;
pub mod library;
pub mod log;
pub mod pack;
pub mod paths;
pub mod prefs;
pub mod settings;
pub mod shepherd;
pub mod state;
pub mod steam;
pub mod steamcmd;
pub mod telemetry;
pub mod vdf;
pub mod workshop;
pub mod workshop_web;

pub use error::{Error, Result};
