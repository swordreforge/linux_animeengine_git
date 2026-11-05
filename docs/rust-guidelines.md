# Project conventions

Our adoption of Microsoft's [Pragmatic Rust Guidelines](https://microsoft.github.io/rust-guidelines/) (2026.6),
reduced to what this project actually needs to decide. Every rule below is either something we already do
correctly and should keep, or something we are measurably not doing yet.

**Scope.** These are application rules, not library rules. The `lib` target of this crate is used only by our
own binaries, which `M-APP-ERROR` explicitly puts under the application rules. That leaves out the entire
Library / Interoperability, UX, Resilience and Building sections — they exist for crates consumed by
strangers, and ours has no strangers.

**How to check any of this.** Run these before saying something is true:

```text
cargo test -p anima-engine-rs --features overlay,bc7,dbus,studio,workshop-web,steamworks
cargo clippy -p anima-engine-rs --all-targets --features overlay,bc7,dbus,studio,workshop-web,steamworks
ANIMA_FN_LIMIT=100 cargo test -p anima-engine-rs --test function_length -- --nocapture
```

Counts quoted below are from the commit that added this file. They are here to be worked down to zero, and
should be re-measured rather than trusted.

## Lints and toolchain

| | Do | Don't |
|---|---|---|
| **M-LINT-OVERRIDE-EXPECT** | Silence a lint with `#[expect(clippy::x, reason = "why")]`, which fails when the lint stops firing | Use `#[allow]` outside generated code — it goes stale silently. *(both former sites are dealt with: one fixture was rewritten so it no longer needs the lint, the other is an `#[expect]` carrying its reason)* |
| **M-MSRV** | Declare `rust-version` in `Cargo.toml`, and find the number by running a `cargo +<older toolchain>` command: cargo names every dependency that refuses that toolchain, and the highest refusal is the floor | Derive it from the language features you use. Ours is 1.92 in `anima-ai`, set by `ort` (ONNX Runtime) — not the 1.88 that let-chains and `is_multiple_of` suggest, which is what I wrote first. The engine is 1.88, and only reaches 1.92 through the `studio` feature that pulls the AI crate in, which is the point of having two numbers |
| **M-STATIC-VERIFICATION** | Keep `cargo clippy --all-targets` at zero for every feature set we ship. The function-length ratchet is part of it | Add a binary without adding it to a feature set that CI builds |

## Errors

| | Do | Don't |
|---|---|---|
| **M-APP-ERROR** | `thiserror` types in library code, one application-level type at the edges. We use `Box<dyn std::error::Error>` in the binaries | Mix several application-level error types, or invent a new one per binary |
| **M-PANIC-ON-BUG** | `unwrap`/`expect` for invariants and bugs, where the alternative is returning an error nobody can act on | `unwrap` on data from outside the process: parsed files, network responses, user config. Those get `?` |
| **M-PANIC-MESSAGE** | Say what was expected in an `expect`: `expect("frame index came from the same list")` | `expect("oops")` |
| **M-ERRORS-CANONICAL-STRUCTS** | One error enum per module, with `#[from]` conversions | `map_err(|e| SomeError(e.to_string()))` — that throws the source away |

## Logging

| | Do | Don't |
|---|---|---|
| **M-LOG-NOT-PRINT** | Emit diagnostics through `log` (`src/log.rs`): an event name and named fields, one line, on stderr, with `ANIMA_LOG=off\|info\|debug` choosing how much of it a user sees. Three kinds stay outside it, each for its own reason: a `println!` to stdout where that stdout *is* the interface (`anima-workshop`, `anima-i18n`, `anima-overlay-ctl`, and the shepherd's `status`, `list`, `prune`); a fatal "cannot start" message, which a log level should not be able to hide; and a print inside a test, whose output is read by whoever ran it | `println!`/`eprintln!` on a path a user runs. *(done: the overlay's 50 sites, the shepherd's 22, the studio's 23)* |
| **M-LOG-STRUCTURED** | One event per line with named fields, so it can be grepped and parsed: `overlay.instance.restored x=100 y=200 scale=100 mode=Opaque` | A sentence with interpolated values, which needs a regular expression to read back |
| **M-LOG-OVERHEAD** | Keep a logging call off the per-frame path, or guard it | Formatting a string every frame and throwing it away |

