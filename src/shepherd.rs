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
}

/// Per-animation bookkeeping between passes.
#[derive(Debug, Clone, Copy, Default)]
struct Progress {
    started: Option<Instant>,
    failures: u32,
    wait_until: Option<Instant>,
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
                continue;
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
