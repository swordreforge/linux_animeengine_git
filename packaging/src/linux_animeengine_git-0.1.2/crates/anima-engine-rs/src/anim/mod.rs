//! Animation decoding — the pure "media" layer.
//!
//! Every runtime animation (GIF, APNG, WebP) is normalized into the same shape:
//! a fully composited RGBA frame list with per-frame delays. The heavy lifting
//! (sub-frame offsets, blend and dispose) is done by the `image` crate's
//! `AnimationDecoder`, which already composites each frame onto the logical
//! canvas — the same result `nuevoMotor.iter_composited_frames` produced in the
//! Python build. Downstream logic (frame cache, playback, hit-testing) is
//! therefore format-agnostic.

mod orientation;

pub use orientation::Orientation;

mod region;

pub use region::{VISIBLE_ALPHA, bounds_rect};

mod decode;
mod ink;

pub use decode::{
    DecodeLimits, DecodeReport, FrameSource, ImageSource, MAX_DECODE_MB_ENV, MAX_FRAMES_ENV,
    decode, decode_path, decode_reporting, decode_with_limits, first_frame, limit_animation,
    limit_frames,
};
pub use ink::InkBox;

/// One composited frame: straight (non-premultiplied) RGBA8 plus display delay.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// `width * height * 4` bytes, row-major, straight RGBA.
    pub rgba: Vec<u8>,
    pub delay_ms: u32,
    /// One bit per pixel, set when alpha is at or above the overlay's hit threshold. Filled by
    /// [`Animation::shrink_to_alpha`] once every pixel reader is finished; empty means "read `rgba`",
    /// which is what a build without the `bc7` feature keeps doing.
    pub alpha_bits: Vec<u8>,
}

/// Delay used when a file stores none (0 ms): browsers clamp these so a GIF with
/// zero delays does not spin at maximum speed.
pub const DEFAULT_FRAME_MS: u32 = 33;

impl Frame {
    /// Delay to actually wait on this frame (0 ms becomes [`DEFAULT_FRAME_MS`]).
    pub fn effective_delay_ms(&self) -> u32 {
        if self.delay_ms == 0 {
            DEFAULT_FRAME_MS
        } else {
            self.delay_ms
        }
    }

    /// Extract the alpha channel (used for pixel hit-testing the overlay).
    pub fn alpha_mask(&self) -> Vec<u8> {
        self.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|px| px[3])
            .collect()
    }
}

/// A decoded animation: every frame shares the logical canvas size.
#[derive(Debug, Clone, PartialEq)]
pub struct Animation {
    pub width: u32,
    pub height: u32,
    pub frames: Vec<Frame>,
}

impl Animation {
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Replace each frame's RGBA with a one-bit-per-pixel alpha mask and return the bytes released.
    ///
    /// Only valid once every reader of the pixels is finished: the hit test and [`InkBox::scan`] must have
    /// run, and the BC7 encoder — the last reader — must have completed, after which `upload_frame` reads
    /// the encoded blocks instead. The mask keeps hit-testing exact at one bit per pixel, so a
    /// 1200x900x959 clip drops from 4.13 GiB of frames to 0.13 GiB.
    pub fn shrink_to_alpha(&mut self) -> u64 {
        let mut freed = 0;
        for frame in &mut self.frames {
            if !frame.alpha_bits.is_empty() {
                continue;
            }
            let mut bits = vec![0u8; (frame.rgba.len() / 4).div_ceil(8)];
            for (index, pixel) in frame.rgba.as_chunks::<4>().0.iter().enumerate() {
                if pixel[3] >= region::VISIBLE_ALPHA {
                    bits[index / 8] |= 1 << (index % 8);
                }
            }
            frame.alpha_bits = bits;
            freed += frame.rgba.len() as u64;
            frame.rgba = Vec::new();
        }
        freed
    }
}

impl Animation {
    /// Bytes of one composited frame at the canvas size.
    pub fn frame_bytes(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height) * 4
    }

    /// Rough memory footprint of the decoded frames.
    pub fn bytes(&self) -> u64 {
        self.frame_bytes() * self.frames.len() as u64
    }
}

