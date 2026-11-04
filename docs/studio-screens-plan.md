# The studio's tabs: a header strip, one column per screen

Status: **steps 1–2 done; the picker float is gone.** 设置 and 诊断 carry their content, 浮层 is the old
panel as a page of its own (`tab_overlays`), and the workshop results are **inline** in the left
column — no popup. Deleted with the float: `picker_layer`, `scrim`, `picker_in`, `open_picker`, the
`picker` / `picker_motion` fields, and the two tests whose only subject was the popup's behaviour. The
open question this file carried about "float or inline" is answered, by the user, in favour of inline.

Revised on purpose, and the revision is the interesting part. The first draft of this file proposed a
navigation rail with four destinations and a table of per-screen state — heavier than the idea needs,
and it treated "despawn the old layout" as a problem requiring machinery. It is not:

- iced is immediate mode. `view()` rebuilds the tree from state every frame, so an unselected branch of
  a `match` **is not built at all**. There is no old layout to dispose of, no teardown, nothing to
  forget.
- Switching is therefore one field and one `match`. There is no callback to register either: a button
  emits `Message::Tab(..)`, `update` sets the field, `view` matches on it. The message loop *is* the
  callback.

So: **a tab strip under the header, like Steam's, and single-column screens.** Nothing else.

## Shape

```
┌ AnimaEngine            语言 [简体中文] [↻] [退出] ┐
├ 工坊 │ 浮层 │ 设置 │ 诊断                              ┤   ← the strip; active one is `primary`
├──────────────────────────────────────────────────────┤
│ the active screen (a single column, scrolling)        │
├──────────────────────────────────────────────────────┤
│ status line — shared by every tab                     │
└──────────────────────────────────────────────────────┘
```

- The strip lives on **its own row below the header**, not inside it: the header already carries the
  title, the language picker, refresh and quit, and at the 760px minimum four more labels would crowd
  it. A second row is also what Steam does, which is the reference that prompted this.
- Tabs, not a rail, not breadcrumbs: one level deep, four destinations, and the content is a column.

## State

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Workshop,
    Overlays,
    Settings,
    Diagnostics,
}
```

`Studio` gains `tab: Tab`; `Message::Tab(Tab)` sets it and does **nothing else**. That "nothing else" is
the whole safety story: no state is cleared on a switch, so coming back shows what you left, and no
state is disposed, so there is nothing to leak.

**Shared, never per-tab:** `i18n`, `snapshot`, `meta`, `thumbs`, `asset_previews`, `overlay`,
`children`, `focus_seen`, the fades, the status/`note` line, `tab`.

**Per-tab but kept across switches:** `query`, `selected_item`, `previewed_asset`, `staged`, the
remote-search state. Being able to come back to the workshop exactly as you left it is a feature.

**Bound to one tab:** the picker float (`picker` + `picker_motion`) renders only while
`tab == Tab::Workshop`.

**Never owned by a tab:** child-process reaping and the overlay fingerprint poll. They run in
`subscription()`/`poll_overlay()` whatever is on screen, because an overlay exiting is not a
screen-local event.

**Not cancellable:** a `Task` started on one tab finishes on another. Its result lands in shared state
and is reported in the shared status line — deliberately, because a background failure must not be
invisible just because you moved.

## Screens

| Tab | Content | Reuses |
|---|---|---|
| **工坊** | exactly today's two-pane view (picker float, details, tuning) | everything in `view()` today |
| **设置** | one column: Steam key, reduce-motion, palette path | `StudioSettings`, `resolve_api_key` |
| **诊断** | one column: version, roots, catalog path, shepherd, D-Bus name, cache size | `refresh()`'s numbers, `shepherd::is_up`, `dbus_intent::BUS_NAME` |
| **浮层** | one column: every instance as a row with its own parameters | `overlay_instances()`, `apply_overlay` |

The workshop tab keeps its two panes — it is a browser with a detail pane, and that is not what "single
column" was about. The other three are columns because they are lists of facts or settings.

The settings tab is what finally removes two "only via a file or an environment variable" corners:
`steam_api_key` (already in `studio.json`) and `ANIMA_NO_MOTION`, which becomes `reduce_motion` in the
same file with the environment kept as the override — the same precedence `resolve_api_key` already
uses, for the same reason.

## Steps

### Step 1 — the strip

1. Add `Tab` and `Message::Tab(Tab)`; `Studio` gains `tab`, `Default = Workshop`.
2. Split `view()`: keep the header and the status line, move today's body into
   `fn tab_workshop(&self) -> El<'_>`, and add `fn tab_strip(&self) -> El<'_>` — a `row!` of four
   buttons, `primary` when active and `text` otherwise.
3. Dispatch: `match self.tab { Workshop => self.tab_workshop(), Overlays => …, Settings => …,
   Diagnostics => … }`, with the three new tabs as `placeholder` boxes for now.
4. `picker_layer()` returns `None` unless `tab == Tab::Workshop`.
5. **Done when** the workshop tab is pixel-for-pixel what it is today, the strip switches to three
   honest placeholders, and every existing test is untouched and green.

### Step 2 — the settings and diagnostics tabs

1. `StudioSettings` gains `#[serde(default)] reduce_motion: bool`.
2. `motion_enabled()` reads the file, with the environment as the override (pure helper
   `resolve_flag(env: Option<bool>, file: bool) -> bool`, tested like `resolve_api_key`).
3. `fn tab_settings(&self) -> El<'_>`: labelled text input for the key, a toggle button for
   reduce-motion, the palette path read-only, and a `Saved` note after a write. Writes are immediate —
   there is nothing here worth a save button — and derived values (theme, `motion`) are re-resolved
   after the write so the change is visible at once.
4. `fn tab_diagnostics(&self) -> El<'_>`: version, roots, catalog path and entry count, shepherd
   liveness, D-Bus name and object path, cache directory and size, and one `Copy all`.
5. **i18n**: `Settings`, `Diagnostics`, `Steam API key`, `Reduce motion`, `Saved`, `Copy all`
   (existing keys cover the rest: `Workshop`, `Overlay`). Four languages, `anima-i18n merge`, `verify`.
6. **Tests**: `studio.json` round-trip with `reduce_motion`; `resolve_flag`; the settings write path
   over a temp file.

## Risks

1. **An error can surface on a tab you are not looking at.** It reports in the shared status line.
   Do not add per-tab error dialogs — they are exactly what would hide a background failure.
2. **Immediate writes can fight the running window.** Re-resolve derived values after a write and say
   `Saved`, or the UI will look unchanged while the file says otherwise.
3. **Four labels at 760px.** Their own row below the header is what makes this fit; putting them in the
   header would crowd a row that already has four things in it.

## Open questions

1. **Does the picker float survive the workshop tab?** It existed because the left column had no room;
   the workshop tab has the same shape as today, so it can stay. But with the workshop now one tab
   among four, an inline always-visible list may read better. Try it before deciding.
2. **`Saved` inline or in the status line?** Lean: inline, because the action is right there.
3. **`Copy all` format**: plain `key: value` lines versus JSON. Lean: plain text, for pasting into an
   issue.

## Verification

- Pure tests: `Tab` default; `studio.json` with `reduce_motion`; `resolve_flag`; the settings write.
- Screenshot: all four tabs, the active one visibly distinct, the workshop tab unchanged from today.
- Cost: per-frame work should drop on the three new tabs. Measure idle CPU with `/proc/<pid>/stat`
  deltas, not `ps` averages — `ps` reports the process lifetime average and lied about this once
  already.
