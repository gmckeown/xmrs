# Per-lane looping (`ChannelLoop`) — architecture reference

**Status: delivered** (model + sequencer free-run + single-pass render,
all suites green). This document explains the design; the DW importer
cites it by name (`dw_module.rs`).

Motivation: faithfully represent the independent-voice replayers (David
Whittaker, and most C64/Amiga engines) where each channel loops its own
sequence at its own length. Canonical case: `xenon2 (title).dw` — four
voices looping at 9888 / 9888 / 9912 / 10014 frames, drifting out of phase
forever. A single global loop point (`Module::song_loop_to`) cannot model
this: it renders ~20 min of tiled drift and then hard-wraps all four
voices at once — which the hardware never does. This is also the standard
DAW gesture (per-track loop braces; different-length clips = polymeter in
Ableton/Bitwig Session View).

The whole mechanism is **gated on `Module::channel_loops` being non-empty**
and is only populated for modules whose voices have *unequal* pass lengths.
When every voice shares one loop (e.g. bubble bobble: 2304 everywhere) the
proven global `song_loop_to` path is kept untouched.

---

## 1. Data model (core)

The loop lives on the **lane** = `(song, target_channel)` — not on the
`Track` (reusable content, shared across clips) nor on a single `Clip`
(the DW loop spans a channel's whole clip sequence).

```rust
// xmrs/src/core/daw/loop_region.rs

/// One voice's loop as a half-open tick region: when the lane's local
/// playhead reaches `end_tick`, it jumps back to `start_tick`. Ticks share
/// the space of `Clip::position_tick` / `TimelineEntry::tick` (absolute
/// sub-song ticks). `start_tick < end_tick`.
pub struct ChannelLoop {
    pub song: u16,
    pub channel: u8,
    pub start_tick: u32,   // loop_to of the position list (0 for xenon2)
    pub end_tick: u32,     // pass length (exclusive)
}
```

Stored on `Module` as `#[serde(default)] channel_loops: Vec<ChannelLoop>`
(empty = current behavior). A `Vec` (not a tuple-keyed map) serializes
cleanly everywhere; N is tiny (≤ 4 channels × a few songs) so the linear
`Module::channel_loop(song, ch)` lookup is negligible. Validation
(`ChannelLoopEmpty`) enforces `start < end` within the song's tick span.

---

## 2. Note resolution (`row_at`)

`row_at` resolves a `(song, pat, row)` to cells through
`clips.active_at(song, ch, abs_tick)`. Per-lane looping inserts a
**modulo on the tick** before the clip lookup, then derives the local row
by an inverse timeline lookup:

```rust
let local_tick = match self.channel_loop(song, ch) {
    Some(l) if playhead >= l.start_tick as u64 => {
        let len = (l.end_tick - l.start_tick) as u64;
        l.start_tick as u64 + (playhead - l.start_tick as u64) % len
    }
    _ => playhead,                         // unlooped lane → unchanged
};
```

The historical `row_at(song, pat, row)` delegates to
`row_at_with_playhead(…, entry.tick)`, so its contract is unchanged.
`TimelineMap::row_for_tick` is a binary search over entries sorted by
`(song, tick)`; DW regions are row-grid aligned (start = 0, end = a
multiple of speed) so `local_tick` always lands on a row boundary. The
same modulo precedes the lookup in
`xmrsplayer/src/channel/lanes.rs::active_at` (vibrato/slide).

**Gating:** empty `channel_loops` ⇒ `local_tick == playhead` ⇒
byte-identical output. XM/IT/MOD are never touched.

---

## 3. Sequencer: infinite free-run (the "no jump" core)

This is what actually removes the wrap. Hardware behavior is an **infinite
loop with no global wrap**: each voice reloops independently forever. So
when `channel_loops` is non-empty, the sequencer stops wrapping globally
and instead reloops the timeline (which covers **one pass max**) while
keeping a **monotonic** tick:

- at the end of the song's timeline, restart at the song's first entry
  (`loop_generation + 1`) instead of wrapping;
- `current_abs_tick_monotonic() = generation * song_tick_span + entry.tick`;
- that monotonic tick feeds `row_at_with_playhead` and the lane folds, so
  each lane loops via its own region and the voices drift exactly like the
  hardware — **never jumping**.

No LCM is computed: we don't wrap, we drift. The timeline stays small (one
pass). `u64` ticks make overflow unrealistic. The player passes the same
monotonic tick to note resolution **and** automation (`tick0` fold), so
notes and modulation stay in lock-step.

**Gating:** empty `channel_loops` ⇒ the current `song_loop_to` wrap branch
is unchanged. All the risk is behind that one test.

---

## 4. DW importer

For a drifting module the importer renders **one pass per lane** (the
simulator already knows each channel's `first_wrap_frame`): it emits the
clips for `[0, first_wrap)` and posts a `ChannelLoop { start_tick =
loop_to, end_tick = first_wrap }`. `song_length` (UI/seek) is the max of
the lanes' `end_tick`. This is done **only** when pass lengths differ;
otherwise the global `song_loop_to` path is kept. Single-pass also makes
the module far lighter (one pass vs ~6 tiled).

The single-pass render is exact: `TickEvent::channel()` lets the trace be
truncated per channel to `[0, first_wrap[ch])`, and per-lane `lane_rows`
passed to `build_segments` ends each voice's final clip at *its* pass (no
dead tiled clip in `[end_ch, span)`).

---

## 5. Cross-cutting impacts

| Area | Handling |
|---|---|
| serde / `.xmr` | new `#[serde(default)]` field → back-compatible |
| `verify_layers_consistent` | `start < end`, within the song's tick span, region ⊇ the lane's clips |
| `xmrs::duration` | a song with `channel_loops` plays forever → return a capped representative duration, never loop infinitely |
| `Module::song_length` | max `end_tick` of the looped lanes |
| `row_at` | contract unchanged; `row_at_with_playhead` is the implementation |
| sequencer | free-run branch, strictly gated `!channel_loops.is_empty()` |

---

## 6. Verification

- **Core unit:** a synthetic module with two unequal loops (e.g. 6 and 8
  ticks) — `row_at_with_playhead` returns the correct looped cell past each
  loop, and the long lane still plays while the short one repeats.
- **Non-regression:** XM/IT/MOD + DW corpus unchanged (empty
  `channel_loops` ⇒ identical output); `free_run_keeps_monotonic_tick_rising`.
- **DW xenon2:** single-pass + regions; via `dw_oracle_trace`, the voices
  drift like the hardware (ch0/ch1 periodic at 9888 across 10014/20028,
  ch2 at 9912, ch3 at 10014).
- **bubble bobble:** stays on the global path (no `channel_loops`), loop
  2304 unchanged.
