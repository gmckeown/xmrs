//! Domain newtypes for the player.
//!
//! Each type wraps a [`crate::fixed::fixed`] primitive but only
//! exposes the operations that make musical sense. The point
//! is that the type system rejects nonsense like
//! `volume * panning` or `pitch + frequency` at compile time,
//! and the rounding / clamping rules of the gain chain live in
//! one place.
//!
//! ## Vocabulary
//!
//! * **Gains** ([`Volume`], [`ChannelVolume`], [`EnvValue`],
//!   [`GlobalVolume`], [`Panning`]): all Q1.15, all in `[0, 1]`
//!   semantically. Distinct types because their update rules
//!   differ even when their numeric ranges coincide.
//! * **Audio sample** ([`Amp`]): Q1.15 in `[-1, 1)`. Distinct
//!   from gains so we don't accidentally pass an audio sample
//!   where a coefficient is expected.
//! * **Pitch chain** ([`Pitch`], [`PitchDelta`], [`PitchAcc`],
//!   [`Finetune`]): Q8.8 stored, Q24.8 in flight. Conversions
//!   between them are explicit method calls.
//! * **Frequency / period / step** ([`Frequency`], [`Period`],
//!   [`SampleStep`]): three views of the same thing, converted
//!   by [`crate::fixed::tables`] (no float, no transcendental).

use crate::fixed::fixed::{Q15, Q24_8, Q7_25, Q8_8};
use serde::{Deserialize, Serialize};

// =====================================================================
// READABILITY MACROS
//
// Most Q-format newtypes wrap a `Q15` (or a `Q8_8`) which itself
// wraps a primitive. Reaching the raw bits idiomatically required
// `vol.raw().raw() as i32`, twice the indirection for what's
// conceptually "give me the integer for a saturating
// accumulation". These macros add a single, intent-named accessor
// per newtype:
//
//   * `as_q15_i16` / `as_q15_i32`     — read the underlying bits
//   * `from_q15_i16` / `from_q15_i32_sat` — build from raw bits
//
// The same idea applies to Q8.8-based types (`Pitch`, `PitchDelta`):
//
//   * `as_q8_8_i16` / `as_q8_8_i32`
//   * `from_q8_8_i32_sat`
//
// These exist so consumer code reads like
//   `let p = pan.as_q15_i32();`
// instead of
//   `let p = pan.raw().raw() as i32;`
// while still letting Rust verify the type identity at the
// call site. None of them introduce new arithmetic.
// =====================================================================

/// Add Q1.15-newtype readability helpers (`as_q15_*`, `from_q15_*`).
///
/// Requires the type to expose `raw() -> Q15` and `from_q15(Q15)`.
macro_rules! impl_q15_readability {
    ($t:ident) => {
        impl $t {
            /// Underlying Q1.15 raw `i16`. One call instead of
            /// `self.raw().raw()`.
            #[inline]
            pub const fn as_q15_i16(self) -> i16 {
                self.0.raw()
            }

            /// Underlying Q1.15 raw, sign-extended to `i32`.
            /// One call instead of `self.raw().raw() as i32`.
            #[inline(always)]
            pub const fn as_q15_i32(self) -> i32 {
                self.0.raw() as i32
            }

            /// Build from a Q1.15 raw `i16`. One call instead of
            /// `Self::from_q15(Q15::from_raw(v))`.
            #[inline]
            pub const fn from_q15_i16(v: i16) -> Self {
                Self(Q15::from_raw(v))
            }

            /// Build from an `i32`, saturating to the Q1.15
            /// `[i16::MIN, i16::MAX]` range. Use when narrowing
            /// from a widened MAC accumulator.
            #[inline(always)]
            pub const fn from_q15_i32_sat(v: i32) -> Self {
                Self(Q15::from_i32_sat(v))
            }
        }
    };
}

/// Add Q8.8-newtype readability helpers (`as_q8_8_*`, `from_q8_8_*`).
///
/// Requires the type to expose `raw() -> Q8_8` and `from_q8_8(Q8_8)`.
macro_rules! impl_q8_8_readability {
    ($t:ident) => {
        impl $t {
            /// Underlying Q8.8 raw `i16`.
            #[inline]
            pub const fn as_q8_8_i16(self) -> i16 {
                self.0.raw()
            }

            /// Underlying Q8.8 raw, sign-extended to `i32`. Used
            /// by pitch chains that accumulate several deltas
            /// before narrowing back.
            #[inline]
            pub const fn as_q8_8_i32(self) -> i32 {
                self.0.raw() as i32
            }

            /// Build from a Q8.8 raw `i16`.
            #[inline]
            pub const fn from_q8_8_i16(v: i16) -> Self {
                Self(Q8_8::from_raw(v))
            }

            /// Build from an `i32`, saturating to the Q8.8
            /// `[i16::MIN, i16::MAX]` range.
            #[inline]
            pub const fn from_q8_8_i32_sat(v: i32) -> Self {
                Self(Q8_8::from_i32_sat(v))
            }
        }
    };
}

/// Add audio-style saturating operator overloading
/// (`Neg`, `Add`, `Sub`) to a Q1.15-newtype. The semantic is
/// "compose two values of the same kind"; types where this
/// doesn't make musical sense (e.g. `Volume + Volume`,
/// `Panning - Panning`) deliberately don't get these.
///
/// Requires the type to wrap a single `Q15` field at index 0.
macro_rules! impl_q15_ops {
    ($t:ident) => {
        impl core::ops::Neg for $t {
            type Output = Self;
            #[inline]
            fn neg(self) -> Self {
                Self(-self.0)
            }
        }
        impl core::ops::Add for $t {
            type Output = Self;
            #[inline]
            fn add(self, rhs: Self) -> Self {
                Self(self.0 + rhs.0)
            }
        }
        impl core::ops::Sub for $t {
            type Output = Self;
            #[inline]
            fn sub(self, rhs: Self) -> Self {
                Self(self.0 - rhs.0)
            }
        }
    };
}

/// Add Q8.8-newtype operator overloading. Same intent as
/// [`impl_q15_ops`] but on a Q8.8 storage.
macro_rules! impl_q8_8_ops {
    ($t:ident) => {
        impl core::ops::Neg for $t {
            type Output = Self;
            #[inline]
            fn neg(self) -> Self {
                Self(-self.0)
            }
        }
        impl core::ops::Add for $t {
            type Output = Self;
            #[inline]
            fn add(self, rhs: Self) -> Self {
                Self(self.0 + rhs.0)
            }
        }
        impl core::ops::Sub for $t {
            type Output = Self;
            #[inline]
            fn sub(self, rhs: Self) -> Self {
                Self(self.0 - rhs.0)
            }
        }
    };
}

// =====================================================================
// AUDIO SIGNAL
// =====================================================================

/// One audio sample on one channel, Q1.15.
///
/// Conversion from the source PCM lives elsewhere (typically
/// in `xmrs::sample::Sample::at`, after this crate's
/// integration): an `i8` PCM byte just shifts left by 8 to
/// become a Q1.15, an `i16` PCM is already Q1.15.
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
#[repr(transparent)]
#[serde(transparent)]
pub struct Amp(Q15);

impl Amp {
    /// Silence (zero amplitude).
    pub const SILENCE: Self = Self(Q15::ZERO);

    /// Wrap a raw [`Q15`] value as an audio sample.
    #[inline]
    pub const fn from_q15(q: Q15) -> Self {
        Self(q)
    }

