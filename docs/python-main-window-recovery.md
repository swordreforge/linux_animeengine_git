# Recovering the main window from `AnimaEngineKIVI.exe`

The Python app's **main window** is the one file with no source in the tree. It exists only inside
`AnimaEngineKIVI.exe`, which is a **PyInstaller archive** built with **Python 3.10** — as its magic
number says (`3439`). Everything else the app ships is readable source under `_internal/`
(`anima_foundation.py`, `anima_design.py`, `anima_overlay_*.py`, the conversion tools), so this is the
only artefact that has to be recovered rather than read.

There is also an existing note for the Steamworks side: [`workshop-steamworks-re.md`](./workshop-steamworks-re.md).

## Getting the bytecode out

The exe is the archive, and pyinstaller's own viewer is enough — no unpacking tool needed:

```bash
uv tool install pyinstaller
uv tool run --from pyinstaller pyi-archive_viewer exe-or-pyc -l   # list
uv tool run --from pyinstaller pyi-archive_viewer exe             # x <entry>, then q
```

The listing shows the entry as `'s', 'AnimaEngineKIVI'` — **373,151 bytes uncompressed**, 730 bytes short
of the 373,167-byte `.pyc` it becomes. What comes out is a **raw marshalled code object** (first byte
`e3`), with **no `.pyc` header**, so one has to be prepended:

```bash
printf '\x6f\x0d\x0d\x0a\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00' > hdr.bin
cat hdr.bin AnimaEngineKIVI.raw > AnimaEngineKIVI.pyc
```

`6f 0d 0d 0a` is Python 3.10's magic. REA's `inspect_python_pyc` then reads it without executing
anything and confirms the shape: **magic 3439, layout `py38`, 729 code objects, 24,922 marshal objects,
depth 14**. (It needs `max_objects` raised past its 16,384 default, and it does **not** decompile — by
design.)

## The wall: pycdc cannot do 3.10 exceptions

`pycdc` is the only decompiler that gets anywhere with 3.10 (`uncompyle6` and `decompyle3` stop at 3.8),
and what it does is bounded by two facts:

* **Unsupported opcodes abort the whole module.** `ASTree.cpp`'s `default:` case does not merely warn —
  it sets `cleanBuild = false` and returns, so one unknown opcode costs the entire file. That is why a
  373 KB script decompiles to **eight lines**.
* **The first hard one is `JUMP_IF_NOT_EXC_MATCH` (121)**, which every `except` in 3.10 begins with. It
  is now handled (see below), and the decompiler reaches **~100 lines** before it dies a different death:
  a **segfault** in pycdc's block/stack-history model, because it models an `except` head as an `if`.

That second failure is a **model** gap, not a table gap. There are no unsupported opcodes left — the
counter is zero — and both a patched `pycdc` and a rewritten bytecode stream die at the **same line**, at
the same `try/except ImportError` in the module's preamble. Fixing it means reworking pycdc's 3.10
exception handling; it is hours, not one more case. Stopping there is a decision, not an oversight.

## Two techniques worth keeping

**Emitting the same sense as the thing you replace.** For `JUMP_IF_NOT_EXC_MATCH` there is no code change
needed at all: it **pops one value and jumps on a condition**, which is exactly `POP_JUMP_IF_TRUE`
(121 → 115). Rewriting the opcode inside the marshalled stream is reusable on any 3.10 file and needs no
compiler; the C++ case in `ASTree.cpp` is the same idea with more friction. The script:

```python
# usage: python rewrite.py in.raw out.raw   (must run under 3.10)
import dis, marshal, sys
code, patched = marshal.loads(open(sys.argv[1], "rb").read()), 0

def fix(co):
    global patched
    bc = bytearray(co.co_code)
    for i in dis.get_instructions(co):          # exact offsets: 3.10 has one-byte opcodes,
        if i.opname == "JUMP_IF_NOT_EXC_MATCH": # so a fixed-stride scan desynchronises
            bc[i.offset] = 115                  # POP_JUMP_IF_TRUE: same stack effect, same sense
            patched += 1
    kids = [fix(c) if hasattr(c, "co_code") else c for c in co.co_consts]
    return co.replace(co_code=bytes(bc), co_consts=tuple(kids))

open(sys.argv[2], "wb").write(marshal.dumps(fix(code)))
print(f"patched {patched}")
```

