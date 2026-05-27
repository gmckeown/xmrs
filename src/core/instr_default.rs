//! `InstrDefault` — the canonical xmrs instrument representation.
//!
//! Decomposed into three sub-types, each tackling one orthogonal
//! concern:
//!
//! - [`VoiceSetup`] — envelope, fadeout, vibrato, filter, panning,
//!   random variations: everything that shapes the voice once it's
//!   been triggered.
//! - [`InstrumentBehavior`] — NNA / DCT / DCA: what happens when a
//!   channel already running this instrument is retriggered.
//! - [`Keyboard`] — per-input-note sample selection and
//!   transposition: the IT-style drum-kit table. For non-IT formats
//!   the keyboard is the identity remap.
//!
//! Plus the resources the instrument owns: the sample list and the
//! optional MIDI routing.

use serde::{Deserialize, Serialize};

use crate::core::instr_midi::InstrMidi;
use crate::core::instrument_behavior::InstrumentBehavior;
use crate::core::keyboard::Keyboard;
use crate::core::sample::Sample;
use crate::core::voice_setup::VoiceSetup;

use alloc::vec::Vec;

// Convenience re-exports — canonical home is `instrument_behavior`.
pub use crate::core::instrument_behavior::{
    DuplicateCheckAction, DuplicateCheckType, NewNoteAction,
};

/// The canonical xmrs sample-based instrument — the dominant
/// variant of [`InstrumentType`] for every XM / MOD / S3M / IT
/// module.
///
/// Decomposed into four orthogonal pieces:
/// - [`VoiceSetup`] — envelope, fadeout, vibrato, filter, panning,
///   random variations: what shapes the voice once triggered.
/// - [`InstrumentBehavior`] — NNA / DCT / DCA: what happens when a
///   channel running this instrument is retriggered.
/// - [`Keyboard`] — IT-style drum-kit table mapping each input note
///   to a `(sample_index, transposition)`.
/// - `sample: Vec<Option<Sample>>` — the actual PCM payloads.
///
/// Plus an optional [`InstrMidi`] routing for hardware-synth setups.
///
/// [`InstrumentType`]: crate::core::instrument::InstrumentType
/// [`VoiceSetup`]: crate::core::voice_setup::VoiceSetup
/// [`InstrumentBehavior`]: crate::core::instrument_behavior::InstrumentBehavior
/// [`Keyboard`]: crate::core::keyboard::Keyboard
/// [`InstrMidi`]: crate::core::instr_midi::InstrMidi
#[derive(Default, Serialize, Deserialize, Debug, Clone)]
pub struct InstrDefault {
    /// Voice-shaping parameters: envelopes, fadeout, vibrato,
    /// filter, panning, random variations.
    pub voice: VoiceSetup,

    /// What happens when this instrument is retriggered: NNA / DCT /
    /// DCA.
    pub behavior: InstrumentBehavior,

    /// Per-input-note keyboard layout. For IT drum kits, maps each
    /// key to a sample and an output pitch; for other formats, all
    /// entries stay at `None` (= identity).
    pub keyboard: Keyboard,

    /// Optional MIDI routing for this instrument.
    pub midi: InstrMidi,
    pub midi_mute_computer: bool,

    /// The samples this instrument can trigger. Indexed by the
    /// `keyboard.sample_for_pitch` table.
    pub sample: Vec<Option<Sample>>,
}

// `Default` is derived above; the explicit constructor is gone now
// that every field has a meaningful `Default`.

impl InstrDefault {
    /// Convenience: set every input-note's sample mapping to
    /// `sample_index`. Forwarded to `Keyboard::map_all_to`. Used by
    /// importers that have a single sample but want it triggered
    /// uniformly across the whole keyboard.
    pub fn change_all_sample_for_pitch(&mut self, sample_index: usize) {
        self.keyboard.map_all_to(sample_index);
    }
}

// `Vec<Option<Sample>>` doesn't derive Default automatically, but
// our derive(Default) above relies on every field having one. Vec
// does have Default (empty vec), so the derive Just Works.
