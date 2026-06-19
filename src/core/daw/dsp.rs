//! Reusable fixed-point DSP primitives for native [`Device`]s.
//!
//! These are the shared building blocks the richer mixer inserts
//! (ParamEq, Distortion, Compressor, Echo, ModDelay, Reverb) are made
//! of. They are **integer-only** (no `f32` ever touches the audio path —
//! the crate is no_std and `float-helpers` is editor-only) and modelled
//! on the proven Q-format math of the per-voice IT filter
//! (`xmrsplayer::state_filter`), generalised so coefficients can be
//! derived from a frequency / Q / gain rather than IT cutoff registers.
//!
//! [`Device`]: crate::core::daw::device::Device
//!
//! ## Contents
//!
//! * [`pow2_q16_16`] — `2^x` for a signed Q16.16 exponent (the basis for
//!   dB→linear and bandwidth→Q conversions), built on the compile-time
//!   [`pow2_frac_q16_16`] used elsewhere in the crate.
//! * [`isqrt_u64`] — integer square root (bandwidth→Q).
//! * [`Biquad`] — a Direct-Form-I RBJ biquad (peaking / low-pass /
//!   high-pass), Q3.28 coefficients, Q15.16 stereo history, `i64` MAC.
//!   Default = identity pass-through.

use alloc::vec;
use alloc::vec::Vec;

use crate::core::fixed::fixed::Q15;
use crate::core::fixed::tables::{pow2_frac_q16_16, sine};
use crate::core::fixed::units::{Amp, Phase};

/// A Q1.15 coefficient as a **raw `i32`** — so values ≥ 1.0 (e.g.
/// Freeverb's `scalewet = 3`, or unit feedback) are representable, unlike
/// the saturating [`crate::core::fixed::fixed::Q15::from_ratio`] — built
/// from an exact rational `num/den` (`num ≥ 0`), rounded to nearest, at
/// **compile time**.
///
/// Integer-only ⇒ reproducible across build hosts (the crate deliberately
/// avoids float arithmetic even in `const`; see `core::fixed::tables`).
/// Use it so DSP coefficients read as fractions (`q15_ratio(28, 100)` =
/// 0.28) instead of opaque magic numbers like `9_175`.
pub const fn q15_ratio(num: i64, den: i64) -> i32 {
    ((num * 32_768 + den / 2) / den) as i32
}

/// `2^x` in Q16.16 for a **signed Q16.16** exponent `x`.
///
/// Decomposes `x` into an integer octave part and a `[0, 1)` fraction,
/// evaluates the fraction with the crate's [`pow2_frac_q16_16`] (a
/// 768-grid Taylor `exp`), then shifts by the octaves. Saturates rather
/// than overflowing at the extremes. Used to turn decibel gains and
/// octave bandwidths into linear coefficients at parameter-change time
/// (never per-sample).
pub fn pow2_q16_16(exp_q16_16: i32) -> i64 {
    let int_part = exp_q16_16 >> 16; // arithmetic shift = floor
    let frac = (exp_q16_16 & 0xFFFF) as u32; // [0, 65536) ⇒ [0, 1)
                                             // Map the Q16 fraction onto the 768-grid the helper expects.
    let frac_768 = (frac * 768) >> 16; // [0, 768)
    let base = pow2_frac_q16_16(frac_768) as i64; // 2^frac in Q16.16 ∈ [1, 2)
    if int_part >= 0 {
        if int_part >= 47 {
            return i64::MAX;
        }
        base << int_part
    } else {
        let sh = (-int_part) as u32;
        if sh >= 63 {
            return 0;
        }
        base >> sh
    }
}

/// A decibel amplitude (in **milli-decibels**, dB × 1000, signed)
/// expressed as **octaves** (`log2` of the linear ratio) in Q16.16.
///
/// `log2(10^(d/20)) = d/20 · log2(10)`. With `d` in mdB:
/// `= d · 3.321928 / 20000 · 65536 ≈ d · 10.8839` (Q16.16).
/// This is the exponent fed to [`pow2_q16_16`]; it is also directly
/// usable in log-domain DSP (compressor gain computers).
#[inline]
pub fn milli_db_to_oct_q16_16(milli_db: i32) -> i32 {
    ((milli_db as i64 * 1_088_394) / 100_000) as i32
}

/// `10^(milli_db / 1000 / 20)` as a linear amplitude ratio in Q16.16.
/// `0 mdB ⇒ 65536` (unity).
pub fn db_milli_to_lin_q16_16(milli_db: i32) -> i64 {
    pow2_q16_16(milli_db_to_oct_q16_16(milli_db))
}

