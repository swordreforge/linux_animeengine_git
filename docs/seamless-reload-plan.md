# Seamless reload for a *geometry change* — implementation plan

## Where we are

Hot reload has two tiers today:

1. **Same geometry** (size, frame count and BC7-vs-RGBA path identical) — verified
   seamless: the new frames are uploaded into the existing texture
   (`reloaded in place: 27 frame(s) 136x132`, same PID).
2. **Different geometry** — falls back to `CommandExt::exec`, which keeps the
   persisted placement but recreates the layer surface, so it flickers.

Tier 2 is what this plan removes. It is a mechanical refactor, not a new feature.

## Why it needs a refactor

`texture`, `texture_view`, `bind_group`, `layers`, `use_array`, `tex_width`,
`tex_height` and `texture_scale` are locals in `main`, and `upload_frame` is a closure
capturing them. A different geometry means new values for all of them, so they must
live in something replaceable.

## `struct Frames`

```rust
struct Frames {
    animation: Arc<Animation>,
    blocks: Option<Blocks>,
    blocks_usable: bool,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    layers: u32,
    use_array: bool,
    tex_width: u32,
    tex_height: u32,
    /// `(width / tex_width, height / tex_height)`: 1:1 for RGBA, < 1 for BC7 padding.
    scale: (f32, f32),
}

impl Frames {
    /// Storage decision, texture creation, the initial upload, the view and the bind
    /// group — everything that depends on the animation's geometry.
    #[allow(clippy::too_many_arguments)]
    fn build(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        loaded: Loaded,
        bc7: bool,
        max_array_layers: u32,
        bind_group_layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        uniform: &wgpu::Buffer,
    ) -> Self;

    /// The old `upload_frame` body, now a method (`index`/`layer` as before).
    fn upload(&self, queue: &wgpu::Queue, index: usize, layer: u32);

    fn len(&self) -> usize { self.animation.frames.len() }
}
```

`Frames::build` absorbs lines ~2260-2392 of `anima-overlay.rs`: the `rgba_array` /
`bc7_array` decision, the `use_array`/`layers` choice, the storage `eprintln!` (keep
the wording, it is quoted in the docs), the block-aligned `(tex_format, tex_width,
tex_height)`, `texture_scale`, `create_texture`, the per-layer upload loop and the
`create_view` (D2Array). The bind group creation that follows it moves in as well so
that `bind_group` and `view` always match.

## Loop changes (mechanical)

| today | after |
| --- | --- |
| `use_array` | `frames.use_array` |
| `upload_frame(&animation, blocks.as_ref(), index, 0)` | `frames.upload(&queue, index, 0)` |
| `texture_scale` | `frames.scale` |
| `frame_count` (wrap, logs) | `frames.len()` |
| `animation.frames[index]` | `frames.animation.frames[index]` |
| `set_bind_group(0, &bind_group, &[])` | `set_bind_group(0, &frames.bind_group, &[])` |
| `state.animation` | keep in sync: `state.animation = Arc::clone(&frames.animation)` |

`index`, `uploaded_layer0`, `next_change` and `last_presented` stay loop locals
(`index = 0`, `uploaded_layer0 = 0`, `last_presented = None` on reload).

## Reload branch

Replace the exec block with:

```rust
if hot_reload.changed() {
    let loaded = load_animation(path.as_deref())?;
    let next = Frames::build(&device, &queue, loaded, bc7, limits.max_texture_array_layers,
                             &bind_group_layout, &sampler, &uniform);

    // Geometry follows the new art; the existing helpers keep it on screen.
    let (width, height) = (next.animation.width, next.animation.height);
    state.animation = Arc::clone(&next.animation);
    state.native_size = (width, height);
    state.crop = next_crop;                     // from the new Loaded
    if !state.hidden {
        let (w, h) = fit_to_output(&state, scaled_size(&state));
        state.desired_size = (w, h);
        state.layer_surface.set_size(w, h);
        state.surface.commit();
        reapply_clamp(&mut state);
    }
    state.frame_index = 0;
    state.region_dirty = true;
    state.visual_dirty = true;
    index = 0;
    uploaded_layer0 = 0;
    last_presented = None;
    frames = next;
    eprintln!("[overlay] rebuilt frames: {} frame(s) {}x{}", ..., width, height);
    persist_geometry(&state);
}
```

The surface resize arrives through the existing `state.configured` path (line ~2728):
the compositor answers with a new size, `config` is updated and `reapply_clamp` runs.

## The BC7 background encoder

It is spawned once, before the loop, and captures the current animation. Factor it into

```rust
fn spawn_bc7_encode(path: Option<&Path>, animation: &Arc<Animation>, bc7: bool);
```

and call it both at startup and after a reload whose `blocks` is `None` (otherwise a
re-published item without a sidecar would never get one until the next launch).

## Acceptance test (the case that flickers today)

```bash
cp ~/projects/AnimaEngine/bochi455.apng /tmp/hot.apng            # 512² x 27
./target/release/anima-overlay /tmp/hot.apng &
# swap in a different geometry while it runs:
cp <workshop>/miku12.apng /tmp/hot.apng                          # 1500×1125 x 64
```

Expect: `rebuilt frames: 64 frame(s) 1500x1125` (or the cropped size), the **same
PID**, no `restored instance` line, and the surface resized. Then swap back to a
smaller animation to prove it works in both directions.

## Risks to watch

- Recreating the bind group on every rebuild is required — a stale one samples the old
  texture (invisible or garbage).
- `state.surface_size` is refreshed at the top of each iteration from `config`, so the
  hit-test/region code needs no extra change once `config` updates.
- `fit_to_output` + `reapply_clamp` must run *after* `state.native_size` changes,
  otherwise the overlay can hang off-screen after growing.
- The region throttle uses `applied_rects`; clearing it (as the reload does) forces a
  rebuild from the new animation.
