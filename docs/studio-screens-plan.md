# The studio's screens: a nav rail and a settings page

Status: **planned, not started.** Written before implementation, so the decisions are on the record
and the work can be resumed without re-deriving them.

## What this is for

The window currently holds everything at once: a workshop browser that floats over the left half, a
narrow overlay panel underneath it, and the detail/tuning pane on the right. That worked while there
were two things to show. There are now three more things users want — the Steam key, the motion
switch, the diagnostics that are presently crammed into one status line — and piling them into the
same two panes would make all of it smaller.

So: **one main window, several screens, and a navigation rail** to move between them.

## The facts this plan rests on (iced 0.14, immediate mode)

These decide most of the design, and they are worth stating because they contradict the usual
instinct about "safely despawning a layout":

1. **There is no layout to despawn.** `view()` rebuilds the widget tree from state every frame.
   Switching screens means building a different tree; the old one is simply not rebuilt. No teardown
   code, nothing to leak, nothing to forget.
2. **What does need care is state outside the tree** — `image::Handle` caches, in-flight `Task`s,
   child processes, the overlay watch. That is what the rules in "State and lifetime" below are for.
3. **A `Task` cannot be cancelled.** A Steam search or a metadata batch started on one screen will
   finish on another. That is harmless if its result only lands in shared state and is *reported in
   the persistent status line* — which is the rule adopted here.
4. **Only the active screen is built**, so switching screens *reduces* per-frame work rather than
   adding to it. The "despawn" benefit comes for free.
5. **Navigation is a state change**, so it rides the existing message/update loop: no router, no
   subscription, no extra dependency.

## Shape

```
┌ header: title · language · refresh · quit ──────────────────┐
│ rail ┬──────────────────────────────────────────────────────│
│ 工坊 │ the active screen                                     │
│ 浮层 │                                                      │
│ 设置 │                                                      │
│ 诊断 │                                                      │
├──────┴──────────────────────────────────────────────────────┤
│ status line — shared by every screen                        │
└─────────────────────────────────────────────────────────────┘
```

A **nav rail**, not tabs and not breadcrumbs, because:

- Material 3's own navigation for 3–5 destinations is a `NavigationRail`;
- breadcrumbs imply depth, and this hierarchy is one level deep — they would suggest a structure that
  does not exist;
- option *cards* are for choosing between alternatives inside a screen; at our minimum window size
  (760×520) they would spend the vertical room the panel needs.

## Screens

| Screen | Shows | Reuses | New state |
|---|---|---|---|
| **工坊** (workshop) | today's content: picker float, detail pane, tuning area | `picker_layer`, `result_rows`, `asset_preview_area`, `overlay_panel` | none |
| **浮层** (overlays) | every instance as a row with its parameters, instead of a narrow panel | `overlay_instances()`, `apply_overlay`, `OverlayAction` | none |
| **设置** (settings) | Steam key, reduce-motion, palette source, defaults | `StudioSettings`, `resolve_api_key` | `studio.json` gains `reduce_motion` |
| **诊断** (diagnostics) | version, roots, catalog path, shepherd state, D-Bus name, cache size | `refresh()`'s numbers, `shepherd::is_up`, `dbus_intent::BUS_NAME` | none |

