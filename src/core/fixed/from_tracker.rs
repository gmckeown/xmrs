//! Tracker LFO and retrig conversions, all `const fn`, no float.
//!
//! Most byte-to-Q-format conversions used by the importers are
//! direct constructors on the destination newtype itself
//! (`Volume::from_byte_64`, `Panning::from_byte_255`,
//! `Finetune::from_ratio`, etc., in [`crate::core::fixed::units`]).
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

use crate::core::fixed::fixed::{Q15, Q8_8};
use crate::core::fixed::units::{PitchDelta, RetrigMul};
use crate::tracker::period::FrequencyType;

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

/// XM vibrato depth nibble (`0..=15`) → [`PitchDelta`], `1/16`
/// semitone units. Retained for the IT importer (`it_effect.rs`),
/// which is linear-only and keeps its historical depth scale; the XM /
/// MOD path uses the frequency-mode-aware [`xm_vibrato_depth`] below.
#[inline]
pub const fn vibrato_depth_from_nibble_16(depth_nibble: u8) -> PitchDelta {
    PitchDelta::from_ratio((depth_nibble & 0x0F) as i16, 16)
}

/// XM `4xy` / `Vxy` vibrato depth nibble (`0..=15`) → lane depth,
/// **frequency-mode aware** to match FT2/ProTracker `doVibrato`
/// (`outPeriod = realPeriod ± (vibratoTab[pos] * depth) >> shift`):
///
/// * **Linear** — `nibble/8` semitone (`PitchDelta`). On a pitch-affine
///   linear period this is the same constant-period nudge FT2 applies
///   (`(255*nibble)>>5 ≈ 8*nibble` linear-period units ≈ `nibble/8` st).
///   This is **2× the historical `nibble/16`**, which was half FT2 depth.
///   ProTracker is always Amiga, so linear is always FT2 — `protracker`
///   has no effect on this branch.
/// * **Amiga** — the peak period amplitude stashed in the `PitchDelta`
///   raw as **Paula-period units**; the player adds it straight to the
///   period (see `StateInstrDefault::update_frequency`), because on the
///   Amiga period (`∝ 2^-pitch`) a constant period nudge is NOT a
///   constant pitch nudge. The replayer-specific shift:
///     * **ProTracker** (`.mod`, `protracker == true`) — `lsr.w #7` →
///       `(255*nibble)>>7` (`pt2_replayer.c:755`).
///     * **FT2** (Amiga `.xm`, `protracker == false`) — `>> 5` →
///       `(255*nibble)>>5` (`ft2_replayer.c:1858`), exactly **4× deeper**
///       than ProTracker for the same nibble (identical 0..255 sine
///       table). This is not a compatibility accident: FT2 applies its
///       own deeper formula to everything it loads, MODs included, so a
///       faithful `.mod` replay must use ProTracker's `>>7`.
///
/// MOD is always Amiga + ProTracker; XM picks the Amiga/Linear branch
/// from its header flag and is always FT2 (`protracker == false`).
#[inline]
pub fn xm_vibrato_depth(
    depth_nibble: u8,
    freq_type: FrequencyType,
    protracker: bool,
) -> PitchDelta {
    let n = (depth_nibble & 0x0F) as i16;
    match freq_type {
        FrequencyType::LinearFrequencies => PitchDelta::from_ratio(n, 8),
        FrequencyType::AmigaFrequencies => {
            let shift = if protracker { 7 } else { 5 };
            PitchDelta::from_q8_8_i16((255 * n) >> shift)
        }
    }
}

// =====================================================================
// NOTE-RETRIG VOLUME MODIFIER (FT2 Rxy / S3M Qxy)
// =====================================================================

