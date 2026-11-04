//! Overlay lifecycle: one long-lived process that keeps wanted animations running.
//!
//! The tray lives inside each overlay, so it dies with it — and once the main window is
//! closed nothing can bring an animation back. That is the hole behind "open once, quit
//! from the tray, then Play does nothing". The shepherd owns the lifecycle instead:
//!
//! * `shepherd.json` lists the animations that should be running (`enable` / `disable`);
//! * every [`POLL`] it asks [`ControlState::is_running`] for each wanted animation and
//!   starts the ones that are not up, backing off when one keeps dying;
//! * it announces itself through `<data>/shepherd.pid`, so the window can tell whether a
//!   supervisor is up before deciding who starts an overlay.
//!
//! It never paints anything and never touches an overlay's surface: starting the process is
//! the whole job — the same thing the window's Play button does when no shepherd runs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::control::ControlState;
use crate::paths;

/// Registry file inside [`paths::user_data_dir`].
pub const REGISTRY_FILE: &str = "shepherd.json";
/// Marker holding the shepherd's own pid.
pub const MARKER_FILE: &str = "shepherd.pid";
/// How long between supervision passes.
pub const POLL: Duration = Duration::from_secs(1);
/// A start that dies sooner than this counts as a failure.
const EARLY_EXIT: Duration = Duration::from_secs(10);
/// Wait before the next attempt, by consecutive failure count (index 0 = right away).
const BACKOFF: [Duration; 4] = [
    Duration::ZERO,
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(120),
];

pub fn registry_path() -> PathBuf {
    paths::user_data_dir().join(REGISTRY_FILE)
}

pub fn marker_path() -> PathBuf {
    paths::user_data_dir().join(MARKER_FILE)
}

/// Is a shepherd running? The window asks this before deciding how to start an overlay.
pub fn is_up() -> bool {
    crate::control::marker_running(&marker_path())
}

/// One wanted animation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub path: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

/// The animations the shepherd should keep running.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub instances: Vec<Entry>,
}

impl Registry {
    pub fn load(path: &Path) -> Self {
        paths::load_json(path)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        paths::atomic_write_json(path, self)
    }

    /// Mark an animation as wanted. Returns whether anything changed.
    pub fn enable(&mut self, animation: &str) -> bool {
        let key = paths::canonical_path(animation);
        self.set(&key, true)
    }

    /// Mark an animation as not wanted. The entry stays, so its state is remembered.
    pub fn disable(&mut self, animation: &str) -> bool {
        let key = paths::canonical_path(animation);
        self.set(&key, false)
    }

    fn set(&mut self, key: &str, enabled: bool) -> bool {
        match self.instances.iter_mut().find(|entry| entry.path == key) {
            Some(entry) if entry.enabled == enabled => false,
            Some(entry) => {
                entry.enabled = enabled;
                true
            }
            None => {
                self.instances.push(Entry {
                    path: key.to_string(),
                    enabled,
                });
                true
            }
        }
    }

    pub fn get(&self, animation: &str) -> Option<&Entry> {
        let key = paths::canonical_path(animation);
        self.instances.iter().find(|entry| entry.path == key)
    }

    pub fn wanted(&self) -> impl Iterator<Item = &Entry> {
        self.instances.iter().filter(|entry| entry.enabled)
    }
}

/// What one pass did, so the caller can log it and tests can assert on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Started(String),
    /// We started it and it is gone again within [`EARLY_EXIT`].
    Exited {
        path: String,
        failures: u32,
    },
    SpawnFailed {
        path: String,
        error: String,
    },
    /// The user quit it on purpose (the overlay's menu, the tray, the CLI, the window), so
    /// supervision was turned off instead of restarting it.
    StoppedOnPurpose(String),
}

/// Per-animation bookkeeping between passes.
#[derive(Debug, Clone, Copy, Default)]
struct Progress {
    started: Option<Instant>,
    failures: u32,
    wait_until: Option<Instant>,
    /// Whether this supervisor has seen the instance alive. A `quit` flag only counts as
    /// "the user meant it" if we did — otherwise it is a leftover from a past session.
    seen_running: bool,
}