**Work in a directory that cannot shadow the interpreter.** The app's own directory ships a copy of the
CPython standard library as `.py`, so running `python3` inside it imports *those* files and dies with
`SRE module mismatch` from `import re`. Run everything from outside it, or with `-I`, and keep the output
in a scratch directory — as the numbers above were produced.

## What this buys, and what it does not

`pycdas` (ships with pycdc) **disassembles the file completely: 89,017 lines, zero errors.** That is the
working artefact, and it answers questions reliably — grep a code object's name, read its instructions.
It is what produced the main window's own vocabulary:

```
TransparentApp  WorkshopTile  WorkshopUploadWorker  SpriteSecuenciaWindow  WorkerBridge
'Manager in process'  'Preview'  'No preview'  'Loading...'
```

and its hand-written Steamworks layer (`SteamworksFlat`, `_bind_steam_core`, `_SteamUGCDetailsLatest`,
`Workshop_GetItemUpdateProgress`), which the Rust side has replaced with the Web API and a managed
schedule.

It does **not** give readable source for the whole module. For that, the choice is: fix pycdc's 3.10
exception model, or read the disassembly one question at a time. The second is what has been done so far,
and it has been enough for every question asked.

## Worked example: the MP4 conversion, and what is adjustable in it

Answering this from the disassembly took one grep, and the answer was smaller than expected: the option
lives in a handler and is **hard-coded**, so the honest finding is that the user cannot adjust it at all.

* The main window contains **no conversion code**. `'Convert MP4 to animation'` appears only in
  `_build_tools_page` — a button on the Tools page — and the work happens in a `subprocess` call to the
  standalone tool.
* That tool **is** source we already have: `_internal/mp4ToAnimations.py`. Three pipelines:
  * **MP4 → GIF** through `moviepy`: `clip.write_gif(output_path, fps=fps)`. No palette, no dithering, no
    scale — whatever moviepy decides.
  * **MP4 → WebP** through `ffmpeg` twice: extract `frame_%04d.png` with `-pix_fmt rgba`, then
    `-vcodec libwebp -lossless 1 -q:v 100 -loop 0 -an`.
  * **MP4 → APNG** through `ffmpeg`, same extraction.
* **The knobs are literals.** `start_conversion(..., fps, ...)` takes a frame rate, but its caller writes
  `fps = 30  # Puedes cambiar el FPS si es necesario` — a comment where a control should be. The WebP path
  does not even use that parameter: it passes `-framerate 60` with a comment saying the same thing. And
  `mp4_to_webp`'s docstring documents `quality` and `lossless` parameters that **are not in its signature**;
  both are literals inside the command.
* The ZIP-of-images path is the same story from the other end: `create_video_with_ffmpeg(..., fps=60)`.

For a port, the interesting part is the inverse of that list: frame rate, quality, lossless, scale and
loop count are all things the original decided for the user, and the `ffmpeg` argument lists above are the
part worth copying exactly.

## Worked example: the colour-key background removal

The other side of the MP4 finding. This one **is** called from the main window — its Tools page imports
`RemoveBackWeb` and `eliminarFondo`, so both the colour-key and the AI removal are behind buttons there —
and unlike the MP4 tool it does have controls. What it does with them is the interesting part.

The whole decision is this, once per pixel, in Python, over `.getdata()` (parallelised across frames with
a process pool):

```python
threshold = int((percent / 100) * 255)          # 50% -> 127
if abs(r - bg[0]) <= threshold and abs(g - bg[1]) <= threshold and abs(b - bg[2]) <= threshold:
    alpha = 0                                    # fully transparent, or nothing at all
```

Three consequences worth knowing before copying it:

* **It is a per-channel box in RGB**, not a distance: a pixel is "background" when every channel is within
  the tolerance, so the region it removes is a cube, not a ball. Ideas of "similar colour" that live near
  that cube's corners are kept.
* **The alpha is binary.** There is no partial transparency and no feathering, so a keyed edge is hard.
* **The default is 50%, and 50% is ±127 of 255.** A white background with the default setting removes
  anything within half the range of white on all three channels at once — which is a very aggressive key
  to start from, and the label does not hint at the scale.

