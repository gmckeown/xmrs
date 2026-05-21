use serde::{Deserialize, Serialize};

use crate::fixed::fixed::Q15;

/// Euclidean Rhythm Instrument.
///
/// `events` pulses distributed evenly over `steps` positions
/// (Bjorklund / Toussaint), rotated by `rotation`. Each pulse
/// triggers the wrapped instrument `instr`.
///
/// # Humanisation
///
/// At playback time each pulse can be nudged earlier by up to
/// `humanize_advance_max_ticks` ticks, with probability
/// `humanize_probability` (Q1.15 in `[0, 1]`). Defaults are
/// `humanize_advance_max_ticks = 0` and `humanize_probability =
/// Q15::ZERO`, i.e. no humanisation — cycle-exact strict.
///
/// The model is asymmetric: advance only, no delay. Forward groove
/// is the typical aesthetic for tracker / electronic music; a
/// retard-based variant, if ever needed, would be a separate
/// parameter.
#[derive(Serialize, Deserialize, Debug)]
pub struct InstrEkn {
    /// Pulsation k
    pub events: u8,
    /// Duration n
    pub steps: u8,
    /// Rotation
    pub rotation: u8,
    /// Instrument number
    pub instr: Option<usize>,

    /// Maximum advance (in ticks) applied randomly to each
    /// pulsation. `0` (default) means no humanisation.
    pub humanize_advance_max_ticks: u8,

    /// Probability that a given pulsation is advanced.
    /// `Q15::ZERO` (default) means never; `Q15::ONE` means every
    /// pulsation. Drawn independently per pulsation by the player.
    pub humanize_probability: Q15,
}

impl Default for InstrEkn {
    fn default() -> Self {
        Self {
            events: 3,
            steps: 8,
            rotation: 0,
            instr: None,
            humanize_advance_max_ticks: 0,
            humanize_probability: Q15::ZERO,
        }
    }
}
