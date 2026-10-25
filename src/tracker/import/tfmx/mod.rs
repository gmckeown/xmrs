#![forbid(unsafe_code)]

//! TFMX (The Final Musicsystem eXtended) importer.
//!
//! Provides parsing and playback support for TFMX files (`mdat.*` / `smpl.*`
//! and `.tfx` / `.sam`).

pub mod header;
pub mod parser;
pub mod projector;
pub mod runtime;
pub mod tables;

pub use header::{TfmxMacro, TfmxMacroStep, TfmxModule, TfmxPattern, TfmxSubSong, TfmxTrackStep};
pub use projector::{build_instruments, compile_xm_bytes, voice_panning, XmInstrumentSpec};
pub use runtime::{ActiveSampleEvent, TfmxSimulation, TfmxSimulator, TfmxTickFrame, VoiceTickSnapshot};
