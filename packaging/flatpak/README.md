# Flatpak

Status: **built and installed, but the overlay does not run in the sandbox here.**
`org.flatpak.Builder` and `org.freedesktop.Sdk//26.08` were installed user-level
(no sudo needed; a user `flathub` remote was required because `--user` installs
cannot see the system remote), and the app builds and installs as
`io.github.swordreforge.AnimaEngine` 0.1.0. The sandboxed CLI works
(`flatpak run --command=anima-overlay-ctl … status`).

The overlay itself exits with:

```
Error: "binding zwlr_layer_shell_v1: the requested global was not found in the registry"
```

i.e. the sandboxed client is **not offered `zwlr_layer_shell_v1`**, while the same
binary on the host binds it and renders. Until that is understood, run the overlay
from the portable install and keep the Flatpak for the CLI tools.

Build steps (already executed here, kept for reference): see the previous section.

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

## SteamCMD: downloads stay on the host (decided)

`steamcmd` is a host program, so `anima-workshop download` runs **outside** the
sandbox and writes to the host data dir
(`~/.local/share/AnimaEngine/steamcmd/steamapps/workshop/content/3474900/<id>`).
The Flatpak mounts that root read-only (`~/.local/share/AnimaEngine:ro`), so
`flatpak run --command=anima-workshop … play <id>` still finds the item, and
`anima-workshop list` on the host discovers the download root as well — not just
the Steam libraries.

There is deliberately **no** `--share=network`: the overlay needs none, and the
workshop CLI's network paths (`download`, `web`) belong on the host. Grant it with
`flatpak override --user --share=network <id>` if you ever want them inside the
sandbox.

Workflow:

```bash
# Host: fetch and pick an asset
anima-workshop download 3828587753
anima-workshop list

# Sandbox: render what the host downloaded (path or id both work)
flatpak run --command=anima-workshop io.github.swordreforge.AnimaEngine \
    play 3828587753 mikubuenahall
```

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
