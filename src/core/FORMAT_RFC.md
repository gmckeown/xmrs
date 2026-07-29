# Native file format (`.xmr`) — specification & version policy

**Status: proposal.** Nothing here is implemented. This document specifies
the native on-disk format for `Module` and, more importantly, the method by
which it is allowed to evolve. Proposed implementation site: a new
`src/format/` module (`crate::format`), which is where the reader/writer,
the frozen wire types and the migration chain would live. `core` stays the
in-memory model and gains no knowledge of the encoding.

Unlike the other RFCs in this tree, this one is written *before* the code,
because the whole point of a file format is the promise it makes about
files that already exist. That promise cannot be retrofitted.

---

## 0. The problem

`xmrs` has no file format. It has `#[derive(Serialize)]` on `Module` and a
README paragraph recommending [`postcard`]. The consequence is that the
derived serde representation of the live model **is** the de-facto format,
without anything saying so. Three concrete symptoms, all present today:

1. **The model is the schema.** Field order in `Module`, variant indices in
   `Track` / `TrackEffect` / `Device` / `CellEvent` are all load-bearing
   file content. Every refactor this crate does routinely — the DAW
   generalization moving Vibrato/Tremolo out of `Cell::effects` into
   `Module::automation`, `InstrDefault` splitting into `VoiceSetup` /
   `InstrumentBehavior` / `Keyboard` — silently invalidated every file
   previously written. Under a positional codec, an inserted field does not
   error; it shifts everything after it and decodes into plausible garbage.

2. **The compat annotations are inert on the recommended path.** There are
   ~15 `#[serde(default)]` in the model (`module.rs`, `cell.rs:244`,
   `track.rs:138`, `automation.rs:388`, …) carrying comments like *"keeps
   older serialised modules readable"*. The intent is right. But
   `#[serde(default)]` rescues a **missing named field**, which only exists
   in a self-describing codec. postcard is positional: there is no missing
   field, only a cursor. Those annotations protect nothing on the path the
   README recommends.

3. **The wire depends on Cargo features.** `Module::origin` is `#[cfg]`-gated
   on the `import_*` features and `mix_plugins` on `import_it`. So the
   serialized layout of `Module` differs between two builds of the same
   version of the same crate. A file written with `import_it` on decodes
   wrong — silently — in a build with it off. This alone disqualifies
   deriving the format from the live model. It once ran deeper than field
   order: import features used to gate *synthesis* too, so whole
   `InstrumentType` variants appeared and vanished with the feature set.
   §5.2 has since fixed that half (synthesis now gates on `synth_*`,
   independent of any importer); `origin` / `mix_plugins` remain `#[cfg]`
   -gated, which is exactly why §5.1 forbids cfg-gated *wire* types outright.

None of this is serde's fault. serde is a framework, not a format; postcard
*is* serde. The fault is (a) a positional codec and (b) the derive sitting
on the live types. Both are fixable while keeping serde.

## 1. Scope — two artifacts, not one

`Module` serves two consumers with almost nothing in common. Conflating them
is what would condemn both.

| | `.xmr` — **project** | `.xmo` — **runtime image** |
|---|---|---|
| Consumer | editor / DAW / archive | `no_std` player, flash-resident |
| Lifetime | forever | rebuilt at will, disposable |
| Priority | extensibility, compat, incremental save | size, alignment, zero-alloc load |
| Compat duty | full backward, best-effort forward | **none** |
| Encoding | chunked container + CBOR | `zerocopy`/`bytemuck` POD cast |

The relationship is source → object code. The README already gestures at
this ("parse on a host, serialise, load the blob on-device") but postcard
forces the device to rebuild the entire `Module` in RAM, which is the
opposite of what an MCU wants from flash-resident data.

**This RFC specifies `.xmr` only.** `.xmo` is sketched in §9 and is
deliberately unconstrained: because it is a build artifact with no compat
duty, it is free to be optimal, and it must never be asked to double as an
archive.

## 2. Container

