# OPL / AdLib FM synthesis — architecture reference

**Status: delivered.** OPL2 FM synthesis ships: a clean-room, `no_std`,
integer-only chip in `generators/opl/` (ROM tables generated const-fn; EG
an independent, cycle-exact reimplementation of the documented YMF262
envelope mechanism) with isolated player
integration (`xmrsplayer/src/channel/opl.rs`). S3M/MPTM modules carrying
AdLib instruments (`InstrumentType::Opl`) now sound instead of playing with
those channels **dead**; every non-OPL module stays **bit-identical**. This
document is kept as the **architecture reference** for that work — the phased
plan below records how it was built (each step independently shippable, the
bit-identical guarantee holding after every step).

---

## 0. Motivation & sizing

OPL2/OPL3 (Yamaha YM3812 / YMF262 — the AdLib & Sound Blaster FM chips) is
the one *systematic* gap left in tracker fidelity after the IT/XM/MOD/S3M
sample paths were calibrated against their reference clones. It is a
**missing feature, not a replay bug**: the sample engine is correct, but
FM instruments produce no sound at all.

Corpus impact (today, `~/Music`):

| format | files | with AdLib | note |
|--------|------:|-----------:|------|
| S3M    | 97    | 1          | `and_this_is_ultrasound.s3m` (24 FM instruments — a *pure*-OPL module, ideal test vector) |
| IT     | 309   | 0 parsed   | IT OPL is **not parsed yet** (importer has no OPL path); real-world IT FM exists (DOS game rips) |

The corpus count is low, but where OPL is used the failure is total
(whole channels silent), and DOS-game soundtrack rips (OMF 2097, Death
Rally, Crusader — all present as dirs in the corpus, mostly non-OPL but
the genre is OPL-heavy) make this the highest-value remaining *class* of
tracker output we cannot currently produce. It is also a clean fit for the
DAW model: an FM voice generator is just another **synth device**.

---

## 1. What already exists (do not rebuild)

### 1.1 Data model — `xmrs/src/tracker/instr_opl.rs`

```rust
pub struct MdiOpl {            // one operator's register fields
    pub ksl: u8, pub multiple: u8, pub feedback: u8,
    pub attack: u8, pub sustain: u8, pub eg: bool,
    pub decay: u8, pub release: u8, pub total_level: u8,
    pub am: bool, pub vib: bool, pub ksr: bool, pub con: bool,
}
pub struct MdiInstr {
    pub modulator: MdiOpl, pub carrier: MdiOpl,   // 2 operators
    pub modulator_wave_select: u8, pub carrier_wave_select: u8,
}
pub struct InstrOpl {
    pub element: MdiInstr,
    pub volume: u8,            // 0..63
    pub relative_pitch: i8,    // 0 <=> C-4
    pub finetune: Finetune,    // Q1.15 sub-semitone
}
```

`InstrumentType::Opl(InstrOpl)` is a variant of the instrument enum
(`xmrs/src/core/instrument.rs`, cfg `import_s3m`). **Key observation:**
the model is decoded *register fields*, two operators only. S3M/IT AdLib
patches are **always 2-operator OPL2 voices** — there is no 4-op / OPL3
data in these formats. That scopes the emulator (see §3).

### 1.2 Importer — `xmrs/src/tracker/import/s3m/s3m_module.rs`

`S3mOplInstr::to_instr_opl()` (line ~385) decodes the 13-byte AdLib patch
record (`SCRI` instruments) into `InstrOpl`, including `c2spd →
(relative_pitch, finetune)` via `c4freq_to_relative_pitch`. This is
already correct and exercised by the format-load gate.

### 1.3 The silent gate — `xmrsplayer/src/channel/tick0.rs:94`

```rust
// Non-renderable instrument variant (InstrMidi / InstrOpl / InstrSid …).
// xmrsplayer's sample engine only handles InstrumentType::Default; the
// specialised synth / MIDI-out paths are the responsibility of
// downstream code that wraps the player.
false
```

OPL falls into this branch → no voice is installed → silence. This RFC
replaces that `false` for the OPL case with a real synth path.

---

## 2. Architecture decision

There are two shapes. We pick **one shared chip with per-channel output
buffers**, mirroring the reference (Schism `player/snd_fm.c`), because it
is the only one that is *both* fidelity-faithful and DAW-native.

