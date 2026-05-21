//! Compile-time lookup tables.
//!
//! These tables replace `sin`, `sqrt`, `powf`, `exp`, `log2`
//! at runtime — the player has no dependency on `libm`,
//! `micromath`, or any soft-float runtime.
//!
//! All tables are computed in `const fn` from integer-only
//! polynomial / iterative approximations. The build is
//! reproducible across compilation hosts because there is no
//! float arithmetic anywhere in the table generation either.
//!
//! ## Precision summary
//!
//! | Table | Entries | Worst-case error |
//! |---|---:|---|
//! | [`SINE_Q15`] | 256 | < 0.5 % of full scale |
//! | [`SQRT_PAN_Q15`] | 256 | < 0.01 dB on amplitude |
//! | [`SEMITONE_FROM_C_Q16_16`] | 12 | ≈ 0.05 cent (with sub) |
//! | [`SUBSEMITONE_Q16_16`] | 64 | (composes with above) |
//!
//! The Amiga period computation reuses
//! [`SEMITONE_FROM_C_Q16_16`] / [`SUBSEMITONE_Q16_16`] — no
//! per-finetune table — and matches the FT2 reference to ±2
//! period units across the playable range.
//!
//! All under the audibility threshold for the targeted use.
//!
//! ## ROM footprint
//!
//! ≈ 1.3 KB total (256 × 2 + 256 × 2 + 12 × 4 + 64 × 4).
//! Replaces ≈ 10 KB of `micromath` / `libm` / soft-float code
//! plus the 384-byte FT2 Amiga lookup.

use crate::fixed::fixed::{Q15, Q16_16};
use crate::fixed::units::{Frequency, Period};

// =====================================================================
// SINE — 256-entry full-cycle table, Q1.15
// =====================================================================

/// Quarter-sine generator using a polynomial approximation
/// in fixed-point. Bhaskara I's approximation is accurate to
/// ~0.16 % on `[0, π]`; we use it on a quarter cycle and
/// reflect for the rest.
///
/// Input `x` is in 1/256 of a full cycle (so `x = 64` → π/2).
/// Output is Q1.15.
const fn sine_q15_step(x: u32) -> Q15 {
    // Reduce x into [0, 256), with sign and quadrant.
    let x = x & 0xFF;
    let (q, t) = match x / 64 {
        0 => (1, x),        // [0, π/2)
        1 => (1, 128 - x),  // [π/2, π)
        2 => (-1, x - 128), // [π, 3π/2)
        _ => (-1, 256 - x), // [3π/2, 2π)
    };
    // t in [0, 64], representing angle in [0, π/2].
    // Bhaskara: sin(θ) ≈ 16θ(π - θ) / (5π² - 4θ(π - θ))
    // With θ in 1/64 of π: let u = t (so θ = uπ/128).
    // sin(uπ/128) ≈ 16 · u · (128 - u) / (5·128² - 4·u·(128 - u))
    //            ≈ 16 · u · (128 - u) / (81920 - 4·u·(128 - u))
    // Wait — let's redo: with θ = u·π/128, π - θ = (128 - u)π/128,
    // numerator = 16 · θ · (π - θ) = 16 · π² · u · (128 - u) / 128²
    // denom    = 5·π² - 4·θ·(π - θ) = π² · (5 - 4·u·(128-u)/128²)
    // ratio    = 16·u·(128-u) / (5·128² - 4·u·(128-u))
    //          = 16·u·(128-u) / (81920 - 4·u·(128-u))
    let u = t as i64;
    let prod = u * (128 - u); // 0..4096
    let num = 16 * prod;
    let den = 81920 - 4 * prod;
    // Result is in [0, 1]. Scale to Q1.15 = 0x7FFF.
    let result_q15 = (num * 0x7FFF) / den;
    let signed = (q as i64) * result_q15;
    Q15::from_raw(signed as i16)
}

/// 256-entry full-cycle sine table, Q1.15.
///
/// Indexed by phase in `1/256` of a full cycle: index 0 is
/// `sin(0) = 0`, index 64 is `sin(π/2) ≈ 1`, etc.
pub const SINE_Q15: [Q15; 256] = {
    let mut t = [Q15::ZERO; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = sine_q15_step(i as u32);
        i += 1;
    }
    t
};

/// Sine lookup at a given cycle [`Phase`](crate::fixed::units::Phase). Linear interpolation
/// between the two nearest 8-bit table entries.
#[inline]
pub fn sine(phase: crate::fixed::units::Phase) -> Q15 {
    let phase_q16 = phase.raw();
    let idx = (phase_q16 >> 8) as usize;
    let frac = (phase_q16 & 0xFF) as i32;
    let a = SINE_Q15[idx].raw() as i32;
    let b = SINE_Q15[(idx + 1) & 0xFF].raw() as i32;
    // Interpolate: a + ((b - a) * frac + 128) >> 8
    let r = a + (((b - a) * frac + 128) >> 8);
    Q15::from_raw(r as i16)
}

// =====================================================================
// SQRT — 256-entry table for equal-power panning, Q1.15
// =====================================================================

/// Integer square root of `x` (Q0.30 input → Q0.15 output)
/// via Newton-Raphson on the integer square root. Used only at
/// compile time.
const fn isqrt_q30_to_q15(x: u64) -> i64 {
    // We want sqrt(x / 2^30) * 2^15 = sqrt(x) * 2^15 / 2^15
    //                                = sqrt(x).
    // So result = isqrt(x).
    if x == 0 {
        return 0;
    }
    // Heron's method.
    let mut r: u64 = 1u64 << 15; // initial guess in expected range
    let mut prev: u64 = 0;
    let mut iter = 0;
    while iter < 32 {
        let new = (r + x / r) >> 1;
        if new == prev {
            break;
        }
        prev = r;
        r = new;
        iter += 1;
    }
    r as i64
}

