//! Report how much transparent padding an animation has around its visible art.
//!
//! The overlay clamps against the whole canvas, so this padding is exactly what
//! keeps the art from reaching a screen edge when the overlay is pushed against it.
//!
//! ```text
//! cargo run --release --example ink_bounds -- <animation> [step]
//! ```

use std::path::Path;

use anima_engine_rs::anim::{DecodeLimits, decode_reporting};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let path = arguments
        .next()
        .ok_or("usage: ink_bounds <animation> [step]")?;
    let step: usize = arguments
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1)
        .max(1);

    let (animation, report) = decode_reporting(Path::new(&path), DecodeLimits::from_env())?;
    let (width, height) = animation.dimensions();

    // Union of the visible (alpha >= 25, the hit-test threshold) pixels.
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (width, height, 0u32, 0u32);
    let mut sampled = 0usize;
    let mut index = 0;
    while index < animation.frame_count() {
        sampled += 1;
        for y in 0..height {
            for x in 0..width {
                if animation.alpha_at(index, x, y) < 25 {
                    continue;
                }
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
        index += step;
    }

    let (left, top) = (min_x, min_y);
    let (right, bottom) = (width - 1 - max_x, height - 1 - max_y);
    println!(
        "{path}: {width}x{height}, {sampled} of {} frame(s) sampled{}",
        animation.frame_count(),
        report.note()
    );
    println!("  ink box: x {min_x}..={max_x}, y {min_y}..={max_y} ({sampled} frames)");
    println!(
        "  transparent padding: left {left}px, top {top}px, right {right}px, bottom {bottom}px"
    );
    println!(
        "  padding is {:.1}% of the width and {:.1}% of the height",
        100.0 * f64::from(left + right) / f64::from(width),
        100.0 * f64::from(top + bottom) / f64::from(height)
    );
    Ok(())
}
