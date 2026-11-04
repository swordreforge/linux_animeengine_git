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

/// Delay used when a file stores none (0 ms): browsers clamp these so a GIF with
/// zero delays does not spin at maximum speed.
pub const DEFAULT_FRAME_MS: u32 = 33;

impl Frame {
    /// Delay to actually wait on this frame (0 ms becomes [`DEFAULT_FRAME_MS`]).
    pub fn effective_delay_ms(&self) -> u32 {
        if self.delay_ms == 0 {
            DEFAULT_FRAME_MS
        } else {
            self.delay_ms
        }
    }

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

/// How a frame is oriented on screen: quarter-turn rotation plus mirrors.
///
/// The displayed image is the native image rotated clockwise by
/// `quarter_turns * 90°`, then mirrored in display space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Orientation {
    /// Clockwise quarter turns (0..=3).
    pub quarter_turns: u8,
    pub flip_h: bool,
    pub flip_v: bool,
}

impl Orientation {
    /// Build from a rotation in degrees (rounded to the nearest quarter turn)
    /// plus the mirror flags.
    pub fn from_degrees(rotation_degrees: i32, flip_h: bool, flip_v: bool) -> Self {
        let normalized = rotation_degrees.rem_euclid(360);
        let quarter_turns = ((normalized + 45) / 90).rem_euclid(4) as u8;
        Self {
            quarter_turns,
            flip_h,
            flip_v,
        }
    }

    /// Whether the displayed size swaps width and height.
    pub fn swaps_axes(&self) -> bool {
        self.quarter_turns % 2 == 1
    }

    /// Displayed size for a native size.
    pub fn size(&self, native: (u32, u32)) -> (u32, u32) {
        if self.swaps_axes() {
            (native.1, native.0)
        } else {
            native
        }
    }

    /// Rotation in degrees (0/90/180/270).
    pub fn degrees(&self) -> i32 {
        i32::from(self.quarter_turns) * 90
    }

    /// Native unit coordinates (origin top-left) -> display unit coordinates.
    pub fn native_to_display(&self, u: f64, v: f64) -> (f64, f64) {
        let (mut x, mut y) = match self.quarter_turns % 4 {
            0 => (u, v),
            1 => (1.0 - v, u),
            2 => (1.0 - u, 1.0 - v),
            _ => (v, 1.0 - u),
        };
        if self.flip_h {
            x = 1.0 - x;
        }
        if self.flip_v {
            y = 1.0 - y;
        }
        (x, y)
    }

    /// Display unit coordinates -> native unit coordinates (inverse of
    /// [`Self::native_to_display`]).
    pub fn display_to_native(&self, x: f64, y: f64) -> (f64, f64) {
        let (mut x, mut y) = (x, y);
        if self.flip_h {
            x = 1.0 - x;
        }
        if self.flip_v {
            y = 1.0 - y;
        }
        match self.quarter_turns % 4 {
            0 => (x, y),
            1 => (y, 1.0 - x),
            2 => (1.0 - x, 1.0 - y),
            _ => (1.0 - y, x),
        }
    }
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
    /// stretched to `surface` and oriented by `orientation`. Used for
    /// drag-on-solid-pixel.
    pub fn alpha_hit(
        &self,
        frame: usize,
        surface: (u32, u32),
        orientation: Orientation,
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
        let (nu, nv) = orientation.display_to_native(sx / f64::from(sw), sy / f64::from(sh));
        self.alpha_at_unit(frame, nu, nv, threshold)
    }

    /// Opaque test at native unit coordinates (0..1, origin top-left).
    fn alpha_at_unit(&self, frame: usize, u: f64, v: f64, threshold: u8) -> bool {
        let fx = (u * f64::from(self.width)).floor();
        let fy = (v * f64::from(self.height)).floor();
        if fx < 0.0 || fy < 0.0 || fx >= f64::from(self.width) || fy >= f64::from(self.height) {
            return false;
        }
        self.alpha_at(frame, fx as u32, fy as u32) >= threshold
    }

    /// Rectangles `(x, y, w, h)` in surface-local coordinates covering the
    /// opaque pixels of `frame`. The scan runs in **surface space** (each cell is
    /// mapped back to a frame pixel), so its cost scales with the displayed size
    /// rather than the native frame, and rows with identical runs are merged.
    pub fn alpha_rects(
        &self,
        frame: usize,
        surface: (u32, u32),
        orientation: Orientation,
        threshold: u8,
    ) -> Vec<(i32, i32, i32, i32)> {
        self.alpha_rects_step(frame, surface, orientation, threshold, 1)
    }

