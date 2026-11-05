//! Measure BC7 quality and size on a real animation (quality first: uber 4).
//!
//! ```text
//! cargo run --release --features bc7 --example bc7_bench -- <animation>
//! ```

use std::time::Instant;

use anima_engine_rs::anim::{DecodeLimits, decode_reporting};
use anima_engine_rs::bc7::{self, Bc7Params};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let argument = std::env::args()
        .nth(1)
        .ok_or("usage: bc7_bench <animation>")?;
    let path = std::path::PathBuf::from(&argument);
    let (animation, report) = decode_reporting(&path, DecodeLimits::from_env())?;
    println!(
        "{}: {} frame(s) {}x{}, {:.1} MiB raw{}",
        argument,
        animation.frame_count(),
        animation.width,
        animation.height,
        animation.bytes() as f64 / (1024.0 * 1024.0),
        report.note()
    );

    let params = Bc7Params::default();
    println!(
        "bc7: uber level {}, {} weights",
        params.uber_level,
        if params.perceptual {
            "perceptual"
        } else {
            "linear"
        }
    );

    let started = Instant::now();
    let mut blocks_total = 0usize;
    let mut colour_mse = 0.0f64;
    let mut alpha_mse = 0.0f64;
    let mut samples = 0u64;
    let mut alpha_max_error = 0u8;
    for frame in &animation.frames {
        let blocks = bc7::encode(&frame.rgba, animation.width, animation.height, params)?;
        blocks_total += blocks.len();
        let decoded = bc7::decode(&blocks, animation.width, animation.height)?;
        for (source, result) in frame
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .zip(decoded.as_chunks::<4>().0.iter())
        {
            for channel in 0..3 {
                let delta = f64::from(source[channel]) - f64::from(result[channel]);
                colour_mse += delta * delta;
            }
            let delta = f64::from(source[3]) - f64::from(result[3]);
            alpha_mse += delta * delta;
            alpha_max_error = alpha_max_error.max(source[3].abs_diff(result[3]));
            samples += 1;
        }
    }
    let elapsed = started.elapsed().as_secs_f32();

    let raw = animation.bytes();
    let blocks = blocks_total as f64;
    let colour_psnr = 10.0 * (255.0f64 * 255.0 / (colour_mse / (samples * 3) as f64)).log10();
    let alpha_psnr = 10.0 * (255.0f64 * 255.0 / (alpha_mse / samples as f64)).log10();
    println!(
        "encode+decode {elapsed:.2}s  {:.2} fps  {:.1} MiB BC7 ({:.1}% of raw)",
        animation.frame_count() as f32 / elapsed,
        blocks / (1024.0 * 1024.0),
        100.0 * blocks / raw as f64
    );
    println!(
        "quality: colour PSNR {colour_psnr:.2} dB, alpha PSNR {alpha_psnr:.2} dB, max alpha error {alpha_max_error}"
    );
    Ok(())
}
