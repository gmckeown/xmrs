#![forbid(unsafe_code)]
#![allow(dead_code)]

//!
//! XMrs is a Safe SoundTracker Library
//!
//! ```text
//! Module+--->Instrument+--->InstrDefault+--->VoiceSetup+--->Envelope (Volume, Panning, Pitch)
//!       |              |                |              +--->Vibrato
//!       |              |                +--->InstrumentBehavior (NNA, DCT, DCA)
//!       |              |                +--->Keyboard (sample_for_pitch, note_for_pitch)
//!       |              |                +--->InstrMidi
//!       |              |                +--->Sample (Loop, Sustain Loop)
//!       |              +--->InstrEkn (Euclidian Rythm Instrument)
//!       |              +--->InstrMidi
//!       |              +--->InstrOpl (Yamaha OPL)
//!       |              +--->InstrSid (MOS6581 SID Voices)
//!       |              +-+->InstrRobSid+--->InstrSid
//!       +--->Pattern--->Row--->TrackUnit+--->CellNote (Empty, Play(Pitch), KeyOff, NoteCut, NoteFade)
//!                                       +--->TrackEffect
//!                                       +--->GlobalEffect
//! ```
//!
//! You can load historical IT, S3M, SID, MOD, XM files using `import` (see `README.md`)
//!
//! You can serialize your work using serde
//!

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

// `no_std` builds need a float math backend because `f32`'s transcendental
// methods (`sqrt`, `sin`, `log2`, ...) live in `std`. Users must opt into
// exactly one of `libm` or `micromath` when `std` is disabled.
#[cfg(not(any(feature = "std", feature = "libm", feature = "micromath")))]
::core::compile_error!(
    "xmrs: with `std` disabled, enable exactly one float math backend: \
     feature `libm` or feature `micromath`."
);

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
/// New-Note Action / Duplicate Check rules for `InstrDefault`.
pub mod instrument_behavior;
/// IT-style keyboard remap: per-input-note sample + transposition.
pub mod keyboard;
/// SoundTracker Module with Steroid
pub mod module;
/// Period Helper
pub mod period_helper;
/// A typical Note
pub mod cell_note;
/// Compatibility profile (named bundles of format + playback quirks).
pub mod compatibility_profile;

pub mod pitch;
/// Sample with Steroid
pub mod sample;
/// A slot
pub mod track_unit;
/// Vibrato with Steroid
pub mod vibrato;
/// Voice-shaping parameters (envelopes, vibrato, filter, panning).
pub mod voice_setup;
/// All Waveform type
pub mod waveform;
/// A simple way for random values
pub mod xorshift;

/// The Xmrs Prelude
pub mod prelude;

/// Import historical files.
/// Do not use it directly: see Module load* fn impl.
///
/// Compiled whenever at least one historical-format importer is
/// enabled. Granular selection lives behind the `import_mod`,
/// `import_xm`, `import_s3m`, `import_it`, and `import_sid` features;
/// the umbrella `import` feature turns them all on.
#[cfg(any(
    feature = "import_mod",
    feature = "import_xm",
    feature = "import_s3m",
    feature = "import_it",
    feature = "import_sid",
))]
pub mod import;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(42, 42);
    }

    #[cfg(feature = "import_xm")]
    #[test]
    fn load_empty_xm() {
        let data = include_bytes!("../examples/empty.xm");
        let module = crate::module::Module::load_xm(data);
        assert!(
            module.is_ok(),
            "Failed to load empty.xm: {:?}",
            module.err()
        );
    }

    #[cfg(feature = "import_xm")]
    #[test]
    fn load_note_xm() {
        let data = include_bytes!("../examples/note.xm");
        let module = crate::module::Module::load_xm(data).expect("Failed to load note.xm");
        assert!(!module.pattern.is_empty(), "Module should have patterns");
        assert!(
            !module.instrument.is_empty(),
            "Module should have instruments"
        );
    }

    #[cfg(feature = "import_xm")]
    #[test]
    fn load_xi_instrument() {
        let data = include_bytes!("../examples/instr.xi");
        let xmi = crate::import::xm::xi_instrument::XiInstrument::load(data);
        assert!(xmi.is_ok(), "Failed to load instr.xi: {:?}", xmi.err());
    }

    #[cfg(feature = "import_xm")]
    #[test]
    fn autodetect_xm() {
        let data = include_bytes!("../examples/note.xm");
        let module = crate::module::Module::load(data);
        assert!(module.is_ok(), "Autodetect should find XM format");
    }
}
