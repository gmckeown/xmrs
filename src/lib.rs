#![forbid(unsafe_code)]
#![allow(dead_code)]

//! # xmrs — A Safe SoundTracker Library
//!
//! ## Architecture
//!
//! ```text
//! xmrs
//! ├── core/                        ← Modern DAW data model (format-agnostic)
//! │   ├── module::Module
//! │   │    ├── instrument: Vec<Instrument>
//! │   │    ├── tracks: Vec<Track> ──► Cell { event, effects: Vec<TrackEffect> }
//! │   │    ├── clips: SortedClips
//! │   │    ├── automation: Vec<AutomationLane>
//! │   │    ├── timeline_map: TimelineMap
//! │   │    └── quirks: PlaybackQuirks (default all-off)
//! │   ├── compatibility::{PlaybackQuirks, PanResetPolicy}
//! │   ├── daw/                     ← AutomationLane, Clip, Track, TimelineMap, Euclidean
//! │   └── fixed/                   ← Q-format primitives
//! │
//! └── tracker/                     ← Historical-format support
//!     ├── format::ModuleFormat     ← (gated on any `import_*`)
//!     ├── profiles::{ft2, it214, st3, pt}    ← preset quirks
//!     ├── period::FrequencyType
//!     ├── codepage                 ← CP437 / Latin-1
//!     ├── mix_plugin               ← IT OpenMPT extension (cfg import_it)
//!     ├── instr_{opl, sid, robsid} ← cfg-gated per importer
//!     └── import/                  ← XM / IT / S3M / MOD / SID readers
//!         ├── build/               ← Pattern → Tracks + Clips + TimelineMap
//!         └── extract/             ← TrackImportUnit → AutomationLane
//! ```
//!
//! ## Features
//!
//! Default `std + import` matches the desktop / server case. For a
//! clean no-tracker build:
//!
//! ```toml
//! [dependencies]
//! xmrs = { version = "0.13", default-features = false, features = ["std"] }
//! ```
//!
//! In this mode `Module` carries only the universal fields; no
//! `origin`, no `mix_plugins`, no `Opl/Sid/RobSid` instrument
//! variants. The historical presets in `tracker::profiles` are
//! absent — quirks are all-off by default.
//!
//! You can serialize your work using serde.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

/// Core data model — format-agnostic.
pub mod core;

/// Format-tied items: presets, importers, format-specific instruments.
/// Each sub-module is gated behind one or more `import_*` features.
pub mod tracker;

/// Edit command API (atomic, undoable mutations of `Module`)
pub mod edit;

/// The Xmrs Prelude
pub mod prelude;

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
        let module = crate::core::module::Module::load_xm(data);
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
        let module = crate::core::module::Module::load_xm(data).expect("Failed to load note.xm");
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
        let sid = crate::tracker::import::sid::sid_module::SidModule::get_sid_commando();
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
        let module = crate::core::module::Module::load_xm(data).expect("load");
        module
            .verify_layers_consistent()
            .expect("DAW layer should be consistent on empty.xm");
    }

    #[cfg(feature = "import_xm")]
    #[test]
    fn load_xi_instrument() {
        let data = include_bytes!("../examples/instr.xi");
        let xmi = crate::tracker::import::xm::xi_instrument::XiInstrument::load(data);
        assert!(xmi.is_ok(), "Failed to load instr.xi: {:?}", xmi.err());
    }

    #[cfg(feature = "import_xm")]
    #[test]
    fn autodetect_xm() {
        let data = include_bytes!("../examples/note.xm");
        let module = crate::core::module::Module::load(data);
        assert!(module.is_ok(), "Autodetect should find XM format");
    }
}
