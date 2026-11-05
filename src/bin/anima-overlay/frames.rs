//! Frame storage: one texture holding the whole animation — a BC7 array, an RGBA array, or a single
//! re-uploaded layer.
//!
//! Moved out of `main` verbatim, from the closure that was already shaped like this function. Its captures
//! were the whole of its interface: the device and queue, the bind group layout, the adapter's limits, and
//! whether BC7 is usable.

use crate::*;

// Eight arguments is one too many, and the next slice is the one that fixes it: the device, queue, layout,
// sampler and uniform belong in a `Gpu` value that this function can take as a whole.
#[allow(clippy::too_many_arguments)]
pub(super) fn build(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    bind_group_layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    uniform: &wgpu::Buffer,
    limits: &wgpu::Limits,
    bc7: bool,
    loaded: Loaded,
) -> Frames {
    let Loaded {
        animation,
        blocks,
        crop: _,
        limits: decode_limits,
    } = loaded;
    let (width, height) = (animation.width, animation.height);
    let frame_count = animation.frames.len();
    let blocks_usable = blocks.as_ref().is_some_and(|blocks| {
        bc7 && blocks.is_consistent() && blocks.width == width && blocks.height == height
    });
    let frame_bytes = u64::from(width) * u64::from(height) * 4;
    let total_bytes = frame_bytes * frame_count as u64;
    let block_bytes = blocks.as_ref().map_or(0, |blocks| blocks.data.len() as u64);
    let rgba_array =
        frame_count as u32 <= limits.max_texture_array_layers && total_bytes <= ARRAY_BUDGET;
    let bc7_array = blocks_usable
        && frame_count as u32 <= limits.max_texture_array_layers
        && block_bytes <= ARRAY_BUDGET;
    let use_array = bc7_array || rgba_array;
    let layers = if use_array { frame_count as u32 } else { 1 };
    eprintln!(
        "[overlay] {} frame(s) {}x{} — {:.1} MiB decoded, {:.1} MiB bc7, {} (decode cap {:.0} MiB)",
        frame_count,
        width,
        height,
        total_bytes as f64 / (1024.0 * 1024.0),
        block_bytes as f64 / (1024.0 * 1024.0),
        if bc7_array {
            "bc7 array texture"
        } else if blocks_usable {
            "bc7 just-in-time upload"
        } else if rgba_array {
            "rgba array texture"
        } else {
            "rgba just-in-time upload"
        },
        decode_limits.max_bytes as f64 / (1024.0 * 1024.0)
    );

    // BC textures are measured in pixels but stored in whole 4x4 blocks, so the
    // texture is block-aligned (e.g. 1125 -> 1128) and the shader samples only the
    // real pixels via `scale`.
    let (tex_format, tex_width, tex_height) = match blocks.as_ref().filter(|_| blocks_usable) {
        Some(blocks) => (
            TextureFormat::Bc7RgbaUnormSrgb,
            blocks.blocks_per_row() * 4,
            blocks.rows() * 4,
        ),
        None => (TextureFormat::Rgba8UnormSrgb, width, height),
    };
    let scale = (
        width as f32 / tex_width.max(1) as f32,
        height as f32 / tex_height.max(1) as f32,
    );

    let texture = device.create_texture(&TextureDescriptor {
        label: Some("anima-frames"),
        size: Extent3d {
            width: tex_width,
            height: tex_height,
            depth_or_array_layers: layers,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: tex_format,
        usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&TextureViewDescriptor {
        dimension: Some(TextureViewDimension::D2Array),
        ..Default::default()
    });
    let bind_group = device.create_bind_group(&BindGroupDescriptor {
        label: Some("anima-bg"),
        layout: bind_group_layout,
        entries: &[
            BindGroupEntry {
                binding: 0,
                resource: BindingResource::TextureView(&view),
            },
            BindGroupEntry {
                binding: 1,
                resource: BindingResource::Sampler(sampler),
            },
            BindGroupEntry {
                binding: 2,
                resource: BindingResource::Buffer(BufferBinding {
                    buffer: uniform,
                    offset: 0,
                    size: None,
                }),
            },
        ],
    });
    let frames = Frames {
        animation,
        blocks,
        blocks_usable,
        texture,
        bind_group,
        use_array,
        tex_width,
        tex_height,
        scale,
    };
    if use_array {
        for index in 0..frame_count {
            upload_frame(queue, &frames, index, index as u32);
        }
    } else {
        upload_frame(queue, &frames, 0, 0);
    }
    frames
}