    /// Unwrap to the underlying [`Q15`].
    #[inline]
    pub const fn into_q15(self) -> Q15 {
        self.0
    }

    /// Linear interpolation between two samples by a Q1.15
    /// weight `t`, where `t = 0` returns `self` and
    /// `t = Q15::ONE` returns (very nearly) `other`.
    ///
    /// Computed in widened i32 to avoid intermediate
    /// saturation on the `(other - self)` term.
    #[inline(always)]
    pub const fn lerp(self, other: Self, t: Q15) -> Self {
        let u = self.0.raw() as i32;
        let v = other.0.raw() as i32;
        let diff = v - u; // fits in i32 easily (i16 - i16)
        let scaled = (t.raw() as i32 * diff + (1 << 14)) >> 15;
        let r = u + scaled;
        let r = if r > 0x7FFF {
            0x7FFF
        } else if r < -0x8000 {
            -0x8000
        } else {
            r
        };
        Self(Q15::from_raw(r as i16))
    }

    /// Saturating add, used by the per-channel mixer to
    /// accumulate into a stereo bus.
    #[inline]
    pub const fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    /// Widen Q1.15 → Q15.16 raw `i32` for use in IIR / mix
    /// accumulators that need sub-Q1.15 headroom. Same numerical
    /// value, just shifted left by one to align fractional bits.
    /// Replaces `(self.into_q15().raw() as i32) << 1` at every
    /// call site.
    #[inline(always)]
    pub const fn widen_q15_16(self) -> i32 {
        self.0.widen_q15_16()
    }

    /// Saturating accumulate this sample into a Q1.15 raw `i32`
    /// register (the inner mix loop's per-channel sum). The
    /// accumulator widens to `i32` so several `Amp`s can stack
    /// before narrowing back to Q1.15.
    #[inline(always)]
    pub fn accumulate_into(self, acc: &mut i32) {
        *acc = acc.saturating_add(self.as_q15_i32());
    }
}

// =====================================================================
// GAIN STAGE — one newtype per concept so the cascade stays typed
// =====================================================================

/// Per-voice instrument volume. Q1.15, semantically `[0, 1]`.
///
/// Internally we still allow the underlying Q15 to swing
/// negative briefly during `with_tremolo`; the public surface
/// always re-clamps to `[0, ONE]`.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Debug, Default)]
#[repr(transparent)]
#[serde(transparent)]
pub struct Volume(Q15);

impl Volume {
    /// Muted.
    pub const SILENT: Self = Self(Q15::ZERO);
    /// Maximum gain (≈ +1.0 in Q1.15).
    pub const FULL: Self = Self(Q15::ONE);

    /// Wrap a raw [`Q15`] (caller guarantees `0 ≤ q ≤ ONE`).
    #[inline]
    pub const fn from_q15(q: Q15) -> Self {
        Self(q)
    }

    /// Underlying Q15 (for advanced users; prefer the methods).
    #[inline(always)]
    pub const fn raw(self) -> Q15 {
        self.0
    }

    /// Compose this volume with another gain (commutative).
    /// Result is clamped to `[0, ONE]` by the saturation in
    /// [`Q15::mul`].
    #[inline(always)]
    pub const fn scaled_by(self, other: Volume) -> Volume {
        Volume(self.0.mul(other.0))
    }

    /// Apply this volume to an audio sample. The hot path's
    /// per-sample gain step.
    #[inline(always)]
    pub const fn apply(self, s: Amp) -> Amp {
        Amp(self.0.mul(s.into_q15()))
    }

    /// Add a Q1.15 tremolo modulation to this volume, clamping
    /// to `[0, ONE]`. Tremolo can pull volume below zero in
    /// raw form; the clamp is part of the semantics.
    #[inline]
    pub const fn with_tremolo(self, modulation: Q15) -> Volume {
        let s = self.0.saturating_add(modulation);
        Volume(s.clamp(Q15::ZERO, Q15::ONE))
    }

    /// Subtract a fadeout step from the volume, clamping at
    /// zero. Used for the per-tick volume-fadeout decrement.
    #[inline]
    pub const fn faded_by(self, step: Q15) -> Volume {
        let s = self.0.saturating_sub(step);
        Volume(s.clamp(Q15::ZERO, Q15::ONE))
    }

    /// Build from a tracker volume byte in `0..=64` (MOD / XM /
    /// S3M / IT). Maps `0 → SILENT` and `64 → FULL`. Equivalent
    /// to `byte / 64` in the float era.
    ///
    /// `1/64` is exactly `1 << 9` in Q1.15, so the conversion
    /// is a single shift — no rounding error.
    #[inline]
    pub const fn from_byte_64(byte: u8) -> Self {
        if byte >= 64 {
            Self::FULL
        } else {
            Self(Q15::from_raw((byte as i16) << 9))
        }
    }

    /// Inverse of [`Self::from_byte_64`]. Round-trip-exact for
    /// every `byte ∈ 0..=64`.
    #[inline]
    pub const fn to_byte_64(self) -> u8 {
        // Same rounding as `EnvValue::to_byte_64` — `+ 256`
        // bias, `>> 9` divide.
        let raw = self.0.raw() as i32;
        let byte = (raw + 256) >> 9;
        if byte < 0 {
            0
        } else if byte > 64 {
            64
        } else {
            byte as u8
        }
    }

    /// Build from a signed numerator / denominator. Used by
    /// importers whose scale isn't a power-of-two byte
    /// (IT global volume `/ 128`, IT volume_fadeout `/ 1024`,
    /// XM volume_fadeout `/ 32760`, etc.). Replaces
    /// `byte as f32 / den as f32` everywhere — and the result
    /// saturates to `[Self::SILENT, Self::FULL]` so the
    /// previous `.clamp(0.0, 1.0)` is folded into the storage.
    #[inline]
    pub const fn from_ratio(num: i32, den: i32) -> Self {
        Self(Q15::from_ratio(num, den))
    }

    /// Inverse export at an arbitrary u16 scale. Returns
    /// `round(self × scale)` clamped to `0..=u16::MAX`.
    /// Used by XM's `volume_fadeout × 32760 → u16` export
    /// path.
    #[inline]
    pub const fn to_ratio_u16(self, scale: u32) -> u16 {
        let raw = self.0.raw() as i64;
        if raw <= 0 {
            return 0;
        }
        let scaled = raw * scale as i64;
        // Half a Q15 step is 16384.
        let rounded = (scaled + 0x4000) >> 15;
        if rounded > u16::MAX as i64 {
            u16::MAX
        } else {
            rounded as u16
        }
    }
}

/// Channel volume column (XM `vXX`, S3M `Mxx`). Distinct from
/// [`Volume`] because slide rules differ but the gain combines
/// the same way at mix time.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Debug, Default)]
#[repr(transparent)]
#[serde(transparent)]
pub struct ChannelVolume(Q15);

impl ChannelVolume {
    /// Maximum channel volume.
    pub const FULL: Self = Self(Q15::ONE);
    /// Silent channel.
    pub const SILENT: Self = Self(Q15::ZERO);

    /// Wrap a raw Q1.15.
    #[inline]
    pub const fn from_q15(q: Q15) -> Self {
        Self(q)
    }

    /// Apply this channel volume to a per-voice [`Volume`].
    #[inline]
    pub const fn applied_to(self, v: Volume) -> Volume {
        Volume(self.0.mul(v.0))
    }

    /// Underlying Q15.
    #[inline(always)]
    pub const fn raw(self) -> Q15 {
        self.0
    }

