# Steam Workshop subsystem — reverse-engineering notes

Static analysis of the shipped Windows build, to pin down how AnimaEngine talks to
Steam (source for the main entry point is not shipped).

## Provenance / method

- Target: `steamapps/common/AnimaEngine/AnimaEngineKIVI.exe`
  (PE32+ x64, 23 MB, PyInstaller onefile).
- Extracted the CArchive with `pyinstxtractor`; entry script is
  `AnimaEngineKIVI.pyc` (CPython **3.10**).
- Disassembled with real CPython 3.10.22 (via `uv`) using `marshal` + `dis`.
  `pycdc` is incomplete for 3.10 (`Unsupported opcode: JUMP_IF_NOT_EXC_MATCH`).
- **Static only** — nothing was executed. Everything below is read from bytecode.

`steam_api64.dll` is Valve's runtime; `SteamworksPy64.dll` is the bundled
SteamworksPy shim (137 exports). Its `Workshop_*` export list has **no**
`Workshop_DownloadItem`. The app does not rely on that shim for downloads: it
uses its own ctypes bridge to the **flat C API** (`SteamAPI_ISteamUGC_*`).

## `SteamworksFlat` (app's own ctypes bridge)

Class constants:

```
QUERY_RANKED_BY_PUBLICATION_DATE = 1
QUERY_RANKED_BY_TREND            = 3
MATCHING_ITEMS                   = 0
RESULT_OK                        = 1
QUERY_CALLBACK_ID                = 3401
INVALID_QUERY_HANDLE             = 0xFFFFFFFFFFFFFFFF
ITEM_SUBSCRIBED                  = 1
ITEM_INSTALLED                   = 4
ITEM_NEEDS_UPDATE                = 8
ITEM_DOWNLOADING                 = 16
ITEM_DOWNLOAD_PENDING            = 32
```

Methods: `_load_dll`, `_bind_core`, `init`, `shutdown`, `pump`, `_get_ugc_ptr`,
`_get_utils_ptr`, `_bind_subscribed_funcs`, `get_subscribed_ids`,
`_bind_query_funcs`, `_wait_query_result`, `query_workshop`,
`_bind_download_funcs`, `get_item_install_folder`, `unsubscribe_item`,
`subscribe_and_download`.

Lifecycle:

- `init()`: try `SteamAPI_InitFlat` (1024-byte error buffer; success = 0), else
  fall back to `SteamAPI_InitSafe`.
- `pump()`: `SteamAPI_RunCallbacks` (swallows exceptions).
- `shutdown()`: `SteamAPI_Shutdown`.

Bound signatures (from `_bind_download_funcs` / `_bind_subscribed_funcs`):

```
GetNumSubscribedItems(c_void_p) -> c_uint32
GetSubscribedItems(c_void_p, POINTER(c_uint64), c_uint32) -> c_uint32
GetItemState(c_void_p, c_uint64) -> c_uint32
GetItemInstallInfo(c_void_p, c_uint64, POINTER(c_uint64), c_char_p, c_uint32, POINTER(c_uint32)) -> c_bool
SubscribeItem(c_void_p, c_uint64) -> c_uint64
UnsubscribeItem(c_void_p, c_uint64) -> c_uint64
DownloadItem(c_void_p, c_uint64, c_bool) -> c_bool
```

## Download flow (reconstructed)

`subscribe_and_download(published_file_id, timeout=120.0, force=False)` —
`force=True` is used after AnimaEngine intentionally removed a local item, to
bypass Steam's cached `INSTALLED` state.