    /// Like [`Self::alpha_rects`] but samples `step`-pixel surface cells and
    /// emits `step`-sized rectangles, to coarsen very detailed silhouettes.
    pub fn alpha_rects_step(
        &self,
        frame: usize,
        surface: (u32, u32),
        orientation: Orientation,
        threshold: u8,
        step: u32,
    ) -> Vec<(i32, i32, i32, i32)> {
        let (sw, sh) = surface;
        let step = step.clamp(1, 32);
        if sw == 0 || sh == 0 || self.width == 0 || self.height == 0 {
            return Vec::new();
        }
        if self.frames.get(frame).is_none() {
            return Vec::new();
        }

        let cols = sw.div_ceil(step);
        let rows = sh.div_ceil(step);
        let surface_size = (sw, sh);

        let mut rects: Vec<(i32, i32, i32, i32)> = Vec::new();
        let mut band: Vec<(u32, u32)> = Vec::new();
        let mut band_start = 0u32;
        let mut band_height = 0u32;

        for gy in 0..rows {
            let v = (f64::from((gy * step + step / 2).min(sh - 1)) + 0.5) / f64::from(sh);
            let mut runs: Vec<(u32, u32)> = Vec::new();
            let mut start: Option<u32> = None;
            for gx in 0..cols {
                let u = (f64::from((gx * step + step / 2).min(sw - 1)) + 0.5) / f64::from(sw);
                let (nu, nv) = orientation.display_to_native(u, v);
                let solid = self.alpha_at_unit(frame, nu, nv, threshold);
                match (solid, start) {
                    (true, None) => start = Some(gx),
                    (false, Some(from)) => {
                        runs.push((from, gx));
                        start = None;
                    }
                    _ => {}
                }
            }
            if let Some(from) = start {
                runs.push((from, cols));
            }

            if gy == 0 {
                band = runs;
                band_start = 0;
                band_height = 1;
            } else if runs == band {
                band_height += 1;
            } else {
                emit_surface_band(
                    &band,
                    band_start,
                    band_height,
                    step,
                    surface_size,
                    &mut rects,
                );
                band = runs;
                band_start = gy;
                band_height = 1;
            }
        }
        emit_surface_band(
            &band,
            band_start,
            band_height,
            step,
            surface_size,
            &mut rects,
        );
        rects
    }
}

/// Append the rectangles of one merged surface-space row band.
fn emit_surface_band(
    runs: &[(u32, u32)],
    gy: u32,
    gh: u32,
    step: u32,
    surface: (u32, u32),
    rects: &mut Vec<(i32, i32, i32, i32)>,
) {
    let (sw, sh) = surface;
    for &(x0, x1) in runs {
        let left = x0 * step;
        let top = gy * step;
        rects.push((
            left as i32,
            top as i32,
            ((x1 * step).min(sw).saturating_sub(left)) as i32,
            (((gy + gh) * step).min(sh).saturating_sub(top)) as i32,
        ));
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

impl Animation {
    /// Bytes of one composited frame at the canvas size.
    pub fn frame_bytes(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height) * 4
    }

    /// Rough memory footprint of the decoded frames.
    pub fn bytes(&self) -> u64 {
        self.frame_bytes() * self.frames.len() as u64
    }
}

/// Caps on how much of an animation is held in RAM at once.
///
/// A 1350x1012 animation with 448 frames needs ~2.4 GiB decoded. Past these
/// limits frames are decimated — every other one dropped, its delay folded into
/// the survivor — until the set fits, so a huge Workshop item cannot exhaust
/// memory. The animation plays the same length, just with fewer steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeLimits {
    pub max_bytes: u64,
    pub max_frames: usize,
}

/// Overrides [`DecodeLimits::max_bytes`] in MiB (`ANIMA_MAX_DECODE_MB`).
pub const MAX_DECODE_MB_ENV: &str = "ANIMA_MAX_DECODE_MB";
/// Overrides [`DecodeLimits::max_frames`] (`ANIMA_MAX_FRAMES`).
pub const MAX_FRAMES_ENV: &str = "ANIMA_MAX_FRAMES";

impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_bytes: 1024 * 1024 * 1024,
            max_frames: 4096,
        }
    }
}

impl DecodeLimits {
    /// No limits at all (tests that want every frame).
    pub const fn unbounded() -> Self {
        Self {
            max_bytes: u64::MAX,
            max_frames: usize::MAX,
        }
    }

    /// The defaults with `ANIMA_MAX_DECODE_MB` / `ANIMA_MAX_FRAMES` applied.
    pub fn from_env() -> Self {
        let mut limits = Self::default();
        if let Some(mb) = env_u64(MAX_DECODE_MB_ENV) {
            limits.max_bytes = mb.saturating_mul(1024 * 1024);
        }
        if let Some(frames) = env_u64(MAX_FRAMES_ENV) {
            limits.max_frames = frames.min(usize::MAX as u64) as usize;
        }
        limits
    }
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.trim().parse().ok()
}

/// What a bounded decode did: frames kept out of the source and the grid used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeReport {
    pub source_frames: usize,
    pub kept_frames: usize,
    pub bytes: u64,
    pub stride: usize,
}

impl DecodeReport {
    fn whole(animation: &Animation) -> Self {
        Self {
            source_frames: animation.frames.len(),
            kept_frames: animation.frames.len(),
            bytes: animation.bytes(),
            stride: 1,
        }
    }

    /// Whether frames were dropped to satisfy the limits.
    pub fn limited(&self) -> bool {
        self.kept_frames < self.source_frames || self.stride > 1
    }

    /// Log suffix describing any decimation (empty when nothing was dropped).
    pub fn note(&self) -> String {
        if self.limited() {
            format!(
                " (1 of every {} frames: {} of {} kept to fit the memory cap)",
                self.stride, self.kept_frames, self.source_frames
            )
        } else {
            String::new()
        }
    }
}

/// Decimate `frames` until they fit `limits`, folding dropped delays into their
/// neighbours so the total playing time is preserved.
pub fn limit_frames(frames: Vec<Frame>, limits: DecodeLimits) -> (Vec<Frame>, DecodeReport) {
    let frame_bytes = frames.first().map_or(0, |frame| frame.rgba.len()) as u64;
    let mut limiter = FrameLimiter::new(limits, frame_bytes);
    for frame in frames {
        limiter.push(frame);
    }
    limiter.finish()
}

/// [`limit_frames`] for a whole animation.
pub fn limit_animation(animation: Animation, limits: DecodeLimits) -> (Animation, DecodeReport) {
    let Animation {
        width,
        height,
        frames,
    } = animation;
    let (frames, report) = limit_frames(frames, limits);
    (
        Animation {
            width,
            height,
            frames,
        },
        report,
    )
}

/// Streaming half of [`limit_frames`]: frames arrive one at a time and the
/// retained set shrinks as soon as it exceeds the budget — each shrink halves it
/// and doubles the sampling stride, so memory stays bounded while decoding.
struct FrameLimiter {
    limits: DecodeLimits,
    frame_bytes: u64,
    frames: Vec<Frame>,
    pending_ms: u32,
    stride: usize,
    index: usize,
}

impl FrameLimiter {
    fn new(limits: DecodeLimits, frame_bytes: u64) -> Self {
        Self {
            limits,
            frame_bytes,
            frames: Vec::new(),
            pending_ms: 0,
            stride: 1,
            index: 0,
        }
    }

    fn push(&mut self, mut frame: Frame) {
        if self.stride > 1 && !self.index.is_multiple_of(self.stride) {
            self.pending_ms = self.pending_ms.saturating_add(frame.delay_ms);
            self.index += 1;
            return;
        }
        frame.delay_ms = frame.delay_ms.saturating_add(self.pending_ms);
        self.pending_ms = 0;
        self.frames.push(frame);
        self.index += 1;
        self.shrink();
    }

    fn shrink(&mut self) {
        while self.over_budget() && self.frames.len() > 1 {
            self.decimate();
        }
    }

    fn over_budget(&self) -> bool {
        self.frames.len() > self.limits.max_frames
            || (self.frames.len() as u64).saturating_mul(self.frame_bytes) > self.limits.max_bytes
    }