/// Common retrig table; nibble 6's ratio is the only point on
/// which FT2 (`11/16`) and S3M (`5/8`) disagree.
#[inline]
const fn retrig_mul_common(nibble: u8, nibble6_num: i16, nibble6_den: i16) -> RetrigMul {
    match nibble {
        6 => RetrigMul::from_ratio(nibble6_num, nibble6_den),
        7 => RetrigMul::from_ratio(1, 2),
        0xE => RetrigMul::from_ratio(3, 2),
        0xF => RetrigMul::from_ratio(2, 1),
        _ => RetrigMul::UNITY,
    }
}

/// FT2-canonical retrig volume multiplier (nibble 6 = `11/16`).
#[inline]
pub const fn retrig_mul_ft2(nibble: u8) -> RetrigMul {
    retrig_mul_common(nibble, 11, 16)
}

/// S3M-canonical retrig volume multiplier (nibble 6 = `5/8`).
#[inline]
pub const fn retrig_mul_s3m(nibble: u8) -> RetrigMul {
    retrig_mul_common(nibble, 5, 8)
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
    fn xm_vibrato_depth_amiga_matches_ft2_peak() {
        // FT2/Protracker `doVibrato` peak period amplitude is
        // `(vibratoTab[16] * depth) >> 5` with `vibratoTab[16] = 255`
        // (the sine table's max), i.e. `(255 * nibble) >> 5` Paula-period
        // units. Our Amiga depth must reproduce that exactly so the
        // player's `period ± depth*wave` matches the real replayer.
        for n in 0u8..16 {
            let expect = ((255i32 * n as i32) >> 5) as i16;
            assert_eq!(
                xm_vibrato_depth(n, FrequencyType::AmigaFrequencies, false).as_q8_8_i16(),
                expect,
                "amiga vibrato depth nibble {n}"
            );
        }
        // Largest nibble (15) → ±119 period units, the FT2 maximum.
        assert_eq!(
            xm_vibrato_depth(15, FrequencyType::AmigaFrequencies, false).as_q8_8_i16(),
            119
        );
    }

    #[test]
    fn xm_vibrato_depth_protracker_is_quarter_of_ft2() {
        // ProTracker `vibrato2` does `(vibratoTable[pos] * depth) >> 7`
        // (`pt2_replayer.c:755`) — exactly 4× shallower than FT2's `>> 5`,
        // since both use the same 0..255 sine table. A `.mod` must replay
        // with this depth, not FT2's, or the vibrato is 4× too strong.
        for n in 0u8..16 {
            let expect = ((255i32 * n as i32) >> 7) as i16;
            assert_eq!(
                xm_vibrato_depth(n, FrequencyType::AmigaFrequencies, true).as_q8_8_i16(),
                expect,
                "protracker vibrato depth nibble {n}"
            );
        }
        // Largest nibble (15) → ±29 period units, the ProTracker maximum.
        assert_eq!(
            xm_vibrato_depth(15, FrequencyType::AmigaFrequencies, true).as_q8_8_i16(),
            29
        );
        // Linear is FT2-only; `protracker` must not change it.
        for n in 0u8..16 {
            assert_eq!(
                xm_vibrato_depth(n, FrequencyType::LinearFrequencies, true).as_q8_8_i16(),
                xm_vibrato_depth(n, FrequencyType::LinearFrequencies, false).as_q8_8_i16(),
            );
        }
    }

    #[test]
    fn xm_vibrato_depth_linear_is_double_old_scale() {
        // Linear depth is `nibble/8` semitone — twice the historical
        // `nibble/16`, which was half of FT2's `(255*nibble)>>5 ≈
        // nibble/8 st` linear depth.
        for n in 0u8..16 {
            assert_eq!(
                xm_vibrato_depth(n, FrequencyType::LinearFrequencies, false).as_q8_8_i16(),
                PitchDelta::from_ratio(n as i16, 8).as_q8_8_i16(),
            );
            assert_eq!(
                xm_vibrato_depth(n, FrequencyType::LinearFrequencies, false).as_q8_8_i16(),
                2 * vibrato_depth_from_nibble_16(n).as_q8_8_i16(),
            );
        }
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
