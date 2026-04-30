//! Generic fixed-point Q-format primitives.
//!
//! These types know nothing about audio: they are just integers
//! that have agreed on where the decimal point is. All domain
//! types in [`crate::fixed::units`] wrap one of these.
//!
//! # Notation
//!
//! `Qm.n` means: `m` integer bits, `n` fractional bits, plus a
//! sign bit if the underlying type is signed. So `Q1.15` on
//! `i16` covers approximately `[-1.0, +1.0)` with a step of
//! `2⁻¹⁵ ≈ 3.05e-5`.
//!
//! # Multiplication and rounding
//!
//! Every multiplication that goes through this module uses
//! **round-to-nearest** (with the bias `+ (1 << (FRAC - 1))`)
//! and **saturating** widening. A naive truncation would
//! introduce a slow DC drift through the long gain cascade
//! `volume × channel_volume × env × fadeout × global × amp`.

#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_lossless)]

use serde::{Deserialize, Serialize};

// =====================================================================
// Q1.15 — signed i16, [-1.0, +1.0)
// Used for: audio samples, gains (volume / panning / envelope),
// LFO output, finetune within one semitone.
// =====================================================================

/// Q1.15 signed fixed-point on `i16`.
///
/// One sign bit, fifteen fractional bits. Range
/// `[-1.0, +0.99997]`, step `≈ 3.05e-5` (about -90 dBFS).
///
/// `Q15::ONE` represents the largest positive value
/// (`0x7FFF`), not exactly `+1.0`. Multiplications by
/// `Q15::ONE` are therefore *almost* the identity — for the
/// audio path this is well below the quantisation floor and
/// has no audible consequence.
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord, Debug, Default)]
#[repr(transparent)]
#[serde(transparent)]
pub struct Q15(i16);

impl Q15 {
    /// Zero.
    pub const ZERO: Self = Self(0);
    /// Largest positive Q1.15 value (`0x7FFF`, ≈ +0.99997).
    pub const ONE: Self = Self(0x7FFF);
    /// Most negative Q1.15 value (`-0x8000`, exactly -1.0).
    pub const NEG_ONE: Self = Self(-0x8000);
    /// `0.5` exactly (`0x4000`).
    pub const HALF: Self = Self(0x4000);
    /// `-0.5` exactly (`-0x4000`).
    pub const NEG_HALF: Self = Self(-0x4000);
    /// `0.25` exactly (`0x2000`).
    pub const QUARTER: Self = Self(0x2000);
    /// `-0.25` exactly (`-0x2000`).
    pub const NEG_QUARTER: Self = Self(-0x2000);

    /// Build from a signed numerator and denominator at compile
    /// time. Saturates to `[NEG_ONE, ONE]` if the resulting
    /// value is out of range.
    ///
    /// Both arguments are `i32` — wide enough for raw envelope
    /// frame counts (`usize → i32`) as well as the byte/nibble
    /// ratios in the importer. The internal accumulator widens
    /// to `i64` so `(num << 15) / den` cannot overflow.
    #[inline]
    pub const fn from_ratio(num: i32, den: i32) -> Self {
        // Compute `(num << 15) / den`, with sign-aware saturation.
        if den == 0 {
            return if num >= 0 { Self::ONE } else { Self::NEG_ONE };
        }
        let scaled = (num as i64) << 15;
        let q = scaled / den as i64;
        if q > 0x7FFF {
            Self::ONE
        } else if q < -0x8000 {
            Self::NEG_ONE
        } else {
            Self(q as i16)
        }
    }

    /// Construct from a raw `i16`. Internal use only — the
    /// caller is responsible for the bit pattern.
    #[inline]
    pub const fn from_raw(raw: i16) -> Self {
        Self(raw)
    }

    /// Underlying `i16`.
    #[inline(always)]
    pub const fn raw(self) -> i16 {
        self.0
    }