**Rejected — per-voice mini-chip (one 2-op renderer per `Voice`).**
Tempting for MPE/per-voice scopes, but the OPL chip has **global state**
the per-voice model cannot share: the 0xBD register (AM depth / Vib depth /
rhythm mode) is chip-wide, and OPL3 stereo/4-op linking couples operator
pairs. Per-voice emulation would diverge from every reference player. Drop
it.

**Chosen — one `OplChip` owned by `Voices`, Schism-style allocation.**
A single chip instance; tracker channels that trigger an OPL instrument
grab an OPL slot through an allocator that mirrors `snd_fm.c`'s
`GetVoice`/`SetVoice`/`FreeVoice`. This matches our oracle (Schism)
exactly and keeps all global chip state correct.

**DAW-native routing for free.** The MAME/Nuked cores expose
*per-channel* output (`OPLUpdateMulti(chip, buffers[OPL_CHANNELS], …)` in
snd_fm.c renders each of the chip's channels to its own buffer). So we get
single-chip fidelity **and** can feed each tracker channel's OPL output
into that channel's existing insert/send strip
(`Channel::next_sample_with_inject`, the audio-clip path already does
exactly this — see `Voices::accumulate_audio_clips`). Phase A sums the
chip at master (simplest, matches Schism); Phase B upgrades to per-channel
routing with no fidelity change.

```
            tracker channel N (OPL instr)
                  │  note / vol / pan / pitch  →  register writes
                  ▼
   ┌─────────────────────────────────────────┐
   │  OplChip (one instance, owned by Voices) │  ← global 0xBD, OPL3 mode
   │  18 (OPL3) or 9 (OPL2) hardware channels │
   └─────────────────────────────────────────┘
                  │ per-hw-channel i32 buffers (OPLUpdateMulti)
                  ▼
   Phase A: sum → master    Phase B: route each to Channel::insert_chain
```

---

## 3. The emulator (the real work)

### 3.1 Constraints from xmrs

- **MIT-licensed crate.** MAME `fmopl` (Schism's core) is **GPL** → cannot
  vendor. Nuked-OPL3 (OpenMPT's core) is **LGPL-2.1** → incompatible with
  static linking into an MIT crate without relicensing friction. So we do
  **not** vendor either.
- **`no_std`, integer-only.** xmrs has folded *every* `f32` site to integer
  Q-format (see `Cargo.toml` `std` note). The OPL chip is natively
  integer (log-sin + exp ROM tables, phase accumulators) so this is a
  *good* fit, not a fight — but it forbids pulling a float-based crate.

### 3.2 Decision: clean-room integer **OPL2** core, OPL3 stereo bits only

Write a fresh `no_std` integer OPL core in `xmrsplayer/src/opl/` (new
module), modelled on the *publicly documented* register/algorithm spec
(the YMF262 / YM3812 application manual, the well-known log-sin & exp
tables), **not** copied from any GPL/LGPL source. Scope it to what the
corpus needs:

- **OPL2 (YM3812) 2-operator voices** — exactly the S3M/IT patch shape.
  9 channels, 2 operators each, 4 base waveforms (OPL2) + the 4 extra
  OPL3 waveforms (cheap, and `wave_select` in the patch can index them).
- **OPL3 stereo panning** — the one OPL3 feature these formats use
  (`OPL_Pan` sets L/R bits in the feedback/connection register). Enable
  OPL3 mode for the L/R bits and the 18-channel bank; keep 2-op only.
- **Skip for now:** OPL3 4-operator linking, rhythm/percussion mode. No
  corpus module needs them; leave a `// TODO OPL3 4-op` seam.

The deliverable is small precisely because of this scoping: a 2-op OPL2
core is a few hundred lines (phase generator + envelope generator per
operator, one ROM sine table, modulator→carrier FM, feedback), far less
than the ~2000-line MAME/Nuked OPL3 files which carry 4-op and rhythm.

> If a vetted, MIT/BSD/0-clause, `no_std`, integer Rust OPL3 crate is
> found, swapping the clean-room core for it behind the same `OplChip`
> trait is a drop-in alternative — but per the local-sources-first policy,
> do not pull one without a license audit and explicit go-ahead.

### 3.3 `OplChip` surface

```rust
// xmrsplayer/src/opl/mod.rs   (feature = "opl")
pub struct OplChip { /* operators, channels, global regs, phase/env state */ }

impl OplChip {
    pub fn new(sample_rate: u32) -> Self;     // native 49716 Hz, resampled
    pub fn reset(&mut self);
    pub fn write(&mut self, reg: u16, val: u8);          // register poke
    pub fn render(&mut self, out_per_channel: &mut [[i32; 2]]); // 1 frame, N hw-channels
}
```

`render` advances the chip by one *output* frame and writes each hardware
channel's stereo contribution. The native chip rate is `OPLRATEBASE =
49716 Hz`; resample to the mix rate with the same fixed-point Q32.32 step
the audio-clip path uses (`AudioClipRuntime`), or run the chip at mix rate
with the rate-divisor approach Schism uses (`OPLRATEDIVISOR`). Resampling
at the chip boundary keeps timbre rate-independent; **decide in Phase A**
and lock it (it affects the golden hashes for OPL modules only).

---

## 4. Driver layer (mirror `snd_fm.c`, in Rust)

A thin `OplDriver` translates tracker-channel gestures into register
writes, owning the channel-allocation bookkeeping. This is a direct,
auditable port of Schism's high-level driver (which is *not* GPL-encumbered
algorithmically — it is the documented AdLib programming sequence; we
re-implement, citing it):

| tracker gesture | reference (`snd_fm.c`) | register action |
|---|---|---|
| trigger OPL instr on channel | `OPL_Patch` + `SetVoice`/`GetVoice` | allocate hw slot, program operator regs (`AM_VIB`, `KSL_LEVEL`, `ATTACK_DECAY`, `SUSTAIN_RELEASE`, `WAVE_SELECT`, feedback/connection) from `InstrOpl` |
| note pitch (and porta/vibrato/slide) | `OPL_HertzTouch` + `milliHertzToFnum` | note → milli-Hertz → (F-number, block) → write `FNUM_LOW` (0xA0) + `KEYON_BLOCK` (0xB0) with key-on bit |
| note off / key-off | `OPL_NoteOff` | clear key-on bit in 0xB0 |
| volume (cell vol, tremolo, fade) | `OPL_Touch` | `63 - round(oplvol*smpvol/64)` into carrier TL (and modulator TL iff connection=additive) |
| pan (S8x, channel pan) | `OPL_Pan` | L/R bits in feedback/connection reg |
| note cut / new note action | `FreeVoice` | release hw slot |

Channel-allocation policy (from `snd_fm.c`): `opl_to_chan[]` /
`opl_from_chan[]` maps maintain a one-OPL-slot-per-active-tracker-channel
assignment; `PortBases[9] = {0,1,2,8,9,10,16,17,18}` gives each channel's
operator base offsets. Port this verbatim — the offsets are hardware
geometry, not authored code.

**Pitch is the subtle part.** `milliHertzToFnum` in Schism uses float; do
it in integer. The note→frequency already exists in xmrs as the
period/pitch tables (`PeriodHelper`, `c4freq_to_relative_pitch`), so derive
the target frequency from the same path the importer used to set
`relative_pitch`/`finetune`, then convert Hz→(fnum,block) with an integer
version of the documented formula (`fnum = (mHz << (20-block)) /
(conversion·1000)`, block chosen by the documented mHz thresholds). This
keeps porta/vibrato/arpeggio "just work": they already produce a per-tick
effective period in the channel; map that period→Hz→`OPL_HertzTouch` every
tick, exactly as the sample path maps it to a resample step.

---

## 5. Player integration

### 5.1 Where the chip lives

`Voices` (`xmrsplayer/src/voices.rs`) gains an `opl: Option<OplDriver>`,
`None` unless the module has ≥1 `InstrumentType::Opl` instrument. The
`Option` is the bit-identical gate: no OPL instruments → `None` → not one
instruction of the OPL path runs, golden hashes unchanged.

### 5.2 Trigger path

In `tick0.rs`, the `InstrumentType::Opl(opl)` arm (currently the silent
`false`) instead:
1. allocates/reuses an OPL hw slot for this tracker channel,
2. programs the patch (`OplDriver::patch`),
3. sets pitch + key-on (`OplDriver::note_on`),
4. records on the `Channel` that it currently drives an OPL slot (so
   per-tick effects and note-off route to the driver, not the sample
   pool).

No `Voice`/`StateInstrDefault` is installed for OPL notes — OPL voices live
in the chip, not the sample pool. (`live`/`ghosts` stay sample-only; NNA
does not apply to FM here, matching Schism.)

### 5.3 Per-tick path

Each tick, after channel effects resolve, push the channel's current
effective pitch/volume/pan to the driver (`HertzTouch`/`Touch`/`Pan`).
Then in the mix, render the chip once per output frame and add its output
(Phase A: to master; Phase B: per hw-channel into the owning channel's
insert chain).

### 5.4 Mix cadence

The chip renders at **output-frame** granularity (like every other voice),
not tick granularity — `Voices::mix`/`samples_from_channels` calls
`opl.render_frame()` and sums. Register writes happen at tick boundaries;
sample generation is continuous. This matches hardware and Schism.

---

## 6. Feature gating & the bit-identical guarantee

- New cargo feature **`opl`** on `xmrsplayer` (and the `opl/` module is
  `#[cfg(feature = "opl")]`). `import_s3m` already gates the data model on
  the `xmrs` side.
- Runtime gate: `Voices::opl` is `None` for every module with no OPL
  instrument. The existing golden-corpus and cycle-exact suites (all
  sample-based) therefore stay **bit-identical** — assert this explicitly
  by re-running `golden_corpus` + `cycle_exact_pcm` after each phase, same
  as every prior DAW step.
- OPL modules get their **own** golden entries (hash captured once the
  Phase-A resampling decision is locked), kept separate from the
  bit-exact tracker goldens.

---

## 7. Verification

Reuse the existing Tier-1/Tier-2 apparatus (`it/PLAN.md`, `it/BASELINE.md`,
`it_oracle_diff.rs`). OPL cores differ between emulators, so this is a
**tolerance** oracle, never bit-exact — exactly like the IT/XM oracle.

- **Tier-1 (robustness).** Add OPL modules to the format-load gate: load
  fully, instantiate the chip, render N seconds, no panic, bounded output.
  `and_this_is_ultrasound.s3m` is the anchor (pure OPL → exercises only
  the FM path).
- **Tier-2 (fidelity).** `it_oracle_diff` vs **Schism** (fmopl3, our S3M
  reference) and cross-check vs **OpenMPT** (Nuked-OPL3). Targets in the
  same spirit as IT: `env_corr` ≥ ~0.9 (right notes/dynamics/timing),
  `spec_sim` reasonable (FM timbre is sensitive to envelope-rate and
  log-sin table accuracy — expect a lower floor than sample playback),
  `level_db` within a few dB. A clean-room core will not bit-match either
  reference; the bar is *audibly faithful*, measured by the bounded
  metrics.
- **Ear-check.** `and_this_is_ultrasound.s3m` end-to-end vs Schism.

Diagnostic tools to add (kept, per policy): an `opl_regtrace` (dump the
register-write stream for a module, diff vs a Schism trace) and
`opl_voice_render` (solo one OPL channel).

---

## 8. Phasing

Each phase is shippable and preserves the bit-identical guarantee for
non-OPL modules.

- **Phase A — sound.** Clean-room OPL2 core (`opl/`) + `OplDriver` +
  `tick0` trigger path + master-summed mix + Phase-A resampling decision
  locked. Outcome: `and_this_is_ultrasound.s3m` *plays*, Tier-2 vs Schism
  in range. Lock OPL golden hashes. **This is the milestone the user
  asked for.**
- **Phase B — DAW-native routing.** Switch from master-sum to per-hw-channel
  buffers feeding each tracker channel's insert/send strip
  (`Channel::next_sample_with_inject`). Fidelity unchanged; OPL now
  obeys per-channel inserts/sends/mutes like sample channels. (Audio-clip
  path is the working precedent.)
- **Phase C — IT OPL import.** Add the OPL parse path to the IT importer
  (`it_sample_header.rs`/`it_instrument.rs`): IT stores AdLib patches in
  the sample slot behind an OPL marker. Populate `InstrumentType::Opl`;
  the whole player path from Phase A/B then applies unchanged.
- **Phase D (optional) — OPL3 extensions.** 4-operator linking + rhythm
  mode, *iff* a real module in scope needs them (none in the current
  corpus). Behind the same core, additive.

---

## 9. Risks & open questions

1. **Resampling vs run-at-rate (Phase A).** Chip native 49716 Hz vs mix
   rate. Resample at the chip boundary (Q32.32, reuse `AudioClipRuntime`
   logic) is cleaner for rate-independence; run-at-divisor (Schism) is
   closer to the reference. Decide in A; it sets the OPL golden.
2. **Envelope-generator accuracy.** FM timbre lives in the operator EG
   rates (attack/decay/sustain/release) and the KSL/KSR scaling. The
   documented rate tables must be exact or timbres drift — this is where a
   clean-room core is most at risk and where the Tier-2 `spec_sim` floor
   matters. Validate operator-by-operator against a Schism register trace
   if needed.
   **Done (fidelity v2).** The EG is now a cycle-accurate, independent
   reimplementation of the documented YMF262 envelope mechanism (GPL
   emulators such as Schism's `player/fmopl3.c` consulted only as behavioural
   references — no code or table copied): a shared global `eg_cnt` gates each
   phase on `eg_cnt & ((1<<shift)-1)`, a dithered increment table supplies the
   per-phase step, and the attack approaches 0 proportionally
   (`vol += (~vol·inc)>>3`). The attenuation domain (0..511), rate-shift
   /rate-select index math, KSR (`kcode>>KSR`) and sustain table all
   match the chip exactly — no calibrated timing anchor. The operator output
   was also lifted to the real **12-bit** width (±4084), so FM modulation is
   ×1 and feedback is the single law `(prev+cur)>>(9-fb)` for both FM and
   additive channels — the previous `MOD_INDEX_Q8` and per-mode feedback
   compensations are gone. Measured against Schism via `examples/opl_compare`
   on `and_this_is_ultrasound.s3m`: decay-rate error 1.13→0.60 dB/s,
   timbre cosine 0.942→0.957; non-OPL renders stay bit-identical.
3. **Volume law.** `OPL_Touch`'s `63 - round(oplvol*smpvol/64)` is the
   ST3-measured curve (Bisqwit's note in `snd_fm.c`). Port it exactly;
   it is the audible volume mapping and a likely source of `level_db`
   drift if approximated.
4. **License hygiene.** The chip core must be demonstrably clean-room
   (spec-derived ROM tables + documented algorithm), not a line-by-line
   transcription of fmopl/Nuked. The *driver* layer (note→fnum, allocation,
   volume law) is the documented AdLib programming procedure and is safe to
   re-implement citing `snd_fm.c`.
5. **No-core-changes.** Everything here is `xmrsplayer` + the existing
   `xmrs` data model (already present). No `xmrs/src/core/` change is
   required — `InstrumentType::Opl` already exists. If Phase C surfaces a
   need for a core field, raise it before touching core.

---

## 10. File map (proposed)

```
xmrsplayer/src/opl/mod.rs        OplChip: register file + render (clean-room OPL2/OPL3-stereo)
xmrsplayer/src/opl/operator.rs   phase + envelope generator, log-sin/exp tables
xmrsplayer/src/opl/driver.rs     OplDriver: channel allocation, note→fnum/block, Touch/Pan/Patch
xmrsplayer/src/voices.rs         + opl: Option<OplDriver>  (None gate)
xmrsplayer/src/channel/tick0.rs  Opl arm: trigger/patch/note-on instead of `false`
xmrsplayer/src/channel/tick.rs   per-tick HertzTouch/Touch/Pan push
xmrsplayer/examples/opl_regtrace.rs, opl_voice_render.rs   diagnostics
```

Reference (read-only, local): `~/Documents/schismtracker-20251014/player/{snd_fm.c,fmopl2.c,fmopl3.c}`,
`~/Documents/openmpt-master/soundlib/OPL.cpp`. Data model:
`xmrs/src/tracker/instr_opl.rs`. Importer: `s3m_module.rs::to_instr_opl`.
Oracle apparatus: `xmrs/src/tracker/import/it/{PLAN,BASELINE}.md`,
`xmrsplayer/examples/it_oracle_diff.rs`.