```python
ugc = self._get_ugc_ptr()
if not ugc:
    raise RuntimeError('ISteamUGC is unavailable.')
self._bind_download_funcs()
fid = int(published_file_id)
state = int(GetItemState(ugc, c_uint64(fid)))

# Fast path: subscribed + installed + not pending an update.
if not force and (state & SUBSCRIBED) and (state & INSTALLED) \
   and not (state & (NEEDS_UPDATE | DOWNLOADING | DOWNLOAD_PENDING)):
    folder = self.get_item_install_folder(fid)
    if folder and os.path.isdir(folder):
        return folder

# Subscribe if needed, wait up to 2s for the flag to settle.
if not (state & SUBSCRIBED):
    try:
        SubscribeItem(ugc, c_uint64(fid))
    except Exception:
        pass
    deadline = time.monotonic() + 2.0
    while time.monotonic() < deadline:
        self.pump()
        state = int(GetItemState(ugc, c_uint64(fid)))
        if state & SUBSCRIBED:
            break

if not DownloadItem(ugc, c_uint64(fid), True):   # high priority
    raise RuntimeError('Steam rejected DownloadItem.')

# Poll until installed and no longer updating.
deadline = time.monotonic() + max(5.0, float(timeout))
while time.monotonic() < deadline:
    self.pump()
    state = int(GetItemState(ugc, c_uint64(fid)))
    if (state & INSTALLED) and not (state & (NEEDS_UPDATE | DOWNLOADING | DOWNLOAD_PENDING)):
        folder = self.get_item_install_folder(fid)
        if folder and os.path.isdir(folder):
            return folder
    time.sleep(0.2)

folder = self.get_item_install_folder(fid)
if folder and os.path.isdir(folder):
    return folder
raise TimeoutError('Steam Workshop download timed out.')
```

`get_item_install_folder(published_file_id)`:

```python
ugc = self._get_ugc_ptr()
if not ugc:
    return ''
self._bind_download_funcs()
disk_size = c_uint64(0)
timestamp = c_uint32(0)
folder = create_string_buffer(4096)
ok = bool(GetItemInstallInfo(ugc, c_uint64(int(fid)), byref(disk_size),
                             folder, sizeof(folder), byref(timestamp)))
if not ok:
    return ''
return folder.value.decode('utf-8', 'replace')
```

## Browse flow

The app has a three-tier fallback when fetching workshop listings:

1. **Steamworks UGC query** (preferred, follows the logged-in Steam client):
   `CreateQueryAllUGCRequestPager` → `SetSearchText` / `SetAllowCachedResponse`
   → `SendQueryUGCRequest` → poll `IsAPICallCompleted` / `GetAPICallResult`
   (`QUERY_CALLBACK_ID = 3401`) → `GetQueryUGCResult` /
   `GetQueryUGCPreviewURL` → `ReleaseQueryUGCRequest`.
2. **Steam Web API** with a developer key
   (`ANIMAENGINE_STEAM_WEB_API_KEY` or `workshop_api_key.txt`) →
   `IPublishedFileService/QueryFiles/v1` and
   `ISteamRemoteStorage/GetPublishedFileDetails/v1`.
3. **Steam Community HTML** (last resort): scrape
   `https://steamcommunity.com/workshop/browse/` for
   `sharedfiles/filedetails/?id=<id>` and `data-publishedfileid="<id>"`,
   with `User-Agent: AnimaEngine/3474900 Workshop Browser`.

### Tier 1 — Steamworks UGC (preferred)

`_workshop_steamworks_page(sort_mode, search, …)` runs under `_STEAMWORKS_FLAT_LOCK`:
build `SteamworksFlat`, `init()` (raises `RuntimeError("Steamworks could not
initialize. Is Steam open?")` on failure), call `query_workshop(APP_ID, sort,
search, page)`, then `shutdown()`.

`SteamworksFlat.query_workshop`:

```python
query_type = QUERY_RANKED_BY_PUBLICATION_DATE if sort == "recent" else QUERY_RANKED_BY_TREND
handle = CreateQueryAllUGCRequestPage(MATCHING_ITEMS, query_type, app_id, app_id, page)
if handle == INVALID_QUERY_HANDLE:
    raise RuntimeError("Steam could not create the Workshop query.")
try: SetSearchText(ugc, handle, search.encode("utf-8").strip())
try: SetAllowCachedResponse(ugc, handle, ...)
call = SendQueryUGCRequest(ugc, handle)
if not call:
    raise RuntimeError("Steam could not send the Workshop query.")
result = _wait_query_result(call, timeout=18.0)   # IsAPICallCompleted/GetAPICallResult, callback id 3401
if result.eresult != RESULT_OK:
    raise RuntimeError(f"Steam Workshop query returned EResult={result.eresult}.")
items = []
for i in range(result.numResultsReturned):
    d = _SteamUGCDetailsLatest()
    GetQueryUGCResult(ugc, handle, i, byref(d))
    preview = GetQueryUGCPreviewURL(ugc, handle, i, buf, sizeof(buf))
    items.append({
        "publishedfileid": ..., "result": ..., "creator_app_id": ...,
        "consumer_app_id": ..., "title": ..., "description": ...,
        "short_description": ..., "preview_url": ..., "time_created": ...,
        "time_updated": ..., "visibility": ..., "banned": ...,
        "accepted_for_use": ..., "tags": ..., "votes_up": ...,
        "votes_down": ..., "score": ..., "file_size": ...,
        "source": "steamworks",
    })
ReleaseQueryUGCRequest(ugc, handle)
return {"items": items, "total": result.totalMatchingResults, "page": ceil(total / per_page)}
```

