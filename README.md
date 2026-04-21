# XMrs — SoundTracker file format library

A `no_std`-friendly Rust library to read, edit and serialize SoundTracker
data — with pleasure.

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
| SID    | Commodore 64 / MOS6581 (*WIP*)          | `import_sid`  |

The umbrella feature `import` enables all of them at once (and is on by
default together with `std`).

## The `Module` model

xmrs exposes one editor-friendly data model, regardless of what was
loaded:

```text
Module ─┬─ Instrument ─┬─ InstrDefault ─┬─ Sample (with loop & sustain-loop)
        │              │                ├─ Envelope (volume / panning / pitch)
        │              │                ├─ Vibrato
        │              │                └─ InstrMidi
        │              ├─ InstrEkn       (Euclidean-rhythm instrument)
        │              ├─ InstrMidi      (MIDI instrument)
        │              ├─ InstrOpl       (Yamaha OPL)
        │              ├─ InstrSid       (MOS6581 SID voice)
        │              └─ InstrRobSid ── InstrSid  (Rob Hubbard-style)
        └─ Pattern ─ Row ─ TrackUnit ─┬─ TrackEffect
                                      └─ GlobalEffect
```

Two companion fields on `Module` describe where the data came from and
how it should play back:

- **`ModuleFormat`** — purely metadata (`Xm`, `S3m`, `It`, `Mod`,
  `Unknown`). Useful for the UI and for export.
- **`PlaybackQuirks`** — orthogonal switches for each historical replay
  quirk (FT2 pitch-slide overflow, S3M period clamp, arpeggio LUT,
  pattern-loop resume, tremor state, …). Importers populate them to
  match the source format; modules authored from scratch default to a
  clean, quirk-free playback.

This separation lets an editor compose any subset of historical
behaviours without having to lie about which tracker the module "came
from".

## Loading a file

```rust
use xmrs::prelude::*;

let bytes = std::fs::read("song.xm")?;

// Auto-detect: tries XM → S3M → IT → MOD.
let module = Module::load(&bytes)?;

// Or target a specific format:
let module = Module::load_xm(&bytes)?;

println!("{} — {} patterns", module.name, module.pattern.len());
```

Format-specific constructors are also available: `Module::load_mod`,
`load_xm`, `load_s3m`, `load_it`. SID is imported via its own entry
point, `xmrs::import::sid::sid_module::SidModule::load`, and is not part
of the auto-detect path.

## Serialization

`Module` derives `serde::Serialize` / `Deserialize`, so you can round-trip
it through any serde codec.

## Examples

Run from the `xmrs` crate directory:

```sh
# Dump any supported module file:
cargo run --features=demo --example xmrs -- -f path/to/song.xm

# Dump an XI (Fast Tracker II instrument) file:
cargo run --example xmi

# Exercise the bundled SID parsers (Rob Hubbard tunes):
cargo run --example sid
```

## Cargo features

Defaults: `["std", "import"]`.

| Feature        | Purpose                                                     |
| -------------- | ----------------------------------------------------------- |
| `std`          | Build against `std`; uses `f32`'s inherent math methods.    |
| `import`       | Umbrella: enables every `import_*` format importer.         |
| `import_mod`   | Amiga ProTracker MOD.                                       |
| `import_xm`    | Fast Tracker II XM (and XI instruments).                    |
| `import_s3m`   | Scream Tracker 3 S3M.                                       |
| `import_it`    | Impulse Tracker IT.                                         |
| `import_sid`   | Commodore 64 SID (bundled Rob Hubbard tunes).               |
| `libm`         | Float math via [`libm`](https://crates.io/crates/libm) (pick one in `no_std`).           |
| `micromath`    | Float math via [`micromath`](https://crates.io/crates/micromath) (pick one in `no_std`). |
| `demo`         | Pulls in `clap`, `std` and `import` for the CLI examples.   |
| `rand8` / `rand16` / `rand64` | Extra xorshift widths (`rand32` is always on). |

### `no_std` builds

With `std` disabled, exactly one math backend is required — the crate
refuses to compile otherwise:

```sh
# libm backend:
cargo build --no-default-features --features "libm import" --release

# micromath backend (smaller, less precise):
cargo build --no-default-features --features "micromath import" --release
```

### Embedded / footprint-sensitive builds

If you're targeting tight flash budgets, *don't* ship the importers.
Parse modules on a host machine, serialize with
[`bincode`](https://crates.io/crates/bincode) 2.x (`alloc` feature) —
optionally compressed with
[`flate2`](https://crates.io/crates/flate2) — and load the resulting
blob on-device. You end up with a much smaller binary that still
manipulates the full `Module` model.

## License

MIT © Sébastien Béchet. See [LICENSE](LICENSE).