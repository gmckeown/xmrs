//! Top-level [`Instrument`] container + [`InstrumentType`] enum.
//!
//! Every [`crate::core::module::Module`] carries a vector of
//! [`Instrument`]s. A Track holds an instrument *index* (invariant
//! for the track's lifetime); the runtime resolves the actual
//! voice each time a Cell triggers.
//!
//! [`InstrumentType`] discriminates the variants the player knows
//! how to render:
//! - `Empty` — placeholder slot.
//! - `Default(InstrDefault)` — the dominant case (sampled, XM/MOD/
//!   S3M/IT).
//! - `Midi(InstrMidi)` — routed to an external MIDI synth via the
//!   player's observer chain.
//! - `Opl(InstrOpl)` *(cfg `import_s3m`)* — Yamaha OPL FM, opaque
//!   to the sample engine.
//! - `Sid(InstrSid)` / `RobSid(InstrRobSid)` *(cfg `import_sid`)* —
//!   MOS6581 / Rob-Hubbard-style C64, opaque to the sample engine.
//!
//! Euclidean rhythms are not an instrument kind — they are a Track
//! kind ([`Track::Euclidean`](crate::core::daw::track::Track::Euclidean)).

use serde::{Deserialize, Serialize};

use crate::core::instr_default::InstrDefault;
use crate::core::instr_midi::InstrMidi;

use alloc::string::String;

//===========================================================================

/// Instrument payload, discriminated by kind.
//
// `large_enum_variant` allowed: `Default` is ~2 KiB (mostly
// `sample_for_pitch: [Option<usize>; 120]`) vs ~50–100 B for the others.
// Boxing would help only when modules carry many `Empty` slots (rare),
// cost one extra allocation per instrument at load, hurt iteration
// cache locality, and break the public API at every construction /
// match site.
#[allow(clippy::large_enum_variant)]
#[derive(Default, Serialize, Deserialize, Debug)]
pub enum InstrumentType {
    /// No Instrument
    #[default]
    Empty,
    /// Canonical sample-based instrument — envelopes, vibrato,
    /// keyboard map, sample list. Used by XM / MOD / S3M / IT.
    Default(InstrDefault),
    /// Midi Instrument
    Midi(InstrMidi),
    /// Yamaha OPL (gated behind `import_s3m`).
    #[cfg(feature = "import_s3m")]
    Opl(crate::tracker::instr_opl::InstrOpl),
    /// MOS6581 SID Voice (gated behind `import_sid`).
    #[cfg(feature = "import_sid")]
    Sid(crate::tracker::instr_sid::InstrSid),
    /// Rob Hubbard Instrument (gated behind `import_sid`).
    #[cfg(feature = "import_sid")]
    RobSid(crate::tracker::instr_robsid::InstrRobSid),
}

/// One entry in [`Module::instrument`].
///
/// Pairs a name with the actual instrument payload (the
/// [`InstrumentType`] enum) plus a per-instrument mute switch.
/// Tracks reference instruments by their index into the
/// `Module.instrument` vector; the mute flag lets an editor solo
/// or silence an instrument without dropping it from the bank.
///
/// [`Module::instrument`]: crate::core::module::Module::instrument
#[derive(Default, Serialize, Deserialize, Debug)]
pub struct Instrument {
    pub name: String,
    pub instr_type: InstrumentType,
    pub muted: bool,
}
