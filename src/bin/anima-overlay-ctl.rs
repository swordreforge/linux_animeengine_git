//! `anima-overlay-ctl` — control a running `anima-overlay` from the command
//! line, e.g. from compositor keybindings.
//!
//! ```text
//! anima-overlay-ctl toggle                          # opaque pixels <-> click-through
//! anima-overlay-ctl mode <full|opaque|pass-through>
//! anima-overlay-ctl reset                           # initial position, opaque input
//! anima-overlay-ctl quit
//! anima-overlay-ctl status
//! ```
//!
//! It only writes the shared control file (`control::ControlState`); the overlay
//! polls it, so the tool works whether or not the overlay is currently running.

use std::path::Path;
use std::process::ExitCode;

use anima_engine_rs::control::{ControlState, InputMode};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let path = ControlState::path();

    let outcome = match args.next().as_deref() {
        Some("toggle") => update(&path, ControlState::toggle_click_through),
        Some("reset") => update(&path, ControlState::request_reset),
        Some("quit") => update(&path, ControlState::request_quit),
        Some("mode") => match args.next().as_deref().and_then(parse_mode) {
            Some(mode) => update(&path, move |state| state.set_input_mode(mode)),
            None => {
                eprintln!("usage: anima-overlay-ctl mode <full|opaque|pass-through>");
                return ExitCode::FAILURE;
            }
        },
        Some("status") => {
            status(&path);
            Ok(())
        }
        _ => {
            eprintln!(
                "usage: anima-overlay-ctl <toggle|mode <full|opaque|pass-through>|reset|quit|status>"
            );
            return ExitCode::FAILURE;
        }
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("anima-overlay-ctl: {err}");
            ExitCode::FAILURE
        }
    }
}

fn update(
    path: &Path,
    action: impl FnOnce(ControlState) -> ControlState,
) -> Result<(), anima_engine_rs::Error> {
    action(ControlState::load(path)).save(path)
}

fn status(path: &Path) {
    let state = ControlState::load(path);
    let mode = match state.input_mode {
        InputMode::Full => "full",
        InputMode::Opaque => "opaque",
        InputMode::PassThrough => "pass-through",
    };
    println!("mode:       {mode}");
    println!("generation: {}", state.generation);
    println!("file:       {}", path.display());
}

fn parse_mode(value: &str) -> Option<InputMode> {
    match value {
        "full" => Some(InputMode::Full),
        "opaque" | "pixels" => Some(InputMode::Opaque),
        "pass-through" | "through" | "off" => Some(InputMode::PassThrough),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_mode;
    use anima_engine_rs::control::InputMode;

    #[test]
    fn parses_input_modes() {
        assert_eq!(parse_mode("full"), Some(InputMode::Full));
        assert_eq!(parse_mode("opaque"), Some(InputMode::Opaque));
        assert_eq!(parse_mode("pixels"), Some(InputMode::Opaque));
        assert_eq!(parse_mode("pass-through"), Some(InputMode::PassThrough));
        assert_eq!(parse_mode("nope"), None);
    }
}
