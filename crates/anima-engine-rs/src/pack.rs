//! `.mdaa` animation-pack parsing — ported from AnimaTriggers `mdaa_io.py` /
//! `models.py`.
//!
//! A `.mdaa` file is a ZIP (entries at the root) containing:
//!
//! - `pack.json`  — the [`Pack`] document.
//! - `meta.json`  — free-form editor state (optional).
//! - `assets/**`  — referenced media, with `animations[].asset_path` rewritten
//!   to `assets/<basename>`.
//! - `frames/*.npz` — NumPy `.npz` archives (each holds a `frames` array of
//!   shape `[T, H, W, C]`, uint8), read in filename order.
//!
//! Frames stay in memory as [`NpyArray`]; the tiny NPY header parser lives here
//! so no NumPy/npy dependency is needed.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, Result};

/// A drawing/capture region inside the event frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Region {
    pub id: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub kind: String,
    pub points: Option<Vec<[i32; 2]>>,
    pub name: String,
}

impl Default for Region {
    fn default() -> Self {
        Self {
            id: String::new(),
            x: 0,
            y: 0,
            w: 0,
            h: 0,
            kind: "rect".to_string(),
            points: None,
            name: String::new(),
        }
    }
}

/// The recorded event clip frames are matched against.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EventClip {
    pub id: String,
    pub name: String,
    pub fps: u32,
    pub frame_count: u32,
    pub start_index: i32,
    pub end_index: Option<i32>,
}

/// How captured frame regions are compared against the event clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MatchSettings {
    pub mode: String,
    pub threshold: f64,
    pub agg: String,
    pub region_weights: BTreeMap<String, f64>,
    pub cooldown_ms: i64,
    pub start_frames_to_check: u32,
    pub stop_on_mismatch: bool,
    pub stop_grace_ms: i64,
    pub cooldown_from: String,
    pub ref_mode: String,
    pub ref_frame_index: i32,
    pub seq_len: u32,
    pub seq_step: u32,
}

impl Default for MatchSettings {
    fn default() -> Self {
        Self {
            mode: "ORB_HASH".to_string(),
            threshold: 0.75,
            agg: "mean".to_string(),
            region_weights: BTreeMap::new(),
            cooldown_ms: 500,
            start_frames_to_check: 3,
            stop_on_mismatch: true,
            stop_grace_ms: 250,
            cooldown_from: "activate".to_string(),
            ref_mode: "anchor".to_string(),
            ref_frame_index: 0,
            seq_len: 8,
            seq_step: 1,
        }
    }
}

/// One timed action applied to an animation track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Action {
    pub t_ms: i64,
    pub kind: String,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub dur_ms: i64,
    pub move_mode: String,
    pub path: Option<String>,
    pub sound_path: Option<String>,
    pub volume: f64,
}

impl Default for Action {
    fn default() -> Self {
        Self {
            t_ms: 0,
            kind: String::new(),
            x: None,
            y: None,
            dur_ms: 0,
            move_mode: "smooth".to_string(),
            path: None,
            sound_path: None,
            volume: 1.0,
        }
    }
}

/// One overlay animation (its asset plus the actions that drive it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnimTrack {
    pub id: String,
    pub name: String,
    pub asset_path: String,
    pub start_visible: bool,
    pub actions: Vec<Action>,
}

impl Default for AnimTrack {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            asset_path: String::new(),
            start_visible: true,
            actions: Vec::new(),
        }
    }
}

/// The `pack.json` document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pack {
    pub id: String,
    pub name: String,
    pub created_ts: f64,
    pub regions: Vec<Region>,
    pub event: EventClip,
    #[serde(rename = "match")]
    pub match_settings: MatchSettings,
    pub animations: Vec<AnimTrack>,
    pub action_speed: f64,
}

impl Default for Pack {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            created_ts: 0.0,
            regions: Vec::new(),
            event: EventClip::default(),
            match_settings: MatchSettings::default(),
            animations: Vec::new(),
            action_speed: 1.0,
        }
    }
}

/// A parsed NumPy `.npy` array (header + raw little-endian data).
#[derive(Debug, Clone, PartialEq)]
pub struct NpyArray {
    pub descr: String,
    pub fortran_order: bool,
    pub shape: Vec<usize>,
    pub data: Vec<u8>,
}

