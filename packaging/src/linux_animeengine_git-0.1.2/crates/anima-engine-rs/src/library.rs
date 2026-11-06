//! Library configuration and sprite-sheet slicing.
//!
//! Ported from `customtkinter_MInteraDesktop.py`. The library files are plain
//! JSON:
//!
//! - `gifs.json`             — an array of animation paths.
//! - `sprites.json`          — `{ "<state>": ["path", …] }` (static/grabbed/…).
//! - `spritesSecuencia.json` — `{ "<state>": [frame index, …] }`.
//! - `sprite_config.json`    — `{ "<path><state>": <sprite frame config> }`.
//!
//! Sprite sheets are sliced on a grid: `start_frame` is **1-based**, `cols` is
//! `sheet_width / (frame_width + spacing_h)`, and out-of-bounds cells are padded
//! transparent (matching Pillow's `crop`).

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::Path;

use image::RgbaImage;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::Result;
use crate::anim::{Animation, Frame};

/// `gifs.json`
pub type GifLibrary = Vec<String>;
/// `sprites.json` — state name -> sheet paths.
pub type SpriteLibrary = BTreeMap<String, Vec<String>>;
/// `spritesSecuencia.json` — state name -> frame indices.
pub type SpriteSequence = BTreeMap<String, Vec<u32>>;
/// `sprite_config.json` — `<path><state>` -> frame config.
pub type SpriteConfigs = BTreeMap<String, SpriteFrameConfig>;

pub const GIFS_FILE: &str = "gifs.json";
pub const SPRITES_FILE: &str = "sprites.json";
pub const SPRITE_SEQUENCE_FILE: &str = "spritesSecuencia.json";
pub const SPRITE_CONFIG_FILE: &str = "sprite_config.json";

/// Read a JSON file into `T`, falling back to `T::default()` when it is missing
/// or malformed (mirrors the forgiving `load_paths`).
pub fn load<T: DeserializeOwned + Default>(path: &Path) -> T {
    crate::paths::load_json(path)
}

/// Write `value` as JSON atomically.
pub fn save<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    crate::paths::save_json(path, value)
}

/// Look up the frame config for a sheet, keyed by `path + state` (the Python
/// build concatenated the two strings), falling back to the defaults.
pub fn sprite_config_for<'a>(
    configs: &'a SpriteConfigs,
    path: &str,
    state: &str,
) -> Cow<'a, SpriteFrameConfig> {
    match configs.get(&format!("{path}{state}")) {
        Some(config) => Cow::Borrowed(config),
        None => Cow::Owned(SpriteFrameConfig::default()),
    }
}

/// Grid slicing parameters for a sprite sheet.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SpriteFrameConfig {
    pub frame_width: u32,
    pub frame_height: u32,
    pub sheet_padding_left: u32,
    pub sheet_padding_top: u32,
    pub sheet_spacing_horizontal: u32,
    pub sheet_spacing_vertical: u32,
    pub frame_count: u32,
    pub frame_time: u32,
    pub start_frame: u32,
    pub zoom: f64,
}

impl Default for SpriteFrameConfig {
    fn default() -> Self {
        Self {
            frame_width: 64,
            frame_height: 64,
            sheet_padding_left: 0,
            sheet_padding_top: 0,
            sheet_spacing_horizontal: 0,
            sheet_spacing_vertical: 0,
            frame_count: 4,
            frame_time: 100,
            start_frame: 1,
            zoom: 1.0,
        }
    }
}

/// A crop rectangle in sheet pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRect {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

impl FrameRect {
    pub fn width(&self) -> u32 {
        self.right - self.left
    }

    pub fn height(&self) -> u32 {
        self.bottom - self.top
    }
}

impl SpriteFrameConfig {
    /// The crop rectangle for each of `frame_count` frames laid out on a grid
    /// `sheet_width` pixels wide. Empty when a single cell cannot fit.
    pub fn frame_rects(&self, sheet_width: u32) -> Vec<FrameRect> {
        let cell_w = self
            .frame_width
            .saturating_add(self.sheet_spacing_horizontal);
        let cell_h = self
            .frame_height
            .saturating_add(self.sheet_spacing_vertical);
        let cols = sheet_width.checked_div(cell_w).unwrap_or(0);
        if cols == 0 {
            return Vec::new();
        }

        let start = self.start_frame.saturating_sub(1);
        (0..self.frame_count)
            .map(|i| {
                let index = start + i;
                let row = index / cols;
                let col = index % cols;
                let left = self.sheet_padding_left + col * cell_w;
                let top = self.sheet_padding_top + row * cell_h;
                FrameRect {
                    left,
                    top,
                    right: left + self.frame_width,
                    bottom: top + self.frame_height,
                }
            })
            .collect()
    }

