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

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anima_engine_rs::anim::{Animation, Frame, Orientation};
use anima_engine_rs::cache::FrameCache;
use anima_engine_rs::control::{ControlState, InputMode, MAX_SCALE_PERCENT, MIN_SCALE_PERCENT};
use anima_engine_rs::state::SecondaryState;

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
    DeviceDescriptor, Extent3d, FragmentState, LoadOp, MultisampleState, Operations, Origin3d,
    PipelineLayoutDescriptor, PresentMode, PrimitiveState, RenderPassColorAttachment,
    RenderPassDescriptor, RenderPipelineDescriptor, SamplerBindingType, SamplerDescriptor,
    ShaderModuleDescriptor, ShaderSource, ShaderStages, StoreOp, SurfaceConfiguration,
    SurfaceTargetUnsafe, TexelCopyBufferLayout, TexelCopyTextureInfo, TextureAspect,
    TextureDescriptor, TextureDimension, TextureFormat, TextureSampleType, TextureUsages,
    TextureViewDescriptor, TextureViewDimension, VertexState,
};

/// GPU budget for a fully pre-uploaded frame array. Larger animations fall back
/// to uploading the current frame just in time.
const ARRAY_BUDGET: u64 = 256 * 1024 * 1024;

/// Alpha value (0-255) at or above which a pixel counts as "solid" for drag.
const ALPHA_HIT_THRESHOLD: u8 = 25;

/// Fallback window position when an instance has no saved geometry.
const DEFAULT_POS: (i32, i32) = (100, 100);

/// Linux evdev `BTN_LEFT`.
const BTN_LEFT: u32 = 0x110;

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

struct OverlayState {
    configured: Option<(u32, u32)>,
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
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                state.pointer_pos = (surface_x, surface_y);
                state.pointer_over = state.opaque_at(surface_x, surface_y);
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
                if button != BTN_LEFT {
                    return;
                }
                match button_state {
                    WEnum::Value(wl_pointer::ButtonState::Pressed) => {
                        // A locked instance ignores presses; otherwise only drag
                        // when the press lands on a solid pixel.
                        if !state.locked
                            && state.opaque_at(state.pointer_pos.0, state.pointer_pos.1)
                        {
                            state.dragging = true;
                            state.drag_start = state.pointer_pos;
                            eprintln!(
                                "[overlay] drag start at ({:.0}, {:.0})",
                                state.pointer_pos.0, state.pointer_pos.1
                            );
                        }
                    }
                    WEnum::Value(wl_pointer::ButtonState::Released) => {
                        if state.dragging {
                            eprintln!(
                                "[overlay] drag end; margins ({}, {})",
                                state.margin_left, state.margin_top
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
                if width > 0 && height > 0 {
                    state.configured = Some((width, height));
                }
                state.layer_surface.ack_configure(serial);
            }
            zwlr_layer_surface_v1::Event::Closed => {
                state.closed = true;
            }
            _ => {}
        }
    }
}

/// Load an animation, using the frame cache so a file is decoded only once.
fn load_animation(path: Option<&Path>) -> Result<Arc<Animation>, Box<dyn std::error::Error>> {
    let Some(path) = path else {
        return Ok(Arc::new(test_animation()));
    };

    let cache = FrameCache::user_default();
    if let Some(animation) = cache.read(path) {
        eprintln!(
            "[overlay] cache hit: {} frame(s), {}x{}",
            animation.frame_count(),
            animation.width,
            animation.height
        );
        return Ok(Arc::new(animation));
    }

    let started = Instant::now();
    let animation = Arc::new(anima_engine_rs::anim::decode(path)?);
    eprintln!(
        "[overlay] decoded {} frame(s), {}x{} in {:.2}s",
        animation.frame_count(),
        animation.width,
        animation.height,
        started.elapsed().as_secs_f32()
    );

    // Encode/write the cache in the background so playback is not blocked by
    // PNG compression of large animations.
    let cache_animation = Arc::clone(&animation);
    let cache_path = path.to_path_buf();
    std::thread::spawn(move || {
        if let Err(err) = cache.write(&cache_path, &cache_animation) {
            eprintln!("[overlay] cache write failed: {err}");
        } else {
            eprintln!(
                "[overlay] cached {} frame(s)",
                cache_animation.frame_count()
            );
        }
    });

    Ok(animation)
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
        frames.push(Frame { rgba, delay_ms: 70 });
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
            eprintln!("[overlay] input: full surface");
        }
        InputMode::PassThrough => {
            state.region = None;
            state.applied_rects.clear();
            state.surface.set_input_region(Some(&state.empty_region));
            state.surface.commit();
            eprintln!("[overlay] input: click-through");
        }
        InputMode::Opaque => {
            apply_opaque_region(state);
            eprintln!(
                "[overlay] input: opaque pixels ({} rects, step {})",
                state.applied_rects.len(),
                state.applied_step
            );
        }
    }
}

