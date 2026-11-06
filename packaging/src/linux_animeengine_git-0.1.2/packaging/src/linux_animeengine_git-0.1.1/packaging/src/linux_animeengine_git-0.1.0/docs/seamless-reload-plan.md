# Seamless reload for a *geometry change* — implementation plan

**Status: implemented** (commit "Rebuild the frame texture in place on a geometry
change"). What the code does differs from the plan in three small ways, all noted
below: `sampler`/`uniform`/`bind_group_layout` were moved above the frame block as
written; the builder became a closure returning a `Frames` value instead of an
associated function with a `FrameBuild` context; and `view`/`layers` are built
locally and *not* stored in `Frames` (only `bind_group`, `use_array`,
`tex_width`/`tex_height` and `scale` survive past the builder). `DecodeLimits`
already derived `Copy`, so no change was needed there. The `upload_frame` body moved
to a free function taking `&Frames`, which is what lets the builder upload through
the struct it is constructing. Verified end to end: same PID across a touch, a swap
to a 1500×1125×64 animation and a swap back.

Goal: remove the `CommandExt::exec` fallback so that swapping in an animation with a
different size/frame count also happens in place (same PID, no flicker).

Current state (verified): same-geometry reload swaps the pixels in the existing
texture (`reloaded in place: 27 frame(s) 136x132`, PID unchanged). A geometry change
re-execs. Baseline: 139 lib + 3 CLI tests, `cargo clippy --all-targets --features
"overlay,workshop-web,steamworks,bc7"` clean.

Measurements below are for `src/bin/anima-overlay.rs` at 2917 lines.

---

## Step 0 — safety net (do first)

```bash
cd ~/projects/anima-engine-rs
cp src/bin/anima-overlay.rs /tmp/anima-overlay.before-frames.rs
cargo test --quiet                                   # 139 lib pass
```

Rollback at any point: `git checkout -- src/bin/anima-overlay.rs` (nothing else is
touched until Step 5 adds a helper).

---

## Step 1 — move three GPU objects above the frame block

They are created at ≈2450–2520, *after* the frame storage, but a rebuild needs them:

- `let bind_group_layout = device.create_bind_group_layout(...)`
- `let sampler = device.create_sampler(...)`
- `let uniform = device.create_buffer(...)`

Cut all three and paste them right after `wgpu_surface.configure(&device, &config);`
(≈2270). They do not depend on the animation. Leave the pipelines and the standalone
`bind_group` creation where they are for now (Step 3 deletes the latter).

Checkpoint: `cargo check --features overlay` must still pass.

---

## Step 2 — the two structs and the build

Insert above `fn main()`:

```rust
/// Everything that depends on the animation's geometry and must be replaced when the
/// animation changes size, frame count or storage path (BC7 vs RGBA).
struct FrameState {
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
    /// `(width / tex_width, height / tex_height)`: 1:1 for RGBA, <1 for BC7 padding.
    scale: (f32, f32),
}

/// Immutable objects `FrameState::build` needs; all outlive any rebuild.
struct FrameBuild<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    layout: &'a wgpu::BindGroupLayout,
    sampler: &'a wgpu::Sampler,
    uniform: &'a wgpu::Buffer,
    bc7: bool,
    max_layers: u32,
}

impl FrameState {
    /// Storage decision + texture + initial upload + view + bind group.
    /// Body: the current lines ≈2272–2392, with `animation`/`blocks` coming from
    /// `loaded` and `bind_group` built from `ctx.layout`/`ctx.sampler`/`ctx.uniform`.
    fn build(ctx: &FrameBuild, loaded: Loaded) -> Self { /* see below */ }

    /// The current `upload_frame` closure body, verbatim (`self.texture`, `self.*`).
    fn upload(&self, queue: &wgpu::Queue, index: usize, layer: u32) { /* ... */ }

    fn len(&self) -> usize {
        self.animation.frames.len()
    }
}
```

`build` computes, from `loaded.animation` / `loaded.blocks` / `ctx.bc7` /
`ctx.max_layers` (instead of the current locals):

```rust
let (width, height) = (loaded.animation.width, loaded.animation.height);
let frame_count = loaded.animation.frames.len();
let blocks_usable = loaded.blocks.as_ref().is_some_and(|b| {
    ctx.bc7 && b.is_consistent() && b.width == width && b.height == height
});
// … the existing rgba_array / bc7_array / use_array / layers / total_bytes math …
// … the existing storage eprintln! (keep the wording: the docs quote it) …
let (tex_format, tex_width, tex_height) = /* existing match */;
let scale = (width as f32 / tex_width.max(1) as f32,
             height as f32 / tex_height.max(1) as f32);
let texture = ctx.device.create_texture(/* existing descriptor */);
// upload all layers (array) or layer 0 (JIT) through a local helper with the same
// body as `upload`, or construct `Self` first and call `state.upload(...)`.
let view = texture.create_view(&TextureViewDescriptor {
    dimension: Some(TextureViewDimension::D2Array),
    ..Default::default()
});
let bind_group = ctx.device.create_bind_group(&BindGroupDescriptor {
    label: Some("anima-bind"),
    layout: ctx.layout,
    entries: &[/* the existing entries, with &view */],
});
FrameState { animation: loaded.animation, blocks: loaded.blocks, blocks_usable,
             texture, view, bind_group, layers, use_array, tex_width, tex_height, scale }
```

**Tip that avoids a borrow problem**: build the struct with an empty-texture
placeholder is *not* possible; instead do the uploads with a local closure identical
to `upload` (three lines duplicated), or make `upload` a free function taking
`(&Texture, &Animation, Option<&Blocks>, blocks_usable, tex_width, tex_height, width,
height, queue, index, layer)`. The free function is the smaller diff.

---

## Step 3 — replace the inline block in `main`

After Step 1, `main` has `bind_group_layout`, `sampler`, `uniform` before the frame
block. Then:

```rust
let frame_ctx = FrameBuild {
    device: &device,
    queue: &queue,
    layout: &bind_group_layout,
    sampler: &sampler,
    uniform: &uniform,
    bc7,
    max_layers: limits.max_texture_array_layers,
};
let mut frames = FrameState::build(
    &frame_ctx,
    Loaded { animation, blocks, crop: loaded_crop, limits: decode_limits },
);
```

Delete the old inline texture/upload/view block **and** the standalone `bind_group`
creation (its only consumer is the render pass's `set_bind_group`).

`crop` is still needed pre-loop for persistence, so keep the initial `Loaded` in a
variable (`let mut loaded = load_animation(...)?;` at the top) and take `crop` from it;
`FrameState::build` consumes `loaded` only on the first iteration.

---

## Step 4 — loop substitutions (exact)

| current text | replacement |
| --- | --- |
| `let layer = if use_array {` | `let layer = if frames.use_array {` |
| `upload_frame(&animation, blocks.as_ref(), index, 0);` | `frames.upload(&queue, index, 0);` |
| `if texture_scale == (1.0, 1.0) {` | `if frames.scale == (1.0, 1.0) {` |
| the three `texture_scale.0` / `.1` uses below it | `frames.scale.0` / `.1` |
| `index = (index + 1) % frame_count;` | `index = (index + 1) % frames.len();` |
| `frame_delay(&animation.frames[index], fps)` | `frame_delay(&frames.animation.frames[index], fps)` |
| `pass.set_bind_group(0, &bind_group, &[])` | `pass.set_bind_group(0, &frames.bind_group, &[])` |

`frame_count`, `layers`, `use_array`, `texture_scale` stay only inside `FrameState`.
`width`/`height` remain outer (used by the pre-loop state setup and by
`state.native_size`).

Checkpoint: `cargo check --features overlay` and run the same-geometry reload test
(touch a copy) — it must still log `reloaded in place`.

---

## Step 5 — the reload branch

Replace the current in-place/exec block (≈2618–2670) with:

```rust
if hot_reload.changed() {
    let loaded = load_animation(path.as_deref())?;
    let (new_w, new_h) = (loaded.animation.width, loaded.animation.height);
    let new_crop = loaded.crop;
    let next = FrameState::build(&frame_ctx, loaded);

    state.animation = Arc::clone(&next.animation);
    state.native_size = (new_w, new_h);
    state.crop = new_crop;
    state.frame_index = 0;
    state.applied_rects.clear();
    state.region_dirty = true;
    state.visual_dirty = true;
    if !state.hidden {
        // New art, new surface size; the Configure answer lands in `state.configured`.
        let (w, h) = fit_to_output(&state, scaled_size(&state));
        state.desired_size = (w, h);
        state.layer_surface.set_size(w, h);
        state.surface.commit();
        reapply_clamp(&mut state);
    }
    #[cfg(feature = "bc7")]
    if next.blocks.is_none() {
        spawn_bc7_encode(path.as_deref(), &next.animation, bc7);
    }
    frames = next;
    index = 0;
    uploaded_layer0 = 0;
    last_presented = None;
    eprintln!(
        "[overlay] rebuilt frames: {} frame(s) {}x{}",
        frames.len(), new_w, new_h
    );
    persist_geometry(&state);
}
```

`CommandExt` and the `exec` call disappear; the `use std::os::unix::process::CommandExt;`
import goes with them.

---

## Step 6 — factor the BC7 background encoder

The inline block (≈2394–2450) captures the animation by `Arc` and is spawned once. A
rebuild needs it again when the new item has no sidecar:

```rust
#[cfg(feature = "bc7")]
fn spawn_bc7_encode(path: Option<&Path>, animation: &Arc<Animation>, bc7: bool) {
    if !bc7 { return; }
    let Some(source) = path else { return; };
    // … the existing thread body, using `animation` and `source` …
}
```

Call it at startup (after the first `FrameState::build`) and from Step 5.

---

## Step 7 — acceptance

1. `cargo test --quiet` → 139 lib + 3 CLI; `cargo clippy --all-targets --features
   "overlay,workshop-web,steamworks,bc7"` → clean; both `overlay` and `overlay,bc7`
   builds warning-free.
2. **Same geometry** (regression): run a copy of `bochi455.apng`, `touch` it →
   `reloaded in place: 27 frame(s) 136x132`, PID unchanged.
3. **Geometry change** (the new case): while it runs, `cp <workshop>/miku12.apng
   /tmp/hot.apng` (512²×27 → 1500×1125×64) → expect
   `rebuilt frames: 64 frame(s) 1440x1080` (cropped/fitted), **same PID**, no
   `restored instance` line, surface resized. Then swap back to the 512² file and
   expect the same in the other direction.
4. `anima-overlay-ctl --instance /tmp/hot.apng scale 50` / `rotate 90` / `quit` still
   work after a rebuild.

## Step 8 — commit

```bash
cargo fmt
git add -A && git commit -F - <<'EOF'
Rebuild the frame texture in place on a geometry change

… (state the verification numbers from Step 7) …
EOF
```

---

## Checklist of the traps

- **New bind group per rebuild** — a stale one samples the old texture (garbage or
  nothing).
- **`fit_to_output` + `reapply_clamp` after `native_size` changes**, otherwise a
  larger animation can hang off-screen.
- **`last_presented = None` and `uploaded_layer0 = 0`** after a rebuild, or a static
  frame may never be presented and a JIT layer may be skipped.
- **Encoder re-spawn** when the new `blocks` is `None`, or the item never gets a BC7
  sidecar until the next launch.
- `state.surface_size` is refreshed from `config` at the top of each iteration, so the
  hit-test and region code need nothing extra.
