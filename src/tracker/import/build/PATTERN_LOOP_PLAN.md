# Pattern-Loop (E6x / Sxx / SBx) — Fidelity Plan

Multi-session plan to make pattern-loop replay faithful in the DAW
timeline layer. Two **complementary** defects, found while chasing a
silent loop replay in `~/Music/mod/barmansp.mod`:

1. **Notes drop on loop repeats** (the visible barmansp bug) — the
   *clip* layer can't supply cells for a repeated row.
2. **Loop counters are global, not per-channel** — the walker
   collapses every channel's `PatternLoop` into one anchor/counter, so
   multi-channel loops and the format-specific counter quirks are
   wrong.

These are independent: #1 makes the **notes follow** whatever sequence
the walker emits; #2 makes that **sequence itself** faithful. #1 is a
prerequisite for #2 being audible, so do #1 first.

---

## 0. Current state (where the code is)

- **Walker** `import/build/timeline_map.rs`
  - collects `loop_param` per row by *overwriting* on each channel:
    `NavigationEffect::PatternLoop(v) => loop_param = Some(*v)`
    (`timeline_map.rs:145`) → **last channel wins**, single global
    `loop_anchor` + `loop_iters_left` (`:198-221`).
  - Emits one `TimelineEntry` per visited row, tagging loop repeats
    with `loop_iter` ≥ 1 (`timeline.rs:46`).
- **Clip builder** `import/build/segments.rs`
  - builds clips **only** from `loop_iter == 0` entries
    (`segments.rs:194`, and `clip_end_tick_from_timeline` skips
    `loop_iter != 0` at `:276`). Each clip is bounded
    `[position_tick, end_tick)`.
- **Runtime** `core/module.rs:row_at_with_playhead`
  - resolves the played row from the **TimelineMap entry's `row_idx`**
    (not from tick arithmetic), then drops the cell on
    `local_tick >= clip.end_tick` (`module.rs:609`). This bound is the
    sole reason loop repeats are silent.
- **Importers** already emit `PatternLoop` **per channel** (each cell's
  unit): XM `mod_xm_effect.rs:442`, IT `it_effect.rs:544`,
  S3M `s3m_effect.rs:578`, MOD via the amiga effect map. The channel
  identity is still present in the row's unit order — only the walker
  discards it.

---

## 1. Reference semantics (source of truth)

> Per project rule: replay semantics come from the real replayers, not
> heuristics. Locals: `~/Documents/ft2-clone-master` (XM),
> `~/Documents/schismtracker-20251014` (IT),
> `~/Documents/pt2-clone-master` (MOD / ProTracker).

All three keep a **shared global row pointer** (every channel is always
on the same row); the loop only ever *jumps that shared pointer back*.
"Per-channel" refers strictly to **where the start-row + counter STATE
lives** and **how conflicts resolve**, not to channels playing
different rows. → The global linear expansion in the walker is the
correct shape; only its *state* must become per-channel.

### FT2 / XM — `ft2_replayer.c:660 patternLoop()`
Per-channel `patternLoopStartRow`, `patternLoopCounter`:
```
E60      : startRow[ch] = song.row
E6x x>0  : if counter[ch]==0 { counter[ch]=x; pBreakPos=startRow[ch]; jump }
           else if --counter[ch]>0 { pBreakPos=startRow[ch]; jump }
           else { /* counter hit 0: no jump, stays armed-at-0 */ }
```
- `pBreakPos`/`pBreakFlag` are **global**; channels processed ascending,
  so **highest channel asserting a jump wins** the target row.
- State is reset **only** in `resetReplayerState()` (`:82`, song
  start/stop) — **persists across pattern boundaries** (real quirk; the
  `:2340` comment documents the resulting pBreakPos-overflow→row 0 bug).
- FT2 overflow guard: if `song.row >= currNumRows` after a loop,
  `song.row = 0` (`:2344`).

### IT / Schism — `player/effects.c:663 fx_pattern_loop()`
Per-channel `patloop_row`, `cd_patloop`:
```
S60      : patloop_row[ch] = row
S6x x>0  : if cd_patloop[ch] != 0 {
               if --cd_patloop[ch]==0 { patloop_row[ch]=row+1; return /*no loop*/ }
           } else { cd_patloop[ch]=x }
           process_row = patloop_row[ch]-1   // loop back
```
- Differs from FT2: explicit anti-infinite-loop `row+1` re-anchor when
  the counter expires; default `patloop_row` is row 0.
