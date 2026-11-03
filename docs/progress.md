# AnimaEngine Rust port — progress

Status as of 2026-11-03.

A Rust rewrite of the AnimaEngine app, split into two goals:

1. **Pure logic core** — port the platform-independent logic from the Python
   build, with tests, no windowing/GPU dependency.
2. **Native Wayland overlay** — a layer-shell + wgpu always-on-top animation
   surface, replacing the Windows `monoModule` / `nuevoMotor` renderers.

## Completed

### Pure logic core (default features)

| Module | Python source | What it does |
|---|---|---|
| `paths` | `anima_foundation.py` | XDG user dirs, atomic JSON write, canonical/dedupe paths |
| `state` | `monoModule.py` | `secondary_instances.json` model (load/save/upsert/remove) + geometry persistence keyed by animation path |
| `prefs` | `anima_overlay_prefs.py` | overlay menu style / warnings / hotkey, cross-process visibility flag |
| `settings` | scattered `*.json` | `settings.json`, `anim_meta`, `window_position`, `user`, `audioGifRegistro`, `last_selected`, `config` flags |
| `i18n` | `anima_i18n_static.py` | 31-locale catalog, language normalization/detection, `tr()` |
| `library` | `customtkinter_MInteraDesktop.py` | gifs/sprites/sprite-config JSON + sprite-sheet slicing |
| `pack` | `mdaa_io.py` / `models.py` | `.mdaa` (zip + `pack.json` + NPZ frames), minimal NPY parser |
| `input_map` | `deteccionDeTeclas.py` / `funciones.py` | `configTeclas.json` bindings, activation modes, function catalog |
| `cache` | `monoModule.py` | `FrameCache`: sha256 key, ZIP+PNG frames, LRU eviction |
| `anim` | `nuevoMotor.py` | GIF/APNG/WebP decode → composited RGBA frames (via `image`), plus `alpha_at` / `alpha_hit` / `alpha_rects` and `Orientation` (quarter turns + mirrors) |
| `control` | — (new) | cross-process overlay control file (`overlay_control.json`): `InputMode` (full / opaque / pass-through), `scale_percent` (10–100), reset / quit + generation |
| `error` | — | shared error type |

### Workshop

| Module | What it does |
|---|---|
| `vdf` | minimal Valve KeyValues parser |
| `workshop` | `libraryfolders.vdf` / `appworkshop_*.acf` parsing, content-root discovery, item scanning, `sync`, `diff`, `LocalWorkshopSource` |
| `workshop_web` *(feature `workshop-web`)* | Steam Web API metadata: keyless `GetPublishedFileDetails` + `QueryFiles` (needs key), blocking client |
| `steam` *(feature `steamworks`)* | `UgcTransport` + `SteamWorkshop` subscribe/download/poll flow; `FlatSteamApi` loads `libsteam_api` at runtime via `libloading` |

### Overlay binary (`--features overlay`)

`src/bin/anima-overlay.rs` — `smithay-client-toolkit` layer-shell surface on the
**Overlay** layer + transparent wgpu surface.

- explicit size/anchor/margin; `KeyboardInteractivity::None`
- wgpu surface from raw Wayland handles (system backend)
- plays every frame honouring its `delay_ms`; presents only on change
- `FrameCache`: cache hit on load, background cache write on miss (`Arc`-shared)
- GPU frames: 2D array texture when they fit (≤256 MiB, layer limit), otherwise
  just-in-time per-frame upload (large workshop animations)

Interaction and control:

- **per-pixel alpha hit-test + drag**: `wl_seat`/`wl_pointer` bound; a left press
  only starts a drag on an opaque pixel (`Animation::alpha_hit`, threshold 25,
  surface-local → frame pixel mapping), and motion updates the layer-shell
  margins (anchors are Top|Left, so margins grow with pointer movement)
