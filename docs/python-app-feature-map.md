# The Python app's features, and which ones still need porting

Written after reading the shipped sources, the recovered main-window bytecode, and the translation catalog
(793 entries × 31 languages). The companion note is [`python-main-window-recovery.md`](./python-main-window-recovery.md),
which explains how the bytecode was read and where it stops.

## The shape of the thing

`AnimaEngineKIVI.exe` is a **hub**, not an app: a library page, an apps page, a tools page, a workshop
browser with four sub-pages, a discover grid and four "side" action pages — and it launches
`AnimaProfiles.exe` and `AnimaTriggers` as siblings, with labels for failing to find them.

## Already in the Rust window

| Feature | Where it lives in Python |
|---|---|
| Overlay playback, per-instance parameters (scale/rotate/flip), control files | `monoModule.TransparentGifWindow` |
| Workshop browse, installed list, metadata titles, subscribe (hands off to Steam), remove local copy | main window + `anima_foundation` |
| Translation catalog and switching | `anima_i18n_static` (31 languages) |
| Settings, theme, palette, window state | `studio.json`, noctalia palette |

## To port, in the order I would do it

1. **Trigger engine** — `AnimaTriggers/_internal/`: screen capture (`mss`/`dxcam`, DPI-aware), matchers
   (ORB + aHash + `FastSIFTMatcher` with FLANN/RANSAC), `EventRunner` start/stop/cooldown logic, the 5-step
   wizard, and the modern `.mdaa` format (`pack.json`, `assets/`, `frames/regionN.npz`, `meta.json`). This
   is the only feature in the whole app that nothing else does, and the hardest to port correctly.
2. **Tools page** — an MP4 converter (three real pipelines: `moviepy` for GIF, `ffmpeg` for WebP/APNG, and a
   zip-of-images path), a colour-key remover, and an AI remover (`rembg` with alpha matting). The two
   worked examples in the recovery note have the exact ffmpeg argument lists and the two things to fix
   while porting: a real distance instead of the per-channel box, and a frame rate that is not a literal.
3. **Sprite and sequence system** — `SpriteSecuenciaWindow` plus `sprites.json`, `sprite_config.json`,
   `spritesSecuencia.json`: sprite sheets, named sequences, and the four "load moving/static/grabbed/released"
   entry points the tools page offers.
4. **Audio runtime** — `anima_audio` (`animation_audio.json`: per-animation path + loop flag) and
   `anima_audio_runtime` (`AudioRuntimePlayer`, with its own log and diagnostics). Both exist only inside the
   archive, and the current window has no equivalent.
5. **Overlay parity from `BeastConfig`** — the unwired GPU engine specifies more than the shipped overlay:
   per-pixel click-through via a separate alpha mask, `smart_dodge`/`snap_to_edge`/`focus_fade` hover
   behaviours, span-all-monitors, a topmost watchdog, and a decode buffer with a chosen resample filter.
   `scale_to_fit` defaults off, which is the same conclusion the Rust overlay reached as
   `MAX_SCALE_PERCENT = 100`.
6. **Workshop upload** — the three-step publisher, with "update an existing PublishedFileId without
   creating a duplicate" as its stated behaviour. The largest single workshop gap.
7. **OBS bridge** — `anima_obs_bridge` (33 KB, the largest helper, and it is **not** in the loose sources):
   an `AnimaOBSBridge` speaking `AnimaEngineOBS/2.0` over a socket, with `configure`,
   `_frame_bytes`, `_intersects` and `_desktop_hidden`. Either the overlay serves frames to OBS, or OBS
   tells the overlay when the desktop is obscured. Worth reading before deciding.
8. **GIF pipeline and cache** — `anima_gif_pipeline` (19 KB, also archive-only): frame compositing, alpha
   blending, `cache_metadata`, `cache_path` — the thing behind the app's `frame_cache_v2/*.animacache`.
9. **Profile export/import** — `MInteraDesktopExport.zip`, the legacy profile format, with autostart and
   monitor choice.

## Not portable, and not worth pretending otherwise

* **`SendVideoRequestWindow`** sends through Firebase Firestore using the bundled Admin SDK, so it
  authenticates as the author's project and nobody else can reproduce it. A port needs its own endpoint,
  or an honest replacement, or a deliberate deletion — not a copy.
* **Keyboard/mouse/gamepad binding** (`deteccionDeTeclas.py`) — a Rust window has its own way of doing this;
  the file's `eval`-based binding is not a specification worth following.
* **The old Tk `.mdaa` manager** — superseded by AnimaTriggers, and its format is a plugin bundle that
  executes what it carries. Only its "restore the last bundle at startup" behaviour is worth keeping.

## Facts worth keeping from the survey

* The catalog is **31 languages**, so our four are a choice rather than a limit; `auto_Trad.py` shows the
  same "wrap the literals" pain this project hit by hand.
* Animation timing should be **inherited** (`loop`, `duration`, `disposal=2` for GIF) — one tool does this
  right and two do not, and the difference is visible in the output.
* Three separate tools hard-code `-framerate 60`. Any port that exposes it will be better than all three.
