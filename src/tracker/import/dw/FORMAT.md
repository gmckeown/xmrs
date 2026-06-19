# David Whittaker format (`.dw`) — spec & importer

On-demand reference document. Covers the `.dw` format, the replayer semantics
(verified by reverse-engineering), the xmrs importer architecture, the key
decisions and the code conventions.

---

## 1. Sources of truth (priority order)

1. **Ghidra MCP on the `.dw` binary itself** (e.g. `~/Music/dw/xenon2 (title).dw`).
   The `play_tick` function (the 50 Hz tick) and `init_main` are the absolute
   authority — the whole importer is reversed directly from the embedded 68000.
2. **Paula emulation oracle** (`oracle/`): emulates the real 68k replayer and
   captures the Paula registers per tick → frame-to-frame diff against the player
   trace.

By ear: the user has the original and serves as the final oracle.

---

## 2. Code conventions (shared core)

- **Be careful with the xmrs core.** Any change to `xmrs` (core) is confirmed
  first; it is not done lightly.
- **Study the impact on BOTH sister crates** (`xmrs` AND `xmrsplayer`, at the
  same level) before any decision touching shared code.
- **Human-readable code: no magic numbers.** Use named constants (`Q15::ZERO`,
  `Phase::QUARTER`, …). If a constant is missing, add it, or use readable macros.
- **Self-documenting code > trivial unit tests.** Prefer clear code (named
  anchors, transition comments) over basic tests.
- **Verify empirically before concluding** (read the real code, sample values)
  rather than assume.

---

## 3. File structure

Big-endian, 68000. Detection: `xmrs/src/tracker/import/dw/detect.rs`.

- **`init`**: fills the runtime state from the selected sub-song's header.
- **Sub-song** (detected offset, e.g. `0x96e`):
  - `byte[0]` = **speed** (frames per "row"; LongWait multiplier).
  - `byte[1]` = **delay_counter_speed** (global throttle, see §5).
  - then a **per-channel position-list pointer** (`u16` new-player, `u32` qball).
- **Position list** (one per channel): a sequence of **track offsets**,
  terminated by `0x0000` (⇒ loops back to entry 0 / `RestartPosition`) or a value
  with bit `0x8000` set.
- **Track**: a byte stream, terminated by `EndOfTrack` (`0x80`). Deduplicated by
  offset.
- **Samples**: an info table (`0xc` bytes/sample: ptr, len, loop), + PCM data.
- **Volume envelopes** (`0xA0` bracket) and **arpeggios** (`0x90` bracket):
  pointer tables into byte sequences.
- Variants: **New player** (most) vs **Old player** (qball: note =
  `sample*12 + pitch`, `SetSample` inert).

---

## 4. Track command set (byte `b`)

| Condition                     | Command            | Effect |
|-------------------------------|--------------------|--------|
| `b < 0x80`                    | **Note**           | period index (new) / `sample*12+pitch` (old) |
| `b >= 0xE0`                   | **LongWait**       | wait = `(b − 0xDF) × speed` frames (inline) |
| `b >= sample_threshold` (`0xB0`) | **SetSample**   | `sample = b − threshold` |
| `b >= volume_envelope_threshold` (`0xA0`) | **SetVolumeEnvelope** | arms a volume envelope |
| `b >= pitch_arpeggio_threshold` (`0x90`)  | **SetPitchArpeggio**  | arms a pitch arpeggio |
| otherwise, `b & 0x7F`:        | **Effect**:        | |
| 0  | EndOfTrack          | advance in the position-list (does not consume the tick) |
| 1  | Slide               | 2 args: `speed` (i8), `counter` (delay) |
| 2  | Mute                | **ends the tick** (silence during the wait) |
| 3  | WaitUntilNextRow    | **ends the tick** (holds the note during the wait) |
| 4  | StopSong            | |
| 5  | GlobalTranspose     | 1 arg (gated by `enable_global_transpose`) |
| 6  | StartVibrato        | 2 args: `speed`, `max` (depth) |
| 7  | StopVibrato         | |
| 8  | Effect8             | module-dependent: volume-fade / **channel-transpose** / half-vol |
| 9  | Effect9             | 0 args if half-vol, otherwise 2 (restart position, handled at load) |
| 10 | SetSpeed            | changes `playingInfo.Speed` (global) or the delay-counter |
| 11 | GlobalVolumeFade    | |
| 12 | SetGlobalVolume     | (or start sound-fx depending on the module) |
| 13 | StartOrStopSoundFx  | (ignored) |
| 14 | StopSoundFx         | (ignored) |