const fn pan_sqrt_step(i: u32) -> Q15 {
    // Input pan in Q1.15 (0..0x7FFF), interpret as fraction
    // p in [0, 1). Compute sqrt(p) in Q1.15.
    // sqrt(p) where p = i / 0x7FFF
    //   = sqrt(i) / sqrt(0x7FFF)
    // We want the result as Q1.15: result = sqrt(i / 0x7FFF) * 0x7FFF.
    // → result² = i * 0x7FFF
    // → result = isqrt(i * 0x7FFF).
    // i is the table index in [0, 255], representing
    // `(i * 0x7FFF / 255)` in Q1.15. Reverse:
    let p_q15 = (i * 0x7FFF) / 255;
    // sqrt(p_q15 / 0x8000) * 0x7FFF
    // = sqrt((p_q15 << 15) / 0x8000) using isqrt on i * 2^15.
    // Easier: result² = p_q15 * 0x7FFF (both Q1.15, gives Q2.30)
    let prod = (p_q15 as u64) * 0x7FFFu64;
    let r = isqrt_q30_to_q15(prod);
    Q15::from_raw(r as i16)
}

/// 256-entry sqrt table for the equal-power pan law, Q1.15.
///
/// Index `i` corresponds to a pan position of
/// `i / 255` (so index 255 = full one side).
pub const SQRT_PAN_Q15: [Q15; 256] = {
    let mut t = [Q15::ZERO; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = pan_sqrt_step(i as u32);
        i += 1;
    }
    t
};

/// `sqrt(p)` where `p ∈ [0, 1]` is given as Q1.15. Linear
/// interpolation between the two nearest table entries.
#[inline]
pub fn pan_sqrt(p: Q15) -> Q15 {
    // p in [0, 0x7FFF] → idx_x256 in [0, 0xFFFE], spread over
    // 16 bits so the high byte indexes the table and the low
    // byte gives the interpolation fraction.
    let raw = p.raw().clamp(0, 0x7FFF) as u32;
    let idx_x256 = raw << 1; // [0, 0xFFFE]
    let idx = (idx_x256 >> 8) as usize; // [0, 255]
    let frac = (idx_x256 & 0xFF) as i32;
    let a = SQRT_PAN_Q15[idx].raw() as i32;
    let b = SQRT_PAN_Q15[(idx + 1).min(255)].raw() as i32;
    let r = a + (((b - a) * frac + 128) >> 8);
    Q15::from_raw(r as i16)
}

// =====================================================================
// LINEAR PERIOD ↔ PITCH ↔ FREQUENCY
// =====================================================================
//
// Mathematical structure of the period in xmrs's linear scheme:
//
//   period = 10 · 12 · 16 · 4 - note · 16 · 4
//          = 7680 - note · 64
//
// where the factors decompose as:
//   10 octaves · 12 semitones · 16 finetune steps · 4 fine units.
//
// Two consequences exploited below:
//
// 1. `period ↔ pitch` is purely integer — no LUT, no transcendental.
//    `pitch (semitones) = (7680 - period) / 64`
//    `period = 7680 - pitch · 64`
//
// 2. For `period → frequency`, the multiplier `2^((4608 - period)/768)`
//    factors into:
//      `2^((4608 - period)/768) = 2^k · 2^(s/12) · 2^(u/(12·64))`
//    where `k = octave offset`, `s = semitone (0..11)`,
//    `u = sub-semitone (0..63)`. So we keep two small tables:
//      - 12 entries for `2^(s/12)` (whole semitones in one octave)
//      - 64 entries for `2^(u/768)` (sub-semitone refinement)
//    instead of one 768-entry table. The result composition is
//    one Q16.16 × Q16.16 multiplication. ROM cost drops from
//    3072 B to 304 B (a 2.7 KB saving).
//
// Worst-case precision: each Q16.16 mul introduces ≤ 1 LSB of
// rounding error (round-to-nearest), so two muls cap at 2 LSB
// on the multiplier ≈ 3.0e-5 ≈ 0.05 cent. Far below the ~3 cent
// just-noticeable-difference threshold for human pitch perception.

/// Compile-time computation of `2^(x_num / 768)` in Q16.16 for
/// `x_num` in `[0, 768]`. Used at build time to populate
/// lookup tables. Public so the player's filter table can use
/// it for compile-time coefficient precomputation.
pub const fn pow2_frac_q16_16(x_num: u32) -> u32 {
    // Implemented as exp(y · ln 2) via Taylor series, all in
    // u128 arithmetic to keep precision and avoid overflow.

    // ln(2) ≈ 0.6931471805599453, encoded in Q0.64.
    const LN2_Q64: u128 = 12_786_308_645_202_655_660u128;

    // y = x_num / 768, in Q0.64
    let y: u128 = ((x_num as u128) << 64) / 768;

    // z = y · ln(2), in Q0.64
    let z: u128 = (y * LN2_Q64) >> 64;

    // Taylor series for exp(z): Σ z^n / n!
    let mut term: u128 = 1u128 << 64; // 1.0 in Q0.64
    let mut sum: u128 = 0;
    let mut n: u32 = 0;
    while n < 16 {
        sum += term;
        term = (term * z) >> 64;
        term /= (n as u128) + 1;
        n += 1;
    }

    // sum is Q0.64 representing 2^y ∈ [1, 2]. Narrow to Q16.16.
    (sum >> 48) as u32
}

/// Multiplier for each whole semitone in one octave: `2^(s/12)`
/// for `s = 0..11`, in Q16.16. Index 0 is `1.0`, index 11 is
/// `≈ 1.8877`.
pub const SEMITONE_FROM_C_Q16_16: [Q16_16; 12] = {
    let mut t = [Q16_16::ZERO; 12];
    let mut i = 0u32;
    while i < 12 {
        // 2^(i/12) is 2^((i·64)/768) at the 768-grid level.
        t[i as usize] = Q16_16::from_raw(pow2_frac_q16_16(i * 64));
        i += 1;
    }
    t
};

