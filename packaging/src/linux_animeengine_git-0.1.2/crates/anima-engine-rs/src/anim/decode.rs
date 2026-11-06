//! Turning a file into frames: the decoders, the byte and frame budgets, and how a set that goes over them is
//! thinned rather than dropped.
//!
//! Decoding is where the sizes are decided, so this is also where the limits live: a 1350x1012 animation with
//! 448 frames needs about 2.4 GiB decoded, and past those limits frames are decimated — every other one
//! dropped, its delay folded into the survivor — until the set fits. The animation keeps its length and loses
//! steps.

use std::path::Path;

use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, ImageFormat, ImageReader};

use crate::{Error, Result};

use super::{Animation, Frame};

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
#[derive(Debug)]
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

/// The first composited frame of `path`, with the canvas size — a still, for previews.
///
/// [`decode_path`] exists to play the whole thing; this stops at one frame, so a 300-frame
/// APNG is decoded far enough to show a picture and no further. A still image comes back as
/// its own single frame. The first frame of a fade-in can be nearly empty: that is the file's
/// doing, not a decode failure.
pub fn first_frame(path: &Path) -> Result<(u32, u32, Vec<u8>)> {
    let reader = open(path)?;
    let animation = match reader.format() {
        Some(ImageFormat::Gif) => {
            let decoder = GifDecoder::new(reader.into_inner()).map_err(decode_err)?;
            collect_limited(decoder.into_frames().take(1), DecodeLimits::unbounded())?.0
        }
        Some(ImageFormat::Png) => {
            let decoder = PngDecoder::new(reader.into_inner()).map_err(decode_err)?;
            if decoder.is_apng().map_err(decode_err)? {
                let frames = decoder.apng().map_err(decode_err)?.into_frames();
                collect_limited(frames.take(1), DecodeLimits::unbounded())?.0
            } else {
                still(path)?
            }
        }
        Some(ImageFormat::WebP) => {
            let decoder = WebPDecoder::new(reader.into_inner()).map_err(decode_err)?;
            if decoder.has_animation() {
                collect_limited(decoder.into_frames().take(1), DecodeLimits::unbounded())?.0
            } else {
                still(path)?
            }
        }
        other => {
            return Err(Error::Decode(format!(
                "unsupported animation format: {}",
                other.map_or_else(|| "<unknown>".to_string(), |f| format!("{f:?}"))
            )));
        }
    };
    let Animation {
        width,
        height,
        frames,
    } = animation;
    let frame = frames
        .into_iter()
        .next()
        .ok_or_else(|| Error::Decode("the file decoded to no frames".to_string()))?;
    Ok((width, height, frame.rgba))
}

/// Decode a non-animated image as a single frame.
fn still(path: &Path) -> Result<Animation> {
    let image = open(path)?.decode().map_err(decode_err)?.to_rgba8();
    Ok(Animation {
        width: image.width(),
        height: image.height(),
        frames: vec![Frame {
            alpha_bits: Vec::new(),
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
        alpha_bits: Vec::new(),
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

/// A decoder that turns a file into a composited [`Animation`].
pub trait FrameSource {
    fn decode(&self, path: &Path) -> Result<Animation>;
}
