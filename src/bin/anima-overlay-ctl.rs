//! `anima-overlay-ctl` — control a running `anima-overlay` from the command
//! line, e.g. from compositor keybindings.
//!
//! ```text
//! anima-overlay-ctl toggle                          # opaque pixels <-> click-through
//! anima-overlay-ctl mode <full|opaque|pass-through>
//! anima-overlay-ctl scale <percent|up|down>         # 10-100%, never upscales
//! anima-overlay-ctl reset                           # initial position + native size
//! anima-overlay-ctl quit
//! anima-overlay-ctl status
//! ```
//!
//! It only writes the shared control file (`control::ControlState`); the overlay
//! polls it, so the tool works whether or not the overlay is currently running.

use std::path::Path;
use std::process::ExitCode;

use anima_engine_rs::control::{ControlState, InputMode};

/// Percent step for `scale up` / `scale down`.
const SCALE_STEP: i32 = 10;

enum ScaleAction {
    Set(u32),
    Nudge(i32),
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let path = ControlState::path();

    let outcome = match args.next().as_deref() {
        Some("toggle") => update(&path, ControlState::toggle_click_through),
        Some("reset") => update(&path, ControlState::request_reset),
        Some("quit") => update(&path, ControlState::request_quit),
        Some("mode") => match args.next().as_deref().and_then(parse_mode) {
            Some(mode) => update(&path, move |state| state.set_input_mode(mode)),
            None => return usage("mode <full|opaque|pass-through>"),
        },
        Some("scale") => match args.next().as_deref().and_then(parse_scale) {
            Some(ScaleAction::Set(percent)) => {
                update(&path, move |state| state.set_scale_percent(percent))
            }
            Some(ScaleAction::Nudge(delta)) => update(&path, move |state| state.nudge_scale(delta)),
            None => return usage("scale <percent|up|down>"),
        },
        Some("status") => {
            status(&path);
            Ok(())
        }
        _ => {
            return usage("<toggle|mode <m>|scale <s>|reset|quit|status>");
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

fn usage(args: &str) -> ExitCode {
    eprintln!("usage: anima-overlay-ctl {args}");
    ExitCode::FAILURE
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
    println!("scale:      {}%", state.scale_percent);
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

fn parse_scale(value: &str) -> Option<ScaleAction> {
    match value {
        "up" => Some(ScaleAction::Nudge(SCALE_STEP)),
        "down" => Some(ScaleAction::Nudge(-SCALE_STEP)),
        other => other.parse::<u32>().ok().map(ScaleAction::Set),
    }
}

#[cfg(test)]
mod tests {
    use super::{ScaleAction, parse_mode, parse_scale};
    use anima_engine_rs::control::InputMode;

    #[test]
    fn parses_input_modes() {
        assert_eq!(parse_mode("full"), Some(InputMode::Full));
        assert_eq!(parse_mode("opaque"), Some(InputMode::Opaque));
        assert_eq!(parse_mode("pixels"), Some(InputMode::Opaque));
        assert_eq!(parse_mode("pass-through"), Some(InputMode::PassThrough));
        assert_eq!(parse_mode("nope"), None);
    }

    #[test]
    fn parses_scale_actions() {
        assert!(matches!(parse_scale("up"), Some(ScaleAction::Nudge(10))));
        assert!(matches!(parse_scale("down"), Some(ScaleAction::Nudge(-10))));
        assert!(matches!(parse_scale("50"), Some(ScaleAction::Set(50))));
        assert!(matches!(parse_scale("250"), Some(ScaleAction::Set(250))));
        assert!(parse_scale("big").is_none());
    }
}