## Types

| | Do | Don't |
|---|---|---|
| **M-PUBLIC-DEBUG** | `#[derive(Debug)]` on every public type; `src/lib.rs` carries `#![deny(missing_debug_implementations)]` so a new one cannot slip in | Ship a public type a caller cannot print. *(A regex of mine said 30 of 79; the compiler named 13, and it is the one that counts)* |
| **M-STRONG-TYPES-GUARD** | Let the newtype enforce its invariant in a fallible constructor, as `Config`/`Loaded` do for geometry | Build the newtype and trust callers to keep it valid afterwards |
| **M-DI-HIERARCHY** | Concrete types first, then generics, and `dyn` only where the set of implementations is open | Port a C# interface shape into a `dyn Trait` that has exactly one implementor |
| **M-PARAMETER-CONSISTENCY** | Same conceptual parameter in the same position everywhere: **(1)** the subject being changed (`&mut state`), **(2)** read-only context (`&Config`, `&Path`, `&Gpu`), **(3)** call-specific scalars, **(4)** closures last | Let the order drift between two functions that take the same things. Our `poll_control(state, control, control_mtime, control_path, pos_x, pos_y)` is the reference |
| **M-SHORT-NAMES** | Two short words at most: `AppConfig` | `GlobalApplicationConfig`. Don't bake the module into the name: `pack::Id`, not `pack::PackId` |
| **M-WEASEL-WORDS** | A name that says what it is: `RegionTuning`, `HotReload` | `Manager`, `Service`, `Factory`, and `handle`/`process` for a function whose job you cannot name |
| **M-REGULAR-FN** | Free functions for computation; `impl` for constructing and for methods that need a receiver | A type that exists only to host a namespace of functions |
| **M-INIT-BUILDER** / **M-BUILD-RESULT** | A builder when construction has more than four inputs, validating in `build()` | An eight-parameter constructor — that is a missing type, not a long list |

## Functions and modules