    /// Build from a tracker channel-volume byte in `0..=64`.
    /// Same scale as [`Volume::from_byte_64`]; the type
    /// difference exists at the type level, not the wire level.
    #[inline]
    pub const fn from_byte_64(byte: u8) -> Self {
        if byte >= 64 {
            Self::FULL
        } else {
            Self(Q15::from_raw((byte as i16) << 9))
        }
    }

    /// Inverse of [`Self::from_byte_64`].
    #[inline]
    pub const fn to_byte_64(self) -> u8 {
        let raw = self.0.raw() as i32;
        let byte = (raw + 256) >> 9;
        if byte < 0 {
            0
        } else if byte > 64 {
            64
        } else {
            byte as u8
        }
    }

    /// Add a signed Q1.15 delta to this channel volume,
    /// saturating then clamping to `[0, 1]`. Used by the per-
    /// tick channel-volume slide (`Nxy` in IT, the matching
    /// arms in S3M / XM importers). Mirrors
    /// [`Volume::with_tremolo`] in shape.
    #[inline]
    pub const fn shifted_by(self, delta: Q15) -> Self {
        let s = self.0.saturating_add(delta);
        Self(s.clamp(Q15::ZERO, Q15::ONE))
    }
}

/// Envelope output, Q1.15.
///
/// Same numeric range as [`Volume`] but semantically distinct:
/// an envelope holds its last point's value past the end while
/// a volume cannot, and envelopes are computed by linear
/// interpolation between control points (not by repeated
/// multiplication).
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
#[repr(transparent)]
#[serde(transparent)]
pub struct EnvValue(Q15);

impl EnvValue {
    /// Default for a volume envelope (≈ unity).
    pub const VOLUME_DEFAULT: Self = Self(Q15::ONE);
    /// Default for a panning envelope (centre).
    pub const PAN_DEFAULT: Self = Self(Q15::HALF);

    /// Wrap a raw Q1.15.
    #[inline]
    pub const fn from_q15(q: Q15) -> Self {
        Self(q)
    }

    /// Build from a tracker envelope byte in `0..=64`. Maps
    /// `0 → 0.0` exactly and `64 → ≈ 1.0` (one Q1.15 LSB short
    /// of `+1.0`, the same convention as [`Volume::FULL`]).
    /// Equivalent to `byte / 64` in the float era.
    ///
    /// Used by every unsigned-magnitude envelope importer (XM,
    /// IT volume / unsigned-pitch, MOD, S3M).
    #[inline]
    pub const fn from_byte_64(byte: u8) -> Self {
        if byte >= 64 {
            Self(Q15::ONE)
        } else {
            // 1/64 in Q1.15 is exactly `1 << 9 = 512`, so the
            // conversion is a single shift — no divide, no
            // rounding error. Same trick as
            // `from_tracker::volume_from_byte_64`.
            Self(Q15::from_raw((byte as i16) << 9))
        }
    }

    /// Build from an IT signed envelope byte in `-32..=32`,
    /// mapped onto `0..=1` with `0` (the signed centre)
    /// landing at `0.5`. Equivalent to `(byte + 32) / 64` in
    /// the float era.
    ///
    /// Used by the IT panning and signed-pitch envelope
    /// importer paths (see `to_envelope_points_signed`).
    #[inline]
    pub const fn from_signed_byte_64(byte: i8) -> Self {
        // Re-centre into `0..=64` and reuse `from_byte_64`.
        // `byte + 32` saturates harmlessly outside the
        // documented range.
        let centred = byte as i16 + 32;
        if centred <= 0 {
            Self(Q15::ZERO)
        } else if centred >= 64 {
            Self(Q15::ONE)
        } else {
            Self(Q15::from_raw(centred << 9))
        }
    }

    /// Inverse of [`Self::from_byte_64`]. Returns the byte in
    /// `0..=64` that, when re-imported, reconstructs this
    /// envelope value. The conversion is round-to-nearest, so
    /// the round-trip
    /// `from_byte_64(b).to_byte_64() == b` is exact for every
    /// `b ∈ 0..=64`. Values that originally arrived through an
    /// `f32 → Q15` import may drift by at most one byte LSB,
    /// well below the envelope's audible resolution.
    ///
    /// Used by the XM exporter (`XmInstrDefault::from_envelope`).
    #[inline]
    pub const fn to_byte_64(self) -> u8 {
        // Each tracker-byte step is `2^15 / 64 = 512` raw
        // Q1.15 units. Adding `256` (half a step) before the
        // shift gives correct rounding for non-negative
        // values, which envelope values are. The clamp is a
        // defensive cap — well-formed `EnvValue`s in `[0, 1]`
        // already produce a byte ≤ 64.
        let raw = self.0.raw() as i32;
        let byte = (raw + 256) >> 9;
        if byte < 0 {
            0
        } else if byte > 64 {
            64
        } else {
            byte as u8
        }
    }

    /// Apply this envelope as a multiplier on a [`Volume`].
    #[inline]
    pub const fn applied_to(self, v: Volume) -> Volume {
        Volume(self.0.mul(v.0))
    }

    /// Read out the underlying [`Q15`]. Used by the panning
    /// math (which mixes envelope values, panbrello LFO and
    /// the static pan position by hand).
    #[inline(always)]
    pub const fn raw_q15(self) -> Q15 {
        self.0
    }

    /// Linear interpolation between two envelope values by a
    /// Q1.15 weight `t`, where `t = 0` returns `self` and
    /// `t = Q15::ONE` returns (almost) `other`. Mirrors
    /// [`Amp::lerp`] in shape; computed in widened `i32` to
    /// avoid intermediate saturation on the `(other - self)`
    /// term, then narrowed back to Q1.15 with saturation.
    ///
    /// This is the per-frame envelope interpolation between
    /// two adjacent envelope points.
    #[inline(always)]
    pub const fn lerp(self, other: Self, t: Q15) -> Self {
        let u = self.0.raw() as i32;
        let v = other.0.raw() as i32;
        let diff = v - u;
        let scaled = (t.raw() as i32 * diff + (1 << 14)) >> 15;
        let r = u + scaled;
        let r = if r > 0x7FFF {
            0x7FFF
        } else if r < -0x8000 {
            -0x8000
        } else {
            r
        };
        Self(Q15::from_raw(r as i16))
    }
}

/// Module-level master gain (set by `Gxx` and by the user).
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Debug, Default)]
#[repr(transparent)]
#[serde(transparent)]
pub struct GlobalVolume(Q15);

impl GlobalVolume {
    /// Maximum global volume.
    pub const FULL: Self = Self(Q15::ONE);
    /// Silent.
    pub const SILENT: Self = Self(Q15::ZERO);

    /// Wrap a raw Q1.15.
    #[inline]
    pub const fn from_q15(q: Q15) -> Self {
        Self(q)
    }

    /// Underlying Q15.
    #[inline(always)]
    pub const fn raw_q15(self) -> Q15 {
        self.0
    }

    /// Apply to a final mixed sample.
    #[inline]
    pub const fn applied_to(self, s: Amp) -> Amp {
        Amp(self.0.mul(s.into_q15()))
    }
}

/// User-controlled amplification, post-mix. Q4.12 internally
/// (range `[-8, +8)`, step `2⁻¹²`). Can exceed unity, hence
/// not a Q1.15.
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
#[repr(transparent)]
#[serde(transparent)]
pub struct Amplification(i16); // Q4.12

impl Amplification {
    /// Unity gain.
    pub const UNITY: Self = Self(0x1000);

