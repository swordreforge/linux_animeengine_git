//! `anima-shepherd` — keep wanted animations running without a window.
//!
//! ```text
//! anima-shepherd                      # supervise (the default)
//! anima-shepherd run --once           # a single pass, for testing
//! anima-shepherd status               # what is wanted, what is up, where things live
//! anima-shepherd list                 # the wanted animations, one path per line
//! anima-shepherd enable <animation>   # keep it running from now on
//! anima-shepherd disable <animation>
//! anima-shepherd stop <animation>     # ask that instance to quit (control channel)
//! anima-shepherd quit                 # stop supervising; the overlays keep playing
//! ```
//!
//! It starts and stops processes — nothing else. Surfaces belong to the overlays, and a
//! shepherd restarting (or dying) never disturbs one that is already up. The registry is a
//! plain JSON file, so the main window can hand work over by editing it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use anima_engine_rs::control::{ControlState, write_marker};
use anima_engine_rs::paths;
use anima_engine_rs::shepherd::{self, Action, Registry, Supervisor};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let outcome: Result<(), String> = match args.first().map(String::as_str) {
        None | Some("run") => {
            return supervise(args.iter().any(|arg| arg == "--once"));
        }
        Some("status") => {
            status();
            Ok(())
        }
        Some("list") => {
            list();
            Ok(())
        }
        Some("enable") => with_animation(&args, |animation| set(animation, true)),
        Some("disable") => with_animation(&args, |animation| set(animation, false)),
        Some("stop") => with_animation(&args, stop),
        Some("quit") => quit(),
        Some("help" | "-h" | "--help") => {
            usage();
            Ok(())
        }
        Some(other) => Err(format!("unknown command: {other}")),
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("anima-shepherd: {message}");
            usage();
            ExitCode::FAILURE
        }
    }
}

fn usage() {
    eprintln!(
        "usage: anima-shepherd [run [--once]|status|list|enable <animation>|\
         disable <animation>|stop <animation>|quit]"
    );
}

fn with_animation(
    args: &[String],
    action: impl FnOnce(&str) -> Result<(), String>,
) -> Result<(), String> {
    let animation = args.get(1).ok_or("missing <animation>")?;
    action(animation)
}

fn set(animation: &str, enabled: bool) -> Result<(), String> {
    let path = shepherd::registry_path();
    let mut registry = Registry::load(&path);
    let changed = if enabled {
        registry.enable(animation)
    } else {
        registry.disable(animation)
    };
    if changed {
        registry.save(&path).map_err(|error| error.to_string())?;
    }
    println!(
        "{} {}",
        if enabled { "enabled" } else { "disabled" },
        paths::canonical_path(animation)
    );
    Ok(())
}

fn stop(animation: &str) -> Result<(), String> {
    let key = paths::canonical_path(animation);
    let control = ControlState::path_for(Some(&key));
    if !ControlState::is_running(&control) {
        return Err(format!("{key} is not running"));
    }
    ControlState::load(&control)
        .request_quit()
        .save(&control)
        .map_err(|error| error.to_string())?;
    println!("asked {key} to quit");
    Ok(())
}

fn quit() -> Result<(), String> {
    let marker = shepherd::marker_path();
    let pid = std::fs::read_to_string(&marker)
        .ok()
        .and_then(|text| {
            text.split_whitespace()
                .next()
                .and_then(|pid| pid.parse::<u32>().ok())
        })
        .ok_or("no shepherd is running")?;
    if !shepherd::is_up() {
        return Err(format!("{pid} is not a live shepherd"));
    }
    let status = Command::new("kill")
        .arg(pid.to_string())
        .status()
        .map_err(|error| error.to_string())?;
    if !status.success() {
        return Err(format!("could not signal {pid}"));
    }
    println!("asked shepherd {pid} to stop; the overlays keep playing");
    Ok(())
}

fn status() {
    let registry = Registry::load(&shepherd::registry_path());
    println!(
        "shepherd: {}",
        if shepherd::is_up() {
            "running"
        } else {
            "not running"
        }
    );
    println!("registry: {}", shepherd::registry_path().display());
    if registry.instances.is_empty() {
        println!("  (empty — `anima-shepherd enable <animation>`)");
    }
    for entry in &registry.instances {
        let control = ControlState::path_for(Some(&entry.path));
        println!(
            "  {} {}  {}",
            if entry.enabled { "on " } else { "off" },
            if ControlState::is_running(&control) {
                "●"
            } else {
                "○"
            },
            entry.path
        );
    }
}

fn list() {
    for entry in Registry::load(&shepherd::registry_path()).wanted() {
        println!("{}", entry.path);
    }
}

/// `anima-overlay` next to this binary first, then on `PATH`.
fn overlay_binary() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join("anima-overlay");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    anima_engine_rs::steamcmd::find_in_path("anima-overlay")
}

fn supervise(once: bool) -> ExitCode {
    let Some(overlay) = overlay_binary() else {
        eprintln!("anima-shepherd: anima-overlay not found — build or install it first");
        return ExitCode::FAILURE;
    };
    if let Err(error) = write_marker(&shepherd::marker_path(), "anima-shepherd") {
        eprintln!("anima-shepherd: could not announce liveness: {error}");
    }

    let registry_path = shepherd::registry_path();
    let mut supervisor = Supervisor::new(registry_path.clone(), Registry::load(&registry_path));
    eprintln!(
        "[shepherd] {} animation(s) wanted, overlay {}",
        supervisor.registry().wanted().count(),
        overlay.display()
    );

    let mut last_reap = Instant::now();
    // Children we started, so we can wait for them: a killed child that nobody waits for
    // becomes a zombie, and a zombie still appears in `/proc` with the same `comm` — which
    // would make `is_running` report the dead overlay as alive and stop the restart.
    let mut children: HashMap<String, std::process::Child> = HashMap::new();
    loop {
        let now = Instant::now();
        children.retain(|animation, child| match child.try_wait() {
            Ok(Some(status)) => {
                eprintln!("[shepherd] {animation} exited ({status})");
                false
            }
            Ok(None) => true,
            Err(error) => {
                eprintln!("[shepherd] could not wait for {animation}: {error}");
                false
            }
        });
        supervisor.reload();
        let actions = supervisor.poll(now, ControlState::is_running, |animation| {
            let child = Command::new(&overlay).arg(animation).spawn()?;
            children.insert(animation.to_string(), child);
            Ok(())
        });
        for action in &actions {
            match action {
                Action::Started(path) => eprintln!("[shepherd] started {path}"),
                Action::Exited { path, failures } => {
                    eprintln!("[shepherd] {path} exited early ({failures} time(s))");
                }
                Action::SpawnFailed { path, error } => {
                    eprintln!("[shepherd] could not start {path}: {error}");
                }
            }
        }

        // Markers of processes that are gone are only ever tidied here.
        if now.duration_since(last_reap) >= Duration::from_secs(60) {
            last_reap = now;
            for marker in shepherd::stale_markers() {
                if let Err(error) = std::fs::remove_file(&marker) {
                    eprintln!("[shepherd] could not remove {}: {error}", marker.display());
                }
            }
        }

        if once {
            return ExitCode::SUCCESS;
        }
        std::thread::sleep(shepherd::POLL);
    }
}