impl NpyArray {
    pub fn is_uint8(&self) -> bool {
        self.descr.ends_with("u1")
    }

    /// Number of frames in a `[T, H, W, C]` array.
    pub fn frame_count(&self) -> usize {
        self.shape.first().copied().unwrap_or(0)
    }

    /// Raw `[H, W, C]` bytes for one frame.
    pub fn frame(&self, index: usize) -> Option<&[u8]> {
        if self.shape.len() != 4 || index >= self.frame_count() {
            return None;
        }
        let stride = self.shape[1] * self.shape[2] * self.shape[3];
        self.data.get(index * stride..index * stride + stride)
    }

    /// One frame normalized to straight RGBA8 (3-channel frames get alpha 255).
    pub fn frame_rgba(&self, index: usize) -> Option<Vec<u8>> {
        if !self.is_uint8() || self.fortran_order {
            return None;
        }
        let channels = *self.shape.get(3)?;
        let slice = self.frame(index)?;
        let mut out = Vec::with_capacity(slice.len() / channels * 4);
        match channels {
            4 => out.extend_from_slice(slice),
            3 => {
                for px in slice.as_chunks::<3>().0 {
                    out.extend_from_slice(&[px[0], px[1], px[2], 255]);
                }
            }
            _ => return None,
        }
        Some(out)
    }
}

/// A fully loaded `.mdaa` archive.
#[derive(Debug, Clone, PartialEq)]
pub struct Mdaa {
    pub pack: Pack,
    pub meta: Value,
    /// Frame arrays in `frames/*.npz` filename order (each `[T, H, W, C]`).
    pub frames_by_region: Vec<NpyArray>,
    /// ZIP entry name (`assets/<name>`) -> bytes.
    pub assets: BTreeMap<String, Vec<u8>>,
}

impl Mdaa {
    /// Read and parse a `.mdaa` file.
    pub fn read(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        Self::from_bytes(&bytes)
    }

    /// Parse `.mdaa` bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut archive =
            zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| Error::Pack(e.to_string()))?;

        let mut pack_json: Option<String> = None;
        let mut meta = Value::Null;
        let mut assets = BTreeMap::new();
        let mut frame_archives: Vec<(String, Vec<u8>)> = Vec::new();

        for index in 0..archive.len() {
            let mut entry = archive
                .by_index(index)
                .map_err(|e| Error::Pack(e.to_string()))?;
            let name = entry.name().to_string();
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;

            if name == "pack.json" {
                pack_json = Some(String::from_utf8_lossy(&data).into_owned());
            } else if name == "meta.json" {
                meta = serde_json::from_slice(&data).unwrap_or(Value::Null);
            } else if name.starts_with("assets/") && !entry.is_dir() {
                assets.insert(name, data);
            } else if name.starts_with("frames/") && name.ends_with(".npz") {
                frame_archives.push((name, data));
            }
        }

        let pack_json =
            pack_json.ok_or_else(|| Error::Pack("mdaa has no pack.json".to_string()))?;
        let pack: Pack = serde_json::from_str(&pack_json)?;

        frame_archives.sort_by(|a, b| a.0.cmp(&b.0));
        let mut frames_by_region = Vec::with_capacity(frame_archives.len());
        for (name, bytes) in frame_archives {
            let arrays = read_npz(&bytes)?;
            let array = arrays
                .into_iter()
                .find(|(key, _)| key == "frames")
                .map(|(_, array)| array)
                .ok_or_else(|| Error::Pack(format!("{name} has no 'frames' array")))?;
            frames_by_region.push(array);
        }

        Ok(Self {
            pack,
            meta,
            frames_by_region,
            assets,
        })
    }

    /// Asset bytes for a track, matched by basename (asset paths in the pack may
    /// be Windows-absolute or `assets/<name>`).
    pub fn asset_for_track(&self, track: &AnimTrack) -> Option<&[u8]> {
        let wanted = track.asset_path.rsplit(['/', '\\']).next()?;
        self.assets
            .iter()
            .find(|(name, _)| name.rsplit(['/', '\\']).next() == Some(wanted))
            .map(|(_, bytes)| bytes.as_slice())
    }
}