/// `log2(1 + i/256)` in Q16.16 for `i ∈ [0, 256)` — the mantissa table
/// for [`log2_q16_16`]. Built at compile time by **inverting**
/// [`pow2_frac_q16_16`] (binary search on the 768-grid), so it shares
/// the crate's single fixed-point exp implementation with no float.
const LOG2_MANTISSA_Q16_16: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0u32;
    while i < 256 {
        let target = 65536 + i * 256; // (1 + i/256) in Q16.16, [1, 2)
                                      // smallest frac768 with 2^(frac768/768) >= target
        let mut lo = 0u32;
        let mut hi = 768u32;
        while lo < hi {
            let mid = (lo + hi) / 2;
            if pow2_frac_q16_16(mid) < target {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        t[i as usize] = (lo << 16) / 768; // octaves, Q16.16
        i += 1;
    }
    t
};

/// `log2(x)` in **signed Q16.16**, for a positive Q16.16 argument `x`.
/// Returns a large negative sentinel for `x <= 0`. Integer part from the
/// bit position, fractional part from the `LOG2_MANTISSA_Q16_16` table. No
/// transcendental at runtime — a table lookup plus shifts.
pub fn log2_q16_16(x_q16_16: i64) -> i32 {
    if x_q16_16 <= 0 {
        return i32::MIN / 2;
    }
    let x = x_q16_16 as u64;
    let hb = 63 - x.leading_zeros(); // 0-based highest set bit
    let int_part = hb as i32 - 16; // log2 of the integer magnitude
                                   // Normalise the mantissa to Q16.16 ∈ [65536, 131072).
    let m = if hb >= 16 {
        x >> (hb - 16)
    } else {
        x << (16 - hb)
    };
    let idx = ((m - 65536) >> 8) as usize; // 0..255
    (int_part << 16) + LOG2_MANTISSA_Q16_16[idx.min(255)] as i32
}

/// Integer square root of a `u64` (floor). Newton's method; exact.
pub fn isqrt_u64(n: u64) -> u64 {
    if n == 0 {
        return 0;
    }
    let mut x = 1u64 << ((64 - n.leading_zeros()).div_ceil(2));
    loop {
        let y = (x + n / x) >> 1;
        if y >= x {
            return x;
        }
        x = y;
    }
}

// --- Biquad ----------------------------------------------------------

/// Clamp `[20 Hz, 0.49·rate]` and return `(sin ω0, cos ω0)` in Q1.15.
///
/// `ω0 = 2π·fc/rate`; the sine LUT is indexed by a u16 phase
/// `= fc/rate · 65536`, which fits because `fc < rate/2 ⇒ phase < 32768`.
fn omega_sin_cos(fc_hz: u32, rate_hz: u32) -> (i32, i32) {
    let rate = rate_hz.max(1);
    let fc_max = ((rate as u64 * 32113) >> 16) as u32; // 0.49·rate
    let fc = fc_hz.clamp(20, fc_max.max(20));
    let phase_u32 = ((fc as u64) << 16) / rate as u64;
    let phase = Phase::from_raw(phase_u32 as u16);
    let sin_q15 = sine(phase).raw() as i32;
    let cos_q15 = sine(phase.shifted(Phase::QUARTER)).raw() as i32;
    (sin_q15, cos_q15)
}

/// `α = sin ω0 / (2Q)` in Q1.15, given `2Q` in Q16.16.
fn alpha_q15(sin_q15: i32, two_q_q16_16: i64) -> i32 {
    if two_q_q16_16 > 0 {
        ((sin_q15 as i64 * 65536) / two_q_q16_16) as i32
    } else {
        0
    }
}

/// A Direct-Form-I RBJ biquad with stereo history.
///
/// Coefficients are Q3.28 (normalised so `a0 = 1`); the history slots
/// are Q15.16; the per-sample multiply-accumulate is `Q3.28 × Q15.16 →
/// Q18.44` in `i64`, narrowed back to Q15.16. This mirrors the audited
/// `state_filter` math exactly — the difference is only in how the
/// coefficients are *derived* (frequency/Q/gain here vs IT registers
/// there). [`Default`] is the identity pass-through (`b0 = 1`).
#[derive(Clone, Debug, PartialEq)]
pub struct Biquad {
    // Coefficients, Q3.28, normalised a0 = 1.
    b0: i32,
    b1: i32,
    b2: i32,
    a1: i32,
    a2: i32,
    // History, Q15.16, independent per channel.
    xl1: i32,
    xl2: i32,
    yl1: i32,
    yl2: i32,
    xr1: i32,
    xr2: i32,
    yr1: i32,
    yr2: i32,
}

impl Default for Biquad {
    fn default() -> Self {
        Self {
            b0: 1 << 28, // 1.0 in Q3.28 — identity pass-through
            b1: 0,
            b2: 0,
            a1: 0,
            a2: 0,
            xl1: 0,
            xl2: 0,
            yl1: 0,
            yl2: 0,
            xr1: 0,
            xr2: 0,
            yr1: 0,
            yr2: 0,
        }
    }
}

impl Biquad {
    /// Clear the stereo history (parameters/coefficients kept).
    pub fn reset_history(&mut self) {
        self.xl1 = 0;
        self.xl2 = 0;
        self.yl1 = 0;
        self.yl2 = 0;
        self.xr1 = 0;
        self.xr2 = 0;
        self.yr1 = 0;
        self.yr2 = 0;
    }

    /// Store coefficients from un-normalised Q3.28 ingredients,
    /// dividing through by `a0` so `a0 = 1`. `b1`/`a1` are passed once
    /// (`-2cosω0` is shared by every second-order RBJ section).
    fn set_coeffs_q3_28(&mut self, b0: i64, b1: i64, b2: i64, a0: i64, a1: i64, a2: i64) {
        let normalise = |c_pre: i64| -> i32 {
            if a0 == 0 {
                return 0;
            }
            let q = (c_pre << 28) / a0;
            if q > i32::MAX as i64 {
                i32::MAX
            } else if q < i32::MIN as i64 {
                i32::MIN
            } else {
                q as i32
            }
        };
        self.b0 = normalise(b0);
        self.b1 = normalise(b1);
        self.b2 = normalise(b2);
        self.a1 = normalise(a1);
        self.a2 = normalise(a2);
    }

    /// Configure as an RBJ **peaking EQ**: a boost/cut of `gain_db`
    /// (milli-dB, signed) at `center_hz`, with a `bandwidth_octaves`
    /// (Q16.16) skirt. Recomputed at parameter-change time only.
    pub fn set_peaking(
        &mut self,
        rate_hz: u32,
        center_hz: u32,
        bandwidth_octaves_q16_16: i64,
        gain_milli_db: i32,
    ) {
        let (sin_q15, cos_q15) = omega_sin_cos(center_hz, rate_hz);

        // Q from bandwidth N (octaves): Q = √(2^N) / (2^N − 1).
        let two_n = pow2_q16_16((bandwidth_octaves_q16_16.max(1)) as i32); // Q16.16
        let denom = (two_n - 65536).max(1); // (2^N − 1), Q16.16
        let sqrt_two_n = isqrt_u64((two_n as u64) << 16) as i64; // √(2^N) Q16.16
        let q_q16_16 = (sqrt_two_n << 16) / denom; // Q16.16
        let two_q_q16_16 = 2 * q_q16_16;

        let alpha = alpha_q15(sin_q15, two_q_q16_16); // Q1.15
        let alpha_q3_28 = (alpha as i64) << 13;
        let cos_q3_28 = (cos_q15 as i64) << 13;
        let one = 1i64 << 28;

        // A = 10^(gain_db/40) (peaking uses /40). amp = 10^(d/20) gives
        // A² ; A = √amp. amp in Q16.16, A_q16_16 = √(amp·65536).
        let amp_q16_16 = db_milli_to_lin_q16_16(gain_milli_db);
        let a_q16_16 = isqrt_u64((amp_q16_16 as u64) << 16) as i64; // Q16.16

        // alpha·A and alpha/A in Q3.28.
        let alpha_a = (alpha_q3_28 * a_q16_16) >> 16;
        let alpha_div_a = if a_q16_16 > 0 {
            (alpha_q3_28 << 16) / a_q16_16
        } else {
            0
        };
        let m2cos = -(cos_q3_28 << 1); // -2cosω0

        self.set_coeffs_q3_28(
            one + alpha_a,     // b0
            m2cos,             // b1
            one - alpha_a,     // b2
            one + alpha_div_a, // a0
            m2cos,             // a1
            one - alpha_div_a, // a2
        );
    }

    /// Configure as an RBJ **low-pass** at `fc_hz` with quality
    /// `two_q_q16_16` = `2Q` (Q16.16). `2Q = √2` (≈ 92682) is
    /// Butterworth.
    pub fn set_lowpass(&mut self, rate_hz: u32, fc_hz: u32, two_q_q16_16: i64) {
        let (sin_q15, cos_q15) = omega_sin_cos(fc_hz, rate_hz);
        let alpha = alpha_q15(sin_q15, two_q_q16_16);
        let alpha_q3_28 = (alpha as i64) << 13;
        let cos_q3_28 = (cos_q15 as i64) << 13;
        let one = 1i64 << 28;
        let one_minus_cos = one - cos_q3_28;
        let half = one_minus_cos / 2;
        self.set_coeffs_q3_28(
            half,
            one_minus_cos,
            half,
            one + alpha_q3_28,
            -(cos_q3_28 << 1),
            one - alpha_q3_28,
        );
    }

    /// Configure as an RBJ **high-pass** at `fc_hz` with `2Q` (Q16.16).
    pub fn set_highpass(&mut self, rate_hz: u32, fc_hz: u32, two_q_q16_16: i64) {
        let (sin_q15, cos_q15) = omega_sin_cos(fc_hz, rate_hz);
        let alpha = alpha_q15(sin_q15, two_q_q16_16);
        let alpha_q3_28 = (alpha as i64) << 13;
        let cos_q3_28 = (cos_q15 as i64) << 13;
        let one = 1i64 << 28;
        let one_plus_cos = one + cos_q3_28;
        let half = one_plus_cos / 2;
        self.set_coeffs_q3_28(
            half,
            -one_plus_cos,
            half,
            one + alpha_q3_28,
            -(cos_q3_28 << 1),
            one - alpha_q3_28,
        );
    }

    /// Process one stereo Q1.15 sample pair through the section,
    /// advancing the history. Pure integer arithmetic.
    #[inline]
    pub fn process(&mut self, l: Amp, r: Amp) -> (Amp, Amp) {
        let l_q15_16 = l.widen_q15_16();
        let r_q15_16 = r.widen_q15_16();

        let mac = |c: i32, s: i32| -> i64 { (c as i64).wrapping_mul(s as i64) };

        let yl_q18_44 = mac(self.b0, l_q15_16) + mac(self.b1, self.xl1) + mac(self.b2, self.xl2)
            - mac(self.a1, self.yl1)
            - mac(self.a2, self.yl2);
        let yl = ((yl_q18_44 + (1 << 27)) >> 28) as i32;
        self.xl2 = self.xl1;
        self.xl1 = l_q15_16;
        self.yl2 = self.yl1;
        self.yl1 = yl;

        let yr_q18_44 = mac(self.b0, r_q15_16) + mac(self.b1, self.xr1) + mac(self.b2, self.xr2)
            - mac(self.a1, self.yr1)
            - mac(self.a2, self.yr2);
        let yr = ((yr_q18_44 + (1 << 27)) >> 28) as i32;
        self.xr2 = self.xr1;
        self.xr1 = r_q15_16;
        self.yr2 = self.yr1;
        self.yr1 = yr;

        // Q15.16 → Q1.15 (>> 1, round-half, saturate).
        (
            Amp::from_q15_i32_sat((yl + 1) >> 1),
            Amp::from_q15_i32_sat((yr + 1) >> 1),
        )
    }
}

/// Cubic soft-clip waveshaper, fixed-point.
///
/// Applies a pre-`drive` (Q8.8, `256` = ×1), clamps into `[-1, 1]`, then
/// shapes with `f(t) = 1.5·t − 0.5·t³` — the classic memoryless cubic
/// whose derivative `1.5(1 − t²)` is non-negative on `[-1, 1]` (so the
/// transfer curve is monotonic) and saturates softly at the rails. Input
/// and output are Q1.15 (`i32` for headroom).
pub fn soft_clip_q15(x_q15: i32, drive_q8_8: i32) -> i32 {
    let one = Q15::ONE.raw() as i32; // ±1.0 rails
    let t = ((x_q15 * drive_q8_8) >> 8).clamp(-one, one);
    let t2 = (t * t) >> 15;
    let t3 = (t2 * t) >> 15;
    ((3 * t) / 2 - t3 / 2).clamp(-one, one)
}

/// One-pole smoothing coefficient `a` in **Q30** for a time constant of
/// `time_us` microseconds at `rate_hz`. `0 µs` (or unknown rate) ⇒
/// instantaneous (`1<<30`).
///
/// Uses the standard `a ≈ Ts/τ = 1e6/(rate·µs)` approximation, clamped
/// to `1.0`. Q30 (vs Q16) keeps multi-second time constants resolvable —
/// a 3 s τ at 48 kHz is `a ≈ 7e-6`, far below one Q16 LSB but ~7400 in
/// Q30. For very short τ (a few samples) the approximation saturates to
/// `1.0` (instantaneous), which is the correct limit.
pub fn one_pole_coeff_q30(rate_hz: u32, time_us: u32) -> i64 {
    const ONE: i64 = 1 << 30;
    if time_us == 0 || rate_hz == 0 {
        return ONE;
    }
    let denom = (rate_hz as i64) * (time_us as i64);
    ((1_000_000i64 << 30) / denom).min(ONE)
}

/// A peak **envelope follower** with independent attack / release
/// one-pole time constants. Tracks a rectified (≥ 0) Q1.15 target. The
/// running envelope is held in Q15.30 (`i64`) so very long release times
/// integrate cleanly. Reusable by dynamics processors.
#[derive(Clone, Debug, PartialEq)]
pub struct EnvelopeFollower {
    attack_q30: i64,
    release_q30: i64,
    /// Envelope magnitude, Q15 value with 30 fractional bits.
    env_q15_30: i64,
}

impl Default for EnvelopeFollower {
    fn default() -> Self {
        // Instantaneous until configured (safe pass-through detector).
        Self {
            attack_q30: 1 << 30,
            release_q30: 1 << 30,
            env_q15_30: 0,
        }
    }
}

impl EnvelopeFollower {
    /// Set attack / release time constants (microseconds) at `rate_hz`.
    pub fn set_times(&mut self, rate_hz: u32, attack_us: u32, release_us: u32) {
        self.attack_q30 = one_pole_coeff_q30(rate_hz, attack_us);
        self.release_q30 = one_pole_coeff_q30(rate_hz, release_us);
    }

    /// Clear the running envelope to silence.
    pub fn reset(&mut self) {
        self.env_q15_30 = 0;
    }

    /// Feed one rectified sample `target_q15` (≥ 0) and return the
    /// updated envelope (Q1.15). Rises with the attack coefficient,
    /// falls with the release coefficient.
    #[inline]
    pub fn process(&mut self, target_q15: i32) -> i32 {
        let target = (target_q15 as i64) << 30;
        let coeff = if target > self.env_q15_30 {
            self.attack_q30
        } else {
            self.release_q30
        };
        let delta = target - self.env_q15_30;
        self.env_q15_30 += ((delta as i128 * coeff as i128) >> 30) as i64;
        (self.env_q15_30 >> 30) as i32
    }
}

/// A mono ring-buffer **delay line** holding `i32` samples (callers use
/// Q15.16: a Q1.15 sample shifted left 16, giving 16 fractional bits on
/// the recirculating/feedback path so long echo tails don't staircase).
///
/// The backing buffer is allocated by [`DelayLine::prepare`] (sized to
/// the maximum delay at the output rate); processing an un-prepared
/// (empty) line is a safe no-op (`tap` returns 0, `write` drops). Reads
/// are by delay in samples — integer ([`DelayLine::tap`]) or
/// linearly-interpolated fractional ([`DelayLine::tap_frac`], for
/// modulated delays).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DelayLine {
    buf: Vec<i32>,
    pos: usize,
}

