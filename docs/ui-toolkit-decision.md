# Main window toolkit — decision record

**Decision: Iced 0.14** for the main window (`anima-studio`). The overlay keeps its
hand-rolled `wgpu` + `smithay-client-toolkit` stack and stays a separate process.

## Constraints that shaped it

1. **No text stack exists in this tree.** The overlay's `mod menu` draws coloured
   quads only (`MenuVertex` = position + colour, no UV), so a real window — text,
   inputs, scrolling, IME, CJK — needs a toolkit or a hand-written shaping/atlas
   layer. Hand-rolling was ruled out.
2. **Distribution**: Steam (Linux) plus Flatpak; Windows is a maybe. That rules out
   Linux-only toolkits (GTK4) as the primary answer and makes heavy second runtimes
   (webkit2gtk for Tauri) unattractive in a sandbox.
3. **The overlay is the always-on process** launched by Steam. It must stay slim, so
   the window is a separate binary (`anima-studio`, `studio` feature) talking to the
   overlay through the existing `ControlState` control file, never through shared GPU
   state.
4. **i18n is already solved as data**: `anima_translations.json` with four primary
   languages (en, zh-CN, zh-TW, ja) and `I18n::tr()`. A plain Rust API consumes it
   directly; a DSL or a webview would need its own bridge.
5. **CJK needs glyph coverage**; whichever toolkit is chosen must either bundle a font
   or fall back to system fonts.

## Measured facts

| | |
| --- | --- |
| iced latest | `0.14.0`, default features already include `wgpu`, `wayland`, `x11`, `thread-pool` |
| iced GPU stack | `wgpu 27.0.1` (ours is `29.0.4`) → **two wgpu versions in the lockfile** |
| iced text | `cosmic-text 0.15` (shaping + system-font fallback) |
| build | clean first time apart from three API details: `horizontal_space()` → `space().width()`, `button(String)` → `button(text(..))` |
| release size | `anima-studio` **26 MB**, `anima-overlay` unchanged at **16 MB**; release build ≈ 1 min |
| runtime check | window launched on niri/Wayland, stayed up, no stderr; boot printed `2 root(s) · 1 item(s) · catalog …/anima_translations.json · 793 entries` |

Two `wgpu` versions is acceptable because they never meet: separate processes, separate
devices. The cost is build time and a larger tree, not correctness. If window and
overlay ever needed to share textures or a device, that would force a toolkit on the
same `wgpu` version (or hand-rolling) — worth remembering before designing any shared
render path.

## Why Iced over the alternatives

- **vs egui**: egui is faster to write and can render into an existing `wgpu` pass,
  but its look is tool-like, it redraws continuously, and its text layout is weaker for
  mixed CJK/Latin. Iced's `cosmic-text` and retained "app" structure fit a browser +
  settings window better.
- **vs GPUI**: GPUI (Zed) is faster and prettier, but pre-1.0, git-pinned, churning,
  with no accessibility story and Linux historically through X11/XWayland. The daily
  cost is not worth it for lists and buttons.
- **vs GTK4**: best Linux integration (portals, a11y, gettext, Flatpak) but Linux-only,
  which forfeits a Windows build.
- **vs Slint**: strong declarative toolkit with built-in translation, but GPLv3-or-paid
  licensing is a real constraint for a Steam-distributed app.
- **vs Tauri/webview**: would reuse web UI skills and the JSON catalog, but brings
  webkit2gtk and its sandbox/GPU problems into a project that already hit a
  compositor-mismatch issue under Flatpak.

## Consequences / open items

- `Cargo.toml` gains `studio = ["dep:iced"]`; nothing existing is affected.
- The window boots the catalog and the workshop snapshot headlessly (see the test) and
  the window itself runs on niri/Wayland with no errors.
- Still to do: verify CJK glyphs on screen (the language picker's native names are the
  natural check) and decide whether to bundle a font; workshop thumbnails (Steam
  preview images) via the `image` feature; subscribe/repair/unsubscribe actions
  through `anima-workshop`; wiring the window's controls to `ControlState` so it drives
  the overlay; keyboard/IME for the search field; and a window icon + desktop entry
  (the packaging step already ships a `anima-overlay` entry, not yet a studio one).