/// Read every `.npy` member of an in-memory `.npz`, keyed by array name (the
/// `.npy` suffix is stripped, matching `np.load(...)["frames"]`).
pub fn read_npz(bytes: &[u8]) -> Result<BTreeMap<String, NpyArray>> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| Error::Pack(e.to_string()))?;
    let mut arrays = BTreeMap::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|e| Error::Pack(e.to_string()))?;
        let name = entry.name().to_string();
        if !name.ends_with(".npy") {
            continue;
        }
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        let key = name.strip_suffix(".npy").unwrap_or(&name).to_string();
        arrays.insert(key, parse_npy(&data)?);
    }
    Ok(arrays)
}

/// Parse a NumPy `.npy` header (v1.0/v2.0) plus its data.
pub fn parse_npy(bytes: &[u8]) -> Result<NpyArray> {
    const MAGIC: &[u8] = b"\x93NUMPY";
    if !bytes.starts_with(MAGIC) || bytes.len() < 10 {
        return Err(Error::Pack("not a .npy file".to_string()));
    }
    let major = bytes[6];
    let (header_len, header_start) = if major == 1 {
        (u16::from_le_bytes([bytes[8], bytes[9]]) as usize, 10)
    } else {
        if bytes.len() < 12 {
            return Err(Error::Pack("truncated .npy header".to_string()));
        }
        (
            u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize,
            12,
        )
    };
    let header_end = header_start + header_len;
    let header = bytes
        .get(header_start..header_end)
        .ok_or_else(|| Error::Pack("truncated .npy header".to_string()))?;
    let header = String::from_utf8_lossy(header);

    Ok(NpyArray {
        descr: parse_descr(&header).unwrap_or_default(),
        fortran_order: parse_fortran_order(&header),
        shape: parse_shape(&header),
        data: bytes[header_end.min(bytes.len())..].to_vec(),
    })
}

fn parse_descr(header: &str) -> Option<String> {
    let rest = header.split("'descr':").nth(1)?.trim_start();
    let quote = rest.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let end = rest[1..].find(quote)?;
    Some(rest[1..1 + end].to_string())
}

fn parse_fortran_order(header: &str) -> bool {
    header
        .split("'fortran_order':")
        .nth(1)
        .map(|rest| rest.trim_start().starts_with("True"))
        .unwrap_or(false)
}