- **input modes** (`wl_region` input region):
  - `Opaque` (default) — a region built from the visible frame's alpha, so only
    solid pixels grab clicks and transparent ones pass through to what is below
  - `PassThrough` — an empty region: the whole surface is click-through (the
    original's `WS_EX_TRANSPARENT`)
  - `Full` — `set_input_region(None)`: the whole surface is interactive
  - the region is rebuilt when the visible frame's silhouette changes; identical
    silhouettes skip the update, and >4096 rects fall back to the bounding box
- **size** (`scale_percent`, capped at 100%): the surface is resized to
  `native * percent / 100`, so it only ever shrinks and the GPU samples the
  texture down (linear minification) — no resampling
- **geometry persistence**: position + display size are written back to
  `secondary_instances.json` (keyed by the canonical animation path; file is
  `<XDG data>/AnimaEngine/secondary_instances.json`, overridable with
  `ANIMAENGINE_STATE_FILE`) on drag release, size change, reset and exit, and
  restored on startup
- **instance fields**: `fps` (0 = each frame's `delay_ms`, >0 = `1000/fps` ms),
  `locked` (drag disabled), `always_on_top` (Overlay vs Bottom layer at startup),
  `hover_hide` (render nothing while the pointer is on a solid pixel),
  `rotation`/`flip_h`/`flip_v` (shader UV transform, shared with the hit-test and
  the per-pixel input region so all three agree); `click_through` and `locked`
  are written back, the rest are read at startup
- **show/hide**: hiding resizes the layer surface to **1x1** instead of unmapping
  it, so the surface — and therefore the render loop, tray and control channel —
  stays alive (the pattern Wayland needs; an unmapped surface cannot come back);
  showing restores the scaled size. Tray "Visible", `anima-overlay-ctl
  hide|show|toggle-hidden`, `Mod+Alt+H`
- **multi-instance**: one process per animation. Each gets its own control file
  (`<XDG data>/AnimaEngine/overlay_control/<slug>.json`, slug = SHA-256 of the
  canonical path) and its own tray item (`anima-engine-overlay-<slug>`, title =
  file name). `anima-overlay-ctl` broadcasts to every instance by default and
  takes `--instance <animation path>` to target one; the portal shortcut handler
  also broadcasts (shortcut ids are per-instance). `secondary_instances.json`
  already holds one entry per path.
- **in-screen clamping**: margins are clamped to `[0, output - surface]` so the
  whole surface stays inside the output — at startup, on every drag motion, on
  scale change, when showing from hidden, and after a configure. The output size
  comes from SCTK's `OutputState` (the output the surface last entered; the last
  known size is kept when the surface leaves it mid-drag).
- **input-region updates**: generated in **surface space** (cost ∝ displayed area,
  and it shares the display→native mapping with the hit-test), recomputed at most
  every 3rd presented frame (a frame-count throttle, not a wall-clock timer), only
  once for a static image, and force-refreshed on size changes. Above
  `MAX_INPUT_RECTS` the sampling grid is coarsened (2x2, 4x4, ...) and only then
  falls back to the bounding box.
- **tray** (`ksni`): `StatusNotifierItem` menu with "Input" and "Size" submenus
  (100/75/50/25%), "Reset position and interaction", "Quit overlay"
- **CLI** (`anima-overlay-ctl`, no extra features): `toggle`,
  `mode <full|opaque|pass-through>`, `scale <percent|up|down>`, `reset`, `quit`,
  `status` — for compositor keybindings (niri has no `GlobalShortcuts` portal
  backend); see `docs/overlay-control.md`
- **global shortcuts** (`ashpd`, `xdg-desktop-portal` GlobalShortcuts):
  `Ctrl+Shift+T` toggle click-through, `Ctrl+Shift+R` reset
- tray/portal do not touch the overlay directly: they write
  `control::ControlState` (`<XDG data>/AnimaEngine/overlay_control.json`) and the
  render loop applies the newest `generation`

### Docs / examples

- `docs/workshop-steamworks-re.md` — static RE of the shipped Windows build
  (flat API bridge, subscribe/download flow, three-tier browse fallback, upload).
- `docs/overlay-control.md` — input modes, the `anima-overlay-ctl` CLI, niri
  keybindings, tray and portal shortcuts.
- `examples/scan_workshop.rs` — scan a real workshop content dir and print what
  the app would see.

## Verification

- `cargo test`: **108** default (106 lib + 2 CLI), **111** with
  `workshop-web,steamworks`.
- `cargo clippy --all-targets`: clean for default and all features.
- Real data: `scan_workshop` parsed the installed `appworkshop_3474900.acf`
  (1 item) and listed item `3828587753`'s 6 `.apng` files.
- Overlay smoke-tested on niri (no screenshots; visual acceptance manual):
  - generated 24-frame test animation — plays
  - `bochi455.apng` (27 frames, 512²) — array texture; second run cache hit
  - workshop `miku12.apng` (64 frames, 1500×1125, 412 MiB decoded) — 12.4 s
    first decode, then just-in-time upload playback
- Control channel driven from the shell (same writes the tray/portal perform):
  mode changes → `[overlay] input: …`, reset → `[overlay] reset to initial
  position`, quit → process exits 0. Verified repeatable.
- Tray registers on this machine (`tray registered (StatusNotifierItem)` —
  a StatusNotifierWatcher is running).
- Per-pixel input verified live: switching modes through the control file logs
  `input: opaque pixels (N rects)` / `input: click-through` / `input: full
  surface`; the generated 256² test animation yields 64–83 rects and workshop
  `miku12.apng` (1500×1125) yields 3289 rects (under the 4096 cap).
- A real drag against the running overlay moved the margins as expected
  (`drag start` → `drag end; margins …`), confirming alpha hit-testing in practice.
- `anima-overlay-ctl` driven against a running overlay: `status` reports the
  mode, `toggle` → `input: click-through`, `mode full` → `input: full surface`,
  `mode opaque` → `input: opaque pixels (N rects)`, `reset` → `reset to initial
  position`, `quit` → exits 0; unknown subcommands exit 1 with usage.
- Scaling verified on the 256² test animation: `scale 50` → `scale 50% ->
  128x128`, `scale 200` clamps to `256x256`, `scale down` → `230x230`, `reset`
  → `256x256`; an invalid `scale` argument exits 1.
- Geometry persistence round-trip (isolated `ANIMAENGINE_STATE_FILE`): a fresh
  run wrote an instance keyed by the image's canonical path (`x`/`y` 100,
  `scale_percent` 50); a second run logged `restored geometry: (100, 100) at
  50%`; after editing `x`/`y` to 300/250 a third run logged
  `restored geometry: (300, 250) at 50%`.
