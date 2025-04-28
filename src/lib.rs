#![forbid(unsafe_code)]
#![allow(dead_code)]

//!
//! XMrs is a Safe SoundTracker Library
//!
//! ```
//! Module+--->Instrument+--->InstrDefault+--->Sample (Loop, Sustain Loop)
//!       |              |                +--->Envelope (Pitch, Volume, Panning)
//!       |              |                +--->Vibrato
//!       |              |                +--->InstrMidi
//!       |              +--->InstrEkn (Euclidian Rythm Instrument)
//!       |              +--->InstrMidi
//!       |              +--->InstrOpl (Yamaha OPL)
//!       |              +--->InstrSid (MOS6581 SID Voices)
//!       |              +-+->InstrRobSid+--->InstrSid
//!       +--->Pattern--->Row--->TrackUnit+--->TrackEffect
//!                                       +--->GlobalEffect
//! ```
//!
//! You can load historical IT, S3M, SID, MOD, XM files using `import` (see `README.md`)
//!
//! You can serialize your work using serde
//!

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

/// All effects
pub mod effect;
/// Envelope with Steroid
pub mod envelope;
/// Instrument handling samples
pub mod instr_default;
/// Euclidian Rythm Instrument
pub mod instr_ekn;
/// Midi Instrument
pub mod instr_midi;
/// Yamaha OPL Instrument
pub mod instr_opl;
/// Rob Hubbard Instrument
pub mod instr_robsid;
/// MOS6581 SID Instrument
pub mod instr_sid;
/// Instrument with Steroid
pub mod instrument;
/// SoundTracker Module with Steroid
pub mod module;
/// Period Helper
pub mod period_helper;
pub(crate) mod period_helper_cache;
/// A typical Note
pub mod pitch;
/// Sample with Steroid
pub mod sample;
/// A slot
pub mod track_unit;
/// Vibrato with Steroid
pub mod vibrato;
/// All Waveform type
pub mod waveform;
/// A simple way for random values
pub mod xorshift;

/// The Xmrs Prelude
pub mod prelude;

#[cfg(feature = "import")]
/// Import historical files.
/// Do not use it directly: see Module load* fn impl
pub mod import;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(42, 42);
    }
}