    /// Output frame size after `zoom` (truncating, never zero).
    pub fn output_size(&self) -> (u32, u32) {
        let width = (f64::from(self.frame_width) * self.zoom) as u32;
        let height = (f64::from(self.frame_height) * self.zoom) as u32;
        (width.max(1), height.max(1))
    }

    /// Slice `sheet` into a playable [`Animation`] (delay = `frame_time`).
    pub fn animation_from_sheet(&self, sheet: &RgbaImage) -> Animation {
        let (width, height) = self.output_size();
        let frames = slice_sprite_sheet(sheet, self)
            .into_iter()
            .map(|frame| Frame {
                alpha_bits: Vec::new(),
                rgba: frame.into_raw(),
                delay_ms: self.frame_time,
            })
            .collect();
        Animation {
            width,
            height,
            frames,
        }
    }
}

/// Crop and (optionally) zoom every frame described by `config`.
pub fn slice_sprite_sheet(sheet: &RgbaImage, config: &SpriteFrameConfig) -> Vec<RgbaImage> {
    let size = config.output_size();
    let unzoomed = (config.frame_width, config.frame_height);

    config
        .frame_rects(sheet.width())
        .into_iter()
        .map(|rect| {
            let frame = crop_frame(sheet, rect, config.frame_width, config.frame_height);
            if size == unzoomed {
                frame
            } else {
                image::imageops::resize(
                    &frame,
                    size.0,
                    size.1,
                    image::imageops::FilterType::Lanczos3,
                )
            }
        })
        .collect()
}

/// Crop `(left, top)` with the given size, padding out-of-bounds areas with
/// transparent pixels (Pillow's `crop` behaviour).
fn crop_frame(sheet: &RgbaImage, rect: FrameRect, width: u32, height: u32) -> RgbaImage {
    if rect.right <= sheet.width() && rect.bottom <= sheet.height() {
        return image::imageops::crop_imm(sheet, rect.left, rect.top, width, height).to_image();
    }

    let mut out = RgbaImage::new(width, height);
    let copy_w = rect.right.min(sheet.width()).saturating_sub(rect.left);
    let copy_h = rect.bottom.min(sheet.height()).saturating_sub(rect.top);
    if copy_w > 0 && copy_h > 0 {
        let region =
            image::imageops::crop_imm(sheet, rect.left, rect.top, copy_w, copy_h).to_image();
        image::imageops::replace(&mut out, &region, 0, 0);
    }
    out
}

