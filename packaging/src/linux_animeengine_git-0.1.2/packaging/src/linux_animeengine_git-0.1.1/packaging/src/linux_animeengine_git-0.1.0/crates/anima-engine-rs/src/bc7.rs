//! BC7 block compression (quality first) through the vendored `bc7enc_rdo`.
//!
//! BC7 is the highest-quality desktop GPU block format: 8 bits per pixel instead
//! of 32, near-lossless for photographic and antialiased content, with alpha
//! preserved because the encoder picks per-block modes (including the 8-bit alpha
//! modes 5/6/7). `bc7enc_rdo` is MIT / public domain (`vendor/bc7enc_rdo/LICENSE`).
//!
//! Frames are compressed to `ceil(w/4) * ceil(h/4)` 16-byte blocks, which is
//! exactly what a `wgpu` `Bc7RgbaUnorm` texture (with `TEXTURE_COMPRESSION_BC`)
//! expects; the CPU-side originals stay around for alpha-accurate hit-testing.

use std::sync::OnceLock;

use crate::anim::{Animation, Frame};
use crate::{Error, Result};

/// Encoder settings. `uber_level` runs 0..=4: higher is slower and better.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bc7Params {
    pub uber_level: u32,
    /// Weigh error perceptually (YCbCr) instead of per RGB channel.
    pub perceptual: bool,
}

impl Default for Bc7Params {
    fn default() -> Self {
        // Quality first: the slowest/best mode, linear weights so alpha edges and
        // flat colours are not penalised by luminance weighting.
        Self {
            uber_level: 4,
            perceptual: false,
        }
    }
}

/// Bytes a BC7 image of this size occupies (16 per 4x4 block).
pub fn encoded_size(width: u32, height: u32) -> u64 {
    u64::from(width.div_ceil(4)) * u64::from(height.div_ceil(4)) * 16
}

unsafe extern "C" {
    fn anima_bc7_init();
    fn anima_bc7_encode(
        pixels: *const u8,
        width: u32,
        height: u32,
        uber_level: u32,
        perceptual: i32,
        out: *mut u8,
        out_size: u64,
    ) -> u64;
    fn anima_bc7_decode(blocks: *const u8, width: u32, height: u32, out: *mut u8);
}

fn init() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| unsafe { anima_bc7_init() });
}

/// Compress one straight-RGBA8 image into BC7 blocks.
pub fn encode(rgba: &[u8], width: u32, height: u32, params: Bc7Params) -> Result<Vec<u8>> {
    if width == 0 || height == 0 {
        return Err(Error::Decode("empty image".into()));
    }
    let expected = width as usize * height as usize * 4;
    if rgba.len() != expected {
        return Err(Error::Decode(format!(
            "rgba buffer is {} bytes, expected {expected}",
            rgba.len()
        )));
    }

    init();
    let mut out = vec![0u8; encoded_size(width, height) as usize];
    let written = unsafe {
        anima_bc7_encode(
            rgba.as_ptr(),
            width,
            height,
            params.uber_level,
            i32::from(params.perceptual),
            out.as_mut_ptr(),
            out.len() as u64,
        )
    };
    if written as usize != out.len() {
        return Err(Error::Decode("BC7 encoder rejected the image".into()));
    }
    Ok(out)
}

/// Decode BC7 blocks back to straight RGBA8 (tests and quality measurement).
pub fn decode(blocks: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    if blocks.len() != encoded_size(width, height) as usize {
        return Err(Error::Decode(format!(
            "{} bytes of blocks do not match {width}x{height}",
            blocks.len()
        )));
    }
    init();
    let mut out = vec![0u8; width as usize * height as usize * 4];
    unsafe { anima_bc7_decode(blocks.as_ptr(), width, height, out.as_mut_ptr()) };
    Ok(out)
}

/// Rebuild an animation from cached BC7 blocks.
///
/// RGBA is still needed in memory for hit-testing and input regions, so it is
/// decoded back out of the blocks — which also makes the CPU alpha agree with
/// what the GPU actually displays.
pub fn animation_from_blocks(
    width: u32,
    height: u32,
    delays: &[u32],
    data: &[u8],
) -> Result<Animation> {
    let frame_bytes = encoded_size(width, height) as usize;
    if delays.is_empty() || data.len() != frame_bytes * delays.len() {
        return Err(Error::Decode(format!(
            "{} bytes of BC7 blocks do not match {} frame(s) of {width}x{height}",
            data.len(),
            delays.len()
        )));
    }

    let mut frames = Vec::with_capacity(delays.len());
    for (index, &delay_ms) in delays.iter().enumerate() {
        let start = index * frame_bytes;
        let rgba = decode(&data[start..start + frame_bytes], width, height)?;
        frames.push(Frame { rgba, delay_ms });
    }
    Ok(Animation {
        width,
        height,
        frames,
    })
}