The settings screen is what finally removes two "only via a file or an environment variable" corners:
`steam_api_key` (already in `studio.json`, so the screen is a form over it) and `ANIMA_NO_MOTION`
(which becomes `reduce_motion` in the same file, with the environment variable kept as an override,
exactly like the key's precedence).

## State and lifetime

- **Shared, never per-screen**: `i18n`, `snapshot`, `meta`, `thumbs`, `asset_previews`, `overlay`,
  `children`, `focus_seen`, the fade animations, the status/`note` line, and the new `screen`.
- **Per-screen but never dropped on navigation**: `query`, `selected_item`, `previewed_asset`,
  `staged`, the remote-search state. Coming back to the workshop must show what you left, not a reset.
- **Bound to one screen**: the picker float (`picker` + `picker_motion`) is rendered only while
  `screen == Screen::Workshop`, so it cannot appear over the settings form.
- **Never owned by a screen**: child-process reaping and the overlay fingerprint poll. They run in
  `subscription()`/`poll_overlay()` regardless of what is on screen, because an overlay exiting is not
  a screen-local event.
- **Dropped on purpose**: nothing else. A screen you leave keeps its state; that is a feature.

## Steps

### Step 1 — the skeleton (no new content)

1. `enum Screen { Workshop, Overlays, Settings, Diagnostics }`, `Default = Workshop`. Put it next to
   `enum Message`, and add `Message::Screen(Screen)`.
2. `struct Studio` gains `screen: Screen`; `Message::Screen` sets it (and nothing else — no state is
   cleared, per the rules above).
3. `fn view()` currently builds `header` → `bodies` (`left` + `divider` + `details`) → status. Split
   it: keep `header` and the status line, rename the existing body-building into
   `fn screen_workshop(&self) -> El<'_>`, and add `fn nav_rail(&self) -> El<'_>` — a `column!` of four
   buttons, each `Message::Screen(..)`, styled `primary` when active and `text`/`secondary` otherwise,
   in a `container` with `surface_container_low` so it reads as the rail it is.
4. Assemble: `row![nav_rail(), screen]`, then `column![header, that_row, status]`.
5. Gate the float: `picker_layer()` returns `None` unless `self.screen == Screen::Workshop`.
6. **Done when** all four slots exist and are empty placeholders except 工坊, the window still looks
   and behaves exactly as today on that screen, and the tests are unchanged and green.

### Step 2 — the settings screen

1. `StudioSettings` (in `src/settings.rs`) gains `#[serde(default)] reduce_motion: bool`.
2. `motion_enabled()` in the studio reads the file first and the **environment as the override** —
   the same precedence as `resolve_api_key`, for the same reason (a deliberate temporary override
   beats a stored preference), and blank/absent is not an override.
3. `fn screen_settings(&self) -> El<'_>`: a labelled text input for the key, a switch-like button for
   reduce-motion, the palette path (read-only, with the URL to noctalia's file), and a `Saved` note
   after a write. Immediate writes, not a save button — there is nothing to lose here.
4. Persist through the existing `ConfigFile` trait (`StudioSettings::save`), and re-resolve the theme
   and the `motion` flag after a write so the change is visible at once.
5. **i18n**: `Settings`, `Diagnostics`, `Overlays`(reuse `Overlay`), `Steam API key`, `Reduce motion`,
   `Saved`, `Copy all` (diagnostics). Four languages, through `anima-i18n merge`, then `verify`.
6. **Tests**: `studio.json` round-trip with `reduce_motion`; the precedence helper as a pure function
   (`resolve_flag(env: Option<bool>, file: bool) -> bool`), the way `resolve_api_key` is tested.

### Step 3 — overlays and diagnostics

1. `fn screen_overlays(&self) -> El<'_>`: one row per instance from `overlay_instances()` — label,
   path on hover, running marker, and the parameter buttons for *that* row, so a dozen instances stop
   competing for one panel.
2. `fn screen_diagnostics(&self) -> El<'_>`: version, roots, catalog path and entry count, shepherd
   liveness, the D-Bus name and object path, the metadata/preview cache directory and its size. One
   `Copy all` button, because the point of this screen is bug reports.
3. Retire `overlay_panel()` from the workshop screen once the overlays screen covers it (decide by
   using both for a day; the plan is to keep the small panel *only* if it earns its room).

## Risks, and what to do about them

1. **The picker's position depends on the window, not on a screen.** It floats over "the left half of
   the window" today; once the rail exists it should float over the left half of the *content area*.
   The fix is the layer's padding, not a rewrite — but it must be done in the same commit that adds
   the rail, or the float will sit over the navigation.
2. **An error can surface on a screen you are not looking at.** A Steam search that fails after you
   moved to settings reports in the status line, which is shared — deliberately, see the facts above.
   Do not add per-screen error modals; they are exactly what would make a background failure invisible.
3. **Settings written immediately can fight the running window.** Re-resolve derived values (theme,
   `motion`) after a write, and say so in the status line (`Saved`), or the UI will look unchanged
   while the file says otherwise.

## Open questions (decide before step 3)

1. **Picker float or inline list?** The float existed because the left column had no room. The workshop
   screen now has room, so an inline, always-visible list may be the better answer — one less mode to
   learn. Worth trying on the branch before committing to it.
2. **Do settings show `Saved` inline or in the status line?** Inline is more local; the status line is
   consistent with everything else. Current lean: inline note, because the action is right there.
3. **Diagnostics `Copy all` format**: plain text (one `key: value` per line, pasteable into an issue)
   versus JSON. Current lean: plain text.

## Verification, once implemented

- Pure tests: `Screen` default and round-trip; `studio.json` with `reduce_motion`; the flag-precedence
  helper; the settings write path over a temp file.
- E2E by screenshot: all four rails, the active one visibly distinct, the workshop screen pixel-for-
  pixel as before, and the settings write reflected without a restart.
- Cost: per-frame work should *drop* on every screen that is not the workshop. Measure idle CPU the way
  the animation work was measured (`/proc/<pid>/stat` deltas, not `ps` averages) and quote it.