impl DelayLine {
    /// New, un-prepared (empty) line.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate the buffer for up to `max_len` samples of delay and
    /// clear it. Re-call on a rate change.
    pub fn prepare(&mut self, max_len: usize) {
        self.buf = vec![0; max_len.max(1)];
        self.pos = 0;
    }

    /// Clear the tail (buffer + write position) without freeing.
    pub fn reset(&mut self) {
        for s in &mut self.buf {
            *s = 0;
        }
        self.pos = 0;
    }

    /// Capacity in samples.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// `true` if un-prepared.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// The sample written `delay` samples ago (`delay` clamped to
    /// `[1, len]`). `0` if un-prepared.
    #[inline]
    pub fn tap(&self, delay: usize) -> i32 {
        let n = self.buf.len();
        if n == 0 {
            return 0;
        }
        let d = delay.clamp(1, n);
        self.buf[(self.pos + n - d) % n]
    }

    /// Fractional tap: `delay_q16` is the delay in samples, Q16.16,
    /// linearly interpolated between the two neighbouring integer taps
    /// (more delay = older). `0` if un-prepared.
    #[inline]
    pub fn tap_frac(&self, delay_q16: u32) -> i32 {
        let n = self.buf.len();
        if n == 0 {
            return 0;
        }
        let di = (delay_q16 >> 16) as usize;
        let frac = (delay_q16 & 0xFFFF) as i64;
        let d0 = di.clamp(1, n);
        let d1 = (di + 1).clamp(1, n);
        let s0 = self.buf[(self.pos + n - d0) % n] as i64;
        let s1 = self.buf[(self.pos + n - d1) % n] as i64;
        (s0 + (((s1 - s0) * frac) >> 16)) as i32
    }

