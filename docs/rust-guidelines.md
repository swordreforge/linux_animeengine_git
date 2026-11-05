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
cargo test --features overlay,bc7,dbus,studio,workshop-web,steamworks
cargo clippy --all-targets --features overlay,bc7,dbus,studio,workshop-web,steamworks
ANIMA_FN_LIMIT=100 cargo test --test function_length -- --nocapture
```

Counts quoted below are from the commit that added this file. They are here to be worked down to zero, and
should be re-measured rather than trusted.

## Lints and toolchain

| | Do | Don't |
|---|---|---|
| **M-LINT-OVERRIDE-EXPECT** | Silence a lint with `#[expect(clippy::x, reason = "why")]`, which fails when the lint stops firing | Use `#[allow]` outside generated code — it goes stale silently. *(both former sites are dealt with: one fixture was rewritten so it no longer needs the lint, the other is an `#[expect]` carrying its reason)* |
| **M-MSRV** | Declare `rust-version` in `Cargo.toml`, and find the number by running a `cargo +<older toolchain>` command: cargo names every dependency that refuses that toolchain, and the highest refusal is the floor | Derive it from the language features you use. Ours is 1.92, set by the non-optional `ort` (ONNX Runtime) — not the 1.88 that let-chains and `is_multiple_of` suggest, which is what I wrote first |
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
| **M-LOG-NOT-PRINT** | Route diagnostics from the overlay, studio and shepherd through `telemetry`; write to stdout only in the CLI binaries (`anima-workshop`, `anima-i18n`, `anima-overlay-ctl`), where it *is* the interface | `println!`/`eprintln!` in a GUI or daemon. *(`anima-overlay` 41, `anima-shepherd` 46, `anima-studio` 34 sites to move)* |
| **M-LOG-STRUCTURED** | One message per event with named fields, so it can be filtered and grepped | Interpolating a whole sentence. `"[overlay] rebuilt frames: 12 frame(s) 480x270"` becomes fields `frames`, `width`, `height` |
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
| **M-SMALLER-CRATES** | When a subtree is self-contained and independently testable, consider its own crate. `src/ai/` — 712 lines, 13 tests, nothing over 52 — is the candidate | Split for its own sake — a crate boundary costs visibility rules and a workspace to manage |

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
| **M-HOTPATH** | Benchmark the two paths that matter — the overlay's per-frame upload and the AI batch — before changing them | Optimize a path nobody has profiled. There are no benchmarks in this repository yet |
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
| **M-NO-META-DESIGN-DOCUMENTATION** | Document the end state. Put the design journey in `docs/`, as `docs/code-audit.md` does | Write "we chose X over Y" or a change journal into rustdoc. No `//!` line in this tree does, and that is deliberate |
| **C-EXAMPLE** / **M-DESIGN-FOR-AI** | Put a runnable example on public API. Agents and newcomers read these first | Ship public API with no example anywhere. *(`src/` has zero doc examples; the four files in `examples/` are benchmarks and probes, not examples of how to call the API)* |
| **M-DESIGN-FOR-AI** | Keep modules documented, APIs idiomatic, and types strong — compiler checks are what makes an agent's edits safe | Rely on conversation history as the documentation |
| **M-DOCUMENTED-MAGIC** | Name a number and say where it came from: `ASSET_PREVIEW_PX`, `MAX_IDS_PER_REQUEST` | Leave `100` or `512` in place with no name. *(126 named constants so far)* |

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

## Outstanding, in the order worth doing

1. `mimalloc` as the global allocator — then measure it. The guideline says "up to 25%", which is a claim from
   someone else's benchmark, not a result in this tree.
2. Move the overlay's `eprintln!` diagnostics onto `telemetry`.
3. `src/ai/` into its own crate, which means introducing a workspace. `anim/mod.rs`, at 1540 lines with a
   77-line maximum function, is the separate case: it wants splitting by subject, not by function.
4. One benchmark for the overlay's frame path and one for the AI batch, before optimizing either.
5. `push_diagonal`'s ten arguments want a type. It carries an `#[expect]` saying so, which is a note to
   future readers rather than a resolution.

Done from this list: the two lint overrides; `rust-version`, which turned out to be 1.92 rather than the 1.88 I
derived; and `Debug` on the public types, which is now a `#![deny(missing_debug_implementations)]` in
`src/lib.rs` rather than a promise — a new public type without it fails the build. That guard covers the
library only, which is the scope the rule is about: a `pub` type in a binary is not public API.