/// Build an input region from the opaque pixels of the visible frame and install
/// it, so transparent pixels pass clicks through to whatever is below. Very
/// detailed silhouettes are sampled on a coarser grid, then fall back to their
/// bounding box.
fn apply_opaque_region(state: &mut OverlayState) {
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
    if rects == state.applied_rects && state.region.is_some() {
        return;
    }

    let region = state.compositor.create_region(&state.qh, ());
    for &(x, y, w, h) in &rects {
        region.add(x, y, w, h);
    }
    state.surface.set_input_region(Some(&region));
    state.surface.commit();
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
    clamp_position(left, top, scaled_size(state), output)
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
    eprintln!("[overlay] clamped position to ({left}, {top})");
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
        eprintln!("[overlay] scale {percent}% (hidden)");
    } else {
        let (width, height) = scaled_size(state);
        state.layer_surface.set_size(width, height);
        state.surface.commit();
        state.applied_rects.clear();
        eprintln!("[overlay] scale {percent}% -> {width}x{height}");
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
    let (width, height) = if hidden { (1, 1) } else { scaled_size(state) };
    state.layer_surface.set_size(width, height);
    state.surface.commit();
    state.applied_rects.clear();
    eprintln!(
        "[overlay] {}",
        if hidden {
            "hidden (1x1 keep-alive)"
        } else {
            "shown"
        }
    );
    reapply_clamp(state);
}

/// Persist position, display size, click-through, lock and hover-hide for this
/// instance, preserving other instances and fields in `secondary_instances.json`.
fn persist_geometry(state: &OverlayState) {
    let Some(key) = state.instance_key.as_deref() else {
        return;
    };
    let click_through = state.input_mode == InputMode::PassThrough;
    let mut document = SecondaryState::load(&state.state_path);
    let changed = {
        let instance = document.entry(key);
        let changed = instance.x != state.margin_left
            || instance.y != state.margin_top
            || instance.scale_percent != state.scale_percent
            || instance.click_through != click_through
            || instance.locked != state.locked
            || instance.hover_hide != state.hover_hide;
        if changed {
            instance.x = state.margin_left;
            instance.y = state.margin_top;
            instance.scale_percent = state.scale_percent;
            instance.click_through = click_through;
            instance.locked = state.locked;
            instance.hover_hide = state.hover_hide;
        }
        changed
    };
    if changed && let Err(err) = document.save(&state.state_path) {
        eprintln!("[overlay] could not save instance state: {err}");
    }
}