    /// Push one sample (advancing the write head). No-op if un-prepared.
    #[inline]
    pub fn write(&mut self, v: i32) {
        let n = self.buf.len();
        if n == 0 {
            return;
        }
        self.buf[self.pos] = v;
        self.pos = if self.pos + 1 == n { 0 } else { self.pos + 1 };
    }
}

/// `scaledamp` from Freeverb (damping → feedback-LPF coefficient), Q15.
const FREEVERB_SCALEDAMP_Q15: i64 = 13_107; // 0.4 · 32768

/// A Freeverb-style **lowpass-damped comb filter** (a fixed delay whose
/// feedback path runs through a one-pole lowpass). Operates in Q15.16
/// (`i32`) so long, high-feedback tails decay smoothly. Reset/prepared
/// by the owning reverb device.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Comb {
    buf: Vec<i32>,
    pos: usize,
    filterstore: i32, // Q15.16
    feedback_q15: i32,
    damp1_q15: i32,
    damp2_q15: i32,
}

impl Comb {
    pub fn new() -> Self {
        Self {
            damp2_q15: Q15::ONE.raw() as i32,
            ..Self::default()
        }
    }

    /// Allocate the `len`-sample comb buffer and clear state.
    pub fn prepare(&mut self, len: usize) {
        self.buf = vec![0; len.max(1)];
        self.pos = 0;
        self.filterstore = 0;
    }

