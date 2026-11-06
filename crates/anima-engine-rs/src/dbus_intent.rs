//! The shepherd's session-bus face.
//!
//! A game, a launcher or a script can ask for what the tray and the CLI can ask for, and it gets it
//! the same way: the registry file is written, and the supervision loop acts on it. Nothing here
//! owns state of its own — that is the point of the file-as-channel design, and a second source of
//! truth is how the two ends start disagreeing.

use std::path::PathBuf;

use crate::Result;
use crate::shepherd::Registry;

/// The name other programs connect to on the session bus.
pub const BUS_NAME: &str = "dev.AnimaEngine.Shepherd";
/// The object the methods live on.
pub const OBJECT_PATH: &str = "/dev/AnimaEngine/Shepherd";
/// The interface those methods are in.
pub const INTERFACE: &str = "dev.AnimaEngine.Shepherd";

/// The served object: a registry path and nothing else.
#[derive(Debug)]
pub struct Intent {
    registry: PathBuf,
}

impl Intent {
    pub fn new(registry: impl Into<PathBuf>) -> Self {
        Self {
            registry: registry.into(),
        }
    }

    /// Ask for `animation` to be supervised. `true` when the registry changed, so a caller can tell
    /// "asked" from "already wanted" — the same answer `anima-shepherd enable` gives.
    pub fn enable(&self, animation: &str) -> Result<bool> {
        self.change(|registry| registry.enable(animation))
    }

    /// Stop supervising `animation`. The entry stays, so its state is remembered.
    pub fn disable(&self, animation: &str) -> Result<bool> {
        self.change(|registry| registry.disable(animation))
    }

    /// The animations that are wanted right now.
    pub fn wanted(&self) -> Vec<String> {
        Registry::load(&self.registry)
            .wanted()
            .map(|entry| entry.path.clone())
            .collect()
    }

    /// One line, mirroring what `anima-shepherd status` says.
    pub fn summary(&self) -> String {
        let registry = Registry::load(&self.registry);
        format!(
            "{} wanted, {} known",
            registry.wanted().count(),
            registry.instances.len()
        )
    }

    fn change(&self, change: impl FnOnce(&mut Registry) -> bool) -> Result<bool> {
        let mut registry = Registry::load(&self.registry);
        if !change(&mut registry) {
            return Ok(false);
        }
        registry.save(&self.registry)?;
        Ok(true)
    }
}

/// The methods other programs actually call. Each is a line of registry work, so the bus cannot do
/// anything the CLI cannot.
#[zbus::interface(name = "dev.AnimaEngine.Shepherd")]
impl Intent {
    /// Ask for `animation` to be supervised. True when the registry changed.
    fn start(&self, animation: String) -> zbus::fdo::Result<bool> {
        self.enable(&animation).map_err(intent_error)
    }

    /// Stop supervising `animation`. True when the registry changed.
    fn stop(&self, animation: String) -> zbus::fdo::Result<bool> {
        self.disable(&animation).map_err(intent_error)
    }

    /// The animations that are wanted right now.
    fn list(&self) -> Vec<String> {
        self.wanted()
    }

    /// One line, for scripts that only want to know whether the shepherd is alive and busy.
    fn status(&self) -> String {
        self.summary()
    }
}

fn intent_error(error: crate::Error) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(error.to_string())
}

/// Serve the interface on the session bus until the process ends.
///
/// Blocking on purpose: it belongs on its own thread in the shepherd, so a session without a bus
/// costs one log line instead of the supervision loop.
pub fn serve(registry: impl Into<PathBuf>) -> zbus::Result<()> {
    // Held for the life of the process: dropping the connection would release the name.
    let _connection = zbus::blocking::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, Intent::new(registry))?
        .build()?;
    crate::log::event(
        crate::log::Level::Info,
        "shepherd.dbus",
        &[("name", &BUS_NAME), ("path", &OBJECT_PATH)],
    );
    loop {
        std::thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bus_writes_the_same_registry_the_cli_would() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shepherd.json");
        let intent = Intent::new(&path);

        assert!(intent.wanted().is_empty());
        assert!(
            intent.enable("/tmp/a.apng").unwrap(),
            "first ask changes it"
        );
        assert!(
            !intent.enable("/tmp/a.apng").unwrap(),
            "asking again changes nothing"
        );
        assert_eq!(intent.wanted().len(), 1);
        assert!(
            intent.summary().contains("1 wanted"),
            "{}",
            intent.summary()
        );

        // A second object over the same file sees the same state: nothing is cached in the object.
        let other = Intent::new(&path);
        assert_eq!(other.wanted().len(), 1);
        assert!(other.disable("/tmp/a.apng").unwrap());
        assert!(!other.disable("/tmp/a.apng").unwrap());
        assert!(intent.wanted().is_empty());
    }
}