/// The supervision policy. Process handling is injected — `is_running` takes a control
/// file, `spawn` takes an animation — so the whole thing is testable without processes.
pub struct Supervisor {
    registry_path: PathBuf,
    registry: Registry,
    progress: HashMap<String, Progress>,
}

impl Supervisor {
    pub fn new(registry_path: PathBuf, registry: Registry) -> Self {
        Self {
            registry_path,
            registry,
            progress: HashMap::new(),
        }
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Re-read the registry (the CLI and the window both edit that file). Per-animation
    /// progress is kept, so enabling an entry does not wipe a running backoff.
    pub fn reload(&mut self) {
        self.registry = Registry::load(&self.registry_path);
    }

    /// One pass: bring the wanted animations up, and back off when one keeps dying.
    pub fn poll<P, S>(&mut self, now: Instant, is_running: P, mut spawn: S) -> Vec<Action>
    where
        P: Fn(&Path) -> bool,
        S: FnMut(&str) -> std::io::Result<()>,
    {
        let entries = self.registry.instances.clone();
        let mut actions = Vec::new();

        for entry in entries {
            if !entry.enabled {
                continue;
            }
            let control = ControlState::path_for(Some(&entry.path));
            let progress = self.progress.entry(entry.path.clone()).or_default();

            if is_running(&control) {
                progress.started = None;
                progress.failures = 0;
                progress.wait_until = None;
                progress.seen_running = true;
                continue;
            }

            // An intentional quit leaves `quit: true` behind (every path — the overlay's own
            // menu, the tray, the CLI, the window — goes through `request_quit`). Honouring it
            // is the difference between keeping an instance up and fighting the user, who
            // closed it and does not expect it to come back.
            let state = ControlState::load(&control);
            if state.quit {
                let mut cleared = state.clone();
                cleared.quit = false;
                if let Err(error) = cleared.save(&control) {
                    eprintln!("[shepherd] could not clear the quit flag: {error}");
                }
                if progress.seen_running {
                    progress.seen_running = false;
                    progress.started = None;
                    progress.failures = 0;
                    progress.wait_until = None;
                    let mut registry = Registry::load(&self.registry_path);
                    registry.disable(&entry.path);
                    if let Err(error) = registry.save(&self.registry_path) {
                        eprintln!("[shepherd] could not update the registry: {error}");
                    }
                    if let Some(found) = self
                        .registry
                        .instances
                        .iter_mut()
                        .find(|candidate| candidate.path == entry.path)
                    {
                        found.enabled = false;
                    }
                    actions.push(Action::StoppedOnPurpose(entry.path.clone()));
                    continue;
                }
                // Otherwise it is a flag from before this supervisor existed: cleared above,
                // so carry on and start the instance.
            }

            if let Some(started) = progress.started {
                if now.duration_since(started) < EARLY_EXIT {
                    // Still inside the startup window — a big animation takes seconds to
                    // decode. Do not judge it yet, and do not start a second copy.
                    continue;
                }
                // It had a fair chance and is gone again.
                progress.started = None;
                progress.failures = progress.failures.saturating_add(1);
                actions.push(Action::Exited {
                    path: entry.path.clone(),
                    failures: progress.failures,
                });
                progress.wait_until = backoff(now, progress.failures);
            }

            if let Some(until) = progress.wait_until {
                if now < until {
                    continue;
                }
                progress.wait_until = None;
            }

            match spawn(&entry.path) {
                Ok(()) => {
                    progress.started = Some(now);
                    actions.push(Action::Started(entry.path.clone()));
                }
                Err(error) => {
                    progress.failures = progress.failures.saturating_add(1);
                    progress.wait_until = backoff(now, progress.failures);
                    actions.push(Action::SpawnFailed {
                        path: entry.path.clone(),
                        error: error.to_string(),
                    });
                }
            }
        }

        actions
    }
}

/// Wait before the next attempt after `failures` consecutive failures.
fn backoff(now: Instant, failures: u32) -> Option<Instant> {
    let step = BACKOFF[(failures as usize).min(BACKOFF.len() - 1)];
    (step > Duration::ZERO).then(|| now + step)
}

/// One instance as a UI sees it: the control file, the animation behind it (when the
/// persisted list can explain the slug), and whether a process owns it.
#[derive(Debug, Clone)]
pub struct Live {
    pub animation: Option<String>,
    pub control: PathBuf,
    pub running: bool,
}

impl Live {
    /// What to show in a menu: the animation's file name, else the control file's slug.
    pub fn label(&self) -> String {
        self.animation
            .as_deref()
            .and_then(|path| path.rsplit('/').next())
            .map(|name| name.to_string())
            .unwrap_or_else(|| {
                self.control
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "?".to_string())
            })
    }
}