    /// Clear the tail without freeing.
    pub fn reset(&mut self) {
        for s in &mut self.buf {
            *s = 0;
        }
        self.pos = 0;
        self.filterstore = 0;
    }

    /// Feedback gain (Q1.15) — sets the decay time.
    pub fn set_feedback(&mut self, feedback_q15: i32) {
        self.feedback_q15 = feedback_q15;
    }

    /// Damping (Q1.15) — how much high end is lost on each pass.
    pub fn set_damp(&mut self, damp_q15: i32) {
        self.damp1_q15 = ((damp_q15 as i64 * FREEVERB_SCALEDAMP_Q15) >> 15) as i32;
        self.damp2_q15 = Q15::ONE.raw() as i32 - self.damp1_q15;
    }

    /// Process one Q15.16 input sample, return the Q15.16 comb output.
    #[inline]
    pub fn process(&mut self, input: i32) -> i32 {
        let n = self.buf.len();
        if n == 0 {
            return 0;
        }
        let out = self.buf[self.pos];
        // One-pole lowpass in the feedback path.
        self.filterstore = ((out as i64 * self.damp2_q15 as i64
            + self.filterstore as i64 * self.damp1_q15 as i64)
            >> 15) as i32;
        let stored = (input as i64) + ((self.filterstore as i64 * self.feedback_q15 as i64) >> 15);
        self.buf[self.pos] = stored.clamp(-(1 << 30), 1 << 30) as i32;
        self.pos = if self.pos + 1 == n { 0 } else { self.pos + 1 };
        out
    }
}

