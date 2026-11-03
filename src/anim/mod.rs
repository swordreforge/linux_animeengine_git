//! Animation decoding — the pure "media" layer.
//!
//! Every runtime animation (GIF, APNG, WebP) is normalized into the same shape:
//! a fully composited RGBA frame list with per-frame delays. The heavy lifting
//! (sub-frame offsets, blend and dispose) is done by the `image` crate's
//! `AnimationDecoder`, which already composites each frame onto the logical
//! canvas — the same result `nuevoMotor.iter_composited_frames` produced in the
//! Python build. Downstream logic (frame cache, playback, hit-testing) is
//! therefore format-agnostic.

use std::path::Path;

use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, ImageFormat, ImageReader};

use crate::{Error, Result};

/// One composited frame: straight (non-premultiplied) RGBA8 plus display delay.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// `width * height * 4` bytes, row-major, straight RGBA.
    pub rgba: Vec<u8>,
    pub delay_ms: u32,
}

impl Frame {
    /// Extract the alpha channel (used for pixel hit-testing the overlay).
    pub fn alpha_mask(&self) -> Vec<u8> {
        self.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|px| px[3])
            .collect()
    }
}

/// A decoded animation: every frame shares the logical canvas size.
#[derive(Debug, Clone, PartialEq)]
pub struct Animation {
    pub width: u32,
    pub height: u32,
    pub frames: Vec<Frame>,
}

impl Animation {
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Alpha (0-255) of a frame pixel; 0 when out of range.
    pub fn alpha_at(&self, frame: usize, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        let Some(frame) = self.frames.get(frame) else {
            return 0;
        };
        let index = (y as usize * self.width as usize + x as usize) * 4 + 3;
        frame.rgba.get(index).copied().unwrap_or(0)
    }

    /// Hit-test a point in surface-local coordinates, where the frame is
    /// stretched to `surface` size. Used for drag-on-solid-pixel.
    pub fn alpha_hit(
        &self,
        frame: usize,
        surface: (u32, u32),
        sx: f64,
        sy: f64,
        threshold: u8,
    ) -> bool {
        let (sw, sh) = surface;
        if sw == 0 || sh == 0 || self.width == 0 || self.height == 0 {
            return false;
        }
        if !sx.is_finite() || !sy.is_finite() {
            return false;
        }
        let fx = (sx * f64::from(self.width) / f64::from(sw)).floor();
        let fy = (sy * f64::from(self.height) / f64::from(sh)).floor();
        if fx < 0.0 || fy < 0.0 {
            return false;
        }
        let (fx, fy) = (fx as u32, fy as u32);
        if fx >= self.width || fy >= self.height {
            return false;
        }
        self.alpha_at(frame, fx, fy) >= threshold
    }

    /// Rectangles `(x, y, w, h)` in surface-local coordinates covering the
    /// opaque pixels of `frame`, where each frame pixel is stretched to
    /// `surface`. Wayland input regions are rectangle lists, so rows with
    /// identical opaque runs are merged into taller rectangles.
    pub fn alpha_rects(
        &self,
        frame: usize,
        surface: (u32, u32),
        threshold: u8,
    ) -> Vec<(i32, i32, i32, i32)> {
        let (sw, sh) = surface;
        if sw == 0 || sh == 0 || self.width == 0 || self.height == 0 {
            return Vec::new();
        }
        let Some(frame) = self.frames.get(frame) else {
            return Vec::new();
        };

        let mut rects = Vec::new();
        let mut band: Vec<(u32, u32)> = Vec::new();
        let mut band_start = 0u32;
        let mut band_height = 0u32;

        for y in 0..self.height {
            let runs = row_runs(frame, y, self.width, threshold);
            if y == 0 {
                band = runs;
                band_start = 0;
                band_height = 1;
            } else if runs == band {
                band_height += 1;
            } else {
                emit_band(
                    &band,
                    band_start,
                    band_height,
                    surface,
                    (self.width, self.height),
                    &mut rects,
                );
                band = runs;
                band_start = y;
                band_height = 1;
            }
        }
        emit_band(
            &band,
            band_start,
            band_height,
            surface,
            (self.width, self.height),
            &mut rects,
        );
        rects
    }
}

