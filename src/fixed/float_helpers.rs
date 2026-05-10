//! Float ↔ fixed-point conversions, gated behind the
//! `float-helpers` feature.
//!
//! **For the editor / desktop side only.** The audio hot path
//! never sees `f32`; this module exists to bridge the
//! fixed-point world with user-facing GUIs, JSON
//! serialisation, and any other context where a human-friendly
//! `f32` is the right currency.
//!
//! On the embedded build, leave the feature off and this whole
//! module disappears — no `f32` arithmetic in the binary, no
//! pull-in of `compiler-builtins`' soft-float routines.
//!
//! # Convention
//!
//! Conversions follow the standard DSP mapping with a scale
//! factor of `2^FRAC` (not `2^FRAC - 1`):
//!
//! * `Q15::NEG_ONE` ↔ exactly `-1.0`
//! * `Q15::ZERO`    ↔ exactly `0.0`
//! * `Q15::HALF`    ↔ exactly `0.5`
//! * `Q15::ONE`     ↔ `+0.99997` (= `1.0 − 2⁻¹⁵`)
//!
//! `from_f32` clamps to the representable range (so passing
//! `1.0` to `Volume::from_f32` yields `Volume::FULL`).
//!
//! # Round-trip discipline
//!
//! A `Q → f32 → Q` round-trip is **not** the identity: it
//! introduces up to one LSB of drift per pass. Treat `f32` as
//! a *view* of a `Q` value, never as the source of truth. UIs
//! should:
//!
//! 1. Display the current `Q` value via `to_f32()`.
//! 2. When the user edits a value, call `from_f32()` **once**
//!    and store the resulting `Q`.
//! 3. Subsequent edits without a numeric change must reuse
//!    the stored `Q`, not re-derive it from the displayed
//!    `f32`.

use crate::fixed::fixed::{Q15, Q16_16, Q24_8, Q7_25, Q8_8};
use crate::fixed::units::{
    Amp, Amplification, ChannelVolume, EnvValue, Finetune, Frequency, GlobalVolume, Panning,
    Period, Pitch, PitchDelta, RetrigMul, SampleStep, Volume,
};

// =====================================================================
// Internal helpers — saturating / rounding f32 → integer
// =====================================================================

/// Round-to-nearest-even for `f32`, then saturate into an `i32`
/// range. Used as the common path for every signed conversion.
#[inline]
fn f32_to_i32_sat(x: f32, lo: i32, hi: i32) -> i32 {
    if x.is_nan() {
        return 0;
    }
    let r = libm_round(x);
    if r <= lo as f32 {
        lo
    } else if r >= hi as f32 {
        hi
    } else {
        r as i32
    }
}

/// Same idea for unsigned. Negative inputs clamp to 0.
#[inline]
fn f32_to_u32_sat(x: f32, hi: u32) -> u32 {
    if x.is_nan() || x <= 0.0 {
        return 0;
    }
    let r = libm_round(x);
    if r >= hi as f32 {
        hi
    } else {
        r as u32
    }
}

/// Round to nearest, ties away from zero. Uses only operations
/// available in `core` so it works on both `std` and `no_std`
/// builds. Behaviour on NaN is "returns 0.0" — callers that
/// care must check `is_nan` before invoking.
#[inline]
fn libm_round(x: f32) -> f32 {
    // Cast through i64 to truncate toward zero.
    // For x in [i64::MIN, i64::MAX] this is well-defined; our
    // callers have already clamped to i32 range.
    if x >= 0.0 {
        let truncated = x as i64 as f32;
        let frac = x - truncated;
        if frac >= 0.5 {
            truncated + 1.0
        } else {
            truncated
        }
    } else if x < 0.0 {
        let truncated = x as i64 as f32;
        let frac = x - truncated; // ≤ 0.0
        if frac <= -0.5 {
            truncated - 1.0
        } else {
            truncated
        }
    } else {
        // NaN falls here. Return 0.0 — callers should have
        // filtered NaN earlier anyway.
        0.0
    }
}

