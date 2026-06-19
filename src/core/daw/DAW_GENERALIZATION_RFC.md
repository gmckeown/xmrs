# DAW generalization — architecture reference

**Status: delivered.** The critical path (§0–§4) plus the signal-graph
hardening and audio refinements all shipped, bit-identical at every step.
This document is the **architecture reference** for the DAW layer of
`xmrs`/`xmrsplayer`: the design rationale, the compatibility contract, and
the section vocabulary (`§1B`, `§2A`, `§3B`, …) that the player's code
comments cite. It is no longer a plan — read it to understand *why* the
layer is shaped the way it is, not to track work.

Goal: make `xmrs` a **general-purpose DAW** (per-voice modulation, MPE,
effect chains, audio tracks) **without ever regressing** the import and
render of the historical soundtrackers (XM, IT, MOD, S3M, **DW**).

Guiding principle — already proven on the DW importer: **generalize the
core schema**, and let each importer keep *projecting* tracker semantics
into that more general schema. Music, samples and effects may be
reshaped at import time to fit the schema, **as long as the projection
stays audio-identical**.

The golden rule that makes "without regressing" mechanical:

> **Every generalization ships with a *tracker-equivalence default*: the
> new general mechanism, fed by the tracker projection, renders
> BIT-IDENTICALLY to the pre-generalization player.**

Combined with a frozen golden corpus (§0), this rule is *verifiable*, not
merely promised.

---

## 0. Compatibility contract — the backbone

The whole layer rests on a single invariant: tracker renders never move.

1. **Frozen golden corpus.** Reference renders, hashed bit-for-bit over a
   representative sample: XM + IT + MOD/S3M + **DW** (one rep per DW
   cluster C1–C8 plus the hard cases: bubble bobble, grimblood,
   archipelagos, xenon2, bad company). This extends the existing harness
   (Paula oracle under `dw/oracle/`, `load_all_dw_modules_from_music_dir`,
   the xmrs suites).
2. **Per-change labelling.** Each change is either:
   - *behavior-preserving* → golden render must stay bit-identical; or
   - *additive* → tracker path unchanged, proven by the golden **plus** an
     assertion that the importer never emits the new construct.
3. **§0.3 — Serde round-trip.** Serialized modules stay readable; any core
   type change goes through a transparent serde repr (an `Arc<[T]>`
   serializes as `Vec<T>`; a new field is `#[serde(default)]`). Verified
   at runtime: `daw_capstone.rs::daw_project_survives_serde_round_trip`
   JSON-round-trips a `Module` carrying *everything* new (Arc samples,
   `#[serde(skip)]`-state inserts, buses/sends/master, a `ModScope::Voice`
   lane, a `NoteExpression` cell, `Track::Audio`) and renders
   **bit-identically**.

Invariants worth keeping in mind (each cost a debugging session):

- Do **not** merge the two LFOs (`state_auto_vibrato` vs
  `effect_vibrato_tremolo`): the structures diverge too much. Lane
  summation (§1B) creates *N instances*, it does not merge the impls.
- Do **not** touch the DW concert-pitch offset (~28 cents, intentional).
- `Pitch` (semitone space, post-slide) **does not commute** with the
  period-space slide → the DW transpose stays baked end-of-tick. It is
  deliberately not routed through a lane.

---

## 1. Coherence gains (behavior-preserving)

Two independent items, each gated golden-identical.

### 1A — Shared audio pool (`Arc`)

`SampleDataType` holds its PCM behind a shared handle
(`Mono8(Arc<[i8]>)`, …) instead of an owned `Vec`. The DW importer dedups
at import — identical PCM ⇒ same `Arc` (hash cache where
`resolve_synth_instrument` used to clone). *back to the future.dw* drops
from ~60 PCM copies to 16 real samples.

`Arc<[T]>` is immutable by design: sample editing (a DAW feature, not part
of this layer) never mutates in place — it produces a **new** `Arc<[T]>`
(full-buffer replacement), exactly as a tracker editor rewrites a whole
sample. No CoW, no `make_mut`. Read sites are unchanged (`Arc<[T]>` derefs
to `&[T]`). Compatible because the PCM bytes are identical, just shared;
serde is transparent. Lives in `core/sample.rs`; lays the brick for the
asset pool (§4).

