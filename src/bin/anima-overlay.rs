//! `anima-overlay` — minimal always-on-top overlay scaffold.
//!
//! Wayland layer-shell (`zwlr_layer_shell_v1`, **Overlay** layer) + a `wgpu`
//! transparent surface, rendering one frame of an animation (or a generated
//! test pattern such as `anima-overlay`) so the pipeline can be verified on
//! niri/KDE/wlroots.
//!
//! Usage:
//! ```text
//! cargo run --features overlay --bin anima-overlay -- [animation.gif|.apng|.webp]
//! ```
//!
//! The process keeps the surface alive until closed by the compositor or
//! Ctrl+C. Text/visual acceptance is manual.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use smithay_client_toolkit::reexports::client::globals::{GlobalListContents, registry_queue_init};
use smithay_client_toolkit::reexports::client::protocol::{
    wl_compositor, wl_region, wl_registry, wl_surface,
};
use smithay_client_toolkit::reexports::client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
};
use smithay_client_toolkit::reexports::protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1, zwlr_layer_surface_v1,
};

use wgpu::rwh::{RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle};
use wgpu::{
    BindGroupDescriptor, BindGroupEntry, BindGroupLayoutDescriptor, BindGroupLayoutEntry,
    BindingResource, BindingType, BlendState, ColorTargetState, ColorWrites,
    CommandEncoderDescriptor, CompositeAlphaMode, DeviceDescriptor, Extent3d, FragmentState,
    LoadOp, MultisampleState, Operations, Origin3d, PipelineLayoutDescriptor, PresentMode,
    PrimitiveState, RenderPassColorAttachment, RenderPassDescriptor, RenderPipelineDescriptor,
    SamplerBindingType, SamplerDescriptor, ShaderModuleDescriptor, ShaderSource, ShaderStages,
    StoreOp, SurfaceConfiguration, SurfaceTargetUnsafe, TexelCopyBufferLayout,
    TexelCopyTextureInfo, TextureAspect, TextureDescriptor, TextureDimension, TextureFormat,
    TextureSampleType, TextureUsages, TextureViewDescriptor, TextureViewDimension, VertexState,
};

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

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, smp, in.uv);
    // The surface uses premultiplied alpha; premultiply the straight RGBA frame.
    return vec4<f32>(c.rgb * c.a, c.a);
}
"#;

struct OverlayState {
    configured: Option<(u32, u32)>,
    closed: bool,
    surface: wl_surface::WlSurface,
    layer_surface: zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
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

/// Returns `(width, height, straight RGBA8)` for the first animation frame,
/// or a generated test pattern when no path is given.
fn load_frame(path: Option<&Path>) -> Result<(u32, u32, Vec<u8>), Box<dyn std::error::Error>> {
    if let Some(path) = path {
        let animation = anima_engine_rs::anim::decode(path)?;
        let frame = animation.frames.first().ok_or("animation has no frames")?;
        return Ok((animation.width, animation.height, frame.rgba.clone()));
    }
    Ok(test_pattern())
}

fn test_pattern() -> (u32, u32, Vec<u8>) {
    let (width, height) = (256u32, 256u32);
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    let (cx, cy, radius) = (128.0f32, 128.0f32, 100.0f32);
    for y in 0..height {
        for x in 0..width {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let inside = (dx * dx + dy * dy).sqrt() <= radius;
            let i = ((y * width + x) * 4) as usize;
            rgba[i] = (x * 255 / width) as u8;
            rgba[i + 1] = (y * 255 / height) as u8;
            rgba[i + 2] = 200;
            rgba[i + 3] = if inside { 255 } else { 0 };
        }
    }
    (width, height, rgba)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).map(PathBuf::from);
    let (mut frame_width, mut frame_height, rgba) = load_frame(path.as_deref())?;
    let (pos_x, pos_y) = (100i32, 100i32);

    let conn = Connection::connect_to_env()?;
    let mut event_queue: EventQueue<OverlayState> = conn.new_event_queue();
    let qh = event_queue.handle();
    let (globals, _) = registry_queue_init::<OverlayState>(&conn)?;

    let compositor: wl_compositor::WlCompositor = globals.bind(&qh, 1..=4, ())?;
    let layer_shell: zwlr_layer_shell_v1::ZwlrLayerShellV1 = globals.bind(&qh, 1..=4, ())?;

    let surface = compositor.create_surface(&qh, ());
    let layer_surface = layer_shell.get_layer_surface(
        &surface,
        None,
        zwlr_layer_shell_v1::Layer::Overlay,
        "anima-overlay".to_string(),
        &qh,
        (),
    );
    layer_surface.set_size(frame_width, frame_height);
    layer_surface
        .set_anchor(zwlr_layer_surface_v1::Anchor::Top | zwlr_layer_surface_v1::Anchor::Left);
    layer_surface.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
    layer_surface.set_margin(pos_y, 0, 0, pos_x);
    // Receive input (dragging/menu later). For click-through, set an empty region:
    //   let empty = compositor.create_region(&qh, ());
    //   surface.set_input_region(Some(&empty));
    surface.set_input_region(None);
    surface.commit();

    let mut state = OverlayState {
        configured: None,
        closed: false,
        surface,
        layer_surface,
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

    if let Some((w, h)) = state.configured {
        frame_width = w;
        frame_height = h;
    }
    let mut config = SurfaceConfiguration {
        usage: TextureUsages::RENDER_ATTACHMENT,
        format,
        width: frame_width.max(1),
        height: frame_height.max(1),
        present_mode,
        alpha_mode,
        view_formats: vec![],
        desired_maximum_frame_latency: 1,
    };
    wgpu_surface.configure(&device, &config);

    // --- frame texture + pipeline ------------------------------------------
    let texture = device.create_texture(&TextureDescriptor {
        label: Some("anima-frame"),
        size: Extent3d {
            width: frame_width.max(1),
            height: frame_height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: TextureFormat::Rgba8UnormSrgb,
        usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: Origin3d::ZERO,
            aspect: TextureAspect::All,
        },
        &rgba,
        TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(frame_width * 4),
            rows_per_image: Some(frame_height),
        },
        Extent3d {
            width: frame_width,
            height: frame_height,
            depth_or_array_layers: 1,
        },
    );
    let texture_view = texture.create_view(&TextureViewDescriptor::default());
    let sampler = device.create_sampler(&SamplerDescriptor::default());

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
                    view_dimension: TextureViewDimension::D2,
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

    // --- present loop ------------------------------------------------------
    let frame_interval = Duration::from_millis(33);
    let mut last_tick = Instant::now();
    while !state.closed {
        event_queue.dispatch_pending(&mut state)?;

        if let Some((w, h)) = state.configured
            && (w != config.width || h != config.height)
        {
            config.width = w.max(1);
            config.height = h.max(1);
            wgpu_surface.configure(&device, &config);
        }

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

        let elapsed = last_tick.elapsed();
        if elapsed < frame_interval {
            std::thread::sleep(frame_interval - elapsed);
        }
        last_tick = Instant::now();
    }

    Ok(())
}