/// Clamp `x` to `[lo, hi]`. NaN maps to `lo`. Uses only `core`
/// operations.
///
/// Note that `f32::clamp` exists in `std` since Rust 1.50 and
/// in `core` since 1.87, but our minimum supported Rust is
/// 1.83 and we want this to work in `no_std + float-helpers`
/// builds, so we keep a hand-rolled version. The `is_nan ||
/// x < lo` ordering matters: `NaN < lo` is `false`, so without
/// the explicit `is_nan` check NaN would fall through to the
/// `x` arm and propagate.
#[inline]
fn clamp_f32(x: f32, lo: f32, hi: f32) -> f32 {
    if x.is_nan() || x < lo {
        lo
    } else if x > hi {
        hi
    } else {
        x
    }
}

// =====================================================================
// Q-format primitives
// =====================================================================

impl Q15 {
    /// Convert to `f32` in `[-1.0, +1.0)`.
    #[inline]
    pub fn to_f32(self) -> f32 {
        self.raw() as f32 / 32768.0
    }

    /// Convert from `f32`, clamping into representable range.
    /// `+1.0` saturates to `Q15::ONE` (`+0.99997`); `-1.0` is
    /// represented exactly as `Q15::NEG_ONE`.
    #[inline]
    pub fn from_f32(x: f32) -> Self {
        let scaled = x * 32768.0;
        Self::from_raw(f32_to_i32_sat(scaled, -32768, 32767) as i16)
    }
}

impl Q8_8 {
    /// Convert to `f32`.
    #[inline]
    pub fn to_f32(self) -> f32 {
        self.raw() as f32 / 256.0
    }

    /// Convert from `f32`, clamping to `[-128, +127.996]`.
    #[inline]
    pub fn from_f32(x: f32) -> Self {
        let scaled = x * 256.0;
        Self::from_raw(f32_to_i32_sat(scaled, -32768, 32767) as i16)
    }
}

impl Q24_8 {
    /// Convert to `f32`.
    #[inline]
    pub fn to_f32(self) -> f32 {
        self.raw() as f32 / 256.0
    }

    /// Convert from `f32`, clamping to the i32 range divided
    /// by 256.
    #[inline]
    pub fn from_f32(x: f32) -> Self {
        let scaled = x * 256.0;
        let r = if x.is_nan() {
            0
        } else if scaled <= i32::MIN as f32 {
            i32::MIN
        } else if scaled >= i32::MAX as f32 {
            i32::MAX
        } else {
            libm_round(scaled) as i32
        };
        Q24_8::from_raw(r)
    }
}

impl Q16_16 {
    /// Convert to `f32`.
    #[inline]
    pub fn to_f32(self) -> f32 {
        self.raw() as f32 / 65536.0
    }

    /// Convert from `f32`, clamping into `[0, 65 535.99…]`.
    #[inline]
    pub fn from_f32(x: f32) -> Self {
        let scaled = x * 65536.0;
        Self::from_raw(f32_to_u32_sat(scaled, u32::MAX))
    }
}

impl Q7_25 {
    /// Convert to `f32`.
    #[inline]
    pub fn to_f32(self) -> f32 {
        // `(1u32 << 25)` is exactly representable in f32.
        self.raw() as f32 / 33_554_432.0
    }

    /// Convert from `f32`, clamping into `[0, ~127.999…]`.
    #[inline]
    pub fn from_f32(x: f32) -> Self {
        let scaled = x * 33_554_432.0;
        Self::from_raw(f32_to_u32_sat(scaled, u32::MAX))
    }
}

// =====================================================================
// Domain newtypes — gain stage
// =====================================================================

impl Amp {
    /// Convert to `f32` in `[-1.0, +1.0)`.
    #[inline]
    pub fn to_f32(self) -> f32 {
        self.into_q15().to_f32()
    }

    /// Convert from `f32`, clamping to `[-1.0, +1.0)`.
    #[inline]
    pub fn from_f32(x: f32) -> Self {
        Amp::from_q15(Q15::from_f32(x))
    }
}

impl Volume {
    /// Convert to `f32` in `[0.0, 1.0]`.
    #[inline]
    pub fn to_f32(self) -> f32 {
        self.raw().to_f32()
    }