    /// Drop every other kept frame, adding its delay to the survivor.
    fn decimate(&mut self) {
        let frames = std::mem::take(&mut self.frames);
        let last = frames.len().saturating_sub(1);
        let mut kept = Vec::with_capacity(frames.len().div_ceil(2));
        let mut carry = 0u32;
        for (position, mut frame) in frames.into_iter().enumerate() {
            frame.delay_ms = frame.delay_ms.saturating_add(carry);
            carry = 0;
            if position % 2 == 0 {
                kept.push(frame);
            } else {
                carry = frame.delay_ms;
            }
            if position == last
                && carry > 0
                && let Some(previous) = kept.last_mut()
            {
                previous.delay_ms = previous.delay_ms.saturating_add(carry);
                carry = 0;
            }
        }
        if carry > 0
            && let Some(previous) = kept.last_mut()
        {
            previous.delay_ms = previous.delay_ms.saturating_add(carry);
        }
        self.frames = kept;
        self.stride *= 2;
    }

    fn finish(mut self) -> (Vec<Frame>, DecodeReport) {
        // The last source frames may have been skipped; their time still belongs
        // to the animation, so fold it into the final kept frame.
        if self.pending_ms > 0
            && let Some(last) = self.frames.last_mut()
        {
            last.delay_ms = last.delay_ms.saturating_add(self.pending_ms);
            self.pending_ms = 0;
        }
        let report = DecodeReport {
            source_frames: self.index,
            kept_frames: self.frames.len(),
            bytes: (self.frames.len() as u64).saturating_mul(self.frame_bytes),
            stride: self.stride,
        };
        (self.frames, report)
    }
}

/// Decoder backed by the `image` crate.
pub struct ImageSource;

impl FrameSource for ImageSource {
    fn decode(&self, path: &Path) -> Result<Animation> {
        decode_path(path)
    }
}

/// Decode any supported animation file into composited frames (bounded by
/// [`DecodeLimits::default`]).
pub fn decode(path: &Path) -> Result<Animation> {
    ImageSource.decode(path)
}

/// Decode `path` using the `image` crate, dispatching on the detected format.
pub fn decode_path(path: &Path) -> Result<Animation> {
    decode_with_limits(path, DecodeLimits::default())
}

/// [`decode_path`] with explicit memory limits.
pub fn decode_with_limits(path: &Path, limits: DecodeLimits) -> Result<Animation> {
    decode_reporting(path, limits).map(|(animation, _)| animation)
}