- Instance fields restored and written back: a seeded record with
  `click_through`, `locked`, `hover_hide`, `fps` and `always_on_top` logged
  `mode PassThrough, locked true, hover_hide true, fps 10`; `ctl unlock` then
  logged `locked false` and `ctl mode opaque` rewrote `click_through: false`,
  leaving the untouched fields intact.
- Rotation/mirrors: seeded `rotation` 90/180/270 (plus flips) restored as
  quarters 1/2/3, each built its per-pixel region (990 rects) and quit cleanly;
  `Orientation` unit tests cover the inverse mapping, the axis swap and rect
  remapping.
- Hide/show keep-alive: `hide` logged `hidden (1x1 keep-alive)`, and *while
  hidden* the overlay still applied `scale 75% (hidden)`, `input: full surface`
  and `show` → `shown`, then quit cleanly — the 1x1 surface keeps the event loop
  and control channel alive, matching the live2d-rs approach.
- Multi-instance: two overlays (two images) created two per-instance control
  files and `status` listed both; a broadcast `toggle` logged
  `input: click-through` in **both** logs, while `--instance <p1> mode full` only
  affected the first and `--instance <p2> scale 50` only the second; a broadcast
  `quit` exited both with rc 0, and both trays registered.
- In-screen clamping: seeding an off-screen position (5000, 5000) for a 593×412
  image logged `clamped position to (1327, 668)` — i.e. it detected the
  1920×1080 output — and wrote the corrected geometry back to
  `secondary_instances.json`; an on-screen (100, 100) seed was left untouched.
  `clamp_position` unit tests cover the bounds, negatives and a surface larger
  than the output.
