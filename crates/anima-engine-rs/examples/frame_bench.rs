//! Times the overlay's two per-frame costs: the input region it rebuilds every few frames, and the texture
//! upload.
//!
//! ```text
//! cargo run --release --features overlay --example frame_bench
//! cargo run --release --features overlay --example frame_bench -- <animation>
//! ```
//!
//! With a path, the region section runs on that animation's real frames and, at the end, applies the policy
//! `apply_opaque_region` uses — the soft and hard rectangle budgets, and `max_step = min(surface) / 128`
//! clamped to 8 — to say which step `Auto` would settle on and what that costs. Without a path it uses a
//! synthetic frame, which is what the upload section always uses.
//!
//! The region part calls the library directly: `Animation::alpha_rects_step` is what the overlay runs. The
//! upload part reproduces the three wgpu calls `upload_frame` makes, because that function lives in the binary
//! and an example cannot link to a binary. The format, size and usage flags are copied from
//! `src/bin/anima-overlay/frames.rs`, so the number describes the same work; it is a reproduction rather than a
//! call, and moving the upload into the library is what would make it a call.
//!
//! `REGION_UPDATE_FRAMES` is 3, so at 60 fps the region is rebuilt about twenty times a second, which is what
//! the per-second column is for. Everything else is per frame.

use std::hint::black_box;
use std::path::Path;
use std::time::{Duration, Instant};

use anima_engine_rs::anim::{Animation, DecodeLimits, Frame, Orientation, decode_reporting};

/// The overlay's own values, copied from `src/bin/anima-overlay.rs`.
const ALPHA_HIT_THRESHOLD: u8 = 25;
const REGION_UPDATE_FRAMES: u64 = 3;
const DEFAULT_REGION_SOFT_RECTS: usize = 768;
const DEFAULT_REGION_MAX_RECTS: usize = 4096;
/// How many frames of a real animation to measure. A clip can have hundreds, and the shape of the cost is the
/// point rather than its total.
const FRAMES_MEASURED: usize = 60;
/// Rounds per measurement, of which the best is kept.
///
/// Five was not enough. Two runs of *identical* code produced 1.442 ms and 1.473 ms at step 1 — a spread of
/// 2.1 % — which is the same size as the effect anyone would be trying to see, so the round count has to be
/// high enough that a two-percent change is distinguishable before any such change is believed. Fifty rounds
/// of sixty frames costs a few seconds.
const ROUNDS: usize = 50;
const FRAME_SIZE: (u32, u32) = (480, 270);
const FRAMES: usize = 12;

fn best_of<F: FnMut()>(rounds: usize, mut case: F) -> Duration {
    let mut best = Duration::MAX;
    for _ in 0..rounds {
        let started = Instant::now();
        case();
        best = best.min(started.elapsed());
    }
    best
}

/// A frame with a soft-edged disc, so the region scan has a silhouette with a boundary rather than a solid
/// rectangle it would merge into one run.
fn synthetic_frame(width: u32, height: u32, seed: u32) -> Frame {
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    let (cx, cy) = (width as f32 / 2.0, height as f32 / 2.0);
    let radius = (width.min(height) as f32) * (0.34 + 0.02 * (seed % 4) as f32);
    for y in 0..height {
        for x in 0..width {
            let distance = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            let alpha = ((radius - distance) / 6.0).clamp(0.0, 1.0);
            let index = ((y * width + x) * 4) as usize;
            rgba[index] = (x % 256) as u8;
            rgba[index + 1] = (y % 256) as u8;
            rgba[index + 2] = 128;
            rgba[index + 3] = (alpha * 255.0) as u8;
        }
    }
    Frame {
        rgba,
        delay_ms: 33,
        alpha_bits: Vec::new(),
    }
}

/// One measurement at one step: the cost of a rebuild, and the rectangle counts the policy compares with its
/// budgets.
fn measure_step(
    animation: &Animation,
    surface: (u32, u32),
    orientation: Orientation,
    step: u32,
    rounds: usize,
    frames: usize,
) -> (Duration, usize, usize) {
    let elapsed = best_of(rounds, || {
        for frame in 0..frames {
            let rects =
                animation.alpha_rects_step(frame, surface, orientation, ALPHA_HIT_THRESHOLD, step);
            black_box(&rects);
        }
    }) / frames as u32;

    let mut total = 0usize;
    let mut worst = 0usize;
    for frame in 0..frames {
        let rects =
            animation.alpha_rects_step(frame, surface, orientation, ALPHA_HIT_THRESHOLD, step);
        total += rects.len();
        worst = worst.max(rects.len());
    }
    (elapsed, total / frames.max(1), worst)
}