/// Decode `path` and report how much of it was decimated to fit `limits`.
pub fn decode_reporting(path: &Path, limits: DecodeLimits) -> Result<(Animation, DecodeReport)> {
    let reader = open(path)?;
    match reader.format() {
        Some(ImageFormat::Gif) => {
            let decoder = GifDecoder::new(reader.into_inner()).map_err(decode_err)?;
            collect_limited(decoder.into_frames(), limits)
        }
        Some(ImageFormat::Png) => {
            let decoder = PngDecoder::new(reader.into_inner()).map_err(decode_err)?;
            if decoder.is_apng().map_err(decode_err)? {
                collect_limited(decoder.apng().map_err(decode_err)?.into_frames(), limits)
            } else {
                still(path).map(single)
            }
        }
        Some(ImageFormat::WebP) => {
            let decoder = WebPDecoder::new(reader.into_inner()).map_err(decode_err)?;
            if decoder.has_animation() {
                collect_limited(decoder.into_frames(), limits)
            } else {
                still(path).map(single)
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

/// Decode frames one at a time, decimating as soon as `limits` are exceeded.
fn collect_limited<I>(frames: I, limits: DecodeLimits) -> Result<(Animation, DecodeReport)>
where
    I: Iterator<Item = std::result::Result<image::Frame, image::ImageError>>,
{
    let mut frames = frames;
    let first = frames
        .next()
        .ok_or_else(|| Error::Decode("image has no frames".into()))?
        .map_err(decode_err)?;
    let (width, height) = (first.buffer().width(), first.buffer().height());
    let mut limiter = FrameLimiter::new(limits, u64::from(width) * u64::from(height) * 4);
    limiter.push(convert_frame(first));
    for frame in frames {
        limiter.push(convert_frame(frame.map_err(decode_err)?));
    }
    let (frames, report) = limiter.finish();
    Ok((
        Animation {
            width,
            height,
            frames,
        },
        report,
    ))
}

fn convert_frame(frame: image::Frame) -> Frame {
    Frame {
        delay_ms: delay_ms(frame.delay()),
        rgba: frame.into_buffer().into_raw(),
    }
}

/// Report for a single-frame decode (nothing can be dropped).
fn single(animation: Animation) -> (Animation, DecodeReport) {
    let report = DecodeReport::whole(&animation);
    (animation, report)
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

    fn plain_frame(delay_ms: u32) -> Frame {
        Frame {
            rgba: vec![0, 0, 0, 255],
            delay_ms,
        }
    }

    #[test]
    fn zero_delay_frames_fall_back_to_a_visible_delay() {
        assert_eq!(plain_frame(0).effective_delay_ms(), DEFAULT_FRAME_MS);
        assert_eq!(plain_frame(50).effective_delay_ms(), 50);
    }

    #[test]
    fn limits_keep_small_animations_intact() {
        let frames: Vec<Frame> = (0..4).map(|_| plain_frame(50)).collect();
        let (kept, report) = limit_frames(frames, DecodeLimits::unbounded());
        assert_eq!(kept.len(), 4);
        assert_eq!(report.stride, 1);
        assert!(!report.limited());
        assert_eq!(kept.iter().map(|f| f.delay_ms).sum::<u32>(), 200);
    }

    #[test]
    fn limits_decimate_and_preserve_total_delay() {
        let frames: Vec<Frame> = (0..8).map(|_| plain_frame(30)).collect();
        // 4 bytes per frame: a 16-byte budget keeps 4 of the 8.
        let limits = DecodeLimits {
            max_bytes: 16,
            max_frames: usize::MAX,
        };
        let (kept, report) = limit_frames(frames, limits);
        assert_eq!(kept.len(), 4);
        assert_eq!(report.stride, 2);
        assert_eq!(report.source_frames, 8);
        assert!(report.limited());
        assert!(report.bytes <= 16);
        assert_eq!(kept.iter().map(|f| f.delay_ms).sum::<u32>(), 240);
        assert!(report.note().contains("1 of every 2 frames"));
    }

    #[test]
    fn limits_respect_the_frame_cap() {
        let frames: Vec<Frame> = (0..8).map(|_| plain_frame(10)).collect();
        let limits = DecodeLimits {
            max_bytes: u64::MAX,
            max_frames: 3,
        };
        let (kept, report) = limit_frames(frames, limits);
        assert!(kept.len() <= 3);
        assert_eq!(kept.iter().map(|f| f.delay_ms).sum::<u32>(), 80);
        assert!(report.limited());
    }

    #[test]
    fn decoding_a_gif_respects_the_byte_limit() {
        let frames: Vec<gif::Frame<'static>> = (0..6)
            .map(|i| {
                gif_frame(
                    2,
                    2,
                    0,
                    0,
                    if i % 2 == 0 { 1 } else { 2 },
                    gif::DisposalMethod::Keep,
                    5,
                )
            })
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("six.gif");
        std::fs::write(&path, encode_gif((2, 2), &frames)).unwrap();

        let (animation, report) = decode_reporting(&path, DecodeLimits::unbounded()).unwrap();
        assert_eq!(animation.frame_count(), 6);
        assert!(!report.limited());
        assert_eq!(animation.frames[0].delay_ms, 50);

        // 2x2 RGBA = 16 bytes per frame; this budget holds three of them.
        let limits = DecodeLimits {
            max_bytes: 48,
            max_frames: usize::MAX,
        };
        let (animation, report) = decode_reporting(&path, limits).unwrap();
        assert_eq!(animation.frame_count(), 3);
        assert_eq!(report.stride, 2);
        assert_eq!(
            animation.frames.iter().map(|f| f.delay_ms).sum::<u32>(),
            300
        );
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

        let id = Orientation::default();

        // surface == frame size
        assert!(anim.alpha_hit(0, (2, 1), id, 0.5, 0.5, 25));
        assert!(!anim.alpha_hit(0, (2, 1), id, 1.5, 0.5, 25));

        // surface stretched 2x: x in [2, 4) maps to frame column 1 (alpha 0)
        assert!(anim.alpha_hit(0, (4, 2), id, 0.5, 0.5, 25));
        assert!(!anim.alpha_hit(0, (4, 2), id, 3.0, 0.5, 25));

        // degenerate / invalid inputs
        assert!(!anim.alpha_hit(0, (2, 1), id, -0.1, 0.0, 25));
        assert!(!anim.alpha_hit(0, (2, 1), id, f64::NAN, 0.0, 25));
        assert!(!anim.alpha_hit(0, (0, 0), id, 0.0, 0.0, 25));
        assert!(!anim.alpha_hit(1, (2, 1), id, 0.5, 0.5, 25));

        assert_eq!(anim.alpha_at(0, 0, 0), 255);
        assert_eq!(anim.alpha_at(0, 5, 5), 0);
    }

    #[test]
    fn alpha_rects_merges_and_scales() {
        let id = Orientation::default();

        // solid 3x2 block → one rectangle
        let solid = Animation {
            width: 3,
            height: 2,
            frames: vec![Frame {
                rgba: vec![255; 3 * 2 * 4],
                delay_ms: 0,
            }],
        };
        assert_eq!(solid.alpha_rects(0, (3, 2), id, 25), vec![(0, 0, 3, 2)]);

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
            diagonal.alpha_rects(0, (2, 2), id, 25),
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
        assert!(empty.alpha_rects(0, (2, 2), id, 25).is_empty());

        // surface stretched 2x
        let half = Animation {
            width: 2,
            height: 1,
            frames: vec![Frame {
                rgba: vec![255, 0, 0, 255, 0, 255, 0, 0],
                delay_ms: 0,
            }],
        };
        assert_eq!(half.alpha_rects(0, (4, 2), id, 25), vec![(0, 0, 2, 2)]);

        // below threshold → transparent
        let faint = Animation {
            width: 1,
            height: 1,
            frames: vec![Frame {
                rgba: vec![0, 0, 0, 10],
                delay_ms: 0,
            }],
        };
        assert!(faint.alpha_rects(0, (1, 1), id, 25).is_empty());
    }

    #[test]
    fn bounds_rect_wraps_all() {
        assert_eq!(bounds_rect(&[]), None);
        assert_eq!(
            bounds_rect(&[(2, 3, 1, 1), (0, 0, 2, 2)]),
            Some((0, 0, 3, 4))
        );
    }

    #[test]
    fn orientation_rotation_and_mirrors() {
        let rot90 = Orientation::from_degrees(90, false, false);
        assert!(rot90.swaps_axes());
        assert_eq!(rot90.size((2, 3)), (3, 2));
        assert_eq!(
            Orientation::from_degrees(-90, false, false).quarter_turns,
            3
        );
        assert_eq!(
            Orientation::from_degrees(450, false, false).quarter_turns,
            1
        );
        assert_eq!(Orientation::default().size((2, 3)), (2, 3));

        // display_to_native is the inverse of native_to_display
        for (u, v) in [(0.25, 0.75), (0.0, 0.0), (1.0, 1.0), (0.5, 0.5)] {
            let (x, y) = rot90.native_to_display(u, v);
            let (u2, v2) = rot90.display_to_native(x, y);
            assert!((u - u2).abs() < 1e-9 && (v - v2).abs() < 1e-9);
        }

        // mirror only
        let mirrored = Orientation::from_degrees(0, true, false);
        assert!((mirrored.native_to_display(0.25, 0.5).0 - 0.75).abs() < 1e-9);

        // 2x1 frame, only pixel (0,0) opaque
        let anim = Animation {
            width: 2,
            height: 1,
            frames: vec![Frame {
                rgba: vec![255, 0, 0, 255, 0, 0, 0, 0],
                delay_ms: 0,
            }],
        };

        // 90° CW displays as 1x2 with the opaque pixel at the top
        assert!(anim.alpha_hit(0, (1, 2), rot90, 0.5, 0.5, 25));
        assert!(!anim.alpha_hit(0, (1, 2), rot90, 0.5, 1.5, 25));
        assert_eq!(anim.alpha_rects(0, (1, 2), rot90, 25), vec![(0, 0, 1, 1)]);

        // a horizontal mirror moves that pixel to the right column
        assert_eq!(
            anim.alpha_rects(0, (2, 1), Orientation::from_degrees(0, true, false), 25),
            vec![(1, 0, 1, 1)]
        );
    }
}
