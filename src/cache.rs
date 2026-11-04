//! Frame cache — ported from `monoModule.py`.
//!
//! Decoding and compositing an animation (GIF delta frames, APNG blend/dispose,
//! …) is expensive. The first decode writes the *composited* RGBA frames to a
//! file keyed by a content hash of the source; later loads read that file instead
//! of re-decoding. A size cap evicts the least-recently-used entries, mirroring
//! the Python `_cleanup_cache`.
//!
//! Layout of an entry (`.animacache`), little-endian:
//!
//! ```text
//! b"ANIMCACH"                       8-byte magic
//! u32 version, width, height, count
//! u32 delays[count]
//! zstd stream of count * width * height * 4 raw RGBA bytes
//! ```
//!
//! One zstd stream over the raw frames replaced the old PNG-per-frame ZIP:
//! PNG-encoding large frames cost far more CPU than it saved in bytes (see
//! `examples/cache_bench.rs`).

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::anim::{Animation, Frame};
use crate::paths::{ensure_dir, user_data_dir};
use crate::{Error, Result};

/// Default cache size cap (512 MiB), matching the Python build.
pub const DEFAULT_MAX_BYTES: u64 = 512 * 1024 * 1024;

const CACHE_VERSION: u64 = 3;
const EXTENSION: &str = "animacache";
const MAGIC: &[u8; 8] = b"ANIMCACH";
/// zstd level: fast enough to hide behind playback, still finds frame redundancy.
const COMPRESSION_LEVEL: i32 = 3;

/// A content-addressed store of composited animation frames.
pub struct FrameCache {
    root: PathBuf,
    max_bytes: u64,
}