/// The steps the policy can reach at this surface size: `min(surface) / 128`, clamped to 1..=8.
fn reachable_step(surface: (u32, u32)) -> u32 {
    (surface.0.min(surface.1) / 128).clamp(1, 8)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let orientation = Orientation::default();
    let path = std::env::args().nth(1);

    let (animation, surface, label) = match path.as_deref() {
        Some(path) => {
            let (animation, _) = decode_reporting(Path::new(path), DecodeLimits::unbounded())?;
            let surface = (animation.width, animation.height);
            (animation, surface, path.to_string())
        }
        None => (
            Animation {
                width: FRAME_SIZE.0,
                height: FRAME_SIZE.1,
                frames: (0..FRAMES)
                    .map(|seed| synthetic_frame(FRAME_SIZE.0, FRAME_SIZE.1, seed as u32))
                    .collect(),
            },
            FRAME_SIZE,
            "a synthetic 480x270 frame".to_string(),
        ),
    };

    let frames = animation.frames.len().min(FRAMES_MEASURED);
    let max_step = reachable_step(surface);
    println!(
        "\nregion scan on {} frame(s) at {}x{}, measuring {frames}, surface {surface:?}\n  from: {label}\n",
        animation.frames.len(),
        animation.width,
        animation.height
    );

    let mut at: Vec<(u32, Duration, usize)> = Vec::new();
    for step in [1u32, 2, 4, 8] {
        let (per_rebuild, mean, worst) =
            measure_step(&animation, surface, orientation, step, ROUNDS, frames);
        let reachable = if step <= max_step {
            ""
        } else {
            "  (unreachable: above max_step)"
        };
        println!(
            "  step {step}: {:>7.3} ms per rebuild   rects {:>5} mean, {:>5} worst   {:>6.2} ms/s at 60 fps{reachable}",
            per_rebuild.as_secs_f64() * 1000.0,
            mean,
            worst,
            per_rebuild.as_secs_f64() * (60.0 / REGION_UPDATE_FRAMES as f64) * 1000.0
        );
        at.push((step, per_rebuild, worst));
    }

    // The policy from `apply_opaque_region`, applied to the numbers just measured: coarsen by powers of two
    // while the worst frame is over the soft budget, and stop at max_step.
    let cost_at = |step: u32| {
        at.iter()
            .find(|(each, _, _)| *each == step)
            .map(|(_, cost, _)| *cost)
            .unwrap_or(Duration::ZERO)
    };
    let rects_at = |step: u32| {
        at.iter()
            .find(|(each, _, _)| *each == step)
            .map(|(_, _, worst)| *worst)
            .unwrap_or(0)
    };
    let mut chosen = 1u32;
    while rects_at(chosen) > DEFAULT_REGION_SOFT_RECTS && chosen < max_step {
        chosen *= 2;
    }
    let chosen_cost = cost_at(chosen);
    let per_second = chosen_cost.as_secs_f64() * (60.0 / REGION_UPDATE_FRAMES as f64);

    println!(
        "\n  budgets: soft {DEFAULT_REGION_SOFT_RECTS}, hard {DEFAULT_REGION_MAX_RECTS}, max_step {max_step} \
         (min(surface)/128)"
    );
    println!(
        "  Auto would pick step {chosen}: {} rects in the worst frame, {:.3} ms per rebuild, {:.2} ms/s at 60 \
         fps = {:.1}% of the process's wall time",
        rects_at(chosen),
        chosen_cost.as_secs_f64() * 1000.0,
        per_second * 1000.0,
        100.0 * per_second
    );
    if rects_at(chosen) > DEFAULT_REGION_MAX_RECTS {
        println!("  and the hard cap would then collapse it to one bounding rectangle");
    }

    // --- the upload, reproducing what `upload_frame` does ---------------------------------------
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    })) {
        Ok(adapter) => adapter,
        Err(error) => {
            println!(
                "\n  upload: no adapter ({error}); the region numbers above are still valid\n"
            );
            return Ok(());
        }
    };
    let info = adapter.get_info();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("frame-bench"),
        ..Default::default()
    }))?;

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("anima-frames"),
        size: wgpu::Extent3d {
            width: FRAME_SIZE.0,
            height: FRAME_SIZE.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    let uploads = 300usize;
    let mut per_upload = Duration::MAX;
    for _ in 0..5 {
        let started = Instant::now();
        for index in 0..uploads {
            let rgba = &animation.frames[index % frames].rgba;
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: 0, y: 0, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(FRAME_SIZE.0 * 4),
                    rows_per_image: Some(FRAME_SIZE.1),
                },
                wgpu::Extent3d {
                    width: FRAME_SIZE.0,
                    height: FRAME_SIZE.1,
                    depth_or_array_layers: 1,
                },
            );
        }
        queue.submit(std::iter::empty());
        // The overlay never waits for the GPU — it presents, and that is its synchronisation. This drain is
        // here so a round does not measure a queue that never empties, and its cost is part of the per-frame
        // number below, because the bytes do have to arrive.
        device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })?;
        per_upload = per_upload.min(started.elapsed() / uploads as u32);
    }

    println!(
        "\n  upload ({}, {:?}, {}x{} frame data):",
        info.name, info.backend, FRAME_SIZE.0, FRAME_SIZE.1
    );
    println!(
        "    {:>7.3} ms per frame   {:.1}% of a 16.7 ms budget   {:.0} MiB/s\n",
        per_upload.as_secs_f64() * 1000.0,
        100.0 * per_upload.as_secs_f64() * 60.0,
        (FRAME_SIZE.0 * FRAME_SIZE.1 * 4) as f64 / per_upload.as_secs_f64() / (1024.0 * 1024.0)
    );

    Ok(())
}
