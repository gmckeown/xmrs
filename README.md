# XMrs — SoundTracker file format library

A `no_std`-friendly Rust library to read, edit and serialize SoundTracker
data — with pleasure. It imports the tracker classics (MOD, XM, S3M, IT)
alongside the more exotic DW (David Whittaker, Amiga) and SID (Commodore 64)
formats into one in-memory `Module` model, and carries chip-synthesis
instruments — SID through the cycle-accurate *sidera* core and clean-room
OPL2/AdLib FM.

Because *"Representation is the Essence of Programming"*.

## Supported formats

Historical module files — imported into xmrs's in-memory `Module` model:

| Format | Description                             | Feature       |
| ------ | --------------------------------------- | ------------- |
| MOD    | Amiga ProTracker / Soundtracker family  | `import_mod`  |
| XM     | Fast Tracker II                         | `import_xm`   |
| S3M    | Scream Tracker 3                        | `import_s3m`  |
| IT     | Impulse Tracker (incl. OpenMPT extras)  | `import_it`   |
| XI     | Fast Tracker II instrument file         | `import_xm`   |
| DW     | David Whittaker custom Amiga player (`.dw`) | `import_dw` |
| SID    | Commodore 64 / MOS6581 — cycle-accurate via *sidera*, three Rob Hubbard player generations | `import_sid`  |

The umbrella feature `import` enables all of them at once (and is on by
default together with `std`).

Unlike the tracker formats above, a `.dw` file is not a data container
but a self-contained 68000 binary with the music data embedded inside
David Whittaker's own Amiga replayer. There is no magic header: the
importer's detection layer scans the first few KB of the payload for a
characteristic opcode quadruple, walks the call graph from there, reads
the replayer's init-time constants, and parses the data tables they
point to — re-interpreting the original per-voice playback semantics
into xmrs's `Module` model (per-voice loops, period-space vibrato,
arpeggio pitch lanes, sub-song selection, …).

## The `Module` model

xmrs exposes one editor-friendly data model, regardless of what was
loaded:

```text
Module ─┬─ Instrument ─┬─ InstrDefault ─┬─ VoiceSetup     (envelopes, vibrato, filter, panning)
        │              │                ├─ InstrumentBehavior  (NNA, DCT, DCA)
        │              │                ├─ Keyboard       (per-input-note sample + transposition)
        │              │                ├─ InstrMidi
        │              │                └─ Sample         (loop & sustain-loop)
        │              ├─ InstrMidi      (MIDI instrument)
        │              ├─ InstrOpl       (Yamaha OPL)
        │              ├─ InstrSid       (MOS6581 SID voice)
        │              └─ InstrRobSid ── InstrSid  (Rob Hubbard-style)
        └─ Track ─┬─ Notes { rows: Vec<Cell> } ──┬─ CellEvent       (None, NoteOn{pitch,velocity}, NoteOnGhost{…}, InstrReset, NoteOff{retrig}, NoteCut, NoteFade)
                  │                              ├─ Vec<TrackEffect>  (discrete / per-trigger effects only — see below)
                  │                              └─ NoteExpression    (per-note pitch-bend / volume / pan — MPE; neutral for every tracker import)
                  ├─ Euclidean { events, steps, rotation, prototype: Cell, humanize_* }  (Bjorklund generator)
                  └─ Audio { source: AudioSource, source_rate }  (recorded-audio clip — inline or pooled, see below)
```