The container is **bespoke and open** — not PNG, not IFF, not RIFF (§10 records
why). It takes IFF's chunk *model* and PNG's compat-bit *policy* (§2.1), but
its bytes are its own: little-endian, 8-byte aligned, with a versioned header.
"Open" is the axis that matters here — the grammar is fully specified in this
document and the payloads are a published standard (CBOR, §5) — and it is
orthogonal to "bespoke": the format borrows no container standard, and needs
none, because the standardness that carries a file forward 30 years lives in
the payload codec, not the frame.

All integers little-endian. Header is 16 bytes:

| off | size | field | |
|---|---|---|---|
| 0 | 4 | `magic` | `"XMRS"` (`0x58 0x4D 0x52 0x53`) |
| 4 | 2 | `container_version` | grammar of §2 itself. `1`. Bumps ~never. |
| 6 | 2 | `flags` | reserved, MUST be 0 |
| 8 | 4 | `schema_version` | what this file *is* (§6) |
| 12 | 4 | `min_reader_version` | oldest reader that can read it *correctly* (§6) |

Then a sequence of chunks to EOF. Each chunk:

| off | size | field |
|---|---|---|
| 0 | 4 | `id` — 4 ASCII bytes |
| 4 | 4 | `len` — payload length, excluding this header and padding |
| 8 | `len` | payload |
| | 0–7 | zero padding to the next 8-byte boundary |

Chunks are 8-byte aligned, which covers every alignment the model needs
(`i8`/`i16`/`f32` PCM). Order is not significant except that `PCM ` SHOULD
come last so a writer can stream structure before bulk. Duplicate `id` is an
error.

### 2.1 Chunk id case bits

Straight from PNG, for the same reasons:

- **byte 0 case — critical vs ancillary.** Uppercase: a reader that does not
  know this id MUST refuse the file. Lowercase: it MUST skip it.
- **byte 3 case — safe-to-copy.** Uppercase: a reader that does not
  understand this chunk MUST NOT copy it into a rewritten file (it may
  depend on data that changed). Lowercase: it MAY copy it verbatim.

This gives the entire forward-compat policy as two bits, with no
compatibility matrix to maintain.

### 2.2 The critical/ancillary line

The line is not a taste question. It is:

> **A chunk is ancillary if and only if ignoring it cannot change the
> rendered audio.**

For anything that affects the render, silently skipping it means playing
something other than what the author wrote — which is strictly worse than
refusing to open the file. Degraded-but-honest reading is only defensible
for data with no audible consequence: names, comments, UI hints, edit
history, writer provenance.

## 3. Chunk catalogue

Mapping the fields of `core::module::Module`. Absent chunk = the model's
`Default` for its fields; the writer omits any chunk that would encode only
defaults (so a tracker import writes no `GRAF`, no `ASET`, no `PCM ` beyond
its instrument samples).

| id | crit | payload |
|---|---|---|
| `MHDR` | ● | `name`, `comment`, `frequency_type`, `default_tempo`, `default_bpm`, `pattern_highlight`, `pitch_wheel_depth`, `mix_volume`, `channel_names`, `channel_defaults` |
| `QRKS` | ● | `PlaybackQuirks` — the only field playback reads for tracker bug emulation |
| `INST` | ● | `(Vec<Instrument>, Vec<InstSampleRef>)` — instrument *headers* (sample PCM stripped to `None`) + refs `(instrument, sample, kind, offset, len)` into `PCM ` |
| `TRKS` | ● | `(Vec<Track>, Vec<TrackSampleRef>)` — tracks (`Notes` / `Euclidean` / `Audio`); refs cover `Track::Audio` inline PCM |
| `CLIP` | ● | `SortedClips` |
| `AUTO` | ● | `Vec<AutomationLane>` |
| `TMAP` | ● | `TimelineMap` |
| `LOOP` | ● | `song_loop_to`, `channel_loops` |
| `GRAF` | ● | `channel_inserts`, `buses`, `channel_sends`, `master_chain` |
| `MIDI` | ● | `midi_macros` |
| `ASET` | ● | `(Vec<Sample>, Vec<AssetSampleRef>)` — asset-pool *headers* (PCM stripped) + refs into `PCM ` |
| `PCM ` | ● | the blob region (§4) |
| `orgn` | ○ | `origin` — source-format provenance (§3.1), safe-to-copy |
| `genr` | ○ | writer provenance — crate name + semver (§6); regenerated on every write, never gates compat |
| `hstc` | ○ | edit history (§8), safe-to-copy |

