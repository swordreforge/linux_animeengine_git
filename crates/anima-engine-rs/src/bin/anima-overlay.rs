//! `anima-overlay` — minimal always-on-top overlay scaffold.
//!
//! Wayland layer-shell (`zwlr_layer_shell_v1`, **Overlay** layer) + a `wgpu`
//! transparent surface. It plays an animation sequence honouring each frame's
//! `delay_ms`, using [`FrameCache`] so a file is only decoded once and then
//! loaded from the cache on later runs.
//!
//! Interaction: the left mouse button only starts a drag when it lands on an
//! **opaque** pixel of the frame currently on screen (per-pixel alpha hit-test,
//! matching the Python renderer's `_pixel_opaque_at`). Dragging updates the
//! layer-shell margins.
//!
//! A StatusNotifierItem tray and `xdg-desktop-portal` GlobalShortcuts control
//! the overlay from outside (input mode, reset, quit) by writing
//! [`anima_engine_rs::control::ControlState`]; the render loop polls it.
//!
//! Input modes: `Opaque` (default) installs a `wl_region` built from the
//! visible frame's alpha, so only solid pixels grab clicks; `PassThrough` makes
//! the whole surface click-through; `Full` keeps the whole surface interactive.
//!
//! Usage:
//! ```text
//! cargo run --features overlay --bin anima-overlay -- [animation.gif|.apng|.webp]
//! ```
//!
//! Without a path a generated multi-frame test pattern is shown. The process
//! stays alive until closed by the compositor or Ctrl+C.

// The pipelines live in their own file: `main` is 852 lines and this is the cleanest piece of it.
#[path = "anima-overlay/config.rs"]
mod config;
#[path = "anima-overlay/frames.rs"]
mod frames;
use config::Config;
#[path = "anima-overlay/gpu.rs"]
mod gpu;
use gpu::Gpu;
#[path = "anima-overlay/pipelines.rs"]
mod pipelines;
#[path = "anima-overlay/wayland.rs"]
mod wayland;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use wayland::Wayland;

use anima_engine_rs::anim::{Animation, DecodeLimits, Frame, Orientation};
use anima_engine_rs::cache::{Blocks, FrameCache};
use anima_engine_rs::control::{ControlState, InputMode, MAX_SCALE_PERCENT, MIN_SCALE_PERCENT};
use anima_engine_rs::log;
use anima_engine_rs::settings::{ANIM_META_FILE, AnimMeta, ConfigFile};
use anima_engine_rs::state::SecondaryState;
use anima_engine_rs::workshop::WorkshopWatcher;

use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::globals::{GlobalListContents, registry_queue_init};
use smithay_client_toolkit::reexports::client::protocol::{
    wl_compositor, wl_output, wl_pointer, wl_region, wl_registry, wl_seat, wl_surface,
};
use smithay_client_toolkit::reexports::client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
};
use smithay_client_toolkit::reexports::protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1, zwlr_layer_surface_v1,
};

use wgpu::rwh::{RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle};
use wgpu::{
    BindGroupDescriptor, BindGroupEntry, BindGroupLayoutDescriptor, BindGroupLayoutEntry,
    BindingResource, BindingType, BlendState, BufferBinding, BufferBindingType, BufferDescriptor,
    BufferUsages, ColorTargetState, ColorWrites, CommandEncoderDescriptor, CompositeAlphaMode,
    Extent3d, FragmentState, LoadOp, MultisampleState, Operations, Origin3d,
    PipelineLayoutDescriptor, PresentMode, PrimitiveState, RenderPassColorAttachment,
    RenderPassDescriptor, RenderPipelineDescriptor, SamplerBindingType, SamplerDescriptor,
    ShaderModuleDescriptor, ShaderSource, ShaderStages, StoreOp, SurfaceConfiguration,
    SurfaceTargetUnsafe, TexelCopyBufferLayout, TexelCopyTextureInfo, TextureAspect,
    TextureDescriptor, TextureDimension, TextureFormat, TextureSampleType, TextureUsages,
    TextureViewDescriptor, TextureViewDimension, VertexState,
};

/// Alpha value (0-255) at or above which a pixel counts as "solid" for drag.
const ALPHA_HIT_THRESHOLD: u8 = 25;

/// Fallback window position when an instance has no saved geometry.
const DEFAULT_POS: (i32, i32) = (100, 100);

/// Linux evdev `BTN_LEFT`.
const BTN_LEFT: u32 = 0x110;

/// Linux evdev `BTN_RIGHT`.
const BTN_RIGHT: u32 = 0x111;

const SHADER: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    var uvs = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );
    var out: VsOut;
    out.pos = vec4<f32>(positions[vi], 0.0, 1.0);
    out.uv = uvs[vi];
    return out;
}

struct FrameCtl {
    layer: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
    uv_mat: vec4<f32>,
    uv_off: vec2<f32>,
    _pad3: vec2<f32>,
};

@group(0) @binding(0) var src: texture_2d_array<f32>;
@group(0) @binding(1) var smp: sampler;
@group(0) @binding(2) var<uniform> ctl: FrameCtl;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Display UV -> native UV, honouring rotation and mirrors.
    let uv = ctl.uv_mat.xy * in.uv.x + ctl.uv_mat.zw * in.uv.y + ctl.uv_off;
    let c = textureSample(src, smp, uv, i32(ctl.layer));
    // The surface uses premultiplied alpha; premultiply the straight RGBA frame.
    return vec4<f32>(c.rgb * c.a, c.a);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FrameCtl {
    layer: u32,
    _pad: [u32; 3],
    uv_mat: [f32; 4],
    uv_off: [f32; 2],
    _pad2: [f32; 2],
}

/// A menu vertex: NDC position + premultiplied RGBA.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MenuVertex {
    pos: [f32; 2],
    color: [f32; 4],
}

