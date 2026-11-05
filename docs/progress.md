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
| `control` | — (new) | cross-process overlay control file (`overlay_control.json`): `InputMode` (full / opaque / pass-through), `scale_percent` (10–100), `rotation` / `flip_h` / `flip_v`, reset / quit + generation |
| `error` | — | shared error type |

### Workshop

| Module | What it does |
|---|---|
| `vdf` | minimal Valve KeyValues parser |
| `workshop` | `libraryfolders.vdf` / `appworkshop_*.acf` parsing, content-root discovery, item scanning, `sync`, `diff`, `LocalWorkshopSource`; Linux Steam root/library discovery (`steam_roots`, `libraries_from_roots`, `discover_libraries`) |
| `workshop_web` *(feature `workshop-web`)* | Steam Web API metadata: keyless `GetPublishedFileDetails` + `QueryFiles` (needs key), blocking client |
| `steam` *(feature `steamworks`)* | `UgcTransport` + `SteamWorkshop` subscribe/download/poll flow; `FlatSteamApi` loads `libsteam_api` at runtime via `libloading` |
| `steamcmd` | SteamCMD wrapper for `workshop_download_item` (no SDK); `STEAMCMD`, `STEAMCMD_DIR`; implements `WorkshopSource` (subscriptions from the client acf, missing items downloaded) |

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
  known size is kept when the surface leaves it mid-drag). The surface **size** is
  pinned too: a configure with a different size (niri shrinks layer surfaces
  pushed past an edge) is bounced back to the pinned size, and content that cannot
  fit the output is scaled down with its aspect preserved.
- **input-region updates**: generated in **surface space** (cost ∝ displayed area,
  and it shares the display→native mapping with the hit-test), recomputed at most
  every 3rd presented frame (a frame-count throttle, not a wall-clock timer), only
  once for a static image, and force-refreshed on size changes. Above
  `MAX_INPUT_RECTS` the sampling grid is coarsened (2x2, 4x4, ...) and only then
  falls back to the bounding box. Optional heuristic via `ANIMA_REGION_STEP`:
  `auto` coarsens by powers of two until the list fits the 768-rect soft budget,
  bounds the display size (small overlays stay pixel-accurate); a number 1–32
  forces that grid; default `off`. The soft budget and hard cap themselves are
  configurable via `ANIMA_REGION_SOFT_RECTS` (default 768) and
  `ANIMA_REGION_MAX_RECTS` (default 4096). All three can also be changed at
  runtime with `anima-overlay-ctl region step|soft|max` or the tray's "Region"
  menu; the environment only seeds the startup values.
- **right-click menu**: a column of geometric icon buttons drawn **inside the
  surface** with wgpu (no font, no xdg_popup), placed near the click and clamped
  inside the surface. Items: cycle input mode (dots show the current mode),
  scale −, scale +, lock, reset, quit. Right-click opens/closes, left-click picks
  an item, and clicking elsewhere or leaving the surface dismisses. While open the
  input region covers the whole surface (the panel can sit over transparent
  pixels) and hover-hide is suspended; closing restores the per-pixel region.
  It is only reachable where the surface receives input — opaque pixels in
  `Opaque`, anywhere in `Full`; in `PassThrough`/hidden use the tray or ctl.
- **tray** (`ksni`): `StatusNotifierItem` menu with "Input", "Size"
  (100/75/50/25%) and "Region" submenus, "Visible"/"Lock position" checkmarks,
  "Reset position and interaction", "Quit overlay"
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
- `docs/code-audit.md` — the audit of the two large binaries and the splits that
  came out of it: before-and-after numbers, the move-then-let-the-compiler-talk
  method, the nine lessons that cost time, the function-length ratchet, and what
  was deliberately left alone and why.
- `docs/rust-guidelines.md` — our conventions, as do/don't pairs: Microsoft's
  Pragmatic Rust Guidelines reduced to the roughly forty items this project
  actually has to decide, the ones that do not apply and why, and the outstanding
  five in the order worth doing.
- `docs/python-app-feature-map.md` — the whole Python app as a porting checklist,
  with the parts that are not worth porting named as such.
- `docs/python-main-window-recovery.md` — how the main window's bytecode was
  recovered from the shipped build, including where the tools stop working.
- `examples/scan_workshop.rs` — scan a real workshop content dir and print what
  the app would see.
- `anima-workshop` (bin, no extra features): `roots`, `libraries`, `list`,
  `show <id>`, `download <id...>` (SteamCMD), `play <id> [asset]` (install if
  needed, then run it in `anima-overlay`), `upload <dir> [--title T] [--id N]
  [--user U] [--dry-run]` (writes `workshop.vdf` + `workshop_build_item`),
  `sync`; `web <id...>` with `--features workshop-web`.

## Verification

- Two crates, so two test runs. `anima-engine-rs`: **193** with default features,
  **196** with the Steam adapters, **198** with `bc7` (which needs a C++ toolchain,
  as does `overlay,bc7`), and **233** with every feature. `anima-ai`: **13**. Five
  are ignored: four studio tests that need a display, and the end-to-end AI
  removal, which downloads a 176 MB model.
- The workspace: `crates/anima-engine-rs` and `crates/anima-ai`, with the library
  and five binaries in the first. `src/ai` referenced nothing else in the library,
  and it was the only reason the tree depended on ONNX Runtime — so the overlay's
  dependency graph is 420 lines of `cargo tree` against 1239 with `studio`, and
  `ort` appears in the second and not the first.
- `cargo clippy --all-targets`: clean for default and all features.
- `tests/function_length.rs`: a ratchet on function length — it fails when any
  function passes 150 lines without being on a list with a reason, and when a
  listed one grows or shrinks. `docs/code-audit.md` explains why.
- `ANIMA_FN_LIMIT=100 cargo test -p anima-engine-rs --test function_length -- --nocapture` prints the
  eleven functions between 100 and 150 lines: the next pass, on purpose.
- Real data: `scan_workshop` parsed the installed `appworkshop_3474900.acf`
  (1 item) and listed item `3828587753`'s 6 `.apng` files.
- `anima-workshop`: `roots` / `libraries` found `~/.local/share/Steam`; `list`
  reported item `3828587753` with 6 assets; `show` listed the 6 `.apng` files;
  the keyless `web 3828587753` returned "hatsune miku and teto halloween trend",
  581.4 MiB and its tags.
- SteamCMD: installed (distro wrapper `/usr/bin/steamcmd`, real install under
  `/usr/lib/steamcmd`). `anima-workshop download 3828587753` really downloaded the
  item (609,690,948 bytes / 582 MiB, 6 assets) to
  `~/.local/share/AnimaEngine/steamcmd/steamapps/workshop/content/3474900/3828587753`
  and exited 0. The default download root is the XDG data dir because the program
  itself lives in a read-only system location; the missing-tool path and the
  fake-script path are still tested.
- `anima-workshop sync` reads the Steam client's `appworkshop_3474900.acf`
  subscriptions and returns the installed item (`3828587753`, 6 assets) without
  downloading; any missing item would be fetched through SteamCMD.
- Runtime rotate/mirror: `anima-overlay-ctl --instance <path> rotate 90` logged
  `transform 90° flip_h false flip_v false` and `flip h` produced
  `transform 0° flip_h true`; the surface is resized to the rotated size
  (output-fit, aspect preserved) and the transform is written back to
  `secondary_instances.json`.
- Download → sync → playback loop: `anima-workshop play 3828587753` resolved the
  installed item, picked its first asset and launched the overlay
  (`[workshop] .../TETO12.apng → .../anima-overlay`), which kept running until
  the 6 s test timeout. An uninstalled item would be downloaded first.
- Upload: `upload <dir> --dry-run` prints the generated `workshop.vdf`
  (`appid` 3474900, `publishedfileid` 0 for a new item, `contentfolder`,
  optional `previewfile`, `visibility`, `title`, `description`). Driven through a
  fake `steamcmd`, the real path wrote `<STEAMCMD_DIR>/anima_upload/workshop.vdf`
  and ran `+login tester +workshop_build_item <vdf> +quit`, then parsed the
  published id from `Success. Workshop item 9999999999 published.` into
  `https://steamcommunity.com/sharedfiles/filedetails/?id=9999999999`.
- Uploading a **real** item still needs the user's Steam account (`--user` or
  `STEAM_USER`); anonymous login cannot publish.
- Bounded decode: `ANIMA_MAX_DECODE_MB=64` on `tetonahall.apng` (448 frames of
  1350x1012, ~2.4 GiB raw) logged
  `decoded 7 frame(s) at 1350x1012, 36.5 MiB (1 of every 64 frames: 7 of 448 kept
  to fit the memory cap) in 1.60s` — bounded as asked, at the cost of choppier
  playback (tune with the env vars; the default 1 GiB keeps roughly 1/4 of it).
- `anim_meta.json` fps: with `{"<canonical path>": {"fps": 60}}` the overlay
  logged `restored instance: ... fps 0 (meta 60)` — instance `fps` still wins.
- Frame cache: entries are version 3 (magic + header + one zstd stream of raw
  RGBA). `examples/cache_bench.rs` on `miku12.apng` (64 frames, 1500x1125): zstd
  write 0.63 s / read 0.23 s / 46.3 MiB vs PNG-in-ZIP write 1.66 s / read 0.57 s
  / 43.5 MiB. Version 2 (PNG) entries still on disk were 192 MiB and 144 MiB for
  two large animations; they are orphaned by the key change and LRU-evicted.
- BC7 quality (uber 4, linear weights, `examples/bc7_bench.rs`): bochi455 6.8 MiB
  at 71.6 dB colour / 69.2 dB alpha (max alpha error 4); miku12 103.3 MiB at
  45.8 dB colour / 44.5 dB alpha (max alpha error 133), 2.7 fps — the average is
  visibly lossless, the worst silhouette pixels are not, which is why the CPU
  alpha stays authoritative for hit-testing and regions.
- BC7 in the overlay (two runs of `bochi455.apng`, 27 frames 512²):
  - pass 1 — `27.0 MiB decoded, 0.0 MiB bc7, rgba array texture`, then the
    background encode wrote a **7,077,916-byte** (6.75 MiB) sidecar;
  - pass 2 — `cache hit: 27 frame(s), 512x512 + bc7 blocks` and
    `27.0 MiB decoded, 6.8 MiB bc7, bc7 array texture`, i.e. the GPU now holds the
    compressed array.
  - Device check: `gpu Intel(R) Arc(tm) Graphics (MTL) | bc7 blocks: supported`.
  - miku12 (1500x1125) is the case this exists for: 412 MiB RGBA was over the
    256 MiB array budget (`just-in-time upload`), 103 MiB of BC7 is not.
- BC7-only storage (bochi455, cache cleared first): pass 1 logged
  `bc7 sidecar written: 6.8 MiB in 0.3s` and the cache held **exactly one entry**
  (`<key>.bc7.animacache`, no frames entry); pass 2 logged
  `bc7 cache hit: 27 frame(s), 512x512, 6.8 MiB blocks` and
  `6.8 MiB bc7, bc7 array texture`.