The save side is better than the load side, and it is worth copying exactly: for **GIF** it goes through
Pillow with `loop` and `duration` **inherited from the source animation** and `disposal=2`, which means a
re-saved file keeps its timing; **APNG** goes through `ffmpeg` with `-plays 0` — also correct — but
`-framerate 60`, hard-coded, the same flaw as the MP4 tool. Resize is offered at 25/50/75 and a custom
value, and it is a separate save path (`save_gif` versus `save_gif_without_resizing`), so the two do not
agree about which is the default.

For a port: keep `loop`/`duration`/`disposal` inheritance and `-plays 0`; replace the cube with a real
distance in a perceptual space; give alpha soft edges; and pick a default somebody could see the effect of.

## Worked example: the AI removal pipeline (`eliminarFondo`)

The colour-key tool's opposite in every way that matters: this one **does** produce soft edges, and its
troubles are in the plumbing instead. Four stages, all of them visible in 206 lines.

1. **Extract** — `ffmpeg -i in -pix_fmt rgba out/frame_%04d.png`. RGBA is preserved through the whole
   pipeline, which is the thing the colour-key tool also gets right.
2. **Remove** — per frame, one call to `rembg.remove()` with the session made once:
   `post_process_mask=True, alpha_matting=True, foreground=270, background=20, erode=11`. Those are real
   matting parameters, so edges are feathered rather than binary. Source frames are deleted as they are
   read, so the intermediate directory never holds two copies.
3. **Model** — `new_session(name, providers=[CUDAExecutionProvider | CPUExecutionProvider])`, the provider
   picked from the detected device. The **name comes from the chosen file's basename**, so renaming the
   model file changes which model loads, and the file picker opens at a hard-coded
   `C:/Users/<user>/.u2net`.
4. **Encode** — GIF through Pillow with `loop=0` and **`duration=100` as a default parameter**, so the
   source animation's timing is *not* inherited; APNG through `ffmpeg -plays 0`; WebP through
   `ffmpeg -c:v libwebp_anim -lossless 1 -qscale 100`.

### The two things a port should not copy

* **`-framerate 60`, again.** This is the third tool in a row to hard-code it. The GIF path has its own
  version of the same problem: a default `duration=100` where the source file already knows the answer.
* **`-qscale 100` is not a libwebp option.** It is ignored rather than honoured, so "maximum quality" on
  the WebP path is a comment, not a setting — and nothing in the UI would tell you.

### The thing that looks like a mistake and is not

`process_frames` runs the model **sequentially**, where its sibling `RemoveBackWeb` spreads its (much
cheaper) per-pixel work over a process pool. That is not an oversight: an ONNX session belongs to the
process that created it, so a pool would either need one session per worker — loading the model N times —
or would serialise anyway. Sequential inference with one session is the correct shape for this pipeline,
and it is worth writing down so the next reader does not "fix" it.

## Worked example: the Tk MDAA manager, and the two generations of `.mdaa`

`advancedCodeAnimations.py`, 483 lines, Tk. It is the predecessor of `AnimaTriggers`, and the part worth
knowing is that **the two do not share a format** — only the extension.

### What the old one does

A `.mdaa` is a ZIP with exactly three members: `miniatura.png`, `core.exe`, `actions.mddo`. Loading one:

* writes the chosen path into `mdaaStart.txt` (which is what the app reads to restore the last file),
* unzips it to a temp directory and shows the thumbnail,
* **runs `core.exe`** — an executable carried inside the bundle,
* then polls `actions.mddo`, gated by an `activador.act` flag, and for each line
  `data = eval(line)`, then hands `url` and `position` to `execute_function`, which does
  `coords(eval(position.split("-")[1]))`.

Creating one is the mirror image: pick a thumbnail, pick a `core.exe`, pick an `actions.mddo`, zip them
with those names. There is no manifest, no version and no description of what the bundle does.

### The three findings

1. **The extension is shared and nothing else is.** The modern `.mdaa` (`AnimaTriggers/_internal/mdaa_io.py`)
   holds `pack.json`, `assets/`, `frames/regionN.npz` and `meta.json` — structured data, no executable.
   So "read a `.mdaa`" is an ambiguous request: the answer is the generation, not the file.
