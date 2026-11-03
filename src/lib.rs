//! `anima-engine-rs` — Rust rewrite of AnimaEngine.
//!
//! This crate starts with the **pure logic** core: JSON state, filesystem
//! conventions, preferences and (next) animation decoding/caching. None of the
//! modules here depend on a windowing system or GPU, so they are unit-testable
//! on any platform. The always-on-top window/render layer is added later.

pub mod anim;
pub mod cache;
pub mod error;
pub mod i18n;
pub mod input_map;
pub mod library;
pub mod pack;
pub mod paths;
pub mod prefs;
pub mod settings;
pub mod state;
pub mod steam;
pub mod vdf;
pub mod workshop;
pub mod workshop_web;

pub use error::{Error, Result};
