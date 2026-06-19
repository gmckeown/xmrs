# Plan — faithful playback of all of `~/Music/dw` (`.dw`)

> Self-contained multi-session execution plan. Read alongside
> [`FORMAT.md`](FORMAT.md) (spec + replayer semantics + conventions).
> **Goal**: all 120 `.dw` load without error **and** play with a faithful model
> (Ghidra oracle when needed), with **no questions** to the user except the final
> listen.

---

## 0. State

- **120** files in `~/Music/dw/`. **116 load**, **4 cleanly rejected** as
  `dw_no_song` (SFX engines with no PCM table): `anarchy sfx`, `blood money sfx`,
  `spellbound sfx`, `feud fake` (2 KB placeholder). ⇒ **loading = achieved** (a
  loaded module needs no song; see FORMAT.md §10).
- **Fidelity**: measured per-cluster against the Paula oracle (see `BASELINE.md`
  for the scoreboard). Gate agreement is 82–100 % everywhere; period-space is
  aligned on all clusters; volume is 96–100 % exact on the verified clusters. The
  remaining residual is per-tick vibrato/slide drift in exact period units on a
  few channels.
- **Robustness** (Tier 1): `xmrsplayer/tests/dw_robustness.rs` renders the 116
  modules via `XmrsPlayer` (song 0, `max_loop_count(1)`, cap 60 s @ 24 kHz), 0
  panic. Hard invariant: any channel carrying a note at velocity > 0 must produce
  sound ⇒ 0 failure. (The player only plays **song 0**, so the channel census is
  bounded to song 0.)

---

## 1. The "clustering": player clusters

David Whittaker evolved his replayer; the corpus groups by
**(variant, dispatcher, period table, sub-song row width, flags)**. Work
**per cluster** (fix a representative, propagate) rather than per file.

| # | Cluster (key) | Representative | Count ≈ | Notable quirks |
|---|---|---|---|---|
| C1 | New · `[B0,A0,90]` · P3 | **xenon2 (title)** ✅Ghidra | ~55 | canonical; `enable_channel_transpose` (Effect8) on xenon2 |
| C2 | New · `[B0,A0,90]` · P2 | **bad company** | ~2 | (jupitermaster1) |
| C3 | New · `[C0,B0,A0]` · P3 | **bubble bobble** ✅Ghidra | ~26 | possible `volume_bracket_is_pitch`; A3-rebased: bubble bobble, alfred chicken, gunship2000, krustyssuperfunhouse, snow strike |
| C4 | New · `[C0,B0,A0]` · P2 | **grimblood** | ~2 | A3-rebased (grimblood −0x388); cosmic pirate |
| C5 | New · `[C0,A0]` · P2 (tetris family) | **tetris** ✅Ghidra | 8 | `enable_sample_transpose` (−3, sample-info stride 16); `volume_bracket_is_pitch`; Effect8 = global volume offset. Members: army moves, bmx simulator, exotic 210, kickstart ii, sidewinder, tetris, the hunt for red october, xenon |
| C6 | **Old** · P1 · `[]` | **qball** ✅Ghidra | 3 | note = `sample×12+pitch`; 32-bit pointers; `SetSample` inert; one-shot samples. `xenon-sfx`/`leviathan-sfx` = 0-track banks (instruments-only) |
| C7 | **Old**-stream · **P2** · `[C0]` | **leviathan-deabs** ✅Ghidra | 1 | old stream BUT new period (`period_via_finetune`, table @0x320=P2, `×instr[+0xC]>>10`); direct note; static per-instr volume `instr+0xE` (table @0x380) |
| C8 | **Old**-stream · **P3** · `[C0,B0,A0]` | **the empire strikes back** ✅Ghidra | 1 | old stream + new period (`period_via_finetune`, table @0x64a=P3, `×instr[+0xA]>>10`); global(0x582)+channel(slot 0x3) transpose; per-tick arpeggio |
| — | half-volume | **obliterator-title** / `obliterator-ingame` | 2 | `enable_half_volume` (jump-table: slot8=`ST`, slot9=`SF`) |
| — | no MULU anchor | **millennium** | 1 | fallback heuristic (see detect.rs) |
| — | empty channel (≈3 voices) | **sentinel** (`pos[0]=0`), sidewinder, super wonderboy | 3+ | an empty-channel position-list |

Representatives **loaded in the Ghidra `xenon2` project** (9): xenon2 (title),
bubble bobble, tetris, qball, bad company, grimblood, obliterator-title,
leviathan-deabs, the empire strikes back.

---

## 2. Verification strategy (autonomous → ear last)

Three tiers; the first two are automatic.

### Tier 1 — Robustness (no oracle, covers all 116)
Render each module via `xmrsplayer` to the end of its loop (reasonable cap) and
assert: no panic / NaN / Inf; no out-of-range Paula period; on each channel that
carries notes, audio RMS > threshold (no spurious silence); `verify_layers_consistent`
OK. ⇒ captures the essence of "works without errors" with no ear.

### Tier 2 — Fidelity (Ghidra oracle, 1 representative / cluster)
**Oracle = PCode emulation of the binary's own replayer** (absolute authority).
- Load the representative in Ghidra.
- Emulate `init_main` once then `play_tick` N times, capturing per-channel Paula
  register writes (`AUDxPER` `+0xa6`, `AUDxVOL` `+0xa8`, `DMACON` `0xdff096`).
- **Our side**: a per-tick observer (`channel_snapshots()` →
  `ChannelSnapshot { period, volume, gate }`) replays the `Module` (1 player tick
  = 1 DW frame) and captures the same trace.
- **Frame-to-frame diff** (`oracle/diff_baseline.py`): exact period in Amiga units
  (±1), exact volume (±1), same sample/voice on/off. Target ≥ 99 % of the first
  ~3000 frames; residuals documented in `BASELINE.md`.