    /// Build from a raw Q4.12 value (caller's responsibility).
    #[inline]
    pub const fn from_raw_q4_12(raw: i16) -> Self {
        Self(raw)
    }

    /// Underlying Q4.12 raw value.
    #[inline(always)]
    pub const fn raw_q4_12(self) -> i16 {
        self.0
    }

    /// Build from an integer multiplier.
    #[inline]
    pub const fn from_int(n: i8) -> Self {
        Self((n as i16) << 12)
    }

    /// Apply this amplification to a final mixed sample,
    /// saturating to `[-1, +1)`.
    #[inline]
    pub const fn applied_to(self, s: Amp) -> Amp {
        // Q4.12 × Q1.15 → Q5.27, narrow back to Q1.15 with
        // round-to-nearest.
        let prod = (self.0 as i32) * (s.into_q15().raw() as i32);
        let prod = prod + (1 << 11);
        let r = prod >> 12;
        let r = if r > 0x7FFF {
            0x7FFF
        } else if r < -0x8000 {
            -0x8000
        } else {
            r
        };
        Amp(Q15::from_raw(r as i16))
    }

    // ---- Readability helpers -------------------------------------

    /// Sign-extended `i32` view of the Q4.12 raw bits. Use when
    /// composing several gain factors in a widened register
    /// before narrowing back. One call instead of
    /// `self.raw_q4_12() as i32`.
    #[inline]
    pub const fn as_q4_12_i32(self) -> i32 {
        self.0 as i32
    }
}

// =====================================================================
// PANNING
// =====================================================================

/// Pan position, Q1.15 representing `[0, 1]` (`0` = full left,
/// `Q15::HALF` = centre, `Q15::ONE` = full right).
///
/// The high bit of the underlying [`Q15`] is unused (we never
/// go negative), but we keep the same representation as the
/// gain stage for uniformity.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Debug, Default)]
#[repr(transparent)]
#[serde(transparent)]
pub struct Panning(Q15);

impl Panning {
    /// Hard left.
    pub const LEFT: Self = Self(Q15::ZERO);
    /// Centre.
    pub const CENTER: Self = Self(Q15::HALF);
    /// Hard right.
    pub const RIGHT: Self = Self(Q15::ONE);

    /// Wrap a raw Q1.15.
    #[inline]
    pub const fn from_q15(q: Q15) -> Self {
        Self(q)
    }

    /// Underlying Q15, mostly for the `tables::pan_sqrt` lookup.
    #[inline(always)]
    pub const fn raw(self) -> Q15 {
        self.0
    }

    /// Build from an IT panning byte in `0..=64` (where `32` is
    /// centre). Replaces `byte / 64` in the float era. Exact
    /// (`1/64` is `1 << 9` in Q1.15).
    #[inline]
    pub const fn from_byte_64(byte: u8) -> Self {
        if byte >= 64 {
            Self::RIGHT
        } else {
            Self(Q15::from_raw((byte as i16) << 9))
        }
    }

    /// Inverse of [`Self::from_byte_64`].
    #[inline]
    pub const fn to_byte_64(self) -> u8 {
        let raw = self.0.raw() as i32;
        let byte = (raw + 256) >> 9;
        if byte < 0 {
            0
        } else if byte > 64 {
            64
        } else {
            byte as u8
        }
    }

    /// Build from an XM panning byte in `0..=255` (where `128`
    /// is centre). Replaces `byte / 255` in the float era.
    /// Result is `byte / 255` rounded to nearest in Q1.15;
    /// round-trip via [`Self::to_byte_255`] is exact for every
    /// `byte ∈ 0..=255`.
    #[inline]
    pub const fn from_byte_255(byte: u8) -> Self {
        Self(Q15::from_ratio(byte as i32, 255))
    }

    /// Inverse of [`Self::from_byte_255`]. Round-trip-exact for
    /// every `byte ∈ 0..=255`.
    #[inline]
    pub const fn to_byte_255(self) -> u8 {
        // Round-to-nearest: `(raw * 255 + 0x4000) >> 15`. The
        // bias is half a Q15 step (`2^14 = 16384`).
        let raw = self.0.raw() as i64;
        if raw <= 0 {
            return 0;
        }
        let scaled = raw * 255;
        let rounded = (scaled + 0x4000) >> 15;
        if rounded > 255 {
            255
        } else {
            rounded as u8
        }
    }

    /// Build from an arbitrary integer ratio. Used when the
    /// importer's denominator isn't 64 or 255 — typically for
    /// generic Q1.15 panning constructions.
    #[inline]
    pub const fn from_ratio(num: i32, den: i32) -> Self {
        Self(Q15::from_ratio(num, den))
    }

    /// Add a signed Q1.15 delta to this pan position,
    /// saturating then clamping to `[0, 1]`. Used by the per-
    /// tick pan slide. Mirrors [`Volume::with_tremolo`] /
    /// [`ChannelVolume::shifted_by`] in shape.
    #[inline]
    pub const fn shifted_by(self, delta: Q15) -> Self {
        let s = self.0.saturating_add(delta);
        Self(s.clamp(Q15::ZERO, Q15::ONE))
    }

    /// Compute the equal-power stereo gain pair from this pan
    /// position. Uses [`crate::fixed::tables::pan_sqrt`].
    ///
    /// Returns `(left_gain, right_gain)`, both as [`Volume`].
    #[inline]
    pub fn stereo_gains(self) -> (Volume, Volume) {
        let p = self.0;
        // 1 - p, saturating
        let one_minus = Q15::ONE.saturating_sub(p);
        let l = crate::fixed::tables::pan_sqrt(one_minus);
        let r = crate::fixed::tables::pan_sqrt(p);
        (Volume(l), Volume(r))
    }

    /// Linear stereo split: `(1 - pan, pan)` as two `Volume`.
    /// This is the historical tracker convention
    /// (PT/ST3/FT2/IT, schism `sndmix.c:544`):
    /// ```text
    /// chan->left_volume_new  = realvol * (256 - pan) >> 8;
    /// chan->right_volume_new = realvol * pan >> 8;
    /// ```
    /// Use this in the per-tick gain stage. The constant-power
    /// alternative ([`Self::stereo_gains`]) is for engines that
    /// prefer the equal-power crossfade — schism doesn't.
    #[inline(always)]
    pub const fn stereo_split_linear(self) -> (Volume, Volume) {
        let p = self.0.raw();
        // `Q15::ONE.raw() = 0x7FFF`. `pan_left = 0x7FFF - p`.
        let pan_left = 0x7FFF_i16 - p;
        (Volume(Q15::from_raw(pan_left)), Volume(Q15::from_raw(p)))
    }
}

// =====================================================================
// PITCH / PERIOD / FREQUENCY
// =====================================================================

/// Musical pitch in semitones, Q8.8.
///
/// Valid notes are `[0, 119]` (ten octaves of MIDI numbering);
/// the type allows wider for transposition headroom.
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord, Debug, Default)]
#[repr(transparent)]
#[serde(transparent)]
pub struct Pitch(Q8_8);

impl Pitch {
    /// `C0` = MIDI note 0 in this scheme.
    pub const C0: Self = Self(Q8_8::from_int(0));
    /// `C4` = MIDI note 48.
    pub const C4: Self = Self(Q8_8::from_int(48));
    /// `A4` = MIDI note 57.
    pub const A4: Self = Self(Q8_8::from_int(57));
    /// `B9` = MIDI note 119, the top of the valid range.
    pub const B9: Self = Self(Q8_8::from_int(119));

