//! Compares the system allocator with mimalloc on this project's allocation shapes.
//!
//! Microsoft's guideline recommends mimalloc for applications, with a figure of "up to 25%" taken from their
//! own benchmarks. That figure describes their workload, so this measures ours instead, and the answer is
//! allowed to be "no difference" — in which case the dependency is not worth having.
//!
//! Run both ways and compare:
//!
//! ```text
//! cargo run --release --example alloc_bench
//! cargo run --release --example alloc_bench --features mimalloc
//! ```
//!
//! Each case runs several rounds and reports the best, which is less noisy than the mean on a desktop. The
//! throughput column is the bytes the case asks for, counted from its own parameters rather than from an
//! instrumented allocator, so it is honest about what the shape is even if it is not a profile.

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL_MIMALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::hint::black_box;
use std::time::{Duration, Instant};

const SMALL: usize = 50_000;
const THREADS: usize = 4;
const FRAMES: usize = 200;
const MODEL_RUNS: usize = 20;
/// 480x270 at four bytes a pixel, the overlay's default animation size.
const FRAME_BYTES: usize = 480 * 270 * 4;
/// The AI preprocessor's input: 1024x1024x3 f32.
const MODEL_BYTES: usize = 1024 * 1024 * 3 * 4;

/// The average of the sizes `small_churn` asks for: 16 + (i % 8) * 24.
const SMALL_AVERAGE: usize = 100;

fn best_of<F: FnMut()>(rounds: usize, mut case: F) -> Duration {
    let mut best = Duration::MAX;
    for _ in 0..rounds {
        let started = Instant::now();
        case();
        best = best.min(started.elapsed());
    }
    best
}

fn report(name: &str, best: Duration, bytes: Option<usize>) {
    match bytes {
        Some(bytes) => {
            let per_second = bytes as f64 / best.as_secs_f64() / (1024.0 * 1024.0);
            println!(
                "  {name:36} {:>8.1} ms   {:>9.1} MiB/s",
                best.as_secs_f64() * 1000.0,
                per_second
            );
        }
        None => println!("  {name:36} {:>8.1} ms", best.as_secs_f64() * 1000.0),
    }
}

/// Many short-lived small buffers, the shape of building a vertex or rectangle list.
fn small_churn(count: usize) {
    let mut keep = Vec::with_capacity(count);
    for i in 0..count {
        let size = 16 + (i % 8) * 24;
        let mut buffer = Vec::with_capacity(size);
        buffer.resize(size, i as u8);
        keep.push(buffer);
    }
    black_box(&keep);
}

/// One RGBA frame at a time, allocated and filled the way an upload would.
fn frame_buffers(count: usize) {
    for i in 0..count {
        let mut frame = vec![0u8; FRAME_BYTES];
        frame[0] = i as u8;
        black_box(&frame);
    }
}

/// The preprocessor's model input, about 12 MiB per frame.
fn model_input(count: usize) {
    for i in 0..count {
        let mut pixels = vec![0f32; MODEL_BYTES / 4];
        pixels[0] = i as f32;
        black_box(&pixels);
    }
}

/// Growing a vector from empty, which is what the conventions say to avoid when the size is known.
fn grow_from_empty(count: usize) {
    for _ in 0..count {
        let mut numbers = Vec::new();
        for i in 0..512 {
            numbers.push(i);
        }
        black_box(&numbers);
    }
}

fn threads(count: usize, each: usize) {
    let handles: Vec<_> = (0..count)
        .map(|_| std::thread::spawn(move || small_churn(each)))
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
}

fn peak_rss() -> String {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest.trim().to_string();
        }
    }
    "unknown".to_string()
}

fn main() {
    #[cfg(feature = "mimalloc")]
    let which = "mimalloc";
    #[cfg(not(feature = "mimalloc"))]
    let which = "system";

    println!("\nallocator: {which}\n");

    let best = best_of(5, || small_churn(SMALL));
    report(
        "small churn, 50k buffers",
        best,
        Some(SMALL * SMALL_AVERAGE),
    );

    let best = best_of(3, || frame_buffers(FRAMES));
    report(
        "frame buffers, 480x270 RGBA x200",
        best,
        Some(FRAMES * FRAME_BYTES),
    );

    let best = best_of(3, || model_input(MODEL_RUNS));
    report(
        "model input, 12 MiB x20",
        best,
        Some(MODEL_RUNS * MODEL_BYTES),
    );

    let best = best_of(5, || grow_from_empty(2_000));
    report("grow from empty, 512 elem x2000", best, None);

    let best = best_of(3, || threads(THREADS, SMALL));
    report(
        "4 threads, 50k buffers each",
        best,
        Some(THREADS * SMALL * SMALL_AVERAGE),
    );

    println!("\n  peak RSS: {}\n", peak_rss());
}
