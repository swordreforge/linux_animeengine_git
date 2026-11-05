# Rust performance: do / don't

The same code, read against a different book: [*The Rust Performance Book*](https://nnethercote.github.io/perf-book/)
(Nicholas Nethercote et al.), whole — Introduction through General Tips and Compile Times, 890 extracted lines.

This is the companion to `docs/rust-guidelines.md` and inherits its rule, which is why the document exists:
**quote only a number produced by a command in this tree.** The book is the source of the *rules*; this
repository is the source of the *numbers*. Where a rule has a number here it is cited; where it does not, the
row says **unmeasured** rather than guessing. Where a book technique does not fit an application, it is in
*Deliberately not applicable*; where the repository has data against it, it is in *Measured, and decided against*.

**Scope.** An application, not a library: two crates, five binaries, eight examples, Linux only. The `lib` target
is used by our own binaries and no strangers, so a technique that exists to make a library cheap to *other*
people does not apply.

**How to check any of this.**

```text
cargo build --features studio
cargo run --release --features overlay --example frame_bench     # the overlay's per-frame numbers
cargo run --release --example alloc_bench                        # add --features mimalloc for the A/B
cargo clippy -p anima-engine-rs -p anima-ai --all-targets --features overlay,bc7,dbus,studio,workshop-web,steamworks
```

**Linting is closed.** The book says at line 297 that the rest of it "will not mention performance problems
that Clippy detects by default", and this tree is clean under that command — zero lints across both crates and
all targets. So everything Clippy catches is already caught, and nothing below repeats it; a reader should not
re-walk that class of finding.

## Build configuration

The book's closing line on this chapter is "Benchmark all changes, one at a time", and that is the status of
almost every row here: this workspace has **no `[profile.release]` at all** (the root `Cargo.toml` and
`crates/anima-engine-rs/Cargo.toml` carry only `[features]` and the default profile), so every runtime-speed
flag the chapter offers is a candidate, none is measured, and none is set.

| | Do | Don't |
|---|---|---|
| **Release build** | Run the shipped binaries and the benchmarks with `--release`; the dev profile omits the optimizations every number in `docs/rust-guidelines.md` assumes. `examples/frame_bench.rs:4` prints the release invocation in its own header | Compare a dev number against a release one |
| **`codegen-units = 1`** | Try it and re-time `frame_bench` before/after if you want it. **Unmeasured** — no `[profile.release]` exists to hold it | Set it because the book says "one" improves runtime speed, without a before/after on this machine |
| **`lto = "thin" \| "fat"`** | Same treatment: the book's "10–20 % or more" is about other programs. **Unmeasured** | Assume a percentage from the book lands on a wgpu/iced desktop binary |
| **`-C target-cpu=native`** | — (deliberately not done) | Target a CPU we do not know. `docs/rust-guidelines.md`'s *Deliberately not applicable* and `M-TARGET-CPU` already settled this: a desktop binary ships to unknown CPUs, so the baseline target is correct, and portability is the cost the book warns about |
| **`panic = "abort"`** | It is *eligible*: neither crate calls `catch_unwind` (grep: none) and `unwrap`/`expect` is the house style, and the book's condition is precisely "if you do not need to unwind". **Unmeasured** — not set anywhere | Set it without checking a dependency does not rely on unwinding |
| **Faster linker (`lld`/`mold`)** | Nothing to do: the book notes `lld` has been the Linux default since Rust 1.90, and this machine builds with rustc 1.99.1 — the flag would be a no-op | Add `-C link-arg=-fuse-ld=` for a compile-time win the default already gives |
| **PGO / BOLT** | Only if a profile ever shows the binary is hot and a training pass is possible; the book also notes PGO is incompatible with `cargo install` distribution. **Unmeasured**; no `cargo-pgo` in the tree | Adopt it for the book's "10 % or more" |
| **Dev-profile `debug`** | Consider `[profile.dev] debug = false` if dev builds feel slow; the book says 20–40 % off *their* builds. **Unmeasured** | Set it and then be surprised that stack traces lose lines |
| **Alternative allocator** | **Measured and rejected** — see *Measured, and decided against* | Adopt mimalloc/jemalloc on the book's "large improvements … in practice" without measuring this workload |

## Allocations

| | Do | Don't |
|---|---|---|
| **Reserve when the size is known** | `Vec::with_capacity` / `String::with_capacity` where the count is known: `crates/anima-ai/src/preprocess.rs:27` (the 1024×1024×3 tensor), `src/cache.rs:346,355`, `src/anim/decode.rs:190`, `src/bin/anima-overlay.rs:315` (the menu's vertex list), `src/control.rs:28`, `src/cache.rs:322` | Grow a buffer from empty when the length is known first |
| **Reuse a workhorse collection** | Hoist a `Vec` out of a loop and `clear()` it, or keep it in the struct, when the loop runs per frame | Allocate a fresh `Vec` inside a hot loop: `src/anim/region.rs:102` builds `runs` from `Vec::new()` once per scanned row, inside `for gy in 0..rows`. That loop **is** the overlay's measured per-frame cost (1.442 ms per rebuild at step 1, `docs/rust-guidelines.md`), so it is the one allocation site with a number attached — see *Outstanding* |
| **Share only what is shared** | `Arc<Animation>` (`src/bin/anima-overlay.rs:710`) is shared by the render loop, the cache writer and the hit-test — a real share, and the book's `Rc`/`Arc` warning is about values that are *not* | Reach for `Rc`/`Arc` for a value used once. There is no `Rc` and no `RefCell` anywhere in `crates/anima-engine-rs/src`, so that failure mode is absent by construction |
| **`clone_from` into a live buffer** | — (nothing needs it; no hot path clones into a reusable target) | Blanket-de-clone on a count. **176 `.clone()` across 22 files, 55 of them in `src/bin/anima-studio.rs`**; the window's `view` (`src/bin/anima-studio.rs:2382`) is not the per-frame path this project measured — the overlay's is — and most of those clones are of an `image::Handle` (a refcount bump), a short title `String`, or a `PathBuf`. What would have to be measured is the *window's* redraw cost, which nothing here measures |

## Hashing

| | Do | Don't |
|---|---|---|
| **A faster hasher only when hashing is hot** | The book's condition is "if profiling shows that hashing is hot". Nothing here does: every map is cold — the i18n catalogue load (`src/i18n.rs:18`), the per-tick thread table (`src/telemetry.rs:106,294`), and the shepherd registry (`src/shepherd.rs:157`). `docs/rust-guidelines.md`'s `M-FAST-HASHER` records the same decision | Add `rustc-hash`/`ahash` on the strength of the book's rustc numbers (6 %, 1–4 %, 4–84 %); those measure rustc, not this |
| **Enforce an alternative only after adopting one** | — | Add `disallowed-types` to a `clippy.toml` before there is an alternative to disallow; there is no `clippy.toml` in the tree |
| **`nohash_hasher`, byte-wise hashing** | — | Apply a randomised-integer-key or `ByteHash` technique; every key here is a `String` or a path, so neither is a fit |

## Standard library types

The chapter (read whole) covers `Vec::swap_remove` over `Vec::remove`, `Vec::retain`, `Option::ok_or_else`
over `ok_or` (and the same for `Option`/`Result` combinators), `Rc::make_mut`, and `parking_lot`. It does
**not** advise `sort_unstable_by`, so the six `sort_by` sites are not a finding from this book and are not
listed here.

| | Do | Don't |
|---|---|---|
| **`Vec::retain` to drop many items in place** | `src/state.rs:161` and `src/bin/anima-shepherd.rs:240,246,292,305` remove a set of items in one pass | Drain a `Vec` element by element where `retain` expresses it |
| **`swap_remove` when order is irrelevant** | — | Reach for it blindly. The tree has one front-removal — `src/ffmpeg.rs:144`, `args.remove(0)` — on a short argv-style list where order *is* the point, so `swap_remove` does not apply |
| **`ok_or_else` when the error is built** | `src/pack.rs:286,297,363` and `src/workshop.rs:185` build the error inside `ok_or_else`; the examples' `ok_or("usage: …")` (`examples/cache_bench.rs:18`) is a `&'static str` and free | `ok_or(Error::X(format!(…)))`, which formats even on the `Ok` path |
| **`parking_lot` only if measured** | — | Swap std `Mutex`/`OnceLock` for `parking_lot` without a measurement. The three sync sites — `src/log.rs:41`, `src/bc7.rs:56` (both `OnceLock` init guards) and `src/shepherd.rs:450` (a test mutex) — are not contended, and the book itself says the std types have "greatly improved" and to measure first |
| **`Rc::make_mut` clone-on-write** | — | Introduce shared-mutable state to use it; the tree shares immutably (`Arc`) and has no `Rc` |

## Iterators

| | Do | Don't |
|---|---|---|
| **`iter().copied()` for small `Copy` items** | `crates/anima-ai/src/preprocess.rs:38` (a `fold` over `f32`) and `crates/anima-ai/src/session.rs:64,65,90,91` | Hand a `&f32` to a consumer that wants the value |
| **`filter_map` over `filter`+`map`** | `src/telemetry.rs:64,242,324`, `src/control.rs:196,268,304,498`, `src/shepherd.rs:380,387,418`, `src/bin/anima-workshop.rs:192,419` | `filter(..).map(..)` where one `filter_map` says it |
| **`extend` over collect-then-append** | `src/anim/ink.rs:44` (`extend_from_slice` per row band), `src/bin/anima-studio.rs:2050,2941`, `src/i18n.rs:349,443`, `src/steamcmd.rs:287` | `collect()` a `Vec` only to `append` it — no site does this |
| **`chunks_exact` only when the size divides** | — | Change `src/bin/anima-studio.rs:1396`'s `.chunks(MAX_IDS_PER_REQUEST)` to `chunks_exact`: the id count rarely divides and this is one batched network request, not a hot loop |
| **`size_hint` on a custom iterator** | — | (There is no custom iterator to give one to) |

## Inlining

| | Do | Don't |
|---|---|---|
| **`#[inline]` on small cross-crate accessors, then re-measure** | There are **zero `#[inline]` attributes in either crate**, and the tree *is* split across a crate boundary — `frame_bench` and the overlay both call `anim::region` — which is exactly the cross-crate case the book names. Candidate: `src/anim/region.rs:14,49`; **unmeasured** | Sprinkle `#[inline(always)]` on a hunch. The book says the effects can be "unpredictable" and to measure again after each, and that inlining is non-transitive |
| **`#[cold]` for rare paths** | — | Mark anything cold without a profile that says the path is rare; nothing here is |

## Bounds checks

| | Do | Don't |
|---|---|---|
| **Keep safe access; let the compiler elide the check** | `src/anim/region.rs:22` and `crates/anima-ai/src/preprocess.rs:77` use `get(..).copied().unwrap_or(..)` — checked and total, no panic | Reach for `get_unchecked` "as a last resort" before there is a measurement. There is no `get_unchecked` in the tree, and `unsafe` is confined to FFI (`src/bc7.rs`, `src/steam.rs`, `src/bin/anima-overlay/gpu.rs`), as `M-UNSOUND` requires |
| **Rewrite an indexing loop to iteration where the shape allows** | `src/anim/ink.rs:42-44` iterates rows and slices each span rather than indexing inside a double loop | Rewrite `crates/anima-ai/src/preprocess.rs:46-49` (`tensor[channel * plane + index]`, `pixels[index * 3 + channel]`) for its own sake — it is in the AI stages measured at 2.8 % of the pipeline, so it is not a candidate. The scan at `src/anim/region.rs:21` indexes because it maps surface coordinates to native ones, which an iterator cannot express |

## Compile times

| | Do | Don't |
|---|---|---|
| **`cargo --timings` before touching the crate graph** | Your own build with `--timings` prints an HTML Gantt chart of the two crates and five binaries; **unmeasured**, because the compile in `build --features studio` finished in 8.42 s here and nobody has complained | Guess which crate serializes the build |
| **Cut generic IR bloat only if `cargo llvm-lines` shows it** | `Option::map` / `Result::map_err` are used pervasively (`src/pack.rs`, `src/workshop_web.rs`, `src/steamcmd.rs`); the book says replacing a famously-instantiated one with `match` can help compile time *and* binary size. **Unmeasured** | Rewrite every `map_err` into a `match` for a compile time nobody has measured |
| **Macros** | Nothing to do: the tree has no `macro_rules!` (grep: none) and `docs/rust-guidelines.md` records "we have no macros, by choice" | Run `-Zmacro-stats` / `cargo-expand` on a codebase with no macros |

## General

| | Do | Don't |
|---|---|---|
| **Optimize only what is hot** | The two measured hot paths both say *leave it*: the AI CPU stages are 2.8 % of the pipeline and the overlay's region scan is 2.9 % of wall time at 480×270 (28.8 ms/s) and 3.1 % at 370×370 (31.2 ms/s) — `docs/rust-guidelines.md` | Touch a path nobody has profiled; the book's own framing is "it is only worth optimizing hot code" |
| **Prefer an algorithmic win to a low-level one** | The region scan's cost is its rectangle count (128–273 on real clips) against a 768 soft budget, i.e. a shape/algorithm question | Micro-tune the leaf functions of a path whose *shape* is the cost; the book puts algorithms and data structures ahead of low-level tweaks |
| **Lazy / on-demand work** | The frame cache is keyed and LRU'd (`src/cache.rs`); the studio's 2 s poll is a safety net behind a `notify` watcher; the D-Bus interface reads the registry per call | Do per frame what an on-change read would get |
| **Handle the small/common case directly** | The scan merges equal rows before emitting (`src/anim/region.rs:125`), so a solid shape is one rectangle, not one per row | Emit a rectangle per row and merge later |
| **Comment an optimization with the measurement behind it** | `examples/frame_bench.rs:1-21` and `examples/alloc_bench.rs:1-16` do exactly this, and `docs/rust-guidelines.md` cites the command beside every number it quotes | Leave a "this is faster" comment with no number behind it |
| **Measure, then write the number** | The rule this document and its sibling are built on: a percentage from a document or from memory is not evidence. See `docs/code-audit.md` for why | Quote the book's own figures as if they were ours |

## Deliberately not applicable

Techniques from the book that do not fit this program. The reason is given each time; where the reason is a
judgement rather than a measurement, it says so.

| Technique (chapter) | Why not |
|---|---|
| **Shrinking hot types, `SmallVec`/`ArrayVec`, `smartstring`** (Type Sizes, Heap Allocations) | The hot data here is a handful of *large* buffers — 518 kB frame buffers and a 12 MiB model input (`examples/alloc_bench.rs`:30-32) — not many small short-lived objects. *Judgement, **unmeasured**.* The one exception is `runs` in `src/anim/region.rs:102`, which genuinely *is* a short vector, and it is in *Outstanding* |
| **Alternative hashers, byte-wise hashing, `nohash_hasher`** (Hashing) | Every map is on a cold path (see the Hashing rows), and every key is a string or a path |
| **`Cow` for mixed borrowed/owned** (Heap Allocations) | No hot path builds a mixture; the UI strings are short and constructed once |
| **`get_unchecked`, hand-written SIMD, `core::arch`** (Bounds Checks, Machine Code) | The safe forms are enough and `unsafe` is confined to FFI by project rule (`M-UNSOUND`). Nothing measured is hot enough to justify leaving safe Rust; the largest share is 3.1 %. *Judgement.* |
| **Thread parallelism — `rayon`, `crossbeam`** (Parallelism) | The overlay is latency-bound on a single frame and already runs decode and cache work on their own threads; the AI path is one frame at a time. *Judgement.* |
| **PGO / BOLT / `cargo-pgo`** (Build Configuration) | The binary ships to unknown CPUs and is not distributed through `cargo install`. *Judgement*, and **unmeasured** — see the Build configuration row |
| **Wrapper-type grouping, `Arc<Mutex<(A, B)>>`** (Logging and Debugging) | No hot pair of lock-wrapped values exists; there are three sync sites, none contended |
| **Reading input as raw bytes, `read_until`** (Bounds Checks / I/O) | Nothing reads a byte-oriented line stream |
| **Hand-inspecting machine code — `cargo-show-asm`, Compiler Explorer** (Machine Code) | Nothing is hot enough, and the one hot path that *is* small (the region scan) is already measured end to end. *Judgement.* |

## Measured, and decided against

Where the repository has data that says a book technique does not pay here.

**1. Region coarsening (`ANIMA_REGION_STEP=auto`) — the knob makes it slower.** Measured on the largest clip in
the library, a 183 MB APNG, at the instance's persisted 40 % scale, twelve seconds each, with the `Debug` events
added in commit `20210c0`:

| `ANIMA_REGION_STEP` | step seen | rects (mean) | scan | install | total |
|---|---|---|---|---|---|
| `off` (the default) | 1 | 1101 | **10.51 ms/s** | 0.98 ms/s | 11.49 ms/s = 1.1 % |
| `auto` | 1 and 2 | **548** | **15.75 ms/s** | 0.67 ms/s | 16.42 ms/s = 1.6 % |

Coarsening halves the rectangles and makes the scan *slower*, because `apply_opaque_region` runs a full scan,
finds the count over the soft budget, and runs it again at the coarser step: on any frame over 768 rectangles
the saving has to cover a scan whose result was thrown away. The policy is worth revisiting rather than
disabling — deciding the step from the *previous* frame's count would cost one scan — but as it stands, leaving
it off is measured to be better, and this is the opposite of what the knob is for. Every figure is per clip
*and* per scale: the same clip at another `scale_percent` gives another number, so these are not comparable
across clips.

**2. mimalloc as the global allocator.** The book recommends an alternative allocator for applications and
quotes "large improvements … in practice"; Microsoft's guideline quotes "up to 25 %". That describes someone
else's workload, so `examples/alloc_bench.rs` measures ours. Intel Core Ultra 7 155H, glibc 2.44, rustc 1.99.1,
mimalloc 0.1.52, best of fifteen rounds (`docs/rust-guidelines.md`; also commit `0744bd1`):

| case | system | mimalloc |
|---|---|---|
| 50,000 short-lived buffers | 3.3 ms | **0.8 ms** (4.1x faster) |
| 200 × 480×270 RGBA frames | 2.6 ms | 2.6 ms |
| 20 × 12 MiB f32 buffers | **9.3 ms** | 10.6 ms (1.14x slower) |
| 2,000 vectors grown from empty | 1.1 ms | 1.2 ms |
| 4 threads × 50,000 buffers | 4.6 ms | **2.2 ms** (2.1x faster) |
| peak RSS | 41,588 kB | **31,588 kB** (24 % lower) |

It is faster on many small short-lived allocations and on cross-thread contention, and slower on large blocks —
which is what this program's per-frame work is. What settles it is the arithmetic, not the ratios: the
small-object win is 2.5 ms over 50,000 allocations, about **50 ns each**, so a frame making a hundred of them
saves **5 µs** of a 16.7 ms budget, while the same frame is dominated by the allocation mimalloc is slower at.
The `mimalloc` feature and the benchmark are kept, off by default (`crates/anima-engine-rs/Cargo.toml:33`); the
studio's Iced element tree is the shape mimalloc is *supposed* to win and `alloc_bench` does not measure it — if
a profile ever points there, run `alloc_bench --features mimalloc` and time the window, which costs one build.

**2. Optimizing the AI removal's CPU stages.** `examples/ai_frame_bench.rs` on 1280×720 frames
(`docs/rust-guidelines.md`): `preprocess` **82.03 ms**, `mask_from_output` **50.97 ms**, `apply_mask`
**1.96 ms**, total **134.95 ms** per frame. Against the ~4.9 s per frame the whole pipeline took when it was
verified against rembg, those three stages are **2.8 %**. The model is the cost; optimising our own code here
would not change what a user waits for. The useful shape of a benchmark is that it stops work as often as it
starts it.

**3. Optimizing the overlay's region scan.** `frame_bench` shows the scan is **1.442 ms** per rebuild at step 1
(28.8 ms/s at 60 fps), six times the **0.229 ms** texture upload (1.4 % of a frame). But three real workshop
clips produce **128–273 rectangles** against a **768** soft budget, so `Auto` picks **step 1 every time** and the
coarsening machinery — the doubling loop, the `max_step` bound — never runs. At the worst case (370×370) the scan
is **3.1 % of wall time**. The knob if that ever matters is `ANIMA_REGION_SOFT_RECTS`; at 256, the 264-rectangle
clip would move to step 2 and 8 ms/s instead of 31, paid for in two-pixel-batch input accuracy. Worth trading
only if the 3 % shows up somewhere.

*(Reproduced 2026-11-05 on the same machine, Intel Arc MTL via Vulkan: step 1 **1.473 ms**, upload **0.216 ms** —
within noise of the recorded figures above, so the recorded table stands.)*

The memory split is worth one line here because it is the number a person would reach for: the running overlay
reads **118 960 kB** as `VmRSS` but **48 267 kB** as `Pss` (`131 924` / `37 554` for the window; commit
`3b35d81`), and a `Pss` of ~48 MB is not what an allocation tweak on a 16.7 ms frame budget moves. The GPU
figure is the same story: two live overlays read **0.7 %**, which matched the raw `drm-engine-*` counters
(**0.77 %**) over the same interval (commit `f1477bd`).

## Outstanding, in the order worth doing

Cheapest first. Each says what it would take and what would have to be measured to accept it.

1. **Reuse the per-row `runs` vector in the scan** (`src/anim/region.rs:95-102`). *Take:* hoist `runs` above
   `for gy in 0..rows` and `clear()` it each row (or make it a small fixed-capacity `SmallVec<[(u32, u32); 16]>`,
   which is the one place the book's *short `Vec`s* advice fits). *Measure:* `frame_bench` step-1 ms per rebuild
   with the rect counts unchanged — accept only if the cost drops at the same silhouette.
2. **Pre-size `band`/`rects`** (`src/anim/region.rs:95-96`): `band` can reserve `cols`, which is known one line
   earlier. *Take:* one `with_capacity`. *Measure:* the same benchmark.
3. **Try `#[inline]` on the cross-crate scan accessors** (`src/anim/region.rs:14,49`). *Take:* an attribute each.
   *Measure:* Cachegrind to see whether inlining actually changed, plus `frame_bench`. The book warns the effect
   can be zero or negative, so this is a try, not a fix.
4. **Try the build-configuration flags, one at a time, against `frame_bench`** — `codegen-units = 1`, then
   `lto = "thin"`, then `panic = "abort"`. *Take:* a `[profile.release]` in the root `Cargo.toml` (none exists
   today). *Measure:* `frame_bench` before and after each, changing one flag per build, as the book's summary
   says. Accept each on its own number, not on the chapter's.
5. **Only if a profile ever shows the window:** measure the studio's redraw, where the 55 clones and the Iced
   element tree live. *Take:* a `perf` run of `cargo build --features studio` (or the `mimalloc` feature, re-run
   against the window). *Measure:* the window's own frame time, which nothing here measures — the numbers this
   document quotes are the **overlay's**.
6. **Compile-time work, last.** *Take:* `cargo --timings` and `cargo llvm-lines`, one command each. *Measure:*
   the Gantt chart and the IR line list; touch generic code only if a number says to. Nothing has measured
   compile time here beyond the 8.42 s `build --features studio` on a machine where the tree was already warm.

## Coverage

**Chapters read, in full, from the 890-line extraction:** Introduction; Benchmarking; Build Configuration
(Release Builds, Codegen Units, LTO, Alternative Allocators, CPU-specific Instructions, PGO, opt-level, Abort on
panic, Strip Symbols, linkers, Dev debug info, Parallel Front-end, Cranelift, Custom profiles, Summary);
Linting; Profiling (profilers, debug info, frame pointers, symbol demangling); Inlining (attributes, simple and
harder cases, outlining); Hashing (alternative hashers, byte-wise hashing); Heap Allocations (Box, Rc/Arc, Vec
short/long, String, clone/`clone_from`, Cow, reusing collections, reading lines, alternative allocator); Type
Sizes; Standard Library Types; Iterators (collect/extend, chaining, chunks, copied); Bounds Checks (including
the I/O locking/buffering and line-reading sections); Logging and Debugging (including wrapper types); Machine
Code; Parallelism; General Tips; Compile Times (`--timings`, macros, LLVM IR).

**Chapters not read:** none — every chapter in the extraction was read.

**Caveat on the extraction.** It is `pdftotext`-style, and it mangles tables: the Standard Library Types and
Heap Allocations chapters come through as reflowed table cells rather than sentences, so their content was
recovered item by item (`swap_remove`, `retain`, `ok_or_else`, `make_mut`, `parking_lot`; `SmallVec`,
`ArrayVec`, `with_capacity`, `Cow`, workhorse collections). If a technique is not named in this document, treat
it as **not read in a form worth attributing**, not as absent from the book.

**One finding checked and dropped.** The six `sort_by` sites were verified to exist (`src/telemetry.rs:175`,
`src/shepherd.rs:338`, `src/i18n.rs:478`, `src/pack.rs:289`, `src/bin/anima-shepherd.rs:454`,
`src/bin/anima-overlay/wayland.rs:16`, and all of them sort by an explicit key). They are **not** listed above,
because the Standard Library Types chapter in this extraction does not advise `sort_unstable_by` — recommending
it would attribute to the book a rule it does not state. There are also two `sort_by_key` sites
(`src/cache.rs:157`, `src/bin/anima-studio.rs:3072`) checked the same way.
