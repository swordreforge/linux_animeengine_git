//! The silhouette: which pixels of a frame are opaque, and the rectangles that describe them.
//!
//! The overlay installs these rectangles as its input region, so only solid pixels take clicks. The scan runs
//! in *surface* space rather than native pixels — its cost follows the displayed size, and a step coarsens it
//! for detailed silhouettes — and `VISIBLE_ALPHA` is the threshold that decides what counts as solid.

use super::{Animation, Orientation};

/// Alpha at or above this counts as visible (the overlay's hit-test threshold).
pub const VISIBLE_ALPHA: u8 = 25;

impl Animation {
    /// Alpha (0-255) of a frame pixel; 0 when out of range.
    pub fn alpha_at(&self, frame: usize, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        let Some(frame) = self.frames.get(frame) else {
            return 0;
        };
        let index = (y as usize * self.width as usize + x as usize) * 4 + 3;
        frame.rgba.get(index).copied().unwrap_or(0)
    }

    /// Hit-test a point in surface-local coordinates, where the frame is
    /// stretched to `surface` and oriented by `orientation`. Used for
    /// drag-on-solid-pixel.
    pub fn alpha_hit(
        &self,
        frame: usize,
        surface: (u32, u32),
        orientation: Orientation,
        sx: f64,
        sy: f64,
        threshold: u8,
    ) -> bool {
        let (sw, sh) = surface;
        if sw == 0 || sh == 0 || self.width == 0 || self.height == 0 {
            return false;
        }
        if !sx.is_finite() || !sy.is_finite() {
            return false;
        }
        let (nu, nv) = orientation.display_to_native(sx / f64::from(sw), sy / f64::from(sh));
        self.alpha_at_unit(frame, nu, nv, threshold)
    }

    /// Opaque test at native unit coordinates (0..1, origin top-left).
    fn alpha_at_unit(&self, frame: usize, u: f64, v: f64, threshold: u8) -> bool {
        let fx = (u * f64::from(self.width)).floor();
        let fy = (v * f64::from(self.height)).floor();
        if fx < 0.0 || fy < 0.0 || fx >= f64::from(self.width) || fy >= f64::from(self.height) {
            return false;
        }
        self.alpha_at(frame, fx as u32, fy as u32) >= threshold
    }

    /// Rectangles `(x, y, w, h)` in surface-local coordinates covering the
    /// opaque pixels of `frame`. The scan runs in **surface space** (each cell is
    /// mapped back to a frame pixel), so its cost scales with the displayed size
    /// rather than the native frame, and rows with identical runs are merged.
    pub fn alpha_rects(
        &self,
        frame: usize,
        surface: (u32, u32),
        orientation: Orientation,
        threshold: u8,
    ) -> Vec<(i32, i32, i32, i32)> {
        self.alpha_rects_step(frame, surface, orientation, threshold, 1)
    }

    /// Like [`Self::alpha_rects`] but samples `step`-pixel surface cells and
    /// emits `step`-sized rectangles, to coarsen very detailed silhouettes.
    pub fn alpha_rects_step(
        &self,
        frame: usize,
        surface: (u32, u32),
        orientation: Orientation,
        threshold: u8,
        step: u32,
    ) -> Vec<(i32, i32, i32, i32)> {
        let (sw, sh) = surface;
        let step = step.clamp(1, 32);
        if sw == 0 || sh == 0 || self.width == 0 || self.height == 0 {
            return Vec::new();
        }
        if self.frames.get(frame).is_none() {
            return Vec::new();
        }

        let cols = sw.div_ceil(step);
        let rows = sh.div_ceil(step);
        let surface_size = (sw, sh);

        let mut rects: Vec<(i32, i32, i32, i32)> = Vec::new();
        let mut band: Vec<(u32, u32)> = Vec::new();
        // Two buffers, swapped rather than moved. `band = runs` used to hand the row's allocation to the band
        // and leave the next row to allocate again; swapping gives the row the buffer the previous band used,
        // so after the first row nothing in this loop allocates. `cols` is an upper bound on the runs a row can
        // produce, so the first row does not grow either.
        let mut runs: Vec<(u32, u32)> = Vec::with_capacity(cols as usize);
        let mut band_start = 0u32;
        let mut band_height = 0u32;

        for gy in 0..rows {
            let v = (f64::from((gy * step + step / 2).min(sh - 1)) + 0.5) / f64::from(sh);
            runs.clear();
            let mut start: Option<u32> = None;
            for gx in 0..cols {
                let u = (f64::from((gx * step + step / 2).min(sw - 1)) + 0.5) / f64::from(sw);
                let (nu, nv) = orientation.display_to_native(u, v);
                let solid = self.alpha_at_unit(frame, nu, nv, threshold);
                match (solid, start) {
                    (true, None) => start = Some(gx),
                    (false, Some(from)) => {
                        runs.push((from, gx));
                        start = None;
                    }
                    _ => {}
                }
            }
            if let Some(from) = start {
                runs.push((from, cols));
            }

            if gy == 0 {
                std::mem::swap(&mut band, &mut runs);
                band_start = 0;
                band_height = 1;
            } else if runs == band {
                band_height += 1;
            } else {
                emit_surface_band(
                    &band,
                    band_start,
                    band_height,
                    step,
                    surface_size,
                    &mut rects,
                );
                std::mem::swap(&mut band, &mut runs);
                band_start = gy;
                band_height = 1;
            }
        }
        emit_surface_band(
            &band,
            band_start,
            band_height,
            step,
            surface_size,
            &mut rects,
        );
        rects
    }
}

/// Append the rectangles of one merged surface-space row band.
fn emit_surface_band(
    runs: &[(u32, u32)],
    gy: u32,
    gh: u32,
    step: u32,
    surface: (u32, u32),
    rects: &mut Vec<(i32, i32, i32, i32)>,
) {
    let (sw, sh) = surface;
    for &(x0, x1) in runs {
        let left = x0 * step;
        let top = gy * step;
        rects.push((
            left as i32,
            top as i32,
            ((x1 * step).min(sw).saturating_sub(left)) as i32,
            (((gy + gh) * step).min(sh).saturating_sub(top)) as i32,
        ));
    }
}

/// Smallest rectangle containing all `rects`.
pub fn bounds_rect(rects: &[(i32, i32, i32, i32)]) -> Option<(i32, i32, i32, i32)> {
    let mut iter = rects.iter();
    let &(x, y, w, h) = iter.next()?;
    let (mut x0, mut y0, mut x1, mut y1) = (x, y, x + w, y + h);
    for &(x, y, w, h) in iter {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x + w);
        y1 = y1.max(y + h);
    }
    Some((x0, y0, x1 - x0, y1 - y0))
}