`mix_plugins` has **no chunk**: it is dropped from the format (§3.1). The
model field stays (the live IT importer still populates and *projects* it
onto `GRAF`), but it is never written to `.xmr`.

### 3.1 The import boundary

The import pipeline is a compiler front-end: MOD / XM / S3M / IT / DW →
`Module`. **The front-end does not ship in the object file — but everything
it *decided* does.** One-shot is the *act* of interpretation, not its
result. That distinction, not "import vs. the rest", is what places a field.

| | | |
|---|---|---|
| `bin_reader`, `TrackImportUnit`, `extract/`, `build/`, `profiles::*` | the act | never in the file — already unreachable from `Module` ✓ |
| `quirks` | **result** | **critical.** Looks like import residue, is not. |
| `InstrSid` / `InstrOpl` / `InstrRobSid` | **authoring primitive** | **critical** — synthesis, not import (§5.2) |
| `origin` | provenance | **ancillary** (`orgn`) |
| `mix_plugins` | un-interpreted leftover | **dropped** — not archived |

`quirks` is the case worth stating explicitly, because every instinct
misfiles it. FT2 pitch-slide overflow, S3M period clamp, arpeggio LUT,
pattern-loop resume — it reads as pure importer residue. It is the opposite:
per the model's own contract it is *the only field playback reads*. The
importer picking `profiles::ft2()` is the one-shot act; the resulting
`PlaybackQuirks` is load-bearing forever. Drop it and the module renders
something other than what was imported.

`origin` earns `orgn` (ancillary) rather than a seat in `MHDR`: nothing
reads it to make audio. `xmrsplayer.rs:118` states it outright — *"metadata
only (display, export, …)"* — and the only live reads are a `tprintln!`
(`cpal_player.rs:502`) and a load-time guard (`lib.rs:273`). Under the §2.2
rule it cannot be critical.

`mix_plugins` is **dropped from the format** (decided — `.it` re-export is
not a goal). The reasoning: the IT importer already *projects* the plugin
table onto native `Device`s at load time, so **all audible content lives in
`GRAF`** and playback is unaffected by the drop. What the raw blobs hold that
the projection does not is threefold — (a) *unknown VSTs*, which the importer
leaves unconverted and which xmrs cannot render either way (inert with or
without the blob); (b) *plugin identity* (`name` / `library_name`), lost when
a plugin becomes an anonymous native `Device`; (c) the *exact DMO parameter
blobs*, since the DMO→`Device` projection is one-way and approximate at the
edges. None of that is a sample of audio xmrs plays. Preserving it would only
serve faithful `.it` re-export or a future xmrs that learns to run
today's-inert VSTs — neither a goal — so the format carries only the
projected `GRAF` devices. This was the one product call in the catalogue, and
it is now made.

## 4. The PCM region

PCM is ~99% of the bytes and ~1% of the semantics. It is separated from the
structure for four distinct wins: `mmap`/zero-copy on host, partial and
streamed load on device, cheap incremental save (an unmodified sample is not
rewritten), and diffs that mean something.

`PCM ` payload = concatenated blobs, each padded to an 8-byte boundary. A
sample's PCM lives here, not in its structural chunk; the chunk (`INST` /
`TRKS` / `ASET`) carries the sample with `data = None` plus a ref
`(kind, offset, len)`, where `kind` is `Mono8` / `Mono16` / `Stereo8` /
`Stereo16` / `StereoFloat` (mirroring `SampleDataType`) and `(offset, len)`
are relative to the start of the `PCM ` payload. Blobs are raw little-endian
samples — no per-blob encoding, no compression. On a device, small and
aligned loses to *castable*; whole-file `zstd` remains available to whoever
wants small.

