//! `VoiceSetup` — the parameters that govern a voice once it's
//! been triggered.
//!
//! Volume envelope, pan envelope, pitch envelope, fadeout, vibrato,
//! random variations, filter setup. Distinct from
//! [`InstrumentBehavior`](crate::core::instrument_behavior::InstrumentBehavior),
//! which decides what *happens* when a new note retriggers; and from
//! [`Keyboard`](crate::core::keyboard::Keyboard), which decides which
//! sample plays at which pitch. `VoiceSetup` is everything else
//! about the voice itself.

use crate::core::envelope::Envelope;
use crate::core::fixed::fixed::Q15;
use crate::core::fixed::units::{Panning, Volume};
use crate::core::pitch::Pitch;
use crate::core::vibrato::Vibrato;
use serde::{Deserialize, Serialize};

/// Voice-shaping parameters for an [`InstrDefault`]: everything
/// that decides how the voice sounds once a sample is triggered.
///
/// Groups three envelopes (volume / panning / pitch), a vibrato
/// LFO, a low-pass filter, the volume fadeout cadence, and the
/// per-instrument default panning. The runtime keeps a parallel
/// state machine per active voice; this struct just stores the
/// authored configuration.
///
/// [`InstrDefault`]: crate::core::instr_default::InstrDefault
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VoiceSetup {
    // === Volume
    /// Master volume of the instrument, Q1.15 in `[0, 1]`.
    /// Multiplied with the per-cell volume, the song-level
    /// `Module.global_volume`, the channel volume and the
    /// envelope to produce the final voice gain.
    pub volume: Volume,
    pub volume_envelope: Envelope,
    /// Per-tick volume-fadeout step. Same Q1.15 scale as
    /// [`Volume`] — subtracted from the running fadeout
    /// register each tick, clamped at zero.
    ///
    /// Some XM modules can encode a fadeout value that
    /// nominally maps to `> 1.0` (the import path is
    /// `byte / 32760` and `byte` is u16); those saturate to
    /// `Volume::FULL` here, which is musically equivalent to
    /// "fade instantly".
    pub volume_fadeout: Volume,
    /// Random volume variation amplitude, Q1.15 in `[0, 1]`
    /// (IT-style 0..100 percent at import time).
    pub random_volume_variation: Q15,

    // === Panning
    pub pan_envelope: Envelope,
    /// Default pan position, Q1.15 in `[0, 1]`.
    pub default_pan: Panning,
    /// Pitch-pan separation coefficient, Q1.15 signed in
    /// `[-1, +1]`. Multiplies pitch deviation from the
    /// `pitch_pan_center` to bias panning.
    pub pitch_pan_separation: Q15,
    /// Center note for panning
    pub pitch_pan_center: Pitch,
    /// Random panning variation amplitude, Q1.15 in `[0, 1]`.
    pub random_pan_variation: Q15,

    // === Pitch
    pub pitch_envelope: Envelope,
    pub pitch_envelope_as_low_pass_filter: bool,
    pub vibrato: Vibrato,

    // === Filter
    /// Initial filter cutoff frequency (0-127)
    /// f = 110*2^(0.25+ce/fe),
    /// where ce is the cutoff frequency
    /// and fe is 24 for standard filter range
    /// or 20 if using OpenMPT's extended filter range.
    pub initial_filter_cutoff: u8,

    /// Initial filter resonance (0-127)
    /// The formula used is 10^((-resonance*24.0)/(128.0f*20.0f))
    pub initial_filter_resonance: u8,
}

impl Default for VoiceSetup {
    fn default() -> Self {
        Self {
            volume: Volume::FULL,
            volume_envelope: Envelope::default(),
            volume_fadeout: Volume::SILENT,
            random_volume_variation: Q15::ZERO,

            pan_envelope: Envelope::default(),
            default_pan: Panning::CENTER,
            pitch_pan_separation: Q15::ZERO,
            pitch_pan_center: Pitch::C4,
            random_pan_variation: Q15::ZERO,

            pitch_envelope: Envelope::default(),
            pitch_envelope_as_low_pass_filter: false,
            vibrato: Vibrato::default(),

            initial_filter_cutoff: 0,
            initial_filter_resonance: 0,
        }
    }
}