impl FrameCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }

    pub fn with_limit(root: impl Into<PathBuf>, max_bytes: u64) -> Self {
        Self {
            root: root.into(),
            max_bytes,
        }
    }

    /// `<user data dir>/frame_cache_v2` (replaces `%LOCALAPPDATA%\AnimaEngine\frame_cache_v2`).
    pub fn user_default() -> Self {
        Self::new(user_data_dir().join("frame_cache_v2"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// Path of the cache entry for `source`.
    pub fn entry_path(&self, source: &Path) -> Result<PathBuf> {
        Ok(self
            .root
            .join(format!("{}.{}", cache_key(source)?, EXTENSION)))
    }

    /// Read cached frames for `source`, or `None` when absent or corrupt.
    /// A corrupt entry is discarded so the next run decodes cleanly.
    pub fn read(&self, source: &Path) -> Option<Animation> {
        let path = self.entry_path(source).ok()?;
        if !path.exists() {
            return None;
        }
        match read_entry(&path) {
            Ok(animation) => {
                touch(&path);
                Some(animation)
            }
            Err(_) => {
                let _ = fs::remove_file(&path);
                None
            }
        }
    }

    /// Store composited frames for `source` (atomic), then enforce the size cap.
    pub fn write(&self, source: &Path, animation: &Animation) -> Result<()> {
        let path = self.entry_path(source)?;
        write_entry(&path, animation)?;
        self.enforce_limit();
        Ok(())
    }

    /// Evict least-recently-used entries until the total is at or below 80% of
    /// the cap. Returns the bytes remaining. Never fails.
    pub fn enforce_limit(&self) -> u64 {
        let mut entries = self.entries();
        let mut total: u64 = entries.iter().map(|e| e.size).sum();
        if total <= self.max_bytes {
            return total;
        }

        let target = self.max_bytes / 5 * 4; // 80%
        entries.sort_by_key(|e| e.modified);
        for entry in entries {
            if total <= target {
                break;
            }
            if fs::remove_file(&entry.path).is_ok() {
                total = total.saturating_sub(entry.size);
            }
        }
        total
    }

    /// Remove every cache entry. Never fails.
    pub fn clear(&self) {
        for entry in self.entries() {
            let _ = fs::remove_file(entry.path);
        }
    }

    fn entries(&self) -> Vec<Entry> {
        let Ok(dir) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some(EXTENSION) {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_file() {
                out.push(Entry {
                    path,
                    size: meta.len(),
                    modified: meta.modified().unwrap_or(UNIX_EPOCH),
                });
            }
        }
        out
    }
}

struct Entry {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
}

/// Cache key: sha256 of `resolved path | size | mtime_ns | version`, like the
/// Python `_cache_key`. Changes whenever the source file changes.
pub fn cache_key(source: &Path) -> Result<String> {
    let resolved = fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
    let meta = fs::metadata(&resolved)?;
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());

    let mut hasher = Sha256::new();
    hasher.update(
        format!(
            "{}|{}|{}|v{}",
            resolved.display(),
            meta.len(),
            mtime_ns,
            CACHE_VERSION
        )
        .as_bytes(),
    );
    Ok(hex(hasher.finalize().as_ref()))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

fn read_entry(path: &Path) -> Result<Animation> {
    let mut file = BufReader::new(File::open(path)?);

    let mut magic = [0u8; MAGIC.len()];
    file.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(Error::Cache("not an AnimaEngine cache entry".into()));
    }
    let version = read_u32(&mut file)?;
    if u64::from(version) != CACHE_VERSION {
        return Err(Error::Cache(format!("cache version {version}")));
    }

    let width = read_u32(&mut file)?;
    let height = read_u32(&mut file)?;
    let count = read_u32(&mut file)? as usize;
    let mut delays = Vec::with_capacity(count);
    for _ in 0..count {
        delays.push(read_u32(&mut file)?);
    }

    // Stream the frames straight out of the zstd payload: no second full-size
    // buffer of the whole animation.
    let frame_bytes = width as usize * height as usize * 4;
    let mut decoder = zstd::stream::read::Decoder::new(file).map_err(cache_err)?;
    let mut frames = Vec::with_capacity(count);
    for delay_ms in delays {
        let mut rgba = vec![0u8; frame_bytes];
        decoder.read_exact(&mut rgba)?;
        frames.push(Frame { rgba, delay_ms });
    }

    Ok(Animation {
        width,
        height,
        frames,
    })
}

fn write_entry(path: &Path, animation: &Animation) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    ensure_dir(dir)?;

    let tmp = dir.join(format!(".{}.tmp", std::process::id()));
    {
        let mut file = BufWriter::new(File::create(&tmp)?);
        file.write_all(MAGIC)?;
        file.write_all(&(CACHE_VERSION as u32).to_le_bytes())?;
        file.write_all(&animation.width.to_le_bytes())?;
        file.write_all(&animation.height.to_le_bytes())?;
        file.write_all(&(animation.frames.len() as u32).to_le_bytes())?;
        for frame in &animation.frames {
            file.write_all(&frame.delay_ms.to_le_bytes())?;
        }

        let mut encoder =
            zstd::stream::write::Encoder::new(file, COMPRESSION_LEVEL).map_err(cache_err)?;
        for frame in &animation.frames {
            encoder.write_all(&frame.rgba)?;
        }
        let mut file = encoder.finish().map_err(cache_err)?;
        file.flush()?;
    }

    fs::rename(&tmp, path)?;
    Ok(())
}

fn read_u32(reader: &mut impl Read) -> Result<u32> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