    /// Convert from `f32`, clamping to `[0.0, 1.0]`.
    #[inline]
    pub fn from_f32(x: f32) -> Self {
        Volume::from_q15(Q15::from_f32(clamp_f32(x, 0.0, 1.0)))
    }
}

impl ChannelVolume {
    /// Convert to `f32` in `[0.0, 1.0]`.
    #[inline]
    pub fn to_f32(self) -> f32 {
        self.raw().to_f32()
    }

    /// Convert from `f32`, clamping to `[0.0, 1.0]`.
    #[inline]
    pub fn from_f32(x: f32) -> Self {
        let clamped = clamp_f32(x, 0.0, 1.0);
        ChannelVolume::from_q15(Q15::from_f32(clamped))
    }
}

impl EnvValue {
    /// Convert to `f32`.
    ///
    /// Range is `[-1, +1)` in raw Q1.15 terms (volume
    /// envelopes only use `[0, 1]`, but pan envelopes in some
    /// formats use the full signed range).
    #[inline]
    pub fn to_f32(self) -> f32 {
        self.raw_q15().to_f32()
    }

    /// Convert from `f32`, clamping to `[-1.0, +1.0)`.
    #[inline]
    pub fn from_f32(x: f32) -> Self {
        EnvValue::from_q15(Q15::from_f32(x))
    }
}

impl GlobalVolume {
    /// Convert to `f32` in `[0.0, 1.0]`.
    #[inline]
    pub fn to_f32(self) -> f32 {
        // GlobalVolume's underlying Q15 is private; route
        // through `applied_to(Amp::SILENCE)`-style trick is
        // ugly. Add a tiny accessor in `units.rs` (see patch
        // below) so this method becomes a one-liner.
        self.raw_q15().to_f32()
    }

    /// Convert from `f32`, clamping to `[0.0, 1.0]`.
    #[inline]
    pub fn from_f32(x: f32) -> Self {
        let clamped = clamp_f32(x, 0.0, 1.0);
        GlobalVolume::from_q15(Q15::from_f32(clamped))
    }
}

impl Amplification {
    /// Convert to `f32`. Range is `[-8.0, +8.0)`.
    #[inline]
    pub fn to_f32(self) -> f32 {
        self.raw_q4_12() as f32 / 4096.0
    }

    /// Convert from `f32`, clamping to `[-8.0, +8.0)`.
    /// Replaces the previous `Amplification::from_linear`.
    #[inline]
    pub fn from_f32(x: f32) -> Self {
        let scaled = x * 4096.0;
        Amplification::from_raw_q4_12(
            f32_to_i32_sat(scaled, i16::MIN as i32, i16::MAX as i32) as i16
        )
    }
}

impl RetrigMul {
    /// Convert to `f32`. Range is `[-4.0, +4.0)`.
    #[inline]
    pub fn to_f32(self) -> f32 {
        self.raw_q3_13() as f32 / 8192.0
    }

    /// Convert from `f32`, clamping to `[-4.0, +4.0)`.
    #[inline]
    pub fn from_f32(x: f32) -> Self {
        let scaled = x * 8192.0;
        RetrigMul::from_raw_q3_13(f32_to_i32_sat(scaled, i16::MIN as i32, i16::MAX as i32) as i16)
    }
}

// =====================================================================
// Panning
// =====================================================================

impl Panning {
    /// Convert to `f32` in `[0.0, 1.0]` (`0.0` = full left,
    /// `1.0` = full right).
    #[inline]
    pub fn to_f32(self) -> f32 {
        self.raw().to_f32()
    }

    /// Convert from `f32`, clamping to `[0.0, 1.0]`.
    #[inline]
    pub fn from_f32(x: f32) -> Self {
        let clamped = clamp_f32(x, 0.0, 1.0);
        Panning::from_q15(Q15::from_f32(clamped))
    }
}

// =====================================================================
// Pitch chain
// =====================================================================

impl Pitch {
    /// Convert to `f32` semitone count.
    #[inline]
    pub fn to_f32(self) -> f32 {
        self.raw().to_f32()
    }