**Dedup by `Arc` identity.** The writer interns blobs by their `Arc<[T]>`
data-buffer address, so the sharing the import-time dedup produced is stored
once rather than expanded to N copies (serde does not reconstruct shared
identity — the model's own `sample.rs` comment notes this). On read the
resolver re-shares by offset, so the sharing is *restored* as one allocation,
not just deduplicated on disk. This captures the realistic case (import
sharing) at zero cost; **content** dedup across distinct-but-equal `Arc`s, and
a per-blob BLAKE3-128 in `ASET` to enable incremental save / a loose-object
store, are future refinements — neither is stored today.

**`StereoFloat` — decided: raw `f32` bits in `PCM `.** The apparent wart
(`SampleDataType::StereoFloat(Arc<[f32]>)` being the one `f32` in an
otherwise integer-only crate) is smaller than it looks, because `PCM ` is a
**raw byte region that never passes through serde**. A `StereoFloat` blob is
stored as its IEEE-754 little-endian bytes verbatim — round-trip is
bit-exact, and the NaN/subnormal concern evaporates because we store bits,
not interpreted values. The integer-only ethos matters for the *structural*
CBOR payloads, and it holds there: `StereoFloat` is the sole `f32` in the
model and it lives entirely in `PCM `, so no `f32` ever reaches a structural
chunk. No Q-format conversion (it would be lossy for out-of-`[-1,1]` recorded
audio); `f32` LE is universal on every target Rust supports.

## 5. Payload encoding

Each chunk payload is **CBOR** (`ciborium`), i.e. still serde — self-describing
and length-delimited, which is precisely what postcard is not. Named fields
make `#[serde(default)]` mean what its comments already claim, and
`#[serde(alias)]` buys renames for free.

CBOR's text keys cost bytes; that was the argument for `minicbor`'s integer
indices. It is rejected: 99% of the bytes are PCM, which never goes through
serde at all, so the compaction would apply to ~1% of the file at the price
of a second derive ecosystem alongside the serde one already in use
everywhere. Revisit only with a measurement.

### 5.1 Wire types are not model types

`crate::format::wire::vN` holds plain structs mirroring the model, deriving
`Serialize`/`Deserialize`, with explicit conversion to/from `core`. This is
the decoupling the whole design rests on — it is what lets `core::Module`
keep being refactored freely, which this crate does constantly.

Non-negotiable rules for wire types:

- **Never `#[cfg]`-gated.** This is the fix for §0.3: the file format cannot
  depend on which Cargo features the writer was built with. `Module::origin`
  is `#[cfg]`-gated on `import_*` in the *model*, but the wire's `orgn`
  provenance chunk is not: a build with no importer still reads and
  round-trips it. The wire mirrors the union of what the model can hold,
  regardless of the writer's feature set.
  - **The cleaner fix, where the model allows it: don't gate the variant at
    all.** `InstrumentType`'s `Opl` / `Sid` / `RobSid` were `#[cfg]`-gated on
    `synth_*`, which would have made `INST` feature-dependent. Rather than
    mirror the enum in the wire to paper over that, the *model* was changed:
    the synth **data types** (`core::instr_{opl,sid,robsid}`) are
    integer-only with no emulator dependency, so they are now compiled in
    every build; only the render **engines** (`generators::{opl,sid}`) gate on
    `synth_*`. `InstrumentType`'s variant set is therefore stable, `INST`
    rides the model derive with no cfg problem, and a `.xmr` round-trips a
    SID/OPL instrument even in a build that cannot synthesise it. Prefer this
    (stabilise the model) over a wire mirror whenever the gated thing is data,
    not a heavyweight dependency.
- **No `Arc`, no borrows, no `usize`.** Explicit widths (`u32`, not `usize`
  — the file must not differ across 32/64-bit hosts; `default_tempo` /
  `default_bpm` are `usize` in the model and become `u32` on the wire).
- **Frozen on release.** Once a `vN` module ships, it is never edited again.
  §7 makes that mechanical rather than aspirational.

### 5.2 Prerequisite: synthesis is an authoring primitive, not import

**Status: delivered in both `xmrs` and `xmrsplayer`.**

§5.1's "never `#[cfg]`-gated" rule was not implementable against the
original feature graph, and the reason was not a packaging detail — it was
the same confusion this format exists to undo.

Before:

```
import_s3m = ["dep:oplon"]     # generators::opl + InstrumentType::Opl gated on import_s3m
import_sid = ["dep:sidera"]    # generators::sid + InstrumentType::{Sid,RobSid} on import_sid
```

`InstrumentType::Opl` was gated on `import_s3m`; `Sid` / `RobSid` on
`import_sid`. So **playing** — or **authoring** — a module containing a SID
instrument required enabling an **import** feature, though no `.sid` is ever
parsed.

The deeper tell was in the tree, not the feature graph. `instr_opl.rs`,
`instr_sid.rs`, `instr_robsid.rs` import **nothing** from `tracker::import`
— zero coupling — yet they lived under `tracker/`, whose `mod.rs` declares
itself *"Format-tied items"*. Filed under "tied to a format" while being
tied to no format. A SID or OPL voice is not import residue: it is a
**creation primitive**, exactly like a sample or an envelope — first-class
in the model as `InstrDefault` is.

Three consequences, in increasing depth:

1. **The variant set of `InstrumentType` depended on Cargo features.** Worse
   than §0.3's field-order problem — entire variants vanished from the enum,
   so the same wire bytes were un-decodable rather than mis-decoded.
2. **It broke the README's own embedded promise** ("don't ship the
   importers; parse on a host, serialise, load the blob on-device") for
   exactly the modules best suited to it. Chip synthesis is tiny — the ideal
   MCU payload — and it was the one payload you could not have without
   dragging in an importer you would never call.
3. **The instrument types were misplaced** under `tracker/` (format I/O)
   despite being format-agnostic data.

The fix, now applied. The three types moved `tracker/ → core/` (beside
`instr_default` / `instr_midi`), and standalone synthesis features carry the
dependency arrow one way only:

```
synth_opl  = ["dep:oplon"]     # gates generators::opl + core::instr_opl + InstrumentType::Opl
synth_sid  = ["dep:sidera"]    # gates generators::sid + core::instr_{sid,robsid} + …::{Sid,RobSid}
import_s3m = ["synth_opl"]     # an importer needs the synth it targets…
import_sid = ["synth_sid"]     # …a synth never needs a parser.
```

Verified: `--no-default-features --features "std,synth_opl,synth_sid"` now
compiles — chip synthesis with zero importers, the case the old graph made
impossible — and it is both the honest embedded build and the minimal
authoring build. The default, bare `no_std`, single-importer,
`--all-features`, and `xmrsplayer` builds are unchanged (tests green),
because `import_s3m ⇒ synth_opl` keeps every existing `import_*` gate
satisfied.

**`xmrsplayer` (also done).** The player replicated the same confusion — its
FM/SID *playback* paths (`voices.rs`, `channel/opl.rs`, `channel/sid.rs`, the
OPL/SID tick code) were gated on `import_s3m` / `import_sid`. Those sites now
gate on the player's own `synth_opl` / `synth_sid` features (forwarding to
`xmrs/synth_*`), while the genuinely import-side sites stay on `import_sid`.
(`cpal_player.rs` no longer has any: `.sid` files go through plain
`Module::load` like every other format, and the bundled-tune registry the
`--sid` switch fronted is gone — the images now sit behind `sid_songs`.) `default` carries both synth features explicitly, and each
importer pulls the synth it targets (`import_s3m = ["xmrs/import_s3m",
"synth_opl"]`) so enabling an importer never silently drops the playback of
what it imports. Verified: `--no-default-features --features
"std,synth_opl,synth_sid"` plays chip instruments with no importer; the bare,
default, single-importer, `demo`, and full test builds are green.

