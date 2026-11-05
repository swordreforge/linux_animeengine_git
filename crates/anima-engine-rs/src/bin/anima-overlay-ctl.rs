//! `anima-overlay-ctl` — control running `anima-overlay` instances.
//!
//! ```text
//! anima-overlay-ctl toggle                          # opaque pixels <-> click-through
//! anima-overlay-ctl mode <full|opaque|pass-through>
//! anima-overlay-ctl scale <percent|up|down>         # 10-100%, never upscales
//! anima-overlay-ctl region step <off|auto|1..32>      # input-region sampling
//! anima-overlay-ctl region soft <rects> | region max <rects>
//! anima-overlay-ctl lock | unlock                   # disable/enable dragging
//! anima-overlay-ctl hide | show | toggle-hidden     # hide keeps the surface alive (1x1)
//! anima-overlay-ctl reset                           # initial position + native size
//! anima-overlay-ctl quit
//! anima-overlay-ctl status                          # per-instance state (alias: --list)
//! ```
//!
//! By default a command applies to **every** running instance (each polls its own
//! control file); `--instance <animation path>` targets one. The tool only writes
//! control files, so it works whether or not the overlays are running.

use std::path::PathBuf;
use std::process::ExitCode;

use anima_engine_rs::control::{ControlState, InputMode};

/// Percent step for `scale up` / `scale down`.
const SCALE_STEP: i32 = 10;

enum ScaleAction {
    Set(u32),
    Nudge(i32),
}

fn main() -> ExitCode {
    let mut selector: Option<String> = None;
    let mut words: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--instance" | "-i" => match args.next() {
                Some(path) => selector = Some(path),
                None => return usage("[--instance <animation path>] <command>"),
            },
            "--all" => selector = None,
            _ => words.push(arg),
        }
    }

    let mut words = words.into_iter();
    let command = words.next();
    let paths = target_paths(selector.as_deref());

    let outcome = match command.as_deref() {
        Some("toggle") => apply_all(&paths, ControlState::toggle_click_through),
        Some("toggle-hidden") => apply_all(&paths, ControlState::toggle_hidden),
        Some("reset") => apply_all(&paths, ControlState::request_reset),
        Some("quit") => apply_all(&paths, ControlState::request_quit),
        Some("lock") => apply_all(&paths, |state| state.set_locked(true)),
        Some("unlock") => apply_all(&paths, |state| state.set_locked(false)),
        Some("hide") => apply_all(&paths, |state| state.set_hidden(true)),
        Some("show") => apply_all(&paths, |state| state.set_hidden(false)),
        Some("mode") => match words.next().as_deref().and_then(parse_mode) {
            Some(mode) => apply_all(&paths, move |state| state.set_input_mode(mode)),
            None => return usage("[--instance <path>] mode <full|opaque|pass-through>"),
        },
        Some("scale") => match words.next().as_deref().and_then(parse_scale) {
            Some(ScaleAction::Set(percent)) => {
                apply_all(&paths, move |state| state.set_scale_percent(percent))
            }
            Some(ScaleAction::Nudge(delta)) => {
                apply_all(&paths, move |state| state.nudge_scale(delta))
            }
            None => return usage("[--instance <path>] scale <percent|up|down>"),
        },
        Some("region") => match words.next().as_deref() {
            Some("step") => match words.next() {
                Some(value) => apply_all(&paths, move |state| state.set_region_step(&value)),
                None => return usage("[--instance <path>] region step <off|auto|1..32>"),
            },
            Some("soft") => match words.next().and_then(|value| value.parse::<usize>().ok()) {
                Some(value) => apply_all(&paths, move |state| state.set_region_soft_rects(value)),
                None => return usage("[--instance <path>] region soft <rects>"),
            },
            Some("max") => match words.next().and_then(|value| value.parse::<usize>().ok()) {
                Some(value) => apply_all(&paths, move |state| state.set_region_max_rects(value)),
                None => return usage("[--instance <path>] region max <rects>"),
            },
            _ => return usage("[--instance <path>] region <step|soft|max> <value>"),
        },
        Some("rotate") => match words.next().as_deref() {
            Some("cw") => apply_all(&paths, |state| state.rotate_by(90)),
            Some("ccw") => apply_all(&paths, |state| state.rotate_by(-90)),
            Some(value) => match value.trim_end_matches('°').parse::<i32>() {
                Ok(degrees) => apply_all(&paths, move |state| state.set_rotation(degrees)),
                Err(_) => return usage("[--instance <path>] rotate <deg|cw|ccw>"),
            },
            None => return usage("[--instance <path>] rotate <deg|cw|ccw>"),
        },
        Some("flip") => match words.next().as_deref() {
            Some("h") => apply_all(&paths, |state| state.toggle_flip_h()),
            Some("v") => apply_all(&paths, |state| state.toggle_flip_v()),
            _ => return usage("[--instance <path>] flip <h|v>"),
        },
        Some("status") | Some("--list") => {
            status(&paths);
            Ok(())
        }
        _ => {
            return usage(
                "[--instance <path>|--all] <toggle|toggle-hidden|mode <m>|scale <s>|\
                 region <step|soft|max>|lock|unlock|hide|show|reset|quit|status>",
            );
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

/// Control files a command applies to: one instance, or every instance (with a
/// fallback to the key-less file when none exist yet).
fn target_paths(selector: Option<&str>) -> Vec<PathBuf> {
    if let Some(path) = selector {
        let key = anima_engine_rs::paths::canonical_path(path);
        return vec![ControlState::path_for(Some(&key))];
    }
    let instances = ControlState::instances();
    if instances.is_empty() {
        vec![ControlState::path()]
    } else {
        instances
    }
}

fn apply_all(
    paths: &[PathBuf],
    action: impl Fn(ControlState) -> ControlState,
) -> Result<(), anima_engine_rs::Error> {
    for path in paths {
        action(ControlState::load(path)).save(path)?;
    }
    Ok(())
}

fn status(paths: &[PathBuf]) {
    use std::io::Write as _;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for path in paths {
        let state = ControlState::load(path);
        let mode = match state.input_mode {
            InputMode::Full => "full",
            InputMode::Opaque => "opaque",
            InputMode::PassThrough => "pass-through",
        };
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("unknown");
        // A closed pipe (e.g. `| head`) must not panic the CLI.
        let _ = writeln!(out, "{name}");
        let _ = writeln!(out, "  mode:   {mode}");
        let _ = writeln!(out, "  scale:  {}%", state.scale_percent);
        let _ = writeln!(out, "  locked: {}  hidden: {}", state.locked, state.hidden);
        let _ = writeln!(
            out,
            "  transform: {}°  flip_h {}  flip_v {}",
            state.rotation, state.flip_h, state.flip_v
        );
        let _ = writeln!(
            out,
            "  region: step {}  soft {}  max {}",
            state.region_step, state.region_soft_rects, state.region_max_rects
        );
        let _ = writeln!(out, "  gen:    {}", state.generation);
        let _ = writeln!(out, "  file:   {}", path.display());
    }
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
    use super::{ScaleAction, parse_mode, parse_scale, target_paths};
    use anima_engine_rs::control::{InputMode, instance_slug};

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

    #[test]
    fn selector_targets_one_instance() {
        let paths = target_paths(Some("/gifs/a.apng"));
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with(format!("{}.json", instance_slug("/gifs/a.apng"))));
    }
}
