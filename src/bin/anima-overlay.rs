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

use anima_engine_rs::anim::{Animation, Frame};
use anima_engine_rs::cache::FrameCache;

use smithay_client_toolkit::reexports::client::globals::{GlobalListContents, registry_queue_init};
use smithay_client_toolkit::reexports::client::protocol::{
    wl_compositor, wl_pointer, wl_region, wl_registry, wl_seat, wl_surface,
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
};

@group(0) @binding(0) var src: texture_2d_array<f32>;
@group(0) @binding(1) var smp: sampler;
@group(0) @binding(2) var<uniform> ctl: FrameCtl;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, smp, in.uv, i32(ctl.layer));
    // The surface uses premultiplied alpha; premultiply the straight RGBA frame.
    return vec4<f32>(c.rgb * c.a, c.a);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FrameCtl {
    layer: u32,
    _pad: [u32; 3],
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
    surface_size: (u32, u32),
}

impl OverlayState {
    /// True when the surface-local point hits an opaque pixel of the frame
    /// currently on screen. The frame is stretched to the surface.
    fn opaque_at(&self, sx: f64, sy: f64) -> bool {
        self.animation.alpha_hit(
            self.frame_index,
            self.surface_size,
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
        _: &mut Self,
        _: &wl_surface::WlSurface,
        _: wl_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
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
            }
            wl_pointer::Event::Leave { .. } => {
                state.dragging = false;
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                state.pointer_pos = (surface_x, surface_y);
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
                state.margin_left = new_left.max(0);
                state.margin_top = new_top.max(0);
                state
                    .layer_surface
                    .set_margin(state.margin_top, 0, 0, state.margin_left);
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
                        // Only drag when the press lands on a solid pixel.
                        if state.opaque_at(state.pointer_pos.0, state.pointer_pos.1) {
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

fn frame_delay(frame: &Frame) -> Duration {
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
    let (pos_x, pos_y) = (100i32, 100i32);

    let conn = Connection::connect_to_env()?;
    let mut event_queue: EventQueue<OverlayState> = conn.new_event_queue();
    let qh = event_queue.handle();
    let (globals, _) = registry_queue_init::<OverlayState>(&conn)?;

    let compositor: wl_compositor::WlCompositor = globals.bind(&qh, 1..=4, ())?;
    let layer_shell: zwlr_layer_shell_v1::ZwlrLayerShellV1 = globals.bind(&qh, 1..=4, ())?;
    let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=9, ())?;

    let surface = compositor.create_surface(&qh, ());
    let layer_surface = layer_shell.get_layer_surface(
        &surface,
        None,
        zwlr_layer_shell_v1::Layer::Overlay,
        "anima-overlay".to_string(),
        &qh,
        (),
    );
    layer_surface.set_size(width, height);
    layer_surface
        .set_anchor(zwlr_layer_surface_v1::Anchor::Top | zwlr_layer_surface_v1::Anchor::Left);
    layer_surface.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
    layer_surface.set_margin(pos_y, 0, 0, pos_x);
    // Receive input so we can drag on opaque pixels. For click-through, set an
    // empty input region instead:
    //   let empty = compositor.create_region(&qh, ());
    //   surface.set_input_region(Some(&empty));
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
        surface_size: (width, height),
    };
    event_queue.roundtrip(&mut state)?;

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
    let sampler = device.create_sampler(&SamplerDescriptor::default());
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
    let mut index = 0usize;
    let mut next_change = Instant::now();
    let mut uploaded_layer0 = 0usize;

    while !state.closed {
        // Snapshot before dispatch: every Motion in this batch shares one base.
        state.frame_margin_left = state.margin_left;
        state.frame_margin_top = state.margin_top;
        state.surface_size = (config.width, config.height);

        event_queue.dispatch_pending(&mut state)?;

        if let Some((w, h)) = state.configured
            && (w != config.width || h != config.height)
        {
            config.width = w.max(1);
            config.height = h.max(1);
            wgpu_surface.configure(&device, &config);
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
        queue.write_buffer(
            &uniform,
            0,
            bytemuck::bytes_of(&FrameCtl {
                layer,
                _pad: [0; 3],
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
                    pass.draw(0..3, 0..1);
                }
                queue.submit(std::iter::once(encoder.finish()));
                frame.present();
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                wgpu_surface.configure(&device, &config);
            }
            _ => {}
        }

        // The frame just presented stays up for its own delay; hit-testing
        // must use the frame that is actually visible.
        next_change = now + frame_delay(&animation.frames[index]);
        state.frame_index = index;
        index = (index + 1) % frame_count;
    }

    Ok(())
}
