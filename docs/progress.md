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
| `state` | `monoModule.py` | `secondary_instances.json` model (load/save/upsert/remove) |
| `prefs` | `anima_overlay_prefs.py` | overlay menu style / warnings / hotkey, cross-process visibility flag |
| `settings` | scattered `*.json` | `settings.json`, `anim_meta`, `window_position`, `user`, `audioGifRegistro`, `last_selected`, `config` flags |
| `i18n` | `anima_i18n_static.py` | 31-locale catalog, language normalization/detection, `tr()` |
| `library` | `customtkinter_MInteraDesktop.py` | gifs/sprites/sprite-config JSON + sprite-sheet slicing |
| `pack` | `mdaa_io.py` / `models.py` | `.mdaa` (zip + `pack.json` + NPZ frames), minimal NPY parser |
| `input_map` | `deteccionDeTeclas.py` / `funciones.py` | `configTeclas.json` bindings, activation modes, function catalog |
| `cache` | `monoModule.py` | `FrameCache`: sha256 key, ZIP+PNG frames, LRU eviction |
| `anim` | `nuevoMotor.py` | GIF/APNG/WebP decode → composited RGBA frames (via `image`), plus `alpha_at` / `alpha_hit` |
| `control` | — (new) | cross-process overlay control file (`overlay_control.json`): `InputMode` (full / opaque / pass-through) + reset / quit + generation |
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
- **tray** (`ksni`): `StatusNotifierItem` menu with an "Input" submenu
  (full / opaque / click-through), "Reset position and interaction", "Quit overlay"
- **global shortcuts** (`ashpd`, `xdg-desktop-portal` GlobalShortcuts):
  `Ctrl+Shift+T` toggle click-through, `Ctrl+Shift+R` reset
- tray/portal do not touch the overlay directly: they write
  `control::ControlState` (`<XDG data>/AnimaEngine/overlay_control.json`) and the
  render loop applies the newest `generation`

### Docs / examples

- `docs/workshop-steamworks-re.md` — static RE of the shipped Windows build
  (flat API bridge, subscribe/download flow, three-tier browse fallback, upload).
- `examples/scan_workshop.rs` — scan a real workshop content dir and print what
  the app would see.

## Verification

- `cargo test`: **102** default, **105** with `workshop-web,steamworks`.
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
- [ ] Throttle or coarsen the per-pixel region for very detailed silhouettes
      (>4096 rects currently collapse to a bounding box)
- [ ] Recover hotkeys on niri: either install a portal backend implementing
      `GlobalShortcuts`, or ship a tiny `anima-overlay-ctl` CLI and bind it in
      `niri`'s config (tray already works today)
- [ ] Right-click control menu on the surface (port `anima_overlay_controls`);
      largely covered by the tray
- [ ] Multi-instance: one process per animation (or multiple surfaces)
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
- Per-pixel input sends one region rectangle per opaque span whenever the
  silhouette changes (thousands for detailed art); it is not throttled yet.
- The control file is read-modify-written by tray and portal without a lock, so a
  rare concurrent toggle could lose an update.
- Overlay is Wayland-only (layer-shell); X11/Windows not targeted yet.
- Visual acceptance of the overlay is manual; no screenshots are produced by the
  smoke tests.

## How to run

```bash
cargo test                                        # pure logic (102 tests)
cargo test --features "workshop-web,steamworks"   # + Steam adapters (105)

cargo clippy --all-targets --features "overlay,workshop-web,steamworks"

cargo run --example scan_workshop -- ~/.local/share/Steam/steamapps/workshop/content/3474900

cargo run --features overlay --bin anima-overlay  # generated test animation
cargo run --features overlay --bin anima-overlay -- /path/to/animation.apng
```

## Reference

- `docs/workshop-steamworks-re.md` — reverse engineering of the shipped build.
- Commit history: pure core → workshop RE + scanner → Steam bridge → overlay
  scaffold → multi-frame playback → alpha drag → SNI tray + portal control →
  per-pixel input region.
