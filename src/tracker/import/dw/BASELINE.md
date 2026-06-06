# DW Tier-2 fidelity — BASELINE

Frame-to-frame comparison of the **xmrsplayer** per-tick state against the
**Ghidra Paula oracle** goldens (one per player cluster, see
`oracle/README.md`). This is the measured starting point that the modeling
backlog in `PLAN.md` is meant to close — *not* a pass/fail gate.

## How this was produced

1. **Oracle side** — `oracle/*.csv`: the real 68k replayer emulated in Ghidra,
   one row per 50 Hz tick, capturing each Amiga channel's gate (DMA bit),
   `AUDxPER` (period) and `AUDxVOL` (0..64). See `oracle/README.md`.
2. **Player side** — `xmrsplayer/examples/dw_oracle_trace.rs`: renders the same
   `.dw` through `XmrsPlayer`, advancing **one replayer tick per call**
   (`advance_trace_tick`) and snapshotting each channel
   (`channel_snapshots()` → `ChannelSnapshot { period, volume, gate }`,
   units chosen 1:1 with Paula). Emits the *same CSV schema* as the goldens.
   ```sh
   cargo run --release --no-default-features --features std,import_dw \
     --example dw_oracle_trace -- "<file>.dw" 1500 /tmp/player_<key>.csv
   ```
3. **Diff** — `oracle/diff_baseline.py --markdown`: aligns the two CSVs
   tick-by-tick (player tick N ↔ golden tick N — init is separate on both
   sides, so tick 0 lines up) and reports per channel:
   * **gate✓** — fraction of ticks where player gate == golden gate.
   * On **both-gated** ticks only (an ungated Paula channel's PER/VOL are
     stale scratch and don't affect audio):
     period exact %, **median signed Δ** (the systematic offset),
     mean-abs-Δ (MAE, the drift), max |Δ|; same for volume.

All runs are song 0, 1500 ticks (≈30 s at 50 Hz).

## Results (2026-06-03; rev. SeqPtr loop fix — bad company)

**Changelog**
- *2026-06-05* — **Phase C cont.: the desync residuals are largely MEASUREMENT /
  CAPTURE artifacts too, not player bugs.** Sub-classifying the remaining
  strike-issue modules by (gate%, medΔ, perMAE, captured entry):
  * **beast1.04 / castle / forest / gameover / underground** (gate ~20 %, perMAE
    **1.5**): the notes are *perfect* where they overlap — beast1.04 ch1's first 8
    strikes are byte-identical to the oracle — but the oracle golden plays the
    short song **once and stops** (~t268, speed 28) while the player **loops** it,
    so gate-agree collapses over 2000 ticks. A capture/comparison artifact (oracle
    doesn't loop), not a fidelity bug. (Also: the batch first picked the 0x0/0xe
    wrapper entry for these; the engine is beast1.title's 0x66/0x1b6 — but both
    give the same short capture, so the entry isn't the cause.)
  * **genuine per-module fidelity issues remain** but are a smaller, heterogeneous
    set: obliterator-ingame / wrath of the demon (medΔ≈0 but occasional huge |Δ|
    up to ~1000 → a specific note/command mishandled now and then) and kickstart
    ii / xenon (P2, bigger period spread — likely a slide/porta rendered as
    discrete notes). bubble C3 (gate 21 %) is the known custom-command case.
  Net: after the frozen-frame fix + this triage, the importer is in much better
  shape than the raw audit implied — the alarm was dominated by (1) the frozen
  -frame struck artifact and (2) short-song-vs-loop capture mismatch. Next:
  improve the comparison (loop the oracle / diff first-pass only) to de-noise the
  audit, THEN per-module RE on the genuine handful.
- *2026-06-05* — **De-noise DONE + genuine set isolated to kickstart/xenon.**
  `diff_baseline.compare(clip=True)` (new `oracle_last_active`) clips to the
  oracle's last gated tick + window — the silent post-stop tail no longer
  manufactures fake gate-disagreement / spurious strikes. strike-clean **57→61**;
  beast1.* family fell from huge desync to 2/8 stray strikes. A first-pass MAE
  probe (≤200/≤400 vs ≤2000 ticks) then split the big-perMAE list cleanly:
  * **late-desync / loop-phase artifacts** — *faithful first pass*, diverge only
    after ~400 ticks: obliterator-ingame (≤200 MAE 4/5/27/0!), days of thunder
    (2/0/2/5), beyond ice palace (6/9/2/6), gunship2000 (ch1-3 perfect), wrath
    (partial). NOT fidelity bugs — the player renders correctly and just drifts
    from the oracle's loop phase late in the 2000-tick window. (My earlier
    "occasional |Δ|~1000" guess for obliterator/wrath was this artifact.)
  * **genuine, wrong from tick 0** — **kickstart ii** (MAE 100-276 on ALL 4 ch at
    ≤200) and **xenon** (ch0=90, ch3=236 at ≤200). kickstart = `variant=New
    period=P2`, `arp=true` (32 arp tables, 30 non-trivial) yet `events:arp=0`;
    oracle shows per-frame modulation the player misses + wrong notes (ch2 oracle
    266 vs player 534 = exact octave; ch0/ch3 non-octave ratios). This is the
    arpeggio-per-frame class (cf. tetris/bubble lane work) plus an octave/table
    issue. **The genuine remaining fidelity work = the period-space pair
    kickstart+xenon** — a per-module Ghidra RE, not yet started; everything else
    in the corpus audit is now accounted for as measurement artifact.
- *2026-06-05* — **kickstart/xenon were NOT a fidelity bug; corpus audit now
  FULLY clean.** The Ghidra RE of kickstart's replayer showed the period math is
  byte-identical to the importer (`PERIODS2[note] × (0x369E99/freq) >> 10`, the
  `instr[+0xA]` multiplier `init`-confirmed) — so the 150-240 MAE could not come
  from rendering. Root cause: `pick_main_sub_song_index`'s "longest position
  list" heuristic picked the **wrong main sub-song** (kickstart→#1, xenon→#2)
  while the replayer default (and the oracle golden) is **sub-song 0** (`init`
  reads `d0`; canonical call is `d0=0`). `to_module` renders `[selected,others…]`
  so "song 0" became the wrong tune. **Fix** (dw_module.rs): prefer sub-song 0
  unless it's a degenerate leading stub (position-list score ≤ 8 — the beast1.*
  jingle case). kickstart MAE 240→**5-9**, xenon 152→**11** (Paula-oracle
  verified at default selection); only 2 / 113 modules change, all suites green
  (xmrs 288, player 32+8). Every remaining audit flag is now a measurement
  artifact (frozen-frame / loop-phase / strike-window ±12), not fidelity.
  *(Tooling note: `DW_SUBSONG=0` hits the override hook and differs from unset,
  which runs the heuristic — always regenerate `/tmp/ptr` after an importer
  change before trusting `audit.py`.)*
