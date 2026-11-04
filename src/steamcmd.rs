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
/// Subdirectory of the SteamCMD install used for generated item VDFs.
const UPLOAD_SUBDIR: &str = "anima_upload";

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
        // Default to our own XDG data dir: steamcmd usually lives in a read-only
        // system location (e.g. /usr/bin) and would fail to write there.
        let install_dir = std::env::var_os(STEAMCMD_DIR_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::paths::user_data_dir().join("steamcmd"));
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
        self.content_root().join(id.to_string())
    }

    /// `<install>/steamapps/workshop/content/<appid>`.
    pub fn content_root(&self) -> PathBuf {
        self.install_dir
            .join(WORKSHOP_CONTENT_SUBDIR)
            .join(APP_ID.to_string())
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

    /// SteamCMD arguments for uploading (or updating) a `workshop.vdf` item.
    pub fn upload_arguments(&self, vdf: &Path, user: &str) -> Vec<String> {
        vec![
            "+login".to_string(),
            user.to_string(),
            "+workshop_build_item".to_string(),
            vdf.display().to_string(),
            "+quit".to_string(),
        ]
    }

    /// Write the item VDF and run `workshop_build_item`, returning the published
    /// file id when SteamCMD reports one. `stdin`/`stderr` are inherited so the
    /// login prompt and errors stay visible.
    pub fn upload(&self, upload: &WorkshopUpload, user: &str) -> Result<Option<u64>> {
        let dir = self.install_dir.join(UPLOAD_SUBDIR);
        std::fs::create_dir_all(&dir)
            .map_err(|err| Error::Steam(format!("creating {}: {err}", dir.display())))?;
        let vdf_path = dir.join("workshop.vdf");
        std::fs::write(&vdf_path, upload.to_vdf())
            .map_err(|err| Error::Steam(format!("writing {}: {err}", vdf_path.display())))?;

        let mut child = Command::new(&self.program)
            .args(self.upload_arguments(&vdf_path, user))
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .map_err(|err| Error::Steam(format!("running steamcmd: {err}")))?;

        let mut output = String::new();
        if let Some(stdout) = child.stdout.take() {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stdout)
                .lines()
                .map_while(std::result::Result::ok)
            {
                println!("{line}");
                output.push_str(&line);
                output.push('\n');
            }
        }
        let status = child
            .wait()
            .map_err(|err| Error::Steam(format!("waiting for steamcmd: {err}")))?;
        if !status.success() {
            return Err(Error::Steam(format!("steamcmd exited with {status}")));
        }
        Ok(parse_published_id(&output))
    }
}

/// Visibility of a published workshop item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    #[default]
    Public,
    Friends,
    Private,
    Unlisted,
}

impl Visibility {
    /// The numeric value `workshop_build_item` expects.
    pub fn as_vdf(self) -> &'static str {
        match self {
            Self::Public => "0",
            Self::Friends => "1",
            Self::Private => "2",
            Self::Unlisted => "3",
        }
    }

    /// Parse a name (`public`) or the numeric value.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "public" | "0" => Some(Self::Public),
            "friends" | "1" => Some(Self::Friends),
            "private" | "2" => Some(Self::Private),
            "unlisted" | "3" => Some(Self::Unlisted),
            _ => None,
        }
    }
}

/// Metadata for one `workshop_build_item` run.
#[derive(Debug, Clone, Default)]
pub struct WorkshopUpload {
    pub title: String,
    pub description: String,
    pub content_dir: PathBuf,
    pub preview: Option<PathBuf>,
    /// `None` creates a new item; `Some(id)` updates an existing one.
    pub published_file_id: Option<u64>,
    pub visibility: Visibility,
    pub changenote: String,
}

impl WorkshopUpload {
    /// The `workshop.vdf` document for this item.
    pub fn to_vdf(&self) -> String {
        let mut out = String::from("\"workshopitem\"\n{\n");
        vdf_field(&mut out, "appid", &APP_ID.to_string());
        vdf_field(
            &mut out,
            "publishedfileid",
            &self.published_file_id.unwrap_or(0).to_string(),
        );
        vdf_field(
            &mut out,
            "contentfolder",
            &self.content_dir.display().to_string(),
        );
        if let Some(preview) = &self.preview {
            vdf_field(&mut out, "previewfile", &preview.display().to_string());
        }
        vdf_field(&mut out, "visibility", self.visibility.as_vdf());
        vdf_field(&mut out, "title", &self.title);
        vdf_field(&mut out, "description", &self.description);
        if !self.changenote.is_empty() {
            vdf_field(&mut out, "changenote", &self.changenote);
        }
        out.push_str("}\n");
        out
    }
}

fn vdf_field(out: &mut String, key: &str, value: &str) {
    out.push_str("\t\"");
    out.push_str(key);
    out.push_str("\"\t\t\"");
    out.push_str(&vdf_escape(value));
    out.push_str("\"\n");
}

fn vdf_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace(['\n', '\r'], " ")
}

/// Pull the published file id out of SteamCMD's output.
pub fn parse_published_id(output: &str) -> Option<u64> {
    for line in output.lines().rev() {
        let lower = line.to_ascii_lowercase();
        if !(lower.contains("published") || lower.contains("workshop item")) {
            continue;
        }
        if let Some(id) = longest_number(line) {
            return Some(id);
        }
    }
    None
}

