# The audit, and the splits that came out of it

This documents one long session's pass over the two large binaries, and the Tools tab work that preceded it.
It exists because the numbers are the argument: code that is too long to hold in your head is where careful
edits go wrong, and the only way to keep a refactor from being undone is a check that runs on its own.

## The numbers

| | Before | After |
|---|---|---|
| `src/bin/anima-studio.rs` | **5033** | **4131** |
| — `handle` (the message match) | **570** | **87** |
| `src/bin/anima-overlay.rs` | 3020 | **2561** |
| — `main` | **852** | **312** |
| The Tools tab | one 444-line file, one 345-line function | **4 files**, largest function ~134 |
| Functions over 150 lines | 4 | **4, each on a list with a reason** |
| Tests | 196 | **238** |
| Clippy | — | **0** |

The library side was already healthy: apart from `anim/mod.rs` (1540 lines, largest function 77) no library
file has a function over 100 lines. All of the trouble was in the binaries, which is worth knowing before
deciding that a big file is a big problem — `src/ai/` is 700 lines with 19 tests and no function over 52, and
that is the shape the rest now aims at.

## The method that worked

1. **Print the region, verbatim, with line numbers.** Not a sketch of it, not a summary.
2. **Move it unchanged** into its new home, at the same indent, so the diff is a deletion plus an insertion.
3. **Let the compiler list what the new surroundings are missing** — parameters from the enum it passed to
   itself, visibility, a `&mut` binding.

Five of the overlay slices went this way, and four of the studio ones.
The two that failed did not. The first wrote a header from a sketch of what it
should look like, invented a struct pattern naming fields that do not exist, and produced fifty-seven errors.
The second tried to bind the loop's counters by reference so the body would not change, and fought the borrow
checker in both directions at once: adding `mut` made the `*`s wrong, removing them made the assignments
wrong. Both were reverted rather than committed.

## What the splits found

- **`handle` was 52% card handlers.** Splitting by message family made the match a list — an arm with a block
  became a method next to the state it touches; an arm that is one assignment stayed put, because a method
  wrapped around one assignment is ceremony.
- **The overlay's `main` was four programs in one**: startup, a GPU, a Wayland surface, and a loop. Six slices
  later, the first three are `config`, `gpu`, `frames`, `pipelines` and `wayland`, and what is left in `main`
  is the loop and the tray.
- **An argument list that had grown to eight** was the symptom of one missing type: `frames::build` wanted a
  `Gpu`, not five pieces of one.

## Lessons that cost time, in the order they cost it

1. **An unbounded text replacement is a bug with a delay.** Replacing `limits,` in `anima-overlay.rs` hit
   seven places, five of which are struct literals. Anchor the edit, and read what the compiler says about
   the ones you got wrong.
2. **Removing a `return` means removing its semicolon.** Three times in one session. Removing a dead tail
   means replacing the line, not blanking it — blanking deleted a function's closing brace.
3. **"Does this function still need a tail?"** is answered by whether every path already returns, not by what
   the last line looks like. The cheap rule is to add the tail always and let clippy say which are
   unreachable.
4. **A pattern must name every field or end in `..`.** After a move, most of the fields are no longer its
   business, and the unused-variable warnings name them one by one.
5. **`mut` goes on the binding, not the reference.** `mut state: &mut OverlayState` looks wrong and is right;
   the body reborrows it.
6. **An attribute belongs immediately above its item.** Inserting a `mod` between a `#[path]` and its `mod`
   silently moves the attribute to the wrong declaration, and `cargo fmt` then rearranges what is left.
7. **Quote the heredoc.** An unquoted one let the shell execute the backticked words in a commit message.
8. **Measure before writing a number.** A guessed line count in a commit message is wrong often enough that
   the number should come from the command that writes it.
9. **`clippy --fix` for mechanical work.** Redundant borrows and needless returns are its job, not mine.

## The guard

`tests/function_length.rs` fails when any function passes 150 lines without being on its `KNOWN` list, when a
listed function grows, and when one shrinks without its entry being updated. It caught the overlay's `main`
three times during the session — once before each of the commits that followed a shrink — and it is the only
reason the splits will still be splits in three months.

Four functions are on the list: the overlay's `main` at 312, the studio's `view` at 302, the overlay's
`vertices` at 281, and the shepherd's `menu` at 172. Run `ANIMA_FN_LIMIT=100 cargo test --test function_length
-- --nocapture` to see the eleven functions between 100 and 150; that is the next pass.

## What is left, and why it is left

**The playback loop, 213 lines, is one function and should stay that way for now.** Its body owns seven
counters and replaces the frame storage outright, so it is not a move but a rewrite — and the six modules
around it have no tests, because they are glue: `Gpu::create` needs a device, `wayland::connect` needs a
compositor. Nothing can catch a mis-moved loop, which is exactly why both attempts at it failed. An untested
213-line function is safer than three untested seams.

If it is attempted: make the loop a method on a type that owns the counters, and replace `index` with
`self.index` throughout. That is thirty mechanical edits in one direction, rather than the two-directional
fight that binding them by reference produces.

## Testing layout

4775 of 21758 lines are `#[cfg(test)]` modules, 22%, holding 241 `#[test]` functions — 238 of which pass,
4 are ignored, and the rest is the end-to-end AI removal that has to be asked for by name. That is deliberate: they test private
behaviour, next to the code whose specification they are, and `#[cfg(test)]` costs nothing in a release build.
Moving them to `tests/` would make them public-API-only — either leaking internals or forcing
`#[cfg(test)] pub(crate)` shims. If any need to leave their file, put them in `src/foo/tests.rs` behind
`#[cfg(test)] mod tests;`, which keeps the privacy and is the only split that costs nothing.

The two files that are in `tests/` are there because they are about the crate rather than a module: an
end-to-end AI removal (ignored by default; it downloads 176 MB), and the length guard.

## Checking any of this

```text
cargo test --features overlay,bc7,dbus,studio,workshop-web,steamworks
cargo clippy --all-targets --features overlay,bc7,dbus,studio,workshop-web,steamworks
ANIMA_FN_LIMIT=100 cargo test --test function_length -- --nocapture
```
