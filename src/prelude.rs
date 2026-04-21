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
    effect::{GlobalEffect, NoteRetrigOperator, TrackEffect},
    envelope::{Envelope, EnvelopePoint},
    instr_default::{DuplicateCheckAction, DuplicateCheckType, InstrDefault, NewNoteAction},
    instr_ekn::InstrEkn,
    instr_midi::InstrMidi,
    instr_opl::{InstrOpl, MdiInstr, MdiOpl},
    instr_robsid::InstrRobSid,
    instr_sid::InstrSid,
    instrument::{Instrument, InstrumentType},
    module::{Module, ModuleFormat, Pattern, Row, MAX_NUM_ROWS},
    period_helper::{FrequencyType, PeriodHelper},
    pitch::Pitch,
    sample::{LoopType, Sample, SampleDataType},
    track_unit::TrackUnit,
    vibrato::Vibrato,
    waveform::Waveform,
};
