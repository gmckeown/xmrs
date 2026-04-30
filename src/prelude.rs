/// The Xmrs Prelude.
///
/// The purpose of this module is to alleviate imports of Xmrs module parts
///
/// ```
/// #![allow(unused_imports)]
/// use xmrs::prelude::*;
/// ```
///
pub use crate::{
    cell_note::CellNote,
    compatibility_profile::CompatibilityProfile,
    effect::{GlobalEffect, MidiMacroType, NoteRetrigOperator, TrackEffect},
    envelope::{Envelope, EnvelopePoint},
    fixed::units::{Amplification, Amp, ChannelVolume, Finetune, Panning, Period, Pitch as PitchQ, PitchDelta, SampleRate, Volume},
    instr_default::{DuplicateCheckAction, DuplicateCheckType, InstrDefault, NewNoteAction},
    instr_ekn::InstrEkn,
    instr_midi::InstrMidi,
    instr_opl::{InstrOpl, MdiInstr, MdiOpl},
    instr_robsid::InstrRobSid,
    instr_sid::InstrSid,
    instrument::{Instrument, InstrumentType},
    instrument_behavior::InstrumentBehavior,
    keyboard::Keyboard,
    mix_plugin::{MixPlugin, MixPluginInfo, MixPlugins},
    module::{
        ChannelDefault, MidiMacros, Module, ModuleFormat, PanResetPolicy, Pattern,
        PatternHighlight, PlaybackQuirks, Row, MAX_NUM_ROWS,
    },
    period_helper::{FrequencyType, PeriodHelper},
    pitch::Pitch,
    sample::{LoopType, Sample, SampleDataType},
    track_unit::TrackUnit,
    vibrato::Vibrato,
    voice_setup::VoiceSetup,
    waveform::Waveform,
};
