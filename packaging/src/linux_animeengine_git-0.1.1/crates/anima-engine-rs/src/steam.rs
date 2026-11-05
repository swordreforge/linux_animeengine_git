//! Consumer-side Steam Workshop bridge.
//!
//! The subscribe → download → poll flow is transport-agnostic (and unit-tested
//! with a mock) so the logic is verified without Steam. The real transport,
//! `flat::FlatSteamApi`, loads Valve's `libsteam_api` at **runtime** with
//! `libloading` (feature `steamworks`) — no build-time SDK dependency — and
//! calls the flat C API exactly as the shipped app's `SteamworksFlat` does.
//!
//! When the library (or a running Steam client) is unavailable, callers can
//! fall back to [`crate::workshop::LocalWorkshopSource`], SteamCMD, or the Web
//! API adapter.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::Error;
use crate::Result;
use crate::workshop::WorkshopSource;

/// `k_EItemStateSubscribed`
pub const ITEM_SUBSCRIBED: u32 = 1;
/// `k_EItemStateInstalled`
pub const ITEM_INSTALLED: u32 = 4;
/// `k_EItemStateNeedsUpdate`
pub const ITEM_NEEDS_UPDATE: u32 = 8;
/// `k_EItemStateDownloading`
pub const ITEM_DOWNLOADING: u32 = 16;
/// `k_EItemStateDownloadPending`
pub const ITEM_DOWNLOAD_PENDING: u32 = 32;

/// Default download wait (matches the app's `subscribe_and_download` timeout).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

const SUBSCRIBE_GRACE: Duration = Duration::from_secs(2);
const SUBSCRIBE_POLL: Duration = Duration::from_millis(100);
const POLL_INTERVAL: Duration = Duration::from_millis(200);
const MIN_DOWNLOAD_WAIT: Duration = Duration::from_secs(5);

pub fn is_installed(state: u32) -> bool {
    state & ITEM_INSTALLED != 0
}

pub fn is_updating(state: u32) -> bool {
    state & (ITEM_NEEDS_UPDATE | ITEM_DOWNLOADING | ITEM_DOWNLOAD_PENDING) != 0
}

/// Installed and not pending an update/download.
pub fn is_ready(state: u32) -> bool {
    is_installed(state) && !is_updating(state)
}

/// Installation info returned by `GetItemInstallInfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallInfo {
    pub size: u64,
    pub folder: PathBuf,
    pub timestamp: u32,
}

/// The low-level Steam UGC calls, separated so the flow can be tested.
pub trait UgcTransport {
    fn subscribed_ids(&self) -> Result<Vec<u64>>;
    fn item_state(&self, id: u64) -> Result<u32>;
    fn item_install_info(&self, id: u64) -> Result<Option<InstallInfo>>;
    fn subscribe(&self, id: u64) -> Result<()>;
    fn unsubscribe(&self, id: u64) -> Result<()>;
    /// `ISteamUGC::DownloadItem`; `Ok(false)` when Steam rejects the request.
    fn download(&self, id: u64, high_priority: bool) -> Result<bool>;
    /// `SteamAPI_RunCallbacks`.
    fn pump(&self);
}

/// Subscribe-and-download flow over any [`UgcTransport`].
#[derive(Debug)]
pub struct SteamWorkshop<T> {
    transport: T,
    timeout: Duration,
}