impl InkBox {
    /// Scan `animation`, sampling every `step` frames (1 = every frame), for
    /// pixels with alpha >= [`VISIBLE_ALPHA`]. `None` when nothing is visible.
    pub fn scan(animation: &Animation, step: usize) -> Option<Self> {
        let step = step.max(1);
        let (width, height) = (animation.width, animation.height);
        let mut found: Option<Self> = None;

        let mut index = 0usize;
        while index < animation.frames.len() {
            let alpha = animation.frames[index].alpha_mask();
            for y in 0..height {
                for x in 0..width {
                    if alpha[(y * width + x) as usize] < VISIBLE_ALPHA {
                        continue;
                    }
                    found = Some(match found {
                        None => Self {
                            min_x: x,
                            min_y: y,
                            max_x: x,
                            max_y: y,
                        },
                        Some(mut bounds) => {
                            bounds.min_x = bounds.min_x.min(x);
                            bounds.min_y = bounds.min_y.min(y);
                            bounds.max_x = bounds.max_x.max(x);
                            bounds.max_y = bounds.max_y.max(y);
                            bounds
                        }
                    });
                }
            }
            index += step;
        }
        found
    }

    pub fn width(&self) -> u32 {
        self.max_x - self.min_x + 1
    }

    pub fn height(&self) -> u32 {
        self.max_y - self.min_y + 1
    }

    /// Transparent padding around the box: `(left, top, right, bottom)`.
    pub fn padding(&self, canvas: (u32, u32)) -> (u32, u32, u32, u32) {
        (
            self.min_x,
            self.min_y,
            canvas.0.saturating_sub(1 + self.max_x),
            canvas.1.saturating_sub(1 + self.max_y),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::path::PathBuf;

    use crate::{Error, Result};

    use super::*;

    fn decode_bytes(bytes: &[u8], name: &str) -> Result<Animation> {
        let dir = tempfile::tempdir().unwrap();
        let path: PathBuf = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        decode_path(&path)
    }

    #[test]
    fn first_frame_returns_one_frame_of_a_still() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("still.png");
        let mut source = image::RgbaImage::new(3, 2);
        source.put_pixel(0, 0, image::Rgba([10, 20, 30, 255]));
        source.put_pixel(2, 1, image::Rgba([1, 2, 3, 4]));
        source.save(&path).unwrap();

        let (width, height, rgba) = first_frame(&path).unwrap();
        assert_eq!((width, height), (3, 2));
        assert_eq!(&rgba[0..4], &[10, 20, 30, 255]);
        // Pixel (2, 1) is the sixth of six.
        assert_eq!(&rgba[20..24], &[1, 2, 3, 4]);
        assert_eq!(rgba.len(), 3 * 2 * 4);
    }

    #[test]
    fn first_frame_says_so_when_there_is_nothing_to_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.png");
        assert!(first_frame(&path).is_err());
    }

    fn pixel(anim: &Animation, frame: usize, x: usize, y: usize) -> [u8; 4] {
        let off = (y * anim.width as usize + x) * 4;
        let rgba = &anim.frames[frame].rgba;
        [rgba[off], rgba[off + 1], rgba[off + 2], rgba[off + 3]]
    }

    // ---- GIF fixtures (built with the `gif` crate) ----

    const GIF_PALETTE: [u8; 12] = [0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255];