fn parse_shape(header: &str) -> Vec<usize> {
    let Some(rest) = header.split("'shape':").nth(1) else {
        return Vec::new();
    };
    let rest = rest.trim_start().strip_prefix('(').unwrap_or(rest);
    let inner = rest.split(')').next().unwrap_or("");
    inner
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<usize>().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_write(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, data) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    /// Minimal spec-correct NPY writer for uint8 arrays (C order).
    fn npy_u8(shape: &[usize], data: &[u8]) -> Vec<u8> {
        let dims = shape
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let dict = format!("{{'descr': '|u1', 'fortran_order': False, 'shape': ({dims}), }}");
        let preamble = 8 + 2 + 2;
        let mut header = dict;
        let pad = (64 - ((preamble + header.len() + 1) % 64)) % 64;
        header.push_str(&" ".repeat(pad));
        header.push('\n');

        let mut out = Vec::new();
        out.extend_from_slice(b"\x93NUMPY");
        out.push(1);
        out.push(0);
        out.extend_from_slice(&(header.len() as u16).to_le_bytes());
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(data);
        out
    }

    const DEMO_PACK: &str = r#"{
        "id": "p1", "name": "demo", "created_ts": 123.5,
        "regions": [{"id":"r0","x":1,"y":2,"w":3,"h":4,"kind":"rect","points":[[0,0],[1,1]],"name":"R"}],
        "event": {"id":"e","name":"ev","fps":30,"frame_count":2,"start_index":0,"end_index":1},
        "match": {"mode":"ORB_HASH","threshold":0.8,"region_weights":{"r0":1.0},
                  "cooldown_ms":500,"start_frames_to_check":3,"stop_on_mismatch":true,
                  "ref_mode":"sequencer","seq_len":4,"seq_step":2},
        "animations": [{"id":"a","name":"A","asset_path":"assets/foo.gif","start_visible":true,
                        "actions":[{"t_ms":0,"kind":"show","volume":0.5}]}],
        "action_speed": 1.5
    }"#;

    fn demo_frames_npz() -> Vec<u8> {
        let data: Vec<u8> = (0..2 * 3 * 4 * 3).map(|i| i as u8).collect();
        let npy = npy_u8(&[2, 3, 4, 3], &data);
        zip_write(&[("frames.npy", &npy)])
    }

    fn demo_mdaa() -> Vec<u8> {
        let npz = demo_frames_npz();
        zip_write(&[
            ("pack.json", DEMO_PACK.as_bytes()),
            ("meta.json", br#"{"zoom":2}"#),
            ("assets/foo.gif", b"GIF89a-fake"),
            ("frames/region0.npz", &npz),
        ])
    }

    #[test]
    fn npy_roundtrip() {
        let data: Vec<u8> = (0..24).collect();
        let npy = npy_u8(&[2, 3, 4], &data);
        let array = parse_npy(&npy).unwrap();
        assert_eq!(array.descr, "|u1");
        assert!(!array.fortran_order);
        assert_eq!(array.shape, vec![2, 3, 4]);
        assert_eq!(array.data, data);
        assert!(array.is_uint8());
    }

    #[test]
    fn reads_npz_members() {
        let arrays = read_npz(&demo_frames_npz()).unwrap();
        assert_eq!(arrays["frames"].shape, vec![2, 3, 4, 3]);
    }

    #[test]
    fn parses_mdaa_pack() {
        let mdaa = Mdaa::from_bytes(&demo_mdaa()).unwrap();

        assert_eq!(mdaa.pack.name, "demo");
        assert_eq!(mdaa.pack.action_speed, 1.5);
        assert_eq!(mdaa.pack.regions[0].w, 3);
        assert_eq!(mdaa.pack.regions[0].points, Some(vec![[0, 0], [1, 1]]));
        assert_eq!(mdaa.pack.event.fps, 30);
        assert_eq!(mdaa.pack.event.end_index, Some(1));
        assert_eq!(mdaa.pack.match_settings.mode, "ORB_HASH");
        assert_eq!(mdaa.pack.match_settings.ref_mode, "sequencer");
        assert_eq!(mdaa.pack.match_settings.seq_step, 2);
        assert_eq!(mdaa.pack.animations[0].asset_path, "assets/foo.gif");
        assert_eq!(mdaa.pack.animations[0].actions[0].volume, 0.5);
        assert_eq!(mdaa.meta["zoom"], 2);
    }

    #[test]
    fn exposes_frames_and_assets() {
        let mdaa = Mdaa::from_bytes(&demo_mdaa()).unwrap();

        assert_eq!(mdaa.frames_by_region.len(), 1);
        let region = &mdaa.frames_by_region[0];
        assert_eq!(region.frame_count(), 2);
        assert_eq!(region.frame(1).unwrap().len(), 3 * 4 * 3);

        // 3-channel frame expands to RGBA.
        let rgba = region.frame_rgba(0).unwrap();
        assert_eq!(rgba.len(), 3 * 4 * 4);
        assert_eq!(&rgba[0..4], &[0, 1, 2, 255]);

        assert_eq!(
            mdaa.asset_for_track(&mdaa.pack.animations[0]),
            Some(&b"GIF89a-fake"[..])
        );
    }

    #[test]
    fn defaults_fill_missing_fields() {
        let minimal = zip_write(&[
            ("pack.json", br#"{"name":"tiny","regions":[{"id":"r"}]}"#),
            ("frames/region0.npz", &demo_frames_npz()),
        ]);
        let mdaa = Mdaa::from_bytes(&minimal).unwrap();
        assert_eq!(mdaa.pack.name, "tiny");
        assert_eq!(mdaa.pack.action_speed, 1.0);
        assert_eq!(mdaa.pack.match_settings.mode, "ORB_HASH");
        assert_eq!(mdaa.pack.regions.len(), 1);
        assert_eq!(mdaa.pack.regions[0].kind, "rect");
        assert_eq!(mdaa.pack.animations.len(), 0);
        assert_eq!(mdaa.meta, Value::Null);
    }

    #[test]
    fn missing_pack_json_errors() {
        let bytes = zip_write(&[("frames/region0.npz", &demo_frames_npz())]);
        assert!(matches!(Mdaa::from_bytes(&bytes), Err(Error::Pack(_))));
    }

    #[test]
    fn reads_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.mdaa");
        std::fs::write(&path, demo_mdaa()).unwrap();
        let mdaa = Mdaa::read(&path).unwrap();
        assert_eq!(mdaa.pack.name, "demo");
    }
}