const MENU_SHADER: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(@location(0) pos: vec2<f32>, @location(1) color: vec4<f32>) -> VsOut {
    var out: VsOut;
    out.pos = vec4<f32>(pos, 0.0, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}
"#;

/// In-surface context menu, drawn with wgpu near the click.
///
/// Like live2d-rs's toolbar this is a column of geometric icons (no font, no
/// texture, no xdg_popup): right-click opens it, left-click picks an item, and
/// clicking elsewhere or leaving the surface closes it.
mod menu {
    use super::{InputMode, MenuVertex};

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Action {
        CycleInput,
        ScaleDown,
        ScaleUp,
        ToggleMirror,
        ToggleLock,
        Reset,
        Quit,
    }

    const ITEMS: [Action; 7] = [
        Action::CycleInput,
        Action::ScaleDown,
        Action::ScaleUp,
        Action::ToggleMirror,
        Action::ToggleLock,
        Action::Reset,
        Action::Quit,
    ];

    #[derive(Clone, Copy, Debug)]
    struct Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    }

    impl Rect {
        fn contains(&self, x: f32, y: f32) -> bool {
            x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
        }
    }

    fn empty_rect() -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        }
    }

    pub struct Menu {
        pub open: bool,
        pub hover: Option<usize>,
        panel: Rect,
        buttons: [Rect; 7],
    }

    impl Menu {
        pub fn new() -> Self {
            Self {
                open: false,
                hover: None,
                panel: empty_rect(),
                buttons: [empty_rect(); 7],
            }
        }

        /// Open near the click, clamped inside the surface.
        pub fn open_at(&mut self, surface: (u32, u32), x: f64, y: f64) {
            let sw = surface.0 as f32;
            let sh = surface.1 as f32;
            let pad = 3.0;
            let bw = 48.0_f32.min((sw - 2.0 * pad).max(16.0));
            let bh = ((sh - 2.0 * pad) / ITEMS.len() as f32).clamp(4.0, 32.0);
            let total = bh * ITEMS.len() as f32;
            let ox = (x as f32 + 8.0).clamp(pad, (sw - bw - pad).max(pad));
            let oy = (y as f32 + 8.0).clamp(pad, (sh - total - pad).max(pad));
            self.panel = Rect {
                x: ox - pad,
                y: oy - pad,
                w: bw + 2.0 * pad,
                h: total + 2.0 * pad,
            };
            for (i, slot) in self.buttons.iter_mut().enumerate() {
                *slot = Rect {
                    x: ox,
                    y: oy + i as f32 * bh,
                    w: bw,
                    h: bh,
                };
            }
            self.open = true;
            self.hover = None;
        }

        pub fn close(&mut self) {
            self.open = false;
            self.hover = None;
        }

        pub fn hit(&self, x: f64, y: f64) -> Option<Action> {
            if !self.open {
                return None;
            }
            let (x, y) = (x as f32, y as f32);
            self.buttons
                .iter()
                .position(|b| b.contains(x, y))
                .map(|i| ITEMS[i])
        }

        /// Update the hovered row; returns whether it changed.
        pub fn update_hover(&mut self, x: f64, y: f64) -> bool {
            if !self.open {
                return false;
            }
            let (x, y) = (x as f32, y as f32);
            let hover = self.buttons.iter().position(|b| b.contains(x, y));
            if hover != self.hover {
                self.hover = hover;
                true
            } else {
                false
            }
        }

        /// Triangle list for the menu (premultiplied colours, NDC).
        pub fn vertices(&self, surface: (u32, u32), mode: InputMode) -> Vec<MenuVertex> {
            let mut out = Vec::with_capacity(768);
            if !self.open {
                return out;
            }
            let frame = (surface.0 as f32, surface.1 as f32);
            let white = [1.0, 1.0, 1.0, 0.9];

            push_rect(&mut out, frame, self.panel, [0.08, 0.08, 0.10, 0.82]);
            for (i, b) in self.buttons.iter().enumerate() {
                let bg = if self.hover == Some(i) {
                    [0.95, 0.95, 1.0, 0.28]
                } else {
                    [0.9, 0.9, 0.95, 0.10]
                };
                push_rect(&mut out, frame, *b, bg);
            }

            for (i, b) in self.buttons.iter().enumerate() {
                let cx = b.x + b.w / 2.0;
                let cy = b.y + b.h / 2.0;
                let r = (b.h * 0.22).max(3.0);
                match ITEMS[i] {
                    Action::CycleInput => {
                        let n = match mode {
                            InputMode::Opaque => 1,
                            InputMode::Full => 2,
                            InputMode::PassThrough => 3,
                        };
                        // Outline square = the surface, dots = the current mode.
                        let s = r * 1.35;
                        let t = 1.2;
                        for edge in [
                            Rect {
                                x: cx - s,
                                y: cy - s,
                                w: s * 2.0,
                                h: t,
                            },
                            Rect {
                                x: cx - s,
                                y: cy + s - t,
                                w: s * 2.0,
                                h: t,
                            },
                            Rect {
                                x: cx - s,
                                y: cy - s,
                                w: t,
                                h: s * 2.0,
                            },
                            Rect {
                                x: cx + s - t,
                                y: cy - s,
                                w: t,
                                h: s * 2.0,
                            },
                        ] {
                            push_rect(&mut out, frame, edge, white);
                        }
                        let dot = (r * 0.28).max(1.5);
                        let spacing = dot * 2.6;
                        let start = cx - spacing * (n as f32 - 1.0) / 2.0;
                        for k in 0..n {
                            push_rect(
                                &mut out,
                                frame,
                                Rect {
                                    x: start + k as f32 * spacing - dot,
                                    y: cy - dot,
                                    w: dot * 2.0,
                                    h: dot * 2.0,
                                },
                                white,
                            );
                        }
                    }
                    Action::ToggleMirror => {
                        let s = r * 1.35;
                        let t = 1.2;
                        // Mirror axis with unequal halves on either side.
                        for edge in [
                            Rect {
                                x: cx - s,
                                y: cy - s,
                                w: s * 2.0,
                                h: t,
                            },
                            Rect {
                                x: cx - s,
                                y: cy + s - t,
                                w: s * 2.0,
                                h: t,
                            },
                            Rect {
                                x: cx - s,
                                y: cy - s,
                                w: t,
                                h: s * 2.0,
                            },
                            Rect {
                                x: cx + s - t,
                                y: cy - s,
                                w: t,
                                h: s * 2.0,
                            },
                        ] {
                            push_rect(&mut out, frame, edge, white);
                        }
                        let half = s - 1.5 * t;
                        let tall = s * 2.0 - 2.0 * t;
                        push_rect(
                            &mut out,
                            frame,
                            Rect {
                                x: cx - t / 2.0,
                                y: cy - s,
                                w: t,
                                h: s * 2.0,
                            },
                            white,
                        );
                        push_rect(
                            &mut out,
                            frame,
                            Rect {
                                x: cx - s + t,
                                y: cy - s + t,
                                w: half,
                                h: tall,
                            },
                            [1.0, 1.0, 1.0, 0.5],
                        );
                        push_rect(
                            &mut out,
                            frame,
                            Rect {
                                x: cx + t / 2.0,
                                y: cy - s + t,
                                w: half,
                                h: tall,
                            },
                            [1.0, 1.0, 1.0, 0.15],
                        );
                    }
                    Action::ScaleDown => {
                        push_rect(
                            &mut out,
                            frame,
                            Rect {
                                x: cx - r,
                                y: cy - 1.0,
                                w: r * 2.0,
                                h: 2.0,
                            },
                            white,
                        );
                    }
                    Action::ScaleUp => {
                        push_rect(
                            &mut out,
                            frame,
                            Rect {
                                x: cx - r,
                                y: cy - 1.0,
                                w: r * 2.0,
                                h: 2.0,
                            },
                            white,
                        );
                        push_rect(
                            &mut out,
                            frame,
                            Rect {
                                x: cx - 1.0,
                                y: cy - r,
                                w: 2.0,
                                h: r * 2.0,
                            },
                            white,
                        );
                    }
                    Action::ToggleLock => {
                        push_rect(
                            &mut out,
                            frame,
                            Rect {
                                x: cx - r * 0.8,
                                y: cy - r * 0.1,
                                w: r * 1.6,
                                h: r * 1.2,
                            },
                            white,
                        );
                        push_rect(
                            &mut out,
                            frame,
                            Rect {
                                x: cx - r * 0.5,
                                y: cy - r * 1.1,
                                w: 2.0,
                                h: r,
                            },
                            white,
                        );
                        push_rect(
                            &mut out,
                            frame,
                            Rect {
                                x: cx + r * 0.5 - 2.0,
                                y: cy - r * 1.1,
                                w: 2.0,
                                h: r,
                            },
                            white,
                        );
                        push_rect(
                            &mut out,
                            frame,
                            Rect {
                                x: cx - r * 0.5,
                                y: cy - r * 1.1,
                                w: r,
                                h: 2.0,
                            },
                            white,
                        );
                    }
                    Action::Reset => {
                        let s = r * 1.1;
                        let t = 1.6;
                        for edge in [
                            Rect {
                                x: cx - s,
                                y: cy - s,
                                w: s * 2.0,
                                h: t,
                            },
                            Rect {
                                x: cx - s,
                                y: cy + s - t,
                                w: s * 2.0,
                                h: t,
                            },
                            Rect {
                                x: cx - s,
                                y: cy - s,
                                w: t,
                                h: s * 2.0,
                            },
                            Rect {
                                x: cx + s - t,
                                y: cy - s,
                                w: t,
                                h: s * 2.0,
                            },
                        ] {
                            push_rect(&mut out, frame, edge, white);
                        }
                        push_rect(
                            &mut out,
                            frame,
                            Rect {
                                x: cx - 1.5,
                                y: cy - 1.5,
                                w: 3.0,
                                h: 3.0,
                            },
                            white,
                        );
                    }
                    Action::Quit => push_cross(&mut out, frame, (cx, cy), r, white),
                }
            }
            out
        }
    }

    fn ndc(frame: (f32, f32), x: f32, y: f32) -> [f32; 2] {
        [x / frame.0 * 2.0 - 1.0, 1.0 - y / frame.1 * 2.0]
    }

    /// A line segment to draw: two endpoints, the origin they are measured from, and its half width.
    ///
    /// The sibling of `Rect` for lines. Both exist so the drawing helpers take a shape rather than a list of
    /// numbers, which is the same reason `push_rect` takes a `Rect`.
    struct Segment {
        /// The point `from` and `to` are measured from, so the caller can place a shape by its centre.
        origin: (f32, f32),
        from: (f32, f32),
        to: (f32, f32),
        half_width: f32,
    }

    fn push_rect(out: &mut Vec<MenuVertex>, frame: (f32, f32), r: Rect, color: [f32; 4]) {
        let c = [
            color[0] * color[3],
            color[1] * color[3],
            color[2] * color[3],
            color[3],
        ];
        let p = [
            ndc(frame, r.x, r.y),
            ndc(frame, r.x + r.w, r.y),
            ndc(frame, r.x + r.w, r.y + r.h),
            ndc(frame, r.x, r.y + r.h),
        ];
        for i in [0usize, 1, 2, 0, 2, 3] {
            out.push(MenuVertex {
                pos: p[i],
                color: c,
            });
        }
    }

    /// The shape that means quit: two diagonals crossing at the centre.
    fn push_cross(
        out: &mut Vec<MenuVertex>,
        frame: (f32, f32),
        centre: (f32, f32),
        r: f32,
        color: [f32; 4],
    ) {
        let s = r * 0.9;
        for (x1, y1, x2, y2) in [(-s, -s, s, s), (s, -s, -s, s)] {
            push_diagonal(
                out,
                frame,
                Segment {
                    origin: centre,
                    from: (x1, y1),
                    to: (x2, y2),
                    half_width: 1.6,
                },
                color,
            );
        }
    }

    fn push_diagonal(
        out: &mut Vec<MenuVertex>,
        frame: (f32, f32),
        segment: Segment,
        color: [f32; 4],
    ) {
        let Segment {
            origin: (cx, cy),
            from: (x1, y1),
            to: (x2, y2),
            half_width,
        } = segment;
        let c = [
            color[0] * color[3],
            color[1] * color[3],
            color[2] * color[3],
            color[3],
        ];
        let (dx, dy) = (x2 - x1, y2 - y1);
        let len = (dx * dx + dy * dy).sqrt().max(0.001);
        let (nx, ny) = (-dy / len * half_width, dx / len * half_width);
        let (ax, ay) = (cx + x1, cy + y1);
        let (bx, by) = (cx + x2, cy + y2);
        let p = [
            ndc(frame, ax + nx, ay + ny),
            ndc(frame, bx + nx, by + ny),
            ndc(frame, bx - nx, by - ny),
            ndc(frame, ax - nx, ay - ny),
        ];
        for i in [0usize, 1, 2, 0, 2, 3] {
            out.push(MenuVertex {
                pos: p[i],
                color: c,
            });
        }
    }
}

struct OverlayState {
    configured: Option<(u32, u32)>,
    /// Size we pin the layer surface to; differing configures are bounced back
    /// so the frame is never stretched to another aspect ratio.
    desired_size: (u32, u32),
    closed: bool,
    surface: wl_surface::WlSurface,
    layer_surface: zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
    /// Kept alive so the seat stays bound.
    _seat: wl_seat::WlSeat,
    /// Kept alive so pointer events keep arriving.
    _pointer: Option<wl_pointer::WlPointer>,
    pointer_pos: (f64, f64),
    dragging: bool,
    drag_start: (f64, f64),
    margin_left: i32,
    margin_top: i32,
    frame_margin_left: i32,
    frame_margin_top: i32,
    animation: Arc<Animation>,
    frame_index: usize,
    /// Native frame size; `scale_percent` is a percentage of it.
    native_size: (u32, u32),
    scale_percent: u32,
    orientation: Orientation,
    surface_size: (u32, u32),
    compositor: wl_compositor::WlCompositor,
    qh: QueueHandle<OverlayState>,
    /// Empty region used for full click-through.
    empty_region: wl_region::WlRegion,
    input_mode: InputMode,
    /// Region currently installed as the surface input region (kept alive).
    region: Option<wl_region::WlRegion>,
    /// Rectangles backing `region`, so redundant updates are skipped.
    applied_rects: Vec<(i32, i32, i32, i32)>,
    /// Canonical animation path; the key used to persist instance geometry.
    instance_key: Option<String>,
    /// `secondary_instances.json` location.
    state_path: PathBuf,
    /// Playback override in frames per second; 0 follows the frame delays.
    fps: u32,
    /// `anim_meta.json` override for this animation; 0 when unset.
    meta_fps: u32,
    /// Where the surface starts inside the author's canvas (cropped padding).
    crop: (u32, u32),
    /// Dragging disabled.
    locked: bool,
    /// Hide the surface while the pointer is over an opaque pixel.
    hover_hide: bool,
    /// Hidden via the 1x1 keep-alive (surface stays mapped).
    hidden: bool,
    /// Whether the pointer is currently over the surface.
    pointer_inside: bool,
    /// Whether the pointer is currently over an opaque pixel.
    pointer_over: bool,
    outputs: OutputState,
    /// Output the surface was last seen on (used for clipping).
    active_output: Option<wl_output::WlOutput>,
    /// Last logical output size we saw, kept when the surface leaves the output.
    last_output_size: Option<(u32, u32)>,
    /// Surface size changed: force the next input-region update.
    region_dirty: bool,
    /// Visible content changed: present a frame.
    visual_dirty: bool,
    /// Optional region coarsening policy and rectangle budgets.
    region_tuning: RegionTuning,
    /// Sampling step used for the installed region.
    applied_step: u32,
    /// In-surface context menu.
    menu: menu::Menu,
    /// This instance's control file (the menu writes actions there).
    control_path: PathBuf,
}

