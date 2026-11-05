//! AI background removal: one ONNX model, one session, and the same three steps rembg takes.
//!
//! The step that has to be copied exactly is the preprocessing, because it is not the obvious one. rembg
//! divides by the brightest pixel in *that* image rather than by 255, and the DIS-family session it uses
//! for anime art passes `std = (1, 1, 1)` rather than the ImageNet standard deviation every neighbouring
//! session uses. Getting either wrong produces a mask instead of an error, which is the worst kind of
//! wrong, so both are pinned in tests below.
//!
//! The model is `isnet-anime`, the anime-tuned segmentation network the Python app was written for. It
//! still downloads from the same URL: rembg renamed the *session* to `dis-anime` and kept the weights, and
//! the Python tool broke only because it asked for the old session name.

use md5::{Digest, Md5};
use std::io::Read;
use std::path::PathBuf;

/// Where rembg keeps these models. The same file the Python app would have used.
pub const MODEL_URL: &str =
    "https://github.com/danielgatis/rembg/releases/download/v0.0.0/isnet-anime.onnx";
pub const MODEL_FILE: &str = "isnet-anime.onnx";
/// Cut into 1024x1024 squares, the size this network was trained at.
pub const MODEL_SIZE: u32 = 1024;
/// The md5 rembg publishes for this file. A download that does not match is not usable, and saying so at
/// download time is much kinder than a session that fails to load later.
pub const MODEL_MD5: &str = "6f184e756bb3bd901c8849220a83e38e";

/// The mean and standard deviation the DIS session normalises with. `std` is all ones here, deliberately:
/// the neighbouring sessions use ImageNet's `(0.229, 0.224, 0.225)`.
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [1.0, 1.0, 1.0];

/// Where the model lives once downloaded.
pub fn model_path() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("AnimaEngine").join("models").join(MODEL_FILE)
}

/// Whether the model is here, so the UI can offer to fetch it rather than failing on the first frame.
pub fn model_ready() -> bool {
    model_path().is_file()
}

/// Turn a decoded RGBA frame into the network's input tensor: the whole image squeezed into one
/// 1024x1024 RGB square, divided by its own brightest pixel, then normalised per channel.
///
/// `image` is the crate's own resize, and `Lanczos3` is what Pillow's `LANCZOS` is, so the pixels that go
/// in are the pixels the Python version would have sent.
pub fn preprocess(image: &image::RgbaImage) -> Vec<f32> {
    let resized = image::imageops::resize(
        image,
        MODEL_SIZE,
        MODEL_SIZE,
        image::imageops::FilterType::Lanczos3,
    );

    let mut pixels: Vec<f32> = Vec::with_capacity((MODEL_SIZE * MODEL_SIZE * 3) as usize);
    for pixel in resized.pixels() {
        // Straight to RGB, and drop alpha: a transparent frame is still a frame to segment.
        for channel in 0..3 {
            pixels.push(pixel.0[channel] as f32);
        }
    }

    // The quirk: rembg scales by the brightest channel value in this image, not by 255. Two frames of the
    // same animation can therefore take different paths to the network, exactly as they do in Python.
    let brightest = pixels.iter().copied().fold(0.0f32, f32::max).max(1.0);
    for value in &mut pixels {
        *value /= brightest;
    }

    // HWC to CHW, because the graph takes channels first, and normalise per channel on the way.
    let mut tensor = vec![0.0f32; pixels.len()];
    let plane = (MODEL_SIZE * MODEL_SIZE) as usize;
    for index in 0..plane {
        for channel in 0..3 {
            tensor[channel * plane + index] =
                (pixels[index * 3 + channel] - MEAN[channel]) / STD[channel];
        }
    }
    tensor
}