impl<T: UgcTransport> SteamWorkshop<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn with_timeout(transport: T, timeout: Duration) -> Self {
        Self { transport, timeout }
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Ensure `id` is installed and return its folder.
    ///
    /// Mirrors the shipped app's `subscribe_and_download`: fast path when it is
    /// already installed, otherwise subscribe (2 s grace), `DownloadItem`, then
    /// poll `GetItemState` / `GetItemInstallInfo` until ready. `force` bypasses
    /// the fast path (used after AnimaEngine removed a local item).
    pub fn download_and_install(&self, id: u64, force: bool) -> Result<PathBuf> {
        let state = self.transport.item_state(id)?;

        if !force
            && state & ITEM_SUBSCRIBED != 0
            && is_ready(state)
            && let Some(info) = self.transport.item_install_info(id)?
            && info.folder.is_dir()
        {
            return Ok(info.folder);
        }

        if state & ITEM_SUBSCRIBED == 0 {
            let _ = self.transport.subscribe(id);
            let deadline = Instant::now() + SUBSCRIBE_GRACE;
            while Instant::now() < deadline {
                self.transport.pump();
                if self.transport.item_state(id)? & ITEM_SUBSCRIBED != 0 {
                    break;
                }
                std::thread::sleep(SUBSCRIBE_POLL);
            }
        }

        if !self.transport.download(id, true)? {
            return Err(Error::Steam("Steam rejected DownloadItem".to_string()));
        }

        let deadline = Instant::now() + self.timeout.max(MIN_DOWNLOAD_WAIT);
        while Instant::now() < deadline {
            self.transport.pump();
            if is_ready(self.transport.item_state(id)?)
                && let Some(info) = self.transport.item_install_info(id)?
                && info.folder.is_dir()
            {
                return Ok(info.folder);
            }
            std::thread::sleep(POLL_INTERVAL);
        }

        if let Some(info) = self.transport.item_install_info(id)?
            && info.folder.is_dir()
        {
            return Ok(info.folder);
        }
        Err(Error::Steam(
            "Steam Workshop download timed out".to_string(),
        ))
    }
}

impl<T: UgcTransport> WorkshopSource for SteamWorkshop<T> {
    fn subscribed_ids(&self) -> Result<Vec<u64>> {
        self.transport.subscribed_ids()
    }

    fn ensure_installed(&self, id: u64) -> Result<Option<PathBuf>> {
        self.download_and_install(id, false).map(Some)
    }
}

#[cfg(feature = "steamworks")]
pub mod flat {
    //! Runtime binding to Valve's `libsteam_api` flat C API via `libloading`.

    use std::ffi::CStr;
    use std::os::raw::{c_char, c_void};
    use std::path::{Path, PathBuf};

    use libloading::Library;

    use super::{InstallInfo, UgcTransport};
    use crate::workshop::APP_ID;
    use crate::{Error, Result};

    type FnGetNum = unsafe extern "C" fn(*mut c_void) -> u32;
    type FnGetSubscribed = unsafe extern "C" fn(*mut c_void, *mut u64, u32) -> u32;
    type FnGetState = unsafe extern "C" fn(*mut c_void, u64) -> u32;
    type FnInstallInfo =
        unsafe extern "C" fn(*mut c_void, u64, *mut u64, *mut c_char, u32, *mut u32) -> bool;
    type FnItemOp = unsafe extern "C" fn(*mut c_void, u64) -> u64;
    type FnDownload = unsafe extern "C" fn(*mut c_void, u64, bool) -> bool;
    type FnVoid = unsafe extern "C" fn();

    /// Versioned `SteamAPI_SteamUGC_v*` accessor names, newest first.
    const UGC_ACCESSORS: &[&[u8]] = &[
        b"SteamAPI_SteamUGC_v022\0",
        b"SteamAPI_SteamUGC_v021\0",
        b"SteamAPI_SteamUGC_v020\0",
        b"SteamAPI_SteamUGC_v019\0",
        b"SteamAPI_SteamUGC_v018\0",
    ];

    /// A loaded `libsteam_api` with the UGC entry points bound.
    #[derive(Debug)]
    pub struct FlatSteamApi {
        _lib: Library,
        ugc: *mut c_void,
        get_num: FnGetNum,
        get_subscribed: FnGetSubscribed,
        get_state: FnGetState,
        install_info: FnInstallInfo,
        subscribe: FnItemOp,
        unsubscribe: FnItemOp,
        download: FnDownload,
        run_callbacks: Option<FnVoid>,
        shutdown: Option<FnVoid>,
    }

    // The pointer is only dereferenced by Steam's own flat API.
    unsafe impl Send for FlatSteamApi {}
    unsafe impl Sync for FlatSteamApi {}