> **Naming**: identifiers track the Ghidra function — the `0xA0` bracket arms a
> **volume envelope** (`SetVolumeEnvelope`, `volume_envelope_threshold`,
> `current_envelope`) and the `0x90` bracket arms a **pitch arpeggio**
> (`SetPitchArpeggio`, `pitch_arpeggio_threshold`, `current_arpeggio`). The old
> `SetArpeggio`/`SetEnvelope` names (swapped vs their role) were renamed to remove
> that trap. Special case: on 2-bracket modules (tetris) the `0xA0` bracket
> actually carries an arpeggio — see `volume_bracket_is_pitch`.

---

## 5. Tick semantics (50 Hz PAL)

Per frame, `play_tick`:
1. **Delay counter**: 8-bit accumulator `acc += delay_counter_speed`; on overflow
   (`carry`) the whole frame is **skipped**. ⇒ uniform slowdown of
   `(256 − delay)/256` (≈ 46 Hz if delay=20). *See §6: modeled as BPM, not as a
   skip.*
2. Global volume-fade.
3. For each channel: `speed_counter--`; if `== 0` → read commands
   (`ReadTrackCommands`); if `> 1` → `DoFrameStuff` (per-tick effects).

**Per-channel timing**: `speed_counter` is reloaded to the **last LongWait** value
(`(b−0xDF) × speed`) after **every** command that ends the tick — Note, Mute,
**WaitUntilNextRow**. A repeated `WaitUntilNextRow` thus holds a note over N waits.

**DoFrameStuff** (per-tick effects): arpeggio (offset cycle, +note → period),
slide (`SlideValue += speed; period −= SlideValue`, after `counter` delay),
vibrato (triangle, see §7), volume envelope (advances every `step_interval+1` ticks).

---

## 6. Importer architecture (xmrs side)

`dw_module.rs`:
- `DwModule::load` → parse header, samples, position-lists, tracks (dedup),
  envelopes, arps.
- `to_module()` → projects to a DAW `Module` (clips/tracks/timeline/automation).
  - 1 row = `speed` (=3) frames. Quantized notes: `row = (frame−1)/speed`.
  - Each sub-song = one xmrs "song" (cap `MAX_SONGS=32`; song 0 = primary).
  - Segments cut at pattern boundaries (64 rows); clips deduplicated by content.
  - Amiga **LRRL** panning (channels 0,3 left; 1,2 right).

`runtime.rs`: `Simulator` replays the state-machine frame by frame and emits
`TickEvent`s (NoteOn, Mute, VibratoStart/Stop, SlideStart, …) that `to_module`
turns into cells.

`xmrsplayer` (sister crate) consumes the `Module`. Vibrato/tremolo **always** go
through `WaveformState::value_q15` (no exhaustive match on `Waveform`).
`update_frequency` sums the `PitchDelta`s in **semitone space** then converts to
the Amiga curve (Amiga modules apply effect vibrato in **period space** — see §7).

---

## 7. Key decisions & fixes (verified)

- **WaitUntilNextRow rearms `speed_counter`** to the last LongWait (Ghidra case
  `0x83` → `chan[+0x616] = chan[+0x614]`). Without it, held notes collapsed to 3
  frames → track order destroyed + a stray note. *The layout bug.*
- **Delay counter modeled as BPM**, NOT as a frame skip. The skip + row
  quantization drifted the spacing ±1 row (audible "hesitation"). The simulator
  runs every frame (notes on the integer grid); `to_module` sets
  `BPM = 125 × (256 − delay)/256` (xenon2 → 115) on `default_bpm` and each
  `bpm_at_row`.