### Tier 2 — Steam Web API (developer key)

```python
except Exception as e:
    print("[WORKSHOP] Steamworks query fallback:", e)

key = _get_workshop_api_key()   # ANIMAENGINE_STEAM_WEB_API_KEY, or <data>/AnimaEngine/workshop_api_key.txt
if key:
    r = requests.post(
        "https://api.steampowered.com/IPublishedFileService/QueryFiles/v1/",
        data={"key": key, "appid": APP_ID, "search_text": search,
              "return_short_description": "true", "page": page, "numperpage": per_page})
    r.raise_for_status()
    body = r.json()["response"]
    items = body.get("publishedfiledetails", [])
    for it in items:
        it["source"] = "web_api"
    total = body.get("total", len(items))
```

### Tier 3 — Steam Community HTML (last resort)

```python
r = requests.get(
    "https://steamcommunity.com/workshop/browse/",
    params={"browsesort": "MostRecent" if sort == "recent" else "trend",
            "section": "readytouseitems", "searchtext": search, "p": page},
    headers={"User-Agent": "AnimaEngine/3474900 Workshop Browser"})
r.raise_for_status()
ids = re.findall(r"sharedfiles/filedetails/\?id=(\d+)", r.text) \
    + re.findall(r"data-publishedfileid=[\"'](\d+)[\"']", r.text)
r2 = requests.post(
    "https://api.steampowered.com/ISteamRemoteStorage/GetPublishedFileDetails/v1/",
    data={"itemcount": len(ids), "publishedfileids[i]": id, ...})
r2.raise_for_status()
items = r2.json()["response"]["publishedfiledetails"]
for it in items:
    it["source"] = "public"
```

Every item carries `source` in `{steamworks, web_api, public}` so the UI can show
provenance. `_workshop_public_pages` loops pages and dedupes by `publishedfileid`.

## Upload flow

Two upload paths exist.

### Modern — `WorkshopUploadWorker` (SteamworksPy)

```python
def run(self):
    # validate payload: title, content_dir isdir, preview isfile, tags non-empty
    self.log("Inicializando SteamAPI...")
    if not STEAMWORKS.initialize():
        self.error("SteamAPI_Init falló. Abre Steam (cliente) e inicia sesión ...")
        return
    self.log("SteamAPI_Init OK")

    ws = SteamWorkshop()
    ws.SetItemCreatedCallback(_CreateItemResult_t(self.on_created))
    ws.SetItemUpdatedCallback(_SubmitItemUpdateResult_t(self.on_submitted))

    if payload.published_id:
        self.log("Updating existing Workshop item: ...")
        published_id = payload.published_id
    else:
        self.log("Creating Workshop item...")
        ws.CreateItem(app_id, EWorkshopFileType.k_EWorkshopFileTypeCommunity)
        while not created and not stopped:
            ws.run_callbacks(); time.sleep(...)

    handle = ws.StartItemUpdate(app_id, c_uint64(published_id))
    if handle == 0:
        raise RuntimeError("StartItemUpdate devolvió handle inválido")
    ws.SetItemTitle(handle, title)
    ws.SetItemDescription(handle, description)
    ws.SetItemContent(handle, content_dir)
    ws.SetItemPreview(handle, preview_img)
    ws.SetItemVisibility(handle, ERemoteStoragePublishedFileVisibility...)
    ws.Workshop_SetItemTags(handle, tags_ptr_array, len(tags))
    ws.SubmitItemUpdate(handle, change_note)
    while not submitted and not stopped:
        p = ws.GetItemUpdateProgress(handle)
        self.progress(p["progress"]); ws.run_callbacks()
```