This was a prerequisite, not a follow-up, for the *format*: freezing
`wire::v1` against a feature-dependent enum would have frozen the bug into
the file format's first version, where §6.4 makes it permanent.

## 6. Versioning

One version number forces a binary choice ("I know it or I refuse"). Impulse
Tracker got the shape right in 1995: `it_header.rs:64-67` parses **both**
`created_with_tracker` (`cwtv`, provenance — *who wrote it*) and
`compatible_with_tracker` (`cmwt`, a compat floor — *who can read it*). The
crate imports the mechanism and uses neither. This format uses both halves.

The **compat pair** — the `cmwt` half, the only fields a reader consults to
decide what it can read:

- **`schema_version`** — what this file is. Monotonic. The migration clock.
- **`min_reader_version`** — the oldest `schema_version` a reader must
  implement to interpret this file correctly.

The **provenance** — the `cwtv` half: the writing crate's name + semver,
stamped in the ancillary `genr` chunk (§3), regenerated on every write. It
records *which xmrs wrote the file* and is **never read to make a compat
decision** (§6.2). This is the answer to "should the format follow the
crate's semver": the crate's version rides along as provenance, never as the
gate.

These are the format's **own** clocks, and they already carry semver's
meaning without borrowing its dotted triple:

| semver | this format |
|---|---|
| MAJOR (old readers locked out) | `min_reader_version` rises |
| MINOR (old readers still read) | additive `schema_version` bump |
| PATCH (no wire change) | nothing on disk moves |