- **Looping**: a position-list terminated by `0` ⇒ `loop_to = Some(0)`.
  `run_with_loop` renders exactly ONE loop period (re-alignment = LCM of the
  per-channel pass lengths via `first_wrap_frame`) and sets
  `Module::song_loop_to = Some(0)`. Per-channel loops that drift independently are
  modeled via `ChannelLoop` / `Module.channel_loops` (free-run sequencer, no global
  wrap).
- **Vibrato in period space** (Amiga). Ghidra `play_tick` (cmd 0x86): a signed-char
  accumulator ramps by `speed` toward `target`, flipping at the bounds, and the
  Paula period is nudged `period ± accumulator` — *not* a semitone delta (FT2/
  Protracker `doVibrato` does the same). In Amiga mode the player adds the vibrato
  straight to the period (`value_raw()`); linear mode keeps the semitone path. The
  importer's `xm_vibrato_depth` is frequency-mode aware (XM/MOD Amiga = FT2
  `(255*nibble)>>5` period units; XM linear = `nibble/8`; DW = `max`).
- **Slide is exact** (Ghidra `play_tick` effect `case 0x81`): arms
  `SlideValue=0, SlideSpeed=speed, SlideCounter=counter`; per frame, if `counter==0`
  then `SlideValue += speed; period = base − SlideValue`, else `counter--`. `base`
  is recomputed fresh each frame ⇒ a **linear** ramp of `−speed` period units/frame
  (not accelerating), after a `counter`-frame delay. The cmd 0x81 handler is keyed
  on the entry `CLR.W (0x20,A0)` (`sig::CLRW_20_A0`), classified **before** the
  `BSET #1`→vibrato rule (it shares the slide-active `BSET #1`, so without this the
  whole canonical-jump-table family mis-read slides as vibrato).
- **Per-frame arpeggio → `TrackPitch` Points lane.** DW arpeggio is per-frame,
  per-channel, continuous: the held-note branch advances a per-channel pointer one
  offset every frame and adds it to the note's period-table *index* (semitone
  space), wrapping at `0x80` and persisting across notes (reset only on a fresh
  `0x90`/`0xA0` arp command). Baked as a `LaneKind::Points` curve of
  `AutomationValue::Pitch`, evaluated **per frame**. Gated
  `use_arpeggio_pitch_lane = !(command_map && P2)` — excludes only the C4
  oscillator family (grimblood, period-space oscillator, kept on its
  pitch-envelope). The classic `Arpeggio` effect now fires only for non-DW formats.
- **Per-channel `SeqPtr` loop** (cmd 0x89, bad company family): an intro track ends
  `SeqPtr(X) ; SeqAdvance`, redirecting the position sequencer to a sub-sequence at
  `X` that loops on itself — so the intro plays once and only the body loops.
  `follow_seq_ptr_loops` rebuilds each intro-`SeqPtr` channel as
  `[intro] ++ sub-sequence(X)` and records `loop_start_frame` → per-channel
  `ChannelLoop::start_tick`.
- **Note-repeat = TIE on the suppression families** (cmd 0x83, xenon2/beast/bad
  company/empire). The one-frame-early DMA clear is guarded by `CMPI.B #$83,(A1)`
  (`0C 11 00 83`) — if the next byte is `0x83` the clear is skipped, so `0x83`
  re-affirms an already-on channel = no 0→1 strike = a held note. `detect_features`
  sets `note_repeat_is_tie` from that guard quad; `event.rs` routes
  `K::Retrigger → WaitUntilNextRow` when set. Families without the guard
  (bubble bobble/grimblood) genuinely re-strike.
- **End-of-tick transpose re-resolution.** The replayer recomputes a held note's
  base period every frame from the *current* global transpose. The simulator
  re-resolves each NoteOn's `effective_note` at end-of-tick (`runtime.rs::tick`).
- **StopSong detection.** Cmd 0x84's handler kills DMACON inline
  (`DAT_630=0; DMACON=0x000F` or `0x0007` for 3-voice, via inline `BRA`/`BSR`/
  `JSR`). `classify_handler` matches the kill signature both as a bare-branch
  handler and inline in the handler body; `to_module` sets `song_loop_to=Some(0)`
  (the tracker loop-to-start idiom) when a song halted via StopSong.