    /// Convert from `f32` semitones, clamping to `[0, 119]`.
    #[inline]
    pub fn from_f32(x: f32) -> Self {
        let clamped = clamp_f32(x, 0.0, 119.0);
        Pitch::from_q8_8(Q8_8::from_f32(clamped))
    }
}

impl PitchDelta {
    /// Convert to `f32` semitone offset.
    #[inline]
    pub fn to_f32(self) -> f32 {
        self.raw().to_f32()
    }

    /// Convert from `f32`, clamping to the Q8.8 range.
    #[inline]
    pub fn from_f32(x: f32) -> Self {
        PitchDelta::from_q8_8(Q8_8::from_f32(x))
    }
}

impl Finetune {
    /// Convert to `f32` in `[-1.0, +1.0)` (one full semitone
    /// in either direction).
    #[inline]
    pub fn to_f32(self) -> f32 {
        self.into_q15().to_f32()
    }

    /// Convert from `f32`, clamping to `[-1.0, +1.0)`.
    #[inline]
    pub fn from_f32(x: f32) -> Self {
        Finetune::from_q15(Q15::from_f32(x))
    }
}

// =====================================================================
// Time / period / frequency
// =====================================================================

impl Period {
    /// Convert to `f32`. Period is dimensionless.
    #[inline]
    pub fn to_f32(self) -> f32 {
        self.raw() as f32
    }

    /// Convert from `f32`, clamping to `[0, 65535]`.
    #[inline]
    pub fn from_f32(x: f32) -> Self {
        Period::from_raw(f32_to_u32_sat(x, u16::MAX as u32) as u16)
    }
}

impl Frequency {
    /// Convert to `f32` Hz. Q24.8 raw `/ 256.0`.
    #[inline]
    pub fn to_f32(self) -> f32 {
        self.raw_q24_8() as f32 / 256.0
    }

    /// Convert from `f32` Hz. Clamps to `[0, ~16.7 MHz]` (the
    /// Q24.8 integer-part ceiling — well above any tracker
    /// playback frequency).
    #[inline]
    pub fn from_f32(x: f32) -> Self {
        if x <= 0.0 || x.is_nan() {
            return Frequency::ZERO;
        }
        let scaled = x * 256.0;
        if scaled >= u32::MAX as f32 {
            Frequency::from_raw_q24_8(u32::MAX)
        } else {
            Frequency::from_raw_q24_8(scaled as u32)
        }
    }
}

impl SampleStep {
    /// Convert to `f32` (fractional samples per output frame).
    #[inline]
    pub fn to_f32(self) -> f32 {
        self.into_q7_25().to_f32()
    }

    /// Convert from `f32`, clamping to the Q7.25 range.
    #[inline]
    pub fn from_f32(x: f32) -> Self {
        SampleStep::from_raw_q7_25(Q7_25::from_f32(x))
    }
}

