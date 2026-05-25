//! SID sound-effects scheduler used by [`super::one_sid`] to drive
//! Rob-Hubbard-style per-tick voice updates (waveform cycling,
//! pulse-width sweeps, frequency arpeggios, vibrato envelopes).
//! Each `SoundFx` entry describes one effect timeline; the SID
//! importer materialises the table into per-row `TrackImportUnit`
//! mutations.

use crate::tracker::instr_sid::SidVoice;

#[derive(Debug)]
pub struct SoundFx {
    pub incdec_start_at_end: bool,
    pub incdec_counter: i8,
    pub note_start: u8,
    pub note_delta: u8,
    pub note_end: u8,
    pub flipflop_voice1_ctrl: bool,
    pub voice0_ctrl: bool,
    pub voice1_ctrl: bool,
    pub voice0: SidVoice,
    pub voice1: SidVoice,
}