/// Bump the entry's mtime so LRU eviction keeps hot entries.
fn touch(path: &Path) {
    if let Ok(file) = File::options().write(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}

fn cache_err(err: std::io::Error) -> Error {
    Error::Cache(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn source(dir: &Path, name: &str, contents: &[u8]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, contents).unwrap();
        path
    }

    fn two_frame_animation() -> Animation {
        Animation {
            width: 2,
            height: 2,
            frames: vec![
                Frame {
                    rgba: [255, 0, 0, 255].repeat(4),
                    delay_ms: 50,
                },
                Frame {
                    rgba: [0, 255, 0, 255].repeat(4),
                    delay_ms: 70,
                },
            ],
        }
    }

    #[test]
    fn key_changes_with_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = source(dir.path(), "a.gif", b"one");
        let first = cache_key(&path).unwrap();
        fs::write(&path, b"a longer payload").unwrap();
        let second = cache_key(&path).unwrap();
        assert_ne!(first, second);
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let src = source(dir.path(), "clip.gif", b"data");
        let cache = FrameCache::new(dir.path().join("cache"));
        let animation = two_frame_animation();

        cache.write(&src, &animation).unwrap();
        let loaded = cache.read(&src).unwrap();
        assert_eq!(loaded, animation);
    }

    #[test]
    fn missing_source_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let cache = FrameCache::new(dir.path().join("cache"));
        assert!(cache.read(&dir.path().join("nope.gif")).is_none());
    }

    #[test]
    fn corrupt_entry_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let src = source(dir.path(), "clip.gif", b"data");
        let cache = FrameCache::new(dir.path().join("cache"));
        let animation = two_frame_animation();
        cache.write(&src, &animation).unwrap();

        let entry = cache.entry_path(&src).unwrap();
        fs::write(&entry, b"not an animacache").unwrap();

        assert!(cache.read(&src).is_none());
        assert!(!entry.exists());
    }

    #[test]
    fn entry_header_describes_the_frames() {
        let dir = tempfile::tempdir().unwrap();
        let src = source(dir.path(), "clip.gif", b"data");
        let cache = FrameCache::new(dir.path().join("cache"));
        cache.write(&src, &two_frame_animation()).unwrap();

        let bytes = fs::read(cache.entry_path(&src).unwrap()).unwrap();
        assert_eq!(&bytes[..8], MAGIC);
        assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 3);
        assert_eq!(u32::from_le_bytes(bytes[12..16].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(bytes[20..24].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 50);
    }

    #[test]
    fn entry_claiming_more_frames_than_it_has_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let src = source(dir.path(), "clip.gif", b"data");
        let cache = FrameCache::new(dir.path().join("cache"));
        cache.write(&src, &two_frame_animation()).unwrap();

        let entry = cache.entry_path(&src).unwrap();
        let mut bytes = fs::read(&entry).unwrap();
        bytes[20..24].copy_from_slice(&9u32.to_le_bytes());
        fs::write(&entry, &bytes).unwrap();

        assert!(cache.read(&src).is_none());
        assert!(!entry.exists());
    }

    #[test]
    fn frames_and_delays_survive_the_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let src = source(dir.path(), "clip.gif", b"data");
        let cache = FrameCache::new(dir.path().join("cache"));
        let animation = two_frame_animation();
        cache.write(&src, &animation).unwrap();

        let loaded = cache.read(&src).unwrap();
        assert_eq!(loaded.frames[0].delay_ms, 50);
        assert_eq!(loaded.frames[1].delay_ms, 70);
        assert_eq!(loaded.frames[0].rgba, animation.frames[0].rgba);
    }

    #[test]
    fn enforce_limit_evicts_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let src_a = source(dir.path(), "a.gif", b"aaaa");
        let src_b = source(dir.path(), "b.gif", b"bbbb");
        let anim = two_frame_animation();

        let cache = FrameCache::new(dir.path().join("cache"));
        cache.write(&src_a, &anim).unwrap();
        cache.write(&src_b, &anim).unwrap();

        let entry_a = cache.entry_path(&src_a).unwrap();
        let entry_b = cache.entry_path(&src_b).unwrap();
        let size = fs::metadata(&entry_a).unwrap().len();

        // Make A the least recently used, and set the cap so both fit nowhere.
        File::options()
            .write(true)
            .open(&entry_a)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(1_000))
            .unwrap();

        let cache = FrameCache::with_limit(dir.path().join("cache"), size * 2 - 1);
        let remaining = cache.enforce_limit();

        assert!(!entry_a.exists(), "oldest entry should be evicted");
        assert!(entry_b.exists(), "newest entry should survive");
        assert!(remaining <= size * 2);
    }

    #[test]
    fn clear_removes_entries() {
        let dir = tempfile::tempdir().unwrap();
        let src = source(dir.path(), "clip.gif", b"data");
        let cache = FrameCache::new(dir.path().join("cache"));
        cache.write(&src, &two_frame_animation()).unwrap();
        let entry = cache.entry_path(&src).unwrap();
        assert!(entry.exists());

        cache.clear();
        assert!(!entry.exists());
    }
}