### Tier 3 — Ear (user, once, at the end)
Final list: one representative per cluster (8 files) to validate by listening.

---

## 3. Modeling backlog (priority ↓), mapped to clusters

Each item is **oracle-verifiable** by a Tier-2 trace diff on its representative.
Status reflects the current code.

1. **Vibrato in period space** — ✅ done (the replayer adds a triangle to the
   *period*; the player applies it in period space for Amiga modules). Remaining:
   per-tick vibrato/slide drift in exact period units on a few channels (the open
   fidelity residual).
2. **GlobalVolume / GlobalVolumeFade as a continuous lane** — ⏳ not done as a lane,
   but volume was largely resolved per-cluster (tetris per-sample word, qball
   per-channel table, empire global master, leviathan per-instrument volume).
3. **Volume envelope at frame resolution** — ✅ done (`volume_to_volume_envelope`,
   1 point/frame).
4. **`SetSpeed` (cmd 10) mid-song** → re-quantize the frame→row mapping. The
   per-sub-song quantization grid is ✅ done (`speed`/`bpm_at_row` = the sub-song's
   `dw_speed`, the exact tracker idiom; zero core change — the runtime honors a
   per-song tempo via `TimelineEntry::speed_at_row`/`bpm_at_row`). ⏳ SetSpeed
   *mid-song* (re-grid on the GCD of encountered speeds) not done.
5. **Half-volume** (obliterator) — ✅ detection done (`enable_half_volume` via the
   effect jump-table; `Effect9` width corrected). Toggle modeling open.
6. **Empty / 3-voice channels** (sentinel, sidewinder, super wonderboy) — ⏳ open
   (verify `get_num_channels` and rendering handle an empty position-list).
7. **Square-waveform** (`enable_square_waveform`) — ⏳ open.
8. **`Effect9` RestartPosition non-zero** (mid-list jump) — ⏳ open (no corpus
   module currently triggers it).
9. **Old player C7/C8** (leviathan-deabs, the empire) — ✅ done
   (`period_via_finetune`, 1- and 3-bracket on the Old variant).
10. **Per-channel looping** (independent voices that drift) — ✅ done. Ghidra on
    `xenon2 (title).dw`: cmd 0x80 loops EACH channel to its own list, no global
    restart; the 4 voices have unequal pass lengths and phase apart forever
    (faithful to hardware). `channel_loops` is populated per voice (drift branch) +
    free-run sequencer (`loop_generation`/`current_abs_tick_monotonic`,
    `advance_to_next_entry` re-loops the timeline without a global wrap). Gated:
    empty `channel_loops` → XM/IT/MOD untouched. See `daw/LOOP_REGION_RFC.md`.

---

## 4. Acceptance criteria (final gate)

- **Loading**: 116/116 load + `to_module` + `verify_layers_consistent`; 4 rejected
  `dw_no_song`.
- **Robustness**: 116/116 rendered to end-of-loop via `xmrsplayer`, no panic/NaN,
  healthy RMS on channels with notes.
- **Fidelity**: per cluster representative, Paula trace period exact (±1) on ≥ 99 %
  of the first ~3000 frames, volume ±1; residuals documented.
- **Ear**: Tier-3 set validated by the user.

---

## 5. File & anchor map (exact)

- Importer: `xmrs/src/tracker/import/dw/dw_module.rs`
  (`DwModule::load`, `to_module` ~l.249, `attach_vibrato_lanes` ~l.1272,
  `attach_slide_lanes` ~l.1420).
- Runtime/simulator: `xmrs/src/tracker/import/dw/runtime.rs`
  (`apply_event` ~l.598, `tick_channel` ~l.512, `ChannelState`, `TickEvent`).
- Detection/clusters: `xmrs/src/tracker/import/dw/detect.rs` (`DwFeatures` l.37:
  `enable_*`; detection axes see FORMAT.md §10).
- Event decoder: `xmrs/src/tracker/import/dw/event.rs` (`DwTrackEvent`, l.27).
- Player: `xmrsplayer/src/channel/lanes.rs` (`apply_slides_from_lanes` ~l.260),
  `xmrsplayer/src/audio_observer.rs` (per-tick observer).
- Quirks: `xmrs/src/core/compatibility.rs` (`PlaybackQuirks`).
- Gate test: `xmrs/src/lib.rs::load_all_dw_modules_from_music_dir` (~l.194).
- Tools: `xmrs/examples/inspect_dw.rs` (triage; `-q` summary; `DW_TRACE=1` trace),
  `xmrs/examples/dump_dw_pattern.rs` (rendered grid per pattern/channel).
- **Ghidra (oracle, single source of truth)**: `play_tick` `0x1b6`, `init_main`
  `0x66`, `sample_loader` `0x1414`; Paula registers `0xdff0a0 + 0x10*v`
  (+`0xa6` PER, +`0xa8` VOL), DMACON `0xdff096`. Tick-exact Paula emulation:
  `oracle/`.

---

## 6. Typical commands

```sh
# Triage / clusters (one-line summary per file)
cargo run --release --features=demo --example inspect_dw -- -q ~/Music/dw/*.dw
# Full event trace of one module
DW_TRACE=1 cargo run --release --features=demo --example inspect_dw -- "~/Music/dw/<f>.dw"
# Rendered grid (pattern, channel)
cargo run --release --features=demo --example dump_dw_pattern -- "<f>.dw" <song> <pattern> [ch]
# Gate test
cargo test --release --features=demo load_all_dw_modules_from_music_dir
# Full suites
( cd xmrs && cargo test --release --features=demo ) && ( cd xmrsplayer && cargo test --release )
```