- Region tuning: a 593×412 silhouette produced **990 rects at both 100% and 50%**
  — the rectangle count follows edge complexity, not display scale — while the
  surface-space scan costs 4× less at 50% (∝ area). Updates are throttled to every
  3rd presented frame, and a static image is scanned once.
- Portal on this machine: `A portal frontend implementing
  org.freedesktop.portal.GlobalShortcuts was not found` (expected — the installed
  backend does not implement it); the code path is in place where a backend does.

## TODO

### Overlay interaction / windowing

- [x] Per-pixel alpha hit-test (`Animation::alpha_hit`) — drag only on opaque pixels
- [x] Drag to move
- [ ] Persist position to `secondary_instances.json`
- [x] Click-through (empty input region) and per-pixel pass-through
      (`Opaque` default: region from the frame's alpha; transparent areas pass
      clicks through — note the surface then receives no pointer events there)
- [x] Tray (`ksni`, Input submenu) + portal GlobalShortcuts for mode / reset / quit
- [x] Throttle the per-pixel region by frame count and coarsen the sampling grid
      above `MAX_INPUT_RECTS` (bounding box only as a last resort)
- [x] Recover hotkeys on niri: `anima-overlay-ctl` CLI + compositor keybinds
      (documented in `docs/overlay-control.md`); the portal path stays for
      backends that implement `GlobalShortcuts`
- [x] Size control (`scale_percent`): shrink-only, capped at the native size;
      CLI `scale` + tray "Size" submenu
- [x] Persist position + `scale_percent` per instance in
      `secondary_instances.json` (keyed by the canonical animation path)
- [x] Wire the remaining `Instance` fields: `click_through` (restored + written
      back), `locked`, `fps`, `always_on_top`, `hover_hide`,
      `rotation`/`flip_h`/`flip_v`
- [x] Show/hide via the 1x1 keep-alive (surface stays mapped, so the tray and CLI
      keep working while hidden)
- [ ] `hover_hide` fidelity — deferred: it needs a global pointer *while* hidden,
      and none is available on this niri setup (see "Known gaps"); today the input
      region stays installed, so clicks are still captured while it is hidden
- [ ] Right-click control menu on the surface (port `anima_overlay_controls`);
      largely covered by the tray
- [x] Multi-instance: one process per animation, per-instance control file and
      tray item, broadcast CLI/portal with `--instance` targeting
- [ ] Multi-monitor: per-output layer surfaces
- [ ] GNOME/Mutter fallback (no layer-shell → compositor rules / normal window)
- [ ] HiDPI buffer scale + pin surface size across compositor reconfigures
      (the live2d-rs configure-bounce pattern)
- [ ] Frame timing edge cases: GIF centisecond delays, APNG fractions,
      per-path `fps` overrides from `anim_meta.json`

### Steam / Workshop

- [ ] Provide a Linux `libsteam_api.so` and wire `SteamWorkshop` into the app
  (this install only ships Windows `steam_api64.dll`)
- [ ] Subscribe / download / repair / unsubscribe actions in the UI
- [ ] `ItemInstalled` / `DownloadItemResult` callbacks → `sync` + `diff` hot reload
- [ ] Upload via SteamCMD (`workshop_build_item`) and/or the Steamworks worker,
  incl. legal-agreement handling
- [ ] Browse UI using the three-tier fallback documented in the RE notes
- [ ] Linux Steam library discovery from `~/.steam` roots

### Robustness / distribution

- [ ] Large animations currently decode fully into RAM (tetonahall: ~2.4 GiB) —
  needs streaming / bounded decode
- [ ] Cache encodes PNGs (CPU heavy for large frames); consider zstd raw frames
- [ ] Skip caching or clean stale `.tmp` files for interrupted background writes
- [ ] Ship the i18n catalog as a data file; verify all locales
- [ ] Linux packaging (AppImage/Flatpak); include `libsteam_api.so` if licensing allows

