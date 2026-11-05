//! The ink box: the union of the visible pixels across an animation's frames.
//!
//! Authors usually leave transparent padding around the art, and clamping against the whole canvas means the
//! art can never reach a screen edge. This box is what both the padded clamp and the auto-crop are built on,
//! and [`Animation::crop`] is the auto-crop: every frame is cut by the same box, so a frame that moves inside
//! the canvas keeps its motion.

use super::{Animation, Frame};

/// Union of the visible pixels across frames, in canvas coordinates (inclusive).
///
/// Authors usually leave transparent padding around the art, and the overlay
/// clamped against the whole canvas — so the art could not reach a screen edge.
/// This box is what both the padded clamp (A) and the auto-crop (C) are built on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InkBox {
    pub min_x: u32,
    pub min_y: u32,
    pub max_x: u32,
    pub max_y: u32,
}

impl Animation {
    /// Copy `bounds` (clamped to the canvas) out of every frame, keeping the frame
    /// count and delays. The transparent padding disappears, so the overlay's
    /// bounds, hit-testing space and GPU memory all shrink with it — and a frame
    /// that moves inside the canvas keeps its motion because every frame is cropped
    /// by the same box.
    pub fn crop(&self, bounds: InkBox) -> Animation {
        let min_x = bounds.min_x.min(self.width.saturating_sub(1));
        let min_y = bounds.min_y.min(self.height.saturating_sub(1));
        let max_x = bounds.max_x.clamp(min_x, self.width.saturating_sub(1));
        let max_y = bounds.max_y.clamp(min_y, self.height.saturating_sub(1));
        let width = max_x - min_x + 1;
        let height = max_y - min_y + 1;

        let frames = self
            .frames
            .iter()
            .map(|frame| {
                let mut rgba = Vec::with_capacity((width * height * 4) as usize);
                for y in min_y..=max_y {
                    let start = ((y * self.width + min_x) * 4) as usize;
                    rgba.extend_from_slice(&frame.rgba[start..start + (width * 4) as usize]);
                }
                Frame {
                    rgba,
                    delay_ms: frame.delay_ms,
                }
            })
            .collect();

        Animation {
            width,
            height,
            frames,
        }
    }
}