`container_version` (§2) is a third, grammar-level version — the byte layout
of §2 itself — and bumps ~never; it is orthogonal to both of the above.

### 6.1 `min_reader_version` is computed per file, from content

This is the load-bearing rule, and the thing that makes the whole policy
work.

`min_reader_version` is **not** the writer's version. At save time the
writer computes it as the maximum, over everything the file actually
contains, of the `schema_version` that introduced that feature. A writer at
v9 emitting a plain XM import with no post-v3 feature in it writes
`min_reader_version = 3`, and a v3 reader opens it.

The consequence is the one that matters: **adding a variant to `TrackEffect`
or `Device` costs nothing for the files that do not use it.** Growth stops
being a compatibility event and becomes a per-file property. This requires
the writer to carry an introduced-in table — one entry per wire feature.
That table is the price, and it is worth it.

### 6.2 What bumps what

> **Bump `schema_version` for what `#[serde(default)]` cannot express.**

- **Free, no bump.** New ancillary chunk. New field with a default. New enum
  variant. The chunks and the defaults absorb these; that is their job.
- **Bump `schema_version` + migration.** Data moves (the DAW generalization:
  Vibrato/Tremolo from `Cell::effects` to `Module::automation` — the data
  existed, it changed place and shape; no default rescues that). Units change
  (Q1.15 → Q8.24). The meaning of an existing field changes.
- **Raise `min_reader_version` for a given file.** That file uses a feature
  introduced at version N (§6.1).

`schema_version` is **not** the crate version. Different clocks: 0.14 → 0.15
must not touch the format, or the migration chain fills with no-ops and the
discipline dies of boredom. The crate's semver rides along only as provenance
(`genr`, §3/§6) — a file records which xmrs wrote it, but a reader decides
what it can read from `schema_version` / `min_reader_version` alone.

### 6.3 Why file-level and not per-chunk

Per-chunk versions are tempting — `INST` and `TMAP` evolve at different
rates. Rejected: the refactors that actually need migrating are
*cross-chunk*. The DAW generalization touched what would be `TRKS` and
`AUTO` together; a migration must see the whole document. So:
`schema_version` is the migration unit, chunks are the additivity mechanism
within a version. Two roles, deliberately not merged.

### 6.4 The migration chain

Frozen `wire::vN` types, and pure `fn(vN::Doc) -> vN1::Doc` steps chained.
Reading a v1 file with a v7 reader runs six pure functions, each small,
individually testable, and never touched again.

Every project fails at the same spot: after three versions, duplicating
`Module` wholesale becomes unbearable, someone "just tweaks" `wire::v1`, and
the past breaks with nobody noticing for months. The trick that makes
freezing affordable:

> **A version redefines only what changed and re-exports the rest.**

```text
mod v3 {
    pub use super::v2::{Instruments, Timeline, Quirks};  // unchanged: aliased
    pub struct Tracks { /* … */ }                        // changed: redefined
}
```

Since a refactor almost never touches more than one or two chunks, the real
cost per version is tens of lines, not a copy of the model. That is the
difference between a discipline that holds and a good intention.

### 6.5 Layering: unknown variants are unreachable

Because the ancillary line (§2.2) puts every rendering-affecting structure in
a critical chunk, and because `min_reader_version` is content-derived (§6.1),
a reader never meets an enum variant it does not know: a file containing one
would have a `min_reader_version` above it and be refused at the header, 16
bytes in.