### Main UI (optional)

- [ ] Control window (library / workshop / settings). Framework undecided
  (Bevy / egui / other); the overlay is intentionally independent.

## Known gaps / risks

- No live Steamworks validation (missing `libsteam_api.so` on this machine).
- Portal GlobalShortcuts depends on a portal backend that implements it (this
  machine's does not); the tray is the working control path here.
- Per-pixel input still sends one region rectangle per edge span (thousands for
  detailed art), and the count does not shrink with display scale; updates are
  throttled but each update still carries the full list.
- `secondary_instances.json` is read-modify-written per update without a lock, so
  two overlays saving at once can lose an update (as in the Python original).
- The instance key is the canonical animation path, so moving/renaming a file
  starts a fresh instance entry.
- `rotation` is honoured as quarter turns (rounded to the nearest 90°); other
  angles are not supported. `fps` defaults to 0 (native frame delays) rather than
  the Python's fixed 15 fps timer.
- Hiding uses a 1x1 surface. A few compositors dislike tiny surfaces (live2d-rs
  falls back to 50x50 or `set_minimized` on GNOME/KDE); niri is fine with 1x1.
- Global pointer (for a faithful `hover_hide`) is not available on this niri
  setup. Checked:
  - XWayland `XQueryPointer` returns a **frozen** last-known position — niri's
    xwayland-satellite only forwards the pointer to X while it is over an X
    surface, so `DISPLAY=:0` is not a live cursor source (verified: 20 s poll,
    one position, stale button/modifier mask).
  - evdev (`/dev/input/event*`) is `root:input 660` and the user is not in the
    `input` group.
  - only `gtk.portal` is installed, which does not implement
    `org.freedesktop.portal.InputCapture` (and that portal is for input *capture*,
    not cursor tracking).
  - a full-screen bottom (Background) surface, as a wallpaper uses, only sees the
    pointer where no other surface is above — it cannot see it over the overlay.
  So `hover_hide` stays best-effort: it draws nothing while the cursor is on a
  solid pixel and keeps the silhouette input region so `Leave` restores it;
  clicks on the character are still captured while it is hidden.
- The control file is read-modify-written by tray and portal without a lock, so a
  rare concurrent toggle could lose an update.
- Overlay is Wayland-only (layer-shell); X11/Windows not targeted yet.
- Visual acceptance of the overlay is manual; no screenshots are produced by the
  smoke tests.

## How to run

```bash
cargo test                                        # pure logic (108 tests)
cargo test --features "workshop-web,steamworks"   # + Steam adapters (111)

cargo clippy --all-targets --features "overlay,workshop-web,steamworks"

cargo run --example scan_workshop -- ~/.local/share/Steam/steamapps/workshop/content/3474900

cargo run --features overlay --bin anima-overlay  # generated test animation
cargo run --features overlay --bin anima-overlay -- /path/to/animation.apng
# position/scale persist to <XDG data>/AnimaEngine/secondary_instances.json;
# point ANIMAENGINE_STATE_FILE elsewhere to isolate or inspect it

cargo build --release --bin anima-overlay-ctl     # CLI for compositor keybinds
anima-overlay-ctl scale 50                         # shrink the overlay to 50%

# multi-instance: one process per animation
cargo run --features overlay --bin anima-overlay -- /path/a.apng &
cargo run --features overlay --bin anima-overlay -- /path/b.apng &
anima-overlay-ctl status                           # one block per instance
anima-overlay-ctl --instance /path/a.apng mode full  # target a single instance
```

## Reference

- `docs/workshop-steamworks-re.md` — reverse engineering of the shipped build.
- Commit history: pure core → workshop RE + scanner → Steam bridge → overlay
  scaffold → multi-frame playback → alpha drag → SNI tray + portal control →
  per-pixel input region → CLI control (`anima-overlay-ctl`) → size control →
  geometry persistence → remaining instance fields (lock/fps/layer/hover/orientation)
  → 1x1 keep-alive hide/show → multi-instance → in-screen clamping →
  frame-count region throttling/coarsening.
