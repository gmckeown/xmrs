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
//!       |              +--->InstrEkn (Euclidean Rhythm Instrument, wraps another Instrument)
//!       |              +--->InstrMidi
//!       |              +--->InstrOpl (Yamaha OPL)
//!       |              +--->InstrSid (MOS6581 SID Voices)
//!       |              +-+->InstrRobSid+--->InstrSid
//!       +--->Track+--->TrackUnit+--->CellNote (Empty, Play(Pitch), KeyOff, NoteCut, NoteFade)
//!       |          +--->TrackEffect
//!       |          +--->GlobalEffect
//!       +--->Clip (placement of a Track on the timeline)
//!       +--->AutomationLane (continuous parameter curves)
//!       +--->TimelineMap (linearised playback order)
//! ```
//!
//! You can load historical IT, S3M, SID, MOD, XM files using `import` (see `README.md`)
//!
//! You can serialize your work using serde
//!

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

/// A typical Note
pub mod cell_note;
/// Detect the legacy 8-bit codepage of a name field and decode it
/// to UTF-8 (CP437 vs Latin-1 vs ASCII).
pub mod codepage;
/// Compatibility profile (named bundles of format + playback quirks).
pub mod compatibility_profile;
/// Playback-duration estimator (walks the row scheduler).
pub mod duration;
/// All effects
pub mod effect;
/// Envelope with Steroid
pub mod envelope;
/// Fixed-point primitives, domain newtypes, lookup tables and tracker-byte
/// conversions. Provides the integer-only substrate `xmrs` uses to run
/// on FPU-less targets — every former `f32` site on the runtime path
/// has been folded onto these types. See [`fixed`] for the layer-by-
/// layer breakdown.
pub mod fixed;
/// Instrument handling samples
pub mod instr_default;
/// Euclidean Rhythm Instrument
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
/// IT-style mix-plugin table (OpenMPT extension).
pub mod mix_plugin;
/// SoundTracker Module with Steroid
pub mod module;
/// Period Helper
pub mod period_helper;

/// DAW layer: tracks, clips, automation lanes, timeline cache,
/// build pipeline, and Euclidean detection.
pub mod daw;
/// Edit command API (atomic, undoable mutations of `Module`)
pub mod edit;
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
        assert!(
            !module.instrument.is_empty(),
            "Module should have instruments"
        );
        assert!(
            !module.timeline_map.entries.is_empty(),
            "timeline_map should be populated"
        );
        assert!(!module.tracks.is_empty(), "tracks should be populated");
        module
            .verify_layers_consistent()
            .expect("DAW layer should be consistent after import");
    }

    #[cfg(feature = "import_sid")]
    #[test]
    fn load_sid_commando_has_consistent_layer() {
        let sid = crate::import::sid::sid_module::SidModule::get_sid_commando();
        let modules = sid.to_modules(false);
        assert!(
            !modules.is_empty(),
            "SID should produce at least one Module"
        );
        for (i, m) in modules.iter().enumerate() {
            assert!(
                !m.tracks.is_empty(),
                "SID module {} should have tracks after build_timeline_layer",
                i
            );
            m.verify_layers_consistent()
                .unwrap_or_else(|e| panic!("SID module {} inconsistent: {:?}", i, e));
        }
    }

    #[cfg(feature = "import_xm")]
    #[test]
    fn load_empty_xm_has_consistent_layer() {
        let data = include_bytes!("../examples/empty.xm");
        let module = crate::module::Module::load_xm(data).expect("load");
        module
            .verify_layers_consistent()
            .expect("DAW layer should be consistent on empty.xm");
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