/// The inverse of the network's output: one raw output plane, stretched back to the frame it came from.
///
/// The output is a single-channel map of unbounded scores, so it is min-max normalised to 0..=1 first —
/// the same stretch rembg applies — then read as luminance and resized with `Lanczos3`. The result is an
/// alpha mask, not a colour image: the RGB of the original frame is left alone, which is what makes a
/// half-transparent edge look right instead of grey.
pub fn mask_from_output(output: &[f32], width: u32, height: u32) -> image::GrayImage {
    let (mut low, mut high) = (f32::MAX, f32::MIN);
    for value in output {
        if value.is_finite() {
            low = low.min(*value);
            high = high.max(*value);
        }
    }
    let span = if (high - low).abs() < f32::EPSILON {
        1.0
    } else {
        high - low
    };

    let scaled = image::GrayImage::from_fn(MODEL_SIZE, MODEL_SIZE, |x, y| {
        let index = (y * MODEL_SIZE + x) as usize;
        let value = output.get(index).copied().unwrap_or(0.0);
        let normalised = ((value - low) / span).clamp(0.0, 1.0);
        image::Luma([(normalised * 255.0).round() as u8])
    });
    image::imageops::resize(
        &scaled,
        width,
        height,
        image::imageops::FilterType::Lanczos3,
    )
}

/// Apply a mask the way `putalpha` does: alpha becomes the mask, colour is untouched.
pub fn apply_mask(frame: &image::RgbaImage, mask: &image::GrayImage) -> image::RgbaImage {
    let mut out = frame.clone();
    for (x, y, pixel) in out.enumerate_pixels_mut() {
        if let Some(m) = mask.get_pixel_checked(x, y) {
            pixel.0[3] = m.0[0];
        }
    }
    out
}