impl OutputHandler for OverlayState {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if let Some(info) = self.outputs.info(&output)
            && let Some((width, height)) = info.logical_size
        {
            self.last_output_size = Some((width.max(1) as u32, height.max(1) as u32));
        }
        reapply_clamp(self);
    }

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

smithay_client_toolkit::delegate_output!(OverlayState);

impl OverlayState {
    /// True when the surface-local point hits an opaque pixel of the frame
    /// currently on screen. The frame is stretched to the surface.
    fn opaque_at(&self, sx: f64, sy: f64) -> bool {
        self.animation.alpha_hit(
            self.frame_index,
            self.surface_size,
            self.orientation,
            sx,
            sy,
            ALPHA_HIT_THRESHOLD,
        )
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for OverlayState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for OverlayState {
    fn event(
        _: &mut Self,
        _: &wl_compositor::WlCompositor,
        _: wl_compositor::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for OverlayState {
    fn event(
        state: &mut Self,
        _: &wl_surface::WlSurface,
        event: wl_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_surface::Event::Enter { output } => {
                state.active_output = Some(output);
                reapply_clamp(state);
            }
            // Keep the last known output on leave: the surface can leave the
            // output mid-drag, and we still need its size to clamp it back.
            wl_surface::Event::Leave { .. } => {}
            _ => {}
        }
    }
}

impl Dispatch<wl_region::WlRegion, ()> for OverlayState {
    fn event(
        _: &mut Self,
        _: &wl_region::WlRegion,
        _: wl_region::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for OverlayState {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities } = event
            && let WEnum::Value(capabilities) = capabilities
            && capabilities.contains(wl_seat::Capability::Pointer)
        {
            state._pointer = Some(seat.get_pointer(qh, ()));
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for OverlayState {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            } => {
                state.pointer_pos = (surface_x, surface_y);
                state.pointer_inside = true;
                state.pointer_over = state.opaque_at(surface_x, surface_y);
            }
            wl_pointer::Event::Leave { .. } => {
                state.dragging = false;
                state.pointer_inside = false;
                state.pointer_over = false;
                close_menu(state);
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                state.pointer_pos = (surface_x, surface_y);
                state.pointer_over = state.opaque_at(surface_x, surface_y);
                if state.menu.update_hover(surface_x, surface_y) {
                    state.visual_dirty = true;
                }
                if !state.dragging {
                    return;
                }
                // Margins are relative to the frame-start snapshot, so every
                // Motion in this dispatch batch shares one stable base.
                let dx = surface_x - state.drag_start.0;
                let dy = surface_y - state.drag_start.1;
                // Anchor is Top|Left, so window origin == left/top margin:
                // moving the pointer right/down must *increase* the margin.
                let new_left = (f64::from(state.frame_margin_left) + dx).round() as i32;
                let new_top = (f64::from(state.frame_margin_top) + dy).round() as i32;
                let (left, top) = clamp_margin(state, new_left, new_top);
                state.margin_left = left;
                state.margin_top = top;
                state.layer_surface.set_margin(top, 0, 0, left);
                state.surface.commit();
            }
            wl_pointer::Event::Button {
                button,
                state: button_state,
                ..
            } => {
                match button_state {
                    WEnum::Value(wl_pointer::ButtonState::Pressed) => {
                        if button == BTN_RIGHT {
                            if state.menu.open {
                                close_menu(state);
                            } else if state.opaque_at(state.pointer_pos.0, state.pointer_pos.1) {
                                open_menu(state, state.pointer_pos);
                            }
                            return;
                        }
                        if state.menu.open {
                            if let Some(action) =
                                state.menu.hit(state.pointer_pos.0, state.pointer_pos.1)
                            {
                                menu_action(state, action);
                            }
                            close_menu(state);
                            return;
                        }
                        // A locked instance ignores presses; otherwise only drag
                        // when the press lands on a solid pixel.
                        if !state.locked
                            && state.opaque_at(state.pointer_pos.0, state.pointer_pos.1)
                        {
                            state.dragging = true;
                            state.drag_start = state.pointer_pos;
                            log::event(
                                log::Level::Info,
                                "overlay.drag.start",
                                &[("x", &state.pointer_pos.0), ("y", &state.pointer_pos.1)],
                            );
                            // Anyone watching (the main window) can now follow along.
                            if let Err(error) = ControlState::claim_focus(&state.control_path) {
                                log::failure("overlay.focus.claim_failed", &error);
                            }
                        }
                    }
                    WEnum::Value(wl_pointer::ButtonState::Released) => {
                        if button != BTN_LEFT {
                            return;
                        }
                        if state.dragging {
                            log::event(
                                log::Level::Info,
                                "overlay.drag.end",
                                &[("left", &state.margin_left), ("top", &state.margin_top)],
                            );
                            persist_geometry(state);
                        }
                        state.dragging = false;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<zwlr_layer_shell_v1::ZwlrLayerShellV1, ()> for OverlayState {
    fn event(
        _: &mut Self,
        _: &zwlr_layer_shell_v1::ZwlrLayerShellV1,
        _: zwlr_layer_shell_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for OverlayState {
    fn event(
        state: &mut Self,
        _: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                state.layer_surface.ack_configure(serial);
                // niri shrinks layer surfaces pushed past a screen edge; taking
                // that size would stretch the frame. Shrink our pinned size only
                // if it cannot fit the output (aspect preserved), then bounce.
                state.desired_size = fit_to_output(state, state.desired_size);
                if width > 0 && height > 0 && (width, height) != state.desired_size {
                    state
                        .layer_surface
                        .set_size(state.desired_size.0, state.desired_size.1);
                    state.surface.commit();
                    log::event(
                        log::Level::Debug,
                        "overlay.configure",
                        &[
                            ("offered_w", &width),
                            ("offered_h", &height),
                            ("pinned_w", &state.desired_size.0),
                            ("pinned_h", &state.desired_size.1),
                        ],
                    );
                }
                state.configured = Some(state.desired_size);
            }
            zwlr_layer_surface_v1::Event::Closed => {
                state.closed = true;
            }
            _ => {}
        }
    }
}

/// What `load_animation` produced.
struct Loaded {
    animation: Arc<Animation>,
    blocks: Option<Blocks>,
    /// Where the visible art started inside the author's canvas; `(0, 0)` when the
    /// animation was used as-is.
    crop: (u32, u32),
    /// Decode limits this load used; recorded in the sidecar when encoding (the
    /// encode bakes them in, so a different setting must re-encode).
    limits: DecodeLimits,
}

/// Everything that depends on the loaded animation's geometry: replacing these is what
/// makes a reload with a different size, frame count or storage path possible without
/// restarting the process.
struct Frames {
    animation: Arc<Animation>,
    blocks: Option<Blocks>,
    blocks_usable: bool,
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    tex_width: u32,
    tex_height: u32,
    /// `(width / tex_width, height / tex_height)`: 1:1 for RGBA, < 1 for the BC7
    /// padding up to whole 4x4 blocks.
    scale: (f32, f32),
}

impl Frames {
    fn frame_count(&self) -> usize {
        self.animation.frames.len()
    }
}

/// Upload one frame into the frame texture: BC7 blocks when we have usable ones,
/// otherwise the decoded RGBA. Only writes to the texture, so `&Frames` is enough.
fn upload_frame(queue: &wgpu::Queue, frames: &Frames, index: usize, layer: u32) {
    let started = std::time::Instant::now();
    let texture = &frames.texture;
    let (tex_width, tex_height) = (frames.tex_width, frames.tex_height);
    let (width, height) = (frames.animation.width, frames.animation.height);
    let (data, bytes_per_row, rows, size) =
        match frames.blocks.as_ref().filter(|_| frames.blocks_usable) {
            Some(blocks) => {
                let block_bytes = blocks.frame_bytes();
                let start = index * block_bytes;
                (
                    &blocks.data[start..start + block_bytes],
                    blocks.blocks_per_row() * 16,
                    blocks.rows(),
                    (tex_width, tex_height),
                )
            }
            None => (
                frames.animation.frames[index].rgba.as_slice(),
                width * 4,
                height,
                (width, height),
            ),
        };
    queue.write_texture(
        TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: Origin3d {
                x: 0,
                y: 0,
                z: layer,
            },
            aspect: TextureAspect::All,
        },
        data,
        TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row),
            rows_per_image: Some(rows),
        },
        Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
    );
    log::event(
        log::Level::Debug,
        "overlay.upload",
        &[
            ("us", &started.elapsed().as_micros()),
            ("kb", &(size.0 as u64 * size.1 as u64 * 16 / 16 / 1024)),
        ],
    );
}

/// First run (or reload) for a file without a BC7 sidecar: encode one in the background
/// while the RGBA path plays. A later reload uploads the blocks directly.
#[cfg(feature = "bc7")]
fn spawn_bc7_encode(
    path: Option<&std::path::Path>,
    animation: &Arc<Animation>,
    crop: (u32, u32),
    limits: DecodeLimits,
    bc7: bool,
) {
    if !bc7 {
        return;
    }
    let Some(source) = path else {
        return;
    };
    let frames = Arc::clone(animation);
    let source = source.to_path_buf();
    std::thread::spawn(move || {
        let started = Instant::now();
        let params = anima_engine_rs::bc7::Bc7Params::default();
        let mut data = Vec::with_capacity(frames.frames.len() * frames.frame_bytes() as usize);
        for frame in &frames.frames {
            match anima_engine_rs::bc7::encode(&frame.rgba, frames.width, frames.height, params) {
                Ok(blocks) => data.extend_from_slice(&blocks),
                Err(err) => {
                    log::failure("overlay.bc7.encode_failed", &err);
                    return;
                }
            }
        }
        let blocks = Blocks {
            width: frames.width,
            height: frames.height,
            count: frames.frames.len(),
            uber_level: params.uber_level,
            crop,
            limits,
            delays: frames.frames.iter().map(|frame| frame.delay_ms).collect(),
            data,
        };
        match FrameCache::user_default().write_blocks(&source, &blocks) {
            Ok(()) => log::event(
                log::Level::Info,
                "overlay.bc7.written",
                &[
                    (
                        "mib",
                        &format!("{:.1}", blocks.data.len() as f64 / (1024.0 * 1024.0)),
                    ),
                    ("seconds", &started.elapsed().as_secs_f32()),
                ],
            ),
            Err(err) => log::failure("overlay.bc7.write_failed", &err),
        }
    });
}

/// Reduce every frame to its alpha mask once the BC7 encoder has finished with the pixels.
///
/// The encoder is the last reader: `upload_frame` uses the blocks when it has them, the hit test and the ink
/// scan have already run, and a `bc7` build never writes an RGBA cache entry. `Arc::get_mut` needs the only
/// reference, so the copy held by `frames` becomes an empty placeholder first — cloning the animation would
/// copy the whole allocation being freed. The `blocks_usable` guard keeps a first run intact: there the
/// blocks live only in the encoder thread until the next start reads them back, so the pixels are still
/// needed and shrinking would render a black frame.
fn shrink_frames(frames: &mut Frames, state: &mut OverlayState) {
    // Nothing to do twice, and nothing to do when this run has no blocks in memory: on a first run they
    // exist only inside the encoder thread, which writes them to disk, so `upload_frame` still needs the
    // pixels until the next start reads them back. Shrinking then would render a black frame.
    if frames
        .animation
        .frames
        .first()
        .is_none_or(|frame| frame.rgba.is_empty())
        || !frames.blocks_usable
    {
        return;
    }
    // The placeholder has to keep the frame *count*: the loop advances `index` modulo `frame_count()`, and
    // the delays are what the next start re-uses if the blocks are read back early. Pixels and masks are
    // empty, which is a few kilobytes rather than the animation.
    let width = frames.animation.width;
    let height = frames.animation.height;
    let frames_meta = frames
        .animation
        .frames
        .iter()
        .map(|frame| Frame {
            rgba: Vec::new(),
            delay_ms: frame.delay_ms,
            alpha_bits: Vec::new(),
        })
        .collect();
    frames.animation = Arc::new(Animation {
        width,
        height,
        frames: frames_meta,
    });
    let Some(animation) = Arc::get_mut(&mut state.animation) else {
        return;
    };
    let freed = animation.shrink_to_alpha();
    // Returning the pages to the allocator is not the same as returning them to the kernel: glibc keeps freed
    // memory in its arenas and only trims when asked. Measured before this call existed, a run held 644.6 MiB
    // of RSS for twenty-four seconds after releasing 753.4 MiB — steady to a tenth of a megabyte.
    #[cfg(target_env = "gnu")]
    unsafe {
        malloc_trim(0);
    }
    log::event(
        log::Level::Info,
        "overlay.shrink",
        &[(
            "freed_mib",
            &format!("{:.1}", freed as f64 / (1024.0 * 1024.0)),
        )],
    );
}

// glibc's arena trim, declared here rather than taking a `libc` dependency for one call.
#[cfg(target_env = "gnu")]
unsafe extern "C" {
    fn malloc_trim(pad: usize) -> i32;
}

/// Crop the author's transparent padding away, reporting where the art started.
///
/// Art usually carries padding (measured: 227 px on miku12's right, 344 px on
/// teto234, 73% of bochi455's canvas), and the overlay clamps against the whole
/// canvas — so without this the art can never reach a screen edge. Cropping also
/// shrinks the texture, the input-region scan and GPU memory.
fn crop_to_ink(animation: Animation) -> (Animation, (u32, u32)) {
    let canvas = (animation.width, animation.height);
    let step = if animation.frames.len() > 200 { 2 } else { 1 };
    let Some(ink) = anima_engine_rs::anim::InkBox::scan(&animation, step) else {
        return (animation, (0, 0));
    };
    let offset = (ink.min_x, ink.min_y);
    if offset == (0, 0) && (ink.width(), ink.height()) == canvas {
        return (animation, (0, 0));
    }
    let cropped = animation.crop(ink);
    log::event(
        log::Level::Info,
        "overlay.crop",
        &[
            ("from_w", &canvas.0),
            ("from_h", &canvas.1),
            ("to_w", &cropped.width),
            ("to_h", &cropped.height),
            ("at_x", &offset.0),
            ("at_y", &offset.1),
        ],
    );
    (cropped, offset)
}

/// Load an animation (and any cached BC7 blocks), using the frame cache so a file
/// is decoded only once.
fn load_animation(path: Option<&Path>) -> Result<Loaded, Box<dyn std::error::Error>> {
    let limits = DecodeLimits::from_env();
    let Some(path) = path else {
        return Ok(Loaded {
            animation: Arc::new(test_animation()),
            blocks: None,
            crop: (0, 0),
            limits,
        });
    };

    let cache = FrameCache::user_default();
    let stale = cache.clean_stale_temps(std::time::Duration::from_secs(3600));
    if stale > 0 {
        log::event(
            log::Level::Info,
            "overlay.cache.removed_stale",
            &[("mib", &format!("{:.1}", stale as f64 / (1024.0 * 1024.0)))],
        );
    }

    // With `bc7` the blocks are the cached format: rebuild the RGBA that
    // hit-testing and regions need from them, instead of keeping a second copy of
    // the frames on disk.
    #[cfg(feature = "bc7")]
    {
        let cached = cache.read_blocks(path);
        // The encode bakes the decode limits in, so a sidecar from a different
        // setting is dropped and re-encoded instead of silently reused.
        if let Some(blocks) = cached.as_ref().filter(|blocks| blocks.limits == limits)
            && let Ok(animation) = anima_engine_rs::bc7::animation_from_blocks(
                blocks.width,
                blocks.height,
                &blocks.delays,
                &blocks.data,
            )
        {
            let (animation, report) = anima_engine_rs::anim::limit_animation(animation, limits);
            // The blocks already hold the cropped art and the sidecar remembers where
            // the crop started, so saved canvas positions keep working and no rescan
            // is needed.
            let crop = blocks.crop;
            log::event(
                log::Level::Info,
                "overlay.bc7.cache_hit",
                &[
                    ("frames", &animation.frame_count()),
                    ("width", &animation.width),
                    ("height", &animation.height),
                    (
                        "mib",
                        &format!("{:.1}", blocks.data.len() as f64 / (1024.0 * 1024.0)),
                    ),
                    ("note", &report.note()),
                ],
            );
            return Ok(Loaded {
                animation: Arc::new(animation),
                blocks: Some(blocks.clone()),
                crop,
                limits,
            });
        }
        if cached.is_some_and(|blocks| blocks.limits != limits) {
            log::event(log::Level::Info, "overlay.bc7.limits_changed", &[]);
            if let Ok(blocks_path) = cache.blocks_path(path) {
                let _ = std::fs::remove_file(blocks_path);
            }
        }
    }

    if let Some(animation) = cache.read(path) {
        let (animation, report) = anima_engine_rs::anim::limit_animation(animation, limits);
        let blocks = cache.read_blocks(path);
        let (animation, crop) = crop_to_ink(animation);
        log::event(
            log::Level::Info,
            "overlay.cache.hit",
            &[
                ("frames", &animation.frame_count()),
                ("width", &animation.width),
                ("height", &animation.height),
                ("note", &report.note()),
                ("blocks", &if blocks.is_some() { "bc7" } else { "rgba" }),
            ],
        );
        return Ok(Loaded {
            animation: Arc::new(animation),
            blocks,
            crop,
            limits,
        });
    }

    let started = Instant::now();
    let (animation, report) = anima_engine_rs::anim::decode_reporting(path, limits)?;
    log::event(
        log::Level::Info,
        "overlay.decoded",
        &[
            ("frames", &animation.frame_count()),
            ("width", &animation.width),
            ("height", &animation.height),
            (
                "mib",
                &format!("{:.1}", animation.bytes() as f64 / (1024.0 * 1024.0)),
            ),
            ("note", &report.note()),
            ("seconds", &started.elapsed().as_secs_f32()),
        ],
    );

    let (animation, crop) = crop_to_ink(animation);
    let animation = Arc::new(animation);
    // Read any existing BC7 sidecar before `cache` moves into the writer thread.
    let blocks = cache.read_blocks(path);
    // A decimated decode depends on the configured limits, so it is never
    // cached: the cache always holds a complete decode. Encoding runs in the
    // background so playback is not blocked by PNG compression of large files.
    // With `bc7` the animation is cached as blocks instead (by the background
    // encoder below), so no frames entry is written at all.
    #[cfg(not(feature = "bc7"))]
    if !report.limited() {
        let cache_animation = Arc::clone(&animation);
        let cache_path = path.to_path_buf();
        std::thread::spawn(move || {
            if let Err(err) = cache.write(&cache_path, &cache_animation) {
                log::failure("overlay.cache.write_failed", &err);
            } else {
                log::event(
                    log::Level::Info,
                    "overlay.cache.written",
                    &[("frames", &cache_animation.frame_count())],
                );
            }
        });
    }

    Ok(Loaded {
        animation,
        blocks,
        crop,
        limits,
    })
}

/// A small generated animation so the binary is useful with no arguments.
fn test_animation() -> Animation {
    let (width, height) = (256u32, 256u32);
    let count = 24usize;
    let mut frames = Vec::with_capacity(count);
    for index in 0..count {
        let t = index as f32 / count as f32;
        let cx = 48.0 + t * 160.0;
        let cy = 128.0 + (t * std::f32::consts::TAU).sin() * 48.0;
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let dx = x as f32 - cx;
                let dy = y as f32 - cy;
                if (dx * dx + dy * dy).sqrt() <= 56.0 {
                    let i = ((y * width + x) * 4) as usize;
                    rgba[i] = (t * 255.0) as u8;
                    rgba[i + 1] = ((1.0 - t) * 255.0) as u8;
                    rgba[i + 2] = 220;
                    rgba[i + 3] = 255;
                }
            }
        }
        frames.push(Frame {
            rgba,
            delay_ms: 70,
            alpha_bits: Vec::new(),
        });
    }
    Animation {
        width,
        height,
        frames,
    }
}

/// Default hard rectangle cap: beyond this the sampling grid is coarsened
/// (2x2, 4x4, ...) and only then falls back to a bounding box.
const DEFAULT_REGION_MAX_RECTS: usize = 4096;

/// Recompute the input region every N presented frames. Frame-count based on
/// purpose: a wall-clock timer would make the hit area lag by a variable amount.
const REGION_UPDATE_FRAMES: u64 = 3;

/// Default soft rectangle budget for [`RegionStep::Auto`].
const DEFAULT_REGION_SOFT_RECTS: usize = 768;

/// Optional coarsening policy for the input region, from `ANIMA_REGION_STEP`:
/// `off` (default), `auto`, or a fixed `1..=32` pixel sampling grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegionStep {
    Off,
    Auto,
    Fixed(u32),
}

impl RegionStep {
    fn parse(value: &str) -> Self {
        match value {
            "auto" => Self::Auto,
            other => other
                .parse::<u32>()
                .map(|step| Self::Fixed(step.clamp(1, 32)))
                .unwrap_or(Self::Off),
        }
    }

    fn from_env() -> Self {
        std::env::var("ANIMA_REGION_STEP")
            .ok()
            .as_deref()
            .map(Self::parse)
            .unwrap_or(Self::Off)
    }

    fn label(self) -> String {
        match self {
            Self::Off => "off".to_string(),
            Self::Auto => "auto".to_string(),
            Self::Fixed(step) => step.to_string(),
        }
    }
}

/// Region tuning, all overridable from the environment:
/// `ANIMA_REGION_STEP`, `ANIMA_REGION_SOFT_RECTS`, `ANIMA_REGION_MAX_RECTS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RegionTuning {
    step: RegionStep,
    soft_rects: usize,
    max_rects: usize,
}

impl RegionTuning {
    fn from_env() -> Self {
        let requested_soft = env_usize(
            "ANIMA_REGION_SOFT_RECTS",
            DEFAULT_REGION_SOFT_RECTS,
            16,
            65_536,
        );
        let max_rects = env_usize(
            "ANIMA_REGION_MAX_RECTS",
            DEFAULT_REGION_MAX_RECTS,
            16,
            1_000_000,
        );
        // The soft budget can never exceed the hard cap.
        let soft_rects = requested_soft.min(max_rects);
        Self {
            step: RegionStep::from_env(),
            soft_rects,
            max_rects,
        }
    }

    fn from_control(control: &ControlState) -> Self {
        Self {
            step: RegionStep::parse(&control.region_step),
            soft_rects: control.region_soft_rects.min(control.region_max_rects),
            max_rects: control.region_max_rects,
        }
    }

    fn describe(self) -> String {
        format!(
            "step {}, soft {} rects, max {} rects",
            self.step.label(),
            self.soft_rects,
            self.max_rects
        )
    }
}

/// Parse an env var as `usize`, clamped; invalid or missing values fall back to
/// `default`.
fn env_usize(name: &str, default: usize, min: usize, max: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.clamp(min, max))
        .unwrap_or(default)
}

/// Open the in-surface menu at the pointer. While it is open the input region
/// covers the whole surface so the panel receives clicks even over transparent
/// animation pixels; closing restores the per-pixel region.
fn open_menu(state: &mut OverlayState, at: (f64, f64)) {
    state.menu.open_at(state.surface_size, at.0, at.1);
    state.surface.set_input_region(None);
    state.surface.commit();
    state.visual_dirty = true;
    log::event(
        log::Level::Info,
        "overlay.menu.open",
        &[("x", &at.0), ("y", &at.1)],
    );
}

fn close_menu(state: &mut OverlayState) {
    if !state.menu.open {
        return;
    }
    state.menu.close();
    state.applied_rects.clear();
    state.region_dirty = true;
    state.visual_dirty = true;
    log::event(log::Level::Info, "overlay.menu.closed", &[]);
}

fn menu_action(state: &mut OverlayState, action: menu::Action) {
    let path = state.control_path.clone();
    let current = ControlState::load(&path);
    let next = match action {
        menu::Action::CycleInput => {
            let mode = match state.input_mode {
                InputMode::Opaque => InputMode::Full,
                InputMode::Full => InputMode::PassThrough,
                InputMode::PassThrough => InputMode::Opaque,
            };
            current.set_input_mode(mode)
        }
        menu::Action::ScaleDown => current.nudge_scale(-10),
        menu::Action::ScaleUp => current.nudge_scale(10),
        menu::Action::ToggleMirror => current.toggle_flip_h(),
        menu::Action::ToggleLock => current.toggle_locked(),
        menu::Action::Reset => current.request_reset(),
        menu::Action::Quit => current.request_quit(),
    };
    if let Err(err) = next.save(&path) {
        log::failure("overlay.menu.action_failed", &err);
    }
    log::event(
        log::Level::Info,
        "overlay.menu.action",
        &[("action", &format!("{action:?}"))],
    );
}

/// Apply an input mode to the surface.
fn apply_input_mode(state: &mut OverlayState, mode: InputMode) {
    if state.input_mode == mode {
        return;
    }
    state.input_mode = mode;
    match mode {
        InputMode::Full => {
            state.region = None;
            state.applied_rects.clear();
            state.surface.set_input_region(None);
            state.surface.commit();
            log::event(log::Level::Debug, "overlay.input.full", &[]);
        }
        InputMode::PassThrough => {
            state.region = None;
            state.applied_rects.clear();
            state.surface.set_input_region(Some(&state.empty_region));
            state.surface.commit();
            log::event(log::Level::Debug, "overlay.input.click_through", &[]);
        }
        InputMode::Opaque => {
            apply_opaque_region(state);
            log::event(
                log::Level::Debug,
                "overlay.input.opaque",
                &[
                    ("rects", &state.applied_rects.len()),
                    ("step", &state.applied_step),
                ],
            );
        }
    }
}

/// Build an input region from the opaque pixels of the visible frame and install
/// it, so transparent pixels pass clicks through to whatever is below. Very
/// detailed silhouettes are sampled on a coarser grid, then fall back to their
/// bounding box.
fn apply_opaque_region(state: &mut OverlayState) {
    // The scan and the install are timed apart because they are different kinds of cost. The first is this
    // process's CPU over its own memory, which `frame_bench` measures. The second is a Wayland round trip
    // whose other end is the compositor's hit-testing over however many rectangles we just sent it — the
    // bench cannot see it at all, and it is the half nobody has a number for.
    let scan_started = Instant::now();
    let build = |state: &OverlayState, step: u32| {
        state.animation.alpha_rects_step(
            state.frame_index,
            state.surface_size,
            state.orientation,
            ALPHA_HIT_THRESHOLD,
            step,
        )
    };

    let mut step = match state.region_tuning.step {
        RegionStep::Fixed(step) => step.max(1),
        RegionStep::Off | RegionStep::Auto => 1,
    };
    let mut rects = build(state, step);

    if state.region_tuning.step == RegionStep::Auto {
        // Coarsen by powers of two until the soft budget is met, bounded by what
        // the display size can justify (keeps small overlays pixel-accurate).
        let max_step = (state.surface_size.0.min(state.surface_size.1) / 128).clamp(1, 8);
        while rects.len() > state.region_tuning.soft_rects && step < max_step {
            step *= 2;
            rects = build(state, step);
        }
    }

    // Hard cap: keep coarsening, then fall back to the bounding box.
    while rects.len() > state.region_tuning.max_rects && step < 32 {
        step *= 2;
        rects = build(state, step);
    }
    if rects.len() > state.region_tuning.max_rects
        && let Some(bounds) = anima_engine_rs::anim::bounds_rect(&rects)
    {
        rects = vec![bounds];
    }
    let scan_ms = scan_started.elapsed().as_secs_f64() * 1000.0;

    if rects == state.applied_rects && state.region.is_some() {
        // The common case, and worth a line of its own: it is the only evidence of how often the install is
        // actually skipped, which no other number here can show.
        log::event(
            log::Level::Debug,
            "overlay.region.skip",
            &[
                ("rects", &rects.len()),
                ("step", &step),
                ("scan_ms", &format!("{scan_ms:.3}")),
            ],
        );
        return;
    }

    let install_started = Instant::now();
    let region = state.compositor.create_region(&state.qh, ());
    for &(x, y, w, h) in &rects {
        region.add(x, y, w, h);
    }
    state.surface.set_input_region(Some(&region));
    state.surface.commit();
    log::event(
        log::Level::Debug,
        "overlay.region.install",
        &[
            ("rects", &rects.len()),
            ("step", &step),
            ("scan_ms", &format!("{scan_ms:.3}")),
            (
                "install_ms",
                &format!("{:.3}", install_started.elapsed().as_secs_f64() * 1000.0),
            ),
        ],
    );
    if let Some(previous) = state.region.replace(region) {
        previous.destroy();
    }
    state.applied_rects = rects;
    state.applied_step = step;
}

/// Logical size of the output the surface is on: the last output it entered,
/// falling back to the last size seen and then to any output we have info for.
fn output_size(state: &OverlayState) -> Option<(u32, u32)> {
    let size_of = |output: &wl_output::WlOutput| {
        state
            .outputs
            .info(output)
            .and_then(|info| info.logical_size)
            .map(|(width, height)| (width.max(1) as u32, height.max(1) as u32))
    };
    if let Some(size) = state.active_output.as_ref().and_then(size_of) {
        return Some(size);
    }
    if let Some(size) = state.last_output_size {
        return Some(size);
    }
    state.outputs.outputs().find_map(|output| size_of(&output))
}

/// Clamp a margin pair so the whole surface stays inside the output.
fn clamp_margin(state: &OverlayState, left: i32, top: i32) -> (i32, i32) {
    let Some(output) = output_size(state) else {
        return (left.max(0), top.max(0));
    };
    clamp_position(left, top, state.desired_size, output)
}

/// Keep the whole `surface` inside `output`; a surface larger than the output is
/// pinned at the origin.
fn clamp_position(left: i32, top: i32, surface: (u32, u32), output: (u32, u32)) -> (i32, i32) {
    let max_left = output.0.saturating_sub(surface.0) as i32;
    let max_top = output.1.saturating_sub(surface.1) as i32;
    (left.clamp(0, max_left.max(0)), top.clamp(0, max_top.max(0)))
}

/// Clamp the current margins to the output, committing if they moved.
fn reapply_clamp(state: &mut OverlayState) {
    let (left, top) = clamp_margin(state, state.margin_left, state.margin_top);
    if (left, top) == (state.margin_left, state.margin_top) {
        return;
    }
    state.margin_left = left;
    state.margin_top = top;
    state.layer_surface.set_margin(top, 0, 0, left);
    state.surface.commit();
    log::event(
        log::Level::Info,
        "overlay.clamped",
        &[("left", &left), ("top", &top)],
    );
    persist_geometry(state);
}

/// The displayed surface size for the current scale and orientation.
fn scaled_size(state: &OverlayState) -> (u32, u32) {
    let (disp_w, disp_h) = state.orientation.size(state.native_size);
    let percent = u64::from(state.scale_percent);
    (
        (u64::from(disp_w) * percent / 100).max(1) as u32,
        (u64::from(disp_h) * percent / 100).max(1) as u32,
    )
}

/// Resize the surface to `percent` of the native frame size. Only shrinking is
/// allowed (clamped at 100%), so the GPU can sample the frame texture down and
/// no resampling is ever required.
fn apply_scale(state: &mut OverlayState, percent: u32) {
    let percent = percent.clamp(MIN_SCALE_PERCENT, MAX_SCALE_PERCENT);
    if state.scale_percent == percent {
        return;
    }
    state.scale_percent = percent;
    state.visual_dirty = true;
    state.region_dirty = true;
    if state.hidden {
        log::event(
            log::Level::Info,
            "overlay.scale.hidden",
            &[("percent", &percent)],
        );
    } else {
        let (width, height) = fit_to_output(state, scaled_size(state));
        state.desired_size = (width, height);
        state.layer_surface.set_size(width, height);
        state.surface.commit();
        state.applied_rects.clear();
        log::event(
            log::Level::Info,
            "overlay.scale",
            &[
                ("percent", &percent),
                ("width", &width),
                ("height", &height),
            ],
        );
        reapply_clamp(state);
    }
    persist_geometry(state);
}

/// Show or hide the overlay. Hiding resizes the layer surface to 1x1 instead of
/// unmapping it: the surface — and therefore our event loop, tray and pointer
/// handling — stays alive, which Wayland needs, since an unmapped surface cannot
/// be brought back.
fn apply_hidden(state: &mut OverlayState, hidden: bool) {
    if state.hidden == hidden {
        return;
    }
    state.hidden = hidden;
    state.visual_dirty = true;
    state.region_dirty = true;
    let (width, height) = if hidden {
        (1, 1)
    } else {
        fit_to_output(state, scaled_size(state))
    };
    state.desired_size = (width, height);
    state.layer_surface.set_size(width, height);
    state.surface.commit();
    state.applied_rects.clear();
    log::event(
        log::Level::Info,
        "overlay.visibility",
        &[("hidden", &hidden)],
    );
    reapply_clamp(state);
}

/// Apply a new rotation/mirror at runtime: resize to the rotated size (fit to
/// the output, aspect preserved), rebuild regions and redraw.
fn apply_orientation(state: &mut OverlayState, orientation: Orientation) {
    if orientation == state.orientation {
        return;
    }
    state.orientation = orientation;
    state.region_dirty = true;
    state.visual_dirty = true;
    state.applied_rects.clear();
    log::event(
        log::Level::Info,
        "overlay.transform",
        &[
            ("degrees", &orientation.degrees()),
            ("flip_h", &orientation.flip_h),
            ("flip_v", &orientation.flip_v),
        ],
    );
    if state.hidden {
        return;
    }
    let (width, height) = fit_to_output(state, scaled_size(state));
    state.desired_size = (width, height);
    state.layer_surface.set_size(width, height);
    state.surface.commit();
    reapply_clamp(state);
    persist_geometry(state);
}

/// Shrink `size` to fit the output, preserving its aspect ratio.
fn fit_to_output(state: &OverlayState, size: (u32, u32)) -> (u32, u32) {
    let Some((out_w, out_h)) = output_size(state) else {
        return size;
    };
    if size.0 <= out_w && size.1 <= out_h {
        return size;
    }
    let scale = (f64::from(out_w) / f64::from(size.0)).min(f64::from(out_h) / f64::from(size.1));
    (
        ((f64::from(size.0) * scale).floor() as u32).max(1),
        ((f64::from(size.1) * scale).floor() as u32).max(1),
    )
}

/// Persist position, display size, transform, click-through, lock and hover-hide
/// for this instance, preserving other instances and fields in the document.
fn persist_geometry(state: &OverlayState) {
    let Some(key) = state.instance_key.as_deref() else {
        return;
    };
    let click_through = state.input_mode == InputMode::PassThrough;
    let rotation = state.orientation.degrees();
    // Stored x/y stay in the author's canvas space. The surface sits `crop` pixels
    // into the canvas, so the canvas position is the surface position minus that.
    let canvas_x = state.margin_left - (state.crop.0 * state.scale_percent / 100) as i32;
    let canvas_y = state.margin_top - (state.crop.1 * state.scale_percent / 100) as i32;
    let mut document = SecondaryState::load(&state.state_path);
    let changed = {
        let instance = document.entry(key);
        let changed = instance.x != canvas_x
            || instance.y != canvas_y
            || instance.scale_percent != state.scale_percent
            || instance.click_through != click_through
            || instance.locked != state.locked
            || instance.hover_hide != state.hover_hide
            || instance.rotation != rotation
            || instance.flip_h != state.orientation.flip_h
            || instance.flip_v != state.orientation.flip_v;
        if changed {
            instance.x = canvas_x;
            instance.y = canvas_y;
            instance.scale_percent = state.scale_percent;
            instance.click_through = click_through;
            instance.locked = state.locked;
            instance.hover_hide = state.hover_hide;
            instance.rotation = rotation;
            instance.flip_h = state.orientation.flip_h;
            instance.flip_v = state.orientation.flip_v;
        }
        changed
    };
    if changed && let Err(err) = document.save(&state.state_path) {
        log::failure("overlay.instance.save_failed", &err);
    }
}

/// Read-modify-write helper for the control file.
fn write_control(path: &Path, action: impl FnOnce(ControlState) -> ControlState) {
    let next = action(ControlState::load(path));
    if let Err(err) = next.save(path) {
        log::failure("overlay.control.write_failed", &err);
    }
    // A tray or portal action means the user is looking at this instance.
    if let Err(err) = ControlState::claim_focus(path) {
        log::failure("overlay.focus.claim_failed", &err);
    }
}

/// Write a specific input mode to the control file.
fn write_mode(path: &Path, mode: InputMode) {
    write_control(path, move |state| state.set_input_mode(mode));
}

/// `StatusNotifierItem` (DBus) tray: lets the user toggle click-through, reset
/// the overlay or quit it by writing [`ControlState`].
struct OverlayTray {
    control_path: PathBuf,
    id: String,
    title: String,
}

impl ksni::Tray for OverlayTray {
    // A left click opens the menu instead of doing anything else.
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        self.id.clone()
    }

