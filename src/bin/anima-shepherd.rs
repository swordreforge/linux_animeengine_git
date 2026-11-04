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

/// One tray icon for every overlay, instead of one per overlay.
///
/// The menu is rebuilt from the registry and the liveness markers each time the host asks,
/// so it shows the truth on disk rather than a cached idea of it.
struct ShepherdTray {
    registry_path: PathBuf,
    studio: Option<PathBuf>,
}

impl ksni::Tray for ShepherdTray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "anima-engine-shepherd".into()
    }

    fn title(&self) -> String {
        "AnimaEngine".into()
    }

    fn icon_name(&self) -> String {
        "applications-multimedia".into()
    }

    fn menu(&self) -> Vec<ksni::menu::MenuItem<Self>> {
        use ksni::menu::{CheckmarkItem, MenuItem, StandardItem, SubMenu};

        let registry = Registry::load(&self.registry_path);
        let live = shepherd::live_instances();

        // Union of "what should run" and "what is on disk": a manager has to show both, or an
        // animation the user started by hand would be invisible here.
        let mut rows: Vec<(String, Option<String>, bool, bool, PathBuf)> = Vec::new();
        for entry in &registry.instances {
            let control = ControlState::path_for(Some(&entry.path));
            rows.push((
                entry.path.clone(),
                Some(entry.path.clone()),
                entry.enabled,
                ControlState::is_running(&control),
                control,
            ));
        }
        for instance in &live {
            let name = instance
                .animation
                .clone()
                .unwrap_or_else(|| instance.label());
            if rows.iter().any(|(path, ..)| *path == name) {
                continue;
            }
            rows.push((
                name,
                instance.animation.clone(),
                false,
                instance.running,
                instance.control.clone(),
            ));
        }
        rows.sort_by(|a, b| b.3.cmp(&a.3).then_with(|| a.0.cmp(&b.0)));

        let mut items = Vec::new();
        for (name, animation, enabled, running, control) in rows {
            let suffix = match (enabled, running) {
                (true, _) => "",
                (false, true) => " · not supervised",
                (false, false) => " · off",
            };
            let label = format!(
                "{} {}{suffix}",
                if running { "●" } else { "○" },
                name.rsplit('/').next().unwrap_or(&name)
            );
            let mut children = Vec::new();

            match &animation {
                Some(animation) => {
                    if !enabled || !running {
                        let animation = animation.clone();
                        let registry_path = self.registry_path.clone();
                        children.push(MenuItem::Standard(StandardItem {
                            label: if running {
                                "Keep supervised".into()
                            } else {
                                "Start (and supervise)".into()
                            },
                            activate: Box::new(move |_: &mut Self| {
                                let mut registry = Registry::load(&registry_path);
                                registry.enable(&animation);
                                if let Err(error) = registry.save(&registry_path) {
                                    eprintln!("[shepherd] registry write failed: {error}");
                                }
                            }),
                            ..Default::default()
                        }));
                    }
                    if running {
                        let path = ControlState::path_for(Some(animation));
                        let state = ControlState::load(&path);
                        let toggle = path.clone();
                        children.push(MenuItem::Checkmark(CheckmarkItem {
                            label: "Visible".into(),
                            checked: !state.hidden,
                            activate: Box::new(move |_: &mut Self| {
                                let next = ControlState::load(&toggle).toggle_hidden();
                                if let Err(error) = next.save(&toggle) {
                                    eprintln!("[shepherd] tray write failed: {error}");
                                }
                            }),
                            ..Default::default()
                        }));
                        children.push(MenuItem::Standard(StandardItem {
                            label: "Quit this overlay".into(),
                            activate: Box::new(move |_: &mut Self| {
                                let next = ControlState::load(&path).request_quit();
                                if let Err(error) = next.save(&path) {
                                    eprintln!("[shepherd] tray write failed: {error}");
                                }
                            }),
                            ..Default::default()
                        }));
                    }
                    if enabled {
                        let animation = animation.clone();
                        let registry_path = self.registry_path.clone();
                        children.push(MenuItem::Standard(StandardItem {
                            label: "Stop supervising".into(),
                            activate: Box::new(move |_: &mut Self| {
                                let mut registry = Registry::load(&registry_path);
                                registry.disable(&animation);
                                if let Err(error) = registry.save(&registry_path) {
                                    eprintln!("[shepherd] registry write failed: {error}");
                                }
                            }),
                            ..Default::default()
                        }));
                    }
                }
                None => {
                    // A control file the persisted list cannot explain: nothing to
                    // supervise, but quitting it is still useful.
                    let quit = control.clone();
                    children.push(MenuItem::Standard(StandardItem {
                        label: "Quit this overlay".into(),
                        activate: Box::new(move |_: &mut Self| {
                            let next = ControlState::load(&quit).request_quit();
                            if let Err(error) = next.save(&quit) {
                                eprintln!("[shepherd] tray write failed: {error}");
                            }
                        }),
                        ..Default::default()
                    }));
                }
            }

            items.push(MenuItem::SubMenu(SubMenu {
                label,
                submenu: children,
                ..Default::default()
            }));
        }

        if items.is_empty() {
            items.push(MenuItem::Standard(StandardItem {
                label: "Nothing yet — start an overlay, or anima-shepherd enable <animation>"
                    .into(),
                ..Default::default()
            }));
        }

        items.push(MenuItem::Separator);
        match &self.studio {
            Some(studio) => {
                let studio = studio.clone();
                items.push(MenuItem::Standard(StandardItem {
                    label: "Open main window".into(),
                    activate: Box::new(move |_: &mut Self| {
                        if let Err(error) = std::process::Command::new(&studio).spawn() {
                            eprintln!("[shepherd] could not open {}: {error}", studio.display());
                        }
                    }),
                    ..Default::default()
                }));
            }
            None => items.push(MenuItem::Standard(StandardItem {
                label: "Open main window (not installed — build --features studio)".into(),
                ..Default::default()
            })),
        }
        items.push(MenuItem::Standard(StandardItem {
            label: "Quit shepherd (overlays keep playing)".into(),
            activate: Box::new(|_: &mut Self| std::process::exit(0)),
            ..Default::default()
        }));
        items
    }
}

