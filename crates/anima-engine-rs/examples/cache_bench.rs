//! Compare the frame cache (one zstd stream of raw frames) with the previous
//! PNG-per-frame ZIP format on a real animation.
//!
//! ```text
//! cargo run --release --example cache_bench -- <animation>
//! ```

use std::io::{Read, Write};
use std::time::Instant;

use anima_engine_rs::anim::{Animation, DecodeLimits, decode_reporting};
use anima_engine_rs::cache::FrameCache;
use image::ImageEncoder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let argument = std::env::args()
        .nth(1)
        .ok_or("usage: cache_bench <animation>")?;
    let path = std::path::PathBuf::from(&argument);

    let started = Instant::now();
    let (animation, _) = decode_reporting(&path, DecodeLimits::unbounded())?;
    let raw = animation.bytes();
    println!(
        "{}: {} frame(s) {}x{}, {:.1} MiB raw, decode {:.2}s",
        argument,
        animation.frame_count(),
        animation.width,
        animation.height,
        raw as f64 / (1024.0 * 1024.0),
        started.elapsed().as_secs_f32()
    );

    let dir = tempfile::tempdir()?;
    let cache = FrameCache::new(dir.path());
    let started = Instant::now();
    cache.write(&path, &animation)?;
    let zstd_write = started.elapsed().as_secs_f32();
    let size = std::fs::metadata(cache.entry_path(&path)?)?.len();
    let started = Instant::now();
    let loaded = cache.read(&path).ok_or("cache read failed")?;
    let zstd_read = started.elapsed().as_secs_f32();
    println!(
        "zstd  write {zstd_write:>6.2}s  read {zstd_read:>6.2}s  {:>7.1} MiB ({:>5.1}% of raw)  {} frame(s) back",
        size as f64 / (1024.0 * 1024.0),
        100.0 * size as f64 / raw as f64,
        loaded.frame_count()
    );

    let (png_size, png_write, png_read) = png_zip(&animation)?;
    println!(
        "png   write {png_write:>6.2}s  read {png_read:>6.2}s  {:>7.1} MiB ({:>5.1}% of raw)",
        png_size as f64 / (1024.0 * 1024.0),
        100.0 * png_size as f64 / raw as f64
    );
    Ok(())
}

/// The previous format: every frame PNG-encoded inside a ZIP. Returns
/// `(bytes, encode seconds, decode seconds)`.
fn png_zip(animation: &Animation) -> Result<(u64, f32, f32), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut out);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (index, frame) in animation.frames.iter().enumerate() {
            zip.start_file(format!("frames/{index:06}.png"), options)?;
            let mut png = Vec::new();
            image::codecs::png::PngEncoder::new(&mut png).write_image(
                &frame.rgba,
                animation.width,
                animation.height,
                image::ExtendedColorType::Rgba8,
            )?;
            zip.write_all(&png)?;
        }
        zip.finish()?;
    }
    let encode = started.elapsed().as_secs_f32();
    let bytes = out.into_inner();
    let size = bytes.len() as u64;

    let started = Instant::now();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes))?;
    for index in 0..animation.frames.len() {
        let mut png = Vec::new();
        archive
            .by_name(&format!("frames/{index:06}.png"))?
            .read_to_end(&mut png)?;
        let image = image::load_from_memory_with_format(&png, image::ImageFormat::Png)?.to_rgba8();
        std::hint::black_box(image);
    }
    let read = started.elapsed().as_secs_f32();

    Ok((size, encode, read))
}