`on_created` emits `created` + `published_id` and a `needs_legal` flag when
`userNeedsToAcceptWorkshopLegalAgreement` is set; `on_submitted` emits
`submitted`.

### Legacy — SteamCMD `workshop.vdf`

`TransparentApp.upload_to_workshop` → `run_upload` writes `workshop.vdf` and
invokes SteamCMD:

```text
"workshopitem"
{
    "appid"         "3474900"
    "contentfolder" "<folder>"
    "previewfile"   "<preview>"
    "title"         "<title>"
    "description"   "<description>"
    "visibility"    "0"
    "tags"
    {
        "anime" "dance" "cute"
    }
}
```

```python
_run_steamcmd(["+login", steam_username, "+workshop_build_item", "workshop.vdf", "+quit"])
```

`TransparentApp.workshop` is an even older path that just spawns an external
`workShop.exe` when present.

## Local content & management (reconstructed)

- `_get_workshop_api_key`: env `ANIMAENGINE_STEAM_WEB_API_KEY`, else
  `<LOCALAPPDATA|APPDATA|~>/AnimaEngine/workshop_api_key.txt`.
- `_ws_candidate_workshop_content_roots` / `_steam_default_workshop_content_dir`:
  check the SteamCMD executable dir, CWD, `ANIMAENGINE_RUNTIME_DIR`, the Windows
  registry `HKCU\Software\Valve\Steam\SteamPath`, and parse `libraryfolders.vdf`
  with `"path"\s*"([^"]+)"`; content root =
  `<library>/steamapps/workshop/content/3474900`.
- `_workshop_item_id_for_path`: regex
  `\\steamapps\\workshop\\content\\3474900\\(\d+)(?:\\|$)` (case-insensitive)
  → the owning published file id.
- `_workshop_safe_item_folder`: true only for
  `.../steamapps/workshop/content/3474900/<digits>`;
  `_workshop_purge_managed_folder` refuses anything else and removes the folder
  with retries.
- Hidden items: `<data>/AnimaEngine/workshop_hidden_items.json`
  `{"hidden_ids": [...]}` via `_workshop_hidden_ids` / `_workshop_set_hidden`.
- Editable copies: `_workshop_edit_root` =
  `<data>/AnimaEngine/workshop_edits/<id>_edited_<name>`;
  `_detach_workshop_animation_for_edit` copies a Steam-managed file before
  destructive edits (resize/rotate) and carries audio settings across.
- `_ws_sync_workshop_downloads_once`: scan candidate roots → list item dirs →
  skip hidden ids → `_workshop_animation_files` (optionally normalizes GIF→APNG)
  → one atomic merge into the library.
- `_workshop_steamworks_repair_download`: init Steamworks → if a folder exists,
  `unsubscribe_item` → optionally purge the managed folder → `subscribe_and_download`
  (force) → shutdown.

## Mapping to the Rust `steamworks` crate

| App ctypes flat call | `steamworks` crate |
|---|---|
| `SteamAPI_InitFlat` / `InitSafe` | `Client::init_app` |
| `SteamAPI_RunCallbacks` | `single_client().run_callbacks()` |
| `GetNumSubscribedItems` / `GetSubscribedItems` | `ugc.subscribed_items()` |
| `GetItemState` | `ugc.item_state(id)` (same bitflags) |
| `DownloadItem(id, True)` | `ugc.download_item(id, true)` |
| `GetItemInstallInfo` | `ugc.item_install_info(id)` |
| `SubscribeItem` / `UnsubscribeItem` | `ugc.subscribe_item` / `unsubscribe_item` |
| UGC query | `ugc.query_items(...)` (callback-based) |

This slots into `crate::workshop::WorkshopSource`: add `download(id, force)` and
implement `ensure_installed` as fast-path → subscribe → `DownloadItem` → poll.

## Limitations

- Static analysis only; runtime behaviour (e.g. actual Steam timing, callback
  ordering) is inferred from the code, not observed.
- The PYZ member table was not extracted (host/target Python mismatch); the
  entry script itself was extracted and contains the workshop logic.
- `SteamAPI_InitFlat` is a newer SDK entry point; availability depends on the
  shipped `steam_api64.dll` (Windows) / `libsteam_api.so` (Linux, not present
  in this install).