    fn title(&self) -> String {
        self.title.clone()
    }

    fn icon_name(&self) -> String {
        "applications-multimedia".into()
    }

    fn menu(&self) -> Vec<ksni::menu::MenuItem<Self>> {
        use ksni::menu::{CheckmarkItem, MenuItem, StandardItem, SubMenu};

        let state = ControlState::load(&self.control_path);
        let mode_item = |label: &str, mode: InputMode| {
            MenuItem::Checkmark(CheckmarkItem {
                label: label.into(),
                checked: state.input_mode == mode,
                activate: Box::new(move |tray: &mut Self| {
                    write_mode(&tray.control_path, mode);
                }),
                ..Default::default()
            })
        };
        let size_item = |label: &str, percent: u32| {
            MenuItem::Checkmark(CheckmarkItem {
                label: label.into(),
                checked: state.scale_percent == percent,
                activate: Box::new(move |tray: &mut Self| {
                    write_control(&tray.control_path, move |state| {
                        state.set_scale_percent(percent)
                    });
                }),
                ..Default::default()
            })
        };
        let region_item = |label: &str, value: &'static str| {
            MenuItem::Checkmark(CheckmarkItem {
                label: label.into(),
                checked: state.region_step.as_str() == value,
                activate: Box::new(move |tray: &mut Self| {
                    write_control(&tray.control_path, move |state| {
                        state.set_region_step(value)
                    });
                }),
                ..Default::default()
            })
        };
        vec![
            MenuItem::SubMenu(SubMenu {
                label: "Input".into(),
                submenu: vec![
                    mode_item("Full surface", InputMode::Full),
                    mode_item("Opaque pixels only", InputMode::Opaque),
                    mode_item("Click-through (all)", InputMode::PassThrough),
                ],
                ..Default::default()
            }),
            MenuItem::SubMenu(SubMenu {
                label: "Size".into(),
                submenu: vec![
                    size_item("100% (native)", 100),
                    size_item("75%", 75),
                    size_item("50%", 50),
                    size_item("25%", 25),
                ],
                ..Default::default()
            }),
            MenuItem::SubMenu(SubMenu {
                label: "Transform".into(),
                submenu: vec![
                    MenuItem::Standard(StandardItem {
                        label: "Rotate 90°".into(),
                        activate: Box::new(|tray: &mut Self| {
                            write_control(&tray.control_path, |state| state.rotate_by(90));
                        }),
                        ..Default::default()
                    }),
                    MenuItem::Checkmark(CheckmarkItem {
                        label: "Flip horizontal".into(),
                        checked: state.flip_h,
                        activate: Box::new(|tray: &mut Self| {
                            write_control(&tray.control_path, ControlState::toggle_flip_h);
                        }),
                        ..Default::default()
                    }),
                    MenuItem::Checkmark(CheckmarkItem {
                        label: "Flip vertical".into(),
                        checked: state.flip_v,
                        activate: Box::new(|tray: &mut Self| {
                            write_control(&tray.control_path, ControlState::toggle_flip_v);
                        }),
                        ..Default::default()
                    }),
                ],
                ..Default::default()
            }),
            MenuItem::SubMenu(SubMenu {
                label: "Region".into(),
                submenu: vec![
                    region_item("Off (1px)", "off"),
                    region_item("Auto", "auto"),
                    region_item("Grid 2px", "2"),
                    region_item("Grid 4px", "4"),
                ],
                ..Default::default()
            }),
            MenuItem::Checkmark(CheckmarkItem {
                label: "Lock position".into(),
                checked: state.locked,
                activate: Box::new(|tray: &mut Self| {
                    write_control(&tray.control_path, ControlState::toggle_locked);
                }),
                ..Default::default()
            }),
            MenuItem::Checkmark(CheckmarkItem {
                label: "Visible".into(),
                checked: !state.hidden,
                activate: Box::new(|tray: &mut Self| {
                    write_control(&tray.control_path, ControlState::toggle_hidden);
                }),
                ..Default::default()
            }),
            MenuItem::Separator,
            MenuItem::Standard(StandardItem {
                label: "Reset position and interaction".into(),
                activate: Box::new(|tray: &mut Self| {
                    write_control(&tray.control_path, ControlState::request_reset);
                }),
                ..Default::default()
            }),
            MenuItem::Standard(StandardItem {
                label: "Quit overlay".into(),
                activate: Box::new(|tray: &mut Self| {
                    write_control(&tray.control_path, ControlState::request_quit);
                }),
                ..Default::default()
            }),
        ]
    }
}