/// Convenience: is `(x, y)` inside a frame rect?
pub fn rect_contains(rect: FrameRect, x: u32, y: u32) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn rect(left: u32, top: u32, right: u32, bottom: u32) -> FrameRect {
        FrameRect {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn defaults_match_python() {
        let config = SpriteFrameConfig::default();
        assert_eq!((config.frame_width, config.frame_height), (64, 64));
        assert_eq!(config.frame_count, 4);
        assert_eq!(config.frame_time, 100);
        assert_eq!(config.start_frame, 1);
        assert_eq!(config.zoom, 1.0);
    }

    #[test]
    fn rects_lay_out_on_a_grid() {
        let config = SpriteFrameConfig {
            frame_width: 64,
            frame_height: 64,
            frame_count: 2,
            start_frame: 1,
            ..Default::default()
        };
        assert_eq!(
            config.frame_rects(128),
            vec![rect(0, 0, 64, 64), rect(64, 0, 128, 64)]
        );
    }

    #[test]
    fn start_frame_is_one_based() {
        let config = SpriteFrameConfig {
            frame_width: 64,
            frame_height: 64,
            frame_count: 1,
            start_frame: 2,
            ..Default::default()
        };
        assert_eq!(config.frame_rects(128), vec![rect(64, 0, 128, 64)]);
    }

    #[test]
    fn padding_and_spacing_are_applied() {
        let config = SpriteFrameConfig {
            frame_width: 20,
            frame_height: 20,
            sheet_padding_left: 5,
            sheet_padding_top: 3,
            sheet_spacing_horizontal: 2,
            frame_count: 3,
            start_frame: 1,
            ..Default::default()
        };
        // cols = 100 / (20 + 2) = 4
        assert_eq!(
            config.frame_rects(100),
            vec![rect(5, 3, 25, 23), rect(27, 3, 47, 23), rect(49, 3, 69, 23),]
        );
    }

    #[test]
    fn too_narrow_sheet_yields_no_frames() {
        let config = SpriteFrameConfig {
            frame_width: 64,
            frame_height: 64,
            ..Default::default()
        };
        assert!(config.frame_rects(32).is_empty());
    }

    #[test]
    fn output_size_applies_zoom() {
        let config = SpriteFrameConfig {
            frame_width: 64,
            frame_height: 64,
            zoom: 1.5,
            ..Default::default()
        };
        assert_eq!(config.output_size(), (96, 96));
    }

    #[test]
    fn slice_extracts_side_by_side_frames() {
        let mut sheet = RgbaImage::new(4, 2);
        for y in 0..2 {
            for x in 0..4 {
                let pixel = if x < 2 {
                    Rgba([255, 0, 0, 255])
                } else {
                    Rgba([0, 255, 0, 255])
                };
                sheet.put_pixel(x, y, pixel);
            }
        }
        let config = SpriteFrameConfig {
            frame_width: 2,
            frame_height: 2,
            frame_count: 2,
            start_frame: 1,
            ..Default::default()
        };
        let frames = slice_sprite_sheet(&sheet, &config);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].dimensions(), (2, 2));
        assert_eq!(*frames[0].get_pixel(0, 0), Rgba([255, 0, 0, 255]));
        assert_eq!(*frames[1].get_pixel(0, 0), Rgba([0, 255, 0, 255]));
    }

    #[test]
    fn slice_applies_zoom() {
        let sheet = RgbaImage::from_pixel(4, 4, Rgba([1, 2, 3, 255]));
        let config = SpriteFrameConfig {
            frame_width: 2,
            frame_height: 2,
            frame_count: 2,
            start_frame: 1,
            zoom: 2.0,
            ..Default::default()
        };
        let frames = slice_sprite_sheet(&sheet, &config);
        assert_eq!(frames[0].dimensions(), (4, 4));
    }

    #[test]
    fn out_of_bounds_frame_is_padded_transparent() {
        let sheet = RgbaImage::from_pixel(4, 2, Rgba([9, 9, 9, 255]));
        let config = SpriteFrameConfig {
            frame_width: 4,
            frame_height: 2,
            sheet_padding_top: 1,
            frame_count: 1,
            start_frame: 1,
            ..Default::default()
        };
        let frames = slice_sprite_sheet(&sheet, &config);
        assert_eq!(frames[0].dimensions(), (4, 2));
        assert_eq!(*frames[0].get_pixel(0, 0), Rgba([9, 9, 9, 255]));
        assert_eq!(*frames[0].get_pixel(0, 1), Rgba([0, 0, 0, 0]));
    }

    #[test]
    fn sprite_config_lookup_uses_path_plus_state() {
        let mut configs = SpriteConfigs::new();
        configs.insert(
            "sheet.pngstatic".to_string(),
            SpriteFrameConfig {
                frame_count: 6,
                ..Default::default()
            },
        );
        assert_eq!(
            sprite_config_for(&configs, "sheet.png", "static").frame_count,
            6
        );
        assert_eq!(
            sprite_config_for(&configs, "sheet.png", "grabbed").frame_count,
            4
        );
    }

    #[test]
    fn libraries_roundtrip() {
        let dir = tempfile::tempdir().unwrap();

        let gifs: GifLibrary = vec!["a.gif".into(), "b.apng".into()];
        let gifs_path = dir.path().join(GIFS_FILE);
        save(&gifs_path, &gifs).unwrap();
        assert_eq!(load::<GifLibrary>(&gifs_path), gifs);

        let mut configs = SpriteConfigs::new();
        configs.insert("s.pngstatic".into(), SpriteFrameConfig::default());
        let config_path = dir.path().join(SPRITE_CONFIG_FILE);
        save(&config_path, &configs).unwrap();
        assert_eq!(load::<SpriteConfigs>(&config_path), configs);
    }

    #[test]
    fn parses_shipped_shapes() {
        let sprites: SpriteLibrary =
            serde_json::from_str(r#"{"static":[],"grabbed":[],"released":[]}"#).unwrap();
        assert_eq!(sprites.len(), 3);

        let sequence: SpriteSequence =
            serde_json::from_str(r#"{"static":[1,2],"grabbed":[1,2]}"#).unwrap();
        assert_eq!(sequence["static"], vec![1, 2]);
    }

    #[test]
    fn missing_or_malformed_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.json");
        assert!(load::<GifLibrary>(&path).is_empty());
        std::fs::write(&path, b"garbage").unwrap();
        assert!(load::<GifLibrary>(&path).is_empty());
    }
}
