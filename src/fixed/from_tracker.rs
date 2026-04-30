//! Tracker LFO and retrig conversions, all `const fn`, no float.
//!
//! Most byte-to-Q-format conversions used by the importers are
//! direct constructors on the destination newtype itself
//! (`Volume::from_byte_64`, `Panning::from_byte_255`,
//! `Finetune::from_ratio`, etc., in [`crate::fixed::units`]).
//! What lives here is the residue: shared encodings used by
//! several effect families, where the target Q-format isn't
//! obvious from the call site alone.
//!
//! * **LFO speed / depth nibbles** — `4xy`, `7xy`, `Hxy`, `Yxy`,
//!   `Uxy`, etc. Conventions vary (`÷16`, `÷64`, `÷4`) and the
//!   chosen convention is encoded in the function name.
//! * **Retrig volume modifiers** — `Rxy` (FT2) and `Qxy` (S3M)
//!   share the multiplier table layout but disagree on the
//!   nibble-6 ratio.

use crate::fixed::fixed::{Q15, Q8_8};
use crate::fixed::units::{PitchDelta, RetrigMul};

// =====================================================================
// VIBRATO / TREMOLO / PANBRELLO speed and depth
// =====================================================================

/// XM / S3M vibrato / tremolo speed nibble (`0..=15`) → Q8.8
/// cycles-per-tick. Replaces `(param >> 4) as f32 / 64.0`.
#[inline]
pub const fn lfo_speed_from_nibble_64(speed_nibble: u8) -> Q8_8 {
    Q8_8::from_ratio((speed_nibble & 0x0F) as i16, 64)
}

/// XM tremolo / panbrello depth nibble (`0..=15`) → Q1.15
/// amplitude in `[0, ~0.94]`. Replaces `(param & 0x0F) as f32 / 16.0`.
#[inline]
pub const fn lfo_depth_from_nibble_16(depth_nibble: u8) -> Q15 {
    Q15::from_ratio((depth_nibble & 0x0F) as i32, 16)
}

/// IT vibrato (`Hxy`) depth nibble (`0..=15`) → [`PitchDelta`].
/// IT's depth resolution is 4× finer than XM's: `depth = y/4`
/// semitones.
#[inline]
pub const fn it_vibrato_depth_from_nibble_4(depth_nibble: u8) -> PitchDelta {
    PitchDelta::from_ratio((depth_nibble & 0x0F) as i16, 4)
}

/// IT panbrello (`Yxy`) depth nibble (`0..=15`) → Q1.15
/// amplitude, saturating at full swing.
///
/// The OLD f32 code computed `y * 4 / 16 = y * 0.25`, letting
/// the LFO output overflow `[-1, 1]` and relying on the
/// downstream panning clamp to produce a full-swing effect.
/// `Q15::from_ratio` saturates at `Q15::ONE` for any `num/den ≥
/// 1`, which gives exactly the same audible behaviour: `y ∈
/// 0..=3` scale linearly, `y ≥ 4` all reach the panning clamp
/// every half-cycle.
#[inline]
pub const fn it_panbrello_depth_from_nibble_4(depth_nibble: u8) -> Q15 {
    Q15::from_ratio((depth_nibble & 0x0F) as i32, 4)
}

/// XM vibrato depth nibble (`0..=15`) → [`PitchDelta`].
/// XM `4xy` treats `y` as `1/16` semitone units.
#[inline]
pub const fn vibrato_depth_from_nibble_16(depth_nibble: u8) -> PitchDelta {
    PitchDelta::from_ratio((depth_nibble & 0x0F) as i16, 16)
}

// =====================================================================
// NOTE-RETRIG VOLUME MODIFIER (FT2 Rxy / S3M Qxy)
// =====================================================================

/// FT2-canonical retrig volume multiplier (nibble 6 = `11/16`).
#[inline]
pub const fn retrig_mul_ft2(nibble: u8) -> RetrigMul {
    match nibble {
        6 => RetrigMul::from_ratio(11, 16),
        7 => RetrigMul::from_ratio(1, 2),
        0xE => RetrigMul::from_ratio(3, 2),
        0xF => RetrigMul::from_ratio(2, 1),
        _ => RetrigMul::UNITY,
    }
}

/// S3M-canonical retrig volume multiplier (nibble 6 = `5/8`).
#[inline]
pub const fn retrig_mul_s3m(nibble: u8) -> RetrigMul {
    match nibble {
        6 => RetrigMul::from_ratio(5, 8),
        7 => RetrigMul::from_ratio(1, 2),
        0xE => RetrigMul::from_ratio(3, 2),
        0xF => RetrigMul::from_ratio(2, 1),
        _ => RetrigMul::UNITY,
    }
}

/// Retrig volume *additive* delta (nibbles `1..=5` and `9..=D`).
/// Each step is `±N/64` where `N ∈ {1, 2, 4, 8, 16}`.
#[inline]
pub const fn retrig_volume_delta_q15(nibble: u8) -> Q15 {
    match nibble {
        1 => Q15::from_ratio(-1, 64),
        2 => Q15::from_ratio(-2, 64),
        3 => Q15::from_ratio(-4, 64),
        4 => Q15::from_ratio(-8, 64),
        5 => Q15::from_ratio(-16, 64),
        9 => Q15::from_ratio(1, 64),
        0xA => Q15::from_ratio(2, 64),
        0xB => Q15::from_ratio(4, 64),
        0xC => Q15::from_ratio(8, 64),
        0xD => Q15::from_ratio(16, 64),
        _ => Q15::ZERO,
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lfo_speed_nibble_quantization() {
        // 16 distinct values, monotonically increasing.
        let mut prev = i16::MIN;
        for n in 0..16 {
            let q = lfo_speed_from_nibble_64(n);
            assert!(q.raw() > prev);
            prev = q.raw();
        }
    }

    #[test]
    fn it_panbrello_saturates_at_full_swing() {
        // `y ∈ 0..=3` scales linearly, `y ≥ 4` saturates.
        assert_eq!(it_panbrello_depth_from_nibble_4(0), Q15::ZERO);
        assert_eq!(it_panbrello_depth_from_nibble_4(4), Q15::ONE);
        assert_eq!(it_panbrello_depth_from_nibble_4(15), Q15::ONE);
    }

    #[test]
    fn retrig_mul_ft2_known_values() {
        assert_eq!(retrig_mul_ft2(7), RetrigMul::from_ratio(1, 2));
        assert_eq!(retrig_mul_ft2(0xF), RetrigMul::from_ratio(2, 1));
    }

    #[test]
    fn retrig_volume_delta_signs() {
        // Positive nibble (9..=D) and its negative counterpart
        // (1..=5) should have opposite raw values.
        for (neg, pos) in [(1u8, 9u8), (2, 0xA), (3, 0xB), (4, 0xC), (5, 0xD)] {
            assert_eq!(
                retrig_volume_delta_q15(neg).raw(),
                -retrig_volume_delta_q15(pos).raw()
            );
        }
        assert_eq!(retrig_volume_delta_q15(0), Q15::ZERO);
    }
}