    impl FlatSteamApi {
        /// Load `libsteam_api`, initialize the API and bind the UGC calls.
        pub fn open(path: Option<&Path>) -> Result<Self> {
            let lib = match path {
                Some(path) => unsafe { Library::new(path) }.map_err(steam_err)?,
                None => load_default()?,
            };

            ensure_steam_appid();
            if !init_api(&lib) {
                return Err(Error::Steam(
                    "SteamAPI_Init failed (is Steam running and logged in?)".to_string(),
                ));
            }

            let ugc = UGC_ACCESSORS
                .iter()
                .find_map(|name| unsafe {
                    lib.get::<unsafe extern "C" fn() -> *mut c_void>(name)
                        .ok()
                        .map(|accessor| (*accessor)())
                })
                .unwrap_or(std::ptr::null_mut());
            if ugc.is_null() {
                return Err(Error::Steam("ISteamUGC interface unavailable".to_string()));
            }

            unsafe {
                let get_num: FnGetNum = sym(&lib, b"SteamAPI_ISteamUGC_GetNumSubscribedItems\0")?;
                let get_subscribed: FnGetSubscribed =
                    sym(&lib, b"SteamAPI_ISteamUGC_GetSubscribedItems\0")?;
                let get_state: FnGetState = sym(&lib, b"SteamAPI_ISteamUGC_GetItemState\0")?;
                let install_info: FnInstallInfo =
                    sym(&lib, b"SteamAPI_ISteamUGC_GetItemInstallInfo\0")?;
                let subscribe: FnItemOp = sym(&lib, b"SteamAPI_ISteamUGC_SubscribeItem\0")?;
                let unsubscribe: FnItemOp = sym(&lib, b"SteamAPI_ISteamUGC_UnsubscribeItem\0")?;
                let download: FnDownload = sym(&lib, b"SteamAPI_ISteamUGC_DownloadItem\0")?;
                let run_callbacks = lib
                    .get::<FnVoid>(b"SteamAPI_RunCallbacks\0")
                    .ok()
                    .map(|f| *f);
                let shutdown = lib.get::<FnVoid>(b"SteamAPI_Shutdown\0").ok().map(|f| *f);

                Ok(Self {
                    _lib: lib,
                    ugc,
                    get_num,
                    get_subscribed,
                    get_state,
                    install_info,
                    subscribe,
                    unsubscribe,
                    download,
                    run_callbacks,
                    shutdown,
                })
            }
        }
    }

