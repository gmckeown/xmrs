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
//! - `Euclidean(InstrEkn)` — wraps another instrument + a Bjorklund
//!   pattern (synthetic rhythm).
//! - `Midi(InstrMidi)` — routed to an external MIDI synth via the
//!   player's observer chain.
//! - `Opl(InstrOpl)` *(cfg `import_s3m`)* — Yamaha OPL FM, opaque
//!   to the sample engine.
//! - `Sid(InstrSid)` / `RobSid(InstrRobSid)` *(cfg `import_sid`)* —
//!   MOS6581 / Rob-Hubbard-style C64, opaque to the sample engine.

use serde::{Deserialize, Serialize};

use crate::core::instr_default::InstrDefault;
use crate::core::instr_ekn::InstrEkn;
use crate::core::instr_midi::InstrMidi;

use alloc::string::String;

//===========================================================================

/// Instrument Type
///
/// clippy signals a large size difference between `Default` (~2 KiB, mostly
/// `InstrDefault::sample_for_pitch: [Option<usize>; 120]`) and the other
/// variants (~50-100 B). The standard fix is `Box<InstrDefault>`, but:
///   - The XM-style `Default` variant is the dominant case in every XM/MOD/S3M
///     module, so the memory win only materialises for modules that have many
///     `Empty` instrument slots — an XM-file pathological case, not the norm.
///   - Boxing adds one allocation per instrument at load time and breaks
///     cache locality while iterating the instrument table.
///   - It is a breaking change in the public API (every construction site
///     and pattern-match against `InstrumentType::Default(d)` would have to
///     switch to `Box::new(...)` / auto-deref).
///
/// Kept as-is intentionally.
#[allow(clippy::large_enum_variant)]
#[derive(Default, Serialize, Deserialize, Debug)]
pub enum InstrumentType {
    /// No Instrument
    #[default]
    Empty,
    /// Historical XM Instrument
    Default(InstrDefault),
    /// Euclidean Rhythm Instrument
    Euclidean(InstrEkn),
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