    /// From an integer semitone count.
    #[inline]
    pub const fn from_semitone(n: i16) -> Self {
        Self(Q8_8::from_int(n))
    }

    /// Wrap a raw Q8.8.
    #[inline]
    pub const fn from_q8_8(q: Q8_8) -> Self {
        Self(q)
    }

    /// Underlying Q8.8.
    #[inline(always)]
    pub const fn raw(self) -> Q8_8 {
        self.0
    }

    /// Round to the nearest integer semitone (used by the
    /// glissando effect to snap fractional pitches).
    #[inline]
    pub const fn quantized(self) -> Self {
        Self(Q8_8::from_int(self.0.round()))
    }

    /// Truncate the fractional semitone part. `Pitch::C4` plus
    /// any sub-semitone offset returns `48`.
    #[inline]
    pub const fn semitone_trunc(self) -> i16 {
        self.0.trunc()
    }

    /// Round to the nearest integer semitone, ties away from
    /// zero. Replaces the manual `(raw + 0x80) >> 8` idiom on
    /// the raw bits.
    #[inline]
    pub const fn semitone_round(self) -> i16 {
        self.0.round()
    }

    /// Round up to the next integer semitone. Replaces the
    /// manual `(raw + 0xFF) >> 8` ceiling idiom on the raw
    /// bits.
    #[inline]
    pub const fn semitone_ceil(self) -> i16 {
        self.0.ceil_int()
    }

    /// Sub-semitone position as a 4-bit Amiga finetune index
    /// (`0..=15`). Picks the high nibble of the Q8.8 fractional
    /// byte — `1/16` of a semitone per step, matching the
    /// Amiga period table's resolution.
    #[inline]
    pub const fn sub_semitone_q4(self) -> u8 {
        self.0.frac_byte() >> 4
    }

    /// Add a [`PitchDelta`] (saturating).
    #[inline]
    pub const fn shift(self, delta: PitchDelta) -> Self {
        Self(self.0.saturating_add(delta.0))
    }
}

/// A signed pitch difference (note offset, arpeggio offset,
/// portamento step). Same Q8.8 representation as [`Pitch`] but
/// distinct so we don't add two absolute pitches by mistake.
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
#[repr(transparent)]
#[serde(transparent)]
pub struct PitchDelta(Q8_8);

impl PitchDelta {
    /// Zero delta.
    pub const ZERO: Self = Self(Q8_8::ZERO);

    /// From an integer semitone count.
    #[inline]
    pub const fn from_semitones(n: i16) -> Self {
        Self(Q8_8::from_int(n))
    }

    /// Wrap a raw Q8.8.
    #[inline]
    pub const fn from_q8_8(q: Q8_8) -> Self {
        Self(q)
    }

    /// Underlying Q8.8.
    #[inline(always)]
    pub const fn raw(self) -> Q8_8 {
        self.0
    }

    /// Saturating add.
    #[inline]
    pub const fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    /// Build from an integer ratio of semitones — equivalent to
    /// `num as f32 / den as f32` semitones, in saturating Q8.8.
    /// Used by importers that scale a small byte / nibble into
    /// a sub-semitone modulation amount (XM vibrato depth =
    /// `byte / 30`, SID vibrato depth = `byte / 15`, etc.).
    /// Both arguments are `i16` — see [`Q8_8::from_ratio`] for
    /// the rationale.
    #[inline]
    pub const fn from_ratio(num: i16, den: i16) -> Self {
        Self(Q8_8::from_ratio(num, den))
    }

    /// Inverse export: `round(self * den)` clamped to `0..=255`.
    /// Replaces `(delta * den as f32) as u8` in the exporters.
    #[inline]
    pub const fn to_ratio_byte(self, den: u32) -> u8 {
        self.0.to_ratio_byte(den)
    }
}

/// Detune within a single semitone, Q1.15 in `[-1, +1]`.
///
/// Shares its underlying type with the audio gain stage but
/// is *not* assignable to a [`Volume`]; the type system
/// keeps them apart.
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
#[repr(transparent)]
#[serde(transparent)]
pub struct Finetune(Q15);

impl Finetune {
    /// No detune.
    pub const ZERO: Self = Self(Q15::ZERO);

    /// Wrap a raw Q1.15.
    #[inline]
    pub const fn from_q15(q: Q15) -> Self {
        Self(q)
    }

    /// Unwrap to the underlying Q1.15.
    #[inline]
    pub const fn into_q15(self) -> Q15 {
        self.0
    }

    /// Build from a signed numerator / denominator. Used by
    /// importers that store finetune as `byte / 127` (Amiga,
    /// XM) or similar small ratios — replaces the previous
    /// `(byte as f32 / 127.0).clamp(-1.0, 1.0)`.
    ///
    /// Saturates to `[-1.0, +0.99997]` if the resulting value
    /// is out of Q1.15 range — same clamp the old f32 path
    /// did, just at the boundary of the storage type instead
    /// of with an explicit `.clamp` call.
    #[inline]
    pub const fn from_ratio(num: i32, den: i32) -> Self {
        Self(Q15::from_ratio(num, den))
    }

    /// Inverse export: `round(self * scale)` clamped to the
    /// signed byte range `-128..=127`. Replaces
    /// `(value * scale as f32) as i8` in the exporters.
    ///
    /// Example: `s.finetune.to_signed_byte(127)` is the inverse
    /// of `Finetune::from_ratio(byte as i32, 127)` for any
    /// `byte ∈ -127..=127`.
    #[inline]
    pub const fn to_signed_byte(self, scale: u32) -> i8 {
        // Round-half-away-from-zero: bias of `scale / 2` before
        // the divide. Computed in i64 to avoid overflow on the
        // multiply.
        let raw = self.0.raw() as i64;
        let scaled = raw * (scale as i64);
        // `2^14 = 16384` is half a Q1.15 step; mirror the bias
        // from `Panning::to_byte_255` (which is the same shape:
        // round-to-nearest signed Q15 → byte at scale).
        let rounded = if raw >= 0 {
            (scaled + 0x4000) >> 15
        } else {
            -(((-scaled) + 0x4000) >> 15)
        };
        if rounded > 127 {
            127
        } else if rounded < -128 {
            -128
        } else {
            rounded as i8
        }
    }
}

/// Wide pitch accumulator, Q24.8 on `i32`.
///
/// Built from a base [`Pitch`] and any number of deltas /
/// finetunes; collapses back to [`Pitch`] at the very end of
/// the pitch chain (right before the period lookup). This is
/// what keeps the cumulative rounding error of
/// `note + arpeggio + finetune + vibrato_modulation` below one
/// cent.
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
#[repr(transparent)]
#[serde(transparent)]
pub struct PitchAcc(Q24_8);

impl PitchAcc {
    /// From a [`Pitch`].
    #[inline]
    pub const fn from_pitch(p: Pitch) -> Self {
        Self(Q24_8::from_q8_8(p.raw()))
    }

    /// Add a delta.
    #[inline]
    pub const fn add_delta(self, d: PitchDelta) -> Self {
        Self(self.0.saturating_add(Q24_8::from_q8_8(d.raw())))
    }

    /// Add a finetune (interpreted as a fraction of a
    /// semitone).
    #[inline]
    pub const fn add_finetune(self, ft: Finetune) -> Self {
        Self(self.0.add_finetune_q15(ft.into_q15()))
    }