    impl Drop for FlatSteamApi {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown {
                unsafe { shutdown() };
            }
        }
    }

    impl UgcTransport for FlatSteamApi {
        fn subscribed_ids(&self) -> Result<Vec<u64>> {
            let count = unsafe { (self.get_num)(self.ugc) } as usize;
            if count == 0 {
                return Ok(Vec::new());
            }
            let mut ids = vec![0u64; count];
            let actual =
                unsafe { (self.get_subscribed)(self.ugc, ids.as_mut_ptr(), count as u32) } as usize;
            ids.truncate(actual.min(count));
            Ok(ids)
        }

        fn item_state(&self, id: u64) -> Result<u32> {
            Ok(unsafe { (self.get_state)(self.ugc, id) })
        }

        fn item_install_info(&self, id: u64) -> Result<Option<InstallInfo>> {
            let mut size = 0u64;
            let mut timestamp = 0u32;
            let mut buf = vec![0 as c_char; 4096];
            let ok = unsafe {
                (self.install_info)(
                    self.ugc,
                    id,
                    &mut size,
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    &mut timestamp,
                )
            };
            if !ok {
                return Ok(None);
            }
            let folder = unsafe { CStr::from_ptr(buf.as_ptr()) }
                .to_string_lossy()
                .into_owned();
            Ok(Some(InstallInfo {
                size,
                folder: PathBuf::from(folder),
                timestamp,
            }))
        }

        fn subscribe(&self, id: u64) -> Result<()> {
            unsafe { (self.subscribe)(self.ugc, id) };
            Ok(())
        }

        fn unsubscribe(&self, id: u64) -> Result<()> {
            unsafe { (self.unsubscribe)(self.ugc, id) };
            Ok(())
        }

        fn download(&self, id: u64, high_priority: bool) -> Result<bool> {
            Ok(unsafe { (self.download)(self.ugc, id, high_priority) })
        }

        fn pump(&self) {
            if let Some(run_callbacks) = self.run_callbacks {
                unsafe { run_callbacks() };
            }
        }
    }

    unsafe fn sym<T: Copy>(lib: &Library, name: &[u8]) -> Result<T> {
        let symbol = unsafe { lib.get::<T>(name) }.map_err(steam_err)?;
        Ok(*symbol)
    }

    fn load_default() -> Result<Library> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(path) = std::env::var("STEAM_API_LIB")
            && !path.trim().is_empty()
        {
            candidates.push(PathBuf::from(path));
        }
        candidates.push(PathBuf::from("libsteam_api.so"));
        let exe = crate::paths::exe_dir();
        candidates.push(exe.join("libsteam_api.so"));
        candidates.push(exe.join("_internal").join("libsteam_api.so"));

        for candidate in candidates {
            let is_plain_name = candidate == Path::new("libsteam_api.so");
            if !is_plain_name && !candidate.exists() {
                continue;
            }
            if let Ok(lib) = unsafe { Library::new(&candidate) } {
                return Ok(lib);
            }
        }
        Err(Error::Steam(
            "libsteam_api.so not found (set STEAM_API_LIB or place it next to the exe)".to_string(),
        ))
    }

    fn init_api(lib: &Library) -> bool {
        unsafe {
            if let Ok(init_flat) =
                lib.get::<unsafe extern "C" fn(*mut c_char) -> i32>(b"SteamAPI_InitFlat\0")
            {
                let mut error = vec![0 as c_char; 1024];
                if (*init_flat)(error.as_mut_ptr()) == 0 {
                    return true;
                }
            }
            if let Ok(init) = lib.get::<unsafe extern "C" fn() -> bool>(b"SteamAPI_Init\0")
                && (*init)()
            {
                return true;
            }
            if let Ok(init_safe) = lib.get::<unsafe extern "C" fn() -> bool>(b"SteamAPI_InitSafe\0")
                && (*init_safe)()
            {
                return true;
            }
        }
        false
    }

    fn ensure_steam_appid() {
        let value = format!("{APP_ID}\n");
        let dirs = std::env::current_dir()
            .ok()
            .into_iter()
            .chain(std::iter::once(crate::paths::exe_dir()));
        for dir in dirs {
            let _ = std::fs::write(dir.join("steam_appid.txt"), &value);
        }
    }

    fn steam_err(err: libloading::Error) -> Error {
        Error::Steam(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    struct MockUgc {
        state: Cell<u32>,
        folder: PathBuf,
        download_ok: bool,
        install_on_download: bool,
        subscribe_calls: Cell<u32>,
        download_calls: Cell<u32>,
        subscribed: Vec<u64>,
    }

    impl MockUgc {
        fn new(folder: PathBuf) -> Self {
            Self {
                state: Cell::new(0),
                folder,
                download_ok: true,
                install_on_download: true,
                subscribe_calls: Cell::new(0),
                download_calls: Cell::new(0),
                subscribed: Vec::new(),
            }
        }

        fn with_state(self, state: u32) -> Self {
            self.state.set(state);
            self
        }
    }

    impl UgcTransport for MockUgc {
        fn subscribed_ids(&self) -> Result<Vec<u64>> {
            Ok(self.subscribed.clone())
        }

        fn item_state(&self, _id: u64) -> Result<u32> {
            Ok(self.state.get())
        }

        fn item_install_info(&self, _id: u64) -> Result<Option<InstallInfo>> {
            if is_installed(self.state.get()) && self.folder.is_dir() {
                Ok(Some(InstallInfo {
                    size: 1,
                    folder: self.folder.clone(),
                    timestamp: 2,
                }))
            } else {
                Ok(None)
            }
        }

        fn subscribe(&self, _id: u64) -> Result<()> {
            self.subscribe_calls.set(self.subscribe_calls.get() + 1);
            self.state.set(self.state.get() | ITEM_SUBSCRIBED);
            Ok(())
        }

        fn unsubscribe(&self, _id: u64) -> Result<()> {
            Ok(())
        }

        fn download(&self, _id: u64, _high_priority: bool) -> Result<bool> {
            self.download_calls.set(self.download_calls.get() + 1);
            if !self.download_ok {
                return Ok(false);
            }
            if self.install_on_download {
                let ready = (self.state.get() | ITEM_INSTALLED)
                    & !(ITEM_NEEDS_UPDATE | ITEM_DOWNLOADING | ITEM_DOWNLOAD_PENDING);
                self.state.set(ready);
            }
            Ok(true)
        }

        fn pump(&self) {}
    }

    fn folder() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("3828587753");
        std::fs::create_dir_all(&folder).unwrap();
        (dir, folder)
    }

    #[test]
    fn state_helpers() {
        assert!(is_installed(ITEM_INSTALLED));
        assert!(is_updating(ITEM_DOWNLOADING));
        assert!(is_ready(ITEM_INSTALLED));
        assert!(!is_ready(ITEM_INSTALLED | ITEM_NEEDS_UPDATE));
    }

    #[test]
    fn fast_path_returns_folder_without_download() {
        let (_dir, folder) = folder();
        let mock = MockUgc::new(folder.clone()).with_state(ITEM_SUBSCRIBED | ITEM_INSTALLED);
        let workshop = SteamWorkshop::new(mock);

        let path = workshop.download_and_install(1, false).unwrap();
        assert_eq!(path, folder);
        assert_eq!(workshop.transport().download_calls.get(), 0);
    }

    #[test]
    fn installed_but_unsubscribed_subscribes_then_downloads() {
        let (_dir, folder) = folder();
        let mock = MockUgc::new(folder.clone()).with_state(ITEM_INSTALLED);
        let workshop = SteamWorkshop::new(mock);

        assert_eq!(workshop.download_and_install(1, false).unwrap(), folder);
        assert_eq!(workshop.transport().subscribe_calls.get(), 1);
        assert_eq!(workshop.transport().download_calls.get(), 1);
    }

    #[test]
    fn force_bypasses_fast_path() {
        let (_dir, folder) = folder();
        let mock = MockUgc::new(folder).with_state(ITEM_SUBSCRIBED | ITEM_INSTALLED);
        let workshop = SteamWorkshop::new(mock);

        workshop.download_and_install(1, true).unwrap();
        assert_eq!(workshop.transport().download_calls.get(), 1);
    }

    #[test]
    fn needs_update_redownloads() {
        let (_dir, folder) = folder();
        let mock =
            MockUgc::new(folder).with_state(ITEM_SUBSCRIBED | ITEM_INSTALLED | ITEM_NEEDS_UPDATE);
        let workshop = SteamWorkshop::new(mock);

        workshop.download_and_install(1, false).unwrap();
        assert_eq!(workshop.transport().download_calls.get(), 1);
    }

    #[test]
    fn rejected_download_is_an_error() {
        let (_dir, folder) = folder();
        let mut mock = MockUgc::new(folder).with_state(ITEM_SUBSCRIBED | ITEM_INSTALLED);
        mock.download_ok = false;
        let workshop = SteamWorkshop::new(mock);

        let err = workshop.download_and_install(1, true).unwrap_err();
        assert!(matches!(err, Error::Steam(_)));
        assert_eq!(workshop.transport().download_calls.get(), 1);
    }

    #[test]
    fn subscribed_ids_passthrough() {
        let (_dir, folder) = folder();
        let mut mock = MockUgc::new(folder);
        mock.subscribed = vec![111, 222];
        let workshop = SteamWorkshop::new(mock);
        assert_eq!(workshop.subscribed_ids().unwrap(), vec![111, 222]);
    }

    #[test]
    fn implements_workshop_source() {
        let (_dir, folder) = folder();
        let mock = MockUgc::new(folder.clone()).with_state(ITEM_SUBSCRIBED | ITEM_INSTALLED);
        let workshop = SteamWorkshop::new(mock);
        assert_eq!(workshop.ensure_installed(1).unwrap(), Some(folder));
    }

    #[cfg(feature = "steamworks")]
    #[test]
    fn missing_library_is_reported() {
        let result = crate::steam::flat::FlatSteamApi::open(Some(std::path::Path::new(
            "/nonexistent/libsteam_api.so",
        )));
        assert!(matches!(result, Err(Error::Steam(_))));
    }
}