This is why **no `Unknown { tag, payload }` catch-all is specified** for
`TrackEffect`, `Device` or `CellEvent`. An earlier draft of this design
called for one. It is dropped, for two reasons. It does not work with serde
derive anyway — `#[serde(other)]` only covers unit variants, so an open enum
would need manual nested length-delimited encoding, costing an extra
allocation and encode pass *per cell*, on the bulk of the file. And it is
not needed: unknown-preservation belongs at the chunk level, where the
granularity is coarse and the traffic is nil.

## 7. Enforcement

The section that gets skipped and that decides whether any of the above is
real.

The checks split by whether they pin an on-disk artifact. The ones that
**regenerate** each run are safe while the model is still settling and are
live now; the ones that **freeze bytes** must wait until v1 is frozen, because
model churn (a pending `RobSid` revision, say) would otherwise break a golden
file the moment it changed. Freezing the corpus prematurely would protect a
baseline we do not yet have.

**Live now (freeze-independent):**

- **Round-trip proptest.** *(Delivered — `edit_proptest.rs`,
  `xmrs_format_round_trip_is_stable`.)* A module built by a random
  `EditCommand` sequence goes `write → read`, its structure is compared by
  fingerprint, and the image is asserted **byte-stable** (`write` of the
  re-read module reproduces the bytes) — so nothing written can be silently
  dropped or mis-decoded. Regenerates every case; pins nothing.

**Deferred to the freeze:**

- **Golden corpus, versioned in-tree.** One reference file per
  `schema_version`, with its expected decode. Never regenerated — only added
  to. CI decodes the entire history with `HEAD` on every commit. This is the
  only thing that makes "just tweak `wire::v1`" impossible: the v1 test goes
  red immediately. A note in `CONTRIBUTING` will not survive five years; a
  failing test will. The first entry *is* the v1 file, so this lands with the
  freeze, not before.
- **`min_reader_version` is correct by construction.** Property: for every
  corpus file, a reader built with only the schema up to `min_reader_version`
  decodes it without error — keeping the introduced-in table (§6.1) honest.
  Vacuous at v1 (every feature is v1, `min_reader` is always 1); meaningful
  once a second version exists.
- **Totality of the chain.** A test asserting every version *n* < current has
  a migration to *n+1*. Nothing to assert until there are two versions.
- **Feature-matrix decode.** Decode a fixture in a `--no-default-features`
  build and an `--all-features` build and assert identical results — the
  regression test for §0.3. A CI-level check (two build configs), seeded from
  a frozen fixture, so it lands with the corpus.

## 8. Degradation must be visible *(delivered)*

The real danger of a tolerant reader is not reading — it is **saving**. A
reader skips two ancillary chunks it does not know, the user saves, and the
data is gone with no notice ever given.

So: loading returns a **report** alongside the `Module` — which chunks were
skipped, which were retained opaque. The editor surfaces it and refuses a
silent save-in-place. And rewriting MUST re-emit retained opaque chunks
byte-for-byte, subject to the safe-to-copy bit (§2.1). Without both, being
tolerant is just destroying politely.

Implemented as `LoadReport { retained, dropped, inconsistency }`:

- `retained` holds the **payload** of each unknown ancillary chunk whose id is
  safe to copy; `document::write_preserving(module, &report)` re-emits them
  verbatim. Plain `write` does not — the negligent path stays available, it is
  just no longer the only one.
- `dropped` holds the **id only** of unknown ancillary chunks that are not safe
  to copy. Keeping their bytes would be worse than losing them, so the loss is
  recorded instead of hidden.
- Two questions, two predicates, because they have different answers: `is_clean`
  (did this build understand the file in full?) and `rewrite_is_lossless` (will
  saving lose anything?). A retained chunk makes the first false and the second
  true. An editor about to save in place asks the second.
- `inconsistency` carries `verify_layers_consistent`, run once the whole module
  exists: no single chunk can see that a `Clip` names a track that is not there.

Note the asymmetry that makes this work at all: the report is produced at
**open** time and the destruction happens at **save** time, so the bytes have to
be carried in the report itself. A warning nobody can consult at the moment it
matters protects nothing.

`hstc` (ancillary, safe-to-copy) can carry the `EditCommand` log: the
commands are already serde-able and the apply/undo round-trip is already
proptested, so cross-session undo is nearly free. Ancillary only — a journal
as the primary representation costs a replay on every load and grows without
bound. Pleasing historical rhyme: IT had an edit-history chunk too, and this
crate already parses it (`it_edit_history.rs`).