The continuous-modulation families (Vibrato* / Tremolo* /
Panbrello* / Portamento* / TonePortamento / VolumeSlide* /
ChannelVolumeSlide* / PanningSlide*) and every song-level global
(Bpm / Speed / GlobalVolume + slides) live on
`Module::automation: Vec<AutomationLane>` instead, extracted upstream
from `TrackImportUnit` at import time. The `MidiMacro` global effect
relocates to `Cell::effects` as `TrackEffect::MidiMacro` during
materialisation. Navigation globals (`PatternBreak` / `PositionJump` /
`PatternLoop` / `PatternDelay`) are absorbed into
[`TimelineMap`](#the-daw-timeline-layer) at import time.

Tracks are arranged on the timeline by `Clip`s; see
[*The DAW timeline layer*](#the-daw-timeline-layer) below for the
full layout.

`InstrDefault` is split into three orthogonal sub-types so each
concern lives in one place: [`VoiceSetup`] (how the voice sounds
once triggered), [`InstrumentBehavior`] (what happens when the same
instrument retriggers), [`Keyboard`] (per-input-note sample / pitch
remap, used by IT drum kits).

Per-module playback semantics live in two orthogonal fields on
`Module`:

- **`quirks`** ([`PlaybackQuirks`]) — the switches each historical
  tracker flips: FT2 pitch-slide overflow, S3M period clamp,
  arpeggio LUT, pattern-loop resume, tremor state, IT vibrato
  tick-zero, pan reset policy, and so on. This is the only field
  playback reads.
- **`origin`** (`Option<`[`ModuleFormat`]`>`) — source-format tag
  (`Mod`, `S3m`, `Xm`, `It`, `Sid`, `Dw`; `None` for
  editor-authored modules). Informational only — no playback
  decision reads it.

Preset quirk bundles are free functions in
[`xmrs::tracker::profiles`]: `profiles::ft2()`, `it214()`,
`it215()`, `st3()`, `pt()` — each returns a `PlaybackQuirks`. An
editor-authored module with no historical bug emulation uses
[`PlaybackQuirks::default()`] (all quirks off). Importers pick the
right preset for the source file and overlay any header-driven
flags on top.

`Module` also carries a [`Vec<ChannelDefault>`] for per-channel
initial state (panning, channel volume, mute, surround). S3M's
`channel_settings` and IT's `initial_channel_pan` /
`initial_channel_volume` populate it; XM/MOD leave it empty.

## The DAW timeline layer

The historical `Pattern → Row → TrackUnit` matrix is gone — `Module`
stores songs natively in a DAW-style layout, populated by the
importers (or on demand via
[`xmrs::daw::build_timeline::build_timeline_layer`]):

```text
Module ─┬─ tracks         : Vec<Track>           (one per song + instrument)
        ├─ clips          : SortedClips          (placements on the timeline)
        ├─ automation     : Vec<AutomationLane>  (lanes for every continuous modulation
        │                                         + Bpm / Speed / GlobalVolume song-levels,
        │                                         extracted at import or via the edit API)
        ├─ timeline_map   : TimelineMap          (linearised pattern_order)
        ├─ song_loop_to   : Option<u32>          (XM/MOD restart byte, as an absolute tick)
        ├─ channel_loops  : Vec<ChannelLoop>     (per-lane independent loops; empty for trackers)
        ├─ channel_inserts: Vec<DeviceChain>     (per-channel insert effects)   ┐ signal graph —
        ├─ buses          : Vec<Bus>             (aux / return buses)           │ all empty for
        ├─ channel_sends  : Vec<Vec<Send>>       (per-channel sends → buses)    │ every tracker
        ├─ master_chain   : DeviceChain          (master-bus inserts)           │ import (⇒ inert
        └─ assets         : Vec<Sample>          (shared audio asset pool)      ┘ ⇒ bit-identical)
```

- **`Track`** is an enum with three shapes. `Track::Notes` owns the
  per-row `Vec<Cell>` (the classic tracker representation, produced
  by every importer and by manual editing). `Track::Euclidean` is a
  Bjorklund generator — `(events, steps, rotation)` plus a single
  prototype `Cell` — and synthesises one cell per row at playback
  time (see [*Euclidean rhythm tracks*](#euclidean-rhythm-tracks)).
  `Track::Audio` is a recorded-audio clip (no note grid): a `Sample`
  played from its placement and resampled from `source_rate` to the
  output rate — the DAW generalization (see
  [*Signal graph, recorded audio & asset pool*](#signal-graph-recorded-audio--asset-pool)).
  The note variants carry a fixed `instrument` index, invariant for
  the Track's lifetime; a change of instrument starts a new Track. One
  special `instrument` value is reserved: `EFFECT_ONLY_INSTRUMENT`
  marks segments that carry only effects (no note/instrument) and
  modify a voice inherited from a previous pattern.
- **`Clip`** places a `Track` onto a `(song, target_channel)` lane at
  a given `position_tick`, with `end_tick` stored at extract time so
  half-open `active_at` lookups are exact.
- **`SortedClips`** keeps clips sorted by
  `(song, target_channel, position_tick)` and exposes
  `from_unsorted`, `lane`, `active_at`, `modify`, `insert`.
- **`TimelineMap`** flattens the `pattern_order` into absolute ticks
  and absorbs every navigation effect (`Bxx` / `Dxx` / `E6y` / `EEy`
  + restart byte) at import time — the player navigates through
  `timeline_map.entries` linearly, no row-by-row jump interpretation.
- **`AutomationLane`** carries the modulation curves: `Points` for
  typed `(tick, value)` curves (Bpm/Speed/GlobalVolume), `Lfo` for
  Vibrato/Tremolo/Panbrello, `Slide` for VolumeSlide/Portamento/
  ChannelVolumeSlide/PanningSlide/BpmSlide/GlobalVolumeSlide,
  `Glide` for TonePortamento.
- **`ChannelLoop`** is a per-lane loop region `[start_tick, end_tick)`
  on a `(song, channel)`: that lane wraps independently of the others.
  `song_loop_to` wraps the whole song in lockstep — every tracker
  format uses it and leaves `channel_loops` empty. Some replayers
  don't: David Whittaker (and most custom C64/Amiga drivers) give each
  voice its own loop, so they drift forever (e.g. `xenon2 (title).dw`'s
  four voices loop at 9888/9888/9912/10014 frames). That's exactly how
  a DAW exposes distinct per-track clip-loop lengths (Ableton/Bitwig
  polymeter). When a lane has no `ChannelLoop`, playback is the
  byte-for-byte global-wrap path.

The import pipeline (run by every `import_*` feature) is:

```text
build_timeline_map        ── linearise order + absorb navigation
  → extract_tracks_and_clips     ── split by instrument, emit clips
  → dedupe_tracks_by_content     ── fuse bit-identical tracks
  → auto_convert_strict_euclidean_tracks    ── collapse Bjorklund tracks
  → extract_per_track_lanes_from_patterns   ── Vibrato/Tremolo/Panbrello/
                                              Portamento/TonePortamento/
                                              VolumeSlide/ChannelVolumeSlide/
                                              PanningSlide → AutomationLanes
  → extract_song_lanes_from_patterns        ── Bpm/Speed/GlobalVolume + slides
```

[`Module::row_at(song, pattern, row)`] is the single cell-resolution
API. It looks up the absolute tick in `timeline_map`, picks the
active clip on each `(song, target_channel)` lane via
`SortedClips::active_at`, and returns one `(Cell, Option<usize>)` per
channel (the `Option<usize>` is the active Track's instrument index).
All reads go through the DAW layer; there is no legacy fallback. The
[`xmrs::edit`] module exposes incremental commands
(`cmd_create_clip`, `cmd_duplicate_clip`, …) for editor / DAW UIs,
with apply/undo round-trip verified by a proptest suite.

### Euclidean rhythm tracks

Euclidean rhythms are a *Track* kind, not an instrument kind:
`Track::Euclidean` stores `(events, steps, rotation, prototype)` —
a single `Cell` prototype plus the Bjorklund parameters — alongside
the same `instrument` / `muted` / `name` fields as `Track::Notes`.
When a `Track::Notes` whose cells match a canonical Bjorklund
pattern (uniform pitch, uniform velocity, no per-cell effects) is
detected at import, `auto_convert_strict_euclidean_tracks` rewrites
it in place as a `Track::Euclidean`. The row resolver expands the
pattern at playback time via the canonical
`euclidean_pattern(events, steps)` generator — bit-identical to the
original, but the on-disk Track holds parameters instead of N
explicit rows. Two optional fields (`humanize_advance_max_ticks` /
`humanize_probability`) carry pulse-level humanisation that the
player applies only when the user opts in.

## Signal graph, recorded audio & asset pool

On top of the timeline layer, `Module` carries a small **DAW
generalization**: a native signal graph, recorded-audio tracks, an
asset pool, and per-voice / per-note modulation. The governing rule is
**additive and bit-identical**: every importer leaves all of these
empty / neutral, so a loaded tracker module renders byte-for-byte
identically to a build without them. They only come alive when an
editor authors them.

- **Devices & routing.** `Device` is a native, fixed-point mixer
  insert. Beyond `Gain` and a one-pole `LowPass`, the full DirectX-DMO
  effect set has integer equivalents — `ParamEq`, `Distortion`,
  `Compressor`, `Echo`, `ModDelay` (chorus / flanger), `Reverb`
  (Freeverb / WavesReverb, with freeze and independent wet/dry),
  `I3DL2Reverb` and `Gargle` — all stateful (`process` takes
  `&mut self`) and `no_std` integer-only. A `DeviceChain` is an ordered,
  bypassable list of them; `Module::channel_inserts` puts a chain on each
  mixer channel, `buses` / `channel_sends` wire post-insert sends into
  aux/return `Bus`es, and `master_chain` processes the final summed mix.
- **Plugins → native devices.** `mix_plugins` holds the opaque,
  host-only IT/MPTM plugin chunks the engine cannot run directly. At
  import the IT loader *projects* them onto the native devices above —
  decoding the DMO parameter blobs (and two known VSTs) into a
  `DeviceChain` — so a plugin-bearing `.it` plays with its effects by
  default, while plugin-free modules stay bit-identical. This is distinct
  from IT's resonant filter, which is rendered by a dedicated per-voice
  biquad, not by this chain.
- **Automatable parameters.** `AutomationTarget::DeviceParam
  { track, device, param }` lets an automation lane drive an insert
  parameter (e.g. a filter cutoff) over time, alongside the existing
  pitch / volume / pan / channel-volume targets.
- **Recorded audio.** `Track::Audio` plays a `Sample` from its clip
  placement, linear-resampled from `source_rate`. Its waveform is an
  `AudioSource`: `Inline(Sample)` (owned) or `Pooled(index)` — a
  reference into the module-level **asset pool** `Module::assets`, so
  one recording placed many times is stored once. Resolve a track
  against the pool with `Module::track_audio`; intern a shared asset
  with `Module::push_asset`. This generalises the per-instrument
  `Arc<[..]>` PCM sharing into an index-referenced registry.
- **Per-voice & per-note modulation.** An `AutomationLane` carries a
  `ModScope`: `Track` (the tracker default — modulation reaches only
  the live voice, detached NNA ghosts keep their frozen state) or
  `Voice` (modulation reaches every voice of the track, the
  per-voice / MPE path). Each `Cell` also carries a `NoteExpression`
  (per-note pitch-bend / volume / pan), neutral for every tracker
  import.

## Loading a file

```rust
use xmrs::prelude::*;

let bytes = std::fs::read("song.xm")?;

// Auto-detect: tries XM → S3M → IT → MOD → DW.
let module = Module::load(&bytes)?;

// Or target a specific format:
let module = Module::load_xm(&bytes)?;

println!("{} — {} tracks, {} clips", module.name, module.tracks.len(), module.clips.len());
```

Format-specific constructors are also available: `Module::load_mod`,
`load_xm`, `load_s3m`, `load_it`, `load_dw`, `load_sid`. SID is settled
first and exclusively, off its `PSID`/`RSID` magic — the one format here
that says what it is. DW is tried last: its detection is purely
structural (no magic header), so running it after MOD avoids ever
stealing a valid MOD parse.

A `.sid` holding several tunes becomes **one module with several
sub-songs**, the way `.dw` files already do. The crate ships the
reverse-engineered *record* for each of the fifteen transcribed Rob
Hubbard tunes and recognises your own copy of one by fingerprint, so it
plays with the hand-tuned tempo and effect settings that no detector can
recover. The tunes themselves are not shipped — see the `sid_songs`
feature below.

## Saving — the native `.xmr` format (v1)

`Module` derives `serde::Serialize` / `Deserialize`, so you can round-trip
it through any serde codec. That is fine for a cache and wrong for an
archive: a derived repr has no version, so any refactor of the model
silently invalidates every file already written.

The `format` feature (off by default, `no_std` + `alloc`) adds the
project's own archival format instead — a chunked container with a frozen
per-schema wire layer:

```rust
use xmrs::format::document::{read, write};

let bytes = write(&module)?;              // -> a `.xmr` image
let (module, report) = read(&bytes)?;     // -> the module, plus what was skipped
assert!(report.is_clean());
```

What it buys over `serde`-on-the-model:

- **Versioned.** The header carries `schema_version` and a per-file
  `min_reader_version` computed from the content, so an old reader refuses
  a file it would misread instead of half-decoding it.
- **Forward-compatible by construction.** PNG-style case bits on each
  4-byte chunk id: an unknown *ancillary* chunk is skipped, an unknown
  *critical* one is refused. No table to keep in sync.
- **Sample data is not CBOR.** PCM lives in one raw `PCM ` region, 8-byte
  aligned and deduplicated by `Arc` identity — so shared samples stay one
  blob, and a host can `mmap` and cast in place.
- **Chip instruments round-trip in every build.** The instrument *data*
  types are never feature-gated, only the render engines, so a build
  without `synth_sid` still loads, keeps and re-saves a SID instrument —
  it just cannot play it.
- **A tolerant reader that does not destroy.** Skipping a chunk you do not
  understand is only tolerance if the writer puts it back. `read` returns a
  `LoadReport` that *carries the bytes* of every unknown chunk marked safe to
  copy; `write_preserving(&module, &report)` re-emits them verbatim. Chunks
  whose id forbids copying are listed instead, so an editor can warn before a
  save-in-place drops them — `rewrite_is_lossless()` is that question in one
  call. The report also carries a layer-consistency verdict, since no single
  chunk can tell that a clip names a track that is not there.

The full specification and the reasoning behind it are in
`src/core/FORMAT_RFC.md`.

> `.xmr` is the *project* format — editor, DAW, archive. It is
> deliberately not the embedded playback path: `Module::load` reads the
> historical tracker formats, and a flash-resident runtime image
> (`.xmo`) is sketched in §9 of the RFC, not implemented.

## Examples

Run from the `xmrs` crate directory:

```sh
# Dump any supported module file:
cargo run --features=demo --example xmrs -- -f path/to/song.xm

# Dump an XI (Fast Tracker II instrument) file:
cargo run --example xmi

# Exercise the bundled SID parsers (needs the images — repository checkout):
cargo run --features="std sid_songs" --example sid

# Inspect a David Whittaker .dw file (detection + parsed tables):
cargo run --features="std import_dw" --example inspect_dw -- path/to/song.dw
```

## Cargo features

Defaults: `["std", "import"]`.

| Feature           | Purpose                                                             |
| ----------------- | ------------------------------------------------------------------- |
| `std`             | Build against `std`. Only forwards `std` to `serde`; the crate itself does not need `std` for arithmetic — see below. |
| `format`          | The native `.xmr` project format (schema **v1**) — chunked container, frozen `wire::v1` types, raw PCM region, CBOR payloads via `ciborium`. `no_std` + `alloc`, independent of every importer. See above and `src/core/FORMAT_RFC.md`. |
| `float-helpers`   | Add `f32 ↔ Q` conversions on every fixed-point and domain type in `crate::fixed`. Intended for the editor / desktop side (level-meters, plotting, GUI numeric fields). Self-contained, uses only `core`-stable `f32` operations — no math backend pulled in. Never enable it on the embedded target. |
| `import`          | Umbrella: enables every `import_*` format importer.                 |
| `import_mod`      | Amiga ProTracker MOD.                                               |
| `import_xm`       | Fast Tracker II XM (and XI instruments).                            |
| `import_s3m`      | Scream Tracker 3 S3M. Pulls in `synth_opl` (S3M carries OPL instruments). |
| `import_it`       | Impulse Tracker IT.                                                 |
| `import_dw`       | David Whittaker custom Amiga player (`.dw`).                        |
| `import_sid`      | Commodore 64 SID. Pulls in `synth_sid` (the voice it targets) and `import_mod` (whose effect-memory model it decodes through). |
| `sid_songs`       | **Off by default.** Bundle the `.sid` images themselves and the `get_sid_*` constructors that play them without a file. The crate's own contribution is the reverse-engineered *record* per tune, which `import_sid` always compiles; the images are third-party binaries, excluded from the published tarball, so this feature builds from a repository checkout only. |
| `synth_opl`       | Yamaha OPL2/OPL3 FM voice — `generators::opl` + `core::instr_opl` + `InstrumentType::Opl`. Pulls in `oplon`. **Independent of any importer**: an editor can author or a `no_std` target can play OPL voices with no parser compiled in. |
| `synth_sid`       | MOS6581/8580 SID voice — `generators::sid` + `core::instr_{sid,robsid}` + `InstrumentType::{Sid,RobSid}`. Pulls in `sidera`. Independent of any importer (see `synth_opl`). |
| `demo`            | Pulls in `clap`, `std` and `import` for the CLI examples.           |
| `rand8` / `rand16` / `rand64` | Extra xorshift widths (`rand32` is always on).          |

### `no_std` builds

The crate compiles **fully `no_std` with no math backend at all**. Every
former `f32` site on the runtime path has been folded to integer
Q-format arithmetic — pitch / period / frequency tables, the LFO
(`waveform.rs`), the IT importer's `c5_speed_to_finetune` (binary
search via `linear_frequency_to_period`), envelopes, panning, filter,
sample interpolation. No `libm`, no `micromath`, no `num-traits`.

Importer features parse fixed-layout tracker headers through a
cursor-based helper ([`xmrs::import::bin_reader`]) — no third-party
binary-parsing crate is involved. `serde` is the only direct
runtime dependency (the IT keyboard's `[Option<_>; 120]` arrays
ride on an inline `serde_with` shim — no `serde-big-array`).

```sh
# Minimal build, no importers (pre-baked blobs only):
cargo build --no-default-features --release

# Embedded build, MOD + XM importers only:
cargo build --no-default-features --features "import_mod import_xm" --release
```

## Concert pitch (440 Hz / MIDI-aligned)

Notes resolve to **MIDI-aligned frequencies by default**: A-4 plays at
exactly 440 Hz, C-4 at 261.626 Hz, etc. This means tracker output mixes
cleanly with any 440-Hz-aligned source (MIDI sequencers, DAW samples,
commercial audio) without a detune.

Two reference constants govern this, both in `crate::fixed::tables`:

| Mode                      | Default (MIDI-aligned) | Legacy (FT2 / PT)                |
| ------------------------- | ---------------------- | -------------------------------- |
| Linear (XM/IT linear)     | `C4_FREQ_HZ = 8372`    | `C4_FREQ_HZ_LEGACY = 8363` (≈ −2 cents) |
| Amiga (MOD/S3M/IT amiga)  | `AMIGA_C0_PERIOD = 6779` | `AMIGA_C0_PERIOD_LEGACY = 6848` (≈ +17 cents) |

The legacy values are exposed for bit-numerical comparison against
reference players (OpenMPT, libxmp, schism, ft2-clone). Most users
should leave the defaults alone — that's what makes a tracker note
sound *in tune* against modern audio.

### Embedded / footprint-sensitive builds

If you're targeting tight flash budgets, *don't* ship the importers.
Parse modules on a host machine, serialise the resulting `Module`
through any serde codec (`Module` derives `Serialize` /
`Deserialize`), and load the resulting blob on-device.
[`postcard`](https://crates.io/crates/postcard) is a good `no_std`
default — it's `alloc`-only by default, varint-encoded, and produces
compact output; pair it with
[`flate2`](https://crates.io/crates/flate2) or
[`miniz_oxide`](https://crates.io/crates/miniz_oxide) if you need
further compression. You end up with a much smaller binary that
still manipulates the full `Module` model.

Chip-synthesis instruments (OPL, SID) are **not** tied to the importers:
a module carrying them plays on-device by enabling `synth_opl` /
`synth_sid` alone — no parser compiled in. Both are verified building for
a bare-metal `no_std` target (`thumbv7m-none-eabi`), with or without any
importer.

## License

MIT © Sébastien Béchet. See [LICENSE](LICENSE).