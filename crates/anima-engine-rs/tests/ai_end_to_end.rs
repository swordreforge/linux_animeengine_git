//! The one test that needs the network: the model downloaded, a real frame through it.
//!
//! Ignored by default so the suite stays offline. It needs an anime frame as well as the model: point
//! `ANIMA_AI_FRAME` at one, or leave a png in `/tmp/animaframes` and it takes the first it finds. What it
//! asserts is only what this code promises — the size comes back the same and the colour is untouched — and it
//! prints the share it cut, which is the model's opinion rather than a guarantee, for a person to read.
//!
//! ```text
//! ANIMA_AI_FRAME=/path/to/frame.png \
//!   cargo test -p anima-engine-rs --features studio --test ai_end_to_end -- --ignored --nocapture
//! ```

use anima_ai as ai;

#[test]
#[ignore = "downloads 176 MB and runs inference"]
fn the_model_actually_cuts_a_frame() {
    if !ai::model_ready() {
        let path = ai::download_model(|received, total| {
            if received % (32 << 20) < (1 << 20) {
                println!("  downloaded {received} of {total:?} bytes");
            }
        })
        .expect("download the model");
        println!("model at {}", path.display());
    }

    // A real frame matters. isnet-anime segments anime art, and the first version of this test drew a
    // rectangle, which the model "removed" entirely — an input it had never seen.
    let sample: Option<std::path::PathBuf> = std::env::var_os("ANIMA_AI_FRAME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            let directory = std::env::var_os("ANIMA_AI_FRAME_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("/tmp/animaframes"));
            std::fs::read_dir(directory).ok().and_then(|entries| {
                entries.flatten().map(|entry| entry.path()).find(|path| {
                    matches!(
                        path.extension().and_then(|extension| extension.to_str()),
                        Some("png" | "bmp" | "jpg" | "jpeg")
                    )
                })
            })
        });
    let frame = match &sample {
        Some(path) => image::open(path).expect("the sample frame").to_rgba8(),
        None => panic!("put an anime png in /tmp/animaframes or set ANIMA_AI_FRAME"),
    };
    println!("frame {:?} from {sample:?}", frame.dimensions());

    let mut model = ai::Removal::load().expect("load the model");
    let started = std::time::Instant::now();
    let cut = model.remove_background(&frame).expect("cut the frame");
    let elapsed = started.elapsed();

    let total = (cut.width() * cut.height()) as usize;
    let opaque = cut.pixels().filter(|pixel| pixel.0[3] > 200).count();
    let clear = cut.pixels().filter(|pixel| pixel.0[3] < 40).count();
    println!(
        "cut {:?} in {elapsed:?}: {:.1}% opaque, {:.1}% clear",
        cut.dimensions(),
        opaque as f64 * 100.0 / total as f64,
        clear as f64 * 100.0 / total as f64
    );

    // Two things this code promises, and they hold whatever the model decides: the frame comes back the
    // same size, and the colour is untouched. Whether anything was *removed* is the model's opinion rather
    // than this code's guarantee — asked about a UI screenshot it answers "no foreground", correctly.
    assert_eq!(cut.dimensions(), frame.dimensions());
    assert_eq!(cut.get_pixel(0, 0).0[..3], frame.get_pixel(0, 0).0[..3]);
}
