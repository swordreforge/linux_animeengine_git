//! Times the overlay's two per-frame costs: the input region it rebuilds every few frames, and the texture
//! upload.
//!
//! The region part calls the library directly — `Animation::alpha_rects_step` is what the overlay runs. The
//! upload part reproduces the three wgpu calls `upload_frame` makes, because that function lives in the
//! binary and an example cannot link to a binary. The format, size and usage flags are copied from
//! `src/bin/anima-overlay/frames.rs`, so the number describes the same work; it is a reproduction rather than
//! a call, and moving the upload into the library is what would make it a call.
//!
//! ```text
//! cargo run --release --features overlay --example frame_bench
//! ```
//!
//! `REGION_UPDATE_FRAMES` is 3, so at 60 fps the region is rebuilt about twenty times a second, which is what
//! the per-second column is for. Everything else is per frame.

use std::hint::black_box;
use std::time::{Duration, Instant};

use anima_engine_rs::anim::{Animation, Frame, Orientation};

/// The overlay's own values, copied from `src/bin/anima-overlay.rs`.
const ALPHA_HIT_THRESHOLD: u8 = 25;
const REGION_UPDATE_FRAMES: u64 = 3;
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
    Frame { rgba, delay_ms: 33 }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let animation = Animation {
        width: FRAME_SIZE.0,
        height: FRAME_SIZE.1,
        frames: (0..FRAMES)
            .map(|seed| synthetic_frame(FRAME_SIZE.0, FRAME_SIZE.1, seed as u32))
            .collect(),
    };
    let orientation = Orientation::default();

    println!(
        "\nframe path: {} frames at {}x{}\n",
        FRAMES, FRAME_SIZE.0, FRAME_SIZE.1
    );

    // --- the input region, calling the library ------------------------------------------------
    for step in [1u32, 2, 4] {
        let per_round = best_of(20, || {
            for frame in 0..FRAMES {
                let rects = animation.alpha_rects_step(
                    frame,
                    FRAME_SIZE,
                    orientation,
                    ALPHA_HIT_THRESHOLD,
                    step,
                );
                black_box(&rects);
            }
        }) / FRAMES as u32;
        let rects =
            animation.alpha_rects_step(0, FRAME_SIZE, orientation, ALPHA_HIT_THRESHOLD, step);
        let per_second = per_round.as_secs_f64() * (60.0 / REGION_UPDATE_FRAMES as f64);
        println!(
            "  region, step {step}: {:>7.3} ms per rebuild ({:>4} rects)   {:>6.2} ms/s at 60 fps",
            per_round.as_secs_f64() * 1000.0,
            rects.len(),
            per_second * 1000.0
        );
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
            let rgba = &animation.frames[index % FRAMES].rgba;
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

    let frame_budget = 1.0 / 60.0;
    println!(
        "\n  upload ({}, {:?}):\n    {:>7.3} ms per frame, copy plus its share of the drain   {:.1}% of a 16.7 ms budget",
        info.name,
        info.backend,
        per_upload.as_secs_f64() * 1000.0,
        100.0 * per_upload.as_secs_f64() / frame_budget
    );
    println!(
        "    {:>7.0} MiB/s of frame data\n",
        (FRAME_SIZE.0 * FRAME_SIZE.1 * 4) as f64 / per_upload.as_secs_f64() / (1024.0 * 1024.0)
    );

    Ok(())
}