/// A Schroeder **all-pass filter** (fixed feedback 0.5), Q15.16.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Allpass {
    buf: Vec<i32>,
    pos: usize,
    feedback_q15: i32,
}

impl Allpass {
    pub fn new() -> Self {
        Self {
            feedback_q15: 16_384, // 0.5
            ..Self::default()
        }
    }

    pub fn prepare(&mut self, len: usize) {
        self.buf = vec![0; len.max(1)];
        self.pos = 0;
    }

    pub fn reset(&mut self) {
        for s in &mut self.buf {
            *s = 0;
        }
        self.pos = 0;
    }

    /// Process one Q15.16 sample.
    #[inline]
    pub fn process(&mut self, input: i32) -> i32 {
        let n = self.buf.len();
        if n == 0 {
            return 0;
        }
        let bufout = self.buf[self.pos];
        let out = bufout - input;
        let stored = (input as i64) + ((bufout as i64 * self.feedback_q15 as i64) >> 15);
        self.buf[self.pos] = stored.clamp(-(1 << 30), 1 << 30) as i32;
        self.pos = if self.pos + 1 == n { 0 } else { self.pos + 1 };
        out
    }
}

/// Evaluate a bipolar LFO in Q1.15 `[-1, 1]` from a 32-bit phase
/// accumulator (full cycle = `2^32`). `sine_wave` selects sine (via the
/// shared LUT) vs triangle. Used by modulated delays (chorus/flanger).
#[inline]
pub fn lfo_value_q15(phase: u32, sine_wave: bool) -> i32 {
    if sine_wave {
        sine(Phase::from_raw((phase >> 16) as u16)).raw() as i32
    } else {
        // Triangle: rises −1→+1 over the first half, falls back over the
        // second. Top 16 bits index one cycle.
        let p = (phase >> 16) as i32; // 0..65535
        if p < 32_768 {
            -32_768 + p * 2
        } else {
            32_767 - (p - 32_768) * 2
        }
    }
}

/// Per-sample phase increment (of a `2^32` cycle) for an LFO of
/// `freq_milli_hz` millihertz at `rate_hz`.
#[inline]
pub fn lfo_increment(freq_milli_hz: u32, rate_hz: u32) -> u32 {
    if rate_hz == 0 {
        return 0;
    }
    // inc = freq_hz / rate · 2^32 = freq_milli_hz · 2^32 / (1000·rate).
    ((freq_milli_hz as u64 * (1u64 << 32)) / (1000u64 * rate_hz as u64)) as u32
}

/// `2Q = √2` (Butterworth) in Q16.16 — the default biquad quality.
pub const TWO_Q_BUTTERWORTH_Q16_16: i64 = 92_682;

/// One octave in Q16.16 (bandwidth helper).
pub const ONE_OCTAVE_Q16_16: i64 = 65_536;

#[cfg(test)]
mod tests {
    use super::*;

    fn amp(raw: i16) -> Amp {
        Amp::from_q15_i16(raw)
    }

    /// Drive a pure sine tone of `freq` Hz / `ampl` Q1.15 peak through a
    /// section and return the steady-state output peak (measured over
    /// the second half, after the transient settles).
    fn tone_peak(b: &mut Biquad, freq: u32, rate: u32, ampl: i32) -> i32 {
        b.reset_history();
        let inc = (((freq as u64) << 16) / rate as u64) as u16;
        let mut ph = 0u16;
        let mut peak = 0i32;
        let total = 800usize;
        for n in 0..total {
            let s = (sine(Phase::from_raw(ph)).raw() as i32 * ampl) >> 15;
            let (l, _) = b.process(amp(s as i16), amp(s as i16));
            if n >= total / 2 {
                peak = peak.max(l.as_q15_i16().unsigned_abs() as i32);
            }
            ph = ph.wrapping_add(inc);
        }
        peak
    }

    #[test]
    fn q15_ratio_reads_as_fractions() {
        assert_eq!(q15_ratio(1, 2), 16_384); // 0.5
        assert_eq!(q15_ratio(1, 1), 32_768); // 1.0 (raw, unsaturated)
        assert_eq!(q15_ratio(3, 1), 98_304); // 3.0
        assert_eq!(q15_ratio(28, 100), 9_175); // 0.28 (Freeverb scaleroom)
        assert_eq!(q15_ratio(7, 10), 22_938); // 0.70 (Freeverb offsetroom)
        assert_eq!(q15_ratio(2, 5), 13_107); // 0.40 (Freeverb scaledamp)
        assert_eq!(q15_ratio(0, 1), 0);
    }

    #[test]
    fn pow2_basics() {
        // 2^0 = 1.0
        assert_eq!(pow2_q16_16(0), 65536);
        // 2^1 = 2.0
        assert!((pow2_q16_16(1 << 16) - 131072).abs() <= 4);
        // 2^-1 = 0.5
        assert!((pow2_q16_16(-(1 << 16)) - 32768).abs() <= 4);
        // 2^3 = 8.0
        assert!((pow2_q16_16(3 << 16) - (8 * 65536)).abs() <= 32);
    }

