//! Times the AI removal's per-frame CPU stages: resizing and normalising into the model's input, stretching
//! the network's output into a mask, and applying that mask to the frame.
//!
//! What is not here, and why: the inference itself, which needs the 176 MB model, and the ffmpeg extraction,
//! which needs a video. `tests/ai_end_to_end.rs` covers both and is ignored by default for those same reasons.
//! These three stages run on every frame whatever the model does, so they are the part worth having a baseline
//! for before anything is optimised.
//!
//! ```text
//! cargo run --release --example ai_frame_bench
//! ```

use std::hint::black_box;
use std::time::{Duration, Instant};

use anima_engine_rs::ai::{MODEL_SIZE, apply_mask, mask_from_output, preprocess};
use image::{GrayImage, RgbaImage};

/// A frame the size the Python app's clips are.
const FRAME: (u32, u32) = (1280, 720);
/// How many frames one round processes, so the per-frame numbers are averages over a clip's worth of work.
const FRAMES: usize = 10;
/// The clip length the per-clip column is for.
const CLIP: usize = 300;

fn best_of<F: FnMut()>(rounds: usize, mut case: F) -> Duration {
    let mut best = Duration::MAX;
    for _ in 0..rounds {
        let started = Instant::now();
        case();
        best = best.min(started.elapsed());
    }
    best
}

/// A frame with a soft-edged disc: enough structure that the resize and the mask are not working on a flat
/// colour, which the optimiser would otherwise handle far faster than a real frame.
fn synthetic_frame(width: u32, height: u32, seed: u32) -> RgbaImage {
    let mut image = RgbaImage::new(width, height);
    let (cx, cy) = (width as f32 / 2.0, height as f32 / 2.0);
    let radius = (width.min(height) as f32) * (0.30 + 0.03 * (seed % 4) as f32);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let distance = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
        let inside = ((radius - distance) / 8.0).clamp(0.0, 1.0);
        *pixel = image::Rgba([
            (x % 256) as u8,
            (y % 256) as u8,
            ((seed * 37) % 256) as u8,
            (inside * 255.0) as u8,
        ]);
    }
    image
}

/// The shape the network returns: one unbounded plane at the model's resolution.
fn synthetic_output(seed: u32) -> Vec<f32> {
    let size = MODEL_SIZE as usize;
    let mut output = vec![0f32; size * size];
    let (cx, cy) = (size as f32 / 2.0, size as f32 / 2.0);
    let radius = size as f32 * (0.32 + 0.02 * (seed % 4) as f32);
    for y in 0..size {
        for x in 0..size {
            let distance = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            output[y * size + x] = (radius - distance) / 20.0;
        }
    }
    output
}

fn main() {
    println!(
        "\nai removal, per-frame CPU stages: {}x{}, mean of {FRAMES} frames\n",
        FRAME.0, FRAME.1
    );

    let frames: Vec<RgbaImage> = (0..FRAMES)
        .map(|seed| synthetic_frame(FRAME.0, FRAME.1, seed as u32))
        .collect();
    let outputs: Vec<Vec<f32>> = (0..FRAMES)
        .map(|seed| synthetic_output(seed as u32))
        .collect();
    let masks: Vec<GrayImage> = outputs
        .iter()
        .map(|output| mask_from_output(output, FRAME.0, FRAME.1))
        .collect();

    let per_frame = |total: Duration| total.as_secs_f64() * 1000.0 / FRAMES as f64;

    let first = best_of(5, || {
        for frame in &frames {
            black_box(preprocess(frame));
        }
    });
    let mask = best_of(5, || {
        for output in &outputs {
            black_box(mask_from_output(output, FRAME.0, FRAME.1));
        }
    });
    let apply = best_of(5, || {
        for (frame, mask) in frames.iter().zip(&masks) {
            black_box(apply_mask(frame, mask));
        }
    });

    let (a, b, c) = (per_frame(first), per_frame(mask), per_frame(apply));
    println!(
        "  preprocess (to {}x{} f32)      {:>8.2} ms per frame",
        MODEL_SIZE, MODEL_SIZE, a
    );
    println!(
        "  mask_from_output (to {}x{})    {:>8.2} ms per frame",
        FRAME.0, FRAME.1, b
    );
    println!("  apply_mask                     {:>8.2} ms per frame", c);
    println!("  ----------------------------------------------");
    let total = a + b + c;
    println!(
        "  total                          {:>8.2} ms per frame",
        total
    );
    println!(
        "  {CLIP} frames would spend         {:>8.1} s in these three stages alone",
        total * CLIP as f64 / 1000.0
    );
    println!(
        "\n  (the reference run of the whole pipeline, inference and ffmpeg included, was about 4.9 s per\n   \
         frame on this machine; these stages are the part that is ours rather than the model's)\n"
    );
}
