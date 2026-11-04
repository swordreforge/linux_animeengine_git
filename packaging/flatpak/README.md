# Flatpak

Status: **manifest written, not built here.** This machine has Flatpak 1.18.4 and
`org.freedesktop.Platform//26.08`, but **not** `flatpak-builder` and **not**
`org.freedesktop.Sdk//26.08`, so nothing below has been executed. Treat the
manifest as reviewed-by-inspection, not verified.

## Build

```bash
# Two system changes are required first (your call):
sudo flatpak install flathub org.freedesktop.Sdk//26.08
sudo apt install flatpak-builder           # or your distro's package

# Binaries are built outside the sandbox; the manifest only installs them.
cargo build --release --features overlay,bc7

cd packaging/flatpak
flatpak-builder --user --install --force-clean build-dir \
    io.github.swordreforge.AnimaEngine.yml

flatpak run io.github.swordreforge.AnimaEngine <animation.apng>
```

## Permissions and why

| `finish-args` | Reason |
| --- | --- |
| `--socket=wayland` | layer-shell surface (`zwlr_layer_shell_v1`) and wl_pointer |
| `--device=dri` | wgpu needs `/dev/dri` for Vulkan |
| `--share=network` | SteamCMD downloads; Steam Web API metadata |
| `--talk-name=org.kde.StatusNotifierWatcher` | the `ksni` tray item |
| `--filesystem=~/.local/share/Steam:ro` + `~/.steam:ro` | read the client's installed workshop items (the paths the library discovery already uses) |
| `--filesystem=home:ro` | animation sources are wherever you keep them; narrow it if you prefer (see below) |

Deliberately **not** granted: no `--filesystem=host`, no write access to `$HOME`,
no `--socket=pulseaudio`, no X11 socket.

## The one real consequence: `anima-overlay-ctl` must run inside the sandbox

The overlay and its control CLI talk through
`<data dir>/overlay_control/<slug>.json`. Inside a Flatpak the data dir is
`~/.var/app/io.github.swordreforge.AnimaEngine/data`, which the host CLI cannot
see. So the niri binds must go through Flatpak:

```kdl
binds {
    Mod+Alt+O       { spawn "flatpak" "run" "--command=anima-overlay-ctl" "io.github.swordreforge.AnimaEngine" "toggle"; }
    Mod+Alt+Equal   { spawn "flatpak" "run" "--command=anima-overlay-ctl" "io.github.swordreforge.AnimaEngine" "scale" "up"; }
    Mod+Alt+Minus   { spawn "flatpak" "run" "--command=anima-overlay-ctl" "io.github.swordreforge.AnimaEngine" "scale" "down"; }
    Mod+Alt+R       { spawn "flatpak" "run" "--command=anima-overlay-ctl" "io.github.swordreforge.AnimaEngine" "reset"; }
    Mod+Alt+H       { spawn "flatpak" "run" "--command=anima-overlay-ctl" "io.github.swordreforge.AnimaEngine" "toggle-hidden"; }
    Mod+Alt+Shift+Q { spawn "flatpak" "run" "--command=anima-overlay-ctl" "io.github.swordreforge.AnimaEngine" "quit"; }
}
```

The same applies to `anima-i18n`/`anima-workshop` (`flatpak run --command=...`).
Instance state, the frame cache and the translation catalog all live in the
sandbox's data dir; the portable install and the Flatpak therefore do not share
them.

## SteamCMD

`steamcmd` is a host program and is **not** available inside the sandbox. Two
honest options:

1. **Keep downloads on the host** (recommended): run
   `anima-workshop download <id>` outside Flatpak (it writes to the host data
   dir), then read the content through the `~/.local/share/Steam` mount. The
   Flatpak's `anima-workshop list`/`play` only need read access.
2. **Ship SteamCMD in the sandbox**: add a module with
   `type: archive, url: https://steamcdn-a.akamaihd.net/client/installer/steamcmd_linux.tar.gz`
   and its `sha256` (compute it yourself — I will not guess a checksum). It then
   downloads into the sandbox's data dir.

## Narrowing or widening

```bash
# Only allow one animation folder instead of the whole home directory:
flatpak override --user --nofilesystem=home \
    io.github.swordreforge.AnimaEngine
flatpak override --user --filesystem=~/Pictures:ro \
    io.github.swordreforge.AnimaEngine

# Inspect what is actually granted:
flatpak info --show-permissions io.github.swordreforge.AnimaEngine
```

## Known gaps for a real submission

- No `.desktop`-launchable default animation: the entry starts `anima-overlay`
  with the generated test pattern.
- AppStream validation was checked for the portable install; re-run
  `appstreamcli validate` on the installed metadata after a Flatpak build.
- `flatpak-builder` lints (`flatpak-builder-lint`) have not been run.