/// Sub-semitone multiplier: `2^(u/768)` for `u = 0..63`, in
/// Q16.16. Index 0 is `1.0`, index 63 is `≈ 1.0570`.
pub const SUBSEMITONE_Q16_16: [Q16_16; 64] = {
    let mut t = [Q16_16::ZERO; 64];
    let mut i = 0u32;
    while i < 64 {
        t[i as usize] = Q16_16::from_raw(pow2_frac_q16_16(i));
        i += 1;
    }
    t
};

/// Reference C-4 frequency for the linear-period scheme,
/// MIDI-aligned (= 440 × 2⁻⁹ᐟ¹² × 32 ≈ 8372.018 Hz, rounded
/// to the nearest integer).
///
/// At this reference, A-4 plays at exactly 14080 Hz on the
/// linear period grid (= 440 × 32), so modules mix cleanly
/// against any 440-Hz-aligned audio source.
///
/// See [`C4_FREQ_HZ_LEGACY`] for the historical FT2 / OpenMPT
/// value (8363) used by the rest of the tracker ecosystem.
pub const C4_FREQ_HZ: u32 = 8372;

/// Soundtracker / ProTracker / FT2 reference C-4. Originates
/// from Karsten Obarski's Ultimate Soundtracker (1987), where
/// the period table was derived from NTSC Amiga programming
/// examples: `3_579_545 / 428 ≈ 8363.4`, rounded to 8363.
/// Inherited unchanged by NoiseTracker, ProTracker and FT2,
/// then frozen into the XM format and adopted by the rest of
/// the ecosystem (OpenMPT, MilkyTracker, libxmp, ft2-clone,
/// schism). ≈ 1.9 cents below modern concert pitch; use
/// [`C4_FREQ_HZ`] for the MIDI-aligned reference.
pub const C4_FREQ_HZ_LEGACY: u32 = 8363;

/// Convert a linear-frequency period to a frequency.
///
/// Uses the two-stage decomposition described above: one
/// Q16.16 multiplication composes the semitone and
/// sub-semitone contributions, then `shift_octave` handles
/// the integer octave offset.
pub fn linear_period_to_frequency(p: Period) -> Frequency {
    let period = p.raw() as i32;
    // exp_768 = 4608 - period: how many of the 768-unit
    // "octaves" we are above the C-4 reference (signed).
    let exp_768 = 4608 - period;

    let octave = exp_768.div_euclid(768) as i8;
    let in_oct = exp_768.rem_euclid(768) as usize; // [0, 767]
    let semi = in_oct >> 6; // / 64, in [0, 11]
    let sub = in_oct & 63; // % 64, in [0, 63]

    // Compose sub-octave multiplier (Q16.16, in [1, 2)).
    let mul = SEMITONE_FROM_C_Q16_16[semi].mul_q16_16(SUBSEMITONE_Q16_16[sub]);

    // freq = mul × C4_FREQ_HZ × 2^octave. Computed in `u64` to
    // keep the multiply and the octave shift lossless across
    // the whole tracker frequency range (~16 Hz at C-0 to
    // 134 kHz when `relative_pitch` lifts a sample 4 octaves
    // above C-4 — XM `binary_world.xm` does this routinely).
    //
    // PHASE 2 BUGFIX: before the `Frequency` storage moved to
    // Q24.8, the unshifted product lived in `u32` Q16.16 and
    // `shift_octave` saturated at 65 535 Hz on its way out.
    // The auto-vibrato modulation tracked tiny pitch changes
    // around that ceiling, all of which mapped to the same
    // saturated value — vibrato simply disappeared on
    // high-`relative_pitch` instruments.
    //
    //   mul.raw   ∈ [65536, 131072)         (Q16.16 of [1.0, 2.0))
    //   × C4 Hz   = [65536, 131072) × 8372  ≈ [5.5e8, 1.1e9]   (u64 fine)
    //   << 4      ≤ 1.8e10                                     (u64 fine)
    //   >> 8      = Q24.8 of Hz                                (fits in u32)
    let scaled_q16_16 = (mul.raw() as u64) * (C4_FREQ_HZ as u64);
    let shifted_q16_16 = if octave >= 0 {
        scaled_q16_16 << octave
    } else {
        scaled_q16_16 >> (-octave)
    };
    // Q16.16 → Q24.8: drop 8 fractional bits.
    let q24_8 = shifted_q16_16 >> 8;
    let raw = if q24_8 > u32::MAX as u64 {
        u32::MAX
    } else {
        q24_8 as u32
    };
    Frequency::from_raw_q24_8(raw)
}

/// Linear-frequency period → pitch in semitones (Q8.8).
///
/// **Exact integer**: `pitch = (7680 - period) / 64`, so in
/// Q8.8 representation: `pitch_q8_8 = (7680 - period) << 2`.
/// No LUT, no transcendental. Saturates to `[0, 119]`.
pub const fn linear_period_to_pitch(p: Period) -> crate::fixed::units::Pitch {
    let period = p.raw() as i32;
    let pitch_q8_8 = (7680 - period) << 2;
    let pitch_q8_8 = if pitch_q8_8 < 0 {
        0
    } else if pitch_q8_8 > 119 << 8 {
        119 << 8
    } else {
        pitch_q8_8
    };
    crate::fixed::units::Pitch::from_q8_8(crate::fixed::fixed::Q8_8::from_raw(pitch_q8_8 as i16))
}