- Bounded decode and BC7 compose: tetonahall at the default 1 GiB cap decodes
  **112 of 448 frames** (`1 of every 4 frames`), so its BC7 sidecar is 146.1 MiB
  (encoded in 20.6 s) and it still fits the 256 MiB array budget. **The BC7
  just-in-time branch is therefore not yet exercised**: forcing it needs the full
  448 frames (613 MiB of blocks), i.e. a ~7 GiB peak decode that was deliberately
  not run. Note the sidecar bakes the decode limits at encode time, so raising
  `ANIMA_MAX_DECODE_MB` later does not restore the dropped frames.
- Crop + clamp + persistence (`bochi455`, 73% transparent): `cropped 512x512 ->
  136x132 (art starts at 184, 196)` then `clamped position to (1784, 948)` — i.e.
  `1920-136 / 1080-132`, so the art now reaches the right/bottom edge (it stopped
  184/196 px short before); CPU 27 MiB -> 1.8 MiB, BC7 6.8 MiB -> 0.5 MiB. Three
  consecutive runs kept `x = 1600, y = 752` in canvas space and restored the
  surface at `(1784, 948)` every time. An earlier attempt **did** drift
  (1743 -> 1656 -> 1344) because the sidecar did not record the crop — that is why
  the crop is in the block format now, and why the load/persist signs are covered
  by the round-trip above.
- BC7 blocks are stored as `<key>.bc7.animacache`, so the LRU pass counts and
  clears them with the frames; a missing or malformed sidecar falls back to RGBA.
- Cache robustness: a stale `.tmp` (1 MiB, backdated an hour) made the overlay log
  `removed 1.0 MiB of interrupted cache writes` and disappear, while a real entry
  survived; a sidecar written under `ANIMA_MAX_DECODE_MB=512` was refused on the
  next run with `ANIMA_MAX_DECODE_MB=8` (`bc7 sidecar was encoded with different
  decode limits; re-encoding`) and reused on the run after that.
- i18n: `anima-i18n verify <catalog>` printed 795 source strings / 31 languages
  and `rc=1` with `incomplete: ar, bg, cs, ...` (2 missing for `ar`/`bg`/`zh-CN`,
  19 for `cs`/`da`/`de`, one blank value and 39-124 untranslated each);
  `anima-i18n install` wrote the catalog to
  `<data>/AnimaEngine/anima_translations.json` (1,662,324 bytes) and `status` then
  read it back (language `zh-CN`, 793 entries, catalog version
  `2026-09-13-v4.7-final`).
