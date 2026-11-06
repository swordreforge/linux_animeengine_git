//! How a frame is oriented on screen: quarter-turn rotation plus mirrors.
//!
//! The displayed image is the native image rotated clockwise by `quarter_turns * 90°`, then mirrored in
//! display space. [`Orientation::native_to_display`] and [`Orientation::display_to_native`] are inverses, and
//! exist because a pointer arrives in display coordinates and the frame's pixels are in native ones.

/// How a frame is oriented on screen: quarter-turn rotation plus mirrors.
///
/// The displayed image is the native image rotated clockwise by
/// `quarter_turns * 90°`, then mirrored in display space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Orientation {
    /// Clockwise quarter turns (0..=3).
    pub quarter_turns: u8,
    pub flip_h: bool,
    pub flip_v: bool,
}

impl Orientation {
    /// Build from a rotation in degrees (rounded to the nearest quarter turn)
    /// plus the mirror flags.
    pub fn from_degrees(rotation_degrees: i32, flip_h: bool, flip_v: bool) -> Self {
        let normalized = rotation_degrees.rem_euclid(360);
        let quarter_turns = ((normalized + 45) / 90).rem_euclid(4) as u8;
        Self {
            quarter_turns,
            flip_h,
            flip_v,
        }
    }

    /// Whether the displayed size swaps width and height.
    pub fn swaps_axes(&self) -> bool {
        self.quarter_turns % 2 == 1
    }

    /// Displayed size for a native size.
    pub fn size(&self, native: (u32, u32)) -> (u32, u32) {
        if self.swaps_axes() {
            (native.1, native.0)
        } else {
            native
        }
    }

    /// Rotation in degrees (0/90/180/270).
    pub fn degrees(&self) -> i32 {
        i32::from(self.quarter_turns) * 90
    }

    /// Native unit coordinates (origin top-left) -> display unit coordinates.
    pub fn native_to_display(&self, u: f64, v: f64) -> (f64, f64) {
        let (mut x, mut y) = match self.quarter_turns % 4 {
            0 => (u, v),
            1 => (1.0 - v, u),
            2 => (1.0 - u, 1.0 - v),
            _ => (v, 1.0 - u),
        };
        if self.flip_h {
            x = 1.0 - x;
        }
        if self.flip_v {
            y = 1.0 - y;
        }
        (x, y)
    }

    /// Display unit coordinates -> native unit coordinates (inverse of
    /// [`Self::native_to_display`]).
    pub fn display_to_native(&self, x: f64, y: f64) -> (f64, f64) {
        let (mut x, mut y) = (x, y);
        if self.flip_h {
            x = 1.0 - x;
        }
        if self.flip_v {
            y = 1.0 - y;
        }
        match self.quarter_turns % 4 {
            0 => (x, y),
            1 => (y, 1.0 - x),
            2 => (1.0 - x, 1.0 - y),
            _ => (1.0 - y, x),
        }
    }
}