    /// Saturate-clamp to a valid [`Pitch`] in `[0, 119]`.
    #[inline]
    pub const fn into_pitch(self) -> Pitch {
        // Clamp to [0, 119] in Q24.8 (i.e. raw values
        // [0, 119<<8 = 0x7700]) before narrowing.
        let r = self.0.raw();
        let r = if r < 0 {
            0
        } else if r > 119 << 8 {
            119 << 8
        } else {
            r
        };
        Pitch::from_q8_8(Q8_8::from_raw(r as i16))
    }
}

/// Tracker period: the integer index used by trackers as a
/// lookup key into the period→frequency table. `u16` because
/// neither linear (max 7680) nor Amiga (max ~6848) periods
/// have a useful fractional part.
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord, Debug, Default)]
#[repr(transparent)]
#[serde(transparent)]
pub struct Period(u16);

impl Period {
    /// Zero (silence / disabled voice).
    pub const ZERO: Self = Self(0);

    /// From a raw `u16`.
    #[inline]
    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }

    /// Underlying `u16`.
    #[inline(always)]
    pub const fn raw(self) -> u16 {
        self.0
    }

    /// Add a signed slide step in period units, saturating at
    /// the `u16` bounds. Replaces the OLD `period: f32 +
    /// slide_speed: f32` site in the player when both moved to
    /// integer Q-format.
    #[inline]
    pub const fn saturating_add_signed(self, delta: i16) -> Self {
        let s = self.0 as i32 + delta as i32;
        if s < 0 {
            Self(0)
        } else if s > 0xFFFF {
            Self(0xFFFF)
        } else {
            Self(s as u16)
        }
    }

    /// Clamp the period into `[min, max]`. Both bounds are
    /// inclusive.
    #[inline]
    pub const fn clamp(self, min: Self, max: Self) -> Self {
        if self.0 < min.0 {
            min
        } else if self.0 > max.0 {
            max
        } else {
            self
        }
    }

    /// Slide the period one tick toward `goal` by `incr`
    /// period units. The slide stops exactly at `goal` rather
    /// than overshooting. Mirrors the old f32 `slide_towards`
    /// helper but stays in integer arithmetic.
    ///
    /// `incr` is the magnitude of the per-tick step (caller
    /// supplies a non-negative value). Direction is inferred
    /// from the sign of `goal − self`.
    #[inline]
    pub const fn slide_towards(self, goal: Self, incr: i16) -> Self {
        let step = if incr < 0 { -(incr as i32) } else { incr as i32 };
        if step == 0 {
            return goal;
        }
        if self.0 < goal.0 {
            let new = self.0 as i32 + step;
            if new >= goal.0 as i32 {
                goal
            } else {
                Self(new as u16)
            }
        } else if self.0 > goal.0 {
            let new = self.0 as i32 - step;
            if new <= goal.0 as i32 {
                goal
            } else {
                Self(new as u16)
            }
        } else {
            self
        }
    }
}

/// Frequency in Hz, **Q24.8** stored in `u32`.
///
/// Same wire size as the previous storage; the format change
/// is what matters. Before this revision the type was
/// `Q16_16(u32)` — 16 integer bits, capped at ~65 535 Hz. That
/// silently saturated for samples whose `relative_pitch` lifted
/// the playback frequency above ~65 kHz (XM `binary_world.xm`
/// has `relative_pitch=36` samples reaching ≈ 134 kHz). The
/// auto-vibrato modulation produced different Hz values per
/// tick, but they all clamped to the same saturated raw, so
/// the audible pitch wobble disappeared.
///
/// Q24.8 is the right balance for tracker audio:
///
/// * **24 integer bits** = 16 777 216 Hz max — well above the
///   ~200 kHz worst case in extreme tracker setups, no
///   saturation in practice.
/// * **8 fractional bits** = `1/256` Hz step ≈ 0.004 cent at
///   1 kHz, ≈ 0.05 cent at C-0 in the xmrs frequency
///   convention. Sub-cent everywhere in the audible range,
///   safely below any tracker finetune granularity (XM's 1/128
///   semitone, IT's 1/64).
///
/// Conversion is `Hz = raw / 256.0`. Audio-rate consumers go
/// through [`Self::raw_q24_8`] (u32 access) or the f32 bridge
/// (`raw / 256.0` cast).
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord, Debug, Default)]
#[repr(transparent)]
#[serde(transparent)]
pub struct Frequency(u32);

impl Frequency {
    /// Zero Hz.
    pub const ZERO: Self = Self(0);

    /// Reference C-4 frequency for the linear-period scheme,
    /// MIDI-aligned (= 440 × 2⁻⁹ᐟ¹² × 32 ≈ 8372 Hz).
    ///
    /// At this reference, A-4 plays at exactly 14080 Hz on
    /// the linear period grid, so modules mix cleanly against
    /// any 440-Hz-aligned source.
    pub const C4_REFERENCE: Self = Self(8372u32 << 8);

    /// Soundtracker / ProTracker / FT2 historical C-4 reference
    /// (8363 Hz). ≈ 1.9 cents below modern concert pitch; used
    /// by OpenMPT, MilkyTracker, libxmp, ft2-clone, schism.
    /// Exposed for bit-numerical comparison with legacy players.
    pub const C4_REFERENCE_LEGACY: Self = Self(8363u32 << 8);

    /// From an integer Hz value. Saturates above the Q24.8
    /// integer-part range (16 777 215 Hz, way above any
    /// realistic tracker frequency).
    #[inline]
    pub const fn from_hz(hz: u32) -> Self {
        if hz > (u32::MAX >> 8) {
            Self(u32::MAX)
        } else {
            Self(hz << 8)
        }
    }

    /// Wrap a raw Q24.8 (`u32`).
    #[inline]
    pub const fn from_raw_q24_8(raw: u32) -> Self {
        Self(raw)
    }

    /// Underlying Q24.8 raw (`u32`). `Hz = raw / 256`.
    #[inline(always)]
    pub const fn raw_q24_8(self) -> u32 {
        self.0
    }

    /// Multiply by `2^octaves`. Saturates at `u32::MAX` for
    /// extreme positive shifts (well above any realistic
    /// tracker frequency).
    #[inline]
    pub const fn shift_octave(self, oct: i8) -> Self {
        if oct >= 0 {
            let shift = oct as u32;
            if shift >= 32 {
                Self(u32::MAX)
            } else {
                let candidate = (self.0 as u64) << shift;
                if candidate > u32::MAX as u64 {
                    Self(u32::MAX)
                } else {
                    Self(candidate as u32)
                }
            }
        } else {
            let shift = (-oct) as u32;
            if shift >= 32 {
                Self(0)
            } else {
                Self(self.0 >> shift)
            }
        }
    }
}

// =====================================================================
// SAMPLE-RATE & SAMPLE-PLAYBACK STEP
// =====================================================================

/// Output device sample rate, in Hz.
///
/// `u32` is enough — common rates are 8 kHz to 192 kHz, all
/// integer.
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(transparent)]
pub struct SampleRate(pub u32);

impl SampleRate {
    /// CD-Audio rate.
    pub const CD: Self = Self(44_100);
    /// Common professional rate.
    pub const PRO_48K: Self = Self(48_000);

    /// Construct from an integer sample-rate in Hz.
    #[inline]
    pub const fn from_hz(hz: u32) -> Self {
        Self(hz)
    }

    /// Underlying integer sample-rate (Hz).
    #[inline]
    pub const fn hz(self) -> u32 {
        self.0
    }
}

impl Default for SampleRate {
    fn default() -> Self {
        Self::CD
    }
}