// =====================================================================
// Tests (only built when both feature and `cfg(test)` are on)
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ------- Q-format round-trips -------

    #[test]
    fn q15_endpoints() {
        assert_eq!(Q15::NEG_ONE.to_f32(), -1.0);
        assert_eq!(Q15::ZERO.to_f32(), 0.0);
        assert_eq!(Q15::HALF.to_f32(), 0.5);
        // ONE is 1 LSB below 1.0
        assert!((Q15::ONE.to_f32() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn q15_round_trip_lossy_within_one_lsb() {
        for raw in [-32768, -16384, -1, 0, 1, 16384, 32767] {
            let q = Q15::from_raw(raw);
            let q2 = Q15::from_f32(q.to_f32());
            assert!(
                (q2.raw() - q.raw()).abs() <= 1,
                "raw {} drifted to {}",
                raw,
                q2.raw()
            );
        }
    }

    #[test]
    fn q15_from_f32_saturation() {
        assert_eq!(Q15::from_f32(2.0), Q15::ONE);
        assert_eq!(Q15::from_f32(-2.0), Q15::NEG_ONE);
        assert_eq!(Q15::from_f32(f32::NAN), Q15::ZERO);
        assert_eq!(Q15::from_f32(f32::INFINITY), Q15::ONE);
        assert_eq!(Q15::from_f32(f32::NEG_INFINITY), Q15::NEG_ONE);
    }

    #[test]
    fn q8_8_round_trip() {
        for v in [-128.0_f32, -1.5, 0.0, 0.5, 48.0, 119.0, 127.0] {
            let q = Q8_8::from_f32(v);
            let back = q.to_f32();
            assert!((back - v).abs() < 0.005, "{} → {}", v, back);
        }
    }

    #[test]
    fn q16_16_round_trip() {
        for hz in [0.0_f32, 1.0, 440.0, 8372.0, 22050.0, 65535.0] {
            let q = Q16_16::from_f32(hz);
            let back = q.to_f32();
            assert!((back - hz).abs() < 1e-4, "{} → {}", hz, back);
        }
    }

    #[test]
    fn q7_25_round_trip() {
        // Typical playback step values for 8 kHz to 96 kHz at 48 kHz output
        for step in [0.01_f32, 0.17, 0.5, 1.0, 2.0, 16.0] {
            let q = Q7_25::from_f32(step);
            let back = q.to_f32();
            assert!((back - step).abs() < 1e-6, "{} → {}", step, back);
        }
    }

    // ------- Domain newtypes -------

    #[test]
    fn volume_clamps_above_one() {
        assert_eq!(Volume::from_f32(2.0), Volume::FULL);
        assert_eq!(Volume::from_f32(1.0), Volume::FULL);
        assert!((Volume::FULL.to_f32() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn volume_clamps_below_zero() {
        assert_eq!(Volume::from_f32(-0.5), Volume::SILENT);
        assert_eq!(Volume::SILENT.to_f32(), 0.0);
    }

    #[test]
    fn panning_round_trip_centre() {
        let p = Panning::from_f32(0.5);
        assert!((p.to_f32() - 0.5).abs() < 1e-4);
        assert_eq!(p, Panning::CENTER);
    }

    #[test]
    fn panning_endpoints() {
        assert_eq!(Panning::from_f32(0.0), Panning::LEFT);
        assert_eq!(Panning::from_f32(1.0), Panning::RIGHT);
    }

    #[test]
    fn pitch_clamps_to_valid_range() {
        assert_eq!(Pitch::from_f32(-10.0), Pitch::C0);
        assert_eq!(Pitch::from_f32(200.0), Pitch::B9);
        let p = Pitch::from_f32(48.0);
        assert!((p.to_f32() - 48.0).abs() < 0.005);
    }

    #[test]
    fn finetune_round_trip() {
        for v in [-1.0_f32, -0.5, 0.0, 0.25, 0.5, 0.75] {
            let q = Finetune::from_f32(v);
            let back = q.to_f32();
            assert!((back - v).abs() < 1e-4, "{} → {}", v, back);
        }
    }

    #[test]
    fn frequency_a4_round_trip() {
        let f = Frequency::from_f32(440.0);
        let back = f.to_f32();
        assert!((back - 440.0).abs() < 0.01, "got {}", back);
    }

    #[test]
    fn amplification_unity_is_one() {
        assert!((Amplification::UNITY.to_f32() - 1.0).abs() < 1e-4);
        let amp = Amplification::from_f32(2.5);
        assert!((amp.to_f32() - 2.5).abs() < 1e-3);
    }

    #[test]
    fn retrig_mul_known_ratios() {
        // 11/16 = 0.6875
        let m = RetrigMul::from_f32(0.6875);
        assert!((m.to_f32() - 0.6875).abs() < 1e-3);
        // 2.0
        let m = RetrigMul::from_f32(2.0);
        assert!((m.to_f32() - 2.0).abs() < 1e-3);
    }

    // ------- Round-trip discipline check -------

    #[test]
    fn round_trip_drift_is_bounded() {
        // Doing N round-trips Q→f32→Q should not drift more
        // than ~N LSB. We check that at N=1 it's ≤ 1 LSB.
        let mut q = Q15::from_raw(12345);
        for _ in 0..1 {
            q = Q15::from_f32(q.to_f32());
        }
        assert!((q.raw() - 12345).abs() <= 1);
    }
}