/// Every control file on disk, registered or not — a manager has to show what is actually
/// on screen, not only what it was told to supervise.
pub fn live_instances() -> Vec<Live> {
    let persisted = crate::state::SecondaryState::load(&crate::state::SecondaryState::user_path());
    let mut out: Vec<Live> = ControlState::instances()
        .into_iter()
        .map(|control| {
            let stem = control
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            let animation = persisted
                .instances
                .iter()
                .find(|instance| crate::control::instance_slug(&instance.path) == stem)
                .map(|instance| instance.path.clone());
            let running = ControlState::is_running(&control);
            Live {
                animation,
                control,
                running,
            }
        })
        .collect();
    // Running first, then by name, so the menu leads with what is alive.
    out.sort_by(|a, b| {
        b.running
            .cmp(&a.running)
            .then_with(|| a.label().cmp(&b.label()))
    });
    out
}

/// Animation paths (registry or persisted) whose file is gone.
///
/// `exists` decides; the caller passes `Path::is_file`, the tests pass a set. Nothing that
/// still exists is ever reported.
pub fn dead_animations(paths: &[String], exists: impl Fn(&str) -> bool) -> Vec<String> {
    paths.iter().filter(|path| !exists(path)).cloned().collect()
}

/// Control files worth deleting: the animation is gone, or the slug belongs to nothing the
/// persisted list can explain and no process owns it.
pub fn dead_control_files(live: &[Live], exists: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    live.iter()
        .filter(|instance| match &instance.animation {
            Some(animation) => !exists(animation),
            None => !instance.running,
        })
        .map(|instance| instance.control.clone())
        .collect()
}

/// Markers whose process is gone.
pub fn dead_markers(markers: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    markers
        .into_iter()
        .filter(|marker| !crate::control::marker_running(marker))
        .collect()
}

/// Backups that `prune` left beside a JSON file, oldest first.
pub fn prune_backups(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .contains(".before-prune-")
        })
        .filter_map(|entry| {
            let modified = entry.metadata().and_then(|meta| meta.modified()).ok()?;
            Some((modified, entry.path()))
        })
        .collect();
    out.sort();
    out.into_iter().map(|(_, path)| path).collect()
}

/// Keep only the newest `keep` backups; returns the ones removed.
pub fn trim_backups(dir: &Path, keep: usize) -> Vec<PathBuf> {
    let backups = prune_backups(dir);
    let drop = backups.len().saturating_sub(keep);
    let mut removed = Vec::new();
    for path in backups.into_iter().take(drop) {
        if std::fs::remove_file(&path).is_ok() {
            removed.push(path);
        }
    }
    removed
}

/// Cache files named `<digits>.<ext>` whose Workshop item is no longer installed.
pub fn orphaned_cache(
    cache_dir: &Path,
    installed: &std::collections::HashSet<String>,
) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(cache_dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .is_some_and(|stem| {
                    !stem.is_empty()
                        && stem.chars().all(|character| character.is_ascii_digit())
                        && !installed.contains(&stem)
                })
        })
        .collect();
    out.sort();
    out
}