- **3-voice sub-song format** (header 2). The 8-byte sub-song row is either
  4-voice `[ch0..ch3]` (header 0) or **3-voice `[param(2B), ch0, ch1, ch2]`**
  (header 2). `detect.rs::find_sub_song_header` reads the header displacement from
  `init_main`'s indexed channel-base load; `parse_sub_songs` derives
  `voices = (row-header)/ptr` and reads the speed from the param byte.
- **Effect8** = channel transpose on xenon2 (gated by `enable_channel_transpose`).
  The command_map classifier recognizes the per-channel transpose handler
  `MOVE.B (A1)+,(<slot>,A0)` for `<slot>` ∈ **{0x2f, 0x3}** (0x2f = bubble-bobble
  family; 0x3 = empire/bad-company family, read back by `ADD.B (0x3,A0),D0`). An
  unrecognized slot falls to `Unknown` (0 args) → the value byte is not consumed →
  that channel's stream misaligns.

---

## 8. Known approximations / open points

- Slide: **linear** ramp modeled exactly (`rate = −speed`, `counter` delay, applied
  each tick, bounded by `Clear`). Minor residual: engagement point quantized to the
  row (< 1 row).
- Envelope: sampled at **row resolution** for the per-note envelope animation (fine
  for slow `step_interval`, undersamples fast attacks). A per-frame variant exists
  (`volume_to_volume_envelope`, 1 point/frame).
- `Effect9` RestartPosition with a non-zero target (mid-list jump) not modeled (no
  corpus module needs it).
- Square-waveform (`enable_square_waveform`): not covered / out of corpus.
- SetSpeed (cmd 10) **mid-song** (re-grid on the GCD of encountered speeds) not done.

---

## 9. Tooling

- `cargo run --release --features=demo --example inspect_dw -- <file.dw>`: triage
  (detection, samples, dispatcher, position-lists, event mix, loop, simulation).
  `DW_TRACE=1` dumps the full event trace.
- `cargo run --release --features=demo --example dump_dw_pattern -- "<f>.dw" <song> <pattern> [ch]`:
  rendered grid per pattern/channel.
- Gate test: `load_all_dw_modules_from_music_dir` (strict: loads all of `~/Music/dw`
  or rejects as `dw_no_song`) + `to_module` + `verify_layers_consistent` on 116
  modules.

---

## 10. Corpus & cross-module detection (beyond xenon2)

State: `~/Music/dw/` **116 / 120** load (114 songs + `xenon-sfx`, `leviathan-sfx` =
sample banks with no song, loaded *instruments-only*). The remaining **4**
(`anarchy sfx`, `blood money sfx`, `spellbound sfx` = SFX engines with no PCM table;
`feud fake` = 2 KB placeholder) are cleanly rejected as
`ImportError::InvalidMagic("dw_no_song")`. Rule: a loaded module does NOT need a
song — `dw_no_song` only triggers if samples AND sub_songs AND tracks are all empty
(directive: load the samples even of SFX banks).

**Corpus variants**: canonical new player (xenon2, majority); multi-sub-song
(beast1.* — auto-pick = longest cumulative position-list); **old player** (qball:
32-bit pointers, 18-byte sub-song rows, composite note `sample×12+pitch`, Periods1);
`[C0,B0,A0]` dispatcher (speedball, bubble bobble, grimblood); 8-byte sub-song rows
without speed/delay header; modules without a `MULU` anchor (millennium, fallback
heuristic).

**Detection axes** (`detect.rs`):
- **Permissive opening**: accepted if a `LEA -d(PC), A3` (`47 FA <hi≥F0>`) appears in
  the first 0x1000 bytes (subsumes all prologues).
- **Sample loader**: 4 stages — PC-rel call-graph (`41 FA`/`4B FA` A5/`43 FA` A1),
  A3-rel call-graph (`41 EB`), then PC/A3 flat-scans.