- i18n primary set on the real catalog: `en` 776 entries / **19 missing** and every
  value identical to its key (the `en` block is a stub), `ja` 776/19 missing,
  `zh-CN` and `zh-TW` 793/**2 missing** with one blank each. `install --primary`
  trimmed the data file from 1,662,324 to **199,078 bytes** (4 of 31 languages) and
  `status` still resolves `zh-CN` (793 entries) from it.
- Packaging: the installer ran in a scratch prefix (`PREFIX=… XDG_DATA_HOME=…`) and
  for real. `desktop-file-validate` reports `ok` and `appstreamcli validate`
  `✔ 验证成功` (rc 0) for the installed entries and metadata; the niri snippet
  rendered with the absolute prefix
  (`spawn-at-startup "<prefix>/bin/anima-overlay"`); `--uninstall` left zero
  binaries behind; after the real run all four binaries resolve on `PATH` and the
  catalog is the 4-language file (199,078 bytes).
- Flatpak: built with `org.flatpak.Builder` (user install; a user `flathub` remote
  was added first because `--user` installs cannot see the system remote) and
  installed as `io.github.swordreforge.AnimaEngine` 0.1.0. Inside the sandbox
  `flatpak run --command=anima-overlay-ctl … status` works and the data dir is
  `~/.var/app/io.github.swordreforge.AnimaEngine/data/AnimaEngine/…`. The overlay
  run fails at
  `Error: "binding zwlr_layer_shell_v1: the requested global was not found in the registry"`
  while the same binary on the host logs `gpu … | bc7 blocks: supported` and
  renders — so layer-shell is not exposed to the sandbox in this setup.
- Flatpak globals, the decisive measurement: `ANIMA_DUMP_GLOBALS=1` on the host
  lists `wl_compositor v6`, `wl_seat v9`, `zwlr_layer_shell_v1 v5` (niri), while
  the same flag inside the sandbox lists a Mutter/GNOME set
  (`mutter_x11_interop`, `ext_background_effect_manager_v1`, `xdg_wm_base v7`,
  `wp_fractional_scale_manager_v1`, …) with **no `zwlr_*`**) — the sandbox attached
  to a different compositor, so layer-shell was legitimately absent.
- Hot reload (end to end): a copy of `bochi455.apng` was run, `touch`ed, then had
  `miku12.apng` swapped over it, then `bochi455.apng` swapped back. The log went
  `decoded 27 frame(s) at 512x512` → `rebuilt frames: 27 frame(s) 136x132` →
  `decoded 64 frame(s) at 1500x1125` → `rebuilt frames: 64 frame(s) 1266x1078` →
  `rebuilt frames: 27 frame(s) 136x132`, and the process id was **unchanged** across
  all three reloads: the texture, view, bind group, layer count and UV scale are
  rebuilt in place, with no surface recreated, no `restored instance` line and no
  state restore. Each swap also re-spawned the BC7 encoder, because overwriting the
  file invalidates the sidecar for its path (`bc7 sidecar written: 0.5 MiB`).
- Workshop watch (the callbacks fallback): `anima-workshop watch --once` printed
  `watching 1 item(s) in 2 root(s)` with `3828587753 6 asset(s)`, i.e. the Steam
  library and our SteamCMD root, deduplicated by id. Unit tests drive a temporary
  content root through added → updated (a file appended to the same id) → removed
  and assert the diff each time.
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
- Configure pinning (edge-squash fix): miku12 at 100% (1500×1125 on a 1920×1080
  output) is fitted to 1440×1080 (same 4:3 aspect) and niri's offered 1500×1034
  is bounced, so the frame is never stretched to another aspect.
- Region tuning: a 593×412 silhouette produced **990 rects at both 100% and 50%**
  — the rectangle count follows edge complexity, not display scale — while the
  surface-space scan costs 4× less at 50% (∝ area). Updates are throttled to every
  3rd presented frame, and a static image is scanned once.
- Optional region heuristic (`ANIMA_REGION_STEP`): `off` → 990 rects at step 1;
  `auto` → 475 rects at step 2 (990 > the 768 soft budget, capped at 2 by the
  412 px display dimension); `2` → 475 at step 2.
- Configurable budgets: `ANIMA_REGION_MAX_RECTS=300` → 235 rects at step 4;
  `ANIMA_REGION_STEP=auto ANIMA_REGION_SOFT_RECTS=900 ANIMA_REGION_MAX_RECTS=100`
  → the soft budget is clamped to the cap (100) and the region coarsens to 50
  rects at step 16. The effective values are logged at startup.
- Runtime tuning: `ctl region step auto` logged `region limits: step auto, soft
  768, max 4096`; `region max 300` made the soft budget follow (300); `region
  soft 100` then rebuilt to 235 rects at step 4 (auto is capped at 1 by the 206 px
  display, the hard cap took it to 4). `ctl status` reports the step/soft/max.
- Right-click menu: the unit test covers the clamped layout and hit-testing (first
  row → cycle input, last row → quit, misses outside); the smoke run initialises
  the wgpu menu pipeline and shader cleanly. Actual clicking is manual (no input
  synthesis available here).
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
- [x] Right-click menu drawn in-surface with wgpu (input mode / size / lock /
      reset / quit); icons only, no font or popup
- [x] Multi-instance: one process per animation, per-instance control file and
      tray item, broadcast CLI/portal with `--instance` targeting
- [ ] Multi-monitor: per-output layer surfaces
- [ ] GNOME/Mutter fallback (no layer-shell → compositor rules / normal window)
- [ ] HiDPI buffer scale + pin surface size across compositor reconfigures
      (the live2d-rs configure-bounce pattern)
- [x] Frame timing: GIF centisecond and APNG fractional delays were already
      exact through `image::Delay` (tests cover both); `anim_meta.json`'s
      per-path `fps` is now wired into playback and the instance's own `fps` wins
      over it; frames with a 0 ms delay fall back to 33 ms
      (`Frame::effective_delay_ms`)

### Main window (`anima-studio`, Iced)

Follow-up work is planned in [`next-steps.md`](./next-steps.md): the overlay lifecycle
manager (the tray no longer dies with each overlay), the Workshop metadata cache, the
search field with IME, and Workshop browse/search.

- [x] Toolkit decision recorded in `docs/ui-toolkit-decision.md`: **Iced 0.14**, in a
      separate binary, over GPUI/egui/GTK4/Slint/Tauri. Key measured fact: Iced is on
      `wgpu 27` while the overlay is on `wgpu 29`, so the two never share a device —
      they are separate processes, which is the design anyway.
- [x] Skeleton: `studio` feature + `anima-studio` bin. Header with the four primary
      languages (native names, `pick_list`), a refresh action, the installed Workshop
      library from the same discovery the CLI uses, and per-item details (path +
      assets). Text goes through the existing `I18n::tr()` catalog.
- [x] Verified: a headless test boots the state and prints
      `2 root(s) · 1 item(s) · catalog …/anima_translations.json · 793 entries`, and
      the window itself runs on niri/Wayland with no errors (launched, stayed up, no
      stderr).
- [x] CJK glyphs confirmed on screen: the language picker's native names
      (`English / 简体中文 / 繁體中文 / 日本語`) all render correctly, so
      `cosmic-text`'s system font fallback is enough and no font needs bundling yet.
- [x] Previews: a `preview.*` file inside the item directory when the author shipped
      one, otherwise the Steam CDN image from `preview_url` (keyless
      `GetPublishedFileDetails`), cached under `…/AnimaEngine/thumbnails/<id>.img` and
      decoded with the tree's own `image` crate (the `jpeg` feature was added). Verified
      end to end by an opt-in network test: `3828587753: Png 1283x751 decoded, cache
      …/thumbnails/3828587753.img (685382 bytes)`, and the second load is served from
      the cache without touching the network.
- [x] Actions: `Download (SteamCMD)` on a detached thread (`anonymous` login, the
      window stays responsive), `Subscribe… (Steam)` and `Unsubscribe… (Steam)` — both
      open `steam://url/CommunityFilePage/<id>`, because the subscription itself is the
      client's state and pretending otherwise would be a lie — and `Remove local copy`
      for items under *our* SteamCMD root only, behind a confirmation click.
- [x] Layout pass against a real screenshot: the Download button was mislabelled
      ("Search" — the key was reused when it changed meaning) and is now `tr("Download")`;
      raw absolute paths and the CDN `preview_url` are no longer rendered (they are
      unbreakable tokens that wrecked the wrapping) — the directory shows once, shortened
      to `~` and middle-elided, with the full path behind a `Copy path` button
      (`iced::clipboard::write`), and assets list file names only; a fixed 200px preview
      slot stops items with different aspect ratios from shifting the pane; 1px dividers
      group metadata / actions / assets (0.14 has no `rule` helper, so they are
      zero-content containers with a background); button hierarchy is
      `primary` (Download) / `secondary` (subscribe, unsubscribe, remove, refresh) /
      `danger` (confirm remove) / `text` (quit), and the list rows no longer use the
      accent colour for selection (a `▸` marker instead); the window has a minimum size.
- [x] The new UI strings are in `packaging/studio-i18n.json` and merged with
      `anima-i18n merge` (39 entries): `anima-i18n verify` reports the four primary
      languages complete (0 missing, 0 empty). The zh/ja values are assistant-written and
      want a native check, like the earlier four.
- [x] Overlay control wired to the same channel the tray, the portal and the CLI use.
      The panel lists every control file (`ControlState::instances()`), labels each with
      the animation it belongs to (the file name is only a 16-hex slug of the canonical
      path, so `instance_slug` reverses it against `secondary_instances.json`), and every
      button is load → mutate → `save`, where the builder bumps `generation`: hide/show,
      lock/unlock, click-through, ±10% scale, rotate 90°, flip H/V, reset, quit. A
      headless test asserts the discovery matches the control directory, and the CLI is
      interchangeable with the window by construction — they call the same builders.
- [x] The panel live-updates. A 1s `iced::time::every` subscription polls a fingerprint
      of every control file plus `secondary_instances.json` (`path:mtime:size`) and
      re-reads only when it moves. Note for anyone touching this: `iced::time::every`
      needs iced's `tokio` feature — the default `thread-pool` backend ships no timer, so
      without it the function does not exist. Verified live: 0 refreshes while idle, then
      `touch`ing one control file logged `overlay panel refreshed: 11 instance(s)` —
      exactly one reload. An overlay dragging or scaling itself writes its own control
      file, so that shows up the same way.
- [x] Packaging for the window: `packaging/anima-engine-studio.desktop` (`Exec=anima-studio`,
      `Icon=anima-engine`, single main category — `desktop-file-validate` warns about two)
      and, more importantly, the window's **application id**, which was empty before: the
      Linux `PlatformSpecific.application_id` now carries `anima-engine-studio`, matching
      the desktop file's basename as the iced docs advise, so the compositor can associate
      the window with its entry. `install.sh` installs the studio binary and entry only when
      the binary exists (an overlay-only build prints `skipped anima-studio …` and stays
      clean), and `install-remote.sh` gained `--studio`.
      Verified: `niri msg windows` shows `App ID: "anima-engine-studio"` (it was `""`), and a
      temp-prefix install validates `desktop-file-validate: ok` both with and without the
      studio binary.
- [x] Fixed a self-inflicted flicker: `view()` rebuilt `image::Handle::from_rgba(...)` on
      every frame, and every one of those gets a fresh unique id, so the renderer treated
      the preview as a new image and re-uploaded it — which the 1s overlay poll turned into
      a visible flash once a second. The handle is now built once when the item loads and
      lives in `Thumb::Ready(Handle)`; `view()` clones it (same id, cached texture).
- [x] The window can put an item **on the overlay**: each playable asset (`.apng`, `.gif`,
      `.webp`, `.png`) is a `▶` button and the action row gets a primary `Play` for the
      item's first animation. If an instance already plays that asset (its control file
      exists, matched by `paths::canonical_path` + `ControlState::path_for` — the same
      computation the CLI does) the overlay is simply un-hidden; otherwise the window
      spawns `anima-overlay <asset>`, which creates the layer-shell surface itself. The
      window deliberately does **not** try to own an sctk surface: the layer exists on the
      overlay's Wayland connection with its own wgpu 29 stack, while iced runs winit on
      wgpu 27, so embedding it would mean a second event loop and a second GPU device in
      one process for no benefit — starting the process gets the same result.
      Verified: the spawned process logged `decoded 64 frame(s) at 1500x1125` and restored
      its instance, and a headless test proves the "already has an instance" lookup lines
      up with the real control files.
- [x] **Liveness** (reported bug: open once, quit from the tray, then Play does nothing).
      A control file outlives its process — it carries the *desired* state on purpose — so
      the window's "the file exists ⇒ it is running" test was wrong, and after a tray quit
      Play just rewrote a file nobody was reading. The overlay now writes `<slug>.pid`
      (`<pid> <process name>`, `ControlState::announce`) at startup and clears it on exit,
      and the window asks `ControlState::is_running`, which checks `/proc/<pid>` and
      compares the recorded name against `comm` **by prefix** (Linux truncates `comm` to 15
      bytes) so a recycled pid cannot pass. The panel marks each instance `●`/`○`, and the
      poll fingerprint includes the `.pid` files, so starting or stopping an instance
      refreshes the panel even when no control file changes.
      Verified end to end on the reported cycle: open → `announced …/5e98d4bd8c627b31.pid`
      holding `303954 anima-overlay` with the pid alive → quit through the control channel
      (what the tray does) → **0 markers left** → open again → announced and decoded again
      (that step silently did nothing before the fix). The two new unit tests cover the
      round-trip and the recycled-pid case.
- [x] The panel follows the overlay you actually touch, instead of asking you to pick from a
      list of slug-named files (17 of them here). The overlay *claims focus* — `<slug>.focus`,
      a timestamp — on drag start and on every tray/portal action, and the window polls those
      markers and switches its selection when the newest one changes. It also defaults to a
      **running** instance rather than the first file in slug order, which is how the panel
      ended up pointing at a long-exited `conft.png` while the user dragged a live overlay:
      every button wrote into a file nobody was reading, so "nothing could be adjusted". An
      action on a stopped instance now says so instead of doing nothing silently, and the
      state line carries `●`/`○`.
      Verified: with two overlays up, a focus claim for the running one made the window log
      `[studio] following overlay …/head spinning_0_20261022_034140.apng`.
- [x] Workshop **metadata is cached on disk**: `thumbnails/<id>.json` holds Steam's answer
      plus a `fetched` stamp, trusted for a day. A fresh entry is used without touching the
      network, a stale one triggers a refetch, and when Steam is unreachable the old copy is
      served anyway — marked `stale` so the pane appends `· 缓存`. The flag is
      `#[serde(skip)]`: it describes this run, not the data.
      Verified both branches: online logged `metadata …: fetched` then `cache (1s old)` with
      the stamp unchanged (a second load does not refetch), and offline (cache aged past the
      TTL plus a proxy pointing at a closed port) logged `stale cache, Steam unreachable` and
      served `"teto face inspection"`. The TTL boundary is unit-tested, including a clock that
      jumps backwards. Not done: pruning entries for items that are no longer installed.
- [x] Selections are keyed by identity instead of position: the Workshop list by **item id**
      (the `BTreeMap` key) and the overlay panel by the instance's **control file**. Both used
      to hold an index into a vector — and the instance vector is rebuilt every second — so a
      rebuild could silently move the selection onto a different row, with the details pane,
      the remove button and the overlay controls acting on whatever that index happened to
      point at. A test asserts the id lookup and that the control path survives
      `reload_overlay`. This is the prerequisite for the search box: a filtered list cannot
      carry positions.
- [x] A search box over the list, above it, with a `×` to clear and a `shown/total` count
      while filtering. It matches the item id, the Steam title and the asset file names,
      case-insensitively; `matches` is a pure function with a unit test covering the empty
      query, the whitespace-only query, id, title in either case and an asset name. The
      selection is kept by id, so filtering the selected row away does not lose it — the pane
      keeps showing it. The placeholder reuses the catalog's `Search`, so it is translated.
- [x] Workshop search through `IPublishedFileService/QueryFiles`, which needs
      `STEAM_WEB_API_KEY`. The search row gained `Search Steam` — and `Workshop` to go back to
      what is installed — and results are listed with a `☁` marker as selectable rows whose
      pane shows the title, the subscription count, the CDN preview through the same thumbnail
      cache, and the Download / subscribe actions. Without a key the status line says exactly
      `STEAM_WEB_API_KEY is not set — a Steam Web API key is needed for browsing; installed
      items work without one`, and a unit test pins that wording (skipping itself when a key
      is present). `search_text` — trimmed, or nothing for a plain browse — is unit-tested.
- [ ] Not yet: list-row thumbnails once the library is big, plus the small items in
      `next-steps.md` (follow lock, notify-based refresh, distinct icon, shortcuts).

### Overlay lifecycle (`anima-shepherd`)

- [x] A long-lived process owns *starting* overlays, so a closed window no longer means
      nothing can come back (the hole behind "quit from the tray, then Play does nothing").
      `shepherd.json` lists the wanted animations (`anima-shepherd enable | disable | list |
      status | stop | quit`, plus `run --once` for tests); every second the supervisor asks
      `ControlState::is_running` for each wanted animation and starts what is missing, with a
      5s → 30s → 120s backoff for crash loops. The window hands an item over by enabling it
      in that file when a shepherd is up — the registry *is* the channel — and otherwise
      starts a process itself. The panel shows `shepherd ●/○`.
      Verified end to end: enable → exactly one start; `SIGKILL` the overlay → the shepherd
      logged `exited (signal: 9 (SIGKILL))` and started a fresh one; `anima-shepherd quit` →
      the overlay keeps playing, because surfaces belong to the overlays.
- [x] Three bugs the acceptance run exposed, all fixed and covered by tests:
      (a) the overlay announced itself only *after* decoding, so for the first ~5 seconds of a
      large animation the shepherd saw nothing running and would have started a second copy —
      it now announces before the decode, and a start inside its 10s startup window counts as
      "still starting" rather than a failure;
      (b) `marker_running` fell back to a 15-second heartbeat whenever `/proc/<pid>` was
      missing — which is exactly the *dead pid* case, so a SIGKILLed instance looked alive for
      fifteen seconds; where procfs exists a missing pid is now simply dead;
      (c) an unreaped child stays a **zombie**, and a zombie still appears in `/proc` with the
      same `comm`, so `is_running` said "alive" forever — both the shepherd and the window now
      hold their `Child` handles and `try_wait()` them each pass, which also yields the real
      exit status.
- [x] The tray moved into the shepherd: **one** icon for every overlay, with a submenu per
      instance (`●/○` + file name, `Visible`, `Quit this overlay`, `Disable (stop
      supervising)`), plus `Open main window` and `Quit shepherd (overlays keep playing)`.
      The overlay gained `--no-tray`; the shepherd starts its children with it, so N overlays
      are N surfaces and one icon. The menu is rebuilt from the registry and the liveness
      markers, and refreshed right after a change and every five seconds otherwise (the dots
      also move when something the shepherd did not start exits).
      Verified: the shepherd logged `tray registered (StatusNotifierItem)` while the overlay it
      started logged `tray disabled (--no-tray): the shepherd owns it`.
- [x] The tray menu was wrong twice over, both fixed after actually looking at it: it listed
      only the **registry**, so with every entry disabled it offered nothing to start and its
      one action (`Visible`) wrote into a file nobody was reading; and `Open main window`
      silently disappeared when the studio was not installed anywhere the shepherd looked.
      It now lists the **union of the registry and every control file on disk** (running
      first), gives a stopped entry `Start (and supervise)` — which enables it in the registry
      so the supervisor starts it a second later — keeps `Visible` / `Quit this overlay` for
      live ones and `Stop supervising` for enabled ones, and always shows the main-window item,
      explicitly as `Open main window (not installed — build --features studio)` when it cannot
      find it. `anima-studio` is installed beside the other binaries, and the shepherd logs
      where it looked: `[shepherd] main window: …/anima-studio`.
- [x] `anima-shepherd prune [--dry-run]` and `forget <animation>`, so the manual sweep that
      cleaned ten files once is a command. The decisions are pure functions over an injected
      `exists`, tested without touching the disk: registry and persisted paths whose animation
      is gone, control files whose animation is gone (or whose slug means nothing and no
      process owns it), markers whose pid is gone, and cache entries whose Workshop item is no
      longer installed. `prune` prints the plan, backs up both JSON files, then applies;
      `forget` refuses while the instance is running and never touches the animation file.
      Verified with fake animations: the dry run listed exactly one registry path, one control
      file and one stale focus marker; the real run removed 2 files and 1 reference and left
      the 16 real control files alone; afterwards `prune` says `nothing to prune`.
- [x] `prune` takes `--no-backup` (for a machine that already has a backup), and by default
      it keeps only the newest pair: after taking the copies it calls `trim_backups(dir, 2)`,
      so repeated prunes no longer accumulate `*.before-prune-*` files. Verified: six old
      backups became two (`kept the newest backup pair (dropped 6)`) and a `--no-backup` run
      left the count unchanged while printing `(no backup: --no-backup)`.
- [x] An intentional quit is not a crash. Every way of closing an overlay on purpose — its
      own menu, the tray, `anima-overlay-ctl quit`, the window's Quit button — goes through
      `ControlState::request_quit`, which leaves `quit: true` in the control file. The
      supervisor now reads that flag when it finds an instance stopped: if it had seen that
      instance alive (so the flag is not a leftover from a past session) it turns supervision
      off instead of restarting it, clears the one-shot flag and logs
      `… was quit on purpose; not restarting (supervision off)`. Verified live: choosing Quit
      in the overlay's own menu produced `exited (exit status: 0)` and that line, and the
      registry entry went `off`. A unit test covers the flag, the one-shot clearing, the
      registry disable and the "never restarted" rule.
      Note for the future: `kill`-ing a supervised instance is a *crash* by design, so it
      comes back; close it with the menu, the tray, the CLI or `disable` first.
- [x] Three of the small items. The overlay panel gained a **follow lock**: `follow ●/○` in its
      header turns "select the overlay you just touched" off, so dragging two overlays in turn
      no longer bounces the selection; the focus stamp is recorded either way, so unlocking does
      not jump to an old claim. The window has its **own icon** (`anima-engine-studio.svg`, a
      window mark rather than the overlay's), wired into its desktop entry and `install.sh`
      (including uninstall). And the remote-result pane's `(not installed)` label goes through
      the catalog (`not installed`, four languages).
- [ ] Deferred, with the reason rather than a promise: window **keyboard shortcuts** — iced 0.14
      removed `keyboard::on_key_press`, so they need the event-listening API; **list-row
      thumbnails** — they need a prefetch policy, since a preview is fetched on selection only
      today, which is what keeps the API calls polite; **Workshop pagination** — page 1 with 50
      results is what `query_files` is asked for; **`notify`-based instant refresh** of the
      control directory — the 1s poll is fine and `notify` would be a new dependency.
- [x] The window's colours come from **Material 3**: `iced_material` 0.14.2 (which targets iced
      0.14 exactly — `iced_widget ^0.14`, `iced ^0.14` as a dev-dep) supplies the dark colour
      scheme, and only its `Base::palette()` is used. The theme *type* stays `iced::Theme`, so no
      view signature and no button style changed, and deleting `Studio::theme` reverts to iced's
      own theme. The divider stopped hardcoding `rgba(1,1,1,0.10)` and now takes the theme's weak
      surface tone, so it follows whichever theme is active.
      What this is *not*: the shapes, elevation and type scale are still iced's; truly Material
      widgets would mean switching the theme type to `iced_material::Theme` and adopting its
      widget modules (1.2 MB of crate we only take the palettes from today). Light mode and
      following the system preference are also open — the palette is fixed to `Theme::Dark`.
- [x] The theme type **is** `iced_material::Theme` — palette, container and button styles, text roles —
      and the look did warrant it. What is still open from that list: **light/dark by
      `prefers-color-scheme`** (fixed to `Theme::Dark`, and the palette's own value comes from noctalia's
      file) and **pseudo-glass containers** (translucent surface + gradient border, explicitly without
      blur, which a Wayland client cannot do).
- [x] The palette comes from **noctalia**. Its own "single source of truth for downstream
      tools" — `$XDG_CACHE_HOME/nyxuri/palette.toml`, `key = "#rrggbb"` — is parsed for the roles
      iced can use (primary, surface, on_surface, error, with secondary/tertiary as
      success/warning) and the window's theme is built from it; without the file it falls back to
      iced_material's dark scheme. The parser is a pure function with a test that pins hex
      parsing, the role mapping, and the "not a palette" cases. Verified live:
      `[studio] theme: Material You from …/palette.toml`.
      It is read **once at boot**, so a wallpaper change shows up on the next launch —
      watching the file (notify) is the follow-up. The theme *type* is still `iced::Theme`, so no
      view signature changed.
- [x] Step (1) of the theme switch: the noctalia palette now also parses into
      `iced_material::ColorScheme` (`parse_color_scheme`) — the four quartets (colour/text/
      container/container-text), the five surface-container tones, `on_surface_variant`, both
      outline tones, and `shadow`/`scrim`. The inverse roles are not in the file, so they come
      from swapping the surface pair rather than being invented. `palette_roles` is shared with
      the iced palette, so there is one parser. The UI does not consume the scheme yet, which is
      why this commit changes no behaviour; the test pins every role **and parses the live file**
      when it exists.
- [x] The theme **type** is `iced_material::Theme`, so the window draws with Material 3's own
      widget styles: `type El<'a> = Element<'a, Message, iced_material::Theme>` through the view
      signatures, `divider` returns a container of that theme and takes its line colour from
      `surface.container.high`, the buttons map onto `filled` (primary), `outlined` (secondary) and
      `text` (quit), the two destructive ones use a `danger` style built from the error role, and
      the search box carries its own `text_input` style because iced_material has no catalog for
      it. `resolve_theme()` returns noctalia's `ColorScheme` through `Theme::new(…, scheme)` and
      falls back to `Theme::Dark`; the now-unused iced-palette parser and its test went with it.
      Verified: 0 warnings, 13 studio tests, clippy 0, and the window logs
      `theme: Material You from …/palette.toml`.
- [x] The Material step the plan called "swap the widgets" turned out to be smaller:
      `iced_material` ships **styles and Catalogs, not widget wrappers** — its `container`/`text`
      modules are style functions, and there is no `iced_material::button(...)`. So the work was
      applying them: the search box, both pick lists and all three scroll areas now use its
      `default` styles, and the details pane is a `container::surface_container_low`, which gives
      it that surface tone and the crate's 4px corner radius. The buttons were already on
      `filled`/`outlined`/`text` (its `styled()` makes them pills).
      What the crate does **not** provide: the Material type scale (its `text` styles are colour
      roles, not sizes) and elevation beyond what `styled()` computes — so "more Material" from
      here means picking our own sizes and paddings, not adopting a widget set.
- [x] Two things the eye caught. The destructive buttons were square because `danger` was built on
      a default `Border`; it now carries the same pill radius that iced_material's `styled()` gives
      every other button. And refresh became a Material 3 **floating action button** — `↻` in a
      round primary-container circle, 36px in the header and 28px in the overlay panel — because
      refreshing is an affordance rather than a word.
- [x] The right pane now answers "what does *this* asset look like". Its three areas read
      top-to-bottom: the item's cover, title, counts and item-level actions; the asset list,
      where a click selects (the rows no longer play); and the tuning area beneath it, showing
      the selected asset's first frame with the play button and the parameter buttons.
      `anim::first_frame` decodes one frame and stops — a 300-frame APNG is not decoded to show
      a picture — the still is scaled to 512px and cached as `thumbnails/a<fingerprint>.img`,
      keyed by path + size + mtime, and the decode runs on the task pool.
      The parameters are **staged**, not applied to a running instance: they land in the
      previewed asset's own control file, which may not exist yet, and because Play starts an
      overlay that reads exactly that file, what is set here is what it starts with. The panel
      and the tuning area share one widget set (`overlay_params`), pointed at a running instance
      and a staged file respectively, so the two cannot drift, and `apply_action` is the single
      definition of what each action does.
      Honest notes: a fade-in's first frame can look empty (the file's doing — `first_frame`'s
      docs say so); previews are cached per path in memory for the session, so an asset
      re-downloaded while the window is open keeps its old still until the row is re-selected;
      and the item-level Play is gone, because it had to guess which asset was meant.
      Verified: 0 warnings, 159 lib + 3 anim + 16 studio tests, clippy 0, plus the ignored
      `decodes_and_caches_a_real_asset_first_frame` against a real library
      (`animatedddds.apng → 512×384`, cache read-back identical).
- [x] Two follow-ups the layout review turned up. Switching items now drops the asset on show and
      the parameters staged for it, instead of leaving the tuning area pointing at the old item's
      file — and "Remove local copy" drops them too when the file went with the item. The states
      without a picture are now one `placeholder` box: same height and surface as a real preview,
      a large muted glyph and one line of explanation, so switching between them does not move the
      pane. Button rows `.wrap()` now, which stops a narrow window from squeezing "Flip V" into a
      two-line vertical pill, and the overlay pick list shortens its label instead of spilling a
      Steam path out of the row.
- [x] The left column now belongs to the overlay panel, and the workshop browser floats. The list
      used to be a permanent region above the panel: at this window size it had room for a row or
      two, it hid the installed items whenever Steam results replaced them, and it squeezed the
      panel. It is a `stack!` layer over the left half now — the scene, then a scrim that takes the
      dismissing press, then the picker — with its rows, its count and the Steam/local switch in
      its own header. Choosing a row, the `×`, or a press outside closes it; typing opens it, and
      so does the `Workshop` button in the search row. Esc is still not available (0.14 removed the
      key API, see next-steps), so those are the ways out. No new i18n keys, and one new field
      (`picker: bool`) is the whole state.
- [x] The overlay drop-down shows the animation's **file name** (`● TETO12.apng`) instead of a
      shortened Steam path, which wrapped onto two lines in a closed pick list and said nothing
      the file name did not. Two instances can ship assets with the same name, so the label grows
      to `3828587753/TETO12.apng` only when that is actually true of the list in hand; the full
      path stays in the item pane, one click away.
- [x] Two things the eye caught in the empty state. Long asset names — `ssstik.io_1790982665008_0_20261003_024542.apng`
      — wrapped the pick list onto two lines, so labels are now cut in the middle to 28
      characters: `truncate_middle` keeps the head and the tail, which keeps the extension, and
      counts characters rather than bytes so a CJK name cannot be cut mid-glyph. And with nothing
      selected, the right pane rendered `note` while the status bar below rendered the very same
      string, so the same path/status line appeared twice; the pane is an empty state now
      (`placeholder` at `Length::Fill`, "Pick an item to see its details") and the note stays in
      the status bar where it belongs.
- [x] The details pane opens on the picture. The row of "shortened path + Copy path" that used to
      sit above the cover is gone: `Copy path` moved into the actions row next to download,
      subscribe and remove — where the rest of the item's verbs already were — and the full path is
      a hover tip, on the cover and on the asset preview in the tuning area (which shows that
      file's path, not the item's). A raw path is an unbreakable token that wrecks the wrapping, so
      not laying one out at all is also the tidier layout.
- [x] Motion, from iced's own animation module — it re-exports `lilt`, so this needed **no new
      dependency**: `Animation<f32>` plus `Easing::EaseOutCubic`, built by a one-line `fade_in(at)`.
      The item cover and the asset preview fade up over 160ms when their picture arrives instead of
      popping in, and both loading placeholders are a spinner now (`◐ ◓ ◑ ◒`, one turn in 360ms)
      rather than a bare `…`.
      The cost is bounded *and measured*, which is the point of doing it this way: frames are only
      asked for while something moves (`animating()` is an unfinished fade or a `Thumb::Loading`
      anywhere), so an idle window does 7 scheduler ticks in 3s and 1.33% of one core — not the
      ~188 ticks a permanent 16ms tick would cost. During startup (frames on, thumbnails decoding)
      the process averages ~11%.
      Deliberately **not** done: a shared selection highlight that slides between rows (it needs row
      geometry, i.e. fighting iced's layout, for maybe 10% of the feel over a per-row fade), and
      reduced-motion support (iced does not expose the system setting — it would have to be an env
      var or a setting of ours; worth doing before this ships widely).
- [x] The picker arrives instead of appearing: a 200ms slide in from the left on `EaseOutBack`,
      whose overshoot is what makes it land rather than stop. It runs once per *opening* — typing
      keeps the picker up, and restarting the slide on every keystroke would make the list twitch
      under the cursor — and the travel is carried by the container's left padding, because iced
      offers no way to fade or offset a container (`.opacity` exists for `image` and a few others,
      not for this).
- [x] `ANIMA_NO_MOTION=1` turns all of it off: fades resolve instantly, the spinner holds a still
      glyph, the picker does not slide, and **no frame subscription is ever started** — `animating()`
      returns false before it looks at anything else, so this is structural rather than a matter of
      measuring. Measured anyway: the window runs 2 scheduler ticks in 3s with the switch set, which
      is the one-second overlay poll and nothing more. iced does not expose the desktop's own
      reduced-motion setting, so this environment variable is the switch we have.
- [x] The status line **does** fade when the note changes, and the setter is what unblocked it: `say()` sets
      the text and restarts `note_fade` in the same call, which was exactly why the twenty-odd assignment
      sites had to be collected first. The view reads the fade's value, and `animating()` keeps the frame
      clock running while it interpolates.
- [x] The flake is fixed, and it was exactly what it looked like: `live_instances_cover_every_control_file`
      compares two scans of the **real** control directory, and `an_intentional_quit_stops_supervision`
      creates and removes a control file in the same process — a directory scan is not atomic, so a
      write landing between the two reads broke the comparison about one run in three. The tests that
      touch that directory now take a module-level `CONTROL_DIR` mutex (poison-tolerant, so one
      failing test cannot cascade), and six consecutive full runs of the lib suite are green.
- [x] The status line fades in when it says something new. It is written in **nineteen** places, so
      instead of routing all of them through a setter, `update` grew a wrapper: `handle` keeps the
      match, and the wrapper compares what the line *shows* — `note`, or `remote_note` while a search
      has something to say — against what it showed last. The guarantee is the same as a setter's,
      and it is the only one that matters: the fade begins in the same message as the write. Started
      a message later it would draw the new line at full opacity once and then fade it, which reads
      as a flash. Nineteen hand edits were also nineteen chances to miss one. The alpha rides on the
      text colour, because iced has no way to fade a container.
- [x] Workshop search paginates properly, and the picker says how many pages there are. The old code
      sent `page=1` and read a total from nothing, which is exactly the complaint: you had to walk
      forward to learn there was more. Steam's documentation is blunt — `page` is **capped at 1000**
      and "cannot paginate deeply"; the `cursor` parameter (first request `"*"`, then the
      `next_cursor` of each response) is what they tell you to use. `query_files_page` sends the
      cursor now and parses `QueryPage { total, next_cursor, items }`.
      The pager shows `2/37` between `‹` and `›` — a button with no press handler is iced's disabled
      state — and the status line says `about 1823 Workshop result(s) · page 2/37`. Going back works
      because each page's cursor is recorded before the request that uses it, and the search box can
      be edited without changing what "next page" means, because the query the results belong to is
      kept separately.
      Honest notes: Steam's `total` can be rounded for very broad queries, which is why the UI says
      "about"; a page never visited needs one extra round trip to show, since its cursor is not
      known until Steam hands it over.
- [x] The missing-key message says where to get a key now — `STEAM_WEB_API_KEY is not set — …` with
      `https://steamcommunity.com/dev/apikey` — because "not set" on its own is just a complaint. A
      test asserts the URL is in the message.
- [x] The key found a bug on its first run, which is what it was for: `QueryFiles` reports `result`,
      `visibility` and `banned` as **`false`/`true`** where `GetPublishedFileDetails` reports `0`/`1`,
      so the response failed to parse at all — `invalid type: boolean \`false\`, expected i64`. Those
      three fields go through an `int_or_bool` deserializer now, which reads either shape.
      Verified against the live API: a search for "teto" returns **50 rows with `total` 70**, and the
      second page follows the cursor. The first run made it look as if the pages tile the result set
      exactly (50 + 20 = 70) — the entry below says why that was luck rather than a property.
      The ignored `searches_the_workshop_and_pages_with_the_cursor` keeps that honest, and is the
      reason to have such a test at all: it caught the parse failure before a user could.
- [x] The Steam Web API key can come from a settings file now, which is what makes the window usable
      when it is started from a desktop entry or a Flatpak — no shell has exported anything there.
      `studio.json` (the studio's own settings, kept apart from the game's `settings.json`) holds
      `steam_api_key`; `resolve_api_key` reads the environment first and the file second, because an
      exported variable is the deliberate, temporary override, and it treats blank as absent so an
      empty export cannot shadow a good file. The missing-key message names both, with the file's
      shortened path and the URL to get a key.
      Proved on this machine: the key was written to `~/.local/share/AnimaEngine/studio.json` (mode
      600) and three live runs of the ignored search test pass with `STEAM_WEB_API_KEY` explicitly
      unset — which is the desktop-launch case.
- [x] The live test also taught a lesson about what a cursor guarantees, and the assertions were
      changed to match it: page 2 came back with **26, then 14, then 26** rows across three runs of
      the same query, because Steam's Workshop list is live and ranked and shifts between two
      requests, so a cursor can repeat or skip an item. "50 + 20 = 70" is not a property; "the window
      moved on" is. The test asserts that page 2 is non-empty and not identical to page 1, and a
      comment says why the exact counts are not asserted. The window's pager inherits the same
      caveat, which is the real reason it says "about".
- [x] Typing no longer opens the list. Every keystroke called `open_picker`, so the installed list
      popped up under the cursor while the user was still typing a word — one character was enough.
      `Message::Search` only filters now, and the picker opens on **Enter** (`on_submit`, which iced's
      `text_input` has had all along) or on the button next to the field. The Steam search is
      deliberately unchanged: type the whole thing, press `搜索 Steam`.
      The popup was also labelled `Workshop` while showing what is installed on this machine, which
      is exactly why it read as "a Workshop search just fired". It says `Installed` now — in the
      picker's title, the button that opens it, and the button that comes back from Steam results.
      One i18n key, 820 source strings.
- [x] Reviewing the two call chains that both mean "installed" found **two real bugs**, and they were
      the same mistake twice — state that belonged together was kept apart:
      1. `Message::OpenPicker` (Enter, and the button next to the field) only set `picker = true` and
         never touched `remote`, so with Steam results on screen it re-opened the *Steam* list under a
         button labelled `Installed`. `ShowInstalled` had its own seven-line copy of the reset, which
         is exactly how the two drifted apart. Both go through one `leave_remote()` now.
      2. `RemoteResults` landed unconditionally, so a slow search answering *after* the user had left
         the results switched the list back to Steam, over whatever they had moved on to. Replies now
         carry the request number they belong to and are dropped when it is stale.
      Also fixed while in there: `ClearSearch` emptied the box but left Steam results for the query
      that had just been deleted on screen — the box and the list have to agree.
      Kept deliberately: the selection survives the switch (a remote row stays selected when you come
      back to Steam), and choosing a row closes the picker in both modes, because both send the same
      `Message::Select`.
      Verified: 0 warnings, clippy 0, 162 lib + 28 studio tests (4 ignored), two of them new — one for
      each bug above.
- [x] Installed rows show their real titles without being clicked for them. The rule was already
      "metadata title, else the first asset's file name", but metadata was only fetched when an item
      was **selected** — so a row showed `ssstik.io_1790…apng` and turned into a title the moment you
      clicked it, one item at a time, which is exactly what the user described.
      `GetPublishedFileDetails` is **keyless** and takes up to 100 ids per request, so the installed
      list is one request now, made at boot and on Refresh: `installed_meta_task` asks only about ids
      whose cached answer is not fresh. A second run with a warm cache asks nothing at all — offline
      and instant, and no flip at all.
      Proved on this machine by moving a single cache file aside (not deleting it):
        launch A: `metadata: asking for 1 of 9 installed item(s)` → `1 item(s) arrived in the batch`
        launch B: no metadata line at all — nothing to ask for, no request made
      The first attempt at this test showed *no* metadata line either, which looked like a bug and
      was not: all nine items already had fresh cached answers from today's testing, so there was
      genuinely nothing to ask. Moving one file aside is what made the difference visible.
      The file name stays as the immediate, offline fallback for the first paint and for libraries
      past the first hundred, and it is still in the detail pane and the asset list, where the actual
      file is what matters. `meta_from` is shared by both paths now, so the batch and the single-item
      fetch cannot describe an item differently.
- [x] The reason the rows kept showing file names, found by reading the data instead of the picture.
      The freshness check consulted the **disk cache** to decide what to fetch, while the rows read the
      **in-memory `meta`** map — so a warm cache meant "ask for nothing" *and* "show nothing", and
      every row fell back to its file name. The check was doing exactly what it said; the display still
      looked broken. `refresh` now hydrates `meta` from the cache before anything is drawn, through a
      pure `hydrate_from` that skips stale answers and is tested without a cache on disk, and
      `read_cached_meta` is shared by the decision and the hydration so the two cannot disagree.
      Result: `metadata: 9 item(s) came from the cache`, no request, and rows show
      `kasane teto hooked trend` where they showed `Pre-comp 2_0_20260930_032758.apng`.
      The screenshot could not settle it and neither could I: the vision read the rows as "file names",
      which is indistinguishable in a picture from a Steam item whose **title** is a file name — common
      on this Workshop. Reading the cached JSON next to the item directory is what showed the titles
      had been there all along:
        3810914663: title 'kasane teto hooked trend'  ·  file 'Pre-comp 2_0_20260930_032758.apng'
      Same lesson as the two bugs before it: one decision read from two places will eventually go two
      ways, and a picture is not evidence about data.
- [x] The overlay panel refreshes faster: `OVERLAY_POLL_MS` is 250ms instead of 1000, so a tray action
      or a drag shows up within a quarter second instead of up to a full one. This is the free half of
      "notify 即时刷新": `notify` is **not** in the tree, so the real thing would add a crate, a watcher
      thread and a channel; a shorter poll gets the perceived result with none of that. Measured: idle
      CPU stays at **1.0% of one core** either way — the poll's cost is not measurable on this machine,
      because each tick is a handful of `stat` calls and iced only redraws when the fingerprint
      actually changed.
      Correction to an earlier note in this file: the "ticks" quoted there were **CPU jiffies**, not
      scheduler wakeups. Seven jiffies in three seconds is 70ms of CPU, not seven wakeups. The
      conclusion held; the unit did not.
- [x] The shepherd has a session-bus face (`dbus` feature, reusing the `zbus` copy the tray already
      pulls in — **no new crates**). `dev.AnimaEngine.Shepherd` at `/dev/AnimaEngine/Shepherd` exposes
      `Start(animation)`, `Stop(animation)`, `List()` and `Status()`. Each is a line of registry work,
      so the bus cannot do anything the CLI cannot, and `Intent` holds no state of its own: every call
      reads the registry, changes it and writes it back.
      Verified live against a freshly started shepherd, using a path that does not exist so nothing
      could be spawned:
        Status  → s "0 wanted, 5 known"
        Start /tmp/dbus-e2e.apng → b true
        List    → as 1 "/tmp/dbus-e2e.apng"
        Stop    → b true        (then `anima-shepherd forget` removed the test entry)
      The daemon was stopped afterwards, leaving the machine as found, and
      `the_bus_writes_the_same_registry_the_cli_would` covers the registry semantics in a test.
      Packaging had to follow: the feature is off by default, so `install-remote.sh` builds with
      `overlay,dbus` now (plus `bc7`/`studio` as before) and both build hints in `install.sh` name
      `dbus` — otherwise a shipped shepherd would have been built without the interface and the
      feature would exist only on a developer's machine.
- [x] The tabs are in (`docs/studio-screens-plan.md`, steps 1–2): 工坊 / 浮层 / 设置 / 诊断 — one
      `Tab` field, one `Message::Tab`, one `match`. 工坊 is pixel-for-pixel what it was (checked by
      screenshot afterwards), the picker float is gated to it, and 浮层 is still an honest placeholder.
      设置 writes `studio.json` immediately: the Steam key — which is **never rendered**, since the tab
      says "stored" or "not set" and the field clears after saving, so a screenshot cannot leak it — and
      `reduce_motion`, which is where the motion preference lives now, with `ANIMA_NO_MOTION` kept as the
      override and the note saying so when it is set. 診断 (诊断) prints version, roots, installed count,
      catalog, shepherd state, the D-Bus name, the thumbnail directory and the palette path, with a
      `Copy all` for bug reports. 823 source strings, four languages.
- [x] The picker float is gone. The workshop results are inline in the left column, as asked: the
      search row, then `已安装 · 9/9` (or `搜索 · N` with `‹ n/T ›` while Steam results are up), then the
      rows themselves — and the `浮层` panel moved to a tab of its own (`tab_overlays`). Deleted with
      the float: `picker_layer`, `scrim`, `picker_in`, `open_picker`, the `picker` / `picker_motion`
      fields, the two messages that only opened and closed it, the two tests whose only subject was its
      behaviour, and the `stack!` layer the frame used to build. `frame()` is now just header, strip,
      body, status.
      What it looks like: the rows read as Steam titles — `Kipfel touch the screen`, `Cute Dancing GIFs`,
      `kasane teto hooked trend` — instead of file names, which is the metadata batch from earlier doing
      its job somewhere visible.
- [x] The panel really is instant now: the studio watches `overlay_control/` and the data directory with
      `notify`, so a write from the tray, the CLI, the portal or another instance arrives as an event. The
      event only says "something moved" — the fingerprint check that already existed decides whether
      anything changed, so a burst costs one rebuild. The watcher sits on its own thread (it is blocking
      and must outlive the stream) and feeds an unbounded channel that `futures_util::stream::unfold`
      turns into a subscription; a watcher that cannot be created logs and leaves the poll in charge.
      The poll stays, slower: **2000ms** instead of 250, as the safety net for the events a watcher can
      miss (an overflowed inotify queue, a directory replaced under it).
      Cost, stated rather than implied: this time it **is** a new dependency — nothing in the tree spoke
      inotify — `notify 8.2.1` with `notify-types`, `inotify` and `walkdir` (mio was already there for
      tokio). The `studio` feature now also declares `futures-util` and `tokio`, both already compiled
      for the overlay build.
      Verified: the watcher logs `watching …/overlay_control for control changes`, and touching one
      control file produced exactly one `overlay panel refreshed` within the second — with the poll at
      2000ms, so that was the event path and not the timer.
- [x] The 浮层 tab is one row per instance, each with its own controls, and the scale has a **slider with
      1% steps** (`MIN_SCALE_PERCENT..=MAX_SCALE_PERCENT`, `.step(1u32)`) beside the old ±10% buttons for
      coarse jumps. Each row carries hide/lock/click-through, rotate, both flips, reset, and a Quit of its
      own; the running marker and the state summary stay where they were.
      That meant the action had to name its target: `Message::Overlay` became
      `Message::OverlayOn(PathBuf, OverlayAction)`, and `apply_overlay` became `apply_overlay_to(control,
      action)`. `Message::SetScale(PathBuf, u32)` is the slider's and writes through `set_scale_percent`,
      so the overlay's own bounds do the clamping.
      Deleted with the panel: `overlay_panel` (114 lines), `InstanceChoice` with its `Display`,
      `instance_choice`, the `RefreshOverlay` and `SelectOverlay` messages, and the `pick_list` that chose
      an instance to act on. The follow switch was **not** dropped — it moved to the tab's heading, because
      following is a property of the whole list, not of one instance.
      Verified: 0 warnings, clippy 0, 162 lib + 29 studio tests (4 ignored), window relaunched. The rows
      themselves need a click on the tab to see, which is the one thing this cannot screenshot.
- [x] The overlays tab was refined where the first pass was clumsy: **running instances sort first**
      (`sort_by_key(|i| !i.running)`, stable, so order is otherwise kept) and the stopped ones are
      read-only rows — name, marker and summary, no controls, which is all they can honestly offer.
      The **duplicate scale buttons are gone** from this tab: the row has a 1% slider and, beside it, a
      text entry that takes an exact number (`ScaleDraft`/`CommitScale`, digits only, three at most,
      clamped by `set_scale_percent`). `overlay_params` grew a `scale: bool` so the tuning area, which has
      no slider, keeps its ±10% buttons while the tab does not repeat them.
      **Rotation stays quantised to 0/90/180/270 on purpose**: `ControlState` normalises it to those four,
      and the overlay's crop and UV maths is written in terms of axis swaps (`Orientation::swaps_axes`).
      A finer rotation is a rendering change — arbitrary-angle UV mapping and a different bounding box —
      not a slider on top of what exists. Recorded as its own item rather than faked here.
- [x] The per-instance rows stopped stacking. `overlay_params` builds **one wrapping row** — hide, lock,
      click-through, the scale buttons where they apply, rotate, both flips, reset — and the instance row
      puts Quit on the end of it. Four lines of buttons with half the width empty was the ugliest thing in
      the first pass; `.wrap()` keeps the narrow case from overflowing without spending height when there
      is room.
- [x] The buttons whose text flips with state go through one `toggle()` now: the label always says what
      **pressing it will do** — "Show" while hidden, "Unlock" while locked, "turn motion off" while motion
      is on — and the fill says what the state is. Four call sites share it, so the convention cannot drift
      the way "Hide" did when each row invented its own wording. The follow button keeps its dot, because
      there the state *is* the point and the dot says it faster than a verb.
- [x] **Remove local copy is danger-red from the start.** It was `secondary` until the confirm step armed
      it, so the first click on something that deletes a directory looked like an ordinary action and the
      warning only appeared afterwards. The second click is still there; the warning now comes before the
      confirmation rather than after it.
- [x] Removing a local copy asks with a **warning that takes the window**, the way UAC does, instead of
      arming the same button. `Remove local copy` never changes its meaning now: it opens a card with
      **Delete** and **Cancel** side by side over a scrim, and the scrim itself is the "no" — press
      anywhere outside and nothing happens. The card names the item and says what will *not* happen (no
      unsubscribe, your Steam library untouched).
      That also closes a latent bug the inline version carried: `pending_remove` was never cleared when the
      selection moved, so coming back to an armed item showed `Confirm remove` straight away — a warning
      that had already been forgotten. With a modal there is no window in which the selection can move
      while it is armed, and both buttons clear it.
- [x] i18n caught up with the last few sessions. A dozen UI strings had been written straight into the
      code — the removal warning's body, `Cancel`, the settings explanations, the empty overlays tab, the
      motion toggle's labels, and the key's "stored"/"not set" state — all English, some of them the only
      English left on their screen. They go through `tr()` now, in four languages: **834 source strings**,
      all four primary languages complete.
      Deliberately still English: the **diagnostics block**, whose whole purpose is being pasted into a bug
      report — a translated key list would be less useful, not more. The status-line notes were left for
      their own pass at the time; that pass is the entry below.
- [x] The status line is localised, and the line the user caught was a left-over from the arming design:
      `ArmRemove` still wrote *"remove the local copy of …? press again to confirm"* — a prompt for a
      second press on the same button, which the warning dialog replaced — and `CancelRemove` did not
      touch the note, so it stayed put after the warning was dismissed. It says "confirm the removal in
      the warning" now, and cancelling says "cancelled".
      Beyond that, every note goes through `tr()`: the two dozen messages about downloads, overlays,
      staging, the shepherd registry and the Workshop page are catalogued — **861 source strings**, all
      four primary languages complete. Where a sentence carries values, the catalogue holds the whole
      sentence with `{}` placeholders and a small `fill()` helper substitutes them, because languages
      reorder sentences and assembling them from fragments would have been wrong even where it compiled.
      Still English on purpose: the diagnostics block (its job is pasting), and one error detail raised in
      a worker function that has no `i18n` in reach. Both are noted where they live.
- [x] Two strings the i18n sweep missed, both caught by the user: the removal warning's **body** — a Rust
      string with a `\` line continuation, which no literal-matching pass would find — and the
      **missing-key message**, built in a free function with no catalog in reach. Both are catalogued now;
      **862 source strings**, four languages complete.
      The key one needed a structure change rather than a `tr()` call: "no key" is a **precondition**, not
      an I/O failure, so `goto_remote_page` checks it before starting a task — where `self` and its catalog
      exist — and `fetch_remote` keeps only a technical English fallback. The fallback's test no longer
      asserts the URL, because the URL belongs to the message the window shows.
      The lesson is the same shape as the last few: a sweep that matches literals cannot see strings
      written in another shape. The reliable check is the reverse — grep for `text(`/`button(` calls that do
      **not** contain `tr(` — which is how these two surfaced.
- [x] Selection is carried by colour now instead of a `▸` glyph in front of the text. Three lists had that
      marker — the asset list, an overlay's asset rows, and the Workshop rows — and the comment above one of
      them admitted the reason: *"the accent colour is reserved for the primary action, so selection is
      shown with a marker instead"*. The markers are gone.
      There are two selected styles and the difference is not a preference. **Single-line buttons and the
      tab strip** take the literal inversion — accent background, `on_primary` text, which is what
      `button::filled` already did, so they now share one definition with everything else that is
      "selected". **Two-line rows** take Material's selected *list item* (`secondary_container` and its own
      text colour), because a widget's style colours the widget and not the `text` children inside it: a
      filled row would leave its second line in the on-surface colour, unreadable against the accent. Both
      are contrast-correct in every palette because both halves of each pair come from the same palette
      role — a property a hand-picked inverse colour would not have.
      The overlay rows keep their `●`/`○`: running or not is a *state*, a different axis from selection, and
      a glyph is the right channel for it.
- [x] **Subscribe and Unsubscribe are one button now**, labelled for the way the toggle will go. The two
      buttons were worse than redundant: both opened the same page, neither knew the state, and the remote
      list had only the "Subscribe" half — so the same item was described by different buttons depending on
      which list you found it in. The state comes from `self.snapshot`, which is the Steam client's own
      list, so the label cannot claim something Steam disagrees with.
      Each action also carries a tooltip saying where it happens, because that is the part that was
      invisible: **subscribe** opens the item's page in Steam and the toggle is made there, while
      **download** fetches a copy here with SteamCMD and needs no subscription at all. Two different
      mechanisms, one row, and nothing on screen said so.
      A correction to something claimed earlier in this file: **"paid items will fail anonymously" was
      wrong for this app.** These are workshop *items* — wallpaper-style artwork, not paid add-ons or DLC —
      and SteamCMD's anonymous login fetches any public item. The module doc said so and it was right; a
      later summary of mine did not, and this entry is the retraction.
- [x] **Download is greyed until the Steam client has the item, and a download in flight is visible.**
      Two things were missing and both were the user's to point out: the button was live for items nobody
      had subscribed to, and Steam's own download had no representation at all.
      The state comes from a table that was being **parsed and thrown away**: `WorkshopItemDetails` in
      `appworkshop_<appid>.acf` carries `latest_manifest` — the manifest the client *wants*. Steam writes it
      when it decides to fetch something and only moves `manifest` up to meet it once it is done, so the two
      disagreeing is Steam's own "this one is owed a download", whether it is running now or queued behind
      others. `ItemState::pending()` is that comparison, and it is asserted both ways in the tests
      (a detail table that agrees, or that says nothing at all, must not grey the button).
      Refreshed with the snapshot, so the events the watcher already receives are what make the state move;
      no new timer.
      Caught while writing it: the remote pane's condition went in **inverted** — live when *not*
      subscribed — which the tooltip ("subscribe in Steam first") made obvious the moment I read the two
      lines together.
- [x] **Decided the other way round: the button is gone and the client owns the download.** Keeping a second
      mechanism that merely produced the same files was the option I leaned toward and the wrong one — the
      cell reports what Steam is doing and nothing is pressable. `Message::Download` and its SteamCMD thread
      were removed, and the compiler said they had become unreachable before I did (see the three-state entry
      above). This window's job is to show the download, not to be a second way to perform it.
- [x] The download cell has three states and exists in two of them. **Not subscribed: nothing** — no
      button to grey, because there is nothing to download and the subscribe button is the whole story.
      **Subscribed and Steam still working: "downloads…", disabled.** **Done: "downloaded", disabled.**
      Nothing in that cell is pressable any more, and that is the honest end of it: the download belongs to
      the client, and the second copy our own mechanism produced was the lie sitting in the middle of the row.
- [x] **The window watches Steam work by itself.** A one-second tick re-reads the `.acf` and the row changes
      with no manual refresh. The tick lives in the subscription list *outside* the `animating` branch — a
      download is not an animation, and the `animating` branch is where it went first — and the handler
      returns before reading anything unless the client actually owes an item a download, so an idle window
      does no work at all.
      `Message::Download` and its SteamCMD thread are **gone**: with nothing able to press it, it was
      unreachable, and the compiler said so before I did — *variant `Download` is never constructed*.
- [x] A note on the tooling, because it cost time twice: the two action rows could not be found by shape
      (fmt had reflowed them, and the same `let subscribed` line appears in both), so the fix was to print
      the region and cut it by line number. One attempt in between wrote **nothing at all** because it
      crashed before the write — the safe failure — which is exactly why those scripts print what they are
      about to cut.
- [x] The waiting row shows **how much has landed**, measured from the files themselves. The `.acf` keeps
      a *final* size and writes it only when the download finishes, so a percentage computed from it would
      be a guess dressed as a measurement — but bytes on disk grow as Steam works, and a number that moves is
      a progress readout that cannot lie. The tooltip says what the number is, because "12.4 MB" with no
      total is only honest if it says so.
      Measured only for the items Steam still owes: walking a finished item's directory every second would be
      work for nothing, and it happens on the same one-second tick that re-reads the `.acf`. A directory that
      does not exist yet counts as zero bytes, which is the normal case for the first seconds of a download
      and is asserted in the test.
- [ ] A percentage would need a total, and while Steam is downloading there is nowhere honest to get one:
      the `.acf` has no size until it is done, and the keyless `GetPublishedFileDetails` response is the
      obvious candidate to check for a `file_size` field if a bar is wanted later.
- [x] **Two bugs that made the polling invisible, both mine, both found by the user pressing the button.**
      The first was a self-lock: the tick refused to read the `.acf` unless something was *already* known to
      be pending, so the single observation that could have told it "yes, there is one" was the observation
      it refused to make. An optimization that made the only bootstrap path impossible. It reads every tick
      now — one small file twice a second, and there was never a cost worth guarding.
      The second was a silent path guess. `libraryfolders.vdf` lists either `…/steamapps` (older files) or
      the root above it (newer ones), and `acf_path`/`content_root` assumed the root. When the guess was
      wrong the `.acf` was simply not found, the state was cleared, and an item Steam was actively
      downloading showed nothing at all — no error anywhere. `steamapps_of` accepts both spellings and both
      are asserted in the tests.
      The interval came down to 500 ms with them: a download can land in well under a second, and the point
      is to catch the state while it is true, not to be frugal with a file this size.
- [x] **The floor, and the growth signal — both the user's calls, after mine failed twice.** The manifest
      trick was too far up the chain: it depends on Steam having written metadata, and it left the row
      invisible whenever Steam had not. Two things replace the dependence on it.
      First, **the directories are the measurement.** Every item's tree is walked on the half-second tick, and
      a directory that got bigger since the last look *is* a download happening: no `.acf`, no manifest, no
      Steam cooperation required. It stays on screen for about a second and a half after it stops growing,
      which is the difference between "I never saw it" and "I saw it finish".
      Second, **a floor.** A full rescan every two seconds regardless of what the watcher and the fast tick
      managed — and the `Refresh` handler that already existed re-reads the list *and* asks for titles, so
      this is one subscription line and everything the earlier paths could miss is caught anyway.
      The cost is a few hundred `stat` calls twice a second over small trees. That is cheaper than the bug it
      replaces, and it is the user's trade to make, not mine: I twice chose the cleverer mechanism and twice
      it was the one that did not work.
- [x] **The Tools tab exists as a skeleton, and says what it is.** Three cards — MP4 to animation,
      background removal by colour, background removal by AI — each with one sentence about what it would
      do and which formats it takes, and each marked **not wired yet**. No buttons, because a button that
      does nothing is worse than a sentence that says so.
      What *is* real is the environment: `probe_commands()` runs `ffmpeg`, `rembg` and `steamcmd` once at
      startup — spawning is the test, and a non-zero exit still counts, because `-h` is not universal — and
      each card reports found or not-on-PATH for the command its pipeline needs. So the page already answers
      the question a user actually has: *will this work on my machine?*
      Deliberately absent: file pickers and format dropdowns. They need a native-dialog dependency and they
      would be affordances for a pipeline that cannot run yet — the same trap the overlays tab fell into
      with its ±10% button pair.
- [x] **The colour-key removal is wired end to end, as a preview.** On the Tools tab: paste a path, Load
      (frame 0 through `anim::first_frame`), a background swatch or **"use the corner pixel"**, and two
      sliders — tolerance and feather. Both previews are rebuilt when a control changes, not inside `view`:
      the pass is cheap for one scaled frame, but a render loop that runs image processing is a habit worth
      not starting.
      **"Use the corner pixel" is the control the original never had** — it read the background from a
      manual colour picker — and white stays the default because the original starts there.
      The previews sit on `surface_container_highest` for a reason that is not decoration: iced has no
      transparency backdrop, so a keyed frame landing on the window's own surface looks like nothing
      happened, and the container is what makes transparent parts read as transparent.
      **Widgets: nothing new was needed.** Sliders, text input, image, pick_list, button and container all
      appear elsewhere in this window already. The two things that *would* be new if wanted: `progress_bar`
      (unused so far) and the `rfd` crate for a native file dialog — the single gap iced leaves for a tool
      that has to name a file.
      Not done, and the card says so: **writing the file**. Encoding gif/apng/webp needs an encoder the
      `image` crate does not fully provide, so that is the next step.
- [x] **A real file picker, and swatches that are just colour.** `rfd` with the `xdg-portal` backend —
      deliberately not the default one, which links GTK3 for a dialog — and the answer comes back through
      the task idiom like every other result in this window. A chosen path **loads** immediately; a typed
      one does not, because somebody typing is probably still typing, which is why they are two messages
      and not one.
      The swatches lost their labels: a colour is its own explanation, so each is now a 24px circle with the
      name in a tooltip, and the selected one is ringed rather than captioned.
      Worth knowing for a failure: the portal is a desktop service, so on a session without one the dialog
      will not appear. The typed path field stays for exactly that case, and this line is the reason it is
      not redundant.
- [ ] Scale above 100% stays impossible, and that is a design decision rather than an oversight: the
      overlay only shrinks, so it never resamples. Raising `MAX_SCALE_PERCENT` is a one-line change, but it
      turns scaling into upscaling, and that is the part worth thinking about first.
- [x] The rows' per-instance actions were exercised against a **running** overlay from the new tab, and the
      user confirmed the tab as a whole: the ordering puts running ones first, the non-running cards have no
      controls, the 1% slider and the typed value both apply, and the buttons sit on one row. The wiring was
      new but the messages and the file writes were the panel's, which is why it held.
- [ ] The remaining honest gaps in the Material work: type scale and spacing are iced's, and the
      palette is read once at boot (a wallpaper change needs a relaunch; watching the file is the
      follow-up).
      For reference, the facts behind those choices (`iced_material` 0.14.2):
      - it implements iced's `Catalog` for `Theme` on `button`, `container`, `pick_list`,
        `scrollable` and `text` — so those keep their default styles and the "every text call
        needs a style" worry is unfounded; `text_input` has **no** `Catalog` (only
        `text_editor`), so the search box needs one explicit style;
      - its button styles are `elevated`, `filled`, `filled_tonal`, `outlined`, `text` — there is
        no `danger`, so the two destructive buttons need one ~10-line style built from
        `theme.colors().error`;
      - its `Theme` wants its own `ColorScheme` (each role carries colour/text/container), **not**
        an `iced::Palette`, so the noctalia parser has to be retargeted to build a `ColorScheme`
        before the type can switch. That is the first move, and it is the part that is easy to
        underestimate.
      Order: (1) parse noctalia into `ColorScheme`; (2) `type El<'a> = Element<'a, Message,
      iced_material::Theme>` through the three view signatures and `divider`; (3) map our
      `primary/secondary/danger/text_button` onto `filled/outlined/<own danger>/text`; (4) a
      `text_input` style; (5) build green, then — as the second commit — swap in the
      `iced_material` widgets for real MD3 shapes and elevation.
- [ ] Not needed unless asked: a D-Bus "start/stop this animation" call — the registry file is
      already the interface the window uses, and the tray covers the human side.

### Steam / Workshop

- [ ] Provide a Linux `libsteam_api.so` and wire `SteamWorkshop` into the app
  (this install only ships Windows `steam_api64.dll`)
- [ ] Subscribe / repair / unsubscribe actions in the UI (downloads already work
      through `anima-workshop download` / SteamCMD)
- [x] Hot reload: the overlay watches the displayed animation (its size/mtime every
      second) and the Workshop content (`WorkshopWatcher`, every fifth second).
      Everything that depends on the animation's geometry — texture, view, bind group,
      layer count, UV scale — lives in a `Frames` value built by a `build_frames`
      closure, so a reload that changes the size, frame count or storage path is
      rebuilt **in place** too: no restart, no surface gap, no flicker, no state
      restore. Verified mid-run with **the same process id**: `touch` →
      `rebuilt frames: 27 frame(s) 136x132`; swap in `miku12.apng` →
      `decoded 64 frame(s) at 1500x1125` then `rebuilt frames: 64 frame(s) 1266x1078`;
      swap back → `rebuilt frames: 27 frame(s) 136x132`. The `exec` fallback is gone.
- [ ] Steamworks callbacks (`ItemInstalled` / `DownloadItemResult`) still need
      `libsteam_api.so`. The **fallback is already in place**: `WorkshopWatcher` plus
      the overlay's hot reload cover the "new or updated item becomes the animation"
      case without the
      SDK. When the `.so` is available, `steam::FlatSteamApi::run_callbacks` can drive
      the same diff, and `diff` reports **updated** items too (same id, new files)
- [ ] Upload via SteamCMD (`workshop_build_item`) and/or the Steamworks worker,
  incl. legal-agreement handling
- [ ] Browse UI using the three-tier fallback documented in the RE notes
- [x] Linux Steam library discovery from `~/.steam` roots
      (`steam_roots` / `libraries_from_roots` / `discover_libraries`, used by
      `anima-workshop`)

### Robustness / distribution

- [x] Large animations are no longer decoded fully into RAM: a bounded decode
      decimates (halving the retained frames and doubling the stride until the set
      fits `DecodeLimits`, folding dropped delays into the survivor so the total
      playing time is preserved). `ANIMA_MAX_DECODE_MB` (default 1024) and
      `ANIMA_MAX_FRAMES` (default 4096) tune it; a decimated decode is **not**
      cached, so the cache always holds a complete decode
- [x] Cache stores the raw frames in one zstd stream (level 3) instead of a
      PNG-per-frame ZIP (entry format version 3, `b"ANIMCACH"` magic). Measured on
      `miku12.apng` (64 frames, 1500x1125, 412 MiB raw): zstd write 0.63 s / read
      0.23 s at 46.3 MiB versus PNG write 1.66 s / read 0.57 s at 43.5 MiB — about
      2.5x faster for 6% more bytes (`examples/cache_bench.rs`); PNG encoding was
      the reason a big animation took minutes to cache
- [x] BC7 block compression (quality first) through the vendored `bc7enc_rdo`,
      behind the optional `bc7` feature (needs a C++ toolchain). At uber level 4:
      `bochi455.apng` (512², 27 frames) 6.8 MiB, **71.6 dB colour / 69.2 dB alpha**
      PSNR, max alpha error 4; `miku12.apng` (1500x1125, 64 frames) 103.3 MiB
      (25.1% of raw), 24 s (2.7 fps), **45.8 dB colour / 44.5 dB alpha**, max alpha
      error 133. Note BC7 (8 bpp) is ~2.2x **larger** than the zstd cache (46 MiB
      for miku12) — its win is VRAM and direct upload, not disk
- [x] BC7 wired into the renderer: with `bc7` blocks cached, the overlay builds a
      `Bc7RgbaUnormSrgb` array texture (matching the `Rgba8UnormSrgb` frame texture
      and the sRGB surface, so sampling is identical), block-aligned to whole 4x4
      blocks with the UVs scaled by `width/tex_width` so the padding never shows.
      Without blocks it keeps the RGBA array/JIT paths and encodes a sidecar in the
      background for the next run. Verified: pass 1 logged `rgba array texture` and
      wrote a 6.75 MiB sidecar for bochi455; pass 2 logged
      `cache hit: 27 frame(s), 512x512 + bc7 blocks` then
      `6.8 MiB bc7, bc7 array texture`
- [x] Storage policy: with `bc7` enabled the animation is **stored as BC7 blocks
      only** (no zstd frames entry is written), and loading rebuilds the RGBA that
      hit-testing needs by decoding the blocks — so the CPU alpha matches what the
      GPU shows. Without `bc7` the zstd frames cache is used as before. The sidecar
      carries the per-frame delays (block format version 2) so playback timing
      survives
- [x] Transparent padding no longer pins the overlay: `InkBox::scan` finds the
      union of the visible pixels and `Animation::crop` cuts every frame to it, so
      the bounds clamp is tight (the art can reach a screen edge) and the texture,
      the input-region scan and GPU memory all shrink. Saved x/y stay canvas-space
      and are translated by the crop offset; the BC7 sidecar records the crop
      (block format version 3) because without it the position drifted on every
      start
- [x] i18n catalog as a data file + all-locale verification: the catalog is
      installed to `<data>/AnimaEngine/anima_translations.json` (first candidate
      after `ANIMAENGINE_TRANSLATIONS`), `anima-i18n install/status/candidates`
      manage it, and `anima-i18n verify` reports per language: entries, missing
      source strings, blank values and untranslated entries (exit 1 when a
      language is incomplete). Measured on the shipped catalog: 795 source strings
      over 31 languages, and **no language is complete** — 2 missing for
      `ar`/`bg`/`zh-CN`, 19 for `cs`/`da`/`de`, 1 blank value and 39-124
      untranslated entries each
- [x] Primary languages: the port ships and verifies **en, zh-CN, zh-TW, ja**
      first (`i18n::PRIMARY_LANGUAGES`). `verify` fails only while a primary
      language is incomplete (`--all` for every language) and `--missing` lists the
      untranslated source strings for a translator; `install --primary`
      (or `--languages=a,b`) trims the catalog on the way in
- [x] Portable install: `packaging/install.sh` puts the four binaries, both
      desktop entries, the icon and the AppStream metadata under `$PREFIX`
      (default `~/.local`); `--catalog FILE` installs the primary-language
      translations, `--niri` writes an autostart snippet (it does **not** edit the
      user's niri config — it prints the `include` line), `--uninstall` removes
      everything, and `desktop-file-validate` + `appstreamcli validate` run
      automatically (both pass). Verified in a scratch prefix and for real
- [x] i18n completion: language-neutral keys are no longer counted
      (`is_untranslatable`: empty, no letters, or a lowercase identifier — the
      arrows, `25%`/`50%`/`75%` and ids like `process_gif`), and the source
      language (`en`) never counts as missing because the key *is* its text. With
      those rules `zh-CN`/`zh-TW` are complete; `ja` was missing four real strings,
      which `packaging/i18n-additions.json` supplies and `anima-i18n merge` applies.
      After the merge all four primary languages verify clean (exit 0)
- [x] Flatpak: built and installed user-level (`org.flatpak.Builder` +
      `org.freedesktop.Sdk//26.08`, no sudo needed once a user `flathub` remote
      exists). `packaging/flatpak/stage.sh` stages the four binaries plus the
      desktop assets (19 MB) so the multi-gigabyte `target/` never enters the build
      sandbox, and the manifest installs that staging tree. The sandboxed CLI works
      (`flatpak run --command=anima-overlay-ctl … status` reads the sandbox control
      file).
- [ ] **The overlay does not start inside the Flatpak here.** `ANIMA_DUMP_GLOBALS=1`
      showed the cause: the sandbox was offered a **Mutter/GNOME** registry
      (`mutter_x11_interop`, `ext_background_effect_manager_v1`, `xdg_wm_base v7`,
      no `zwlr_*` at all) while the host registry has `zwlr_layer_shell_v1 v5` — so
      the sandboxed app connected to a different compositor's socket, not niri's.
      That is why `zwlr_layer_shell_v1` was missing. Evidence gathered: `ss -xlp`
      shows niri (pid 1153) listening on `/run/user/1000/wayland-1`, the socket
      classifies as niri/wlroots with `zwlr_layer_shell_v1 v5`, and
      `flatpak-session-helper` itself has `WAYLAND_DISPLAY=wayland-1` and
      `XDG_RUNTIME_DIR=/run/user/1000` — so the *proxy path* is where the mismatch
      happens, not our environment. Two bypasses failed deterministically with
      `bwrap: Can't mount on symlink destination` (mounting the socket path, and an
      aliased `$XDG_RUNTIME_DIR` socket). Next: trace what target
      `flatpak-session-helper` relays to (and whether a GNOME session owns the
      portal/proxy). The overlay stays on the portable install until then

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
cargo test -p anima-engine-rs                     # pure logic (193 tests)
cargo test -p anima-ai                            # the AI removal (13 tests)
cargo test -p anima-engine-rs --features "workshop-web,steamworks"  # + Steam (196)
cargo test -p anima-engine-rs --features bc7      # + BC7 encode/decode (198)
cargo run -p anima-engine-rs --release --features bc7 --example bc7_bench -- <animation>
cargo run -p anima-engine-rs --bin anima-i18n -- verify <catalog.json>  # per-language health
cargo run -p anima-engine-rs --bin anima-workshop -- watch --once  # installed items (polling)
bash packaging/install.sh --catalog <catalog.json>    # portable user-local install
curl -fsSL <url> | sh -s -- --repo <git-url>          # build + install one-liner
ANIMA_DUMP_GLOBALS=1 anima-overlay <anim>             # print the compositor globals
cargo run --release --example cache_bench -- <animation>   # zstd vs PNG cache

cargo clippy --all-targets --features "overlay,workshop-web,steamworks"

cargo run --example scan_workshop -- ~/.local/share/Steam/steamapps/workshop/content/3474900

cargo run --bin anima-workshop -- list
cargo run --bin anima-workshop -- download 3828587753   # needs steamcmd
cargo run --bin anima-workshop -- sync
cargo run --bin anima-workshop --features workshop-web -- web 3828587753

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
  frame-count region throttling/coarsening (+ optional auto step and configurable,
  runtime-tunable rectangle budgets) → in-surface right-click menu →
  Steam root/library discovery + `anima-workshop` CLI → SteamCMD download path →
  SteamCmd as a `WorkshopSource` (sync) → runtime rotate/mirror (ctl + tray +
  menu) → `anima-workshop play` (download → sync → overlay) → workshop upload
  (`workshop.vdf` + `workshop_build_item`) → bounded decode + frame-timing
  fidelity → zstd frame cache (PNG dropped) → vendored bc7enc_rdo BC7 encoder and
  renderer wiring (`bc7` feature: blocks are the cache when enabled).
