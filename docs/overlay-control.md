# Controlling the overlay

`anima-overlay` is a bare, frameless, always-on-top surface, so it is driven from
outside through a small JSON file (`<XDG data>/AnimaEngine/overlay_control.json`,
see `control::ControlState`). Three front-ends write it: the SNI tray, the
`xdg-desktop-portal` global shortcuts, and the `anima-overlay-ctl` CLI.

## Input modes

| Mode | Behaviour |
|---|---|
| `opaque` (default) | Only solid pixels of the current frame grab clicks; transparent areas pass through to what is below |
| `pass-through` | The whole surface is click-through |
| `full` | The whole surface grabs clicks |

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
    Mod+Alt+R       { spawn "/home/you/.local/bin/anima-overlay-ctl" "reset"; }
    Mod+Alt+Shift+Q { spawn "/home/you/.local/bin/anima-overlay-ctl" "quit"; }
}
```

The `Mod+Alt` tier is chosen deliberately: a typical niri keymap already binds
`Mod+Shift+T` / `R` / `Q` and `Mod+Shift+Equal` (floating/tiling toggle, config
reload, compositor quit, reset-window-height). niri can only override a bind, not
unbind it, so reusing those would silently break those functions.

## Tray

The overlay registers a `StatusNotifierItem`. niri has no tray host by default —
run one (e.g. `waybar` with the `tray` module) to see the icon. The menu has an
"Input" submenu (full / opaque / click-through) plus "Reset position and
interaction" and "Quit overlay".

## Portal global shortcuts

Where a portal backend implements `org.freedesktop.portal.GlobalShortcuts` (e.g.
KDE), the overlay also registers `Ctrl+Shift+T` (toggle click-through) and
`Ctrl+Shift+R` (reset). This machine's backend does not, hence the niri
keybindings above.