/// Markers whose process is gone. Only written by the process that owns them, so this is
/// for tidying up, not for supervision.
pub fn stale_markers() -> Vec<PathBuf> {
    ControlState::announcements()
        .into_iter()
        .filter(|marker| !crate::control::marker_running(marker))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry_with(entries: &[(&str, bool)]) -> Registry {
        Registry {
            instances: entries
                .iter()
                .map(|(path, enabled)| Entry {
                    path: (*path).to_string(),
                    enabled: *enabled,
                })
                .collect(),
        }
    }

    fn supervisor(entries: &[(&str, bool)]) -> (tempfile::TempDir, Supervisor) {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = Supervisor::new(dir.path().join(REGISTRY_FILE), registry_with(entries));
        (dir, supervisor)
    }

    #[test]
    fn starts_what_is_wanted_and_leaves_the_rest() {
        let (_dir, mut supervisor) = supervisor(&[("/a.apng", true), ("/b.apng", false)]);
        let mut started: Vec<String> = Vec::new();
        let actions = supervisor.poll(
            Instant::now(),
            |_control| false,
            |animation| {
                started.push(animation.to_string());
                Ok(())
            },
        );
        assert_eq!(started, vec!["/a.apng".to_string()]);
        assert_eq!(actions, vec![Action::Started("/a.apng".to_string())]);
    }

    #[test]
    fn does_not_start_what_is_already_running() {
        let (_dir, mut supervisor) = supervisor(&[("/a.apng", true)]);
        let starts = std::cell::Cell::new(0u32);
        let actions = supervisor.poll(
            Instant::now(),
            |_control| true,
            |_animation| {
                starts.set(starts.get() + 1);
                Ok(())
            },
        );
        assert_eq!(starts.get(), 0);
        assert!(actions.is_empty());
    }

    #[test]
    fn a_slow_start_is_not_a_failure_and_is_not_started_twice() {
        let now = Instant::now();
        let (_dir, mut supervisor) = supervisor(&[("/a.apng", true)]);
        let starts = std::cell::Cell::new(0u32);
        let mut spawn = |_animation: &str| {
            starts.set(starts.get() + 1);
            Ok(())
        };

        supervisor.poll(now, |_| false, &mut spawn);
        // A big animation decodes for seconds before it announces itself: nothing to
        // report, and above all no second copy.
        for offset in [1, 5, 9] {
            let actions = supervisor.poll(now + Duration::from_secs(offset), |_| false, &mut spawn);
            assert!(actions.is_empty(), "still starting at +{offset}s");
        }
        assert_eq!(starts.get(), 1);

        // It made it.
        let actions = supervisor.poll(now + Duration::from_secs(12), |_| true, &mut spawn);
        assert!(actions.is_empty());
        assert_eq!(starts.get(), 1);
    }

    #[test]
    fn a_crash_loop_backs_off() {
        let now = Instant::now();
        let (_dir, mut supervisor) = supervisor(&[("/a.apng", true)]);
        let starts = std::cell::Cell::new(0u32);
        let mut spawn = |_animation: &str| {
            starts.set(starts.get() + 1);
            Ok(())
        };

        // 1: starts it.
        supervisor.poll(now, |_| false, &mut spawn);
        assert_eq!(starts.get(), 1);

        // 2: past the startup window and still gone -> one failure, then a wait.
        let actions = supervisor.poll(now + Duration::from_secs(11), |_| false, &mut spawn);
        assert_eq!(
            actions,
            vec![Action::Exited {
                path: "/a.apng".to_string(),
                failures: 1
            }]
        );
        assert_eq!(starts.get(), 1, "inside the 5s step");

        // 3: past the 5s step -> one more attempt; it dies again -> longer wait.
        let actions = supervisor.poll(now + Duration::from_secs(17), |_| false, &mut spawn);
        assert_eq!(
            actions,
            vec![Action::Started("/a.apng".to_string())],
            "the backoff elapsed"
        );
        assert_eq!(starts.get(), 2);
        let actions = supervisor.poll(now + Duration::from_secs(17 + 11), |_| false, &mut spawn);
        assert_eq!(
            actions,
            vec![Action::Exited {
                path: "/a.apng".to_string(),
                failures: 2
            }]
        );

        // 4: the 30s step, so nothing happens for a while.
        let actions = supervisor.poll(now + Duration::from_secs(40), |_| false, &mut spawn);
        assert!(actions.is_empty());
        assert_eq!(starts.get(), 2, "waiting out the longer step");
        let actions = supervisor.poll(now + Duration::from_secs(60), |_| false, &mut spawn);
        assert_eq!(actions, vec![Action::Started("/a.apng".to_string())]);
        assert_eq!(starts.get(), 3);
    }

    #[test]
    fn a_long_run_resets_the_backoff() {
        let now = Instant::now();
        let (_dir, mut supervisor) = supervisor(&[("/a.apng", true)]);
        let starts = std::cell::Cell::new(0u32);
        let mut spawn = |_animation: &str| {
            starts.set(starts.get() + 1);
            Ok(())
        };

        supervisor.poll(now, |_| false, &mut spawn);
        // It stayed up past EARLY_EXIT, then went away.
        let actions = supervisor.poll(now + Duration::from_secs(30), |_| true, &mut spawn);
        assert!(actions.is_empty());
        let actions = supervisor.poll(now + Duration::from_secs(31), |_| false, &mut spawn);
        assert_eq!(
            actions,
            vec![Action::Started("/a.apng".to_string())],
            "healthy runs do not accumulate failures"
        );
        assert_eq!(starts.get(), 2);
    }

    #[test]
    fn a_spawn_failure_is_reported_and_backed_off() {
        let now = Instant::now();
        let (_dir, mut supervisor) = supervisor(&[("/a.apng", true)]);
        let actions = supervisor.poll(
            now,
            |_| false,
            |_animation| Err(std::io::Error::other("no binary")),
        );
        assert_eq!(
            actions,
            vec![Action::SpawnFailed {
                path: "/a.apng".to_string(),
                error: "no binary".to_string()
            }]
        );
    }

    #[test]
    fn an_intentional_quit_stops_supervision() {
        // Needs a real control file (there is no directory override), so it uses an animation
        // path unique to this test and removes what it wrote.
        let dir = tempfile::tempdir().unwrap();
        let animation = dir.path().join("intentional-quit-test.apng");
        std::fs::write(&animation, b"x").unwrap();
        let path = animation.to_string_lossy().into_owned();
        let control = ControlState::path_for(Some(&paths::canonical_path(&path)));
        let _ = std::fs::remove_file(&control);
        let _ = std::fs::remove_file(ControlState::announce_path(&control));

        let registry_path = dir.path().join(REGISTRY_FILE);
        let mut registry = Registry::default();
        registry.enable(&path);
        registry.save(&registry_path).unwrap();

        let mut supervisor = Supervisor::new(registry_path.clone(), Registry::load(&registry_path));
        let starts = std::cell::Cell::new(0u32);
        let mut spawn = |_: &str| {
            starts.set(starts.get() + 1);
            Ok(())
        };

        // First pass: it is alive, which is what makes a later quit meaningful.
        supervisor.poll(Instant::now(), |_| true, &mut spawn);
        assert_eq!(starts.get(), 0);

        // The user quits it on purpose: gone, and the control file carries the flag.
        ControlState::fresh().request_quit().save(&control).unwrap();
        let actions = supervisor.poll(Instant::now(), |_| false, &mut spawn);
        assert_eq!(actions, vec![Action::StoppedOnPurpose(path.clone())]);
        assert_eq!(starts.get(), 0, "an intentional quit must not be restarted");
        assert!(!Registry::load(&registry_path).get(&path).unwrap().enabled);
        assert!(!ControlState::load(&control).quit, "the flag is one-shot");

        // And it does not come back on the next pass either.
        assert!(
            supervisor
                .poll(Instant::now(), |_| false, &mut spawn)
                .is_empty()
        );
        assert_eq!(starts.get(), 0);

        let _ = std::fs::remove_file(&control);
        let _ = std::fs::remove_file(ControlState::announce_path(&control));
    }

    #[test]
    fn live_instances_cover_every_control_file() {
        let live = live_instances();
        assert_eq!(live.len(), ControlState::instances().len());
        for instance in &live {
            assert!(instance.control.is_file());
            assert!(!instance.label().is_empty());
        }
        eprintln!(
            "[shepherd] {} control file(s), {} running",
            live.len(),
            live.iter().filter(|instance| instance.running).count()
        );
    }

    #[test]
    fn dead_references_are_the_ones_that_point_nowhere() {
        let exists = |path: &str| path == "/ok.apng";
        let paths = vec!["/ok.apng".to_string(), "/gone.apng".to_string()];
        assert_eq!(dead_animations(&paths, exists), vec!["/gone.apng"]);

        let live = vec![
            Live {
                animation: Some("/ok.apng".into()),
                control: PathBuf::from("/c/ok.json"),
                running: false,
            },
            Live {
                animation: Some("/gone.apng".into()),
                control: PathBuf::from("/c/gone.json"),
                running: false,
            },
            Live {
                animation: None,
                control: PathBuf::from("/c/unknown-dead.json"),
                running: false,
            },
            Live {
                animation: None,
                control: PathBuf::from("/c/unknown-live.json"),
                running: true,
            },
        ];
        let dead = dead_control_files(&live, exists);
        assert_eq!(dead.len(), 2, "{dead:?}");
        assert!(dead.contains(&PathBuf::from("/c/gone.json")));
        assert!(dead.contains(&PathBuf::from("/c/unknown-dead.json")));
        assert!(!dead.contains(&PathBuf::from("/c/ok.json")));
        assert!(!dead.contains(&PathBuf::from("/c/unknown-live.json")));
    }

    #[test]
    fn backups_are_trimmed_to_the_newest_pair() {
        let dir = tempfile::tempdir().unwrap();
        let mut made = Vec::new();
        for index in 0..4 {
            let path = dir.path().join(format!("state.json.before-prune-{index}"));
            std::fs::write(&path, b"x").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
            made.push(path);
        }
        assert_eq!(prune_backups(dir.path()).len(), 4);
        assert_eq!(trim_backups(dir.path(), 2).len(), 2);
        assert_eq!(
            prune_backups(dir.path()),
            made[2..].to_vec(),
            "the newest pair survives"
        );
    }

    #[test]
    fn dead_markers_and_orphaned_cache() {
        let dir = tempfile::tempdir().unwrap();
        let dead = dir.path().join("dead.pid");
        std::fs::write(&dead, format!("{} anima-overlay\n", u32::MAX)).unwrap();
        let alive = dir.path().join("alive.pid");
        crate::control::write_marker(&alive, &ControlState::announce_name()).unwrap();
        let mut markers = vec![dead.clone(), alive];
        markers.sort();
        assert_eq!(dead_markers(markers), vec![dead]);

        let cache = tempfile::tempdir().unwrap();
        for name in ["123.img", "456.json", "789", "notes.txt", "abc.img"] {
            std::fs::write(cache.path().join(name), b"x").unwrap();
        }
        let installed = std::collections::HashSet::from(["123".to_string()]);
        let orphans = orphaned_cache(cache.path(), &installed);
        let names: Vec<String> = orphans
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["456.json", "789"], "{names:?}");
    }

    #[test]
    fn registry_enable_disable_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(REGISTRY_FILE);
        let mut registry = Registry::default();

        assert!(registry.enable("/gifs/a.apng"));
        assert!(!registry.enable("/gifs/a.apng"), "already enabled");
        assert!(registry.enable("/gifs/b.apng"));
        assert!(registry.disable("/gifs/a.apng"));
        assert!(!registry.disable("/gifs/a.apng"), "already disabled");
        assert_eq!(registry.wanted().count(), 1);

        registry.save(&path).unwrap();
        let reloaded = Registry::load(&path);
        assert_eq!(reloaded, registry);
        assert!(!reloaded.get("/gifs/a.apng").unwrap().enabled);
        assert!(reloaded.get("/gifs/b.apng").unwrap().enabled);
        assert!(reloaded.get("/gifs/missing.apng").is_none());
    }
}