### 1B — Summed lane combination

A real DAW sums *all* modulators of a parameter. The channel carries a
**`Vec` of modulator states** per `(target, kind)` and the player *folds*
them by the value-space monoid (sum of pitch/volume deltas), instead of
the old `find_map()` (first lane wins).

Player-only, no core change: the data model already allows N lanes per
target (`Module::lanes_for` yields an iterator); only the player consumed
just one. Compatible because the tracker importers emit **exactly one**
lane per `(target, kind)` per channel ⇒ N = 1 ⇒ sum ≡ first-match (an
additive assertion guards "no importer emits > 1 same-`(target, kind)`
lane"). This is what lets a DAW author stack two LFOs on one parameter.
Lives in `xmrsplayer/src/channel/lanes.rs` and the channel state in
`channel.rs`. The summed accessors are the `§1B` references throughout the
player.

---

## 2. Per-voice modulation — the fault line

This is what turns "tracker-as-DAW" into a *general* DAW.

Diagnosis: automation is bound to the **track**
(`AutomationTarget::TrackPitch(u32)`), but voices are **polyphonic** (a
channel's NNA ghosts sound together). A per-track lane therefore cannot
express modulation **independent per voice** (nor MPE). This is also the
structural reason the volume envelope **must** stay in the instrument
(per-voice), not in a lane.

Staged so the risky step carries **no** new feature.

### 2A — Voice modulation bus (pure plumbing, golden-identical)

`xmrsplayer/src/voice_mod.rs` introduces `VoiceMod`: an explicit per-tick
snapshot separating a voice's **base** (period/volume/panning/
channel-volume) from the **channel deltas** (vibrato/tremolo/panbrello/
arp/Points). `Channel::voice_mod(&self)` assembles it (pure read of
`self`); `StateInstrDefault::apply_voice_mod(&mut self, &VoiceMod)`
combines it with the voice's **own** envelope and sets the voice's `step`.

This is the one *behavior-preserving but risky* step — all the golden
vigilance concentrates here. `apply_voice_mod` reproduces the old
`tickn_update_instr` arithmetic verbatim (same Q1.15 constants, same
order); with a single consumer (the live voice) the render is identical.
It is the exact point §2B re-points **per voice**.

### 2B — Modulation scope (additive)

```rust
// xmrs/src/core/daw/automation.rs
pub enum ModScope {
    Track,   // tracker-equivalence default: the track's live voice (= today)
    Voice,   // each voice of the track (live + NNA ghosts) reads the bus
}
```

A `#[serde(default)] scope: ModScope` field on `AutomationLane`. Building
on the §2A seam, a `ModScope::Voice` lane applies the channel deltas to
**every** voice: each ghost runs `tick_scoped(&channel_voice_mod)` —
`apply_voice_mod` over a `VoiceMod` whose base is the ghost's frozen
gels and whose deltas are the channel's — storing a per-voice
`scoped_volume`. Compatible because no importer emits `Voice`
(`voice_scope_active` is always false ⇒ ghosts take the unchanged frozen
path). Lives in `automation.rs`, `voice.rs`, `channel/ghosts.rs`,
`channel/mix.rs`, `channel/lanes.rs`.

### 2C — Per-note expression (additive)

A `NoteExpression` (pitch-bend, volume, pan — MPE) on the note event,
snapshotted at trigger and carried by the voice, feeding the `Voice`
scope of §2B. Tracker `Cell`s leave it `None` ⇒ inert. Result: a held
ghost and a fresh voice on the same track carry **independent**
modulation — MPE becomes expressible. Lives on `Cell` and in
`state_instr_default.rs`.

---

## 3. Signal graph (net-new, no threat to compat)

Trackers project an **empty** chain ⇒ identical by construction. Inserts
shipped *before* §3A so that `DeviceParam` is not dead config.

### 3A — Extensible `AutomationTarget` (`DeviceParam`)

`AutomationTarget::DeviceParam { track, device, param }` alongside the
built-in mixer targets (additive). `Device::set_param(param, Q15)` applies
a normalized scalar. The player's `update_device_params` (run at the head
of `tick0`/`tick`) drives insert-device params from these lanes, guarded
`devices.is_empty()` ⇒ total no-op on the tracker path.

