//! The render pipelines, built once at startup.
//!
//! Moved out of `main` verbatim. It is the most independent part of it: nothing here reads the animation,
//! the Wayland state or the control channel — only the device, the target format, and the bind group layout
//! the frames use.

use crate::*;

pub(super) struct Pipelines {
    pub render: wgpu::RenderPipeline,
    pub menu: wgpu::RenderPipeline,
    pub menu_vbo: wgpu::Buffer,
}

pub(super) fn build(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    bind_group_layout: &wgpu::BindGroupLayout,
) -> Pipelines {
    let shader = device.create_shader_module(ShaderModuleDescriptor {
        label: Some("anima-shader"),
        source: ShaderSource::Wgsl(SHADER.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
        label: Some("anima-pl"),
        bind_group_layouts: &[Some(bind_group_layout)],
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

    // --- context-menu pipeline (solid quads, no textures) ------------------
    let menu_shader = device.create_shader_module(ShaderModuleDescriptor {
        label: Some("anima-menu-shader"),
        source: ShaderSource::Wgsl(MENU_SHADER.into()),
    });
    let menu_pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
        label: Some("anima-menu-pl"),
        bind_group_layouts: &[],
        immediate_size: 0,
    });
    let menu_pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
        label: Some("anima-menu-pipeline"),
        layout: Some(&menu_pipeline_layout),
        vertex: VertexState {
            module: &menu_shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<MenuVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x2,
                        offset: 0,
                        shader_location: 0,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x4,
                        offset: 8,
                        shader_location: 1,
                    },
                ],
            }],
        },
        primitive: PrimitiveState::default(),
        depth_stencil: None,
        multisample: MultisampleState::default(),
        fragment: Some(FragmentState {
            module: &menu_shader,
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
    let menu_vbo = device.create_buffer(&BufferDescriptor {
        label: Some("anima-menu-verts"),
        size: 256 * 1024,
        usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    Pipelines {
        render: pipeline,
        menu: menu_pipeline,
        menu_vbo,
    }
}