- **Sub-song row width** via `MULU.W #8` (4×u16, no header) / `#10` (2-byte header +
  4×u16) / `#18` (qball, 4×u32). 10-byte row byte-order: `[u8 speed, u8 delay]` if
  byte0≠0 (xenon2, beast1), `[u16 speed]` if byte0==0 (tetris — else ultra-fast
  speed 1).
- **Dispatcher**: `CMP.B/BLT.B/SUBI.B` up to 3 brackets (`[B0,A0,90]`, `[C0,B0,A0]`);
  relaxed fallback `CMP #XX + BLT/BCS` (XX∈0x90..0xD8) for tetris (`[C0,A0]`).
- **Old variant** if 32-bit pointers OR `4A 2B` (TST.B disp(A3)) after the anchor.
- **`enable_sample_transpose`** (tetris family: −3 on each melodic sample;
  sample-info stride **16** instead of 12).
- **`enable_half_volume`** via the **effect jump-table** (`LEA…A2` + `JMP`): true
  only if effect-8 slot = `ST (1,A0)` AND effect-9 slot = `SF (1,A0)` → 2/120
  (obliterator). A blind `50/51 E8` scan matched vibrato handlers → ~110 false
  positives → shifted the `Effect9` arg → ultrasonic notes (bubble bobble bug).

**A3 rebasing / `start_offset`**: `start_offset = signed_disp16 + (init_offset + 2)`.
Any offset dereferenced **via A3** is relative to this base: position-lists, track
offsets, loop targets, envelope tables, arp tables (file = `value + start_offset`).
Do NOT rebase the table *locations* (found via PC-relative LEA = already absolute).
A3 is 0 on almost everything (no-op), but **6 modules** pointed before the image and
were mis-imported before this fix: `bubble bobble` (−0x442), `alfred chicken`
(−0xD18), `grimblood` (−0x388), `gunship2000` (−0xC6A), `krustyssuperfunhouse`
(−0xCD4), `snow strike` (−0xE).

**SetSample threshold**: `dispatcher.sample_threshold().unwrap_or(0xB0)` (was
hardcoded 0xB0 → `[C0,B0,A0]` resolved `0xC3` as sample 19 instead of 3).

**Period-exact pitch (both families)**, `relative_pitch = 0`, Amiga PAL clock
`3_546_894/period`:
- Old composite (qball): `period = Periods1[pitch%12]` direct (1 octave, no
  finetune; note = `sample×12+pitch`).
- New **and old-stream finetune** (flag `period_via_finetune`):
  `period = PERIODS[note + sample.transpose] × (0x369E99 / sample_freq) >> 10`
  (constant `0x369E99` present in every new player). `period_via_finetune` separates
  the *period path* from the Old/New axis: set for all New, and for **Old** binaries
  whose `play` body carries the finetune idiom
  `45 FA <d16> 32 2D 00 0A|0C 74 0A`. These old streams (C7 leviathan table
  @0x320=P2, `mult@instr+0xC`; C8 empire table @0x64a=P3, `mult@instr+0xA`) decode the
  note directly (full-octave index, instrument set by a separate command), not
  composite. qball lacks the idiom → stays composite.

**Sample loops**: `loop_start` (s32 @ +4) read **only** for new player. Old player =
sample-info table built at runtime (no on-disk loop) → samples forced one-shot.

**Bracket A0 = volume OR pitch**: on a 3-bracket cascade (`[sample,A0,90]`)
A0 = volume envelope, 90 = arpeggio. On a 2-bracket cascade (`[sample,A0]`: tetris &
7 others) **A0 = pitch arpeggio** (`volume_bracket_is_pitch`) —
`table_looks_like_arpeggio` discriminates (signed offsets ≤0x18 + `0x80` terminators
vs ramps toward 0x40). Applying those offsets as volume put tetris near silent.

**`to_module` guards**: `MAX_SONGS=32`; audition sub-songs capped at 6000 frames;
anti-infinite-loop guard in `tick_channel` (`consumed > Σ events + 1024`) against
position-lists with no tick-ending event (OOM on bmx simulator.dw).

**Host**: `cpal_player.rs` replaces `module.name` with the file name when
`origin == Dw` (`.dw` files carry no embedded song name).
