# Follow-up plan

Ordered by "pain removed per unit of risk". Each item says what is *verified* today, what
it needs, how to accept it, and what can bite. This is a working document: tick items off
in `progress.md` as they land, or move them there when they change shape.

## 1. Overlay lifecycle: a manager that outlives the window

**Status.** Done: the registry, supervision with backoff, the CLI, liveness/zombie handling,
and the tray (one icon for every overlay; the overlay takes `--no-tray` and the shepherd
passes it). Left: a D-Bus intent call, which the registry file makes optional.

**Why.** Today the studio is the only thing that starts overlays, and each overlay's tray
(`ksni`, inside the overlay process) dies with it. So: close the window and nothing can
bring an overlay back — the reported "open once, quit from the tray, then Play does
nothing" was the same hole seen from the other side. Per-instance trays also mean N tray
icons for N overlays, which is not what a tray is for.

**What exists and is reusable (verified).**
- `ControlState` is the single control channel: one JSON file per instance in
  `~/.local/share/AnimaEngine/overlay_control/`, writers bump `generation`, the overlay
  applies the newest. The window, the CLI, the tray and the portal all use it.
- Liveness markers: `<slug>.pid` holding `<pid> <process name>`, written by the overlay on
  startup and cleared on exit, validated by `ControlState::is_running` (`/proc` + name
  prefix). An instance starting or stopping is therefore observable.
- `secondary_instances.json` maps an animation path to its persisted geometry, so a
  restarted overlay returns to where it was.
- The overlay reloads in place when its file changes (the seamless-reload work), so
  re-targeting an instance does not flicker.

**Shape.** A small long-lived binary (`anima-shepherd`, name open) that owns:
- the tray (`ksni`) — one icon, a menu listing instances with show/hide/quit each;
- a registry: desired instances (path → wanted/not wanted), read from a file so it
  survives restarts;
- supervision: start an overlay for each wanted instance that has no live marker, reap
  markers whose pid is gone, and restart on demand;
- optional: a D-Bus or file-based "start/stop this animation" call, so the studio sends
  intent instead of spawning processes itself.

**Steps.**
1. Extract the tray from `anima-overlay` into the shepherd: the overlay keeps playing, the
   tray moves out. Keep a `--no-tray` flag so the split can be tested side by side.
2. Shepherded registry file (e.g. `shepherd.json`: `[{path, enabled}]`) + a CLI to edit it
   (`anima-shepherd enable <path>`, `disable`, `status`).
3. Supervision loop: for each enabled path, `is_running(control)` → start if not; on a
   dead marker, log and (if still enabled) restart with a backoff.
4. Wire the studio's Play button to the shepherd when it is running (fall back to spawning
   directly, which is what it does now), and the niri autostart snippet to the shepherd
   instead of one overlay.
5. Tray menu: list instances (`●/○`), show/hide, quit one, quit all, open the studio.

**Acceptance.** With no studio window open: enable two animations, see two overlays come
up (one tray icon), quit one from the tray, see it restart only if still enabled, kill an
overlay with `SIGKILL` and watch the shepherd reap the stale marker and restore it, then
`pkill anima-shepherd` and confirm the overlays stay up (they own their surfaces).

**Risks.** Autostart loops (an enabled instance that crashes immediately → backoff, and a
cap on restarts); the tray moving out means the overlay's own menu items need a
replacement story; two trays while both exist during the transition.

## 2. Cache Workshop metadata on disk

**Status: done** (`thumbnails/<id>.json` with a `fetched` stamp, a day long, stale-served when
Steam is unreachable and marked in the pane; both branches verified — see `progress.md`).
Left over: pruning entries whose item is no longer installed.

**Why.** `fetch_meta` hits the keyless Steam API once per item per session; only the
preview *image* is cached (`thumbnails/<id>.img`). Titles and view counts therefore
disappear offline and are re-fetched after every restart.

**Steps.** Write `thumbnails/<id>.json` next to the image (the same payload the API
returned, plus a timestamp); read it first when fresh (a day is plenty), refresh in the
background when stale, and keep serving the old copy if the network fails. Make "stale
but offline" visible in the pane instead of showing nothing.

**Acceptance.** Disconnect, restart the studio, select an item: title, subscriptions and
views are still there, and the preview renders from the cache.

**Risks.** None worth naming: it is additive. Watch the cache growing; prune entries whose
item is no longer installed.

## 3. Search field with IME

**Why.** The list is a flat dump; with 8 items it is fine, with 80 it is not. A search box
also exercises the input path (IME for zh/ja), which the window has none of today.

**Steps.** `text_input` + a `Message::Filter(String)`; filter on id, the Steam title and
the asset file names, case-insensitively; keep the selection stable by id, not by index
(the current index-based selection breaks when the list changes).

**Acceptance.** Type a CJK query with an IME and see the list filter; clear it and get the
full list back with the previously selected item still selected.

**Risks.** The index-based `selected`/`overlay_selected` fields are the weak spot — fix
that first (key by id/path) or the filter will move the wrong row.

## 4. Browse/search the Workshop beyond what is installed

**Status: done** — `Search Steam` in the search row queries
`IPublishedFileService/QueryFiles` (needs `STEAM_WEB_API_KEY`), lists results with a cloud
marker, and gives them a pane with the CDN preview and the download/subscribe actions. The
keyless case is an explicit message naming the variable. Left: pagination (page 1, 50
results today) and a `(not installed)` label that should become an i18n key.

**Why.** Today the window can only show what is already on disk. The three-tier fallback
(Web API with a key → Community HTML → local metadata) was researched but only the keyless
`GetPublishedFileDetails` is wired.

**Steps.** `WorkshopWeb::query_files` (needs `STEAM_WEB_API_KEY`) behind a search box;
render results as rows that can be downloaded (already possible) or opened in Steam; keep
the "not installed" state distinct from "installed".

**Acceptance.** With a key set, search by title, see results that are not installed, and
download one so it appears in the installed list.

**Risks.** Without a key this tier simply does not work — the UI must say so rather than
fail silently.

## 5. Smaller items

- **List rows with thumbnails** once the library is big: 64px preview + title + id.
- **Asset picker**: `Play` currently takes the first playable asset (the `▶` buttons cover
  the rest). A remembered choice per item, plus keyboard navigation, would be kinder.
- **Disable actions for stopped instances?** The panel writes the desired state either
  way, which seeds the next start — arguably a feature. Decide, then document it.
- **Poll granularity**: the panel polls at 1s. Fine, but a `notify`-based watch on the
  control directory would be instant; only worth it if the delay is annoying.
- **A studio icon** distinct from `anima-engine.svg`.
- **Keyboard shortcuts** in the window: quit, play, refresh, next/prev item.
- **Prune dead references: done.** `anima-shepherd prune [--dry-run]` drops registry and
  persisted paths whose animation is gone, control files nobody can explain, dead markers and
  cache entries for uninstalled items (backing up both JSON files first), and
  `anima-shepherd forget <animation>` drops exactly one instance — refusing while it runs.
  Nothing whose animation still exists is ever touched.
- **Steamworks callbacks** (`ItemInstalled` / `DownloadItemResult`) remain unimplemented
  for lack of a Linux `libsteam_api.so`; the watcher + hot reload cover the behaviour.
