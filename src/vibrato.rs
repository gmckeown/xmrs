use serde::{Deserialize, Serialize};

use crate::fixed::fixed::Q8_8;
use crate::fixed::units::PitchDelta;
use crate::waveform::Waveform;

/// Instrument auto-vibrato.
///
/// All three numeric fields used to be `f32`. They moved to
/// fixed-point in Phase 2 of the no-FPU port:
///
/// * [`speed`](Self::speed) — fractional cycles per tick. The
///   XM range, after `byte / 252` normalisation, is `[0, ~1.012]`.
///   `Q8_8` covers this with room to spare (`±128.0`).
/// * [`depth`](Self::depth) — modulation amplitude as a pitch
///   delta. XM stores it as `byte / 30 ∈ [0, ~8.5]` worst case;
///   SID as `byte / 15 ∈ [0, 1]`. [`PitchDelta`] is the right
///   newtype: same Q8.8 storage, semantically a pitch quantity.
/// * [`sweep`](Self::sweep) — phase threshold for ramp-in,
///   measured in the same units as accumulated `speed`. XM
///   stores it as `byte / 255 ∈ [0, 1]`. `Q8_8` again.
///
/// The runtime phase accumulator (in `xmrsplayer`'s
/// `state_auto_vibrato.rs`) is integer Q.16 cycles; combined
/// with these Q-typed parameters, the auto-vibrato chain is
/// `f32`-free end-to-end.
#[derive(Default, Serialize, Deserialize, Clone, Copy, Debug)]
pub struct Vibrato {
    pub waveform: Waveform,
    pub speed: Q8_8,
    pub depth: PitchDelta,
    pub sweep: Q8_8,
}
