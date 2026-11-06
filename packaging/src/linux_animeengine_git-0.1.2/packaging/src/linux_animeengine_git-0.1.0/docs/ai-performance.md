# The AI path, measured

What the background removal actually costs, what was guessed wrong on the way, and what is left. Every number
here was produced on this machine, Intel Core Ultra 7 155H, and every command to reproduce it is at the end.

## Measure in the profile that ships

The single largest finding is not about the code. Every AI timing this project had was produced by `cargo test`,
which is the **dev** profile. `the_model_actually_cuts_a_frame` reported 4.4 s per frame; the same test in
`--release` reports **1.05 s**. A factor of 4.3, entirely from the build profile.

That matters twice over. The inference itself runs inside a prebuilt C++ library and never cared; everything
around it — the 12 MiB tensor going in, the 4 MiB tensor coming out, our preprocessing and postprocessing — was
unoptimised. And a share computed against a dev number is wrong by that same factor: this document used to say
the AI stages are 2.8 % of a frame. They are **12.1 %**.

## The per-frame budget

| | first frame | steady frame |
|---|---|---|
| `Removal::load()` | 409 ms | — |
| inference | ~820–1000 ms | ~820–1000 ms |
| our three stages | 128 ms | 128 ms |
| **total** | **≈1.46 s** | **≈1.05 s** |

`load()` is once per *job*, and a job is a whole video, so it is 28 % of a single-image job and 0.01 % of a
video. It is not worth caching for the video path and would be for the still-image tool.

The inference is within 6 % of Python's `onnxruntime` on the same model, same thread count, same CPU. The model
was never the slow part of *our* numbers; it is 86 % of the budget and it is running as fast as this stack runs.

## Where our 128 ms goes

| stage | cost | of which |
|---|---|---|
| `preprocess` | 76.8 ms | **66.2 ms is one `image::imageops::resize`, Lanczos3, 1280×720 → 1024×1024** |
| `mask_from_output` | 43.4 ms | its own Lanczos3 resize, plus a per-pixel loop |
| `apply_mask` | 2.0 ms | 0.47 ms of that is the `frame.clone()` |

Profiled with `perf record -e cpu-clock` on a build with symbols kept: **`roundf` is 18.9 % of the whole run**,
and it is not in any loop of ours. It is in `image`'s Lanczos kernel, per tap, next to `sinf32`, `floorf` and
`ceilf`. Both resizes use that filter and both pay it.

## Four explanations that were wrong

| guess | measured |
|---|---|
| the per-pixel closure and bounds check in `mask_from_output` | **3.0 ms** |
| `f32::round` blocking vectorisation in that loop | **1.9 ms** |
| "thirty milliseconds of our own row loop" | not supported by the samples |
| "the resize inside `mask_from_output` is 0 %" | my sub-benchmark resized 1280×720 to 1280×720 |

The pattern is the one this project keeps meeting: a plausible cause named from reading the code, and a
measurement that disagrees by an order of magnitude. What settled it was `perf`, not another reading.

## int8

Two routes, both measured, different answers.

**Dynamic (`quantize_dynamic`)** — 176 → 44 MB, and **5.3× slower**: 4530 ms against 853. It targets MatMul,
which is a transformer's shape; this graph is 84.7 % `Conv`, so every operator paid a quantise/dequantise round
trip for nothing.

**Static QDQ (`quantize_static`, `per_channel=True`, `["Conv","MatMul"]`)** — **1.24× faster** on three
independent measurements (1.21, 1.26, 1.24), 44 MB, and quality that holds across clips:

| | |
|---|---|
| calibration | `boeeee.apng`, 6 frames |
| evaluation | `mik37a11….apng`, 8 frames — **disjoint by clip** |
| IoU at `VISIBLE_ALPHA = 25` | mean **0.9886**, worst 0.9339 |
| foreground | 17.2–30.1 %, int8 moves it −1.34 … +0.13 points |

The step that mattered was evaluating on a *different clip* from the one used for calibration. A first pass
calibrated and evaluated on crops of one image and reported IoU 0.9931, which proved nothing.

**One risk is open.** Frames with almost no subject — foreground 0.10 % — gained 0.15 points of foreground under
int8. On a desktop overlay that is a few stray opaque pixels that would take clicks. No sampled frame of the
evaluation clip was that empty, so this is unmeasured rather than disproved.

The recipe, all of it: convert the model to opset 13 (the quantiser emits `DequantizeLinear.axis`, which opset 11
does not have), `quant_format=QDQ`, `per_channel=True`, `weight_type=QInt8`,
`op_types_to_quantize=["Conv","MatMul"]`, calibration from real clip frames.

## Not worth doing

- **Hand-written SIMD or vectorising our loops.** The samples are in a dependency's Lanczos kernel. Our 3 % does
  not justify leaving safe Rust, and `docs/rust-performance.md` records the same conclusion from the other side.
- **The Lanczos filter itself.** It is the hot code and it is a faithfulness choice, not waste. Replacing it
  changes pixels; it is a quality decision worth about 25 ms a frame.
- **Caching `Removal` across jobs.** 409 ms once per video is not a cost.

## Reproducing

```text
cargo run --release --features studio --example ai_frame_bench
cargo test --release -p anima-engine-rs --features studio --test ai_end_to_end -- --ignored --nocapture

CARGO_PROFILE_RELEASE_STRIP=false cargo build --release --features studio --example ai_frame_bench
perf record -e cpu-clock ./target/release/examples/ai_frame_bench
perf report -i perf.data --stdio --no-children --sort symbol

# the int8 route needs onnx + onnxruntime + pillow in a throwaway environment and runs offline
```
