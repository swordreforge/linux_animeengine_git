# Controlling the overlay

`anima-overlay` is a bare, frameless, always-on-top surface, so it is driven from
outside through small JSON files (`<XDG data>/AnimaEngine/overlay_control/*.json`,
one per instance, see `control::ControlState`). Three front-ends write them: the
SNI tray, the `xdg-desktop-portal` global shortcuts, and the
`anima-overlay-ctl` CLI.

## Input modes

| Mode | Behaviour |
|---|---|
| `opaque` (default) | Only solid pixels of the current frame grab clicks; transparent areas pass through to what is below |
| `pass-through` | The whole surface is click-through |
| `full` | The whole surface grabs clicks |

Position and size persist to `secondary_instances.json` (keyed by the animation's
canonical path), together with `click_through` and `locked`. Other instance
fields (`fps`, `rotation`, `flip_h`/`flip_v`, `always_on_top`, `hover_hide`) are
read from that file at startup.

## Input-region tuning (optional)

The per-pixel input region is rebuilt at most every 3rd presented frame and only
when the silhouette changes. Very detailed silhouettes are coarsened above a hard
rectangle cap. The environment seeds the startup values; they can be changed at
runtime with `anima-overlay-ctl region ...` or the tray's "Region" menu. The
effective values are logged as `region limits: ...`.

| Variable | Default | Behaviour |
|---|---|---|
| `ANIMA_REGION_STEP` | `off` | `off`, `auto` (powers of two until the soft budget, bounded by display size), or a fixed `1`–`32` grid |
| `ANIMA_REGION_SOFT_RECTS` | `768` | target for `auto`; clamped to `16..=65536` and never above the hard cap |
| `ANIMA_REGION_MAX_RECTS` | `4096` | hard cap: coarsen, then bounding box; clamped to `16..=1000000` |

## Multiple instances

Run one `anima-overlay <path>` process per animation. Each instance gets its own
control file (`overlay_control/<slug>.json`, slug = SHA-256 of the canonical
animation path) and its own tray item (title = file name).

`anima-overlay-ctl` applies to **all** running instances by default; pass
`--instance <animation path>` to target one. `status` prints one block per
instance. The tray menu acts only on its own instance, and the portal shortcut
handler broadcasts (each instance registers per-instance shortcut ids).

## Size

Display size is a percentage of the frame's native size, capped at **100%** —
only shrinking is allowed, so the frame is never upscaled and never resampled:
the GPU samples the texture down (linear minification). The per-pixel input
region follows the scaled size automatically.

## CLI

```bash
cargo build --release --bin anima-overlay-ctl   # no extra features needed
# or, to put it on PATH:
cargo install --path . --bin anima-overlay-ctl

anima-overlay-ctl toggle             # opaque pixels <-> click-through
anima-overlay-ctl mode opaque
anima-overlay-ctl mode pass-through
anima-overlay-ctl scale 50           # shrink to 50% (never above 100%)
anima-overlay-ctl scale up            # +10%
anima-overlay-ctl scale down          # -10%
anima-overlay-ctl lock                # disable dragging (persisted)
anima-overlay-ctl unlock
anima-overlay-ctl hide                # hide (surface stays alive at 1x1)
anima-overlay-ctl show
anima-overlay-ctl toggle-hidden
anima-overlay-ctl region step auto   # off | auto | 1..32
anima-overlay-ctl region soft 512    # auto's target (16..=65536)
anima-overlay-ctl region max 2048    # hard cap (16..=1000000)
anima-overlay-ctl reset              # initial position + native size + opaque input
anima-overlay-ctl quit
anima-overlay-ctl status
```

## niri keybindings

niri has no portal backend implementing `GlobalShortcuts`, so bind the CLI. Put
the binds in your own file (e.g. `~/.config/niri/__custom__.kdl`, which niri
includes last, so your binds win) and use an **absolute path** — niri resolves
`spawn` against its own `PATH`, which usually does not include `~/.local/bin`.
niri watches the config and hot-reloads on save.

```kdl
binds {
    Mod+Alt+O       { spawn "/home/you/.local/bin/anima-overlay-ctl" "toggle"; }
    Mod+Alt+Equal   { spawn "/home/you/.local/bin/anima-overlay-ctl" "scale" "up"; }
    Mod+Alt+Minus   { spawn "/home/you/.local/bin/anima-overlay-ctl" "scale" "down"; }
    Mod+Alt+H       { spawn "/home/you/.local/bin/anima-overlay-ctl" "toggle-hidden"; }
    Mod+Alt+R       { spawn "/home/you/.local/bin/anima-overlay-ctl" "reset"; }
    Mod+Alt+Shift+Q { spawn "/home/you/.local/bin/anima-overlay-ctl" "quit"; }
}
```

The `Mod+Alt` tier is chosen deliberately: a typical niri keymap already binds
`Mod+Shift+T` / `R` / `Q` and `Mod+Shift+Equal` (floating/tiling toggle, config
reload, compositor quit, reset-window-height). niri can only override a bind, not
unbind it, so reusing those would silently break those functions.

## Show / hide

Hiding does **not** unmap the surface: the layer surface is resized to **1x1**,
which keeps it mapped so the render loop, tray and control channel stay alive
(an unmapped Wayland surface cannot be brought back). Showing restores the
scaled size. This is the live2d-rs pattern; a few compositors dislike tiny
surfaces (live2d-rs falls back to 50x50 / `set_minimized` on GNOME/KDE), but
niri is fine with 1x1.

## Right-click menu

Right-clicking a solid pixel opens a small icon column *inside* the overlay
surface (drawn with wgpu, no popup). Items, top to bottom: cycle input mode (the
dots show the current mode), scale down, scale up, lock/unlock, reset, quit.
Left-click picks an item; clicking elsewhere, right-clicking again, or leaving the
surface dismisses it. While it is open the input region covers the whole surface
(so the panel receives clicks over transparent pixels), and it is unreachable in
`pass-through`/hidden — use the tray or `anima-overlay-ctl` there.

## Tray

The overlay registers a `StatusNotifierItem`. niri has no tray host by default —
run one (e.g. `waybar` with the `tray` module) to see the icon. The menu has
"Input", "Size" and "Region" submenus, "Visible" and "Lock position" checkmarks,
plus "Reset position and interaction" and "Quit overlay".

## Portal global shortcuts

Where a portal backend implements `org.freedesktop.portal.GlobalShortcuts` (e.g.
KDE), the overlay also registers `Ctrl+Shift+T` (toggle click-through) and
`Ctrl+Shift+R` (reset). This machine's backend does not, hence the niri
keybindings above.