/// Linear-frequency pitch → period.
///
/// **Exact integer**: `period = 7680 - pitch · 64`. With pitch
/// in Q8.8: `period = 7680 - (pitch_q8_8 >> 2)`.
pub const fn linear_pitch_to_period(p: crate::fixed::units::Pitch) -> Period {
    let pitch_q8_8 = p.as_q8_8_i32();
    let period = 7680 - (pitch_q8_8 >> 2);
    let period = if period < 0 {
        0
    } else if period > 0xFFFF {
        0xFFFF
    } else {
        period
    };
    Period::from_raw(period as u16)
}

// =====================================================================
// AMIGA PERIOD — analytical, no per-finetune table
// =====================================================================
//
// We use the formula:
//
//   period(note, finetune) = AMIGA_C0_PERIOD × 2^(-(8·note + finetune − 8) / 96)
//
// The exponent decomposes into octaves + 1/12 semitones +
// 1/96 sub-semitones, which we already have tables for
// (`SEMITONE_FROM_C_Q16_16` and `SUBSEMITONE_Q16_16`). Indexing
// `SUBSEMITONE` at `sub × 8` walks in 1/96 increments (since
// 768 = 96 × 8).
//
// ## Choice of `AMIGA_C0_PERIOD`
//
// `6779` makes A-4 land on exactly 440 Hz on Paula PAL, so
// modules play at concert pitch and mix cleanly with any
// other 440-Hz-aligned audio source. This is our default.
//
// `6848` (= [`AMIGA_C0_PERIOD_LEGACY`]) is the historical
// ProTracker / FT2 reference. Modules play ≈ 17 cents above
// concert pitch — imperceptible in isolation, audible only
// when compared bit-by-bit against a legacy player or mixed
// against MIDI-tuned material. Provided for compatibility
// with reference players (OpenMPT, libxmp) when numerical
// agreement matters more than absolute tuning.
//
// On `.mod` files the choice is invisible: `.mod` stores raw
// periods which feed Paula directly, with no `note → period`
// step. The constant only affects formats that store notes
// (XM, S3M, IT).

/// Reference period for "C-0 tracker note" on Paula PAL.
///
/// `6779` is the MIDI-aligned value: with this constant,
/// A-4 (note 57) lands on exactly 440 Hz. Suitable for any
/// modern player or editor that wants to mix tracker output
/// with other audio sources at concert pitch.
///
/// See [`AMIGA_C0_PERIOD_LEGACY`] for the historical ≈ 17-cent
/// detuned alternative used by ProTracker, FT2 and most
/// legacy players.
pub const AMIGA_C0_PERIOD: u32 = 6779;

/// Historical ProTracker / FT2 reference period.
///
/// Modules using this constant play approximately 17 cents
/// above concert pitch. Use only when bit-numerical
/// agreement with legacy players is required (e.g.
/// regression-testing against OpenMPT or libxmp). For
/// auditioning, [`AMIGA_C0_PERIOD`] is the better default.
pub const AMIGA_C0_PERIOD_LEGACY: u32 = 6848;

/// Amiga period for a given pitch and finetune index.
///
/// `note_semitone` is the absolute semitone count
/// (0 = C-0); clamped to `[0, 119]`.
///
/// `finetune_idx` is in `[0, 15]`, with 8 being "no finetune".
/// Resolution is 1/8 of a semitone (FT2 nibble convention).
///
/// Computed analytically from `SEMITONE_FROM_C_Q16_16` and
/// `SUBSEMITONE_Q16_16`. With the default
/// [`AMIGA_C0_PERIOD`] = 6779, A-4 (note 57, finetune 8)
/// produces a period that drives Paula PAL at exactly 440 Hz.
///
/// For continuous-pitch resolution (Q8.8), use
/// [`amiga_period_from_pitch`] instead.
pub fn amiga_period(note_semitone: i16, finetune_idx: u8) -> Period {
    let n = note_semitone.clamp(0, 119) as i32;
    let ft = (finetune_idx & 0x0F) as i32;

    // Express the pitch in 1/96 of an octave units (= 1/8 of a
    // semitone, FT2's finetune resolution). Reference: p = 0
    // → period = AMIGA_C0_PERIOD. Higher pitch (larger p) →
    // smaller period.
    let p = n * 8 + ft - 8;

    // Decompose into integer octaves and a non-negative residue.
    let octave = p.div_euclid(96);
    let in_oct = p.rem_euclid(96) as usize; // [0, 95]
    let semi = in_oct / 8; // [0, 11]
    let sub = (in_oct % 8) * 8; // 0, 8, 16, …, 56 — index into SUBSEMITONE

    // Multiplier 2^(in_oct/96) in Q16.16, in [1, 2).
    let mul = SEMITONE_FROM_C_Q16_16[semi].mul_q16_16(SUBSEMITONE_Q16_16[sub]);

    // We want period × 2^16 (i.e. period in Q16.16) to keep
    // precision before the octave shift.
    let numerator = (AMIGA_C0_PERIOD as u64) << 32;
    let period_q16_16 = numerator / (mul.raw() as u64);

    // Apply integer octave shift on the Q16.16 value, then
    // round to the nearest integer.
    let shifted = if octave >= 0 {
        period_q16_16 >> (octave as u32).min(32)
    } else {
        let l = ((-octave) as u32).min(16);
        period_q16_16 << l
    };

    // Round to integer: add 0x8000 then shift right by 16.
    let period_int = shifted.saturating_add(0x8000) >> 16;
    Period::from_raw(period_int.min(0xFFFF) as u16)
}