    /// Q1.15 × Q1.15 → Q1.15, round-to-nearest, saturating.
    ///
    /// Implemented as `(a * b + (1 << 14)) >> 15` in a
    /// widened `i32` accumulator. The only saturation case is
    /// `Q15::NEG_ONE * Q15::NEG_ONE`, which would compute to
    /// `+1.0` and is clamped to `Q15::ONE`.
    #[inline(always)]
    pub const fn mul(self, other: Self) -> Self {
        let prod = (self.0 as i32) * (other.0 as i32);
        // Round-to-nearest bias (ties away from zero, which is
        // fine for symmetric signed Q-formats).
        let prod = prod + (1 << 14);
        let shifted = prod >> 15;
        if shifted > 0x7FFF {
            Self::ONE
        } else if shifted < -0x8000 {
            Self::NEG_ONE
        } else {
            Self(shifted as i16)
        }
    }

    /// Saturating add. Clamps to `[NEG_ONE, ONE]`.
    #[inline]
    pub const fn saturating_add(self, other: Self) -> Self {
        let s = (self.0 as i32) + (other.0 as i32);
        if s > 0x7FFF {
            Self::ONE
        } else if s < -0x8000 {
            Self::NEG_ONE
        } else {
            Self(s as i16)
        }
    }

    /// Saturating sub.
    #[inline]
    pub const fn saturating_sub(self, other: Self) -> Self {
        let s = (self.0 as i32) - (other.0 as i32);
        if s > 0x7FFF {
            Self::ONE
        } else if s < -0x8000 {
            Self::NEG_ONE
        } else {
            Self(s as i16)
        }
    }

    /// Clamp to `[lo, hi]`. Behaviour is undefined (in the
    /// trivial sense) if `lo > hi`; we don't check.
    #[inline]
    pub const fn clamp(self, lo: Self, hi: Self) -> Self {
        if self.0 < lo.0 {
            lo
        } else if self.0 > hi.0 {
            hi
        } else {
            self
        }
    }

    /// Absolute value, saturating: `|NEG_ONE|` is clamped to
    /// `ONE` (because `+1.0` is not representable).
    #[inline]
    pub const fn abs(self) -> Self {
        if self.0 == i16::MIN {
            Self::ONE
        } else if self.0 < 0 {
            Self(-self.0)
        } else {
            self
        }
    }

    /// Negate, saturating.
    #[inline]
    pub const fn neg(self) -> Self {
        if self.0 == i16::MIN {
            Self::ONE
        } else {
            Self(-self.0)
        }
    }

    /// Divide by two. Exact `>> 1` (arithmetic shift on the
    /// signed `i16` raw), no rounding. The bottom LSB is
    /// truncated; the result for `Q15::NEG_ONE` (`= 0x8000`)
    /// is exactly `Q15::NEG_HALF` (`= 0xC000`).
    #[inline]
    pub const fn halved(self) -> Self {
        Self(self.0 >> 1)
    }

    // =================================================================
    // Readability helpers — keep `raw().raw() as i32`-style chains out
    // of consumer code by exposing intent-named widening accessors and
    // saturating constructors.
    //
    // None of these introduce new arithmetic; they just give a single
    // call where the bare-bones API would require two `.raw()`s and a
    // cast.
    // =================================================================

    /// Sign-extended `i32` view of the raw bits. Equivalent to
    /// `self.raw() as i32` but reads as a single concept ("widen
    /// to i32 for accumulation"), which is what the consumer
    /// typically wants.
    #[inline]
    pub const fn as_i32(self) -> i32 {
        self.0 as i32
    }

    /// Widen Q1.15 → Q15.16 raw `i32`. The numerical value
    /// stays the same: a Q1.15 raw `r` represents `r/32768`,
    /// while the corresponding Q15.16 raw is `r × 65536/32768
    /// = r × 2`. Used by the IIR / mix accumulators that need
    /// sub-Q1.15 headroom.
    #[inline(always)]
    pub const fn widen_q15_16(self) -> i32 {
        (self.0 as i32) << 1
    }