/// Sample-playback step, Q7.25.
///
/// Computed as `frequency / sample_rate`, in fractional
/// samples per output frame. The 25 bits of fraction match
/// the existing `M = 25` constant in `state_sample.rs`.
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
#[repr(transparent)]
#[serde(transparent)]
pub struct SampleStep(Q7_25);

impl SampleStep {
    /// Zero step (voice disabled).
    pub const ZERO: Self = Self(Q7_25::ZERO);

    /// From a raw Q7.25 value.
    #[inline]
    pub const fn from_raw_q7_25(q: Q7_25) -> Self {
        Self(q)
    }

    /// Underlying Q7.25.
    #[inline]
    pub const fn into_q7_25(self) -> Q7_25 {
        self.0
    }

    /// Build from `frequency / sample_rate`. Saturates if
    /// the frequency drives the step past 7-integer-bit
    /// range (which can happen with extreme S3M portamento).
    #[inline]
    pub fn for_frequency(freq: Frequency, rate: SampleRate) -> Self {
        if rate.0 == 0 {
            return Self::ZERO;
        }
        // `Frequency` is Q24.8 (`raw = Hz << 8`). We want a
        // Q7.25 step, i.e. `freq / rate` with 25 fractional
        // bits. So we want `(hz << 25) / rate`, which is
        // `(Q24.8 << 17) / rate`. Widen to `u64` for the
        // shift+divide so high frequencies don't truncate.
        let numerator = (freq.raw_q24_8() as u64) << 17;
        let q = numerator / rate.0 as u64;
        // Saturate the integer part to 7 bits.
        let q = if q > u32::MAX as u64 {
            u32::MAX
        } else {
            q as u32
        };
        Self(Q7_25::from_raw(q))
    }
}

// =====================================================================
// NOTE-RETRIG MULTIPLIER (uncommon, > 1.0 possible)
// =====================================================================

/// Q3.13 multiplier used by the FT2 / S3M `Rxy` /`Qxy` retrig
/// volume modifier. The standard nibbles produce values
/// `1/2, 11/16, 5/8, 3/2, 2`, which do not all fit in Q1.15
/// — `3/2` and `2` would saturate. Q3.13 gives range `[-4, 4)`
/// with step `2⁻¹³ ≈ 1.2e-4`, plenty for this cold path.
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
#[repr(transparent)]
#[serde(transparent)]
pub struct RetrigMul(i16); // Q3.13

impl RetrigMul {
    /// Identity multiplier.
    pub const UNITY: Self = Self(0x2000);

    /// From a raw Q3.13.
    #[inline]
    pub const fn from_raw_q3_13(raw: i16) -> Self {
        Self(raw)
    }

    /// Underlying Q3.13 raw value.
    #[inline(always)]
    pub const fn raw_q3_13(self) -> i16 {
        self.0
    }

    /// From a numerator / denominator at compile time. The
    /// FT2 / S3M nibble lookup is built with this. Both
    /// arguments are `i16` (RetrigMul is itself Q3.13 / `i16`,
    /// and every caller passes a small literal).
    #[inline]
    pub const fn from_ratio(num: i16, den: i16) -> Self {
        if den == 0 {
            return Self::UNITY;
        }
        let scaled = ((num as i32) << 13) / den as i32;
        let scaled = if scaled > i16::MAX as i32 {
            i16::MAX as i32
        } else if scaled < i16::MIN as i32 {
            i16::MIN as i32
        } else {
            scaled
        };
        Self(scaled as i16)
    }

    /// Apply to a [`Volume`], saturating to `[0, ONE]`.
    #[inline]
    pub const fn applied_to(self, v: Volume) -> Volume {
        // Q3.13 × Q1.15 → Q4.28, narrow back to Q1.15.
        let prod = (self.0 as i32) * (v.raw().raw() as i32);
        let prod = prod + (1 << 12);
        let r = prod >> 13;
        let r = if r > 0x7FFF {
            0x7FFF
        } else if r < 0 {
            0
        } else {
            r
        };
        Volume(Q15::from_raw(r as i16))
    }
}

// =====================================================================
// PHASE — Q0.16 unsigned cyclic position used by LFOs / waveform
// =====================================================================

/// LFO / waveform phase, conceptually a fraction of a full cycle
/// in `[0, 1)`. Stored as `u16` so the `wrapping_add` IS the
/// cycle wrap (full cycle = `u16::MAX + 1`); a phase increment
/// modular-adds naturally with no extra modulo.
///
/// This is **not** a `Q1.15` (signed `i16`): a phase is always
/// non-negative and traverses `[0, 1)` uniformly. The `Phase ↔
/// Q15` conversions are explicit (see [`Self::to_q15_unsigned`]
/// and [`Self::raw_as_i16`]).
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Debug, Default)]
#[repr(transparent)]
#[serde(transparent)]
pub struct Phase(u16);

impl Phase {
    /// Cycle origin.
    pub const ZERO: Self = Self(0);
    /// 1/4 cycle (90°, π/2).
    pub const QUARTER: Self = Self(0x4000);
    /// 1/2 cycle (180°, π).
    pub const HALF: Self = Self(0x8000);
    /// 3/4 cycle (270°, 3π/2).
    pub const THREE_QUARTERS: Self = Self(0xC000);

    /// From a raw `u16`.
    #[inline]
    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }

    /// Underlying `u16`.
    #[inline(always)]
    pub const fn raw(self) -> u16 {
        self.0
    }

    /// Reinterpret the raw bits as `i16` (same memory pattern,
    /// no arithmetic). Used exclusively at the input of the
    /// signed sawtooth path (`RampDown`), where the cycle's
    /// natural `u16` wrap meets `i16::wrapping_neg`.
    #[inline(always)]
    pub const fn raw_as_i16(self) -> i16 {
        self.0 as i16
    }

    /// Phase shift, modular: the cycle wraps at the `u16`
    /// boundary, which IS exactly the full cycle.
    #[inline]
    pub const fn shifted(self, by: Self) -> Self {
        Self(self.0.wrapping_add(by.0))
    }

    /// Halve the position within the cycle. Equivalent to a
    /// right-shift on the underlying `u16`.
    #[inline]
    pub const fn halved(self) -> Self {
        Self(self.0 >> 1)
    }

    /// True iff in the first half-cycle (`[0, HALF)`).
    #[inline]
    pub const fn is_first_half(self) -> bool {
        self.0 < Self::HALF.0
    }

    /// Linear ramp `phase → Q1.15` in the unsigned half
    /// `[0, ONE]`. `Phase::ZERO → Q15::ZERO`,
    /// `Phase ≈ 1 → Q15::ONE`. Drops the LSB (the
    /// quantisation floor of Q1.15 is `1/32768`, while
    /// `Phase` runs at `1/65536`).
    #[inline]
    pub const fn to_q15_unsigned(self) -> Q15 {
        Q15::from_raw((self.0 >> 1) as i16)
    }
}

// =====================================================================
// READABILITY HELPERS — macro-generated `as_q15_*` / `from_q15_*`
// (and Q8.8 equivalents) on every Q-format newtype.
//
// Adding a new newtype? Wrap a `Q15` (or `Q8_8`), expose its
// `from_q15` / `from_q8_8` constructor, and add a single line
// here to inherit the readability surface. No hand-written
// `raw().raw() as i32` should appear in consumer code; if you
// reach for it, add another helper here instead.
// =====================================================================