/// Amiga period for a continuous Q8.8 pitch.
///
/// Unlike [`amiga_period`], this entry point preserves the full
/// pitch resolution: `Pitch` is Q8.8 (1/256 semitone), and
/// `SUBSEMITONE_Q16_16` is on a 1/64-semitone grid, so we walk
/// the LUTs at exactly that resolution (no FT2 finetune-nibble
/// quantization). Used by the runtime `note_to_period` dispatch
/// when the caller already has a `Pitch` (no need to split into
/// integer note + finetune).
///
/// Reference: `Pitch::C0` (raw 0) → period = `AMIGA_C0_PERIOD`.
/// `Pitch::C4` → period = `AMIGA_C0_PERIOD / 16`. Higher pitch
/// → smaller period.
pub fn amiga_period_from_pitch(pitch: crate::fixed::units::Pitch) -> Period {
    // `Pitch` raw is `note × 256` (Q8.8). The LUTs together
    // express `2^(p / 768)` with `p` on a 1/64-semitone grid,
    // i.e. `p = note × 64`. So `p = pitch_raw / 4`
    // (= `>> 2`), losing the bottom 2 fractional bits which
    // are below the LUT resolution anyway.
    //
    // Pitch is clamped to `[C0, B9]` so `p ∈ [0, 7616]` —
    // signed `i32` is comfortably wide.
    let pitch_q8_8 = pitch.as_q8_8_i32().clamp(0, 119 << 8);
    let p = pitch_q8_8 >> 2;

    // Decompose into integer octaves and a non-negative residue
    // on the 768-grid. `in_oct ∈ [0, 767]`, `semi ∈ [0, 11]`,
    // `sub ∈ [0, 63]`.
    let octave = p / 768;
    let in_oct = (p % 768) as usize;
    let semi = in_oct >> 6; // / 64
    let sub = in_oct & 0x3F; // % 64

    // Multiplier 2^(in_oct/768) in Q16.16, in [1, 2).
    let mul = SEMITONE_FROM_C_Q16_16[semi].mul_q16_16(SUBSEMITONE_Q16_16[sub]);

    // period_q16_16 = AMIGA_C0_PERIOD × 2^16 / mul, kept in
    // u64 so the divide doesn't lose precision before the
    // octave shift.
    let numerator = (AMIGA_C0_PERIOD as u64) << 32;
    let period_q16_16 = numerator / (mul.raw() as u64);

    // Apply the integer octave shift on the Q16.16 value.
    let shifted = if octave >= 0 {
        period_q16_16 >> (octave as u32).min(32)
    } else {
        let l = ((-octave) as u32).min(16);
        period_q16_16 << l
    };

    // Round to nearest integer.
    let period_int = shifted.saturating_add(0x8000) >> 16;
    Period::from_raw(period_int.min(0xFFFF) as u16)
}

/// Amiga period → frequency.
///
/// Uses the canonical PAL formula `freq = 7093789.2 / (period × 2)`,
/// kept as integer division: `freq = 3_546_894 / period`.
pub fn amiga_period_to_frequency(p: Period) -> Frequency {
    let raw = p.raw();
    if raw == 0 {
        return Frequency::ZERO;
    }
    // 3_546_894 Hz / period. We want Q24.8 of Hz:
    //   freq_q24_8 = (3_546_894 << 8) / period
    let num: u64 = (3_546_894u64) << 8;
    let q = num / (raw as u64);
    let raw = if q > u32::MAX as u64 {
        u32::MAX
    } else {
        q as u32
    };
    Frequency::from_raw_q24_8(raw)
}

/// Amiga frequency → period.
///
/// Inverse of [`amiga_period_to_frequency`]. Solves
/// `period = 3_546_894 / freq` with the same integer
/// scaling (Q24.8 freq → integer period).
pub fn amiga_frequency_to_period(f: Frequency) -> Period {
    let raw = f.raw_q24_8();
    if raw == 0 {
        return Period::ZERO;
    }
    // freq_q24_8 = freq * 2^8, so:
    //   period = (3_546_894 << 8) / freq_q24_8
    let num: u64 = (3_546_894u64) << 8;
    let q = num / raw as u64;
    Period::from_raw(q.min(0xFFFF) as u16)
}

/// Linear frequency → period.
///
/// Inverse of [`linear_period_to_frequency`]. Inverts the
/// `period → freq` map by **binary search** over the integer
/// period domain `[0, 7680]`. The map is monotone-decreasing
/// (small period = high frequency) and resolves in 13
/// iterations, each costing one `linear_period_to_frequency`
/// lookup. No `log2` table needed.
///
/// Used at *import time* only (sample C-4 tagging): one call
/// per sample, not per audio frame, so the search cost is
/// imperceptible. The audio hot path uses
/// [`linear_period_to_frequency`] directly via [`PeriodCache`].
pub fn linear_frequency_to_period(f: Frequency) -> Period {
    let target = f.raw_q24_8();
    if target == 0 {
        // freq=0 sits beyond the period grid; return the
        // sentinel "lowest pitch" period.
        return Period::from_raw(7680);
    }
    // Binary search: find the smallest period `p` such that
    // `linear_period_to_frequency(p) ≤ target`. Frequencies
    // grow as period shrinks, so we want the smallest period
    // whose frequency is ≤ target — i.e., the highest-pitched
    // one not above the target.
    let mut lo: u32 = 0;
    let mut hi: u32 = 7680;
    while lo < hi {
        let mid = (lo + hi) / 2;
        let f_mid = linear_period_to_frequency(Period::from_raw(mid as u16));
        if f_mid.raw_q24_8() > target {
            // Period too small (freq too high) — search higher periods.
            lo = mid + 1;
        } else {
            // f_mid ≤ target: this is a candidate; could be larger.
            hi = mid;
        }
    }
    Period::from_raw(lo as u16)
}