    /// Saturating constructor from `i32`. Clamps to the
    /// `[i16::MIN, i16::MAX]` range and wraps the result in a
    /// `Q15`. Used at narrowing boundaries (after MAC chains
    /// that may produce out-of-range intermediates).
    #[inline(always)]
    pub const fn from_i32_sat(v: i32) -> Self {
        if v > i16::MAX as i32 {
            Self(i16::MAX)
        } else if v < i16::MIN as i32 {
            Self(i16::MIN)
        } else {
            Self(v as i16)
        }
    }

    /// Saturating constructor from a non-negative `i32`. Clamps
    /// to `[0, 0x7FFF]`. Convenience for the unsigned-Q1.15
    /// types (`Volume`, `Panning`) whose semantic range is
    /// `[0, 1]`.
    #[inline]
    pub const fn from_i32_unsigned_sat(v: i32) -> Self {
        if v > 0x7FFF {
            Self(0x7FFF)
        } else if v < 0 {
            Self(0)
        } else {
            Self(v as i16)
        }
    }
}

// =====================================================================
// Q8.8 — signed i16, [-128.0, +128.0), step 1/256 ≈ 0.0039
// Used for: musical pitch in semitones (persistent storage).
// 1/256 of a semitone ≈ 4 cents, just below the perceptual
// threshold for absolute pitch comparison.
// =====================================================================

/// Q8.8 signed fixed-point on `i16`.
///
/// Eight integer bits, eight fractional bits. Range
/// `[-128.0, +127.996]`, step `1/256`.
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord, Debug, Default)]
#[repr(transparent)]
#[serde(transparent)]
pub struct Q8_8(i16);

impl Q8_8 {
    /// Zero.
    pub const ZERO: Self = Self(0);

    /// Build from an integer semitone count, saturating.
    #[inline]
    pub const fn from_int(n: i16) -> Self {
        if n >= 128 {
            Self(0x7F00)
        } else if n <= -128 {
            Self(-0x8000)
        } else {
            Self(n << 8)
        }
    }

    /// Construct from raw bits.
    #[inline]
    pub const fn from_raw(raw: i16) -> Self {
        Self(raw)
    }

    /// Underlying `i16`.
    #[inline(always)]
    pub const fn raw(self) -> i16 {
        self.0
    }

    /// Truncate towards zero to an integer semitone.
    #[inline]
    pub const fn trunc(self) -> i16 {
        self.0 >> 8
    }

    /// Round to the nearest integer semitone (ties away from
    /// zero — close enough to ties-to-even at this granularity
    /// and cheaper). Computed in `i32` to avoid any overflow
    /// at the `i16::MIN` boundary.
    #[inline]
    pub const fn round(self) -> i16 {
        let raw = self.0 as i32;
        let r = if raw >= 0 {
            (raw + 0x80) >> 8
        } else {
            -((-raw + 0x80) >> 8)
        };
        // Result is in [-129, 128] worst case; saturate.
        if r > i16::MAX as i32 {
            i16::MAX
        } else if r < i16::MIN as i32 {
            i16::MIN
        } else {
            r as i16
        }
    }

    /// Round towards positive infinity (ceiling) to an integer
    /// semitone. Mirror of [`Self::round`] / [`Self::trunc`];
    /// added because period adjustment under FT2's arpeggio
    /// quirk needs a *strict ceiling* on the pitch, not nearest.
    #[inline]
    pub const fn ceil_int(self) -> i16 {
        let raw = self.0 as i32;
        // `(raw + 0xFF) >> 8` is the standard "+ (denom - 1) /
        // denom" ceiling idiom, with `denom = 256` for Q8.8.
        let c = (raw + 0xFF) >> 8;
        if c > i16::MAX as i32 {
            i16::MAX
        } else if c < i16::MIN as i32 {
            i16::MIN
        } else {
            c as i16
        }
    }

