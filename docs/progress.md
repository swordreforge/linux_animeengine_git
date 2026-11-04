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
- `examples/scan_workshop.rs` — scan a real workshop content dir and print what
  the app would see.
- `anima-workshop` (bin, no extra features): `roots`, `libraries`, `list`,
  `show <id>`, `download <id...>` (SteamCMD), `play <id> [asset]` (install if
  needed, then run it in `anima-overlay`), `upload <dir> [--title T] [--id N]
  [--user U] [--dry-run]` (writes `workshop.vdf` + `workshop_build_item`),
  `sync`; `web <id...>` with `--features workshop-web`.

## Verification

- `cargo test`: **142** default (139 lib + 3 CLI); **147** with `bc7` (the `bc7`
  feature needs a C++ toolchain, as does the `overlay,bc7` build).
- `cargo clippy --all-targets`: clean for default and all features.
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
- [ ] Not yet: CJK glyphs confirmed on screen (needs a look at the picker), Workshop
      thumbnails, subscribe/repair/unsubscribe, driving the overlay through
      `ControlState`, a search field with IME, and a desktop entry for the window.

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
cargo test                                        # pure logic (129 tests)
cargo test --features "workshop-web,steamworks"   # + Steam adapters (132)
cargo test --features bc7                         # + BC7 encode/decode (133)
cargo run --release --features bc7 --example bc7_bench -- <animation>
cargo run --bin anima-i18n -- verify <catalog.json>   # per-language health
cargo run --bin anima-workshop -- watch --once        # installed items (polling fallback)
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