2. **The old format is partly a plugin bundle.** Loading it **executes code that arrived inside it**, so
   the file is closer to a program than to a document. That is the real reason the new format is better,
   and it is worth stating plainly rather than as a matter of taste.
3. **The interpreter is fed file contents.** `eval(line)` on `actions.mddo`, and again on the coordinate
   text inside `position`. Anything that produces these files can run whatever it likes in the app.

### Files that are the old format's state

`mdaa.txt`, `mdaaStart.txt`, `activador.act` — the last-touched bundle, the auto-restore path, and the
activation flag. The new generation keeps the equivalent in `user_data/mdaa_registry.json` with an
enable/primary/autorun trio, which is a schema instead of three loose files.

### What it is worth porting

The `mdaaStart.txt` behaviour — restoring the last bundle on launch — is a real convenience and the new
generation does not obviously have it. Everything else here is superseded, including the duplicated
implementation: `animacionesAvanzadas` and `start` are two near-identical copies of the same feature in
one file, differing in how many callbacks the caller passes in.

## The map: what the main window is made of

Neither the disassembly nor the sources answer this quickly; the **shipped translation catalog does**. It
holds **793 entries across 31 languages**, and its keys are the UI's own vocabulary, so the labels are the
inventory. Cross-checked against the page builders that exist as code objects, the main window is:

* **Pages** — `_build_library_page`, `_build_apps_page`, `_build_tools_page`, `_build_workshop_page` with
  four sub-pages of its own (`_workshop_public_page(s)`, `_workshop_steamworks_page`, `_ws_build_upload_page`),
  `_build_discover_grid_page`, and a "side" action set (`_build_side_resize_page`, `_build_side_rotate_page`,
  `_build_side_audio_page`, `_build_side_detail_page`).
* **Tools page — five real entries.** The bytecode's own imports name them all: `mp4ToAnimations`,
  `RemoveBackWeb`, `eliminarFondo`, `advancedCodeAnimations`, and Piskel (a web editor opened at it).
* **Umbrella.** The window also *launches sibling executables* — `AnimaProfiles.exe` and `AnimaTriggers` —
  and has a label for failing to find them, so it is a hub for the whole suite rather than one app.
* **Overlay.** `monoModule` is imported from the window itself, which is where the transparent-window
  renderer is driven from.

### What the catalog exposes that the code does not

* **A dead entry.** `MP4 to Gif converter no losses performance (BETA)` has **no occurrences in the
  bytecode at all** — the label ships, the feature does not. (`mp4ToGif.py`, whose name suggests it is the
  equivalent, is in fact an APNG player for a hard-coded file: dead code with a misleading name.)
* **Two labels, one implementation.** `Animation remove background (IA TOOL)` and
  `MP4 background remover (IA TOOL)` both route to the single `eliminarFondo` import, which takes any video
  through ffmpeg — the split is a menu decision, not an engine one.
* **A feature with no counterpart in the port.** `SendVideoRequestWindow`: "Here you can send video links
  for Luis (AnimaEngine creator) to process and share with the community." Roughly, a request queue to the
  author, which is both a support channel and a content pipeline, and the kind of thing that quietly
  disappears in a rewrite unless it is written down.

### The parts that are already covered

Library browsing, the overlay runtime, the workshop browser with its three-tier fallback, and the
translation catalog all exist in the Rust window. The **Tools page is the gap**: conversion, both kinds of
background removal, and the MDAA manager have no equivalent there yet — which matches what the two worked
examples above found, and is the honest summary of what this file was about.

## Worked example: what `SendVideoRequestWindow` actually sends

The answer is smaller and stranger than the question. The window is a `QLineEdit` with the placeholder
*"Paste the video URL here..."*, a `Send` button, and one call: **`send_to_firestore`**. That name appears
**twice in the whole 373 KB entry script** — once as a string inside a tuple of candidate method names,
once as `LOAD_ATTR` — and **nowhere as a definition**.

What that means, and how it was checked:

* The call is **by name, at run time**. The module deals in tokens read from files (`tokens`, `my_token`,
  `_anim_token`, `_ws_upload_token`, `workshop_api_key.txt`), and the tuple that `send_to_firestore`
  appears in is a list of names to try on an object it was handed. So the implementation is deliberately
  not statically visible from the caller.