    /// Sub-semitone fractional byte: the low 8 bits of the raw
    /// value, as `u8`. For positive Q8.8 values this is the
    /// `[0, 1)` semitone frac; for negative values the bit
    /// pattern is the same but the math reads as the
    /// two's-complement remainder. Callers consume only the
    /// positive-value case (Amiga sub-semitone fine index).
    #[inline]
    pub const fn frac_byte(self) -> u8 {
        self.0 as u8
    }

    /// Saturating add.
    #[inline]
    pub const fn saturating_add(self, other: Self) -> Self {
        let s = (self.0 as i32) + (other.0 as i32);
        if s > 0x7FFF {
            Self(0x7FFF)
        } else if s < -0x8000 {
            Self(-0x8000)
        } else {
            Self(s as i16)
        }
    }

    /// Saturating sub.
    #[inline]
    pub const fn saturating_sub(self, other: Self) -> Self {
        let s = (self.0 as i32) - (other.0 as i32);
        if s > 0x7FFF {
            Self(0x7FFF)
        } else if s < -0x8000 {
            Self(-0x8000)
        } else {
            Self(s as i16)
        }
    }

    /// Build from a signed numerator / denominator at compile
    /// time. Saturates to `[-128.0, +127.996]` on overflow.
    /// Replaces `num as f32 / den as f32` everywhere in the
    /// importer / domain code.
    ///
    /// Both arguments are `i16` — wide enough for every call
    /// site in the importer (vibrato/tremolo speed/depth/sweep
    /// nibbles and bytes, capped at `255`). Q8.8 is itself an
    /// `i16` storage; allowing larger sources here would only
    /// invite truncation later. Sister method [`Q15::from_ratio`]
    /// uses `i32` because envelope ratios start from `usize`
    /// frame counts — those don't reach Q8.8.
    #[inline]
    pub const fn from_ratio(num: i16, den: i16) -> Self {
        if den == 0 {
            return if num >= 0 {
                Self(0x7FFF)
            } else {
                Self(-0x8000)
            };
        }
        // `(num << 8) / den` in widened i32 (or i64) to avoid
        // overflow on the multiply step.
        let scaled = (num as i32) << 8;
        let q = scaled / den as i32;
        if q > 0x7FFF {
            Self(0x7FFF)
        } else if q < -0x8000 {
            Self(-0x8000)
        } else {
            Self(q as i16)
        }
    }

    /// Inverse of [`Self::from_ratio`] for non-negative values
    /// at byte scale. Returns `round(self * den)` clamped into
    /// `0..=255`. Replaces `(value * den as f32) as u8` in the
    /// exporter, with proper round-to-nearest instead of the
    /// previous truncation.
    ///
    /// Example: `q.to_ratio_byte(252)` is the inverse of
    /// `from_ratio(byte, 252)` for any `byte ∈ 0..=255`.
    ///
    /// For negative values the result clamps to `0` — callers
    /// that need a signed export should read [`Self::raw`]
    /// directly and handle the sign themselves.
    #[inline]
    pub const fn to_ratio_byte(self, den: u32) -> u8 {
        let raw = self.0 as i64;
        if raw <= 0 {
            return 0;
        }
        // Widened to i64 so that any reasonable `den` is safe
        // against overflow even when `raw` is near `i16::MAX`.
        // The resulting `rounded` is clamped to `0..=255`.
        let scaled = raw * den as i64;
        let rounded = (scaled + 128) >> 8;
        if rounded > 255 {
            255
        } else {
            rounded as u8
        }
    }

    // =================================================================
    // Readability helpers (mirror Q15's set).
    // =================================================================

