//! The GPU half of the overlay: a wgpu surface over the Wayland handles, the device, and the handful of
//! resources every frame needs.
//!
//! Moved out of `main` verbatim. It is also the fix for an argument list that had grown to eight: a
//! `Gpu` is what `frames::build` wanted all along, so its call sites go back to passing two things.

use crate::*;
use anima_engine_rs::log;

/// Everything here is an Arc-backed wgpu handle, so cloning is a refcount bump. The surface is not a field:
/// it belongs to the window, and it is not `Clone`, which is what settled the question.
#[derive(Clone)]
pub(super) struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub config: SurfaceConfiguration,
    pub format: wgpu::TextureFormat,
    pub bc7: bool,
    pub sampler: wgpu::Sampler,
    pub uniform: wgpu::Buffer,
    pub bind_group_layout: wgpu::BindGroupLayout,
}

impl Gpu {
    pub(super) fn create(
        conn: &Connection,
        surface: &wl_surface::WlSurface,
        width: u32,
        height: u32,
        configured: Option<(u32, u32)>,
    ) -> Result<(wgpu::Surface<'static>, Gpu), Box<dyn std::error::Error>> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let display_ptr = conn.backend().display_ptr() as *mut c_void;
        let surface_ptr = surface.id().as_ptr() as *mut c_void;
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
        // Block-compressed frames (BC7) need this device feature; ask for it when the
        // adapter offers it, and say so — the GPU half of the BC7 route depends on it.
        let adapter_features = adapter.features();
        let bc7 = adapter_features.contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
        let mut descriptor = wgpu::DeviceDescriptor::default();
        if bc7 {
            descriptor.required_features = wgpu::Features::TEXTURE_COMPRESSION_BC;
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&descriptor))?;
        log::event(
            log::Level::Info,
            "overlay.gpu",
            &[
                ("adapter", &adapter.get_info().name),
                ("bc7", &(if bc7 { "supported" } else { "unavailable" })),
            ],
        );

        let limits = adapter.limits(); // the check below runs before a device exists
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

        let (surface_width, surface_height) = configured.unwrap_or((width, height));
        let config = SurfaceConfiguration {
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

        // Created before the frame storage because a rebuild needs them to make the new
        // bind group; none of them depends on the animation.
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

        Ok((
            wgpu_surface,
            Gpu {
                device,
                queue,
                config,
                format,
                bc7,
                sampler,
                uniform,
                bind_group_layout,
            },
        ))
    }
}