/// Apply `action` to every running instance's control file, so one keypress can
/// drive all overlays (the tray, in contrast, targets only its own instance).
fn broadcast_control(action: impl Fn(ControlState) -> ControlState + Copy) {
    let paths = ControlState::instances();
    if paths.is_empty() {
        write_control(&ControlState::path(), action);
        return;
    }
    for path in paths {
        write_control(&path, action);
    }
}

/// Start the tray in the background with a per-instance id/title. On niri (no
/// StatusNotifierWatcher by default) this logs and returns `None` unless a bar
/// such as waybar hosts one.
fn spawn_tray(
    control_path: PathBuf,
    slug: String,
    label: String,
) -> Option<ksni::blocking::Handle<OverlayTray>> {
    use ksni::blocking::TrayMethods;

    let tray = OverlayTray {
        control_path,
        id: format!("anima-engine-overlay-{slug}"),
        title: format!("AnimaEngine — {label}"),
    };
    match tray
        .disable_dbus_name(false)
        .assume_sni_available(true)
        .spawn()
    {
        Ok(handle) => {
            log::event(log::Level::Info, "overlay.tray.registered", &[]);
            Some(handle)
        }
        Err(err) => {
            log::failure("overlay.tray.unavailable", &err);
            None
        }
    }
}

/// Register global shortcuts through `xdg-desktop-portal` in the background.
/// This is the recovery path: once click-through is on, the surface itself can
/// no longer be clicked.
fn spawn_portal(slug: String) {
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(err) => {
                log::failure("overlay.portal.failed", &err);
                return;
            }
        };
        runtime.block_on(async move {
            if let Err(err) = run_portal(slug).await {
                log::failure("overlay.shortcuts.unavailable", &err);
            }
        });
    });
}