## 9. `.xmo` — the runtime image (sketch, out of scope)

Named on the source → object metaphor of §1, not on a `z` suffix: the image is
not compressed and will not be. Should a payload ever need shrinking, that is
zip's job, on top, and none of this format's business.

Generated from `.xmr` by a host tool. Pre-resolved indices, string table
optional (or dropped entirely), naturally aligned POD structs, read by
pointer cast from flash via `zerocopy`/`bytemuck` — zero allocation, no
`Module` reconstruction in RAM. LE, integer-only, matching the fixed-point
ethos the crate already holds everywhere.

serde is the wrong tool here and that is fine: it must visit and construct
values, and while it can borrow `&[u8]`/`&str` from the input, it will never
hand back an aligned `&[i16]` pointing into flash — alignment and endianness
are not in its data model. No compat duty, no versioning: rebuild it.

## 10. Rejected

- **Deriving the format from the live model.** §0. The reason this RFC exists.
- **postcard as the archival codec.** Positional, non-self-describing. Fine
  for a build artifact (`.xmo` is better still), disqualified for an archive.
- **`minicbor`.** §5. Compacts ~1% of the file, costs a second derive
  ecosystem. Revisit with a measurement.
- **Per-chunk versioning.** §6.3.
- **`Unknown` catch-all in rendering enums.** §6.5.
- **Zip container.** Ubiquitous tooling, and `.dawproject` precedent — but
  `no_std`-hostile, and the alignment guarantee §4 rests on is not
  expressible. `.dawproject` stays an export target, not the native format.
- **IFF / RIFF as the container.** The obvious "don't reinvent the wheel"
  candidates, weighed and declined. IFF proper is big-endian — against the
  crate's LE / integer / zero-copy ethos. RIFF (LE IFF) is closer and is the
  domain's lingua franca (WAV / AIFF / DLS / SF2), but it carries no version
  fields (`schema` / `min_reader` need a bespoke chunk regardless), no
  case-bit compat policy (§2.1 is a PNG idea RIFF lacks — grafting it into the
  4CC is non-standard and no RIFF tool honours it), and pads to 2 bytes, not 8
  (the same alignment objection as Zip, §4). The clincher: payloads are CBOR,
  so a generic RIFF tool could enumerate chunk ids but read no value inside —
  the "standard tooling" benefit stops at the chunk boundary. The container we
  keep takes IFF's chunk *model* and PNG's compat-bit *policy* and drops the
  parts that fight the requirements; the archival standardness that matters
  lives in the payload codec (CBOR, RFC 8949), not the frame.
- **Version sniffing** (try each decoder until one parses). Heuristics for a
  problem that a `u32` in a header solves exactly.
- **Journal-as-format.** §8.

## 11. Decisions (formerly open questions)

All five resolved before implementation.

1. **`StereoFloat` → raw `f32` bits in `PCM `** (§4). Not Q-format: the PCM
   region never passes through serde, so raw IEEE-754 LE round-trips
   bit-exact and the integer-only ethos still holds for every structural
   chunk (no `f32` reaches CBOR).
2. **Introduced-in table → hand-maintained, backstopped by §7.** No
   proc-macro. The "`min_reader_version` correct by construction" property
   test (§7) catches any omission, *provided the golden corpus exercises
   every feature* — that corpus discipline is the real obligation, not codegen.
3. **Text projection (RON) → an export tool, not the format.** `.xmr` is
   binary; a `dump --ron` convenience carries no compat duty and does not
   burden v1.
4. **Loose-object PCM store → deferred.** The optional BLAKE3-128 in `ASET`
   (§4) is the hook that keeps it possible later; the single-file format
   ships first, the object store waits for an editor generating large projects.
5. **Lossless `.it` re-export → not a goal; `plgc` dropped** (§3.1).
   `mix_plugins` leaves the wire entirely. Playback is unaffected (all
   audible content is projected to `GRAF`); what is not archived is unknown
   VSTs (inert regardless), plugin identity, and the exact DMO blobs.

[`postcard`]: https://crates.io/crates/postcard