    /// Sign-extended `i32` view of the raw bits. Equivalent to
    /// `self.raw() as i32`. Use when accumulating Q8.8 deltas
    /// in a widened `i32` register (typical: pitch composition
    /// chains adding several `PitchDelta` together).
    #[inline]
    pub const fn as_i32(self) -> i32 {
        self.0 as i32
    }

    /// Saturating constructor from `i32`. Clamps to the i16
    /// boundaries (`±128` semitones in this Q8.8 storage,
    /// already far outside any tracker range).
    #[inline(always)]
    pub const fn from_i32_sat(v: i32) -> Self {
        if v > i16::MAX as i32 {
            Self(i16::MAX)
        } else if v < i16::MIN as i32 {
            Self(i16::MIN)
        } else {
            Self(v as i16)
        }
    }

    /// Build directly from an integer count of semitones, in
    /// Q8.8 raw form. Equivalent to `Q8_8::from_int(n)` but
    /// reads better at the call site when the input is "just
    /// the integer 12 semitones" rather than "a value to
    /// saturate against `[-128, +128)`".
    ///
    /// (Aliases `from_int` because the underlying scale is the
    /// same — kept distinct for readability.)
    #[inline]
    pub const fn from_semitones_int(n: i16) -> Self {
        Self::from_int(n)
    }
}

// =====================================================================
// Q24.8 — signed i32, used as a wide pitch accumulator
// =====================================================================

/// Q24.8 signed fixed-point on `i32`.
///
/// Twenty-four integer bits, eight fractional bits. Used as
/// the intermediate accumulator for the pitch chain
/// `note + arpeggio + finetune + vibrato_modulation` so that
/// the cumulative rounding error on the final pitch stays
/// well below one cent.
///
/// Never stored persistently — always collapses back to
/// [`Q8_8`] before being written into a struct.
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord, Debug, Default)]
#[repr(transparent)]
#[serde(transparent)]
pub struct Q24_8(i32);

impl Q24_8 {
    /// Zero.
    pub const ZERO: Self = Self(0);

    /// Widen a [`Q8_8`] into a [`Q24_8`].
    #[inline]
    pub const fn from_q8_8(p: Q8_8) -> Self {
        Self(p.raw() as i32)
    }

    /// Narrow back to [`Q8_8`], saturating.
    #[inline]
    pub const fn to_q8_8_saturating(self) -> Q8_8 {
        if self.0 > 0x7FFF {
            Q8_8::from_raw(0x7FFF)
        } else if self.0 < -0x8000 {
            Q8_8::from_raw(-0x8000)
        } else {
            Q8_8::from_raw(self.0 as i16)
        }
    }

    /// Saturating add.
    #[inline]
    pub const fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    /// Saturating sub.
    #[inline]
    pub const fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }

    /// Add a Q1.15 value interpreted as a fraction-of-a-semitone.
    /// Conversion is `q15 >> 7` (so `Q15::ONE` ≈ `+1` semitone).
    #[inline]
    pub const fn add_finetune_q15(self, ft: Q15) -> Self {
        Self(self.0 + ((ft.raw() as i32) >> 7))
    }

    /// Underlying `i32`.
    #[inline(always)]
    pub const fn raw(self) -> i32 {
        self.0
    }

    /// Construct from raw bits.
    #[inline]
    pub const fn from_raw(raw: i32) -> Self {
        Self(raw)
    }
}

// =====================================================================
// Q16.16 — unsigned u32, [0, 65536), step ~1.5e-5
// Used for: frequencies in Hz.
// =====================================================================

/// Q16.16 unsigned fixed-point on `u32`.
///
/// Sixteen integer bits, sixteen fractional bits. Range
/// `[0, 65536)`, step `2⁻¹⁶ ≈ 1.5e-5`.
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord, Debug, Default)]
#[repr(transparent)]
#[serde(transparent)]
pub struct Q16_16(u32);

impl Q16_16 {
    /// Zero.
    pub const ZERO: Self = Self(0);