| | Do | Don't |
|---|---|---|
| **The ratchet** | Keep every function under 150 lines. It is enforced by `tests/function_length.rs` | Add to its `KNOWN` list without a one-line reason next to the entry |
| **M-BALANCED-MODULES** | Group by responsibility in files: `bin/anima-overlay/{gpu,frames,pipelines,config,wayland}.rs`, `bin/anima-studio/tools/{key,mp4,ai,mod}.rs` | Let a binary file grow past roughly 2600 lines, or a library file past 1000, without a plan |
| **Library size** | Keep library functions under 100 lines; every one is today, the largest being `shepherd::poll` at exactly 100. Keep library files under about 1000 lines | Assume a large file is fine because its functions are small — `anim/mod.rs` passes 1540 lines with a 77-line maximum function, so its problem is file size rather than function size, and it is the next candidate |
| **M-SMALLER-CRATES** | When a subtree is self-contained and independently testable, give it its own crate. *(done: `anima-ai` holds the AI removal, which referenced nothing else in the library and no longer imposes ONNX Runtime on the overlay's build)* | Split for its own sake — a crate boundary costs visibility rules, a workspace and a guard that now reads two `src` trees |

## Tests

| | Do | Don't |
|---|---|---|
| **M-INTEGRATION-TESTS** | Put tests that only touch public API in `tests/`; keep tests of private behaviour inline beside the code they specify | Mix the two. If a test could live in either place, prefer `tests/` |
| **M-TAUTOLOGICAL-TESTS** | Assert behaviour that could be wrong: the AI removal is checked against the Python reference, bit for bit | Restate a constant list in a test, or mirror the implementation's branches. That passes by construction |
| **M-TEST-UTIL** | Gate test-only helpers behind `#[cfg(test)]`, as we do | Expose a bypass or a fake in the shipped build behind no flag |
| **M-MOCKABLE-SYSCALLS** | Take the clock, the environment and `/proc` reads as inputs where a test would want to vary them | Bury a `/proc/self` read or a `$XDG_*` lookup where no test can reach it |

## Performance

| | Do | Don't |
|---|---|---|
| **Measure first** | Quote a number you produced with a command in this tree | Quote a percentage from a document, or from memory. See `docs/code-audit.md` for why |
| **M-HOTPATH** | Time a path before changing it, with the examples kept for that purpose: `frame_bench` for the overlay's per frame, `ai_frame_bench` for the AI removal's CPU stages, `alloc_bench`, `cache_bench`, `bc7_bench` | Optimize a path nobody has profiled — the numbers below exist because the first guess about the overlay was wrong |
| **M-MEM-REUSE** / **M-INITIAL-CAPACITY** | `Vec::with_capacity` when the size is known, as the preprocessor does for the 1024×1024×3 input | Grow a per-frame buffer from empty every frame |
| **M-THROUGHPUT** | Optimize for throughput in the batch paths, latency in the overlay | Trade one for the other without saying which |
| **M-UNSOUND** | Keep every code path sound | Reach for `unsafe` to make a benchmark look better |

## Unsafe

| | Do | Don't |
|---|---|---|
| **M-UNSAFE** | Confine `unsafe` to FFI and platform calls, with a comment naming the contract that makes it sound. Our only sites are `steam.rs` (libloading), `bc7.rs` (the vendored C++ encoder) and one in `gpu.rs` | Use `unsafe` to shorten a safe program, to bypass `Send`/lifetime bounds, or to transmute between unrelated types |
| **M-ISOLATE-DLL-STATE** | Keep `libsteam_api` state behind one binding, as `steam::flat` does | Let a second call site bind the DLL again and hold its own copy of the state |

## Docs and agents

| | Do | Don't |
|---|---|---|
| **M-MODULE-DOCS** / **M-DOC-FIRST-SENTENCE** | `//!` on every module, first sentence on one line, saying what the module is for and when to use it | Leave a module to speak for itself. *(46 files have it; the binaries' new modules are the gap)* |
| **M-NO-META-DESIGN-DOCUMENTATION** | Document the end state. Put the design journey in `docs/`, as `docs/code-audit.md` does | Write "we chose X over Y" or a change journal into rustdoc. *(one did: `src/ai/mod.rs` explained how it came to be split into files, which is my history rather than the module's, and it is removed. The check that first said there were none searched for the wrong phrases, so the claim was wrong before it was true)* |
| **C-EXAMPLE** / **M-DESIGN-FOR-AI** | Put a runnable example on public API. Agents and newcomers read these first | Ship public API with no example anywhere. *(the library still has no rustdoc examples; `examples/log_events.rs` is the first in `examples/` that shows how to call something rather than measure it)* |
| **M-DESIGN-FOR-AI** | Keep modules documented, APIs idiomatic, and types strong — compiler checks are what makes an agent's edits safe | Rely on conversation history as the documentation |
| **M-DOCUMENTED-MAGIC** | Name a number and say where it came from: `ASSET_PREVIEW_PX`, `MAX_IDS_PER_REQUEST` | Leave `100` or `512` in place with no name. *(126 named constants so far)* |

## Project

| | Do | Don't |
|---|---|---|
| **M-CRATES-FLAT-FOLDER** | One workspace `Cargo.toml` at the root, with both crates in a single direct subdirectory: `crates/anima-engine-rs` and `crates/anima-ai` | Leave a crate at the root because it was there first |
| **M-CARGO-WORKSPACE** | Shared metadata in `[workspace.package]`; `[workspace.dependencies]` for anything two crates share or that crosses the boundary, so `anima-ai` is declared once with `version` and `path` | Duplicate a version in two manifests, or enable a dependency's features in the workspace rather than in the crate that wants them |
| **M-CRATES-IN-WORKSPACE** | Every crate is a member, and an intra-workspace dependency is written `anima-ai.workspace = true` | Depend on a sibling by relative path, which lets its version drift |

## Deliberately not applicable

| Rule | Why not |
|---|---|
| **M-TARGET-CPU** | The guideline is for server applications against a fleet whose CPU you control. We ship a desktop binary to unknown CPUs; the baseline target is the correct choice |
| **M-ASYNC-FN**, **M-ASYNC-STACK-SIZE**, **M-YIELD-POINTS** | The project is deliberately synchronous threads; there is no async runtime to yield to |
| **M-FAST-HASHER** | Hash maps are on cold paths only (`i18n` catalogue load, a `seen` set in `paths`) |
| Macros, `M-PROC-*`, `M-MACRO-*` | We have no macros, by choice |
| **M-SYS-CRATES**, **M-FFI-NAMING** | We bind at runtime through `libloading`; there is no `-sys` crate or link-time FFI surface |
| Library / Interoperability, UX, Resilience, Building | See *Scope* above |
| **M-AVOID-STATICS** | The three statics are `OnceLock`/`Mutex` initialisation guards, which the guideline permits |

## Measured, and decided against

**M-MIMALLOC-APPS — mimalloc as the global allocator.** The guideline says applications *should* set it, with
"up to 25%" taken from Microsoft's own benchmarks. That figure describes their workload, not ours, so
`examples/alloc_bench.rs` measures ours. Intel Core Ultra 7 155H, glibc 2.44, rustc 1.99.1, mimalloc 0.1.52,
best of fifteen rounds:

| case | system | mimalloc |
|---|---|---|
| 50,000 short-lived buffers | 3.3 ms | **0.8 ms** (4.1x faster) |
| 200 × 480×270 RGBA frames | 2.6 ms | 2.6 ms |
| 20 × 12 MiB f32 buffers | **9.3 ms** | 10.6 ms (1.14x slower) |
| 2,000 vectors grown from empty | 1.1 ms | 1.2 ms |
| 4 threads × 50,000 buffers | 4.6 ms | **2.2 ms** (2.1x faster) |
| peak RSS | 41,588 kB | **31,588 kB** (24% lower) |

The two halves agree with each other instead of being noise: mimalloc is faster exactly where it is supposed
to be — many small allocations, and contention between threads — and slower on large blocks, which is what an
allocator that returns memory to the OS more eagerly looks like. That same eagerness is why its peak RSS is
lower.

What settled it is the arithmetic rather than the ratios. The small-object win is 2.5 ms across 50,000
allocations, about **50 ns each**; a frame at 60 fps that made a hundred of them would save **5 microseconds**
out of a 16.7 ms budget. Meanwhile our per-frame work is dominated by the allocation mimalloc is *slower* at:
518 kB frame buffers, and a 12 MiB model input for the AI path. So the dependency buys a path we do not have
and charges us on the one we do.

Not covered, and worth saying plainly: the studio's Iced element tree is rebuilt every frame out of very many
small boxes, which is the shape mimalloc wins on, and this benchmark does not measure it. If that ever shows
up in a profile, run the example with `--features mimalloc` and time the window itself. The feature and the
benchmark are kept, off by default, so that costs one build.

## Measured: where the time goes

Two benchmarks, both run on this machine and both kept as examples so the numbers can be reproduced and
compared against. The first one's answer was not what I expected.

**The overlay's per frame** — `cargo run -p anima-engine-rs --release --features overlay --example frame_bench` (add an animation path for the real
numbers below), 12 frames of 480x270 on an Intel Arc MTL through Vulkan:

| | per rebuild | rects | per second at 60 fps |
|---|---|---|---|
| input region, step 1 | **1.442 ms** | 109 | **28.8 ms** |
| input region, step 2 | 0.364 ms | 55 | 7.3 ms |
| input region, step 4 | 0.095 ms | 28 | 1.9 ms |
| texture upload | 0.229 ms | — | 13.7 ms |

The upload is 1.4% of a 16.7 ms frame. The **region scan is six times the upload**, and it runs every third
frame (`REGION_UPDATE_FRAMES`), so it costs 28.8 ms out of every second — 2.9% of the process's wall time,
spent finding the silhouette.

That raised an obvious question — which step does `Auto` pick for a real animation? — so the same example takes
an animation path and answers it. Three real workshop clips:

| | surface | step 1 | rects, worst frame | step 1 cost | max_step | `Auto` picks |
|---|---|---|---|---|---|---|
| `Miku Miku Dance.gif` | 200x164 | 0.481 ms | 128 | 9.6 ms/s | 1 | step 1 → 1.0% of wall time |
| `Cute Dancing GIF.gif` | 200x200 | 0.471 ms | 273 | 9.4 ms/s | 1 | step 1 → 0.9% |
| `boeeee.apng` | 370x370 | **1.559 ms** | 264 | **31.2 ms/s** | 2 | step 1 → **3.1%** |

Two answers, and the first is the more useful one. **`Auto` picks step 1 every time, because real silhouettes
produce 128 to 273 rectangles and the soft budget is 768.** The coarsening path — the `while rects.len() >
soft_rects` loop, the doubling, the `max_step` bound — never runs on this input. It is machinery that no real
animation reaches, which is not the same as it being wrong, but it is worth knowing before trusting it.

The second: the cost scales with the *surface area*, not with the animation's file size or frame count.
200x200 costs 9.4 ms/s and 370x370 costs 31.2 ms/s — 3.3x for 3.4x the pixels. So the number that matters for a
given overlay is its displayed size, and at 370x370 the scan is 3.1% of wall time. That is real but small, and
the knob is `ANIMA_REGION_SOFT_RECTS`: at 256, `boeeee.apng`'s 264 rectangles would push `Auto` to step 2 and
8 ms/s instead of 31, paying for it in two-pixel-batch input accuracy. Worth trading only if the 3% ever shows
up somewhere.

**The AI removal's CPU stages** — `cargo run -p anima-engine-rs --release --features studio --example ai_frame_bench`, 1280x720 frames:

| | per frame |
|---|---|
| `preprocess` (to 1024x1024 f32) | **82.03 ms** |
| `mask_from_output` (back to 1280x720) | 50.97 ms |
| `apply_mask` | 1.96 ms |
| total | **134.95 ms** |

Against the ~4.9 s per frame the whole pipeline took when it was verified against rembg, these three stages
are **2.8%**. The model is the cost. Optimising our own code here would not change what a user waits for, so
the correct action is to leave it — and to know that `preprocess` is 61% of our share if the model ever gets
faster. This is the useful shape of a benchmark: it stops work as often as it starts it.

Neither number covers the whole picture and both say so in their own output: `frame_bench` reproduces the
upload's three wgpu calls instead of calling `upload_frame`, because that lives in the binary where an example
cannot reach it; `ai_frame_bench` leaves out inference and ffmpeg extraction, which `tests/ai_end_to_end.rs`
covers, ignored, because it wants the 176 MB model.

## Outstanding, in the order worth doing

1. `anim/mod.rs`, at 1540 lines with a 77-line maximum function, is the next candidate: it wants splitting by
   subject, not by function.
2. `push_diagonal`'s ten arguments want a type. It carries an `#[expect]` saying so, which is a note to
   future readers rather than a resolution.

Done from this list: the two lint overrides; `rust-version`, which turned out to be 1.92 rather than the 1.88 I
derived; `Debug` on the public types, which is now a `#![deny(missing_debug_implementations)]` in
`src/lib.rs` rather than a promise — a new public type without it fails the build, and that guard covers the
library only, which is the scope the rule is about, since a `pub` type in a binary is not public API; and the
console output, which `log` now handles in all three binaries, with `examples/log_events.rs` to see the format.
