//! `VoiceSetup` — the parameters that govern a voice once it's
//! been triggered.
//!
//! Volume envelope, pan envelope, pitch envelope, fadeout, vibrato,
//! random variations, filter setup. Distinct from
//! [`InstrumentBehavior`](crate::instrument_behavior::InstrumentBehavior),
//! which decides what *happens* when a new note retriggers; and from
//! [`Keyboard`](crate::keyboard::Keyboard), which decides which
//! sample plays at which pitch. `VoiceSetup` is everything else
//! about the voice itself.

use crate::envelope::Envelope;
use crate::pitch::Pitch;
use crate::vibrato::Vibrato;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VoiceSetup {
    // === Volume
    /// Master volume of the instrument, in 0..=1. Multiplied with
    /// the per-cell volume, the song-level `Module.global_volume`,
    /// the channel volume and the envelope to produce the final
    /// voice gain.
    pub volume: f32,
    pub volume_envelope: Envelope,
    pub volume_fadeout: f32,
    /// Random volume variation
    pub random_volume_variation: f32,

    // === Panning
    pub pan_envelope: Envelope,
    pub default_pan: f32,
    /// Pitch and pan separation (-1 to 1)
    pub pitch_pan_separation: f32,
    /// Center note for panning
    pub pitch_pan_center: Pitch,
    /// Random panning variation
    pub random_pan_variation: f32,

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
            volume: 1.0,
            volume_envelope: Envelope::default(),
            volume_fadeout: 0.0,
            random_volume_variation: 0.0,

            pan_envelope: Envelope::default(),
            default_pan: 0.5,
            pitch_pan_separation: 0.0,
            pitch_pan_center: Pitch::C4,
            random_pan_variation: 0.0,

            pitch_envelope: Envelope::default(),
            pitch_envelope_as_low_pass_filter: false,
            vibrato: Vibrato::default(),

            initial_filter_cutoff: 0,
            initial_filter_resonance: 0,
        }
    }
}