    /// Build from an integer in Hz.
    #[inline]
    pub const fn from_int(hz: u16) -> Self {
        Self((hz as u32) << 16)
    }

    /// Build from raw bits.
    #[inline]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Underlying `u32`.
    #[inline(always)]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Multiply by `2^octaves`, saturating. Negative shifts
    /// down (lower octave), positive shifts up.
    #[inline]
    pub const fn shift_octave(self, octaves: i8) -> Self {
        if octaves >= 0 {
            // Saturate on overflow.
            let shift = octaves as u32;
            if shift >= 32 {
                Self(u32::MAX)
            } else {
                let shifted = (self.0 as u64) << shift;
                if shifted > u32::MAX as u64 {
                    Self(u32::MAX)
                } else {
                    Self(shifted as u32)
                }
            }
        } else {
            let shift = (-octaves) as u32;
            if shift >= 32 {
                Self(0)
            } else {
                Self(self.0 >> shift)
            }
        }
    }

    /// Q16.16 × Q16.16 → Q16.16, round-to-nearest, saturating.
    /// Used to compose sub-octave multipliers in the
    /// period→frequency lookup.
    #[inline(always)]
    pub const fn mul_q16_16(self, other: Self) -> Self {
        let prod = (self.0 as u64) * (other.0 as u64);
        // Round-to-nearest bias.
        let prod = prod + (1u64 << 15);
        let r = prod >> 16;
        if r > u32::MAX as u64 {
            Self(u32::MAX)
        } else {
            Self(r as u32)
        }
    }

    /// Saturating add.
    #[inline]
    pub const fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }
}

// =====================================================================
// Q7.25 — unsigned u32, [0, 128), step ~3e-8
// Used for: sample-playback step (kept identical to the
// existing M = 25 fractional bits in `state_sample`).
// =====================================================================

/// Q7.25 unsigned fixed-point on `u32`. Sample-playback step
/// and accumulated phase. The fractional resolution
/// (`2⁻²⁵ ≈ 3e-8`) is what keeps pitched playback artefact-free.
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord, Debug, Default)]
#[repr(transparent)]
#[serde(transparent)]
pub struct Q7_25(u32);

impl Q7_25 {
    /// Number of fractional bits — kept public for the player
    /// crate which still needs to do the integer / fractional
    /// split by hand on the playback position.
    pub const FRAC_BITS: u32 = 25;
    /// Mask of the fractional bits.
    pub const FRAC_MASK: u32 = (1 << Self::FRAC_BITS) - 1;

    /// Zero.
    pub const ZERO: Self = Self(0);