/// Mean squared error between two 8-bit buffers.
pub fn mse(a: &[u8], b: &[u8]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return f64::INFINITY;
    }
    let total: f64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| {
            let delta = f64::from(x) - f64::from(y);
            delta * delta
        })
        .sum();
    total / a.len() as f64
}

/// Peak signal-to-noise ratio in dB (`inf` for identical buffers).
pub fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = mse(a, b);
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hard-edged disc with an antialiased rim: the case that matters for an
    /// overlay (silhouette alpha plus flat colour).
    fn disc(size: u32) -> Vec<u8> {
        let mut rgba = vec![0u8; size as usize * size as usize * 4];
        let centre = size as f64 / 2.0;
        let radius = size as f64 * 0.4;
        for y in 0..size {
            for x in 0..size {
                let dx = f64::from(x) + 0.5 - centre;
                let dy = f64::from(y) + 0.5 - centre;
                let distance = (dx * dx + dy * dy).sqrt();
                // 2 px soft rim, like a real antialiased sprite edge.
                let alpha = ((radius - distance) / 2.0).clamp(0.0, 1.0);
                let index = ((y * size + x) * 4) as usize;
                rgba[index] = 200;
                rgba[index + 1] = 60;
                rgba[index + 2] = 90;
                rgba[index + 3] = (alpha * 255.0).round() as u8;
            }
        }
        rgba
    }

    #[test]
    fn encoded_size_is_16_bytes_per_block() {
        assert_eq!(encoded_size(4, 4), 16);
        assert_eq!(encoded_size(5, 5), 64);
        assert_eq!(encoded_size(1500, 1125), 375 * 282 * 16);
    }

    #[test]
    fn roundtrip_keeps_colour_and_alpha_high_quality() {
        let (width, height) = (64u32, 64u32);
        let source = disc(width);
        let blocks = encode(&source, width, height, Bc7Params::default()).unwrap();
        assert_eq!(blocks.len() as u64, encoded_size(width, height));

        let decoded = decode(&blocks, width, height).unwrap();
        let overall = psnr(&source, &decoded);
        assert!(overall > 30.0, "overall PSNR {overall:.2} dB is too low");

        // Alpha carries the silhouette, so check it separately.
        let source_alpha: Vec<u8> = source.as_chunks::<4>().0.iter().map(|px| px[3]).collect();
        let decoded_alpha: Vec<u8> = decoded.as_chunks::<4>().0.iter().map(|px| px[3]).collect();
        let alpha_psnr = psnr(&source_alpha, &decoded_alpha);
        assert!(
            alpha_psnr > 30.0,
            "alpha PSNR {alpha_psnr:.2} dB is too low"
        );

        // Fully opaque and fully transparent areas must stay close.
        let max_alpha_error = source_alpha
            .iter()
            .zip(&decoded_alpha)
            .map(|(&a, &b)| a.abs_diff(b))
            .max()
            .unwrap();
        assert!(max_alpha_error <= 64, "alpha error {max_alpha_error}");
    }

    #[test]
    fn solid_colour_stays_exact() {
        let (width, height) = (16u32, 16u32);
        let source = [10u8, 120, 200, 255].repeat((width * height) as usize);
        let blocks = encode(&source, width, height, Bc7Params::default()).unwrap();
        let decoded = decode(&blocks, width, height).unwrap();
        assert!(psnr(&source, &decoded) > 45.0);
    }

    #[test]
    fn blocks_rebuild_an_animation() {
        let (width, height) = (32u32, 32u32);
        let source = disc(width);
        let blocks = encode(&source, width, height, Bc7Params::default()).unwrap();

        let animation = animation_from_blocks(width, height, &[40, 60], &blocks.repeat(2)).unwrap();
        assert_eq!(animation.frame_count(), 2);
        assert_eq!(animation.frames[0].delay_ms, 40);
        assert_eq!(animation.frames[1].delay_ms, 60);
        assert!(psnr(&source, &animation.frames[0].rgba) > 30.0);

        assert!(animation_from_blocks(width, height, &[], &blocks).is_err());
        assert!(animation_from_blocks(width, height, &[40], &blocks[..blocks.len() - 1]).is_err());
    }

    #[test]
    fn rejects_mismatched_buffers() {
        // A 4x4 image is 64 bytes of RGBA; 4x4 blocks are 16 bytes of BC7.
        let one_block = [10u8, 120, 200, 255].repeat(16);
        assert!(encode(&one_block, 4, 4, Bc7Params::default()).is_ok());
        assert!(encode(&[0u8; 16], 4, 4, Bc7Params::default()).is_err());
        assert!(decode(&[0u8; 15], 4, 4).is_err());
    }
}