fn longest_number(line: &str) -> Option<u64> {
    let mut best = None;
    let mut digits = String::new();
    for ch in line.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_digit() {
            digits.push(ch);
            continue;
        }
        if digits.len() >= 6
            && let Ok(value) = digits.parse::<u64>()
        {
            best = Some(value);
        }
        digits.clear();
    }
    best
}

/// Subscriptions come from the Steam client's `appworkshop_*.acf`; missing items
/// are downloaded through SteamCMD.
impl crate::workshop::WorkshopSource for SteamCmd {
    fn subscribed_ids(&self) -> Result<Vec<u64>> {
        use std::collections::BTreeSet;

        let mut ids = BTreeSet::new();
        for library in crate::workshop::discover_libraries() {
            let acf = library
                .join("steamapps")
                .join("workshop")
                .join(crate::workshop::acf_file_name());
            if let Ok(text) = std::fs::read_to_string(&acf)
                && let Ok(items) = crate::workshop::installed_items(&text)
            {
                ids.extend(items.keys().copied());
            }
        }
        if ids.is_empty() {
            ids.extend(crate::workshop::installed_item_dirs(&self.content_root()));
        }
        Ok(ids.into_iter().collect())
    }

    fn ensure_installed(&self, id: u64) -> Result<Option<PathBuf>> {
        // Prefer the Steam client's copy, then our download root, then download.
        for library in crate::workshop::discover_libraries() {
            let dir = crate::workshop::content_root(&library).join(id.to_string());
            if dir.is_dir() {
                return Ok(Some(dir));
            }
        }
        let dir = self.item_dir(id);
        if dir.is_dir() {
            return Ok(Some(dir));
        }
        self.download(id).map(Some)
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

    #[test]
    fn ensure_installed_prefers_the_download_root() {
        use crate::workshop::WorkshopSource;

        let dir = tempfile::tempdir().unwrap();
        let cmd = SteamCmd::with_paths("/bin/true", dir.path());
        let item = cmd.item_dir(7);
        std::fs::create_dir_all(&item).unwrap();
        assert_eq!(cmd.ensure_installed(7).unwrap(), Some(item));
    }

    #[test]
    fn vdf_contains_the_expected_fields() {
        let upload = WorkshopUpload {
            title: "Halloween Miku".to_string(),
            description: "sprites".to_string(),
            content_dir: PathBuf::from("/tmp/anima/halloween"),
            preview: Some(PathBuf::from("/tmp/anima/preview.png")),
            visibility: Visibility::Unlisted,
            changenote: "first".to_string(),
            ..Default::default()
        };
        let vdf = upload.to_vdf();
        assert!(vdf.starts_with("\"workshopitem\"\n{"));
        assert!(vdf.contains(&format!("\"appid\"\t\t\"{APP_ID}\"")));
        assert!(vdf.contains("\"publishedfileid\"\t\t\"0\""));
        assert!(vdf.contains("\"contentfolder\"\t\t\"/tmp/anima/halloween\""));
        assert!(vdf.contains("\"previewfile\"\t\t\"/tmp/anima/preview.png\""));
        assert!(vdf.contains("\"visibility\"\t\t\"3\""));
        assert!(vdf.contains("\"title\"\t\t\"Halloween Miku\""));
        assert!(vdf.contains("\"changenote\"\t\t\"first\""));
        assert!(vdf.ends_with("}\n"));
    }

    #[test]
    fn vdf_escapes_quotes_and_omits_empty_optionals() {
        let upload = WorkshopUpload {
            title: "a \"b\"".to_string(),
            content_dir: PathBuf::from("/c"),
            ..Default::default()
        };
        let vdf = upload.to_vdf();
        assert!(vdf.contains("\"title\"\t\t\"a \\\"b\\\"\""));
        assert!(!vdf.contains("previewfile"));
        assert!(!vdf.contains("changenote"));
        assert!(vdf.contains("\"visibility\"\t\t\"0\""));
    }

    #[test]
    fn update_keeps_the_published_id() {
        let upload = WorkshopUpload {
            published_file_id: Some(3_828_587_753),
            content_dir: PathBuf::from("/c"),
            ..Default::default()
        };
        assert!(
            upload
                .to_vdf()
                .contains("\"publishedfileid\"\t\t\"3828587753\"")
        );
    }

    #[test]
    fn parses_the_published_id_from_steamcmd_output() {
        let output = "Logging in user 'someone' to Steam Public...\n\
                      Success. Workshop item 3828587753 published.\n";
        assert_eq!(parse_published_id(output), Some(3_828_587_753));
        assert_eq!(parse_published_id("nothing here"), None);
        assert_eq!(
            parse_published_id("Update workshop item 12345678 finished"),
            Some(12_345_678)
        );
    }

    #[test]
    fn visibility_parses_names_and_numbers() {
        assert_eq!(Visibility::parse("public"), Some(Visibility::Public));
        assert_eq!(Visibility::parse("Unlisted"), Some(Visibility::Unlisted));
        assert_eq!(Visibility::parse("2"), Some(Visibility::Private));
        assert_eq!(Visibility::parse("hidden"), None);
        assert_eq!(Visibility::default().as_vdf(), "0");
    }
}