/// Read-modify-write helper for the control file.
fn write_control(path: &Path, action: impl FnOnce(ControlState) -> ControlState) {
    let next = action(ControlState::load(path));
    if let Err(err) = next.save(path) {
        eprintln!("[overlay] control write failed: {err}");
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
            eprintln!("[overlay] tray registered (StatusNotifierItem)");
            Some(handle)
        }
        Err(err) => {
            eprintln!("[overlay] tray unavailable (no StatusNotifierWatcher?): {err}");
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
                eprintln!("[overlay] portal runtime failed: {err}");
                return;
            }
        };
        runtime.block_on(async move {
            if let Err(err) = run_portal(slug).await {
                eprintln!("[overlay] global shortcuts unavailable: {err}");
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
    eprintln!("[overlay] global shortcuts bound: Ctrl+Shift+T toggle, Ctrl+Shift+R reset");

    let mut activated = proxy.receive_activated().await?;
    while let Some(event) = activated.next().await {
        let id = event.shortcut_id();
        if id == toggle_id {
            broadcast_control(ControlState::toggle_click_through);
        } else if id == reset_id {
            broadcast_control(ControlState::request_reset);
        } else {
            eprintln!("[overlay] unknown shortcut activated: {id}");
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
    let ms = if frame.delay_ms == 0 {
        33
    } else {
        frame.delay_ms
    };
    Duration::from_millis(u64::from(ms))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).map(PathBuf::from);
    let animation = load_animation(path.as_deref())?;
    let frame_count = animation.frames.len();
    if frame_count == 0 {
        return Err("animation has no frames".into());
    }
    let (width, height) = (animation.width.max(1), animation.height.max(1));

    // Instance identity for persistence: the canonical animation path. The
    // generated test pattern has no path, so it is never persisted.
    let instance_key = path
        .as_deref()
        .map(|path| anima_engine_rs::paths::canonical_path(&path.to_string_lossy()));
    // Identity for the per-instance control file and tray item.
    let slug = instance_key.as_deref().map_or_else(
        || "default".to_string(),
        anima_engine_rs::control::instance_slug,
    );
    let label = path
        .as_deref()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "test pattern".to_string());
    let state_path = SecondaryState::user_path();
    let region_tuning = RegionTuning::from_env();
    eprintln!("[overlay] region limits: {}", region_tuning.describe());
    let persisted = instance_key
        .as_deref()
        .and_then(|key| SecondaryState::load(&state_path).find_by_key(key).cloned());
    let (pos_x, pos_y) = persisted.as_ref().map_or(DEFAULT_POS, |instance| {
        (instance.x.max(0), instance.y.max(0))
    });
    let initial_scale = persisted
        .as_ref()
        .map_or(MAX_SCALE_PERCENT, |instance| instance.scale_percent)
        .clamp(MIN_SCALE_PERCENT, MAX_SCALE_PERCENT);
    let always_on_top = persisted.as_ref().is_none_or(|i| i.always_on_top);
    let initial_mode = if persisted.as_ref().is_some_and(|i| i.click_through) {
        InputMode::PassThrough
    } else {
        InputMode::Opaque
    };
    let initial_locked = persisted.as_ref().is_some_and(|i| i.locked);
    let initial_hover_hide = persisted.as_ref().is_some_and(|i| i.hover_hide);
    let fps = persisted.as_ref().map_or(0, |instance| instance.fps);
    let orientation = Orientation::from_degrees(
        persisted.as_ref().map_or(0, |i| i.rotation),
        persisted.as_ref().is_some_and(|i| i.flip_h),
        persisted.as_ref().is_some_and(|i| i.flip_v),
    );
    let (disp_w, disp_h) = orientation.size((width, height));
    if persisted.is_some() {
        eprintln!(
            "[overlay] restored instance: ({pos_x}, {pos_y}) at {initial_scale}%, \
             mode {initial_mode:?}, locked {initial_locked}, hover_hide {initial_hover_hide}, \
             fps {fps}, quarters {}, flip_h {}, flip_v {}",
            orientation.quarter_turns, orientation.flip_h, orientation.flip_v
        );
    }

    let conn = Connection::connect_to_env()?;
    let mut event_queue: EventQueue<OverlayState> = conn.new_event_queue();
    let qh = event_queue.handle();
    let (globals, _) = registry_queue_init::<OverlayState>(&conn)?;

    let compositor: wl_compositor::WlCompositor = globals.bind(&qh, 1..=4, ())?;
    let layer_shell: zwlr_layer_shell_v1::ZwlrLayerShellV1 = globals.bind(&qh, 1..=4, ())?;
    let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=9, ())?;
    let outputs = OutputState::new(&globals, &qh);

    let surface = compositor.create_surface(&qh, ());
    let layer = if always_on_top {
        zwlr_layer_shell_v1::Layer::Overlay
    } else {
        zwlr_layer_shell_v1::Layer::Bottom
    };
    let layer_surface =
        layer_shell.get_layer_surface(&surface, None, layer, "anima-overlay".to_string(), &qh, ());
    let scaled = |value: u32| (u64::from(value) * u64::from(initial_scale) / 100).max(1) as u32;
    layer_surface.set_size(scaled(disp_w), scaled(disp_h));
    layer_surface
        .set_anchor(zwlr_layer_surface_v1::Anchor::Top | zwlr_layer_surface_v1::Anchor::Left);
    layer_surface.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
    layer_surface.set_margin(pos_y, 0, 0, pos_x);
    // Receive input so we can drag on opaque pixels. The empty region toggled
    // by the tray / portal makes the whole surface click-through.
    let empty_region = compositor.create_region(&qh, ());
    surface.set_input_region(None);
    surface.commit();

    let mut state = OverlayState {
        configured: None,
        closed: false,
        surface,
        layer_surface,
        _seat: seat,
        _pointer: None,
        pointer_pos: (0.0, 0.0),
        dragging: false,
        drag_start: (0.0, 0.0),
        margin_left: pos_x,
        margin_top: pos_y,
        frame_margin_left: pos_x,
        frame_margin_top: pos_y,
        animation: Arc::clone(&animation),
        frame_index: 0,
        native_size: (width, height),
        scale_percent: initial_scale,
        orientation,
        surface_size: (scaled(disp_w), scaled(disp_h)),
        compositor: compositor.clone(),
        qh: qh.clone(),
        empty_region,
        input_mode: InputMode::Full,
        region: None,
        applied_rects: Vec::new(),
        instance_key,
        state_path,
        fps,
        locked: initial_locked,
        hover_hide: initial_hover_hide,
        hidden: false,
        pointer_inside: false,
        pointer_over: false,
        outputs,
        active_output: None,
        last_output_size: None,
        region_dirty: false,
        visual_dirty: true,
        region_tuning,
        applied_step: 1,
    };
    event_queue.roundtrip(&mut state)?;
    // Configures the input region for the restored mode (Opaque builds the
    // per-pixel region; PassThrough installs the empty one).
    apply_input_mode(&mut state, initial_mode);

    // --- control channel: tray + portal global shortcuts -------------------
    let control_path = ControlState::path_for(state.instance_key.as_deref());
    let mut control = ControlState::fresh();
    // Seed the runtime channel with the persisted display size so a CLI
    // read-modify-write cannot silently reset it.
    control.scale_percent = initial_scale;
    control.input_mode = initial_mode;
    control.locked = initial_locked;
    control.region_step = region_tuning.step.label();
    control.region_soft_rects = region_tuning.soft_rects;
    control.region_max_rects = region_tuning.max_rects;
    if let Err(err) = control.save(&control_path) {
        eprintln!("[overlay] control file init failed: {err}");
    }
    let mut control_mtime = std::fs::metadata(&control_path)
        .and_then(|m| m.modified())
        .ok();
    let _tray = spawn_tray(control_path.clone(), slug.clone(), label);
    spawn_portal(slug);

    // --- wgpu surface from the raw Wayland handles -------------------------
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let display_ptr = conn.backend().display_ptr() as *mut c_void;
    let surface_ptr = state.surface.id().as_ptr() as *mut c_void;
    let raw_display = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
        NonNull::new(display_ptr).expect("wayland display ptr"),
    ));
    let raw_window = RawWindowHandle::Wayland(WaylandWindowHandle::new(
        NonNull::new(surface_ptr).expect("wayland surface ptr"),
    ));
    let wgpu_surface = unsafe {
        instance.create_surface_unsafe(SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(raw_display),
            raw_window_handle: raw_window,
        })?
    };

    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: Some(&wgpu_surface),
        force_fallback_adapter: false,
    }))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&DeviceDescriptor::default()))?;

    let limits = adapter.limits();
    if width > limits.max_texture_dimension_2d || height > limits.max_texture_dimension_2d {
        return Err(format!(
            "frame {width}x{height} exceeds max texture size {}",
            limits.max_texture_dimension_2d
        )
        .into());
    }

    let capabilities = wgpu_surface.get_capabilities(&adapter);
    let format = capabilities
        .formats
        .iter()
        .copied()
        .find(|f| f.is_srgb())
        .unwrap_or(capabilities.formats[0]);
    let alpha_mode = capabilities
        .alpha_modes
        .iter()
        .copied()
        .find(|m| *m == CompositeAlphaMode::PreMultiplied)
        .unwrap_or(capabilities.alpha_modes[0]);
    let present_mode = capabilities
        .present_modes
        .iter()
        .copied()
        .find(|m| *m == PresentMode::Fifo)
        .unwrap_or(capabilities.present_modes[0]);

    let (surface_width, surface_height) = state.configured.unwrap_or((width, height));
    let mut config = SurfaceConfiguration {
        usage: TextureUsages::RENDER_ATTACHMENT,
        format,
        width: surface_width.max(1),
        height: surface_height.max(1),
        present_mode,
        alpha_mode,
        view_formats: vec![],
        desired_maximum_frame_latency: 1,
    };
    wgpu_surface.configure(&device, &config);

    // --- frame storage: whole array if it fits, else one re-uploaded layer --
    let frame_bytes = u64::from(width) * u64::from(height) * 4;
    let total_bytes = frame_bytes * frame_count as u64;
    let use_array =
        frame_count as u32 <= limits.max_texture_array_layers && total_bytes <= ARRAY_BUDGET;
    let layers = if use_array { frame_count as u32 } else { 1 };
    eprintln!(
        "[overlay] {} frame(s) {}x{} — {:.1} MiB decoded, {}",
        frame_count,
        width,
        height,
        total_bytes as f64 / (1024.0 * 1024.0),
        if use_array {
            "array texture"
        } else {
            "just-in-time upload"
        }
    );

    let texture = device.create_texture(&TextureDescriptor {
        label: Some("anima-frames"),
        size: Extent3d {
            width,
            height,
            depth_or_array_layers: layers,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: TextureFormat::Rgba8UnormSrgb,
        usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let upload_frame = |index: usize, layer: u32| {
        queue.write_texture(
            TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: Origin3d {
                    x: 0,
                    y: 0,
                    z: layer,
                },
                aspect: TextureAspect::All,
            },
            &animation.frames[index].rgba,
            TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    };
    if use_array {
        for index in 0..frame_count {
            upload_frame(index, index as u32);
        }
    } else {
        upload_frame(0, 0);
    }

    let texture_view = texture.create_view(&TextureViewDescriptor {
        dimension: Some(TextureViewDimension::D2Array),
        ..Default::default()
    });
    // Linear minification keeps downscaled frames (scale < 100%) from aliasing.
    let sampler = device.create_sampler(&SamplerDescriptor {
        min_filter: wgpu::FilterMode::Linear,
        mag_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let uniform = device.create_buffer(&BufferDescriptor {
        label: Some("anima-frame-ctl"),
        size: std::mem::size_of::<FrameCtl>() as u64,
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let shader = device.create_shader_module(ShaderModuleDescriptor {
        label: Some("anima-shader"),
        source: ShaderSource::Wgsl(SHADER.into()),
    });
    let bind_group_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
        label: Some("anima-bgl"),
        entries: &[
            BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::FRAGMENT,
                ty: BindingType::Texture {
                    sample_type: TextureSampleType::Float { filterable: true },
                    view_dimension: TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            },
            BindGroupLayoutEntry {
                binding: 1,
                visibility: ShaderStages::FRAGMENT,
                ty: BindingType::Sampler(SamplerBindingType::Filtering),
                count: None,
            },
            BindGroupLayoutEntry {
                binding: 2,
                visibility: ShaderStages::FRAGMENT,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let bind_group = device.create_bind_group(&BindGroupDescriptor {
        label: Some("anima-bg"),
        layout: &bind_group_layout,
        entries: &[
            BindGroupEntry {
                binding: 0,
                resource: BindingResource::TextureView(&texture_view),
            },
            BindGroupEntry {
                binding: 1,
                resource: BindingResource::Sampler(&sampler),
            },
            BindGroupEntry {
                binding: 2,
                resource: BindingResource::Buffer(BufferBinding {
                    buffer: &uniform,
                    offset: 0,
                    size: None,
                }),
            },
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
        label: Some("anima-pl"),
        bind_group_layouts: &[Some(&bind_group_layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
        label: Some("anima-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: PrimitiveState::default(),
        depth_stencil: None,
        multisample: MultisampleState::default(),
        fragment: Some(FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(ColorTargetState {
                format,
                blend: Some(BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });

    // --- playback loop -----------------------------------------------------
    let (uv_mat, uv_off) = uv_transform(state.orientation);
    let mut index = 0usize;
    let mut next_change = Instant::now();
    let mut uploaded_layer0 = 0usize;
    let mut presented: u64 = 0;
    let mut last_presented: Option<usize> = None;
    let mut last_hide = false;
    let mut region_index: Option<usize> = None;

    while !state.closed {
        // Snapshot before dispatch: every Motion in this batch shares one base.
        state.frame_margin_left = state.margin_left;
        state.frame_margin_top = state.margin_top;
        state.surface_size = (config.width, config.height);

        event_queue.dispatch_pending(&mut state)?;

        // Tray / portal commands arrive through the control file.
        let mtime = std::fs::metadata(&control_path)
            .and_then(|m| m.modified())
            .ok();
        if mtime != control_mtime {
            control_mtime = mtime;
            let next = ControlState::load(&control_path);
            if next.generation != control.generation {
                control = next;
                if control.reset {
                    let (left, top) = clamp_margin(&state, pos_x, pos_y);
                    state.margin_left = left;
                    state.margin_top = top;
                    state.layer_surface.set_margin(top, 0, 0, left);
                    state.surface.commit();
                    // Reset restores the naive interaction state, like the
                    // Python `_reset_interactions`.
                    state.hover_hide = false;
                    eprintln!("[overlay] reset to initial position");
                }
                apply_input_mode(&mut state, control.input_mode);
                if control.scale_percent != state.scale_percent {
                    apply_scale(&mut state, control.scale_percent);
                }
                if control.locked != state.locked {
                    state.locked = control.locked;
                    eprintln!("[overlay] locked {}", state.locked);
                }
                if control.hidden != state.hidden {
                    apply_hidden(&mut state, control.hidden);
                }
                let tuning = RegionTuning::from_control(&control);
                if tuning != state.region_tuning {
                    state.region_tuning = tuning;
                    state.region_dirty = true;
                    eprintln!(
                        "[overlay] region limits: {}",
                        state.region_tuning.describe()
                    );
                }
                persist_geometry(&state);
                if control.quit {
                    state.closed = true;
                    continue;
                }
            }
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

        let layer = if use_array {
            index as u32
        } else {
            if uploaded_layer0 != index {
                upload_frame(index, 0);
                uploaded_layer0 = index;
            }
            0
        };
        // Hidden (1x1 keep-alive) or hover-hide: draw nothing.
        let hide_now = state.hidden || (state.hover_hide && state.pointer_over);

        // Skip presenting when the visible content did not change (a static
        // image) or while hidden; the compositor keeps the last frame.
        let present_now = !state.hidden
            && (state.visual_dirty || last_presented != Some(index) || hide_now != last_hide);
        if present_now {
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
                        pass.set_bind_group(0, &bind_group, &[]);
                        if !hide_now {
                            pass.draw(0..3, 0..1);
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
        next_change = now + frame_delay(&animation.frames[index], state.fps);
        state.frame_index = index;
        if state.pointer_inside {
            state.pointer_over = state.opaque_at(state.pointer_pos.0, state.pointer_pos.1);
        }
        // Input-region updates are throttled by frame count (not by wall-clock
        // delay); a size change forces one immediately, and a static image is
        // only scanned once.
        if state.input_mode == InputMode::Opaque
            && (state.region_dirty
                || (region_index != Some(index) && presented.is_multiple_of(REGION_UPDATE_FRAMES)))
        {
            apply_opaque_region(&mut state);
            state.region_dirty = false;
            region_index = Some(index);
        }
        presented = presented.wrapping_add(1);
        index = (index + 1) % frame_count;
    }

    // Safety net: keep the last geometry even if we exit mid-interaction.
    persist_geometry(&state);

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
}