/// Register the tray. On a desktop without a `StatusNotifierWatcher` this logs and the
/// shepherd carries on headless.
fn spawn_tray(registry_path: PathBuf) -> Option<ksni::blocking::Handle<ShepherdTray>> {
    use ksni::blocking::TrayMethods;

    let tray = ShepherdTray {
        registry_path,
        studio: sibling_binary("anima-studio"),
    };
    match &tray.studio {
        Some(studio) => eprintln!("[shepherd] main window: {}", studio.display()),
        None => eprintln!(
            "[shepherd] main window not found — build with --features studio or install it"
        ),
    }
    match tray
        .disable_dbus_name(false)
        .assume_sni_available(true)
        .spawn()
    {
        Ok(handle) => {
            eprintln!("[shepherd] tray registered (StatusNotifierItem)");
            Some(handle)
        }
        Err(error) => {
            eprintln!("[shepherd] tray unavailable (no StatusNotifierWatcher?): {error}");
            None
        }
    }
}

/// Ask the host to re-read the menu (the closure runs there first).
fn refresh_tray(tray: &Option<ksni::blocking::Handle<ShepherdTray>>) {
    if let Some(tray) = tray {
        let _ = tray.update(|_| ());
    }
}

/// `name` next to this binary first, then on `PATH`.
fn sibling_binary(name: &str) -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    anima_engine_rs::steamcmd::find_in_path(name)
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

    // One icon for all of them; a single pass (`--once`) does not need one.
    let tray = if once {
        None
    } else {
        spawn_tray(registry_path.clone())
    };

    let mut last_reap = Instant::now();
    let mut last_tray = Instant::now();
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
            // The shepherd owns the tray now, so the overlays do not register their own.
            let child = Command::new(&overlay)
                .arg("--no-tray")
                .arg(animation)
                .spawn()?;
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

        // Keep the icon honest: right away after a change, and slowly otherwise (the ●/○
        // dots also move when something exits without this process starting it).
        if !actions.is_empty() || now.duration_since(last_tray) >= Duration::from_secs(5) {
            last_tray = now;
            refresh_tray(&tray);
        }

        if once {
            return ExitCode::SUCCESS;
        }
        std::thread::sleep(shepherd::POLL);
    }
}