    /// Build from raw bits.
    #[inline]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Underlying `u32`.
    #[inline(always)]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

// =====================================================================
// OPERATOR OVERLOADING
//
// Audio Q-format types use the `core::ops::{Neg, Add, Sub, Mul}`
// traits with **saturating** semantics by convention (audio
// arithmetic clips, it does not wrap). The named methods
// (`saturating_add`, `mul`, `neg`) are kept and documented; the
// operator forms simply forward to them.
//
// Why saturating and not wrapping (Rust's release default)?
// Because wrap-around in audio means a near-clip sample
// suddenly becomes a near-`-1.0` impulse — a click that was
// not in the source. Clipping is the standard analogue
// behaviour and what every tracker engine does.
//
// `*` for Q15 is round-to-nearest (Q1.15 × Q1.15 in `i32`,
// `+ (1 << 14)` bias, `>> 15`, saturate). Same as the existing
// `Q15::mul`.
// =====================================================================

impl core::ops::Neg for Q15 {
    type Output = Self;
    /// Saturating sign-flip. `Q15::NEG_ONE` (i.e. `-1.0`)
    /// would naively negate to `+1.0` which doesn't fit in
    /// Q1.15; it clips to `Q15::ONE` (`≈ +0.99997`).
    #[inline]
    fn neg(self) -> Self {
        Q15::neg(self)
    }
}

impl core::ops::Add for Q15 {
    type Output = Self;
    /// Saturating add. Overflow clips to `Q15::ONE`,
    /// underflow to `Q15::NEG_ONE`.
    #[inline]
    fn add(self, rhs: Self) -> Self {
        self.saturating_add(rhs)
    }
}

impl core::ops::Sub for Q15 {
    type Output = Self;
    /// Saturating subtract.
    #[inline]
    fn sub(self, rhs: Self) -> Self {
        self.saturating_sub(rhs)
    }
}

impl core::ops::Mul for Q15 {
    type Output = Self;
    /// Q1.15 × Q1.15 → Q1.15, round-to-nearest, saturating.
    /// Same as [`Q15::mul`].
    #[inline]
    fn mul(self, rhs: Self) -> Self {
        Q15::mul(self, rhs)
    }
}

impl core::ops::Neg for Q8_8 {
    type Output = Self;
    /// Saturating sign-flip on Q8.8. `i16::MIN` clips to
    /// `i16::MAX` (the only ambiguous case).
    #[inline]
    fn neg(self) -> Self {
        if self.0 == i16::MIN {
            Self(i16::MAX)
        } else {
            Self(-self.0)
        }
    }
}

impl core::ops::Add for Q8_8 {
    type Output = Self;
    /// Saturating add (clips at i16 boundaries — far outside
    /// the musical pitch range, but defined for safety).
    #[inline]
    fn add(self, rhs: Self) -> Self {
        self.saturating_add(rhs)
    }
}

impl core::ops::Sub for Q8_8 {
    type Output = Self;
    /// Saturating subtract.
    #[inline]
    fn sub(self, rhs: Self) -> Self {
        self.saturating_sub(rhs)
    }
}

// =====================================================================
// COMPILE-TIME f32-LITERAL → Q FORMAT MACROS
//
// The cast `f32 as i16` is not yet stable in `const` context
// on stable Rust (1.82 stabilized const float arithmetic but
// not the float-to-int cast). The macros below use a literal
// at expansion time: when called with a const-foldable input
// (a numeric literal), LLVM constant-folds the multiply and
// cast at compile time, leaving zero `f32` instructions in
// the embedded binary at `-Copt-level=2/3`.
//
// At debug optimisation levels (`-Copt-level=0/1`) a fold is
// not guaranteed; the recommended embedded build profile is
// `release` (the project's `[profile.release]` already enables
// `lto = true`).
//
// For values that need a *guaranteed* zero-`f32` build at any
// opt level, use the integer constructors instead:
//
//   * `Q15::from_ratio(num, den)` — exact rational, e.g.
//     `Q15::from_ratio(1, 2)` for `0.5`.
//   * Named constants: `Q15::HALF`, `Q15::QUARTER`, etc.
// =====================================================================

/// Q1.15 from an `f32` literal. **Compile-time foldable** at
/// release opt level.
///
/// ```
/// # use xmrs::q15;
/// # use xmrs::fixed::fixed::Q15;
/// let half = q15!(0.5);
/// assert_eq!(half, Q15::HALF);
/// ```
#[macro_export]
macro_rules! q15 {
    ($x:expr) => {
        $crate::fixed::fixed::Q15::from_raw(($x * 32768.0) as i16)
    };
}

/// Q8.8 from an `f32` literal. Same compile-time-fold caveat
/// as [`q15!`].
#[macro_export]
macro_rules! q8_8 {
    ($x:expr) => {
        $crate::fixed::fixed::Q8_8::from_raw(($x * 256.0) as i16)
    };
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- Q15 -------------------------------------------

    #[test]
    fn q15_one_times_one_does_not_overflow() {
        // 0x7FFF * 0x7FFF rounded = 0x7FFE, not a wrap.
        let p = Q15::ONE.mul(Q15::ONE);
        assert!(p.raw() >= 0x7FFE);
    }

    #[test]
    fn q15_neg_one_squared_saturates_to_one() {
        // -1.0 * -1.0 = +1.0, must saturate to ONE.
        let p = Q15::NEG_ONE.mul(Q15::NEG_ONE);
        assert_eq!(p, Q15::ONE);
    }

    #[test]
    fn q15_half_times_half_is_quarter() {
        let p = Q15::HALF.mul(Q15::HALF);
        // Expect 0x2000 ± 1 rounding bit
        assert!((p.raw() - 0x2000).abs() <= 1);
    }

    #[test]
    fn q15_from_ratio_basic() {
        assert_eq!(Q15::from_ratio(1, 2), Q15::HALF);
        assert_eq!(Q15::from_ratio(0, 1), Q15::ZERO);
        // 64 / 64 = 1.0, saturates to ONE
        assert_eq!(Q15::from_ratio(64, 64), Q15::ONE);
        // -1.0 representable exactly
        assert_eq!(Q15::from_ratio(-1, 1), Q15::NEG_ONE);
    }

    #[test]
    fn q15_round_trip_no_drift() {
        // A long cascade of Q15::ONE mul should not bleed away.
        let mut x = Q15::HALF;
        for _ in 0..100 {
            x = x.mul(Q15::ONE);
        }
        // We expect at most a few LSB of drift from the round-bias.
        assert!((x.raw() - Q15::HALF.raw()).abs() < 4);
    }

    #[test]
    fn q15_saturating_add_and_sub() {
        assert_eq!(Q15::ONE.saturating_add(Q15::ONE), Q15::ONE);
        assert_eq!(Q15::NEG_ONE.saturating_sub(Q15::ONE), Q15::NEG_ONE);
    }

    // ---------- Q8_8 ------------------------------------------

    #[test]
    fn q8_8_int_round_trip() {
        for n in -120..120 {
            assert_eq!(Q8_8::from_int(n).trunc(), n);
        }
    }

    #[test]
    fn q8_8_round_to_nearest() {
        // 1.5 → 2 (away from zero on ties)
        let p = Q8_8::from_raw(0x0180); // 1.5
        assert_eq!(p.round(), 2);
        // -1.5 → -2
        let p = Q8_8::from_raw(-0x0180);
        assert_eq!(p.round(), -2);
        // 1.4 → 1
        let p = Q8_8::from_raw(0x0166);
        assert_eq!(p.round(), 1);
    }

    // ---------- Q24_8 -----------------------------------------

    #[test]
    fn q24_8_widen_narrow() {
        let p = Q8_8::from_int(48);
        let acc = Q24_8::from_q8_8(p);
        assert_eq!(acc.to_q8_8_saturating(), p);
    }

    #[test]
    fn q24_8_finetune_addition() {
        // Add a +1 semitone Q15 finetune (which is Q15::ONE,
        // ≈ +1.0 semitone) on top of pitch 48.
        let acc = Q24_8::from_q8_8(Q8_8::from_int(48));
        let acc2 = acc.add_finetune_q15(Q15::ONE);
        // Expect ~49 semitones now.
        assert!((acc2.to_q8_8_saturating().round() - 49).abs() <= 1);
    }

    // ---------- Q16_16 ----------------------------------------

    #[test]
    fn q16_16_octave_shift() {
        let f = Q16_16::from_int(440);
        assert_eq!(f.shift_octave(1), Q16_16::from_int(880));
        assert_eq!(f.shift_octave(-1), Q16_16::from_int(220));
    }

    #[test]
    fn q16_16_octave_saturates() {
        let f = Q16_16::from_int(40000);
        // +2 octaves on 40000 → 160000, doesn't fit u32 high bits
        // (40000 << 16) << 2 = 40000 << 18 = > u32::MAX.
        assert_eq!(f.shift_octave(2), Q16_16::from_raw(u32::MAX));
    }
}