    #[test]
    fn db_to_lin() {
        // 0 dB ⇒ 1.0
        assert!((db_milli_to_lin_q16_16(0) - 65536).abs() <= 4);
        // +6.0206 dB ⇒ ×2.0 (6020 mdB)
        let two = db_milli_to_lin_q16_16(6_020);
        assert!((two - 131072).abs() < 400, "got {two}");
        // -6.0206 dB ⇒ ×0.5
        let half = db_milli_to_lin_q16_16(-6_020);
        assert!((half - 32768).abs() < 200, "got {half}");
        // -60 dB ⇒ ×0.001
        let m60 = db_milli_to_lin_q16_16(-60_000);
        assert!((m60 - 66).abs() < 8, "got {m60} (≈0.001·65536=66)");
    }

    #[test]
    fn log2_known_values() {
        let one = 65536i64;
        // log2(1) = 0
        assert!(log2_q16_16(one).abs() <= 8);
        // log2(2) = 1
        assert!((log2_q16_16(2 * one) - 65536).abs() <= 8);
        // log2(0.5) = -1
        assert!((log2_q16_16(one / 2) + 65536).abs() <= 8);
        // log2(8) = 3
        assert!((log2_q16_16(8 * one) - 3 * 65536).abs() <= 16);
        // round-trip: pow2(log2(x)) ≈ x for a few x
        for &x in &[one / 4, one / 3, one, 3 * one, 10 * one] {
            let l = log2_q16_16(x);
            let back = pow2_q16_16(l);
            let err = (back - x).abs();
            assert!(err * 200 < x, "pow2(log2({x})) = {back}, err {err}");
        }
    }

    #[test]
    fn envelope_follower_tracks() {
        let mut env = EnvelopeFollower::default();
        env.set_times(48_000, 1_000, 100_000); // 1 ms attack, 100 ms release
                                               // Rises toward a held level.
        let mut last = 0;
        for _ in 0..480 {
            last = env.process(20_000);
        }
        assert!(last > 19_000, "did not reach held level: {last}");
        // Falls slower than it rose (release ≫ attack): after the same
        // number of samples at 0 it should still be well above silence.
        let mut after = last;
        for _ in 0..480 {
            after = env.process(0);
        }
        assert!(after > 2_000, "release too fast: {after}");
        assert!(after < last, "envelope did not fall");
    }

    #[test]
    fn one_pole_coeff_bounds() {
        const ONE: i64 = 1 << 30;
        // Zero time ⇒ instantaneous; very short time ⇒ saturates to 1.0.
        assert_eq!(one_pole_coeff_q30(48_000, 0), ONE);
        assert_eq!(one_pole_coeff_q30(48_000, 10), ONE); // 10 µs < 1 sample
                                                         // 3 s ⇒ tiny but non-zero (the whole point of Q30).
        let slow = one_pole_coeff_q30(48_000, 3_000_000);
        assert!(
            slow > 0 && slow < ONE / 10_000,
            "slow coeff out of range: {slow}"
        );
    }

    #[test]
    fn delay_line_taps_and_recirculates() {
        let mut dl = DelayLine::new();
        assert!(dl.is_empty());
        assert_eq!(dl.tap(1), 0); // un-prepared ⇒ no-op
        dl.write(123); // dropped
        assert_eq!(dl.tap(1), 0);

        dl.prepare(8);
        for v in 1..=8 {
            dl.write(v * 100);
        }
        // Most recent write was 800 (delay 1), oldest in window 100.
        assert_eq!(dl.tap(1), 800);
        assert_eq!(dl.tap(2), 700);
        assert_eq!(dl.tap(8), 100);
        // Fractional tap halfway between delay 1 (800) and 2 (700) = 750.
        assert_eq!(dl.tap_frac((1 << 16) + (1 << 15)), 750);

        dl.reset();
        assert_eq!(dl.tap(1), 0);
    }

    #[test]
    fn comb_decays_and_is_stable() {
        let mut c = Comb::new();
        c.prepare(64);
        c.set_feedback(28_000); // ~0.85
        c.set_damp(6_000);
        // Impulse in Q15.16, then silence; the comb should re-emit
        // decaying pulses every 64 samples and never blow up.
        let mut peak_early = 0i64;
        let mut peak_late = 0i64;
        for n in 0..4_000 {
            let x = if n == 0 { 20_000i32 << 16 } else { 0 };
            let y = c.process(x).unsigned_abs() as i64;
            if (60..70).contains(&n) {
                peak_early = peak_early.max(y);
            }
            if (3_000..3_200).contains(&n) {
                peak_late = peak_late.max(y);
            }
        }
        assert!(peak_early > 0, "comb produced no echo");
        assert!(peak_late < peak_early, "comb did not decay");
    }

    #[test]
    fn lfo_shapes_and_increment() {
        // Sine starts at 0, quarter-cycle ≈ +1, half ≈ 0.
        assert!(lfo_value_q15(0, true).abs() < 400);
        assert!(lfo_value_q15(1 << 30, true) > 30_000);
        assert!(lfo_value_q15(1 << 31, true).abs() < 400);
        // Triangle: −1 at 0, +1 at half cycle.
        assert!(lfo_value_q15(0, false) < -32_000);
        assert!(lfo_value_q15(1 << 31, false) > 32_000);
        // 1 Hz at 48 kHz advances 2^32/48000 per sample.
        let inc = lfo_increment(1_000, 48_000);
        assert!((inc as i64 - (1i64 << 32) / 48_000).abs() < 2);
    }