- **Reset lifecycle (verified — IT does NOT diverge after all):**
  normal sequential advance goes through `increment_order`
  (`sndmix.c:872`), which **never touches `cd_patloop`/`patloop_row`**.
  The only reset is `csf_set_current_order` (`csndfile.c:512`), called
  on **external seek / position-set**, not during playback. Bxx/Cxx
  during playback route via `break_row` + `increment_order`, so they
  don't reset it either. → **IT persists loop state across patterns and
  across jumps during playback, same as FT2/PT.** (My earlier
  "IT diverges" note was wrong — corrected.)
- **Loop × break precedence (VERIFIED, `effects.c:2418`):** Cxx
  (`FX_PATTERNBREAK`) is **suppressed entirely while any channel's
  `cd_patloop` is active** (`if (patloop) break;`). So in IT **loop wins
  over break** — playback stays in the pattern. This is the IT combine
  rule; the walker's fixed `Jump > Break > Loop` does NOT reproduce it.

### ProTracker / MOD — `pt2-clone/src/pt2_replayer.c:312 jumpLoop()`
Per-channel `n_pattpos`, `n_loopcount`; row-tick gated (`tick != 0 →
return`):
```
E60      : n_pattpos[ch] = song->row
E6x x>0  : if n_loopcount[ch]==0 { n_loopcount[ch]=x }
           else if --n_loopcount[ch]==0 { return /* expired: no jump */ }
           pBreakPosition = n_pattpos[ch]; pBreakFlag = true
```
- `pBreakPosition`/`pBreakFlag` **global**, channels ascending → last
  channel asserting wins (same shape as FT2, but note the decrement
  branch differs: PT jumps when `loopcount != 0` *after* the
  pre-decrement, returns the row it expires on without a final jump).
- `n_loopcount` reset (verified) only in `doStopIt` (`:228`) and
  `modStop` (`:1588`) — both stop-song; never per-pattern/per-order →
  **persists across patterns** (same as FT2 and IT).
- Dxx pattern-break is separate (`:506`): `pBreakPos = hi*10+lo`,
  clamped `>63 → 0` (decimal param, classic MOD quirk).