- *2026-06-05* — **Phase C: the "systematic over-trigger" was a trace-harness
  artifact, fixed; importer re-strike handling validated corpus-wide.** The ~15
  over-triggering modules (crossbow-title +113, custodian +97, aquaventura/menace
  +250…) ALL had `delay_counter=true`. Cause: on a frozen (delay-carry) frame the
  trace (`dw_oracle_trace.rs`) snapshots WITHOUT advancing, so `struck_this_tick`
  from the prior real tick was re-reported → a phantom second strike at every note
  onset (the real replayer skips its whole channel loop on a carry frame). One-line
  fix: emit `struck=0` on frozen frames. After: those 6+ modules → spurious=0
  missed=0; corpus **strike-clean 35→57, pure re-strike bugs 23→12** (now 1 over,
  11 under). The remaining unders are NOT re-strike-semantics bugs: totalrecall
  -ingame (−105) is a **late-song desync** (ch1/ch2 freeze on one note ~t1670 while
  the oracle plays on); the small ones are varied (weird dreams −4 = a section
  re-trigger missed every 384 ticks; loopz −6 = late desync; beast1.title −2
  isolated). **No dominant shared re-strike bug remains — the cmd-0x83 tie was the
  real one.** The big residuals are the separate desync/period-space class
  (kickstart/xenon perMAE 150-240, bubble C3 gate 21 %). The `struck` metric is now
  trustworthy on delay-counter modules.
- *2026-06-05* — **A2: whole-corpus Paula oracle + strike audit.** Headless batch
  (`oracle/capture_all.sh` + `analyzeHeadless` + `DwPaulaOracle.java`, entry points
  from `examples/dw_oracle_args` heuristics + `oracle/known_args.tsv` overrides)
  captured **113/120 modules** into `oracle/corpus/*.csv` (every real song except
  one). carrier command (init 0x48/play 0x152 — short song, the dead tail tripped
  the validator) and snow strike (init 0x27a/play 0x3be/a3 0xfffffffe — needed the
  detect-init × lea-play cross the candidate list lacked) were hand-RE'd and added
  to `known_args.tsv`; `capture_all.sh` now includes that cross candidate and
  accepts short songs (abs alive count). The 7 remaining: 6 SFX/non-song banks
  (no play routine — expected) + **chip's challenge** (the one real gap: 12
  sub-songs, self-bases A3=0xfffffffe at play=0x4e6, init runs away from 0x3a4 —
  documented in `oracle/KNOWN_GAPS.md` for a future session). `oracle/audit.py` diffs every corpus golden vs
  its player trace and ranks. **Result: 35 strike-clean, 76 with strike issues, of
  which 23 are PURE re-strike bugs** (gate ≥85 %, perMAE ≤15 → notes match, only
  re-attacks differ): **15 over-trigger** (crossbow-title +113, custodian +97,
  gazza 2 +71, bond+/licencetokill +64, aquaventura/menace +250-300 at gate 84),
  **8 under-trigger** (totalrecall-ingame 1 −105, loopz/gold-of-aztecs −6, weird
  dreams, beast1.title −2). Verified NOT an oracle artifact: the oracle detects
  plenty of strikes (164/223/244…), the player consistently emits ~15-30 % more
  with `missed=0` — a **systematic over-triggering across the canonical family**,
  invisible before A1. The other 53 strike-issue modules are desync/period-space
  (bubble C3, beast1.* low-gate, kickstart/xenon high-perMAE = period space). This
  is the Phase-C worklist; likely a shared tie-like / portamento note-semantics
  root cause (cf. the cmd-0x83 fix). The pkill footgun (`pkill -f analyzeHeadless`
  self-matches its own cmdline → kills the shell, exit 144) cost time — noted.
- *2026-06-05* — **A1: per-channel `struck` re-attack signal added to the oracle,
  trace, and diff — closes the re-strike blind spot.** `gate+period+volume`
  aliases a held/tied note and a re-struck one (both gate=1, continuous period,
  same volume curve), which is why the 0x83 bug was invisible. New `cNs` column:
  oracle = a DMACON 0→1 edge (Paula sample reload); player = a `trigger_pitch`
  sample-reset; `diff_baseline.py` windowed-matches them (±12 frames, absorbs the
  delay skew) and reports per channel **spurious** (player re-attacks with no
  oracle counterpart) and **missed** (oracle re-attacks the player skipped). All
  9 cluster goldens re-captured with the new schema (gate/period/volume bytes
  unchanged vs the prior baseline — verified identical). **First audit findings
  (the new objective worklist):**
  - clean (spur=0 miss=0): bad company, grimblood, qball, leviathan, empire,
    obliterator. **bad company & empire (tie families) are now strike-perfect —
    independent confirmation of the 0x83 fix.**
  - ⚠ **xenon2** spurious 6/5/12/10 — residual over-triggering beyond the
    row-0x38 0x83 we fixed (more re-attacks the importer emits than the replayer).
  - ⚠ **bubble bobble** spurious 40/16/39/69 — heavy over-triggering, but tangled
    with the known C3 desync (gate-agree only 29%); treat after C3.
  - ⚠ **tetris** missed 2/0/12/2 — ch2 *under*-triggers (skips 12 re-attacks);
    likely linked to its 28.6 period residual (sub-row arp timing).
  Regenerate: re-run the `DwPaulaOracle` captures (entry-point table in
  `oracle/README.md`) then `diff_baseline.py --markdown`. Player suites green
  (32+8, gate 116, oracle 5).
- *2026-06-05* — **cmd 0x83 = TIE (held note), not re-strike, on the suppression
  families (xenon2 / beast / bad company / empire).** User spotted xenon2 title
  ch1 row 0x38: a B-3 re-attacking where a decaying held note was expected. Paula
  oracle confirmed: ch1 real note-ons at ticks 78/104/260/286, **no gate drop**
  between 104–259 → one B-3 held from row 0x20 with volume decaying 64→48→32→16,
  row 0x38 (tick ~168) is mid-note. Ghidra `play_tick`: the one-frame-early DMA
  clear at counter==1 is guarded by `CMPI.B #$83,(A1)` (`0C 11 00 83` @0x29c) — if
  the next byte is `0x83` the clear is skipped, so `0x83` re-affirms an already-on
  channel = **no 0→1 strike = tie**. Families WITHOUT the guard (bubble bobble /
  grimblood, always clear) genuinely re-strike. Both `0x83` jump-table targets
  start with `MOVE.L A1,(0x4,A0)`, so `classify_handler` can't distinguish them —
  the play-loop guard is the only discriminator. Fix: `detect_features` sets
  `note_repeat_is_tie` from the `0C 11 00 83` quad; `event.rs` routes
  `K::Retrigger → WaitUntilNextRow` (the existing hold semantics, reload
  `speed_counter` from `next_wait_frames`) when set, else keeps `Retrigger`.
  Signature present in xenon2/beast/bad company/empire, absent in
  bubble/grimblood/obliterator (verified Ghidra `search_byte_patterns`). After:
  xenon2 ch1 holds the B-3 with decaying volume, no re-attack at row 0x38; oracle
  diffs unchanged on all tie families (bad company ch0/1 4.8/12.9 etc. = the
  pre-existing vibrato residual), volume near-exact. **Methodology note:** the
  bug was oracle-invisible because the player's sample-restart doesn't drop the
  trace gate (a real re-strike shows as a 1-frame gate=0 in the oracle; ours
  stays 1) — `gate+period+vol` aliases a tie and a re-strike. A per-channel
  "struck" signal on both sides would catch this class directly (deferred). Suites
  green (xmrs 288, player 32+8, gate 116, oracle 5+4), zero regression.