* It is **not** in the loose `_internal/*.py` sources either: the only files there that mention firestore
  are `firebase_admin/firestore.py` and `firestore_async.py`, which are the **vendored official Admin SDK**.
  The SDK being bundled is the real finding — the sending half authenticates as the *project*, not the user.
* `pyinstxtractor` extracts the PYZ (3,913 modules) and finds `send_to_firestore` in exactly the same
  `AnimaEngineKIVI` module, byte-for-byte the entry script again. Dumping it with `pycdas` gives no
  `Object Name: send_to_firestore` either. The definition is not in this build.

### The two `.Lim` files, which are not what their names suggest

They sit in the app directory, two and four bytes long, and they are **constants, not counters**:

```
limitRecur.Lim -> 5000
refres.Lim     -> 10
```

A recurrence limit of 5000 and a refresh value of 10, shipped as plain text, read by a build whose other
state is JSON and `.acf` files. Whatever they gate, the numbers are the author's policy, not user settings.

### What this means for a port

**This feature cannot be reproduced.** Firestore with the Admin SDK needs the project's own
service-account credentials, so the send path belongs to the author's Firebase project and to nobody else.
A rewrite has three honest options: run its own endpoint for the same purpose, keep the feature to "open
the Discord/Steam page instead", or drop it — but it cannot be copied, and it should not be *silently*
dropped either, because the window's own text says what it is for: *"send video links for Luis
(AnimaEngine creator) to process and share with the community"*, which is both a support channel and a
content pipeline.

## Worked example: the GPU engine that never shipped

`nuevoMotor.py` ("beast_overlay_engine", 844 lines) is the ambitious one, and the first thing to say about
it is that **nothing imports it**. It has no references in the main window's bytecode (`monoModule`, the
raster overlay that *does* ship, appears twice) and no loose source imports it either. Whatever follows is
research that stayed in the file.

### What it does about smoothness

The engineering is in the pipeline shape rather than in OpenGL calls:

* **A decode worker thread** (`DecodeWorker`) composes the animation's delta frames into full frames once,
  then emits `QImage` objects — with the per-pixel **alpha mask kept as a separate `bytes` buffer**, which
  is what makes `alpha_opaque_at(x, y, threshold)` possible: click-through decided *per pixel*, not per
  window.
* **A bounded buffer** of decoded frames (`buffer_frames: int = 12`) between the decoder and the painter,
  driven by a `Qt.PreciseTimer`. Decode and presentation are decoupled, so a slow frame does not stall the
  clock.
* **`premultiply_alpha`**, on by default, with a comment recording the bug it fixes: without it the overlay
  "dulls the colours" — the classic halo/darkening that comes from treating straight alpha as
  premultiplied. `smooth_alpha` with a 0.85px blur does the feathering the colour-key tool never had.

### What it does not do

`paintGL` is not an OpenGL renderer. It paints with a `QPainter` — `QImage` → `QPixmap::fromImage` →
`drawPixmap` — which on a `QOpenGLWidget` does end up GPU-composited, but uploads a fresh texture every
frame and gives up all control over the upload. There are no shaders, no `glTexSubImage`, no PBO, no
texture reuse. It is a GL-backed widget used as a canvas, not a renderer, and the **two image conversions
per frame** are the concrete cost of that.

It also re-clears to transparent with `CompositionMode_Source` before every paint, annotated "FIX
stacking" — a real ghosting bug it hit and fixed, and worth knowing because the same artifact appears in
any overlay that forgets it.

### The configuration is the interesting artefact

`BeastConfig` is a better specification than anything the shipped overlay has: fps, alpha blur radius,
premultiply toggle, hit-test threshold, topmost, click-through, fullscreen, **span all monitors**,
`scale_to_fit` (off by default, and the comment says so — the same conclusion the Rust overlay reached from
the other direction with `MAX_SCALE_PERCENT = 100`), `center_when_native`, hover-hide with
`focus_fade`/`smart_dodge`/`snap_to_edge`, a 24 ms hover interval, three alpha levels
(hidden/faded/normal), an 800 ms topmost watchdog, buffer depth, and a decode scale with a chosen resample
filter.

For a port, that list is the useful output of this file: it is the feature set four hover behaviours deep
that nobody documented anywhere else.