/// Download the model next to where rembg would have put it, checking the published md5 before it is
/// allowed to take the final name. `progress` is called with (received, total) as it goes, so a 176 MB
/// download can say something true about itself.
pub fn download_model(mut progress: impl FnMut(u64, Option<u64>)) -> Result<PathBuf, String> {
    let target = model_path();
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{parent:?}: {e}"))?;
    }
    let partial = target.with_extension("onnx.part");

    let response = ureq::get(MODEL_URL)
        .call()
        .map_err(|e| format!("{MODEL_URL}: {e}"))?;
    let total: Option<u64> = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok());

    let mut reader = response.into_body().into_reader();
    let mut file = std::fs::File::create(&partial).map_err(|e| format!("{partial:?}: {e}"))?;
    let mut hasher = Md5::new();
    let mut received = 0u64;
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|e| format!("reading the download: {e}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        std::io::Write::write_all(&mut file, &buffer[..read])
            .map_err(|e| format!("writing {partial:?}: {e}"))?;
        received += read as u64;
        progress(received, total);
    }
    drop(file);

    // Hex by hand: this digest version's output array does not implement LowerHex.
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if digest != MODEL_MD5 {
        let _ = std::fs::remove_file(&partial);
        return Err(format!(
            "the model did not match its checksum (got {digest}, expected {MODEL_MD5}) — not keeping it"
        ));
    }
    std::fs::rename(&partial, &target).map_err(|e| format!("{target:?}: {e}"))?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    #[test]
    fn probe_the_actual_numbers() {
        let t = preprocess(&frame(8, 8, [255, 255, 255, 255]));
        println!("white: t0={:.9} t1={:.9} t2={:.9}", t[0], t[1], t[2]);
        println!(
            "rhs:   r0={:.9} r1={:.9} r2={:.9}",
            1.0 - MEAN[0],
            1.0 - MEAN[1],
            1.0 - MEAN[2]
        );
        println!("diff:  {:.3e}", (t[0] - (1.0 - MEAN[0])).abs());
        let r = preprocess(&frame(4, 4, [255, 0, 0, 255]));
        let plane = (MODEL_SIZE * MODEL_SIZE) as usize;
        println!(
            "red: g0={:.9} green_plane={:.9} want={:.9}",
            r[0],
            r[plane],
            0.0 - MEAN[1]
        );
        let g = preprocess(&frame(8, 8, [128, 128, 128, 255]));
        println!("grey: t0={:.9} want={:.9}", g[0], (1.0 - MEAN[0]));
    }

    use super::*;

    fn frame(width: u32, height: u32, colour: [u8; 4]) -> image::RgbaImage {
        image::RgbaImage::from_pixel(width, height, image::Rgba(colour))
    }

    #[test]
    fn the_brightest_pixel_decides_the_scale_not_255() {
        // A mid-grey frame: 128 is its brightest channel, so the network sees 1.0 and not 128/255.
        let tensor = preprocess(&frame(8, 8, [128, 128, 128, 255]));
        let red = tensor[0];
        // (1.0 - 0.485) / 1.0 — with a 255 divisor this would come out around (0.502 - 0.485).
        assert!((red - 0.515).abs() < 1e-6, "got {red}");
    }

    #[test]
    fn a_dim_frame_is_scaled_up_by_its_own_maximum() {
        // Half the brightness gives the same input as the frame above: this is the quirk, and it is why a
        // dark animation is not simply darker to the network.
        let bright = preprocess(&frame(8, 8, [128, 128, 128, 255]));
        let dim = preprocess(&frame(8, 8, [64, 64, 64, 255]));
        assert!(
            (bright[0] - dim[0]).abs() < 1e-6,
            "{:?} vs {:?}",
            bright[0],
            dim[0]
        );
    }

    #[test]
    fn the_std_is_one_rather_than_imagenets() {
        // White means the network's input is 1.0 in every channel, so each channel should come back as
        // 1.0 minus its own mean. Note where the channels live: they are planes, not neighbours, and a
        // test that reads `tensor[1]` as green is reading the second pixel of red.
        let tensor = preprocess(&frame(8, 8, [255, 255, 255, 255]));
        let plane = (MODEL_SIZE * MODEL_SIZE) as usize;
        for (channel, slot) in [(0, 0), (1, plane), (2, 2 * plane)] {
            let got = tensor[slot];
            let want = 1.0 - MEAN[channel];
            assert!(
                (got - want).abs() < 1e-6,
                "channel {channel}: got {got}, want {want}"
            );
        }
        // ImageNet's std would have made these a quarter smaller, which is the whole point of the test.
        assert!(tensor[plane] > 0.5, "std is not 1.0: {}", tensor[plane]);
    }

    #[test]
    fn channels_are_laid_out_planes_first() {
        // Red in the first pixel must land at the start of the first plane, not at index 0 of an
        // interleaved buffer — and the plane for one channel is exactly one square long.
        let tensor = preprocess(&frame(4, 4, [255, 0, 0, 255]));
        let plane = (MODEL_SIZE * MODEL_SIZE) as usize;
        assert!((tensor[0] - (1.0 - MEAN[0])).abs() < 1e-6);
        assert!(
            (tensor[plane] - (0.0 - MEAN[1])).abs() < 1e-6,
            "green plane"
        );
        assert!(
            (tensor[2 * plane] - (0.0 - MEAN[2])).abs() < 1e-6,
            "blue plane"
        );
    }

    #[test]
    fn the_output_is_stretched_and_resized_back() {
        let plane = (MODEL_SIZE * MODEL_SIZE) as usize;
        let mut output = vec![0.0f32; plane];
        // A raw logit well outside 0..1 on the left half, a negative one on the right: min-max has to
        // map them to 1 and 0, because the network's outputs are unbounded.
        for (index, value) in output.iter_mut().enumerate() {
            let x = index % MODEL_SIZE as usize;
            *value = if x < MODEL_SIZE as usize / 2 {
                9.5
            } else {
                -4.0
            };
        }
        let mask = mask_from_output(&output, 16, 16);
        assert_eq!(mask.dimensions(), (16, 16));
        assert_eq!(mask.get_pixel(2, 8).0[0], 255, "left half should be opaque");
        assert_eq!(mask.get_pixel(13, 8).0[0], 0, "right half should be clear");
    }

    #[test]
    fn a_flat_output_does_not_divide_by_zero() {
        let output = vec![3.0f32; (MODEL_SIZE * MODEL_SIZE) as usize];
        let mask = mask_from_output(&output, 8, 8);
        // A flat map has no range to stretch, so everything lands on zero instead of becoming a NaN the
        // way numpy's 0/0 would. That guard is why a model returning one value cannot poison the mask.
        assert_eq!(mask.get_pixel(4, 4).0[0], 0);
    }

    #[test]
    fn applying_a_mask_leaves_the_colour_alone() {
        let frame = frame(4, 4, [200, 100, 50, 255]);
        let mask = image::GrayImage::from_pixel(4, 4, image::Luma([77]));
        let cut = apply_mask(&frame, &mask);
        assert_eq!(cut.get_pixel(1, 1).0, [200, 100, 50, 77]);
    }

    #[test]
    fn the_model_lands_where_the_python_app_would_have_looked() {
        let path = model_path();
        assert!(
            path.ends_with("AnimaEngine/models/isnet-anime.onnx"),
            "{path:?}"
        );
    }
}