    #[test]
    fn isqrt_exact() {
        assert_eq!(isqrt_u64(0), 0);
        assert_eq!(isqrt_u64(1), 1);
        assert_eq!(isqrt_u64(15), 3);
        assert_eq!(isqrt_u64(16), 4);
        assert_eq!(isqrt_u64(1_000_000), 1000);
        assert_eq!(isqrt_u64(u64::MAX), 4_294_967_295);
    }

    #[test]
    fn soft_clip_is_monotonic_and_bounded() {
        let drive = 4 * 256; // ×4
        let mut prev = i32::MIN;
        for x in (-32_767..=32_767).step_by(101) {
            let y = soft_clip_q15(x, drive);
            assert!(y >= prev, "non-monotonic at x={x}: {y} < {prev}");
            assert!(
                (-32_767..=32_767).contains(&y),
                "out of range at x={x}: {y}"
            );
            prev = y;
        }
        // Hard drive saturates a large input toward the rail.
        assert!(soft_clip_q15(20_000, 8 * 256) > 28_000);
        // Zero in ⇒ zero out, any drive.
        assert_eq!(soft_clip_q15(0, 16 * 256), 0);
    }

    #[test]
    fn identity_biquad_is_passthrough() {
        let mut b = Biquad::default();
        for raw in [0i16, 12_000, -9_000, 30_000, -30_000] {
            let (l, r) = b.process(amp(raw), amp(raw / 2));
            // round-half on the >>1 path ⇒ within 1 LSB.
            assert!((l.as_q15_i16() as i32 - raw as i32).abs() <= 1, "{raw}");
            assert!((r.as_q15_i16() as i32 - (raw / 2) as i32).abs() <= 1);
        }
    }

    /// A peaking-EQ DC gain (response to a constant input) should track
    /// the configured boost/cut at the centre only loosely, but a flat
    /// 0 dB peaking filter must be (near) transparent.
    #[test]
    fn peaking_zero_gain_is_transparent() {
        let mut b = Biquad::default();
        b.set_peaking(48_000, 1_000, ONE_OCTAVE_Q16_16, 0);
        // Feed a mid-band-ish alternating signal; 0 dB ⇒ output ≈ input.
        let mut max_dev = 0i32;
        let mut x = 10_000i16;
        for _ in 0..200 {
            let (l, _) = b.process(amp(x), amp(x));
            max_dev = max_dev.max((l.as_q15_i16() as i32 - x as i32).abs());
            x = -x;
        }
        assert!(max_dev < 200, "0 dB peaking not transparent: dev {max_dev}");
    }

    /// A peaking boost raises the steady amplitude of a tone *at the
    /// centre frequency*; a cut lowers it; 0 dB is transparent.
    #[test]
    fn peaking_boost_and_cut() {
        let ampl = 4_000; // headroom so +12 dB (×3.98) stays unclipped
        let mut boost = Biquad::default();
        boost.set_peaking(48_000, 3_000, ONE_OCTAVE_Q16_16, 12_000); // +12 dB
        let mut cut = Biquad::default();
        cut.set_peaking(48_000, 3_000, ONE_OCTAVE_Q16_16, -12_000); // −12 dB
        let mut flat = Biquad::default();
        flat.set_peaking(48_000, 3_000, ONE_OCTAVE_Q16_16, 0);

        let pb = tone_peak(&mut boost, 3_000, 48_000, ampl);
        let pf = tone_peak(&mut flat, 3_000, 48_000, ampl);
        let pc = tone_peak(&mut cut, 3_000, 48_000, ampl);
        assert!(pb > pf + 1_000, "boost {pb} not clearly > flat {pf}");
        assert!(pc < pf - 1_000, "cut {pc} not clearly < flat {pf}");
        // The boost should be roughly the input ×~4 (12 dB); allow slack.
        assert!(pb > 2 * ampl, "boost {pb} weaker than expected");
    }

    #[test]
    fn lowpass_attenuates_highs() {
        let mut lp = Biquad::default();
        lp.set_lowpass(48_000, 1_000, TWO_Q_BUTTERWORTH_Q16_16);
        let pass = tone_peak(&mut lp, 300, 48_000, 16_000); // below cutoff
        let stop = tone_peak(&mut lp, 12_000, 48_000, 16_000); // well above
        assert!(pass > 12_000, "passband too low: {pass}");
        assert!(stop < 1_500, "highs not attenuated: {stop}");
    }

    #[test]
    fn highpass_attenuates_lows() {
        let mut hp = Biquad::default();
        hp.set_highpass(48_000, 8_000, TWO_Q_BUTTERWORTH_Q16_16);
        let pass = tone_peak(&mut hp, 16_000, 48_000, 16_000); // above cutoff
        let stop = tone_peak(&mut hp, 500, 48_000, 16_000); // well below
        assert!(pass > 12_000, "passband too low: {pass}");
        assert!(stop < 2_000, "lows not attenuated: {stop}");
    }
}
