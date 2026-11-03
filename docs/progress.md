# AnimaEngine Rust port — progress

Status as of 2026-11-03, HEAD `faa4199`.

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
| `anim` | `nuevoMotor.py` | GIF/APNG/WebP decode → composited RGBA frames (via `image`) |
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

- explicit size/anchor/margin; `KeyboardInteractivity::None`; input enabled
  (click-through toggle is a one-liner using an empty `wl_region`)
- wgpu surface from raw Wayland handles (system backend)
- plays every frame honouring its `delay_ms`; presents only on change
- `FrameCache`: cache hit on load, background cache write on miss (`Arc`-shared)
- GPU frames: 2D array texture when they fit (≤256 MiB, layer limit), otherwise
  just-in-time per-frame upload (large workshop animations)

### Docs / examples

- `docs/workshop-steamworks-re.md` — static RE of the shipped Windows build
  (flat API bridge, subscribe/download flow, three-tier browse fallback, upload).
- `examples/scan_workshop.rs` — scan a real workshop content dir and print what
  the app would see.

## Verification

- `cargo test`: **94** default, **97** with `workshop-web,steamworks`.
- `cargo clippy --all-targets`: clean for default and all features.
- Real data: `scan_workshop` parsed the installed `appworkshop_3474900.acf`
  (1 item) and listed item `3828587753`'s 6 `.apng` files.
- Overlay smoke-tested on niri (no screenshots; visual acceptance manual):
  - generated 24-frame test animation — plays
  - `bochi455.apng` (27 frames, 512²) — array texture; second run cache hit
  - workshop `miku12.apng` (64 frames, 1500×1125, 412 MiB decoded) — 12.4 s
    first decode, then just-in-time upload playback

## TODO

### Overlay interaction / windowing

- [ ] Per-pixel alpha hit-test (`Frame::alpha_mask`) — drag only on opaque pixels
- [ ] Drag to move; persist position to `secondary_instances.json`
- [ ] Right-click control menu (port `anima_overlay_controls`)
- [ ] Click-through toggle (empty input region) + `Ctrl+Shift+R` recovery
- [ ] Multi-instance: one process per animation (or multiple surfaces)
- [ ] Multi-monitor: per-output layer surfaces
- [ ] Global hotkeys via `xdg-desktop-portal` GlobalShortcuts
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
- Overlay is Wayland-only (layer-shell); X11/Windows not targeted yet.
- Visual acceptance of the overlay is manual; no screenshots are produced by the
  smoke tests.

## How to run

```bash
cargo test                                        # pure logic (94 tests)
cargo test --features "workshop-web,steamworks"   # + Steam adapters (97)
cargo clippy --all-targets --features "overlay,workshop-web,steamworks"

cargo run --example scan_workshop -- ~/.local/share/Steam/steamapps/workshop/content/3474900

cargo run --features overlay --bin anima-overlay  # generated test animation
cargo run --features overlay --bin anima-overlay -- /path/to/animation.apng
```

## Reference

- `docs/workshop-steamworks-re.md` — reverse engineering of the shipped build.
- Commit history: pure core → workshop RE + scanner → Steam bridge → overlay
  scaffold → multi-frame playback.
