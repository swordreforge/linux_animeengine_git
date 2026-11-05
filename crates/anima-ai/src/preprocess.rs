//! The two conversions either side of the network.
//!
//! These are the parts that have to match rembg exactly, because a mismatch here does not raise: it
//! produces a mask, just the wrong one. Two details in particular are not the obvious ones, and both are
//! pinned by tests below.

use crate::model::MODEL_SIZE;

/// The mean and deviation the DIS session normalises with. `std` is all ones, deliberately: every
/// neighbouring session in rembg uses ImageNet's `(0.229, 0.224, 0.225)` and this one does not.
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [1.0, 1.0, 1.0];

/// An RGBA frame into the network's input tensor: the whole image squeezed into one 1024x1024 RGB square,
/// divided by its own brightest pixel, then normalised per channel and laid out channels-first.
///
/// `Lanczos3` is what Pillow calls `LANCZOS`, so the pixels that go in are the pixels the Python version
/// would have sent.
pub fn preprocess(image: &image::RgbaImage) -> Vec<f32> {
    let resized = image::imageops::resize(
        image,
        MODEL_SIZE,
        MODEL_SIZE,
        image::imageops::FilterType::Lanczos3,
    );

    let mut pixels: Vec<f32> = Vec::with_capacity((MODEL_SIZE * MODEL_SIZE * 3) as usize);
    for pixel in resized.pixels() {
        // Straight to RGB and drop alpha: a transparent frame is still a frame to segment.
        for channel in 0..3 {
            pixels.push(pixel.0[channel] as f32);
        }
    }

    // The quirk: rembg scales by the brightest channel value in this image, not by 255. Two frames of one
    // animation can therefore take different paths to the network, exactly as they do in Python. The
    // `.max(1.0)` is this code's own guard against an all-black frame, where numpy would divide by zero.
    let brightest = pixels.iter().copied().fold(0.0f32, f32::max).max(1.0);
    for value in &mut pixels {
        *value /= brightest;
    }

    // HWC to CHW, normalising per channel on the way.
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

/// The network's output plane, stretched back to the size of the frame it came from.
///
/// The output is a single channel of unbounded values, so it is min-max normalised to 0..=1 first — the
/// same stretch rembg applies — then read as luminance and resized with `Lanczos3`. The result is an alpha
/// mask, not a colour image: the original RGB is left alone, which is what makes a half-transparent edge
/// look right instead of grey.
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
        if let Some(value) = mask.get_pixel_checked(x, y) {
            pixel.0[3] = value.0[0];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32, colour: [u8; 4]) -> image::RgbaImage {
        image::RgbaImage::from_pixel(width, height, image::Rgba(colour))
    }

    #[test]
    fn the_brightest_pixel_decides_the_scale_not_255() {
        // A mid-grey frame: 128 is its brightest channel value, so the network sees 1.0, not 128/255.
        let tensor = preprocess(&frame(8, 8, [128, 128, 128, 255]));
        let red = tensor[0];
        assert!((red - (1.0 - MEAN[0])).abs() < 1e-6, "got {red}");
    }

    #[test]
    fn a_dim_frame_is_scaled_up_by_its_own_maximum() {
        // Half the brightness gives the same input: that is the quirk, and it is why a dark animation is
        // not simply darker to the network.
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
        // White means the network's input is 1.0 in every channel, so each should come back as 1.0 minus
        // its own mean. Note where the channels live: they are planes, not neighbours, and a test that reads
        // `tensor[1]` as green is reading the second pixel of red.
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
        // ImageNet's deviation would have made these a quarter smaller, which is the point of the test.
        assert!(tensor[plane] > 0.5, "std is not 1.0: {}", tensor[plane]);
    }

    #[test]
    fn channels_are_laid_out_planes_first() {
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
        let side = MODEL_SIZE as usize;
        let mut output = vec![0.0f32; side * side];
        // Raw values well outside 0..1 on the left half and negative on the right: min-max has to map them
        // to 1 and 0, because these outputs are unbounded.
        for (index, value) in output.iter_mut().enumerate() {
            let x = index % side;
            *value = if x < side / 2 { 9.5 } else { -4.0 };
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
        // A flat map has no range to stretch, so everything lands on zero instead of becoming a NaN the way
        // numpy's 0/0 would. That guard is why a model returning one value cannot poison the mask.
        assert_eq!(mask.get_pixel(4, 4).0[0], 0);
    }

    #[test]
    fn applying_a_mask_leaves_the_colour_alone() {
        let frame = frame(4, 4, [200, 100, 50, 255]);
        let mask = image::GrayImage::from_pixel(4, 4, image::Luma([77]));
        let cut = apply_mask(&frame, &mask);
        assert_eq!(cut.get_pixel(1, 1).0, [200, 100, 50, 77]);
    }
}