- *2026-06-04* — **multi-sub-song cap raised `MAX_SONGS` 4 → 32 (expose game music
  banks fully).** grimblood ships 18 sub-songs but the importer only exposed 4
  (player showed `1/4`); the other slots were filled with short cues/jingles
  (sub#1 6 clips, sub#2 3 clips) so songs 2-4 sounded like fragments, not a bug.
  grimblood is a game bank: **one main tune (sub#0, 130 clips) + 17 short cues /
  jingles / entry-point variations** (sub#3-#11 are the same theme started at
  different positions; #13-#17 are slow delay-199/241 cues that render silent).
  The original "exhaust memory" rationale was overcautious — measured cost of
  exposing all is modest (the heaviest 21-song modules loopz / gold-of-the-aztecs
  render in ~20 ms / ~5400 clips; looping-song sim is already frame-bounded), so
  32 covers the corpus max (21) with a runaway guard. Order is `[selected, 0, 1,
  …]`, so song 0 (oracle-verified) and the first 4 entries are byte-identical →
  zero fidelity-test change. grimblood now reports `4/13` (the 5 silent trailing
  cues add no timeline entries, so `song_count` excludes them). Corpus: 42/116
  modules have >4 sub-songs. Suites green (xmrs 288, player 32+8, gate 116, 0 reg).
- *2026-06-04* — **sentinel song-end fixed: 3-voice StopSong = `DMACON=0x0007` via
  BSR (ear-check NOK → Ghidra).** sentinel sounded wrong; the golden plays a ~108-
  tick intro then all channels go silent (`dma 0x207→0x200`, a StopSong) but the
  importer never halted (cmd 0x84 → `Unknown`) → it held the looping samples through
  the silence (gate-agree **7 %**). Ghidra: sentinel's real play body is `0x136`
  (the importer detected the DMA-start stub `0x106` — harmless, the dispatcher/
  period table are found independently and the notes match within the concert
  offset). Its 0x84 handler `BSR`s a silence helper (`0x9e`) that writes
  `DMACON = 0x0007` — clearing the **3** channel bits (sentinel is 3-voice), not
  `0x000F`, and via a call, not a bare BRA/inline. `classify_handler`'s DMACON-kill
  detection now (a) matches `0x0007` as well as `0x000F` (`dmacon_kill_in`) and (b)
  resolves `BSR.W`/`BSR.B`/`JSR d16(PC)` targets, not just `BRA.W`. **sentinel
  gate-agree 7 %→97 %, MAE 11/15/7** (concert offset). Golden re-captured truncated
  at the stop (`f0=0x52c`, 111 ticks). **Zero corpus regression** (13 reps
  byte-identical; xmrs 288, player 32+8, gate 116, robustness 116/0-panic).
- *2026-06-04* — **Song-end resolved (archipelagos/fright night): tracker loop +
  consistent golden capture, no core change.** The "infinite loop" after StopSong
  was NOT a playback bug: the real replayer kills itself (cmd 0x84 → `DAT_630=0`,
  plays once), but trackers have no silent-stop — they loop (PositionJump →
  `song_loop_to`). The Paula oracle's silent tail is just it never re-initing. The
  real issue was an inconsistent golden: `bubble_bobble.csv` was captured truncated
  at its stop (445 ticks, via the `f0` playing-flag option) but archipelagos/fright
  night were captured untruncated (1500). Fix (2 parts, zero core change): (1)
  `to_module` sets `song_loop_to=Some(0)` when a song halted via StopSong (the
  tracker loop-to-start idiom — existing mechanism; previously only set when a clean
  loop period was found); (2) re-captured `archipelagos.csv`/`fright_night.csv`
  truncated at the StopSong (`f0=0x630`). **archipelagos gate-agree 22%→97/96/96 %;
  fright night 15%→94/90/98 %, MAE 1.0/2.4/3.5 = near-exact** (no cmd 0x8a). **Zero
  corpus regression** (11 reps byte-identical incl. bubble bobble; xmrs 288, player
  32+8, gate 116, robustness 116/0-panic). archipelagos residual (MAE 120/80/44) is
  now purely the mid-track speed change (cmd 0x8a, PLAN #4) — separate.
- *2026-06-03* — **Song-end: inline DMACON-kill (cmd 0x84) now detected → StopSong.**
  Traced the archipelagos song-end in the Ghidra emulator (per-frame global-state
  dump): the playing flag `DAT_630` clears + `dma 0x207→0x200` exactly when ch0
  reads byte `0x84` at track offset 0x8b0 — i.e. cmd 0x84 (StopSong), whose handler
  does `DAT_630=0; DMACON=0x000F` **inline** (kill @0x5a2). (`0xf` clears bits 0-3 =
  the 3 channel DMA bits via AND-NOT, leaving the 0x200 master — that's why it's
  0x200, not 0xf.) The command_map classifier only recognised the DMACON-kill via a
  bare `BRA.W` handler (bubble-bobble family); archipelagos/fright night inline it,
  so 0x84 fell to `Unknown` (inert) → the importer over-read the track (182 notes)
  and looped forever. Fix: `classify_handler` now also matches the kill signature
  **inline in the handler body** → `ResetAll`/StopSong. **The sim now halts at 365
  frames (golden ~346); ch0 = 7 notes, not 182.** Zero corpus regression (11 reps
  byte-identical, gate 116, xmrs 288). **Incomplete for a full oracle match:** the
  player still *holds* the last note past the stop (DW renders no note-off/rest as
  silence) and the mid-track speed change (cmd 0x8a, PLAN #4) still drifts the
  intro — both are rendering/quantization work, not detection. See memory
  `project_xmrs_dw_archipelagos_3voice`.
- *2026-06-03* — **3-voice sub-song format detected (fixes 6 mis-parsed modules).**
  The 8-byte sub-song row is two shapes: 4-voice `[ch0..ch3]` (header 0) and
  **3-voice `[param(2B), ch0, ch1, ch2]`** (header 2). The importer assumed
  header-0/4-voice for all 8-byte rows → on the 3-voice ones it read the **param
  word as ch0's base** (which lands in the period table), shifted real ch0/1/2 into
  ch1/2/3, invented a phantom ch3, and used the wrong default speed (6). Fix:
  `detect.rs::find_sub_song_header` reads the header displacement from `init_main`'s
  indexed channel-base load (`30 70 00 <d8>` after `mulu #8`) → `sub_song_header`;
  `parse_sub_songs` derives `voices = (row-header)/ptr`, reads only that many
  pointers (rest → empty/silent), and reads the speed from the header (the param
  byte IS the speed — archipelagos = 7). **Ghidra-confirmed on archipelagos
  (`init`@0x54) and sentinel (`init`@0x0)**: both `mulu #8` + channel base at `+2`
  + loop `!= 3`. Fixes all 6 eight-byte modules (archipelagos, fright night,
  sentinel, sidewinder, super wonderboy, crossbow-ingame — the last four incl. the
  PLAN's "≈3-voice" set). **A/B archipelagos: intro now faithful (period medΔ
  +44→+1, t0-12 matches the golden ±1), phantom ch3 silenced (gate-agree 100 %).**
  **Zero regression**: 11 cluster reps byte-identical, gate 116, robustness
  116/0-panic, suites green (xmrs 288, player 32+8). Residual (separate, NOT the
  3-voice structure): archipelagos/fright night play the intro once then the real
  replayer falls silent (golden silent t345-6000) while the importer loops /
  over-reads the track — a song-length/track-end issue. See memory
  `project_xmrs_dw_archipelagos_3voice`.
- *2026-06-03* — **Arpeggio lane verified at scale + 3 new heavy-arp goldens.**
  Captured Paula goldens (`archipelagos.csv`, `fright_night.csv`,
  `buffalo_bills.csv`; all canonical `[B0,A0,90]`, init/play `0x0`/`0xe`) for the
  three heaviest arpeggio users (426 / 416 / 111 arp commands) to validate the new
  per-frame Points lane beyond bubble/tetris. Their parsed arp tables are clean and
  identical to bad company's (chord triads `[0,3,7]`/`[0,4,7]`/`[0,3,7,12]`, range
  `[0,16]` — no grimblood-style oscillator garbage). **buffalo bills: gate-agree
  94–96 %, period MAE 2.1–4.1, max ≤ 8** — near-exact, the arp (111 cmds) tracked
  within the concert offset → **lane faithful at scale**. ⚠️ **New finding (NOT
  arp), root-caused in Ghidra:** archipelagos + fright night are **3-voice modules
  mis-imported as 4-voice**. `init_main` (`FUN_00000054`) reads sub-song rows
  `[param(2B), ch0, ch1, ch2]` (8 B, `mulu #8` + loop `!= 3`); the importer reads
  4 channel pointers instead → it takes the **param word `0x0720` as ch0's base**
  (which lands in the period table at `0x6fc`) → garbage notes from t0 (gate-agree
  ~15 %, MAE ~497 even both-gated), shifts real ch0/1/2 into ch1/2/3, and invents a
  phantom ch3 (the real play body loops channels `0..2` only). Hence the infinite
  loop where the real 3 voices play one intro (~345 ticks) and fall silent. buffalo
  (same canonical replayer, 111 arps) is 4-voice and clean → confirms it's the
  3-voice sub-song format, not the replayer/arp. Goldens kept to drive the
  3-voice-detection fix (`detect.rs`); NOT passing baselines yet. See memory
  `project_xmrs_dw_archipelagos_3voice`.
- *2026-06-03* — **Per-frame pitch arpeggio → `TrackPitch` Points lane (faithful,
  replaces the lossy classic effect for DW).** Ghidra (`tetris FUN_0000010e` 2-bracket
  + `bad company FUN_000001b6` canonical 3-bracket, identical mechanism): the
  held-note branch advances a *per-channel* arpeggio pointer one offset **every
  frame** and adds the offset to the note's period-table *index* (= semitone
  space, `PERIODS[(arp&0x7f)+ch_tr+gl_tr+note]`); the pointer wraps at the `0x80`
  terminator and persists **across notes** (only a fresh `0x90`/`0xA0` arp command
  resets it). The importer was collapsing this onto `TrackEffect::Arpeggio{half1,
  half2}` (3-step, mod-3, trigger-row only) — wrong cycle length **and** stuck
  offset on held rows (tetris ch1/ch2 swung an octave at the wrong phase, MAE
  60/49). Now the simulator tracks the per-frame offset (`runtime.rs::tick_arpeggio`,
  emitting `TickEvent::Arpeggio` on *change*, with the replayer's gate-frame skip
  so the cycle stays phase-aligned across notes) and `attach_arpeggio_pitch_lanes`
  bakes it as a `LaneKind::Points` curve of `AutomationValue::Pitch` points on
  `TrackPitch`, evaluated **per frame** (player reads `current_abs_tick +
  current_tick_in_row`, not just the row start). Gated `use_arpeggio_pitch_lane`
  (every DW module **not** on the `command_map` pitch-envelope path); classic
  `Arpeggio` now only fires for non-DW formats. **A/B tetris: ch1 60.5→3.9, ch2
  49.0→28.6** (ch2 residual = arp-arm/re-arm sub-row timing, the row-quantised
  importer limit). **Zero corpus regression** — xenon2/beast/bad company/
  obliterator/leviathan/empire/qball/grimblood and bubble bobble (envelope path,
  unchanged) all byte-identical; suites green (xmrs 288, player 32+8, gate 116,
  robustness 116/0-panic). This is the first real producer for the core
  `AutomationValue::Pitch` primitive. Transverse: ~45 of 116 modules arm an
  arpeggio (heavy users archipelagos 426, fright night 416, buffalo 111) and now
  ride the faithful per-frame lane. The arpeggio **is** oracle-visible (the trace
  reports `effective_period` = AUDxPER), so the diff validates it. **bubble bobble
  migrated to the lane** (was the per-note pitch-envelope): **ch3 41.5→11.4**,
  ch0/1/2 unchanged. NB `command_map` is the canonical jump-table (most of the
  corpus), so the lane gate is `use_arpeggio_pitch_lane = !(command_map && P2)` —
  this excludes only the C4 **oscillator** family (grimblood, the sole
  `command_map+P2`; Ghidra `FUN_00000d6e` = period-space oscillator, not a
  table-index arp — the lane mis-rendered it 6.8→21.9, so it stays on its
  pitch-envelope). cosmic pirate (C4 but P3, arp_ev=0) is unaffected.
- *2026-06-03* — **C2 bad company finished: per-channel `SeqPtr` loop modelled.**
  bad company's ch0/1/3 start with an *intro* track that ends `SeqPtr(X) ;
  SeqAdvance` (cmd 0x89, Ghidra `Play` case 0x89 sets `chan+0x600`), redirecting
  the position sequencer to a sub-sequence at `X` that loops on *itself* — so the
  intro plays ONCE and only the body loops. The flat-array reader looped back to
  entry 0 (the intro), so every cycle re-played the intro while the oracle was
  mid-body (diverged hard at ~t560 = ch0/1/3 `first_wrap` 561). Fix:
  `event.rs` now surfaces `DwTrackEvent::SeqPtr(X)`; `follow_seq_ptr_loops`
  (`load`, before `parse_tracks`) rebuilds each intro-`SeqPtr` channel as
  `[intro] ++ sub-sequence(X)`; the simulator records `loop_start_frame` (first
  frame at the `loop_to` target) and `to_module` uses it as the per-channel
  `ChannelLoop::start_tick` (the player's `fold_tick` already replays `[start,
  end)` and plays the intro once). **A/B: bad company ch0 79.8→4.8, ch1
  96.9→12.9 (max 867→308), ch3 76.5→3.2 (max 1214→41); all volumes →100 %.**
  Zero corpus regression (xenon2/bubble/grimblood/tetris/qball/leviathan/empire/
  obliterator and beast byte-identical; suites green: xmrs 287, player, gate 116,
  robustness 116/0-panic). Residual: ch0 4.8 = the intro's 1-frame transpose seam
  (end-of-tick bakes ch1's same-frame +12 onto ch0's intro note) + the small
  slide↔transpose composition drift over the slid intro; ch1 12.9 = a recurring
  −47 in a body phrase — both minor, the slide/transpose-in-period-space limit.
- *2026-06-03* — **(earlier step, kept) end-of-tick transpose re-resolution.**
  The replayer recomputes a (held) note's base period every frame from the
  *current* global transpose; bad company's ch1 sets `+12` at f1 **after** ch0's
  note triggers (channel order 0→3), so ch0's slid opening note must use `+12`
  for its whole sustain but the importer baked the trigger-instant `+0`. The
  simulator now **re-resolves each NoteOn's `effective_note` at end-of-tick**
  (`runtime.rs::tick`), correct for the sustain (1-frame error only at the
  trigger). **ch0 MAE 190→80** (max 2125→1115); ch1/ch3 unchanged (96.9/76.5).
  No corpus regression (all 9 representatives byte-identical; xmrs 280 + gate
  116 + robustness 116/0-panic green). **Incomplete:** the recurring slide
  regions (e.g. all channels at t≈560: player slides 1131→609 where the oracle
  holds 280/561/214) need per-frame base recompute against the *time-varying*
  global transpose — i.e. the blocked-core time-varying pitch
  (`AutomationValue::Pitch`) — plus a slide-extent mismatch. Left for a
  dedicated pass / core decision.
- *2026-06-03* — **Oracle trace now models the global delay-counter, exposing
  that the C1 "vibrato/slide drift" was a trace artifact, not a modelling
  error.** The replayer's `play_tick` head runs an 8-bit accumulator
  (`DAT_5ea += DAT_5e8` per 50 Hz frame) and SKIPS the whole channel loop on a
  byte-carry — a uniform `e8/256` slowdown (`beast1.title`/`xenon2` `e8 = 16` →
  6.25 %). The importer folds this into BPM (clean row grid, see `to_module
  bpm_for`), so audio tempo is already correct; but the player trace advances
  one replayer tick per call while the oracle CSV counts 50 Hz frames *including*
  the skips, so the two slid apart by exactly `e8/256` — which, multiplied by
  ch3's octave jumps, looked like a big "drift" (beast ch3 MAE 55.8, max 336).
  `dw_oracle_trace.rs` now replays the same accumulator and emits a frozen row on
  each carry frame (no-op for `e8 = 0` modules). **A/B: beast ch3 MAE 55.8→0.9
  (max 336→1), ch2 12.4→1.4; xenon2 ch0 MAE 21.1→1.4 (max 274→8), ch1 49.8→4.4,
  ch2 16.3→1.4 (328→2), ch3 23.6→3.8 (328→5).** beast and the whole C1 cluster
  are now near-exact (residual = the intentional ~28-cent concert-pitch table
  offset). This is a **harness/measurement** change only — no importer, player,
  or audio change. The genuine remaining residuals (delay-counter `e8 = 0`)
  stand out clearly now: **C2 bad company** ch0/1/3 (MAE 190/97/76), C5 tetris
  ch1/2 (60/49), C3 bubble bobble ch3 (41.5).
- *2026-06-03* — **Canonical 2-arg slide was misclassified as a vibrato on the
  whole canonical-jump-table family (the root cause behind the phantom
  large-depth vibratos).** Ghidra (`beast/xenon2 play_tick`, byte-identical)
  shows cmd 0x81's handler is a SLIDE: `CLR.W (0x20,A0)` (clear slide
  accumulator) + `MOVE.B (A1)+,(0x14/0x15,A0)` (speed/counter) + `BSET #1,(0,A0)`
  (slide-active flag). The importer's `classify_handler` keyed vibrato off that
  shared `BSET #1`, so **every slide became a `StartVibrato`** — with the slide's
  `(speed=-1, counter)` bytes reinterpreted as `(speed=255, depth=120/96/144)`,
  the "phantom" large-depth vibratos. The old semitone vibrato path compressed
  them (hiding the defect); the correct period-space path rendered them as huge
  swings. The 6 binaries checked (xenon2, beast, obliterator, bad company,
  empire — canonical; bubble bobble, grimblood — genuinely shuffled/rebased)
  show this jump-table layout is **canonical** on 5 of them (slot1 = `42 68 00 20`
  slide, slot6 = `50 e8 00 01` vibrato). Added `DwCmdKind::Slide` anchored on the
  entry `CLR.W (0x20,A0)` (`sig::CLRW_20_A0`), classified **before** the
  `BSET #1`→vibrato rule; `event.rs` emits `DwTrackEvent::Slide{speed,counter}`.
  - **A/B (oracle re-diff):** obliterator ch0/2/3 MAE **32→5.4 / 32→4.8 /
    30→2.0** (max 186→15/185→14/185→11) — *better than the pre-vibrato baseline*;
    bad company ch2 **19.7→3.8** (max 732→40), ch0 226→190, ch1 126→97, ch3
    104→76; beast ch2 **23.6→12.4** (max 178→86); xenon2 ch0 exact 6→9 %.
    grimblood/tetris/qball/leviathan/empire/bubble **unchanged** (no canonical
    slide in their traces; bubble bobble's cmd 0x81 reclassified Slide but it
    makes no audible use of it in song 0 — verified byte-identical). All suites
    green (xmrs 280, player 32+8+1+5, DW robustness 116/0-panic).
  - **Note:** `beast1.title` ch3 (MAE 55.8) is the remaining ~22-tick held-note
    timing drift — a separate issue, not slide/vibrato.
- *2026-06-03* — **Effect vibrato is now applied in PERIOD space for Amiga
  modules (FT2/Protracker-faithful), with a new `beast1.title` oracle.**
  Ghidra `xenon2 play_tick` confirms the DW vibrato handler (cmd 0x86 @case
  0x86) is period-space: a **signed-char** accumulator (`+0x625`) ramps by
  `speed` (`+0x624`) toward `target` (`+0x626`), flipping at the bounds, and
  the Paula period is nudged **`period ± accumulator`** (`sVar20 ± cVar12`) —
  *not* a semitone/pitch delta. FT2/Protracker `doVibrato` does the same
  (`outPeriod = realPeriod ± (vibratoTab[pos]*depth)>>5`, table peak 255).
  The player only did semitone-space vibrato (a constant pitch nudge ≠ a
  constant period nudge on the Amiga period `∝2^-pitch`), so the wobble was
  too shallow on high notes and exploded on low ones.
  - **Player** (`update_frequency`): in Amiga mode the effect vibrato is added
    straight to the period (`value_raw()`, `EffectVibratoTremolo`); linear
    mode keeps the semitone path (byte-identical mechanism). Gated on
    `freq_type`, so IT/linear-XM are untouched.
  - **Importers** (`xm_vibrato_depth`, frequency-mode aware): XM/MOD Amiga
    depth = FT2 `(255*nibble)>>5` period units; XM **linear depth doubled to
    `nibble/8`** (was `nibble/16` = half FT2 depth — a pre-existing
    under-depth bug). DW depth = `max` (was the `max<<3` semitone hack). IT
    unchanged. Unit tests lock the FT2 calibration (`from_tracker.rs`).
  - **Measured (oracle re-diff):** genuine small-amplitude vibrato is now
    near-exact — **C4 grimblood ch1 MAE 16.2→8.2, ch0 8.7→6.8 (max 45→25)**;
    `beast1.title` ch2 depth-2 region now tracks the oracle to ±1 (was the
    wrong, register-scaled wobble). All suites green (xmrs 280, player
    32+8+1+5, DW robustness 116/0-panic).
  - **⚠️ Exposes a pre-existing DW decoder DESYNC.** Some files emit *phantom*
    large-depth vibrato events (beast `depth:120 speed:255`; obliterator
    `depth:144/96`) that the **oracle renders as ±2** — i.e. the real stream
    has no such command there; the decoder lost byte-alignment and read
    garbage as a `cmd 0x86`. The old semitone path *masked* this (it
    compressed huge depths via the pitch round-trip); the correct period-space
    path renders them faithfully → big phantom swings → **obliterator MAE
    26→32, beast ch2 MAE 16→24**. Corpus-wide vibrato depth has a continuous
    tail (40,48,60,…,144) with no clean genuine/phantom cutoff, so a blanket
    clamp is unsound. **Root cause = a systematic per-file byte-consumption
    desync (the PLAN.md §3 sweep / per-file Ghidra verification).** Fixing the
    desync is the blocker for the period-space change's full corpus benefit.
    `beast1.title` ch3 also drifts ~22 ticks (held-note length / re-trigger),
    a separate timing issue, not vibrato.
- *2026-06-02* — **C7 leviathan volume → exact** (was 16/0/16/0 % exact, MAE
- *2026-06-02* — **C7 leviathan volume → exact** (was 16/0/16/0 % exact, MAE
  5–9, every note flattened to full scale). Leviathan writes a **static
  per-instrument volume** straight to `AUDxVOL` on every note trigger
  (`MOVE.W (0xE,A5),(0xA8,A6)` @0x17c — no envelope). The volume word lives at
  byte `+0xE` of the 16-byte instrument records (init @0x0 fills `+0/+8/+0xC`
  but leaves `+0xE` as the static image table `[64,64,58,54,55,55,55,55,55]`).
  Located via the `SetSample` arming idiom `LEA (d16,PC),A5 ; MULU.W #0x10,D0`
  (`4B FA <d16> C0 FC 00 10` → `DwLayout::instrument_volume_offset`); read into
  each `DwSample::volume` (gated `period_via_finetune`), and emitted per note
  (effective-vol now prefers the sample's static volume over the qball
  per-channel table for fine-tune old players). **A/B: vol exact 16/0/16/0 % →
  100/31/100/0 %, MAE 5.1/9.2/5.1/9.0 → 0.0/0.7/0.0/1.0 (residual ≤1 LSB).**
  Empire (12-byte records, `MULU #0xC`, envelope-driven volume) doesn't match
  the `#0x10` idiom → untouched (its sample volumes stay full scale); qball and
  tetris unchanged (verified). Closes Task #9.
- *2026-06-02* — **Period-space mismatch resolved on C7 leviathan & C8 empire;
  C2 bad company ch3 transpose fixed.** Two independent Ghidra-derived fixes, no
  core change. (Numbers below are a clean A/B: both fixes OFF vs ON, all 9
  traces regenerated each side. C1/C3/C4/C5/C6/obliterator rows are **byte-for-
  byte identical** both sides — zero collateral. C3 bubble bobble and C4
  grimblood were already resolved in earlier sessions and are **untouched** by
  this work.)
  1. **C7/C8 use the new-player period path, not qball's composite.** Their
     `play` body computes `AUDxPER = table[note] × instr_mult >> 10` over a
     full-range word table (leviathan @0x320 = P2; empire @0x64a = P3), with
     note/instrument decoupled — exactly the new player. Only the command
     *stream* is "old". The importer was forcing `Old → P1` + the one-octave
     composite `PERIODS1[note%12]`, capping every note into octave 0. Added
     `period_via_finetune` (set when the fine-tune LEA idiom
     `45 FA <d16> 32 2D 00 0A|0C 74 0A` fires on an *old* binary; the `0C`
     covers leviathan's `instr+0xC` mult slot): direct note decode + table
     fine-tune. **A/B: C7 medΔ −105/+74/−105/−307 → −1/0/−1/0 (MAE 205→~1);
     C8 medΔ −93/−112/+42/−83 → −1/+2/+2/+2.** qball (no idiom) stays composite.
  2. **command_map ChannelTranspose slot 0x3 was unrecognised.** The jump-table
     classifier only matched the per-channel transpose handler at slot 0x2f
     (`MOVE.B (A1)+,(0x2f,A0)`, the bubble-bobble family); the empire/
     bad-company family writes the transpose to slot **0x3**
     (`MOVE.B (A1)+,(0x3,A0)`, read back by the note trigger's
     `ADD.B (0x3,A0),D0`). It fell through to `Unknown` with **0 arg bytes**,
     so the transpose value byte was never consumed → that channel's stream
     misaligned. Now matched (args=1). Verified affected exactly **empire**
     (ch-transpose events 0→26) and **bad company** (0→13); grimblood (no 0x3
     handler) and bubble-bobble (slot 0x2f) are unchanged. **A/B: empire ch0
     −93→−1 (its +12 transpose); C2 bad company ch3 medΔ −110→+2.**
  Residual on these clusters is now drift-level (MAE from vibrato/slide in
  period space), not a space mismatch. All suites green (xmrs 277 + corpus
  gate 116; player robustness 116/0-panic). Leviathan/empire volume (C7 16 %,
  empire ch2) is the per-instrument-volume backlog (Task #9), not period.
- *2026-05-29* — **C8 empire volume → 96–100 % exact on all four channels**
  (was 0 % everywhere — golden was uniformly `player × 24/64`). The empire-era
  replayer scales every Paula volume by a **static global master volume** before
  a `>> 6`: Ghidra shows both the note-trigger (`0x2bc`) and the per-tick
  envelope-animation (`0x404`) write `AUDxVOL = (volByte × USHORT_0000057a) >> 6`,
  with `0x57a = 24`. Detected via the volume-scale prologue
  `MOVE.W (d16,PC),D2 ; MULU.W D2,D1 ; LSR.W #6,D1` (bytes `34 3A <d16> C2 C2 EC 49`,
  master word at `instr+2+d16`); stored as `DwLayout::master_volume_offset` →
  `DwModule::master_volume`, applied as `× master / 64` to **both** the
  per-note `TrackEffect::Volume` and the envelope-animation ghost cells.
  Residual max|Δ|=15 on ch1/ch3 is the same handful of note-retrigger ticks
  seen on qball. **Generalises across the corpus:** the same idiom is present
  in **90/120** files (both code sites always agree on one master word — strong
  validation); **84 carry the identity `master = 64`** (a guarded no-op, so
  byte-for-byte zero regression — confirmed: every other cluster's volume row
  here is unchanged, e.g. xenon2 71/74/95/95), and **6 carry a real non-64
  master** now faithfully scaled: empire (24), alfred chicken (24),
  chip's challenge (40), menace (40), wizzcat (40), jaws (48), cosmic
  pirate (63). Only empire is cycle-verified (it is the only one with a
  golden); the other five are correct by the identical mechanism + the
  `master == 64` guard, and still pass the strict load gate.
- *2026-05-29* — **C6 qball volume → 99–100 % exact on all four channels**
  (ch1/ch2 were 0 %, off by a flat 24). Old qball-era DW players load a
  **per-channel static volume table** (Ghidra: a per-note volume load gated on
  the old player) and write it straight to `AUDxVOL` on every note, with no
  sample/envelope scaling. Ghidra qball:
  the table sits at `0x2d6` = `[64, 40, 40, 64]`, located via the
  `LEA d16(A3),A0 ; LSL` prologue (`41 EB <d16> E3 4F`). Now detected
  (`DwLayout::channel_volume_offset`), loaded into `DwModule::channel_volumes`,
  and emitted as a per-note `TrackEffect::Volume` for old-player modules.
  Residual on ch1/ch2 is 14 note-retrigger ticks where the replayer briefly
  writes vol 0. **Note:** qball writes the table *raw* — no global scale —
  which is what distinguishes it from the empire family below.
- *2026-05-29* — **C5 tetris volume → 100 % exact on all four channels**
  (was 0/0/0/100 %, off by a flat 8). Root cause via Ghidra (`PlayTick`
  writes `AUDxVOL = sampleInfo[+0xC] − DAT_0000041a`): the importer
  hard-coded `DwSample.volume = 64` and never read the per-sample volume
  word that the 16-byte sample-transpose loader stores at byte +12. Now
  read it (gated on `enable_sample_transpose`, clamped 0..=64). The
  per-channel split (lead/bass 56, percussion 64) is purely those sample
  words; `Effect8`'s `DAT_0000041a` offset is 0 across this capture.
  Zero regression elsewhere (only tetris carries the flag).


| Cluster | ch | gate✓ | both-gated | per exact | per medΔ | per MAE | per max | vol exact | vol MAE | vol max |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| C1 · xenon2 (title) | 0 | 93% | 1394 | 21% | -2 | 1.4 | 8 | 51% | 5.1 | 16 |
|  | 1 | 94% | 1327 | 6% | +3 | 4.4 | 18 | 85% | 4.1 | 32 |
|  | 2 | 83% | 1134 | 0% | -1 | 1.4 | 2 | 100% | 0.0 | 0 |
|  | 3 | 82% | 1127 | 0% | -5 | 3.8 | 5 | 100% | 0.0 | 0 |
| C2 · bad company | 0 | 88% | 1318 | 2% | +3 | 4.8 | 1115 | 100% | 0.0 | 0 |
|  | 1 | 88% | 1318 | 0% | +5 | 12.9 | 308 | 100% | 0.0 | 0 |
|  | 2 | 63% | 941 | 18% | +2 | 3.8 | 40 | 100% | 0.0 | 0 |
|  | 3 | 88% | 1318 | 0% | +2 | 3.2 | 41 | 100% | 0.0 | 0 |
| C3 · bubble bobble | 0 | 96% | 428 | 4% | +3 | 4.5 | 13 | 100% | 0.0 | 0 |
|  | 1 | 98% | 437 | 0% | +5 | 6.4 | 325 | 84% | 0.2 | 1 |
|  | 2 | 96% | 423 | 4% | +3 | 4.5 | 13 | 100% | 0.0 | 0 |
|  | 3 | 93% | 416 | 0% | +2 | 11.4 | 283 | 100% | 0.0 | 1 |
| C4 · grimblood (3-voice) | 0 | 97% | 1453 | 0% | +2 | 6.8 | 45 | 97% | 0.0 | 1 |
|  | 1 | 99% | 1483 | 0% | +5 | 8.2 | 25 | 98% | 0.0 | 1 |
|  | 2 | 99% | 1492 | 1% | +5 | 7.6 | 24 | 98% | 0.0 | 1 |
| C5 · tetris | 0 | 98% | 1465 | 30% | +0 | 3.6 | 14 | 100% | 0.0 | 0 |
|  | 1 | 97% | 1459 | 41% | +0 | 3.9 | 318 | 100% | 0.0 | 0 |
|  | 2 | 96% | 1423 | 37% | +0 | 28.6 | 326 | 100% | 0.0 | 0 |
|  | 3 | 98% | 1465 | 29% | +0 | 3.4 | 13 | 100% | 0.0 | 0 |
| C6 · qball (old) | 0 | 100% | 1500 | 0% | -3 | 3.2 | 4 | 100% | 0.0 | 0 |
|  | 1 | 100% | 1500 | 0% | -4 | 3.6 | 4 | 99% | 0.4 | 40 |
|  | 2 | 100% | 1500 | 0% | -3 | 3.1 | 4 | 99% | 0.4 | 40 |
|  | 3 | 100% | 1500 | 0% | -3 | 3.2 | 4 | 100% | 0.0 | 0 |
| C7 · leviathan-deabs (old) | 0 | 88% | 1327 | 11% | -1 | 1.1 | 4 | 100% | 0.0 | 0 |
|  | 1 | 92% | 1386 | 53% | +0 | 0.5 | 1 | 31% | 0.7 | 1 |
|  | 2 | 88% | 1327 | 11% | -1 | 1.1 | 4 | 100% | 0.0 | 0 |
|  | 3 | 99% | 1480 | 70% | +0 | 0.3 | 1 | 0% | 1.0 | 1 |
| C8 · the empire strikes back (old) | 0 | 96% | 1413 | 23% | -1 | 1.4 | 4 | 100% | 0.0 | 0 |
|  | 1 | 92% | 1375 | 17% | +2 | 2.2 | 11 | 69% | 0.4 | 9 |
|  | 2 | 90% | 1346 | 0% | +2 | 2.0 | 2 | 0% | 1.0 | 1 |
|  | 3 | 94% | 1414 | 21% | +2 | 1.7 | 6 | 80% | 0.2 | 1 |
| ½-vol · obliterator-title | 0 | 99% | 1489 | 8% | +6 | 5.4 | 15 | 100% | 0.0 | 0 |
|  | 1 | 92% | 1374 | 8% | +7 | 6.1 | 14 | 100% | 0.0 | 0 |
|  | 2 | 99% | 1489 | 10% | +5 | 4.8 | 14 | 100% | 0.0 | 0 |
|  | 3 | 97% | 1461 | 58% | +0 | 2.0 | 11 | 100% | 0.0 | 0 |

Reproduce: regenerate the 9 `/tmp/player_<key>.csv` traces (loop in
`oracle/README.md`'s sibling section) then `python3 oracle/diff_baseline.py
--markdown`.

## Reading the numbers

**The structural skeleton is already right.** Gate agreement is 82–100 %
everywhere: the player turns channels on and off at essentially the right
ticks. The remaining error is *what value* plays, not *whether* it plays —
which is the tractable kind.

Two independent axes of error, both already on the `PLAN.md` backlog:

### A. Period — a small constant offset + a per-cluster space mismatch

* **Near-aligned (median Δ within ±10):** C1 xenon2 (±2-3), C4 grimblood
  (+2/+5/+15), **C5 tetris (+0 on every channel)**, C6 qball (-3/-4),
  obliterator (+7). Here the period *space* matches; the residual is
  period-table rounding (the ±2..±7 constant) plus per-tick **drift** (MAE
  16–65, max in the hundreds) from vibrato/portamento not yet computed in
  the replayer's exact period units. → PLAN "vibrato period-space" + slide
  edge cases. tetris ch0/ch3 (MAE 2.9, max 7) are the closest in the corpus
  and the best regression target to nail first.
* **Period-space mismatch — now CLEARED across C2/C3/C7/C8** (all sit at median
  Δ within ±5; residual is vibrato/slide **drift** MAE, not a space mismatch).
  Resolved in two waves: **C3 bubble bobble** (and the rest of the command_map
  family's sequencing) was fixed in earlier sessions; **C7 leviathan / C8 empire**
  were fixed 2026-06-02 (new-player period path via `period_via_finetune`), plus
  the slot-0x3 ChannelTranspose fix that corrected **empire ch0** and **C2 bad
  company ch3** (its only off channel; ch0–2 were already aligned). The mismatch
  was never old-vs-new: it was (a) two old-stream players that compute period the
  *new* way, and (b) per-channel-transpose handlers whose arg byte was dropped.

### B. Volume — systematic offsets, not noise

Several clusters show a *constant* volume delta (MAE == max), i.e. a fixable
scale/threshold rather than random error:

* **C5 tetris:** ✅ **fixed** — was ch0–2 off by exactly **8** every tick;
  it was the per-sample volume word (offset +0xC of the 16-byte
  sample-transpose info row) that the importer hard-coded to 64. Now read;
  100 % exact on all four channels. See the changelog above.
* **C8 empire:** ✅ **fixed** — was 0 % everywhere, **golden = player × 24/64**
  *uniformly*. The player already computed the right *relative* volumes
  (per-channel envelope shapes matched); empire's replayer applies a **static
  global master volume** `USHORT_0000057a = 24` to every Paula write
  (`AUDxVOL = (volByte × 0x57a) >> 6`, at both note-trigger and per-tick
  envelope). Now detected by the volume-scale prologue and applied as
  `× master / 64` to both write paths → 96–100 % exact. The gate is the
  **probe**, not the variant: qball (also old) writes its channel table *raw*
  and lacks the `MULU ; LSR #6` idiom, so it is untouched; new players carry
  the idiom with `master = 64` (a guarded no-op). See the changelog for the
  90/120-file corpus breakdown.
* **C7 leviathan:** small *varying* deltas (MAE 5–10, not constant) — a
  different, near-correct mechanism (likely a volume fade / envelope-rate
  detail), distinct from qball's static table and empire's global scale.
  Lower priority; needs its own Ghidra volume-write trace.
* **C6 qball:** ✅ **fixed** (per-channel static volume table). **obliterator
  ch3 / tetris (all) / qball (all):** volume **100 %/99 % exact** — proof the
  volume path is right once the cluster's volume model is identified.
* **obliterator (½-vol):** volumes are already 81–100 % exact *without* the
  half-volume model — so the attenuation it needs is small/conditional;
  worth confirming against the decomp before modeling.

## Next actions (feeds PLAN.md backlog)

1. ✅ **tetris volume** (per-sample word), ✅ **qball volume** (old-player
   per-channel table), and ✅ **empire global master volume** (×24/64, +5
   other non-64 corpus modules) — done. Remaining volume target:
   * **C7 leviathan** small *varying* deltas (MAE 5–10, not constant) — a
     different near-correct mechanism (likely a volume fade / envelope-rate
     detail), separate and lower-priority; needs its own Ghidra volume trace.
2. **qball/C1 period constant offset** (medΔ -3/-4 on qball, ±2/3 on xenon2 —
   period-table rounding) — small systematic deltas, good warm-up before the
   period-space work.
3. **Period-space for C2/C3/C7/C8** — the big-ticket item; needs the per-
   cluster replayer period/vibrato semantics from Ghidra (PLAN "vibrato
   period-space", "SetSpeed", envelope frame-res). Re-run this diff after
   each change; the median-Δ column is the headline regression metric.
3. **C3 bubble bobble gate** (ch0 50 %, ch3 76 %) — note-trigger dispatch
   divergence; investigate before chasing its period numbers.

Keep this file as the living scoreboard: re-run `diff_baseline.py
--markdown` after each modeling change and update the table + date.

---

## 2026-06-05 — totalrecall `0x83`-hold drop fixed + chip's challenge golden

**totalrecall family `0x89`/`0x88`/`0x83` command set (Ghidra-confirmed from
file bytes).** The totalrecall builds (`disp=[B0,A0,90]`, `sq=true`) use a
jump-table command dispatcher (`JMP (0,A3,D0.w)`); cmd handlers decoded from
the file: cmd3 `0x83` @entry `21 49 00 04` (`MOVE.L A1,(4,A0); MOVE.W
(0x1c,A0),(0x1e,A0)` = save stream ptr + reload duration counter, **no DMA
clear → a HOLD/tie**, = canonical `WaitUntilNextRow`); cmd8 `0x88` = 1-byte
effect (`MOVE.B (A1)+,(3,A0)`); cmd9 `0x89` = 2-byte channel effect
(`MOVE.B (A1)+,(8,A0); (9,A0); MOVE.W #0,(0xa,A0)`) — **NOT a sequencer jump**
(bad company's cmd9 handler is byte-identical, so its `SeqPtr` classification
is incidental; `follow_seq_ptr_loops` only reads each channel's *intro* track,
so the mis-label is inert at playback).

**Bug + fix.** `command_map::classify_handler` recognised the re-trigger entry
`21 49 00 04` only at a *branch target*, not when **inlined** at the handler
entry (which the totalrecall build does). So `0x83` fell through to
`Unknown` (0 args, **no event**) and every `0x83` hold was silently dropped —
halving sustained notes. Drone `ff 18 83 18 83` should be 4×96=384 frames
(LongWait + Note + hold + Note + hold) but rendered 2×96=192, drifting the
whole channel 192 frames/cycle. Fix: classify the inline `21 49 00 04` entry
as `Retrigger` → `event.rs` maps it to `WaitUntilNextRow` under
`note_repeat_is_tie` (the `0C 11 00 83` guard, present). Measured (Paula
oracle, perMAE on gated ticks):

| module               | ch | before | after |
|----------------------|----|--------|-------|
| totalrecall-ingame 4 | 2  | 278.5  | 2.2   |
| totalrecall-ingame 2 | 0  | 33.2   | 3.9   |
| totalrecall-ingame 5 | 2  | 22.9   | 1.8   |

Corpus strike-clean **62 → 68**; bad company / tie-family (xenon2 / beast /
empire) unchanged; all suites green (xmrs 260, player 26+1+5, `load_all_dw`).
Residue: totalrecall-ingame 1 & ingame 3 (ch3 MAE 43) is a **loop-phase
drift**, NOT a re-strike problem. Verified ingame3 ch3: player & oracle are
byte-for-byte aligned 0..575 (58 re-strikes match exactly — the player already
re-triggers consecutive-identical "drum-roll" notes correctly via plain NoteOn
cells, NO effect/core change needed; windowed strikes 0 spurious / 15 missed,
all post-loop). They diverge only at tick 576 = the channel's loop point (the
player loops its ch3 cycle while the oracle plays on) — the documented
per-channel loop-length / loop-phase class (cf. ChannelLoop / xenon2). The
audit's `miss=312` over-counts: it tallies the whole post-divergence region,
not dropped strikes. (The oracle also drops the gate 1 frame before each
re-strike = Paula's DMA-off/on retrigger; the player re-attacks without the gap
— an oracle-side emulation artifact, both restart the sample.)

**chip's challenge — never a fidelity bug.** Detection `[B0,A0,90]` is CORRECT
(verified from the file dispatch cascade @0x746: `E0`dur/`B0`sample/`A0`/`90`).
It lacked a Paula golden only because the **apostrophe in the filename** breaks
Ghidra's `BinaryLoader` import ("invalid character: '''"). Fix: `capture_all.sh`
now stages apostrophe files under a sanitised `/tmp` name before `-import`;
`known_args.tsv` gets `chip's challenge  0x0  0xe  -  -` (init=0x0 play=0xe,
A3-base 0, no stop flag). Captured → perMAE `[9.2, 2.0, 7.7, 2.0]`, strike-
clean. (Gotcha: run `capture_all.sh` from `xmrs/`, NOT from `oracle/` — the cwd
shadows `~/ghidra_scripts` with the reference `oracle/DwPaulaOracle.java` copy,
which fails to compile. The targeted run also truncates `capture_all.log` to
the requested stems; the corpus/ goldens are untouched and a full run restores
the log.)