/// Horizontal opaque runs `[x0, x1)` of one frame row.
fn row_runs(frame: &Frame, y: u32, width: u32, threshold: u8) -> Vec<(u32, u32)> {
    let row_start = y as usize * width as usize * 4;
    let Some(row) = frame.rgba.get(row_start..row_start + width as usize * 4) else {
        return Vec::new();
    };

    let mut runs = Vec::new();
    let mut start: Option<u32> = None;
    for (x, pixel) in row.chunks_exact(4).enumerate() {
        let solid = pixel[3] >= threshold;
        match (solid, start) {
            (true, None) => start = Some(x as u32),
            (false, Some(from)) => {
                runs.push((from, x as u32));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        runs.push((from, width));
    }
    runs
}

/// Scale a merged row band to surface coordinates and append its rectangles.
fn emit_band(
    runs: &[(u32, u32)],
    y0: u32,
    height: u32,
    surface: (u32, u32),
    frame: (u32, u32),
    rects: &mut Vec<(i32, i32, i32, i32)>,
) {
    if runs.is_empty() {
        return;
    }
    let (sw, sh) = surface;
    let (fw, fh) = frame;
    let sy = (u64::from(y0) * u64::from(sh) / u64::from(fh)) as i32;
    let sy_end = (u64::from(y0 + height) * u64::from(sh) / u64::from(fh)) as i32;
    let rect_height = (sy_end - sy).max(1);
    for &(x0, x1) in runs {
        let sx = (u64::from(x0) * u64::from(sw) / u64::from(fw)) as i32;
        let sx_end = (u64::from(x1) * u64::from(sw) / u64::from(fw)) as i32;
        rects.push((sx, sy, (sx_end - sx).max(1), rect_height));
    }
}

/// Smallest rectangle containing all `rects`.
pub fn bounds_rect(rects: &[(i32, i32, i32, i32)]) -> Option<(i32, i32, i32, i32)> {
    let mut iter = rects.iter();
    let &(x, y, w, h) = iter.next()?;
    let (mut x0, mut y0, mut x1, mut y1) = (x, y, x + w, y + h);
    for &(x, y, w, h) in iter {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x + w);
        y1 = y1.max(y + h);
    }
    Some((x0, y0, x1 - x0, y1 - y0))
}

/// A decoder that turns a file into a composited [`Animation`].
pub trait FrameSource {
    fn decode(&self, path: &Path) -> Result<Animation>;
}

/// Decoder backed by the `image` crate.
pub struct ImageSource;

impl FrameSource for ImageSource {
    fn decode(&self, path: &Path) -> Result<Animation> {
        decode_path(path)
    }
}

/// Decode any supported animation file into composited frames.
pub fn decode(path: &Path) -> Result<Animation> {
    ImageSource.decode(path)
}

/// Decode `path` using the `image` crate, dispatching on the detected format.
pub fn decode_path(path: &Path) -> Result<Animation> {
    let reader = open(path)?;
    match reader.format() {
        Some(ImageFormat::Gif) => {
            let frames = GifDecoder::new(reader.into_inner())
                .map_err(decode_err)?
                .into_frames()
                .collect_frames()
                .map_err(decode_err)?;
            animation_from_frames(frames)
        }
        Some(ImageFormat::Png) => {
            let decoder = PngDecoder::new(reader.into_inner()).map_err(decode_err)?;
            if decoder.is_apng().map_err(decode_err)? {
                let frames = decoder
                    .apng()
                    .map_err(decode_err)?
                    .into_frames()
                    .collect_frames()
                    .map_err(decode_err)?;
                animation_from_frames(frames)
            } else {
                still(path)
            }
        }
        Some(ImageFormat::WebP) => {
            let decoder = WebPDecoder::new(reader.into_inner()).map_err(decode_err)?;
            if decoder.has_animation() {
                let frames = decoder.into_frames().collect_frames().map_err(decode_err)?;
                animation_from_frames(frames)
            } else {
                still(path)
            }
        }
        other => Err(Error::Decode(format!(
            "unsupported animation format: {}",
            other.map_or_else(|| "<unknown>".to_string(), |f| format!("{f:?}"))
        ))),
    }
}

fn open(path: &Path) -> Result<ImageReader<std::io::BufReader<std::fs::File>>> {
    ImageReader::open(path)
        .map_err(decode_err)?
        .with_guessed_format()
        .map_err(decode_err)
}

/// Decode a non-animated image as a single frame.
fn still(path: &Path) -> Result<Animation> {
    let image = open(path)?.decode().map_err(decode_err)?.to_rgba8();
    Ok(Animation {
        width: image.width(),
        height: image.height(),
        frames: vec![Frame {
            rgba: image.into_raw(),
            delay_ms: 0,
        }],
    })
}

fn animation_from_frames(frames: Vec<image::Frame>) -> Result<Animation> {
    let first = frames
        .first()
        .ok_or_else(|| Error::Decode("image has no frames".into()))?;
    let (width, height) = (first.buffer().width(), first.buffer().height());

    let frames = frames
        .into_iter()
        .map(|frame| Frame {
            delay_ms: delay_ms(frame.delay()),
            rgba: frame.into_buffer().into_raw(),
        })
        .collect();

    Ok(Animation {
        width,
        height,
        frames,
    })
}

/// `Delay` is a fraction of milliseconds (`numer_denom_ms` returns `(num, den)`
/// such that the value is `num / den` ms).
fn delay_ms(delay: image::Delay) -> u32 {
    let (num, den) = delay.numer_denom_ms();
    if den == 0 {
        num
    } else {
        ((u64::from(num) + u64::from(den) / 2) / u64::from(den)) as u32
    }
}

fn decode_err<E: std::fmt::Display>(err: E) -> Error {
    Error::Decode(err.to_string())
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::path::PathBuf;

    use super::*;

    fn decode_bytes(bytes: &[u8], name: &str) -> Result<Animation> {
        let dir = tempfile::tempdir().unwrap();
        let path: PathBuf = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        decode_path(&path)
    }

    fn pixel(anim: &Animation, frame: usize, x: usize, y: usize) -> [u8; 4] {
        let off = (y * anim.width as usize + x) * 4;
        let rgba = &anim.frames[frame].rgba;
        [rgba[off], rgba[off + 1], rgba[off + 2], rgba[off + 3]]
    }

    // ---- GIF fixtures (built with the `gif` crate) ----

    const GIF_PALETTE: [u8; 12] = [0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255];

    #[allow(clippy::field_reassign_with_default)]
    fn gif_frame(
        w: u16,
        h: u16,
        left: u16,
        top: u16,
        idx: u8,
        dispose: gif::DisposalMethod,
        delay: u16,
    ) -> gif::Frame<'static> {
        let mut frame = gif::Frame::default();
        frame.width = w;
        frame.height = h;
        frame.left = left;
        frame.top = top;
        frame.delay = delay;
        frame.dispose = dispose;
        frame.buffer = Cow::Owned(vec![idx; (w as usize) * (h as usize)]);
        frame
    }

    fn encode_gif(dim: (u16, u16), frames: &[gif::Frame]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = gif::Encoder::new(&mut out, dim.0, dim.1, &GIF_PALETTE).unwrap();
            for frame in frames {
                enc.write_frame(frame).unwrap();
            }
        }
        out
    }

    #[test]
    fn gif_single_frame_is_solid_and_timed() {
        let data = encode_gif(
            (2, 2),
            &[gif_frame(2, 2, 0, 0, 1, gif::DisposalMethod::Keep, 5)],
        );
        let anim = decode_bytes(&data, "one.gif").unwrap();
        assert_eq!(anim.dimensions(), (2, 2));
        assert_eq!(anim.frame_count(), 1);
        assert_eq!(anim.frames[0].delay_ms, 50);
        for px in anim.frames[0].rgba.as_chunks::<4>().0 {
            assert_eq!(px, &[255, 0, 0, 255]);
        }
    }

    #[test]
    fn gif_background_disposal_clears_canvas() {
        let f0 = gif_frame(3, 3, 0, 0, 1, gif::DisposalMethod::Background, 10);
        let f1 = gif_frame(1, 1, 1, 1, 2, gif::DisposalMethod::Keep, 7);
        let anim = decode_bytes(&encode_gif((3, 3), &[f0, f1]), "bg.gif").unwrap();

        assert_eq!(anim.frame_count(), 2);
        assert_eq!(pixel(&anim, 0, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&anim, 1, 1, 1), [0, 255, 0, 255]);
        assert_eq!(pixel(&anim, 1, 0, 0), [0, 0, 0, 0]);
        assert_eq!(anim.frames[1].delay_ms, 70);
    }

    #[test]
    fn gif_previous_disposal_restores_previous_canvas() {
        let f0 = gif_frame(3, 3, 0, 0, 1, gif::DisposalMethod::Keep, 10);
        let f1 = gif_frame(1, 1, 0, 0, 2, gif::DisposalMethod::Previous, 10);
        let f2 = gif_frame(1, 1, 2, 2, 3, gif::DisposalMethod::Keep, 10);
        let anim = decode_bytes(&encode_gif((3, 3), &[f0, f1, f2]), "prev.gif").unwrap();

        assert_eq!(anim.frame_count(), 3);
        assert_eq!(pixel(&anim, 2, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&anim, 2, 1, 1), [255, 0, 0, 255]);
        assert_eq!(pixel(&anim, 2, 2, 2), [0, 0, 255, 255]);
    }

    #[test]
    fn gif_alpha_mask_reflects_transparency() {
        let f0 = gif_frame(2, 1, 0, 0, 1, gif::DisposalMethod::Background, 1);
        let f1 = gif_frame(1, 1, 0, 0, 2, gif::DisposalMethod::Keep, 1);
        let anim = decode_bytes(&encode_gif((2, 1), &[f0, f1]), "mask.gif").unwrap();
        assert_eq!(anim.frames[1].alpha_mask(), vec![255, 0]);
    }

    // ---- PNG / APNG fixtures (built with the `png` crate) ----

    struct ApngFrame {
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        delay: (u16, u16),
        blend: png::BlendOp,
        dispose: png::DisposeOp,
        rgba: Vec<u8>,
    }

    fn solid(w: u32, h: u32, color: [u8; 4]) -> Vec<u8> {
        color
            .iter()
            .copied()
            .cycle()
            .take((w * h * 4) as usize)
            .collect()
    }

    fn encode_png(size: (u32, u32), rgba: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, size.0, size.1);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(rgba).unwrap();
        }
        out
    }

    fn encode_apng(size: (u32, u32), frames: &[ApngFrame]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, size.0, size.1);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.set_animated(frames.len() as u32, 0).unwrap();
            let mut writer = enc.write_header().unwrap();
            for f in frames {
                writer.set_frame_dimension(f.w, f.h).unwrap();
                writer.set_frame_position(f.x, f.y).unwrap();
                writer.set_frame_delay(f.delay.0, f.delay.1).unwrap();
                writer.set_blend_op(f.blend).unwrap();
                writer.set_dispose_op(f.dispose).unwrap();
                writer.write_image_data(&f.rgba).unwrap();
            }
        }
        out
    }

    fn apng_full(w: u32, h: u32, color: [u8; 4], dispose: png::DisposeOp) -> ApngFrame {
        ApngFrame {
            x: 0,
            y: 0,
            w,
            h,
            delay: (1, 10),
            blend: png::BlendOp::Source,
            dispose,
            rgba: solid(w, h, color),
        }
    }

    #[test]
    fn static_png_is_one_frame() {
        let data = encode_png((2, 2), &solid(2, 2, [255, 0, 0, 255]));
        let anim = decode_bytes(&data, "still.png").unwrap();
        assert_eq!(anim.dimensions(), (2, 2));
        assert_eq!(anim.frame_count(), 1);
        assert_eq!(pixel(&anim, 0, 0, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn apng_two_full_frames_and_delay() {
        let frames = [
            apng_full(2, 2, [255, 0, 0, 255], png::DisposeOp::None),
            apng_full(2, 2, [0, 255, 0, 255], png::DisposeOp::None),
        ];
        let anim = decode_bytes(&encode_apng((2, 2), &frames), "two.apng").unwrap();
        assert_eq!(anim.frame_count(), 2);
        assert_eq!(anim.frames[0].delay_ms, 100);
        assert_eq!(pixel(&anim, 0, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&anim, 1, 1, 1), [0, 255, 0, 255]);
    }

    #[test]
    fn apng_over_blend_combines_with_previous() {
        let frame0 = apng_full(1, 1, [255, 0, 0, 255], png::DisposeOp::None);
        let mut frame1 = apng_full(1, 1, [0, 0, 255, 128], png::DisposeOp::None);
        frame1.blend = png::BlendOp::Over;
        let anim = decode_bytes(&encode_apng((1, 1), &[frame0, frame1]), "over.apng").unwrap();

        let px = pixel(&anim, 1, 0, 0);
        // image's fixed-point blend can land one off from 255.
        assert!(px[3] >= 254, "alpha was {}", px[3]);
        assert_eq!(px[1], 0);
        assert!((120..=136).contains(&px[0]), "red channel was {}", px[0]);
        assert!((120..=136).contains(&px[2]), "blue channel was {}", px[2]);
    }

    #[test]
    fn apng_background_dispose_clears_region() {
        let frame0 = apng_full(3, 3, [255, 0, 0, 255], png::DisposeOp::Background);
        let frame1 = ApngFrame {
            x: 1,
            y: 1,
            w: 1,
            h: 1,
            delay: (1, 10),
            blend: png::BlendOp::Source,
            dispose: png::DisposeOp::None,
            rgba: solid(1, 1, [0, 255, 0, 255]),
        };
        let anim = decode_bytes(&encode_apng((3, 3), &[frame0, frame1]), "bgd.apng").unwrap();

        assert_eq!(pixel(&anim, 1, 1, 1), [0, 255, 0, 255]);
        assert_eq!(pixel(&anim, 1, 0, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(&anim, 1, 2, 2), [0, 0, 0, 0]);
    }

    #[test]
    fn apng_previous_dispose_restores_region() {
        let frame0 = apng_full(3, 3, [255, 0, 0, 255], png::DisposeOp::None);
        let frame1 = ApngFrame {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
            delay: (1, 10),
            blend: png::BlendOp::Source,
            dispose: png::DisposeOp::Previous,
            rgba: solid(1, 1, [0, 255, 0, 255]),
        };
        let frame2 = ApngFrame {
            x: 2,
            y: 2,
            w: 1,
            h: 1,
            delay: (1, 10),
            blend: png::BlendOp::Source,
            dispose: png::DisposeOp::None,
            rgba: solid(1, 1, [0, 0, 255, 255]),
        };
        let anim = decode_bytes(
            &encode_apng((3, 3), &[frame0, frame1, frame2]),
            "prevd.apng",
        )
        .unwrap();

        assert_eq!(anim.frame_count(), 3);
        assert_eq!(pixel(&anim, 2, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&anim, 2, 2, 2), [0, 0, 255, 255]);
    }

    // ---- WebP fixtures ----

    /// 4x4, two frames (red then green), 40 ms each, no alpha.
    /// Generated with `ffmpeg -c:v libwebp_anim -lossless 1`.
    const ANIMATED_WEBP: &[u8] = &[
        82, 73, 70, 70, 132, 0, 0, 0, 87, 69, 66, 80, 86, 80, 56, 88, 10, 0, 0, 0, 2, 0, 0, 0, 3,
        0, 0, 3, 0, 0, 65, 78, 73, 77, 6, 0, 0, 0, 255, 255, 255, 255, 0, 0, 65, 78, 77, 70, 40, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 3, 0, 0, 40, 0, 0, 2, 86, 80, 56, 76, 15, 0, 0, 0, 47, 3,
        192, 0, 0, 7, 16, 253, 143, 254, 7, 34, 162, 255, 1, 0, 65, 78, 77, 70, 40, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 3, 0, 0, 3, 0, 0, 40, 0, 0, 0, 86, 80, 56, 76, 15, 0, 0, 0, 47, 3, 192, 0, 0,
        7, 208, 255, 136, 254, 7, 34, 162, 255, 1, 0,
    ];

    fn encode_webp(data: &[u8], w: u32, h: u32, color: image_webp::ColorType) -> Vec<u8> {
        let mut out = Vec::new();
        image_webp::WebPEncoder::new(&mut out)
            .encode(data, w, h, color)
            .unwrap();
        out
    }

    #[test]
    fn animated_webp_frames_and_pixels() {
        let anim = decode_bytes(ANIMATED_WEBP, "anim.webp").unwrap();
        assert_eq!(anim.dimensions(), (4, 4));
        assert_eq!(anim.frame_count(), 2);

        let red = pixel(&anim, 0, 0, 0);
        assert!(
            red[0] >= 250 && red[1] <= 8 && red[2] <= 8 && red[3] == 255,
            "red={red:?}"
        );
        let green = pixel(&anim, 1, 0, 0);
        assert!(
            green[1] >= 250 && green[0] <= 8 && green[2] <= 8 && green[3] == 255,
            "green={green:?}"
        );
        assert!(anim.frames[0].delay_ms > 0);
        assert!(anim.frames[1].delay_ms > 0);
    }

    #[test]
    fn static_opaque_webp_expands_to_rgba() {
        let rgb = [255u8, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0];
        let data = encode_webp(&rgb, 2, 2, image_webp::ColorType::Rgb8);
        let anim = decode_bytes(&data, "still.webp").unwrap();
        assert_eq!(anim.dimensions(), (2, 2));
        assert_eq!(anim.frame_count(), 1);
        assert_eq!(pixel(&anim, 0, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&anim, 0, 1, 0), [0, 255, 0, 255]);
        assert_eq!(pixel(&anim, 0, 0, 1), [0, 0, 255, 255]);
        assert_eq!(pixel(&anim, 0, 1, 1), [0, 0, 0, 255]);
    }

    #[test]
    fn static_alpha_webp_keeps_alpha() {
        let rgba = [255u8, 0, 0, 0, 0, 255, 0, 128, 0, 0, 255, 255, 0, 0, 0, 0];
        let data = encode_webp(&rgba, 2, 2, image_webp::ColorType::Rgba8);
        let anim = decode_bytes(&data, "alpha.webp").unwrap();
        assert_eq!(anim.frame_count(), 1);
        assert_eq!(pixel(&anim, 0, 0, 0)[3], 0);
        assert_eq!(pixel(&anim, 0, 1, 0)[3], 128);
        assert_eq!(pixel(&anim, 0, 0, 1)[3], 255);
        assert_eq!(pixel(&anim, 0, 1, 1)[3], 0);
    }

    #[test]
    fn rejects_unknown_content() {
        let err = decode_bytes(b"not an image at all", "junk.bin").unwrap_err();
        assert!(matches!(err, Error::Decode(_)));
    }

    #[test]
    fn alpha_hit_maps_surface_to_frame() {
        let anim = Animation {
            width: 2,
            height: 1,
            frames: vec![Frame {
                rgba: vec![255, 0, 0, 255, 0, 255, 0, 0],
                delay_ms: 0,
            }],
        };

        // surface == frame size
        assert!(anim.alpha_hit(0, (2, 1), 0.5, 0.5, 25));
        assert!(!anim.alpha_hit(0, (2, 1), 1.5, 0.5, 25));

        // surface stretched 2x: x in [2, 4) maps to frame column 1 (alpha 0)
        assert!(anim.alpha_hit(0, (4, 2), 0.5, 0.5, 25));
        assert!(!anim.alpha_hit(0, (4, 2), 3.0, 0.5, 25));

        // degenerate / invalid inputs
        assert!(!anim.alpha_hit(0, (2, 1), -0.1, 0.0, 25));
        assert!(!anim.alpha_hit(0, (2, 1), f64::NAN, 0.0, 25));
        assert!(!anim.alpha_hit(0, (0, 0), 0.0, 0.0, 25));
        assert!(!anim.alpha_hit(1, (2, 1), 0.5, 0.5, 25));

        assert_eq!(anim.alpha_at(0, 0, 0), 255);
        assert_eq!(anim.alpha_at(0, 5, 5), 0);
    }

    #[test]
    fn alpha_rects_merges_and_scales() {
        // solid 3x2 block → one rectangle
        let solid = Animation {
            width: 3,
            height: 2,
            frames: vec![Frame {
                rgba: vec![255; 3 * 2 * 4],
                delay_ms: 0,
            }],
        };
        assert_eq!(solid.alpha_rects(0, (3, 2), 25), vec![(0, 0, 3, 2)]);

        // diagonal 2x2 → two single-pixel rectangles
        let diagonal = Animation {
            width: 2,
            height: 2,
            frames: vec![Frame {
                rgba: vec![255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 255],
                delay_ms: 0,
            }],
        };
        assert_eq!(
            diagonal.alpha_rects(0, (2, 2), 25),
            vec![(0, 0, 1, 1), (1, 1, 1, 1)]
        );

        // fully transparent → no rectangles
        let empty = Animation {
            width: 2,
            height: 2,
            frames: vec![Frame {
                rgba: vec![0; 2 * 2 * 4],
                delay_ms: 0,
            }],
        };
        assert!(empty.alpha_rects(0, (2, 2), 25).is_empty());

        // surface stretched 2x
        let half = Animation {
            width: 2,
            height: 1,
            frames: vec![Frame {
                rgba: vec![255, 0, 0, 255, 0, 255, 0, 0],
                delay_ms: 0,
            }],
        };
        assert_eq!(half.alpha_rects(0, (4, 2), 25), vec![(0, 0, 2, 2)]);

        // below threshold → transparent
        let faint = Animation {
            width: 1,
            height: 1,
            frames: vec![Frame {
                rgba: vec![0, 0, 0, 10],
                delay_ms: 0,
            }],
        };
        assert!(faint.alpha_rects(0, (1, 1), 25).is_empty());
    }

    #[test]
    fn bounds_rect_wraps_all() {
        assert_eq!(bounds_rect(&[]), None);
        assert_eq!(
            bounds_rect(&[(2, 3, 1, 1), (0, 0, 2, 2)]),
            Some((0, 0, 3, 4))
        );
    }
}