    fn gif_frame(
        w: u16,
        h: u16,
        left: u16,
        top: u16,
        idx: u8,
        dispose: gif::DisposalMethod,
        delay: u16,
    ) -> gif::Frame<'static> {
        gif::Frame {
            width: w,
            height: h,
            left,
            top,
            delay,
            dispose,
            buffer: Cow::Owned(vec![idx; (w as usize) * (h as usize)]),
            ..gif::Frame::default()
        }
    }

    fn encode_gif(dim: (u16, u16), frames: &[gif::Frame]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = gif::Encoder::new(&mut out, dim.0, dim.1, &GIF_PALETTE).unwrap();
            for frame in frames {
                enc.write_frame(frame).unwrap();
            }
        }
        out
    }

    fn plain_frame(delay_ms: u32) -> Frame {
        Frame {
            alpha_bits: Vec::new(),
            rgba: vec![0, 0, 0, 255],
            delay_ms,
        }
    }

    /// A canvas with the visible square `[min..=max]²` and clear padding around it.
    fn padded_animation(size: u32, min: u32, max: u32) -> Animation {
        let mut rgba = vec![0u8; (size * size * 4) as usize];
        for y in min..=max {
            for x in min..=max {
                let index = ((y * size + x) * 4) as usize + 3;
                rgba[index] = 255;
            }
        }
        Animation {
            width: size,
            height: size,
            frames: vec![
                Frame {
                    rgba,
                    delay_ms: 30,
                    alpha_bits: Vec::new(),
                },
                Frame {
                    alpha_bits: Vec::new(),
                    rgba: vec![0u8; (size * size * 4) as usize],
                    delay_ms: 70,
                },
            ],
        }
    }

    #[test]
    fn ink_box_ignores_transparent_padding() {
        let animation = padded_animation(8, 2, 5);
        let ink = InkBox::scan(&animation, 1).unwrap();
        assert_eq!((ink.min_x, ink.min_y, ink.max_x, ink.max_y), (2, 2, 5, 5));
        assert_eq!((ink.width(), ink.height()), (4, 4));
        assert_eq!(ink.padding((8, 8)), (2, 2, 2, 2));

        let cropped = animation.crop(ink);
        assert_eq!((cropped.width, cropped.height), (4, 4));
        assert_eq!(cropped.frames.len(), 2);
        assert_eq!(cropped.frames[1].delay_ms, 70);
        assert_eq!(cropped.frames[0].rgba.len(), 4 * 4 * 4);
        assert!(
            cropped.frames[0]
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|px| px[3] == 255),
            "the crop must keep only the visible square"
        );
        assert!(
            cropped.frames[1].rgba.iter().all(|&byte| byte == 0),
            "an empty frame stays empty, it does not become opaque"
        );
    }

    #[test]
    fn ink_box_unions_frames_and_handles_blanks() {
        let mut animation = padded_animation(8, 2, 5);
        animation.frames.push(Frame {
            alpha_bits: Vec::new(),
            rgba: {
                let mut rgba = vec![0u8; 8 * 8 * 4];
                rgba[((6 * 8 + 6) * 4) as usize + 3] = 255;
                rgba
            },
            delay_ms: 10,
        });
        let ink = InkBox::scan(&animation, 1).unwrap();
        assert_eq!((ink.max_x, ink.max_y), (6, 6));

        let blank = Animation {
            width: 4,
            height: 4,
            frames: vec![Frame {
                alpha_bits: Vec::new(),
                rgba: vec![0u8; 4 * 4 * 4],
                delay_ms: 10,
            }],
        };
        assert_eq!(InkBox::scan(&blank, 1), None);
    }

    #[test]
    fn crop_is_a_single_row_window() {
        let mut rgba = vec![0u8; 3 * 4];
        rgba[4 + 3] = 255;
        let animation = Animation {
            width: 3,
            height: 1,
            frames: vec![Frame {
                rgba,
                delay_ms: 5,
                alpha_bits: Vec::new(),
            }],
        };
        let ink = InkBox::scan(&animation, 1).unwrap();
        assert_eq!(ink.padding((3, 1)), (1, 0, 1, 0));
        let cropped = animation.crop(ink);
        assert_eq!((cropped.width, cropped.height), (1, 1));
        assert_eq!(cropped.frames[0].rgba, vec![0, 0, 0, 255]);
    }

    #[test]
    fn zero_delay_frames_fall_back_to_a_visible_delay() {
        assert_eq!(plain_frame(0).effective_delay_ms(), DEFAULT_FRAME_MS);
        assert_eq!(plain_frame(50).effective_delay_ms(), 50);
    }

    #[test]
    fn limits_keep_small_animations_intact() {
        let frames: Vec<Frame> = (0..4).map(|_| plain_frame(50)).collect();
        let (kept, report) = limit_frames(frames, DecodeLimits::unbounded());
        assert_eq!(kept.len(), 4);
        assert_eq!(report.stride, 1);
        assert!(!report.limited());
        assert_eq!(kept.iter().map(|f| f.delay_ms).sum::<u32>(), 200);
    }

    #[test]
    fn limits_decimate_and_preserve_total_delay() {
        let frames: Vec<Frame> = (0..8).map(|_| plain_frame(30)).collect();
        // 4 bytes per frame: a 16-byte budget keeps 4 of the 8.
        let limits = DecodeLimits {
            max_bytes: 16,
            max_frames: usize::MAX,
        };
        let (kept, report) = limit_frames(frames, limits);
        assert_eq!(kept.len(), 4);
        assert_eq!(report.stride, 2);
        assert_eq!(report.source_frames, 8);
        assert!(report.limited());
        assert!(report.bytes <= 16);
        assert_eq!(kept.iter().map(|f| f.delay_ms).sum::<u32>(), 240);
        assert!(report.note().contains("1 of every 2 frames"));
    }

    #[test]
    fn limits_respect_the_frame_cap() {
        let frames: Vec<Frame> = (0..8).map(|_| plain_frame(10)).collect();
        let limits = DecodeLimits {
            max_bytes: u64::MAX,
            max_frames: 3,
        };
        let (kept, report) = limit_frames(frames, limits);
        assert!(kept.len() <= 3);
        assert_eq!(kept.iter().map(|f| f.delay_ms).sum::<u32>(), 80);
        assert!(report.limited());
    }

    #[test]
    fn decoding_a_gif_respects_the_byte_limit() {
        let frames: Vec<gif::Frame<'static>> = (0..6)
            .map(|i| {
                gif_frame(
                    2,
                    2,
                    0,
                    0,
                    if i % 2 == 0 { 1 } else { 2 },
                    gif::DisposalMethod::Keep,
                    5,
                )
            })
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("six.gif");
        std::fs::write(&path, encode_gif((2, 2), &frames)).unwrap();

        let (animation, report) = decode_reporting(&path, DecodeLimits::unbounded()).unwrap();
        assert_eq!(animation.frame_count(), 6);
        assert!(!report.limited());
        assert_eq!(animation.frames[0].delay_ms, 50);

        // 2x2 RGBA = 16 bytes per frame; this budget holds three of them.
        let limits = DecodeLimits {
            max_bytes: 48,
            max_frames: usize::MAX,
        };
        let (animation, report) = decode_reporting(&path, limits).unwrap();
        assert_eq!(animation.frame_count(), 3);
        assert_eq!(report.stride, 2);
        assert_eq!(
            animation.frames.iter().map(|f| f.delay_ms).sum::<u32>(),
            300
        );
    }

    #[test]
    fn gif_single_frame_is_solid_and_timed() {
        let data = encode_gif(
            (2, 2),
            &[gif_frame(2, 2, 0, 0, 1, gif::DisposalMethod::Keep, 5)],
        );
        let anim = decode_bytes(&data, "one.gif").unwrap();
        assert_eq!(anim.dimensions(), (2, 2));
        assert_eq!(anim.frame_count(), 1);
        assert_eq!(anim.frames[0].delay_ms, 50);
        for px in anim.frames[0].rgba.as_chunks::<4>().0 {
            assert_eq!(px, &[255, 0, 0, 255]);
        }
    }

    #[test]
    fn gif_background_disposal_clears_canvas() {
        let f0 = gif_frame(3, 3, 0, 0, 1, gif::DisposalMethod::Background, 10);
        let f1 = gif_frame(1, 1, 1, 1, 2, gif::DisposalMethod::Keep, 7);
        let anim = decode_bytes(&encode_gif((3, 3), &[f0, f1]), "bg.gif").unwrap();

        assert_eq!(anim.frame_count(), 2);
        assert_eq!(pixel(&anim, 0, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&anim, 1, 1, 1), [0, 255, 0, 255]);
        assert_eq!(pixel(&anim, 1, 0, 0), [0, 0, 0, 0]);
        assert_eq!(anim.frames[1].delay_ms, 70);
    }

    #[test]
    fn gif_previous_disposal_restores_previous_canvas() {
        let f0 = gif_frame(3, 3, 0, 0, 1, gif::DisposalMethod::Keep, 10);
        let f1 = gif_frame(1, 1, 0, 0, 2, gif::DisposalMethod::Previous, 10);
        let f2 = gif_frame(1, 1, 2, 2, 3, gif::DisposalMethod::Keep, 10);
        let anim = decode_bytes(&encode_gif((3, 3), &[f0, f1, f2]), "prev.gif").unwrap();

        assert_eq!(anim.frame_count(), 3);
        assert_eq!(pixel(&anim, 2, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&anim, 2, 1, 1), [255, 0, 0, 255]);
        assert_eq!(pixel(&anim, 2, 2, 2), [0, 0, 255, 255]);
    }

    #[test]
    fn gif_alpha_mask_reflects_transparency() {
        let f0 = gif_frame(2, 1, 0, 0, 1, gif::DisposalMethod::Background, 1);
        let f1 = gif_frame(1, 1, 0, 0, 2, gif::DisposalMethod::Keep, 1);
        let anim = decode_bytes(&encode_gif((2, 1), &[f0, f1]), "mask.gif").unwrap();
        assert_eq!(anim.frames[1].alpha_mask(), vec![255, 0]);
    }

    // ---- PNG / APNG fixtures (built with the `png` crate) ----

    struct ApngFrame {
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        delay: (u16, u16),
        blend: png::BlendOp,
        dispose: png::DisposeOp,
        rgba: Vec<u8>,
    }

    fn solid(w: u32, h: u32, color: [u8; 4]) -> Vec<u8> {
        color
            .iter()
            .copied()
            .cycle()
            .take((w * h * 4) as usize)
            .collect()
    }

    fn encode_png(size: (u32, u32), rgba: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, size.0, size.1);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(rgba).unwrap();
        }
        out
    }

    fn encode_apng(size: (u32, u32), frames: &[ApngFrame]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, size.0, size.1);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.set_animated(frames.len() as u32, 0).unwrap();
            let mut writer = enc.write_header().unwrap();
            for f in frames {
                writer.set_frame_dimension(f.w, f.h).unwrap();
                writer.set_frame_position(f.x, f.y).unwrap();
                writer.set_frame_delay(f.delay.0, f.delay.1).unwrap();
                writer.set_blend_op(f.blend).unwrap();
                writer.set_dispose_op(f.dispose).unwrap();
                writer.write_image_data(&f.rgba).unwrap();
            }
        }
        out
    }

    fn apng_full(w: u32, h: u32, color: [u8; 4], dispose: png::DisposeOp) -> ApngFrame {
        ApngFrame {
            x: 0,
            y: 0,
            w,
            h,
            delay: (1, 10),
            blend: png::BlendOp::Source,
            dispose,
            rgba: solid(w, h, color),
        }
    }

    #[test]
    fn static_png_is_one_frame() {
        let data = encode_png((2, 2), &solid(2, 2, [255, 0, 0, 255]));
        let anim = decode_bytes(&data, "still.png").unwrap();
        assert_eq!(anim.dimensions(), (2, 2));
        assert_eq!(anim.frame_count(), 1);
        assert_eq!(pixel(&anim, 0, 0, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn apng_two_full_frames_and_delay() {
        let frames = [
            apng_full(2, 2, [255, 0, 0, 255], png::DisposeOp::None),
            apng_full(2, 2, [0, 255, 0, 255], png::DisposeOp::None),
        ];
        let anim = decode_bytes(&encode_apng((2, 2), &frames), "two.apng").unwrap();
        assert_eq!(anim.frame_count(), 2);
        assert_eq!(anim.frames[0].delay_ms, 100);
        assert_eq!(pixel(&anim, 0, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&anim, 1, 1, 1), [0, 255, 0, 255]);
    }

    #[test]
    fn apng_over_blend_combines_with_previous() {
        let frame0 = apng_full(1, 1, [255, 0, 0, 255], png::DisposeOp::None);
        let mut frame1 = apng_full(1, 1, [0, 0, 255, 128], png::DisposeOp::None);
        frame1.blend = png::BlendOp::Over;
        let anim = decode_bytes(&encode_apng((1, 1), &[frame0, frame1]), "over.apng").unwrap();

        let px = pixel(&anim, 1, 0, 0);
        // image's fixed-point blend can land one off from 255.
        assert!(px[3] >= 254, "alpha was {}", px[3]);
        assert_eq!(px[1], 0);
        assert!((120..=136).contains(&px[0]), "red channel was {}", px[0]);
        assert!((120..=136).contains(&px[2]), "blue channel was {}", px[2]);
    }

    #[test]
    fn apng_background_dispose_clears_region() {
        let frame0 = apng_full(3, 3, [255, 0, 0, 255], png::DisposeOp::Background);
        let frame1 = ApngFrame {
            x: 1,
            y: 1,
            w: 1,
            h: 1,
            delay: (1, 10),
            blend: png::BlendOp::Source,
            dispose: png::DisposeOp::None,
            rgba: solid(1, 1, [0, 255, 0, 255]),
        };
        let anim = decode_bytes(&encode_apng((3, 3), &[frame0, frame1]), "bgd.apng").unwrap();

        assert_eq!(pixel(&anim, 1, 1, 1), [0, 255, 0, 255]);
        assert_eq!(pixel(&anim, 1, 0, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(&anim, 1, 2, 2), [0, 0, 0, 0]);
    }

    #[test]
    fn apng_previous_dispose_restores_region() {
        let frame0 = apng_full(3, 3, [255, 0, 0, 255], png::DisposeOp::None);
        let frame1 = ApngFrame {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
            delay: (1, 10),
            blend: png::BlendOp::Source,
            dispose: png::DisposeOp::Previous,
            rgba: solid(1, 1, [0, 255, 0, 255]),
        };
        let frame2 = ApngFrame {
            x: 2,
            y: 2,
            w: 1,
            h: 1,
            delay: (1, 10),
            blend: png::BlendOp::Source,
            dispose: png::DisposeOp::None,
            rgba: solid(1, 1, [0, 0, 255, 255]),
        };
        let anim = decode_bytes(
            &encode_apng((3, 3), &[frame0, frame1, frame2]),
            "prevd.apng",
        )
        .unwrap();

        assert_eq!(anim.frame_count(), 3);
        assert_eq!(pixel(&anim, 2, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&anim, 2, 2, 2), [0, 0, 255, 255]);
    }

    // ---- WebP fixtures ----

    /// 4x4, two frames (red then green), 40 ms each, no alpha.
    /// Generated with `ffmpeg -c:v libwebp_anim -lossless 1`.
    const ANIMATED_WEBP: &[u8] = &[
        82, 73, 70, 70, 132, 0, 0, 0, 87, 69, 66, 80, 86, 80, 56, 88, 10, 0, 0, 0, 2, 0, 0, 0, 3,
        0, 0, 3, 0, 0, 65, 78, 73, 77, 6, 0, 0, 0, 255, 255, 255, 255, 0, 0, 65, 78, 77, 70, 40, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 3, 0, 0, 40, 0, 0, 2, 86, 80, 56, 76, 15, 0, 0, 0, 47, 3,
        192, 0, 0, 7, 16, 253, 143, 254, 7, 34, 162, 255, 1, 0, 65, 78, 77, 70, 40, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 3, 0, 0, 3, 0, 0, 40, 0, 0, 0, 86, 80, 56, 76, 15, 0, 0, 0, 47, 3, 192, 0, 0,
        7, 208, 255, 136, 254, 7, 34, 162, 255, 1, 0,
    ];

    fn encode_webp(data: &[u8], w: u32, h: u32, color: image_webp::ColorType) -> Vec<u8> {
        let mut out = Vec::new();
        image_webp::WebPEncoder::new(&mut out)
            .encode(data, w, h, color)
            .unwrap();
        out
    }

    #[test]
    fn animated_webp_frames_and_pixels() {
        let anim = decode_bytes(ANIMATED_WEBP, "anim.webp").unwrap();
        assert_eq!(anim.dimensions(), (4, 4));
        assert_eq!(anim.frame_count(), 2);

        let red = pixel(&anim, 0, 0, 0);
        assert!(
            red[0] >= 250 && red[1] <= 8 && red[2] <= 8 && red[3] == 255,
            "red={red:?}"
        );
        let green = pixel(&anim, 1, 0, 0);
        assert!(
            green[1] >= 250 && green[0] <= 8 && green[2] <= 8 && green[3] == 255,
            "green={green:?}"
        );
        assert!(anim.frames[0].delay_ms > 0);
        assert!(anim.frames[1].delay_ms > 0);
    }

    #[test]
    fn static_opaque_webp_expands_to_rgba() {
        let rgb = [255u8, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0];
        let data = encode_webp(&rgb, 2, 2, image_webp::ColorType::Rgb8);
        let anim = decode_bytes(&data, "still.webp").unwrap();
        assert_eq!(anim.dimensions(), (2, 2));
        assert_eq!(anim.frame_count(), 1);
        assert_eq!(pixel(&anim, 0, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&anim, 0, 1, 0), [0, 255, 0, 255]);
        assert_eq!(pixel(&anim, 0, 0, 1), [0, 0, 255, 255]);
        assert_eq!(pixel(&anim, 0, 1, 1), [0, 0, 0, 255]);
    }

    #[test]
    fn static_alpha_webp_keeps_alpha() {
        let rgba = [255u8, 0, 0, 0, 0, 255, 0, 128, 0, 0, 255, 255, 0, 0, 0, 0];
        let data = encode_webp(&rgba, 2, 2, image_webp::ColorType::Rgba8);
        let anim = decode_bytes(&data, "alpha.webp").unwrap();
        assert_eq!(anim.frame_count(), 1);
        assert_eq!(pixel(&anim, 0, 0, 0)[3], 0);
        assert_eq!(pixel(&anim, 0, 1, 0)[3], 128);
        assert_eq!(pixel(&anim, 0, 0, 1)[3], 255);
        assert_eq!(pixel(&anim, 0, 1, 1)[3], 0);
    }

    #[test]
    fn rejects_unknown_content() {
        let err = decode_bytes(b"not an image at all", "junk.bin").unwrap_err();
        assert!(matches!(err, Error::Decode(_)));
    }

    #[test]
    fn alpha_hit_maps_surface_to_frame() {
        let anim = Animation {
            width: 2,
            height: 1,
            frames: vec![Frame {
                alpha_bits: Vec::new(),
                rgba: vec![255, 0, 0, 255, 0, 255, 0, 0],
                delay_ms: 0,
            }],
        };

        let id = Orientation::default();

        // surface == frame size
        assert!(anim.alpha_hit(0, (2, 1), id, 0.5, 0.5, 25));
        assert!(!anim.alpha_hit(0, (2, 1), id, 1.5, 0.5, 25));

        // surface stretched 2x: x in [2, 4) maps to frame column 1 (alpha 0)
        assert!(anim.alpha_hit(0, (4, 2), id, 0.5, 0.5, 25));
        assert!(!anim.alpha_hit(0, (4, 2), id, 3.0, 0.5, 25));

        // degenerate / invalid inputs
        assert!(!anim.alpha_hit(0, (2, 1), id, -0.1, 0.0, 25));
        assert!(!anim.alpha_hit(0, (2, 1), id, f64::NAN, 0.0, 25));
        assert!(!anim.alpha_hit(0, (0, 0), id, 0.0, 0.0, 25));
        assert!(!anim.alpha_hit(1, (2, 1), id, 0.5, 0.5, 25));

        assert_eq!(anim.alpha_at(0, 0, 0), 255);
        assert_eq!(anim.alpha_at(0, 5, 5), 0);
    }

    #[test]
    fn alpha_rects_merges_and_scales() {
        let id = Orientation::default();

        // solid 3x2 block → one rectangle
        let solid = Animation {
            width: 3,
            height: 2,
            frames: vec![Frame {
                alpha_bits: Vec::new(),
                rgba: vec![255; 3 * 2 * 4],
                delay_ms: 0,
            }],
        };
        assert_eq!(solid.alpha_rects(0, (3, 2), id, 25), vec![(0, 0, 3, 2)]);

        // diagonal 2x2 → two single-pixel rectangles
        let diagonal = Animation {
            width: 2,
            height: 2,
            frames: vec![Frame {
                alpha_bits: Vec::new(),
                rgba: vec![255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 255],
                delay_ms: 0,
            }],
        };
        assert_eq!(
            diagonal.alpha_rects(0, (2, 2), id, 25),
            vec![(0, 0, 1, 1), (1, 1, 1, 1)]
        );

        // fully transparent → no rectangles
        let empty = Animation {
            width: 2,
            height: 2,
            frames: vec![Frame {
                alpha_bits: Vec::new(),
                rgba: vec![0; 2 * 2 * 4],
                delay_ms: 0,
            }],
        };
        assert!(empty.alpha_rects(0, (2, 2), id, 25).is_empty());

        // surface stretched 2x
        let half = Animation {
            width: 2,
            height: 1,
            frames: vec![Frame {
                alpha_bits: Vec::new(),
                rgba: vec![255, 0, 0, 255, 0, 255, 0, 0],
                delay_ms: 0,
            }],
        };
        assert_eq!(half.alpha_rects(0, (4, 2), id, 25), vec![(0, 0, 2, 2)]);

        // below threshold → transparent
        let faint = Animation {
            width: 1,
            height: 1,
            frames: vec![Frame {
                alpha_bits: Vec::new(),
                rgba: vec![0, 0, 0, 10],
                delay_ms: 0,
            }],
        };
        assert!(faint.alpha_rects(0, (1, 1), id, 25).is_empty());
    }

    #[test]
    fn bounds_rect_wraps_all() {
        assert_eq!(bounds_rect(&[]), None);
        assert_eq!(
            bounds_rect(&[(2, 3, 1, 1), (0, 0, 2, 2)]),
            Some((0, 0, 3, 4))
        );
    }

    #[test]
    fn orientation_rotation_and_mirrors() {
        let rot90 = Orientation::from_degrees(90, false, false);
        assert!(rot90.swaps_axes());
        assert_eq!(rot90.size((2, 3)), (3, 2));
        assert_eq!(
            Orientation::from_degrees(-90, false, false).quarter_turns,
            3
        );
        assert_eq!(
            Orientation::from_degrees(450, false, false).quarter_turns,
            1
        );
        assert_eq!(Orientation::default().size((2, 3)), (2, 3));

        // display_to_native is the inverse of native_to_display
        for (u, v) in [(0.25, 0.75), (0.0, 0.0), (1.0, 1.0), (0.5, 0.5)] {
            let (x, y) = rot90.native_to_display(u, v);
            let (u2, v2) = rot90.display_to_native(x, y);
            assert!((u - u2).abs() < 1e-9 && (v - v2).abs() < 1e-9);
        }

        // mirror only
        let mirrored = Orientation::from_degrees(0, true, false);
        assert!((mirrored.native_to_display(0.25, 0.5).0 - 0.75).abs() < 1e-9);

        // 2x1 frame, only pixel (0,0) opaque
        let anim = Animation {
            width: 2,
            height: 1,
            frames: vec![Frame {
                alpha_bits: Vec::new(),
                rgba: vec![255, 0, 0, 255, 0, 0, 0, 0],
                delay_ms: 0,
            }],
        };

        // 90° CW displays as 1x2 with the opaque pixel at the top
        assert!(anim.alpha_hit(0, (1, 2), rot90, 0.5, 0.5, 25));
        assert!(!anim.alpha_hit(0, (1, 2), rot90, 0.5, 1.5, 25));
        assert_eq!(anim.alpha_rects(0, (1, 2), rot90, 25), vec![(0, 0, 1, 1)]);

        // a horizontal mirror moves that pixel to the right column
        assert_eq!(
            anim.alpha_rects(0, (2, 1), Orientation::from_degrees(0, true, false), 25),
            vec![(1, 0, 1, 1)]
        );
    }
}