async fn run_portal(slug: String) -> Result<(), Box<dyn std::error::Error>> {
    use ashpd::desktop::CreateSessionOptions;
    use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut};
    use futures_util::StreamExt;

    let toggle_id = format!("toggle-click-through-{slug}");
    let reset_id = format!("reset-overlay-{slug}");

    let proxy = GlobalShortcuts::new().await?;
    let session = proxy
        .create_session(CreateSessionOptions::default())
        .await?;
    let shortcuts = [
        NewShortcut::new(toggle_id.clone(), "Toggle overlay click-through")
            .preferred_trigger("CTRL+SHIFT+t"),
        NewShortcut::new(reset_id.clone(), "Reset overlay position and interaction")
            .preferred_trigger("CTRL+SHIFT+r"),
    ];
    proxy
        .bind_shortcuts(&session, &shortcuts, None, BindShortcutsOptions::default())
        .await?;
    log::event(
        log::Level::Info,
        "overlay.shortcuts.bound",
        &[("bindings", &"Ctrl+Shift+T toggle, Ctrl+Shift+R reset")],
    );

    let mut activated = proxy.receive_activated().await?;
    while let Some(event) = activated.next().await {
        let id = event.shortcut_id();
        if id == toggle_id {
            broadcast_control(ControlState::toggle_click_through);
        } else if id == reset_id {
            broadcast_control(ControlState::request_reset);
        } else {
            log::event(
                log::Level::Debug,
                "overlay.shortcuts.unknown",
                &[("id", &id)],
            );
        }
    }

    let _ = session.close().await;
    Ok(())
}