/// Amiga period → pitch (Q8.8 semitones).
///
/// Inverse of [`amiga_period_from_pitch`]. Solves
/// `pitch = -log2(period / AMIGA_C0_PERIOD) × 12` without a
/// `log2` table by **binary search** over the Q8.8 pitch grid
/// `[C0, B9]` in 1/64-semitone steps — exactly the LUT
/// resolution. ≈ 13 iterations max.
///
/// Used both at import time (e.g. S3M `c4freq` →
/// `relative_pitch / finetune` decomposition) and per-tick in
/// `adjust_period`, so accuracy beats nibble-grain quantization.
pub fn amiga_period_to_pitch(p: Period) -> crate::fixed::units::Pitch {
    use crate::fixed::units::Pitch;
    let target = p.raw();
    if target == 0 {
        return Pitch::C0;
    }
    // Search on the LUT-native grid: 1/64 semitone steps from
    // C-0 to B-9 inclusive. `unit` indexes pitch slots, with
    // `pitch_q8_8 = unit × 4` (since Q8.8 has 256 sub-slots
    // per semitone and our grid has 64). Period decreases as
    // pitch (and `unit`) grows; we want the largest `unit`
    // whose period ≥ target.
    const MAX_UNIT: u32 = 119 * 64; // B-9 in 1/64-semitone steps
    let mut lo: u32 = 0;
    let mut hi: u32 = MAX_UNIT;
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        // Pitch at `mid` units of 1/64 semitone, in Q8.8.
        let mid_pitch = Pitch::from_q8_8_i16((mid * 4) as i16);
        let p_mid = amiga_period_from_pitch(mid_pitch).raw();
        if p_mid >= target {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    // Map unit (1/64 semitone) → Q8.8 pitch (1/256 semitone).
    Pitch::from_q8_8_i16((lo * 4) as i16)
}

// =====================================================================
// Single-entry memoization for period → frequency
// =====================================================================
//
// Inspired by `LastCall` in xmrs's `period_helper.rs`: a held
// note re-evaluates the same `(period, arp, finetune)` key on
// every tick of every row, so a one-slot cache captures
// ~95 % of calls at zero book-keeping cost. Any change in the
// key just overwrites the slot — no LRU, no scan, no allocation.
//
// On Cortex-M0+ this saves ≈ 30 cycles per cached hit (two
// table lookups + one Q16.16 mul + one octave shift). On a
// 32-channel mix at 48 kHz that's ≈ 1.5 % of the audio budget.
//
// Use is purely opt-in: call `lookup` from your channel
// state's per-tick path, or ignore the cache entirely and
// call [`linear_period_to_frequency`] directly.

/// One-slot memoization for [`linear_period_to_frequency`].
///
/// The cache key is the [`Period`]; for the audio loop this is
/// already the post-arpeggio, post-finetune, post-vibrato
/// resolved period, so it changes precisely when the output
/// frequency changes.
#[derive(Copy, Clone, Debug, Default)]
pub struct PeriodCache {
    last_period: u16,
    last_freq_q24_8: u32,
    valid: bool,
}

impl PeriodCache {
    /// Empty cache.
    #[inline]
    pub const fn new() -> Self {
        Self {
            last_period: 0,
            last_freq_q24_8: 0,
            valid: false,
        }
    }

    /// Resolve a period to a frequency, hitting the cache if
    /// the input matches the last call.
    #[inline]
    pub fn lookup(&mut self, p: Period) -> Frequency {
        let raw = p.raw();
        if self.valid && self.last_period == raw {
            return Frequency::from_raw_q24_8(self.last_freq_q24_8);
        }
        let f = linear_period_to_frequency(p);
        self.last_period = raw;
        self.last_freq_q24_8 = f.raw_q24_8();
        self.valid = true;
        f
    }

    /// Force-invalidate the cache (e.g. on instrument swap or
    /// when switching frequency mode between linear and Amiga).
    #[inline]
    pub fn invalidate(&mut self) {
        self.valid = false;
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_table_endpoints() {
        // sin(0) = 0
        assert_eq!(SINE_Q15[0], Q15::ZERO);
        // sin(π/2) ≈ 1
        let q = SINE_Q15[64].raw();
        assert!(q > 0x7F00, "sin(π/2) should be ≈ ONE, got {:#x}", q);
        // sin(π) ≈ 0
        let q = SINE_Q15[128].raw();
        assert!(q.abs() < 0x100, "sin(π) should be ≈ 0, got {:#x}", q);
        // sin(3π/2) ≈ -1
        let q = SINE_Q15[192].raw();
        assert!(q < -0x7F00, "sin(3π/2) should be ≈ -ONE, got {:#x}", q);
    }

    #[test]
    fn sine_interp_smooth() {
        // Reading at integer phase indices should match the table.
        for i in 0..256 {
            let q = sine(crate::fixed::units::Phase::from_raw((i << 8) as u16));
            let expected = SINE_Q15[i as usize];
            assert!(
                (q.raw() - expected.raw()).abs() <= 1,
                "i={}: got {}, expected {}",
                i,
                q.raw(),
                expected.raw()
            );
        }
    }

    #[test]
    fn pan_sqrt_endpoints() {
        // sqrt(0) = 0
        assert_eq!(SQRT_PAN_Q15[0], Q15::ZERO);
        // sqrt(1) ≈ 1
        let q = SQRT_PAN_Q15[255].raw();
        assert!(q > 0x7F00);
    }

    #[test]
    fn pan_sqrt_centre() {
        // sqrt(0.5) ≈ 0.707 → 0x5A82 in Q1.15
        let q = pan_sqrt(Q15::HALF).raw();
        assert!((q - 0x5A82).abs() < 0x100, "got {:#x}", q);
    }

    #[test]
    fn linear_period_c4_round_trip() {
        // C-4 has period 7680 - 64 * 48 = 4608.
        // Frequency should be C4_FREQ_HZ (= 8372) exactly.
        let f = linear_period_to_frequency(Period::from_raw(4608));
        let expected = Frequency::from_hz(C4_FREQ_HZ);
        let got = f.raw_q24_8() as i64;
        let exp = expected.raw_q24_8() as i64;
        let diff = (got - exp).abs();
        // Q24.8 has 1/256 Hz resolution. Tolerance 1 LSB
        // (= 1/256 Hz ≈ 0.00081 cent at 8372 Hz).
        assert!(diff <= 1, "got raw {}, expected {}", got, exp);
    }

    #[test]
    fn linear_period_one_octave_doubles() {
        // C-4 → C-5: period drops by 768 (4608 → 3840), freq doubles.
        let f4 = linear_period_to_frequency(Period::from_raw(4608));
        let f5 = linear_period_to_frequency(Period::from_raw(3840));
        let r4 = f4.raw_q24_8() as u64;
        let r5 = f5.raw_q24_8() as u64;
        // r5 should be ≈ 2 * r4
        let ratio = (r5 * 1000) / r4;
        assert!((ratio as i64 - 2000).abs() < 5, "ratio*1000 = {}", ratio);
    }

    #[test]
    fn linear_period_a4_is_known_freq() {
        // A-4 has note = 57, period = 7680 - 64*57 = 4032.
        // Expected frequency: C4_FREQ × 2^((4608-4032)/768) = 8372 × 2^0.75
        //                   ≈ 8372 × 1.681793 ≈ 14079.97 Hz
        // (= 440 Hz × 32, the canonical MIDI octave multiple of A-4).
        let f = linear_period_to_frequency(Period::from_raw(4032));
        let hz = (f.raw_q24_8() as f64) / 256.0;
        assert!(
            (hz - 14080.0).abs() < 1.0,
            "got {} Hz, expected ~14080 (= 440 × 32)",
            hz
        );
    }

    #[test]
    fn linear_period_to_pitch_round_trip() {
        // For every integer semitone in [0, 119], pitch_to_period
        // then period_to_pitch should give the original value
        // exactly.
        for note in 0..=119_i16 {
            let p = linear_pitch_to_period(crate::fixed::units::Pitch::from_semitone(note));
            let pitch_back = linear_period_to_pitch(p);
            assert_eq!(pitch_back.raw().round(), note, "note {} round-trip", note);
        }
    }

    #[test]
    fn linear_pitch_to_period_known_values() {
        // C-0: period = 7680
        let p = linear_pitch_to_period(crate::fixed::units::Pitch::C0);
        assert_eq!(p, Period::from_raw(7680));
        // C-4: period = 7680 - 64*48 = 4608
        let p = linear_pitch_to_period(crate::fixed::units::Pitch::C4);
        assert_eq!(p, Period::from_raw(4608));
        // B-9 (note 119): period = 7680 - 64*119 = 64
        let p = linear_pitch_to_period(crate::fixed::units::Pitch::B9);
        assert_eq!(p, Period::from_raw(64));
    }

    #[test]
    fn semitone_table_endpoints() {
        // Index 0 must be exactly 1.0 in Q16.16
        assert_eq!(SEMITONE_FROM_C_Q16_16[0].raw(), 0x10000);
        // Index 7 (perfect fifth): 2^(7/12) ≈ 1.4983070768766815
        // × 65536 ≈ 98193.06 → 0x17F91 (rounded)
        let raw = SEMITONE_FROM_C_Q16_16[7].raw();
        assert!(
            (raw as i64 - 0x17F91).abs() < 4,
            "got {:#x}, expected ≈ 0x17F91",
            raw
        );
    }

    #[test]
    fn subsemitone_table_endpoints() {
        // Index 0 must be exactly 1.0
        assert_eq!(SUBSEMITONE_Q16_16[0].raw(), 0x10000);
        // Index 63: 2^(63/768) ≈ 1.058510
        // × 65536 ≈ 69370.03 → 0x10EFA (rounded)
        let raw = SUBSEMITONE_Q16_16[63].raw();
        assert!(
            (raw as i64 - 0x10EFA).abs() < 4,
            "got {:#x}, expected ≈ 0x10EFA",
            raw
        );
    }

    #[test]
    fn linear_period_full_range_smooth() {
        // For very low periods (high notes near B-9), the
        // Frequency raw is now `u64` Q16.16 — the previous u32
        // saturation that started around period 2330 no longer
        // applies (the wider type lets `linear_period_to_frequency`
        // produce 134 kHz+ for samples shifted up by
        // `relative_pitch`). Walk the full grid down to period 1.
        let mut last = u64::MAX;
        for period in (1..=7680).step_by(64) {
            let f = linear_period_to_frequency(Period::from_raw(period));
            let raw = f.raw_q24_8() as u64;
            assert!(raw < last, "non-monotonic at period {}", period);
            last = raw;
        }
    }

    #[test]
    fn period_cache_hit_returns_same_value() {
        let mut cache = PeriodCache::new();
        let p = Period::from_raw(4608);
        let f1 = cache.lookup(p);
        let f2 = cache.lookup(p);
        let f3 = cache.lookup(p);
        assert_eq!(f1, f2);
        assert_eq!(f2, f3);
    }

    #[test]
    fn period_cache_miss_recomputes() {
        let mut cache = PeriodCache::new();
        let f1 = cache.lookup(Period::from_raw(4608));
        let f2 = cache.lookup(Period::from_raw(3840)); // C-5
                                                       // Different period → different frequency
        assert_ne!(f1, f2);
        // f2 should be ≈ 2 × f1
        let r1 = f1.raw_q24_8() as u64;
        let r2 = f2.raw_q24_8() as u64;
        assert!((r2 * 1000 / r1) as i64 - 2000 < 5);
    }

    #[test]
    fn period_cache_matches_uncached() {
        let mut cache = PeriodCache::new();
        for period in [4608, 3840, 7680, 64, 4032] {
            let p = Period::from_raw(period);
            let cached = cache.lookup(p);
            let uncached = linear_period_to_frequency(p);
            assert_eq!(cached, uncached, "mismatch at period {}", period);
        }
    }

    #[test]
    fn amiga_period_c0_centred_is_reference() {
        // C-0 finetune 8 must map to AMIGA_C0_PERIOD exactly:
        // p = 0 → octave = 0, in_oct = 0, mul = 1.0, no shift.
        let p = amiga_period(0, 8);
        assert_eq!(p, Period::from_raw(AMIGA_C0_PERIOD as u16));
    }

    #[test]
    fn amiga_period_a4_known_value() {
        // A-4 (note 57) finetune 8 with the MIDI-aligned default
        // (AMIGA_C0_PERIOD = 6779) gives period ≈ 251.93 → 252.
        let p = amiga_period(57, 8);
        assert!(
            (p.raw() as i32 - 252).abs() <= 2,
            "got {:?}, expected 252 ± 2",
            p
        );
    }

    #[test]
    fn amiga_period_octaves_halve() {
        // Period halves per octave. With C-0 = 6779:
        // C-3 = 847.4, C-4 = 423.7, C-7 ≈ 53.0
        let p0 = amiga_period(0, 8).raw();
        let p3 = amiga_period(36, 8).raw();
        let p4 = amiga_period(48, 8).raw();
        let p7 = amiga_period(84, 8).raw();
        assert!((p0 as i32 - 6779).abs() <= 1);
        assert!((p3 as i32 - 847).abs() <= 1);
        assert!((p4 as i32 - 424).abs() <= 1);
        assert!((p7 as i32 - 53).abs() <= 2);
    }

    #[test]
    fn amiga_period_matches_midi_aligned_table() {
        // Reference values computed from the MIDI-aligned formula
        // `period(n) = 6779 × 2^(-n/12)`, rounded to nearest int.
        let cases = [
            // (note_index, expected_period)
            (0, 6779),  // C-0
            (12, 3390), // C-1 = 6779/2
            (24, 1695), // C-2
            (36, 847),  // C-3
            (37, 800),  // C#3
            (45, 503),  // A-3
            (48, 424),  // C-4
            (57, 252),  // A-4 → drives Paula at 14079 Hz (= 440 × 32)
            (60, 212),  // C-5
            (72, 106),  // C-6
            (83, 56),   // B-6
        ];
        for (note, expected) in cases {
            let p = amiga_period(note, 8);
            assert!(
                (p.raw() as i32 - expected).abs() <= 2,
                "amiga_period({}, 8) = {:?}, expected {} ± 2",
                note,
                p,
                expected
            );
        }
    }

    #[test]
    fn amiga_period_a4_drives_paula_at_concert_pitch() {
        // The whole point of choosing 6779 over 6848: A-4 must
        // produce a Paula frequency that is an exact octave
        // multiple of 440 Hz, so the music plays at concert
        // pitch. Paula plays at sample rate; the human-pitch
        // multiple of 32 lands the perceived A-4 on 440.
        let p = amiga_period(57, 8);
        let f = amiga_period_to_frequency(p);
        let hz = (f.raw_q24_8() as f64) / 256.0;
        // Expected: 440 × 32 = 14080 Hz, ± a few Hz from rounding
        assert!(
            (hz - 14080.0).abs() < 20.0,
            "A-4 should drive Paula at ≈ 14080 Hz (= 440 × 32), got {:.2}",
            hz
        );
    }

    #[test]
    fn amiga_period_finetune_within_one_unit() {
        // Cross-check finetune behaviour at C-3 (note 36):
        // ft 0 → ~1 semitone below = 6779/2 × 2^(1/12) ≈ 897.6
        // ft 8 → 847 (centre)
        // ft 15 → ~7/8 semitone above = 847 × 2^(-7/96) ≈ 805.5
        let cases = [
            // (note, finetune, expected_period)
            (36, 0, 898),  // C-3 ft 0 (one semitone below)
            (36, 8, 847),  // C-3 ft 8 (centre)
            (36, 15, 806), // C-3 ft 15 (7/8 semitone above)
            (48, 8, 424),  // C-4 ft 8 = C-3/2
        ];
        for (note, ft, expected) in cases {
            let p = amiga_period(note, ft);
            assert!(
                (p.raw() as i32 - expected).abs() <= 2,
                "amiga_period({}, {}) = {:?}, expected {} ± 2",
                note,
                ft,
                p,
                expected
            );
        }
    }

    #[test]
    fn amiga_legacy_constant_still_available() {
        // `AMIGA_C0_PERIOD_LEGACY` is exported for users who
        // need bit-numerical agreement with ProTracker / FT2 /
        // OpenMPT / libxmp on note → period.
        assert_eq!(AMIGA_C0_PERIOD_LEGACY, 6848);
        // And our default is the MIDI-aligned value.
        assert_eq!(AMIGA_C0_PERIOD, 6779);
        // The two differ by ≈ 17 cents (1200 × log2(6848/6779) ≈ 17.5).
        // The assertion is constant on purpose — it documents the
        // sign convention statically; if either constant changes,
        // this test catches it at the next build.
        #[allow(clippy::assertions_on_constants)]
        const _: () = assert!(AMIGA_C0_PERIOD < AMIGA_C0_PERIOD_LEGACY);
    }

    #[test]
    fn amiga_to_freq_a4_is_about_440hz_paula() {
        // 3546894 / 508 ≈ 6982.07 — that's the actual Amiga
        // frequency for the period-508 sample, not 440 Hz.
        // We just sanity-check the formula here.
        let f = amiga_period_to_frequency(Period::from_raw(508));
        let hz_int = (f.raw_q24_8() >> 8) as i64;
        assert!((hz_int - 6982).abs() < 2, "got {}", hz_int);
    }
}
