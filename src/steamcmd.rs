//! SteamCMD integration for workshop downloads (no Steamworks SDK needed).
//!
//! `steamcmd +login anonymous +workshop_download_item <appid> <id>` downloads a
//! public workshop item into `steamapps/workshop/content/<appid>/<id>` under the
//! SteamCMD install directory. Private items need a real login.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::workshop::{APP_ID, WORKSHOP_CONTENT_SUBDIR};
use crate::{Error, Result};

/// Environment variable pointing at the `steamcmd` executable.
pub const STEAMCMD_ENV: &str = "STEAMCMD";
/// Environment variable for the directory SteamCMD downloads into.
pub const STEAMCMD_DIR_ENV: &str = "STEAMCMD_DIR";

/// A `steamcmd` installation used to download workshop items.
pub struct SteamCmd {
    program: PathBuf,
    install_dir: PathBuf,
}

impl SteamCmd {
    /// Locate `steamcmd` through `STEAMCMD`, then `PATH`.
    pub fn detect() -> Option<Self> {
        let program = std::env::var_os(STEAMCMD_ENV)
            .map(PathBuf::from)
            .filter(|path| is_executable(path))
            .or_else(|| find_in_path("steamcmd"))?;
        let install_dir = std::env::var_os(STEAMCMD_DIR_ENV)
            .map(PathBuf::from)
            .or_else(|| program.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        Some(Self {
            program,
            install_dir,
        })
    }

    /// Explicit paths (used by tests).
    pub fn with_paths(program: impl Into<PathBuf>, install_dir: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            install_dir: install_dir.into(),
        }
    }

    /// SteamCMD arguments for downloading one item.
    pub fn arguments(&self, id: u64) -> Vec<String> {
        vec![
            "+force_install_dir".to_string(),
            self.install_dir.display().to_string(),
            "+login".to_string(),
            "anonymous".to_string(),
            "+workshop_download_item".to_string(),
            APP_ID.to_string(),
            id.to_string(),
            "+quit".to_string(),
        ]
    }

    /// Where SteamCMD places the downloaded item.
    pub fn item_dir(&self, id: u64) -> PathBuf {
        self.install_dir
            .join(WORKSHOP_CONTENT_SUBDIR)
            .join(APP_ID.to_string())
            .join(id.to_string())
    }

    /// Run SteamCMD and return the downloaded directory.
    pub fn download(&self, id: u64) -> Result<PathBuf> {
        let status = Command::new(&self.program)
            .args(self.arguments(id))
            .status()
            .map_err(|err| Error::Steam(format!("running steamcmd: {err}")))?;
        if !status.success() {
            return Err(Error::Steam(format!("steamcmd exited with {status}")));
        }
        let dir = self.item_dir(id);
        if dir.is_dir() {
            return Ok(dir);
        }
        Err(Error::Steam(format!(
            "steamcmd finished but {} is missing (private item? use `+login <user>`)",
            dir.display()
        )))
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Find an executable in `PATH`.
pub fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_and_item_dir() {
        let cmd = SteamCmd::with_paths("/usr/bin/steamcmd", "/tmp/sc");
        assert_eq!(
            cmd.arguments(123),
            [
                "+force_install_dir",
                "/tmp/sc",
                "+login",
                "anonymous",
                "+workshop_download_item",
                "3474900",
                "123",
                "+quit",
            ]
            .iter()
            .map(|arg| arg.to_string())
            .collect::<Vec<_>>()
        );
        assert_eq!(
            cmd.item_dir(123),
            PathBuf::from("/tmp/sc/steamapps/workshop/content/3474900/123")
        );
    }

    #[cfg(unix)]
    #[test]
    fn download_runs_the_program_and_finds_the_item() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("sc");
        let program = dir.path().join("steamcmd");
        let item = install
            .join(WORKSHOP_CONTENT_SUBDIR)
            .join(APP_ID.to_string())
            .join("42");
        let script = format!(
            "#!/bin/sh\nmkdir -p '{}'\nprintf x > '{}/fake.apng'\nexit 0\n",
            item.display(),
            item.display()
        );
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();

        let cmd = SteamCmd::with_paths(&program, &install);
        let found = cmd.download(42).unwrap();
        assert_eq!(found, item);
        assert!(found.join("fake.apng").is_file());
    }
}