/// UV matrix + offset for [`FrameCtl`]: display UV -> native UV.
fn uv_transform(orientation: Orientation) -> ([f32; 4], [f32; 2]) {
    let origin = orientation.display_to_native(0.0, 0.0);
    let du = orientation.display_to_native(1.0, 0.0);
    let dv = orientation.display_to_native(0.0, 1.0);
    (
        [
            (du.0 - origin.0) as f32,
            (du.1 - origin.1) as f32,
            (dv.0 - origin.0) as f32,
            (dv.1 - origin.1) as f32,
        ],
        [origin.0 as f32, origin.1 as f32],
    )
}

fn frame_delay(frame: &Frame, fps: u32) -> Duration {
    if fps > 0 {
        return Duration::from_millis(u64::from(1000 / fps.max(1)));
    }
    Duration::from_millis(u64::from(frame.effective_delay_ms()))
}

/// Detects that the displayed animation changed on disk, so the overlay can reload
/// it. Two signals: the file's own size/mtime (any animation, including a file the
/// user just re-exported) and [`WorkshopWatcher`] (an item the current file lives in
/// was re-published). The file stat is checked every second, the Workshop scan every
/// fifth second because it costs a `read_dir` per installed item.
struct HotReload {
    path: Option<PathBuf>,
    stat: Option<(u64, Option<SystemTime>)>,
    watcher: WorkshopWatcher,
    last_check: Instant,
    last_scan: Instant,
}

impl HotReload {
    fn new(path: Option<&Path>) -> Self {
        Self {
            path: path.map(Path::to_path_buf),
            stat: path.and_then(source_stat),
            watcher: WorkshopWatcher::discover(),
            last_check: Instant::now(),
            last_scan: Instant::now(),
        }
    }

    /// Whether the source changed since the last call (never for the test pattern).
    fn changed(&mut self) -> bool {
        let Some(path) = self.path.as_deref() else {
            return false;
        };
        let mut changed = false;
        if self.last_check.elapsed() >= Duration::from_secs(1) {
            self.last_check = Instant::now();
            let stat = source_stat(path);
            changed = stat != self.stat;
            self.stat = stat;
        }
        if !changed && self.last_scan.elapsed() >= Duration::from_secs(5) {
            self.last_scan = Instant::now();
            changed = self.watcher.poll().is_some();
        }
        changed
    }
}

fn source_stat(path: &Path) -> Option<(u64, Option<SystemTime>)> {
    std::fs::metadata(path)
        .ok()
        .map(|meta| (meta.len(), meta.modified().ok()))
}

