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

## CLI

```bash
cargo build --release --bin anima-overlay-ctl   # no extra features needed
# or, to put it on PATH:
cargo install --path . --bin anima-overlay-ctl

anima-overlay-ctl toggle             # opaque pixels <-> click-through
anima-overlay-ctl mode opaque
anima-overlay-ctl mode pass-through
anima-overlay-ctl reset              # initial position + opaque input
anima-overlay-ctl quit
anima-overlay-ctl status
```

## niri keybindings

niri has no portal backend implementing `GlobalShortcuts`, so bind the CLI in
`config.kdl` (use the absolute path if the binary is not on `PATH`):

```kdl
Mod+Shift+T { spawn "anima-overlay-ctl" "toggle"; }
Mod+Shift+R { spawn "anima-overlay-ctl" "reset"; }
Mod+Shift+Q { spawn "anima-overlay-ctl" "quit"; }
```

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