- **Loop × break precedence (VERIFIED, row-end `:1427`+`:1469`):** two
  flags — loop sets `pBreakFlag`, break/jump set `posJumpAssert`. At
  row-end: `if (pBreakFlag) row = pBreakPosition; pBreakPosition = 0`
  (loop, **clears the target**); then `if (posJumpAssert) nextPosition`
  → `row = pBreakPosition` (now 0) + advance order. So **PT loop+break →
  next order at row 0** (the clear wipes the break's row). **FT2
  differs**: its row-end (`ft2_replayer.c:2309`+`:2315`) does NOT clear
  `pBreakPos` between the two steps → **FT2 loop+break → next order at
  `pBreakPos`** (the break's target survives). Three formats, three
  combine rules (PT row 0 / FT2 break-target / IT break-suppressed).
- barmansp = single channel → trivial; `dra.mod` pat 106 (chans 0+2) is
  the local multi-channel case, now oracle-verifiable.

**Deliverable of this phase:** a one-page truth table per format
(E60 / E6x first / E6x repeat / E6x expire / multi-channel same row /
cross-pattern) with the exact post-state, hand-derived from the lines
above. This table is the oracle for the Tier-1 unit tests.

---

## 2. Corpus survey (scope — keep it honest)

Measured (MOD, `examples`-style scan): of 9 MODs, E6x appears in 4;
**multi-channel-in-one-pattern in exactly 1** (`dra.mod` pat 106,
chans 0,2). barmansp/GURU/jimisdea are single-channel.

→ **Per-channel counters are a low-frequency edge case.** The end_tick
fix (#1) alone restores audible notes for ~every real module. Treat #2
as fidelity polish, scoped by evidence.

**Task 2.1** — extend the scan to XM (ft2-clone loader or the project's
own importer) and IT/S3M; count: (a) modules with E6x, (b) modules with
multi-channel E6x in one pattern, (c) modules with nested/stacked E6x,
(d) modules relying on cross-pattern loop-state persistence. Output a
table; let the counts decide how far Phase 4 goes.

---

## 3. Fix #1 — clips cover loop repeats (per-pass clip emission)

**Goal:** notes follow the walker's existing loop expansion.

**⚠️ "Extend `end_tick`" is NOT enough — disproved empirically.**
Inspecting barmansp order 2 (TimelineMap + clips):
- The walker already emits the repeat (rows 0x30–0x3F at ticks
  1344–1404, `loop_iter=1`). ✓
- But ch0's loop body is **split into 6 instrument-segments** (tracks
  6–11, src_rows 0x00/0x30/0x32/0x34/0x3C/0x3E), each its own clip.
- `SortedClips::active_at` returns the clip with the **greatest
  `position_tick ≤ tick`**. At the 2nd-pass tick 1344 (row 0x30) that
  picks track 11 (`pos 1336`, src_row 0x3E), **not** track 7 (src_row
  0x30). The `local_row < source_start_row` guard then drops it →
  still silent. Merely widening `end_tick` cannot fix a segmented loop
  body, because the wrong segment wins `active_at`.

**Correct change (importer only, `segments.rs`, no core):** **emit
duplicate clips per loop pass.** Today clips are built from
`entry_points` = first occurrence per `(song, order_idx)` with
`loop_iter == 0` (`:192`). Generalise to iterate **every contiguous
timeline run of the order** (each `loop_iter` generation, and each
distinct tick-run), and for each run emit a clip for every segment whose
source rows overlap that run's row range, with:
- `position_tick` = the run's tick at the segment's first visited row,
- `end_tick` = tick just past the run's last visited row (bounded to the
  run, preserving the half-open anti-leak guarantee),
- `source_start_row` / `track_row_offset` unchanged (the segment's track
  rows are reused as-is).

So the 2nd pass gets its own clips at `pos 1344…`, and `active_at` picks
the right segment for each repeated row.

**Why it's generic & safe:**
- Played row comes from the timeline entry, not tick math → correct
  source row on every repeat.
- Each pass's clips are positioned at that pass's ticks, so `active_at`
  resolves the correct segment (the segmented-body failure above is
  fixed at the root).
- Per-pass `end_tick` stays within the order → no leak into the next.
- The `loop_iter == 0` path is unchanged ⇒ loop-free and
  single-segment modules stay **byte-identical** (tick-exact).

**Design conflict found while implementing — overlap guard (IMPLEMENTED).**
The existing design deliberately emits **one clip per order position**
even for loops (test `pattern_loop_no_extra_clips`,
`cycle_exact_layer.rs`): when the loop body is **one unsegmented run**,
that single clip's `end_tick` already reaches order-end and *spans* the
repeat ticks, and the runtime resolves the repeated rows via their
timeline `row_idx`. So a per-pass clip there is both redundant **and**
an illegal overlap (`verify_layers_consistent` → `ClipsOverlap`).
Resolution: **only emit a repeat clip where it fills a real gap** — skip
the push when the candidate `[position_tick, end_tick)` overlaps any clip
already on the lane. Net behaviour:
- segmented loop body (barmansp ch0) → real gap → clip kept → fixed;
- single-segment loop at pattern *end* (barmansp ch1–3: the spanning
  clip ends exactly at the repeat start, no span) → gap → clip kept;
- single-segment loop with *trailing rows* (the synthetic tests: spanning
  clip reaches order-end) → candidate overlaps → skipped → stays 1 clip,
  correctness preserved by the spanning clip.
- Graceful-degradation corner: a multi-segment loop body *with trailing
  rows* — the trailing segment's spanning clip can shadow an earlier
  segment's repeat clip (overlap → skipped), so that earlier segment's
  repeated rows stay silent. No regression (silent today too); flagged
  for Fix #2-era follow-up if a real module needs it.

**Verify:** A/B barmansp (2nd pass replays F-2/D-4/D-4/B-2…, all 4
channels, via `DebugObserver` + audio); full xmrs suite green;
tick-exact non-regression on loop-free modules; spot-check GURU.MOD /
jimisdea.mod (single-channel loops) audibly intact.

---

## 4. Fix #2 — per-channel loop counters in the walker

**Goal:** the emitted TimelineEntry sequence matches the reference
replayer for multi-channel / stacked / expiring / cross-pattern loops.

### 4.1 Walker state refactor (`timeline_map.rs`) — **DONE**
- Per-channel state: `loop_anchor: Vec<usize>`, `loop_counter:
  Vec<Option<usize>>`, sized from the **pattern width** (`nch`), NOT
  `module.get_num_channels()` (which reads from clips → 0 before the
  walker runs; that latent trap is documented inline).
- Per-row collection iterates `row.iter().enumerate()`; the shared row
  pointer is redirected by the **last channel** asserting a loop jump
  (ascending = highest wins). `None / Some(0) / Some(n)` transitions are
  byte-identical to the former global model, so a single looping channel
  emits the same sequence (verified: `cycle_exact_layer` still tick-exact,
  full suite + barmansp green).
- The per-channel counter vector is part of the `visited` cycle key so
  two visits under different loop state aren't a false cycle.
- Tier-1 tests added: `pattern_loop_on_nonzero_channel_still_repeats`,
  `it_active_loop_suppresses_same_row_break`.

### 4.2 Format gating — orthogonal quirks (NOT a dialect enum) — **DONE**
The repo's quirk philosophy is "one boolean per *behaviour*, named for
what it does, never for which tracker" (`compatibility.rs` header). So
**no `PatternLoopDialect` enum.** Instead:
- **`active_loop_suppresses_break`** (NEW core quirk): a same-row
  `PatternBreak` is ignored while any channel's loop still iterates. IT
  on (`it214`/`it215`); XM/MOD/S3M off.
- **`pattern_loop_resumes`** (existing quirk, was **dead** — defined +
  set in `st3()` but never consumed): now consumed — on counter expiry
  the channel re-anchors to `row + 1`. S3M on; **also turned on for IT**
  (schism `patloop_row = row + 1`); FT2/PT off.
Both gated through `module.quirks`, set in `tracker/profiles.rs`.

### 4.3 FT2 overflow guard — deferred
Port the `song.row >= currNumRows → row 0` clamp (`ft2_replayer.c:2344`)
into the walker's post-loop row advance, gated to `e60_leaks_to_next_pattern`
(or a dedicated quirk). Not yet done — see §7 (M1).

### 4.4 Loop × other-navigation-effect precedence (partial)
The walker now models **one** verified combo (IT break-suppression,
`active_loop_suppresses_break`). The remaining combos are still on a
**fixed** precedence `PositionJump > PatternBreak > Loop` and are NOT
yet faithful — see §7 (M2). The truth table (§1) must cover:
A faithful loop rule in isolation is **not enough**: the walker applies
a *fixed* precedence `PositionJump > PatternBreak > PatternLoop`
(`timeline_map.rs:185-221`), which is an assumption, not a measured
fact. The reference replayers resolve `pBreakPos` / `posJumpFlag` /
pattern-loop in a specific order when several land on the same (or
adjacent) row, and pattern-delay (EEx/SEx) re-runs the row's effects on
each repeat — which re-fires E6x. **The truth table (§1) must therefore
cover combinations**, not just loop-alone:
- E6x + Dxx (break) same row, E6x + Bxx (jump) same row,
- E6x under EEx pattern-delay (does the loop counter advance per
  delayed repeat?),
- E6x + speed/BPM change on the loop row,
- E6x with no prior E60 (default anchor per dialect).
Derive each post-state from FT2/IT source; these combos are where
"definitive" is actually won or lost.

---

## 5. Verification (3 tiers, project-standard)

- **Tier 1 — synthetic unit tests** (`timeline_map.rs` `#[cfg(test)]`,
  alongside the existing `walker_wrap_tests`): craft patterns for each
  truth-table row (single E6x, two-channel same-row E6x, stacked E60s,
  counter-expire re-arm, cross-pattern persistence, E6x with no prior
  E60). Assert the exact emitted `(order_idx,row_idx,loop_iter,tick)`
  sequence against hand-derived reference values from §1.
- **Tier 2 — differential oracle vs the real replayer:**
  - XM → instrument `ft2-clone` (or read+hand-derive) to dump the
    row-visit sequence; compare on crafted + real XM with E6x.
  - IT → same via `schismtracker` `fx_pattern_loop`.
  - MOD → instrument `pt2-clone` (`jumpLoop` `:312`, `pBreakPosition`
    write at `:330`) to dump the row-visit sequence; compare on crafted
    MOD + `dra.mod` (pat 106 multi-channel). Real PT ground truth — no
    longer a proxy.
  Reuse the harness shape of `examples/dw_oracle_trace.rs` (row/tick
  capture) but target the tracker sequence, not Paula regs.
- **Tier 3 — ear/trace check:** the real modules surfaced by the §2
  survey, especially `dra.mod`; confirm audio + `DebugObserver` trace
  match the oracle.

---

## 6. Sequencing & risk

1. **Fix #1** (end_tick) — high value, generic, importer-only, low risk.
   Ship + verify first; this alone fixes barmansp and every
   single-channel loop in the corpus.
2. **§2 survey** — decide whether Phase 4 is worth its complexity.
3. **Fix #2** (per-channel counters) — only if the survey shows real
   multi-channel/stacked usage. Gate every quirk; keep the
   `loop_iter==0` fast path byte-identical so loop-free and
   single-channel-loop modules stay tick-exact.

**Risks:** (a) core quirk authorization (§4.2); (b) IT pattern-entry
reset rules are subtler than FT2 — verify, don't assume; (c) the FT2
cross-pattern persistence quirk can change downstream order length —
guard the walker's `MAX_ROWS` cap and confirm no new infinite loops.

**Non-goals:** per-channel *row divergence* (channels on different rows
simultaneously). Standard E6x never needs it; the existing
`ChannelLoop` / per-lane free-run path (DW/Whittaker) already covers the
genuinely-divergent case and is out of scope here.

---

## 7. Quirk reuse & migrations survey (post-Fix-#2)

Goal: is the new `active_loop_suppresses_break` (and the now-live
`pattern_loop_resumes`) needed anywhere else, and what small migrations
fall out? **Survey result: the wiring is already clean** — the findings
below are mostly "verified safe", with three bounded migrations.

### 7.1 Where the pattern-loop quirks are consumed
- **Single interpreter.** `NavigationEffect::PatternLoop` is read in
  exactly **one** place: the walker (`timeline_map.rs`). `edit.rs`,
  `extract/mod.rs`, `core/duration.rs` only *mention* it in comments or
  build test patterns. So there is **no duplicated navigation logic to
  migrate** — the quirks land everywhere automatically.
- **Duration estimator** (`core/duration.rs`) runs through
  `build_timeline_layer` → same walker → already covered. No re-walk.
- **Runtime sequencer** drives the pre-expanded `TimelineMap`; it never
  re-interprets loops, so no runtime consumer needs the quirks.

### 7.2 Profile wiring — verified complete
Every importer sets a profile, so the quirks reach every format:
IT → `it214()` (`it_module.rs:606`), MOD → `pt()`
(`amiga_module.rs:425`), SID → `pt()`, XM → `ft2()`
(`xmmodule.rs:184`), S3M → `st3()` (`s3m_module.rs:1061`). `it215()`
delegates to `it214()`, so IT 2.15 inherits the new quirks. **DW does
NOT use `NavigationEffect::PatternLoop`** (own command set) → untouched.

### 7.3 `get_num_channels()`-before-clips trap — audited, contained
The walker was the **only** consumer of `get_num_channels()` that runs
*before* clips exist (it returns 0 then). Fixed by deriving `nch` from
the pattern grid. The other two callers (`module.rs:538`, `:561`, inside
`row_at*`) run at playback time when clips exist → no latent bug. No
migration needed, but **add a doc-comment on `get_num_channels()`**
warning it is clip-derived and 0 pre-build (cheap; prevents the next
pre-clip caller from re-hitting this).

### 7.4 Migrations (bounded, prioritised) — **M1/M2 DONE, M3 closed**
- **M1 — `e60_leaks_to_next_pattern` — DONE.** Consumed in the walker via
  a per-song `ft2_leak_row`: an E6x **jump** records its loop-start row;
  FT2 (unlike PT) never clears it in the loop branch, so a **natural**
  pattern-end starts the next order at that row, clamped to the next
  pattern's length. Gated on the quirk (off → no-op → byte-identical).
  Test `ft2_e60_leaks_loop_start_row_to_next_pattern`. Closes §4.3.
- **M2 — loop+break clear divergence (PT vs FT2) — DONE.** New quirk
  `pattern_loop_break_resets_row` (PT on, via `pt()`): when a loop jumps
  and a `PatternBreak` lands on the same row, the break still wins the
  order advance but lands at **row 0** (PT clears `pBreakPos`) instead of
  the break target (FT2 keeps it). Consumed in the break branch. Test
  `pt_loop_plus_break_lands_at_row_zero`.
- **M3 — cross-pattern persistence — CLOSED (not needed).** §2.1 survey
  ran on the local XM corpus (15 parsed): 4 use E6x; **multi-channel
  E6x is real** (`strobe_-_one_for_all.xm` pat 0, ch 2/5/6 — already
  handled by the per-channel walker) and **stacked single-channel E6x is
  real** (`strobe-paralysicical13.xm` pat 10 ch 3 = 12 cmds — handled by
  per-channel state + `pattern_loop_resumes`). But **no module relies on
  a loop counter surviving a pattern boundary** (that needs a counter
  armed-but-incomplete at pattern end — pathological). The gate condition
  is unmet → `pattern_loop_persists_across_patterns` is **not** added.

### 7.5 Pre-existing clip bugs surfaced by the M3 survey (NOT loop-Fix regressions)
Loading the real E6x XM modules exposed two **pre-existing** layer
inconsistencies (proven: present with `emit_loop_repeat_clips` disabled
AND `e60_leaks` forced off; the affected loops are single-channel so the
walker is byte-identical to the pre-Fix-#2 global model):
- **Degenerate first-pass clips (`ClipsOverlap`) — FIXED.** When a loop
  revisits rows, `find_entry_at_order` / `clip_end_tick_from_timeline`
  (both `loop_iter==0`-only) can resolve a segment's start tick *after*
  its end tick → a clip with `end_tick <= position_tick`. Such a clip
  covers no runtime ticks (audio-neutral) but trips the overlap check.
  Guard added in `extract_tracks_and_clips`: skip clips where
  `end_tick <= position_tick`. Byte-identical for non-loop modules
  (forward-spanning clips never trigger it). 22/24 `~/Music` modules now
  verify clean (was failing on 2).
- **`TrackInstrumentMismatch` (out-of-range instrument) — FIXED.**
  `lamb_-_among_the_stars.xm` cites instrument 30 (2 declared);
  `strobe-paralysicical13.xm` cites 17 (17 declared). FT2 keeps all 128
  XM slots allocated, so a reference past the declared count hits an
  empty slot → silence; importers trim to the declared count, leaving
  the index out of range. Fix `pad_instruments_to_track_refs` (in
  `build_timeline_layer`, format-agnostic): pad `module.instrument` with
  `Instrument::default()` (empty) slots up to the max Track reference.
  Test `out_of_range_instrument_reference_pads_instrument_vec`. lamb now
  verifies clean.
- **`AutomationPointsUnsorted` (`strobe-paralysicical13.xm` only) —
  FLAGGED, out of scope.** A third, independent pre-existing
  inconsistency (lane 24), unmasked once the two above were fixed
  (verify checks #3/#4 ran first). Confirmed pre-existing (present with
  `emit_loop_repeat_clips` disabled). A song/automation-extraction bug
  unrelated to the loop walker — separate follow-up. 23/24 `~/Music`
  modules verify clean; this one module fails only on this.

### 7.6 Verification — Tier-2 reference oracle (DONE)
`tests/loop_reference_oracle.c` is a standalone C cross-check: the loop +
break + row-advance state machines transcribed **verbatim** from the
reference replayers (pt2-clone `jumpLoop`, ft2-clone `patternLoop`,
schism `fx_pattern_loop`). It prints the visited `(order:row)` sequence
per dialect for 3 crafted cases:
- **A** single loop (E60/E61) — all dialects identical;
- **B** E60-leak — **FT2** starts the next order at the loop-start row,
  **PT/IT** at row 0 (validates M1);
- **C** loop-jump + same-row break — **PT** next order row 0, **FT2**
  break target, **IT** break suppressed then fires (validates M2 + the
  IT `active_loop_suppresses_break`).

The xmrs walker reproduces the C oracle **byte-for-byte on all 3 cases ×
3 dialects** — baked in as `walker_matches_reference_replayer_oracle`
(`timeline_map.rs`). Full suite green: **287 lib** + `cycle_exact_layer`
byte-identical + DW oracle.