### 3B — Effect chain (inserts + sends/returns + master)

The "real DAW" core, in `core/daw/device.rs`:

- `Device` (a stateful enum — `process(&mut self)` — of native fixed-point
  effects; see §6.2) and `DeviceChain { devices, bypass }` (front-to-back
  `process`).
- `Module::channel_inserts: Vec<DeviceChain>` (per mixer channel),
  `buses: Vec<Bus>` + `channel_sends: Vec<Vec<Send>>` (returns/aux), and
  `master_chain: DeviceChain` — all `#[serde(default)]`, empty on the
  tracker path. **Distinct from `MixPlugins`** (opaque VST chunks, kept
  only for round-trip): this is the *native, executable* chain.

The player keeps runtime clones (`Channel::insert_chain`, `Voices`'
`bus_chains`/`master_chain`); `Voices::mix` routes each channel's sends
(post-insert × level) into per-bus accumulators, sums the buses, then runs
the final mix through `master_chain`. Empty everywhere on the tracker path
⇒ loops not entered ⇒ bit-identical. These are the pervasive `§3B`
references in `voices.rs`, `channel/mix.rs`, `channel.rs`.

### 3C — Audio track

`Track::Audio` is an additive `Track` variant carrying recorded PCM
(reusing `Sample`, hence the §1A `Arc` and an optional loop) plus a
`source_rate`. The player precomputes `Voices::audio_clips`
(`AudioClipRuntime { position_tick, end_tick, step_q32, cursor_q32, … }`)
and, in `mix`, resamples each active clip by **linear interpolation**
(Q32.32 cursor). Clips are injected **pre-insert** into their
`target_channel` (`Channel::next_sample_with_inject`), so they traverse
the same insert chain and sends as that channel's notes; an out-of-range
target falls back to the master sum (audio is never lost). A muted target
channel silences the clip but its cursor keeps advancing (phase-correct
resume on unmute). `reset_for_goto` resets the free-running cursors so a
seek re-enters each clip cleanly at its head. Empty `audio_clips` ⇒
bit-identical.

Remaining limitation (out of scope): sub-tick timeline-locking under
variable tempo — a seek into the *middle* of a region restarts the clip
at its head rather than playing the aligned excerpt. True timeline-lock
needs the sequencer's sample↔tick integration.

---

## 4. Referenced asset pool

`Module.assets: Vec<Sample>` (`#[serde(default)]`, **empty for every
tracker import**) plus `core/daw/track.rs::AudioSource { Inline(Sample),
Pooled(usize) }` on `Track::Audio`. One recording placed on the timeline
N times is **stored once** (`Module::push_asset` → index) and resolved
pool-aware via `Module::track_audio(track_idx) -> Option<(&Sample, u32)>`.
The player resolves audio through `track_audio` instead of the track
directly. Compatible because no importer fills `assets` ⇒ every track
stays `Inline` ⇒ identical resolution. `Pooled` is a **transparent**
indirection, proven by `pooled_asset_renders_identically_to_inline` (a
shared asset renders byte-identically to two `Inline` copies). Generalizes
the per-instrument `Arc` of §1A into real asset management.

Deferred: content-dedup on insertion, and instruments referencing the pool
(already `Arc`-deduped by §1A — marginal gain, high regression surface).

---

## 6. Beyond the engine

### 6.2 — Real DSP content

§3B delivers the signal-graph *plumbing*; §6.2 is the **DSP content** that
flows through it. This is now delivered as native, fixed-point `Device::`
variants (ParamEq, Distortion, Compressor, Echo, ModDelay, Reverb,
I3DL2Reverb, Gargle) in `core/daw/device.rs`, with the reusable primitives
in `core/daw/dsp.rs`. The IT/MPTM importer projects a module's DirectX-DMO
mix-plugin table onto these devices automatically
(`tracker/import/it/it_plugin_convert.rs`), so a plugin-bearing `.it`
plays with its effects by default while plugin-free modules stay
bit-identical.

Still out of scope (a "usable product", not the engine): real-time
audio/MIDI I/O, a CLAP/VST plugin host, transport/recording/undo, and the
UI (arrangement, piano-roll, mixer). Those dominate the remaining effort
and belong to other layers.