impl_q15_readability!(Amp);
impl_q15_readability!(Volume);
impl_q15_readability!(ChannelVolume);
impl_q15_readability!(EnvValue);
impl_q15_readability!(GlobalVolume);
impl_q15_readability!(Panning);
impl_q15_readability!(Finetune);
impl_q8_8_readability!(Pitch);
impl_q8_8_readability!(PitchDelta);

// Operator overloading: saturating Neg/Add/Sub on the types
// where signed composition is musically meaningful.
//
// `Pitch` is NOT in this list: an absolute pitch + an absolute
// pitch is meaningless, and `Pitch - Pitch` returns
// `PitchDelta`, not `Pitch` — see the cross-type impls below.
impl_q15_ops!(Amp);
impl_q15_ops!(Finetune);
impl_q8_8_ops!(PitchDelta);

// Event-merge `Add` for the absolute gain / pan types. These
// types deliberately don't get the full `Neg/Add/Sub` pack,
// because *composing two current gains* is a multiplication
// (`scaled_by`), not an addition. But two ROW EVENTS touching
// the same attribute (e.g. two `TrackEffect::ChannelVolume`
// emitted on the same row) sum cumulatively, mirroring the
// historical f32 behaviour `(v1 + v2).clamp(0, 1)`. The
// saturating `+` plus an explicit `.clamp(ZERO, ONE)` at the
// call site does exactly that.
//
// The `Sub` and `Neg` are intentionally NOT added: subtracting
// or negating an absolute gain has no meaningful interpretation
// (a `-Volume` would be louder negative? quieter? something
// else?). If you find yourself wanting either, you probably
// want a delta type instead.
impl core::ops::Add for Volume {
    type Output = Self;
    /// Event-merge: saturating sum of two volumes. Caller is
    /// expected to `.clamp(Volume::SILENT, Volume::FULL)` if
    /// the sum needs to stay inside the unit interval (the
    /// merge code does exactly that).
    #[inline]
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl core::ops::Add for ChannelVolume {
    type Output = Self;
    /// Event-merge: saturating sum (see [`Volume`]'s `Add`
    /// docs).
    #[inline]
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl core::ops::Add for GlobalVolume {
    type Output = Self;
    /// Event-merge: saturating sum.
    #[inline]
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl core::ops::Add for Panning {
    type Output = Self;
    /// Event-merge: saturating sum (corner case — two pan
    /// effects on one row).
    #[inline]
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

// Cross-type pitch arithmetic: `Pitch + PitchDelta = Pitch`,
// `Pitch - PitchDelta = Pitch`, `Pitch - Pitch = PitchDelta`.
// These are the building blocks of every transposition step,
// every arpeggio offset, every vibrato deviation — having
// them as bare `+`/`-` makes the pitch chain readable.
impl core::ops::Add<PitchDelta> for Pitch {
    type Output = Pitch;
    #[inline]
    fn add(self, rhs: PitchDelta) -> Pitch {
        Pitch(self.0 + rhs.0)
    }
}

impl core::ops::Sub<PitchDelta> for Pitch {
    type Output = Pitch;
    #[inline]
    fn sub(self, rhs: PitchDelta) -> Pitch {
        Pitch(self.0 - rhs.0)
    }
}

impl core::ops::Sub for Pitch {
    type Output = PitchDelta;
    /// `Pitch - Pitch = PitchDelta` (a relative semitone offset).
    #[inline]
    fn sub(self, rhs: Pitch) -> PitchDelta {
        PitchDelta(self.0 - rhs.0)
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_full_times_sample_is_identity_within_1_lsb() {
        let s = Amp::from_q15(Q15::from_raw(12345));
        let out = Volume::FULL.apply(s);
        assert!((out.into_q15().raw() - 12345).abs() <= 1);
    }

    #[test]
    fn volume_silent_kills_signal() {
        let s = Amp::from_q15(Q15::from_raw(20000));
        assert_eq!(Volume::SILENT.apply(s), Amp::SILENCE);
    }

    #[test]
    fn volume_with_tremolo_clamps_above_one() {
        let v = Volume::from_q15(Q15::from_raw(0x7000));
        let modulated = v.with_tremolo(Q15::from_raw(0x4000));
        // Should clamp to ONE
        assert_eq!(modulated, Volume::FULL);
    }

    #[test]
    fn volume_with_tremolo_clamps_below_zero() {
        let v = Volume::from_q15(Q15::from_raw(0x2000));
        let modulated = v.with_tremolo(Q15::from_raw(-0x4000));
        // Should clamp to ZERO
        assert_eq!(modulated, Volume::SILENT);
    }

    #[test]
    fn pitch_acc_round_trip() {
        let p = Pitch::C4;
        let acc = PitchAcc::from_pitch(p);
        assert_eq!(acc.into_pitch(), p);
    }

    #[test]
    fn pitch_acc_clamps_low() {
        let acc = PitchAcc::from_pitch(Pitch::C0)
            .add_delta(PitchDelta::from_semitones(-50));
        assert_eq!(acc.into_pitch(), Pitch::C0);
    }

    #[test]
    fn pitch_acc_clamps_high() {
        let acc = PitchAcc::from_pitch(Pitch::B9)
            .add_delta(PitchDelta::from_semitones(50));
        assert_eq!(acc.into_pitch(), Pitch::B9);
    }

    #[test]
    fn pitch_quantize_glissando() {
        // 48.4 → 48
        let p = Pitch::from_q8_8(Q8_8::from_raw((48 << 8) + 0x66));
        assert_eq!(p.quantized(), Pitch::from_semitone(48));
        // 48.6 → 49
        let p = Pitch::from_q8_8(Q8_8::from_raw((48 << 8) + 0x9A));
        assert_eq!(p.quantized(), Pitch::from_semitone(49));
    }

    #[test]
    fn retrig_mul_basic_ratios() {
        // Identity
        let v = Volume::FULL;
        assert_eq!(RetrigMul::UNITY.applied_to(v), v);
        // Half
        let half = RetrigMul::from_ratio(1, 2);
        let out = half.applied_to(Volume::FULL);
        // Should be ≈ Q15::HALF
        assert!((out.raw().raw() - Q15::HALF.raw()).abs() <= 4);
        // Double
        let two = RetrigMul::from_ratio(2, 1);
        let half_volume = Volume::from_q15(Q15::HALF);
        let out = two.applied_to(half_volume);
        // Half × 2 saturates to FULL
        assert!(out.raw().raw() >= 0x7FF0);
    }

    #[test]
    fn frequency_octave_shift() {
        let f = Frequency::from_hz(440);
        assert_eq!(f.shift_octave(1), Frequency::from_hz(880));
        assert_eq!(f.shift_octave(-1), Frequency::from_hz(220));
    }

    #[test]
    fn sample_step_zero_rate_yields_zero() {
        let f = Frequency::from_hz(440);
        let r = SampleRate(0);
        assert_eq!(SampleStep::for_frequency(f, r), SampleStep::ZERO);
    }

    #[test]
    fn sample_step_basic() {
        // C-4 reference frequency at 48000 Hz output.
        let f = Frequency::from_hz(8372);
        let r = SampleRate(48_000);
        let s = SampleStep::for_frequency(f, r);
        // Expected: 8372 / 48000 ≈ 0.17441666… in Q7.25
        // = 8372 × 2^25 / 48000 = 280_917_704_704 / 48000
        // = 5_852_452.18… → 5_852_452 by truncation.
        let raw = s.into_q7_25().raw();
        assert!(
            (raw as i64 - 5_852_452).abs() < 4,
            "got raw {}, expected ~5_852_452",
            raw
        );
    }
}
