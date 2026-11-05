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