/// Tray and portal commands arrive through the control file. Returns false when it asked the overlay to
/// quit, which ends the turn early — the loop said `continue` here, and a function cannot.
fn poll_control(
    // The binding, not the reference, has to be mutable: the body reborrows it several times.
    state: &mut OverlayState,
    control: &mut ControlState,
    control_mtime: &mut Option<SystemTime>,
    control_path: &Path,
    pos_x: i32,
    pos_y: i32,
) -> bool {
    // Tray / portal commands arrive through the control file.
    let mtime = std::fs::metadata(control_path)
        .and_then(|m| m.modified())
        .ok();
    if mtime != *control_mtime {
        *control_mtime = mtime;
        let next = ControlState::load(control_path);
        if next.generation != control.generation {
            *control = next;
            if control.reset {
                let (left, top) = clamp_margin(state, pos_x, pos_y);
                state.margin_left = left;
                state.margin_top = top;
                state.layer_surface.set_margin(top, 0, 0, left);
                state.surface.commit();
                // Reset restores the naive interaction state, like the
                // Python `_reset_interactions`.
                state.hover_hide = false;
                log::event(log::Level::Info, "overlay.reset", &[]);
            }
            apply_input_mode(state, control.input_mode);
            if control.scale_percent != state.scale_percent {
                apply_scale(state, control.scale_percent);
            }
            if control.locked != state.locked {
                state.locked = control.locked;
                log::event(
                    log::Level::Info,
                    "overlay.locked",
                    &[("locked", &state.locked)],
                );
            }
            if control.hidden != state.hidden {
                apply_hidden(state, control.hidden);
            }
            let orientation =
                Orientation::from_degrees(control.rotation, control.flip_h, control.flip_v);
            if orientation != state.orientation {
                apply_orientation(state, orientation);
            }
            let tuning = RegionTuning::from_control(control);
            if tuning != state.region_tuning {
                state.region_tuning = tuning;
                state.region_dirty = true;
                log::event(
                    log::Level::Info,
                    "overlay.region_limits",
                    &[("limits", &state.region_tuning.describe())],
                );
            }
            persist_geometry(state);
            if control.quit {
                state.closed = true;
                return false;
            }
        }
    }

    true
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `--no-tray` lets the shepherd own the single tray icon; the animation path is the
    // only other argument.
    let config = Config::load()?;
    // The same destructuring the code above this line used to do inline, so nothing below changed.
    let wayland = Wayland::connect(&config)?;
    let Config {
        path,
        tray_wanted,
        loaded,
        width,
        height,
        slug,
        label,
        control_path,
        pos_x,
        pos_y,
        ..
    } = config;
    let Loaded {
        animation,
        blocks,
        crop,
        limits: decode_limits,
    } = loaded;
    let Wayland {
        conn,
        mut event_queue,
        mut state,
        mut control,
        mut control_mtime,
    } = wayland;
    // The tray is main's business rather than the connection's: it is a handle that has to outlive the
    // program, and naming its type in a struct would say nothing useful about it.
    let _tray = if tray_wanted {
        spawn_tray(control_path.clone(), slug.clone(), label)
    } else {
        log::event(log::Level::Info, "overlay.tray.disabled", &[]);
        None
    };
    spawn_portal(slug);

    // --- wgpu surface from the raw Wayland handles -------------------------
    let (wgpu_surface, gpu) = Gpu::create(&conn, &state.surface, width, height, state.configured)?;
    // Destructured into the same names the rest of this function already used, so the move could not
    // change anything below it.
    let Gpu {
        device,
        queue,
        mut config,
        format,
        bc7,
        sampler: _,
        uniform,
        bind_group_layout,
    } = gpu.clone();
    // --- frame storage: BC7 blocks when available, else decoded RGBA, one layer either way ---
    let mut frames = frames::build(
        &gpu,
        Loaded {
            animation,
            blocks,
            crop,
            limits: decode_limits,
        },
    );

    #[cfg(feature = "bc7")]
    if frames.blocks.is_none() {
        spawn_bc7_encode(path.as_deref(), &frames.animation, crop, decode_limits, bc7);
    }

    let pipelines::Pipelines {
        render: pipeline,
        menu: menu_pipeline,
        menu_vbo,
    } = pipelines::build(&device, format, &bind_group_layout);

    // --- playback loop -----------------------------------------------------
    let mut index = 0usize;
    let mut next_change = Instant::now();
    let mut uploaded_layer0 = usize::MAX; // no layer uploaded yet
    let mut presented: u64 = 0;
    let mut last_presented: Option<usize> = None;
    let mut last_hide = false;
    let mut region_index: Option<usize> = None;
    let mut hot_reload = HotReload::new(path.as_deref());

    while !state.closed {
        shrink_frames(&mut frames, &mut state);
        // Snapshot before dispatch: every Motion in this batch shares one base.
        state.frame_margin_left = state.margin_left;
        state.frame_margin_top = state.margin_top;
        state.surface_size = (config.width, config.height);

        event_queue.dispatch_pending(&mut state)?;

        // Hot reload: everything that depends on the animation's geometry is rebuilt in
        // place, so a changed size, frame count or storage path needs no restart. The
        // state is updated first because `scaled_size`/`reapply_clamp` read it.
        if hot_reload.changed() {
            let loaded = load_animation(path.as_deref())?;
            let new_width = loaded.animation.width;
            let new_height = loaded.animation.height;
            let new_crop = loaded.crop;
            #[cfg(feature = "bc7")]
            let new_limits = loaded.limits;
            let needs_bc7 = loaded.blocks.is_none();

            state.animation = Arc::clone(&loaded.animation);
            state.native_size = (new_width, new_height);
            state.crop = new_crop;
            state.frame_index = 0;
            state.applied_rects.clear();
            state.region_dirty = true;
            state.visual_dirty = true;
            if !state.hidden {
                let (width, height) = fit_to_output(&state, scaled_size(&state));
                state.desired_size = (width, height);
                state.layer_surface.set_size(width, height);
                state.surface.commit();
            }

            frames = frames::build(&gpu, loaded);
            if needs_bc7 {
                #[cfg(feature = "bc7")]
                spawn_bc7_encode(
                    path.as_deref(),
                    &frames.animation,
                    new_crop,
                    new_limits,
                    bc7,
                );
            }
            index = 0;
            uploaded_layer0 = usize::MAX;
            last_presented = None;
            next_change = Instant::now();
            reapply_clamp(&mut state);
            log::event(
                log::Level::Debug,
                "overlay.rebuilt",
                &[
                    ("frames", &frames.frame_count()),
                    ("width", &new_width),
                    ("height", &new_height),
                ],
            );
            persist_geometry(&state);
        }

        if !poll_control(
            &mut state,
            &mut control,
            &mut control_mtime,
            &control_path,
            pos_x,
            pos_y,
        ) {
            continue;
        }
        if let Some((w, h)) = state.configured
            && (w != config.width || h != config.height)
        {
            config.width = w.max(1);
            config.height = h.max(1);
            wgpu_surface.configure(&device, &config);
            state.applied_rects.clear();
            reapply_clamp(&mut state);
        }

        let now = Instant::now();
        if now < next_change {
            std::thread::sleep((next_change - now).min(Duration::from_millis(10)));
            continue;
        }

        if uploaded_layer0 != index {
            upload_frame(&queue, &frames, index, 0);
            uploaded_layer0 = index;
        }
        let layer = 0u32;
        let menu_verts = state.menu.vertices(state.surface_size, state.input_mode);
        if !menu_verts.is_empty() {
            queue.write_buffer(&menu_vbo, 0, bytemuck::cast_slice(&menu_verts));
        }

        // Hidden (1x1 keep-alive) or hover-hide: draw nothing. The menu is always
        // drawn while open, even if hover-hide would otherwise blank the surface.
        let hide_now =
            !state.menu.open && (state.hidden || (state.hover_hide && state.pointer_over));

        // Skip presenting when the visible content did not change (a static
        // image) or while hidden; the compositor keeps the last frame.
        let present_now = !state.hidden
            && (state.visual_dirty || last_presented != Some(index) || hide_now != last_hide);
        if present_now {
            // Rotation/mirror can change at runtime through the control channel.
            let (uv_mat, uv_off) = uv_transform(state.orientation);
            // Block-compressed textures are padded to whole 4x4 blocks; scale the
            // UVs so only the real pixels are sampled (no squashed image).
            let (uv_mat, uv_off) = if frames.scale == (1.0, 1.0) {
                (uv_mat, uv_off)
            } else {
                (
                    [
                        uv_mat[0] * frames.scale.0,
                        uv_mat[1] * frames.scale.1,
                        uv_mat[2] * frames.scale.0,
                        uv_mat[3] * frames.scale.1,
                    ],
                    [uv_off[0] * frames.scale.0, uv_off[1] * frames.scale.1],
                )
            };
            queue.write_buffer(
                &uniform,
                0,
                bytemuck::bytes_of(&FrameCtl {
                    layer,
                    _pad: [0; 3],
                    uv_mat,
                    uv_off,
                    _pad2: [0.0; 2],
                }),
            );

            match wgpu_surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(frame)
                | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                    let view = frame.texture.create_view(&TextureViewDescriptor::default());
                    let mut encoder =
                        device.create_command_encoder(&CommandEncoderDescriptor::default());
                    {
                        let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                            label: Some("anima-pass"),
                            color_attachments: &[Some(RenderPassColorAttachment {
                                view: &view,
                                depth_slice: None,
                                resolve_target: None,
                                ops: Operations {
                                    load: LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                    store: StoreOp::Store,
                                },
                            })],
                            depth_stencil_attachment: None,
                            timestamp_writes: None,
                            occlusion_query_set: None,
                            multiview_mask: None,
                        });
                        pass.set_pipeline(&pipeline);
                        pass.set_bind_group(0, &frames.bind_group, &[]);
                        if !hide_now {
                            pass.draw(0..3, 0..1);
                        }
                        if !menu_verts.is_empty() {
                            pass.set_pipeline(&menu_pipeline);
                            pass.set_vertex_buffer(0, menu_vbo.slice(..));
                            pass.draw(0..menu_verts.len() as u32, 0..1);
                        }
                    }
                    queue.submit(std::iter::once(encoder.finish()));
                    frame.present();
                }
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                    wgpu_surface.configure(&device, &config);
                }
                _ => {}
            }

            last_presented = Some(index);
            last_hide = hide_now;
            state.visual_dirty = false;
        }

        // The frame just presented stays up for its own delay; hit-testing
        // must use the frame that is actually visible.
        let fps = if state.fps > 0 {
            state.fps
        } else {
            state.meta_fps
        };
        next_change = now + frame_delay(&frames.animation.frames[index], fps);
        state.frame_index = index;
        if state.pointer_inside {
            state.pointer_over = state.opaque_at(state.pointer_pos.0, state.pointer_pos.1);
        }
        // Input-region updates are throttled by frame count (not by wall-clock
        // delay); a size change forces one immediately, and a static image is
        // only scanned once.
        if !state.menu.open
            && state.input_mode == InputMode::Opaque
            && (state.region_dirty
                || (region_index != Some(index) && presented.is_multiple_of(REGION_UPDATE_FRAMES)))
        {
            apply_opaque_region(&mut state);
            state.region_dirty = false;
            region_index = Some(index);
        }
        presented = presented.wrapping_add(1);
        index = (index + 1) % frames.frame_count();
    }

    // Safety net: keep the last geometry even if we exit mid-interaction.
    persist_geometry(&state);
    // ...and stop claiming to be alive, so the window spawns a new instance instead of
    // writing into a file nobody reads.
    ControlState::clear_announce(&state.control_path);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::clamp_position;

    #[test]
    fn clamp_keeps_surface_fully_on_screen() {
        let output = (1920, 1080);
        let surface = (256, 256);

        assert_eq!(clamp_position(100, 100, surface, output), (100, 100));
        assert_eq!(clamp_position(5000, 5000, surface, output), (1664, 824));
        assert_eq!(clamp_position(-10, -10, surface, output), (0, 0));
        assert_eq!(
            clamp_position(i32::MAX, i32::MAX, surface, output),
            (1664, 824)
        );

        // a surface larger than the output is pinned at the origin
        assert_eq!(clamp_position(50, 50, (2000, 1200), output), (0, 0));
    }

    #[test]
    fn menu_layout_is_clamped_and_hit_testable() {
        use crate::menu::{Action, Menu};

        let mut menu = Menu::new();
        assert_eq!(menu.hit(10.0, 10.0), None);

        menu.open_at((300, 200), 295.0, 195.0);
        assert!(menu.open);
        // The panel is clamped inside the surface; first and last rows are usable.
        assert_eq!(menu.hit(250.0, 10.0), Some(Action::CycleInput));
        assert_eq!(menu.hit(250.0, 180.0), Some(Action::Quit));
        assert_eq!(menu.hit(-1.0, 10.0), None);

        menu.close();
        assert_eq!(menu.hit(250.0, 10.0), None);
    }
}
