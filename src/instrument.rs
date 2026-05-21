use serde::{Deserialize, Serialize};

use crate::instr_default::InstrDefault;
use crate::instr_ekn::InstrEkn;
use crate::instr_midi::InstrMidi;
use crate::instr_opl::InstrOpl;
use crate::instr_robsid::InstrRobSid;
use crate::instr_sid::InstrSid;

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
    /// Yamaha OPL
    Opl(InstrOpl),
    /// MOS6581 SID Voice
    Sid(InstrSid),
    /// Rob Hubbard Instrument,
    RobSid(InstrRobSid),
}

/// Instrument with Steroid
#[derive(Default, Serialize, Deserialize, Debug)]
pub struct Instrument {
    pub name: String,
    pub instr_type: InstrumentType,
    pub muted: bool,
}
