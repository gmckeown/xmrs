//! The Xmrs Prelude.
//!
//! Re-exports the commonly-used items from `core::` and `tracker::`
//! so consumer code can pull them in with a single `use`:
//!
//! ```
//! #![allow(unused_imports)]
//! use xmrs::prelude::*;
//! ```
//!
//! Tracker-only items (`ModuleFormat`, `MixPlugin*`, `Instr{Opl,
//! Sid,RobSid}`) are gated behind their respective `import_*`
//! features — they simply aren't present in the prelude on a no-
//! tracker build.

pub use crate::{
    core::cell::{Cell, CellEvent},
    core::cell_note::CellNote,
    core::compatibility::{PanResetPolicy, PlaybackQuirks},
    core::daw::automation::{AutomationLane, AutomationPoint, AutomationTarget, AutomationValue},
    core::daw::clip::Clip,
    core::daw::timeline::{TimelineEntry, TimelineMap},
    core::daw::track::Track,
    core::effect::{
        GlobalEffect, MidiMacroType, NavigationEffect, NoteRetrigOperator, SongLevelEffect,
        TrackEffect,
    },
    core::envelope::{Envelope, EnvelopePoint},
    core::fixed::units::{
        Amp, Amplification, ChannelVolume, Finetune, Panning, Period, Pitch as PitchQ, PitchDelta,
        SampleRate, Volume,
    },
    core::instr_default::{DuplicateCheckAction, DuplicateCheckType, InstrDefault, NewNoteAction},
    core::instr_ekn::InstrEkn,
    core::instr_midi::InstrMidi,
    core::instrument::{Instrument, InstrumentType},
    core::instrument_behavior::InstrumentBehavior,
    core::keyboard::Keyboard,
    core::module::{ChannelDefault, MidiMacros, Module, PatternHighlight},
    core::pitch::Pitch,
    core::sample::{LoopType, Sample, SampleDataType},
    core::vibrato::Vibrato,
    core::voice_setup::VoiceSetup,
    core::waveform::Waveform,
    tracker::period::{FrequencyType, PeriodHelper},
};

#[cfg(feature = "import_s3m")]
pub use crate::tracker::instr_opl::{InstrOpl, MdiInstr, MdiOpl};

#[cfg(feature = "import_sid")]
pub use crate::tracker::instr_sid::InstrSid;

#[cfg(feature = "import_sid")]
pub use crate::tracker::instr_robsid::InstrRobSid;

#[cfg(feature = "import_it")]
pub use crate::tracker::mix_plugin::{MixPlugin, MixPluginInfo, MixPlugins};

#[cfg(any(
    feature = "import_mod",
    feature = "import_xm",
    feature = "import_s3m",
    feature = "import_it",
    feature = "import_sid",
))]
pub use crate::tracker::format::ModuleFormat;
