//! Colour-key background removal: turn "pixels near this colour" into transparency.
//!
//! The Python tool this replaces (`_internal/RemoveBackWeb.py`) tests each channel separately —
//! `abs(r - bg.r) <= t && abs(g - bg.g) <= t && abs(b - bg.b) <= t` — so the region it removes is a
//! *cube* in RGB rather than a ball, and the alpha it writes is either 0 or untouched. Two consequences
//! the user sees: colours that look close but sit near a corner of that cube survive, and every keyed
//! edge is hard.
//!
//! This does the two things that file does not:
//!
//! * a **perceptual distance** rather than a box, using the "redmean" approximation — a weighted
//!   Euclidean metric that costs three multiplications and no dependence on a colour-space crate, and
//!   tracks human perception far better than plain RGB distance;
//! * a **soft edge**: the tolerance is a band, not a cliff, so alpha ramps from 0 at `tolerance` to 255
//!   at `tolerance + feather`, which is what stops a keyed animation from looking cut out with scissors.

/// The distance formula's weights, from the redmean approximation. Cheap, and much closer to perceived
/// colour difference than the unweighted version.
fn distance(a: [u8; 3], b: [u8; 3]) -> f32 {
    let rmean = (a[0] as f32 + b[0] as f32) * 0.5;
    let dr = (a[0] as f32 - b[0] as f32).abs();
    let dg = (a[1] as f32 - b[1] as f32).abs();
    let db = (a[2] as f32 - b[2] as f32).abs();
    (((512.0 + rmean) * dr * dr) / 256.0 + 4.0 * dg * dg + ((767.0 - rmean) * db * db) / 256.0)
        .sqrt()
}

/// The largest distance the metric produces, used to normalise a 0-100 tolerance onto it.
const MAX_DISTANCE: f32 = 764.833;

/// Key out one RGBA buffer in place. `tolerance` and `feather` are 0-100, and feather ramps alpha up as
/// the distance grows past the tolerance, so a value of 0 gives the hard edge the original tool had.
pub fn key_out(rgba: &mut [u8], background: [u8; 3], tolerance: u8, feather: u8) {
    let near = f32::from(tolerance.min(100)) / 100.0 * MAX_DISTANCE;
    let band = f32::from(feather.min(100)) / 100.0 * MAX_DISTANCE;
    for pixel in rgba.as_chunks_mut::<4>().0 {
        // Nothing to do for a pixel that is already fully transparent, and skipping it keeps the
        // original alpha of a sprite that was keyed once already.
        if pixel[3] == 0 {
            continue;
        }
        let d = distance([pixel[0], pixel[1], pixel[2]], background);
        let keep = if band <= f32::EPSILON {
            if d <= near { 0.0 } else { 1.0 }
        } else if d <= near {
            0.0
        } else if d >= near + band {
            1.0
        } else {
            (d - near) / band
        };
        // Multiply rather than assign: a pixel that was already semi-transparent stays that way.
        pixel[3] = (f32::from(pixel[3]) * keep).round().clamp(0.0, 255.0) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(rgba: &[u8]) -> [u8; 4] {
        [rgba[0], rgba[1], rgba[2], rgba[3]]
    }

    #[test]
    fn the_background_goes_fully_transparent() {
        let mut data = vec![255, 255, 255, 255];
        key_out(&mut data, [255, 255, 255], 10, 0);
        assert_eq!(pixel(&data)[3], 0);
    }

    #[test]
    fn a_colour_far_from_the_key_is_untouched() {
        let mut data = vec![0, 200, 0, 255];
        key_out(&mut data, [255, 255, 255], 10, 0);
        assert_eq!(pixel(&data), [0, 200, 0, 255]);
    }

    #[test]
    fn the_feather_band_produces_partial_alpha() {
        // Just past the tolerance, so the ramp has started but has not finished.
        let mut data = vec![200, 200, 200, 255];
        key_out(&mut data, [255, 255, 255], 5, 40);
        let alpha = pixel(&data)[3];
        assert!(
            alpha > 0 && alpha < 255,
            "expected a soft edge, got {alpha}"
        );
    }

    #[test]
    fn the_metric_is_not_a_per_channel_box() {
        // (255, 178, 0) is far from white on two channels but its *distance* is moderate, while
        // (128,128,128) is uniform yet further away as a colour. A per-channel box cannot tell these
        // two apart the way a distance can, which is the whole reason for the metric.
        let mut amber = vec![255, 178, 0, 255];
        let mut grey = vec![128, 128, 128, 255];
        key_out(&mut amber, [255, 255, 255], 30, 0);
        key_out(&mut grey, [255, 255, 255], 30, 0);
        assert_eq!(pixel(&amber)[3], 255, "amber is not close to white");
        assert_eq!(pixel(&grey)[3], 255, "and neither is mid grey");
    }

    #[test]
    fn an_already_transparent_pixel_stays_transparent() {
        let mut data = vec![255, 255, 255, 0];
        key_out(&mut data, [0, 0, 0], 50, 20);
        assert_eq!(pixel(&data)[3], 0);
    }

    #[test]
    fn a_semi_transparent_pixel_is_multiplied_not_replaced() {
        let mut data = vec![255, 255, 255, 128];
        key_out(&mut data, [255, 255, 255], 0, 0);
        assert_eq!(
            pixel(&data)[3],
            0,
            "the key applies, and nothing is added back"
        );

        let mut outside = vec![0, 0, 255, 128];
        key_out(&mut outside, [255, 255, 255], 0, 0);
        assert_eq!(pixel(&outside)[3], 128, "outside the key it is left alone");
    }
}
