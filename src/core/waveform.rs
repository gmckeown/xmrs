//! [`Waveform`] — LFO waveform shapes used by vibrato / tremolo /
//! panbrello (cell-level and instrument-level alike).
//!
//! Two shape groups plus `Random`, all bipolar (`[-1, +1]`):
//! * `Bipolar*` — cell-level effect LFOs (`BipolarSine`,
//!   `BipolarSquare`, `BipolarRampDown`, `BipolarRampUp`,
//!   `BipolarTriangle`).
//! * `AutoVib*` — instrument auto-vibrato (`AutoVibSine`,
//!   `AutoVibSquare`, `AutoVibRampDown`; XM also reuses
//!   `BipolarRampDown`/`BipolarRampUp`). Distinct from the cell
//!   shapes — see the [`Waveform`] enum docs.
//! * `Random` — a fresh signed draw per tick (see
//!   [`WaveformState::value_q15`]).
//!
//! All shapes are evaluated at a phase angle in Q1.15.
//! [`WaveformState`] holds the running phase and the RNG seed for
//! `Random`, advancing one step per tick.

use super::xorshift::XorShift32;
use crate::core::fixed::fixed::Q15;
use crate::core::fixed::tables::sine;
use crate::core::fixed::units::Phase;
use serde::{Deserialize, Serialize};

/// LFO waveform shape — the phase → amplitude mapping every
/// vibrato / tremolo / panbrello LFO consults.
///
/// Two shape groups plus `Random`, all bipolar (`[-1, +1]`):
/// * `Bipolar*`: cell-level effect LFOs (vibrato / tremolo /
///   panbrello driven by pattern rows).
/// * `AutoVib*`: instrument auto-vibrato shapes. The "same" shape
///   differs from its `Bipolar*` cell counterpart (e.g. `AutoVibSine`
///   is `+sin`, `BipolarSine` is `−sin`): IT (`IT_MUSIC.ASM`) and FT2
///   apply the auto-vibrato in a domain (frequency / period) whose
///   sign and phase conventions differ from the cell LFO, so the
///   faithful pitch-domain shape is not the same. See each variant.
///
/// `Random` is shared by cell effects and auto-vibrato alike (see
/// [`WaveformState::value_q15`]).
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, Eq, Hash, PartialEq)]
pub enum Waveform {
    // --- Cell-effect LFO shapes ---
    #[default]
    BipolarSine,
    /// Square: `−1` over the first half, `+1` over the second
    /// (falls first).
    BipolarSquare,
    /// Ramp-down: `0 → −1` over the first half, jump to `+1`,
    /// `+1 → 0` over the second (discontinuity at mid-cycle).
    BipolarRampDown,
    /// Bipolar ramp-up: `0 → +1` over the first half, jump to `−1`,
    /// `−1 → 0` over the second half (discontinuity at mid-cycle).
    /// Mirror of [`Self::BipolarRampDown`]; XM auto-vibrato type 3.
    BipolarRampUp,
    /// Bipolar symmetric triangle: `0 → +1 → 0 → −1 → 0` over one
    /// cycle, linear segments, no discontinuity. The exact shape of a
    /// David Whittaker (`.dw`) vibrato (and a standard tracker vibrato
    /// option).
    BipolarTriangle,

    // --- Instrument auto-vibrato shapes ---
    /// Auto-vibrato sine — `+sin`, rising first (`0 → +1 → 0 → −1`).
    /// Both IT (`+sin` table in the frequency domain) and FT2 (`−sin`
    /// table in the period domain) resolve to this same rising-first
    /// pitch shape — unlike [`Self::BipolarSine`] (`−sin`, falls
    /// first), which the cell LFO uses.
    AutoVibSine,
    /// Auto-vibrato square — `+1` over the first half, `−1` over the
    /// second (rises first). Distinct from [`Self::BipolarSquare`]
    /// (which falls first).
    AutoVibSquare,
    /// Auto-vibrato ramp-down — linear `+1 → −1` across the cycle
    /// with the discontinuity at the cycle boundary (peak at phase
    /// 0). IT auto-vibrato type 1 (`ramp_down_table`). Differs in
    /// phase from the mid-cycle-discontinuity [`Self::BipolarRampDown`]
    /// used by XM auto-vibrato type 2.
    AutoVibRampDown,

    /// Random — a fresh signed draw per tick; see
    /// [`WaveformState::value_q15`].
    Random,
}

impl Waveform {
    /// Evaluate the waveform at a given cycle [`Phase`]. Returns
    /// a [`Q15`].
    ///
    /// All deterministic shapes span the full `[-1, +1]` range.
    ///
    /// [`Self::Random`] is the exception. `Waveform` is a pure,
    /// stateless shape descriptor, so it has no RNG to draw from —
    /// this method returns the neutral [`Q15::ZERO`] for it. The real
    /// random value requires a per-instance RNG and is produced by
    /// [`WaveformState::value_q15`], which intercepts `Random` before
    /// delegating here. In practice every runtime caller goes through
    /// [`WaveformState`], so this `ZERO` branch is only reached by
    /// direct callers / tests.
    pub fn value_q15(&self, phase: Phase) -> Q15 {
        // Arms follow the declaration order of the enum.
        match self {
            // `−sin(τ · step)` — invert the LUT result. The
            // saturating `Neg` impl on `Q15` clips the single
            // problematic case (`−NEG_ONE`) to `ONE` for us.
            Waveform::BipolarSine => -sine(phase),

            Waveform::BipolarSquare => {
                if phase.is_first_half() {
                    Q15::NEG_ONE
                } else {
                    Q15::ONE
                }
            }

            // Sawtooth in `[−1, +1]`. The `i16` reinterpretation
            // of the cycle position followed by `wrapping_neg`
            // produces:
            //   phase 0          →  0
            //   phase ≈ HALF     → ≈ −1
            //   phase HALF       → −1   (i16 wrap)
            //   phase ≈ 1 cycle  → ≈  0+
            // — i.e. ramp 0 → −1 over the first half, then jump
            // to +1 and ramp back to 0 over the second half.
            Waveform::BipolarRampDown => Q15::from_raw(phase.raw_as_i16().wrapping_neg()),

            // Mirror of `BipolarRampDown`: the raw `i16`
            // reinterpretation *without* the negation. Ramps
            // `0 → +1` over the first half, jumps to `−1` at the
            // mid-point, then `−1 → 0`.
            Waveform::BipolarRampUp => Q15::from_raw(phase.raw_as_i16()),

            // Bipolar symmetric triangle: linear between the cycle
            // anchors `ZERO → ONE → ZERO → NEG_ONE → ZERO` at the
            // quarter points (continuous — no discontinuity, unlike
            // the ramps). `t` is the position within the current
            // quarter as a `[0, ONE]` ramp; each quarter is that ramp,
            // mirrored / negated to walk between its two anchors.
            Waveform::BipolarTriangle => {
                let into_quarter = phase.raw() % Phase::QUARTER.raw();
                let t = Q15::from_ratio(into_quarter as i32, Phase::QUARTER.raw() as i32);
                if phase.raw() < Phase::QUARTER.raw() {
                    t // ............................ ZERO → +ONE
                } else if phase.raw() < Phase::HALF.raw() {
                    Q15::ONE.saturating_sub(t) // ... +ONE → ZERO
                } else if phase.raw() < Phase::THREE_QUARTERS.raw() {
                    t.neg() // ...................... ZERO → −ONE
                } else {
                    Q15::ONE.saturating_sub(t).neg() // −ONE → ZERO
                }
            }

            // Auto-vibrato sine: `+sin` (the LUT result un-negated),
            // so the pitch rises during the first half-cycle.
            Waveform::AutoVibSine => sine(phase),

            // Auto-vibrato square: high (`+1`) first half, low (`−1`)
            // second half — the rising-first counterpart of
            // `BipolarSquare`.
            Waveform::AutoVibSquare => {
                if phase.is_first_half() {
                    Q15::ONE
                } else {
                    Q15::NEG_ONE
                }
            }

            // Auto-vibrato ramp-down: linear `+1 → −1` across the
            // whole cycle, peak at phase 0, discontinuity at the
            // cycle boundary. `value = 1 − 2·phase_fraction`,
            // computed in `i32` then narrowed (the result stays in
            // `[−ONE, +ONE]`, so the `i16` cast never truncates).
            Waveform::AutoVibRampDown => {
                let p = phase.to_q15_unsigned().raw() as i32;
                Q15::from_raw((Q15::ONE.raw() as i32 - 2 * p) as i16)
            }

            // No RNG in a pure `&self` shape descriptor → neutral
            // `ZERO`. The real signed draw is produced by
            // `WaveformState::value_q15`, which never reaches here for
            // `Random`.
            Waveform::Random => Q15::ZERO,
        }
    }
}

/// Running LFO evaluator: pairs a [`Waveform`] selection with a
/// per-instance [`XorShift32`] seed. The seed only matters for
/// `Waveform::Random`; the deterministic shapes ignore it but
/// keep it threaded so `WaveformState` is uniformly cheap to
/// snapshot.
///
/// [`XorShift32`]: crate::core::xorshift::XorShift32
#[derive(Default, Clone, Copy, Debug)]
pub struct WaveformState {
    wf: Waveform,
    rng: XorShift32,
}

impl WaveformState {
    pub fn new(wf: Waveform) -> Self {
        Self {
            wf,
            rng: XorShift32::default(),
        }
    }

    /// Current waveform shape. Mainly useful for diagnostics and
    /// for validating channel-side LFO state against the
    /// `AutomationLane` reconstruction.
    #[inline]
    pub fn waveform(&self) -> Waveform {
        self.wf
    }

    /// Q-format LFO sampler. Returns the waveform amplitude at
    /// the given [`Phase`]. The `Random` shape consults the
    /// per-instance RNG and returns a **bipolar** value in
    /// `[-1, +1]`, re-drawn every call (i.e. every tick).
    ///
    /// Bipolarity matches the trackers: IT's original auto-vibrato
    /// (`IT_MUSIC.ASM`) draws `(Random & 127) − 64 ∈ [-64, +63]` and
    /// applies it through `PitchSlideUp`/`Down` depending on sign;
    /// Schism's `rn_vibrato`/`rn_sample_vibrato` use `128·rand − 64`.
    /// Both the cell-level effect LFO and the instrument auto-vibrato
    /// consume this through their own [`WaveformState`], so a single
    /// signed draw is correct for every caller.
    pub fn value_q15(&mut self, phase: Phase) -> Q15 {
        if let Waveform::Random = self.wf {
            // Pure-integer RNG → signed Q15: reinterpret the raw
            // draw as an `i16`, giving a uniform value across the
            // full bipolar `[-1, +1]` range.
            let n = self.rng.next().unwrap() as i16;
            Q15::from_raw(n)
        } else {
            self.wf.value_q15(phase)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Q15-raw approximate equality (within `tol` LSBs).
    fn approx_eq_q15(a: Q15, b: Q15, tol: i16) -> bool {
        (a.raw() as i32 - b.raw() as i32).abs() <= tol as i32
    }

    #[test]
    fn bipolar_sine_falls_first() {
        // Cell-effect sine is `−sin`: 0 at phase 0, −1 at the
        // quarter, 0 at the half, +1 at three-quarters — the
        // sign-opposite of `AutoVibSine`.
        let wf = Waveform::BipolarSine;
        assert!(approx_eq_q15(wf.value_q15(Phase::ZERO), Q15::ZERO, 4));
        assert!(approx_eq_q15(wf.value_q15(Phase::QUARTER), Q15::NEG_ONE, 4));
        assert!(approx_eq_q15(wf.value_q15(Phase::HALF), Q15::ZERO, 4));
        assert!(approx_eq_q15(
            wf.value_q15(Phase::THREE_QUARTERS),
            Q15::ONE,
            4
        ));
    }

    #[test]
    fn random_waveform_not_stuck() {
        let mut ws = WaveformState::new(Waveform::Random);
        let first = ws.value_q15(Phase::ZERO);
        let second = ws.value_q15(Phase::ZERO);
        // With a proper RNG, two consecutive values should differ.
        assert_ne!(first.raw(), second.raw(), "Random waveform appears stuck");
    }

    #[test]
    fn random_waveform_is_bipolar() {
        // The IT/Schism `Random` LFO shape is bipolar `[-1, +1]`
        // (symmetric around 0). Over a long run we must observe both
        // signs — the old unipolar draw (`n & 0x7FFF` masking) never
        // went negative.
        let mut ws = WaveformState::new(Waveform::Random);
        let mut saw_negative = false;
        let mut saw_positive = false;
        for _ in 0..1024 {
            let v = ws.value_q15(Phase::ZERO).raw();
            saw_negative |= v < 0;
            saw_positive |= v > 0;
        }
        assert!(
            saw_negative && saw_positive,
            "Random should span both signs (neg={saw_negative}, pos={saw_positive})"
        );
    }

    #[test]
    fn autovib_sine_rises_first() {
        // `+sin`, bipolar: 0 at phase 0, +1 at the quarter, 0 at the
        // half, −1 at three-quarters. Distinct from `BipolarSine`
        // (`−sin`, which falls first).
        let wf = Waveform::AutoVibSine;
        assert!(approx_eq_q15(wf.value_q15(Phase::ZERO), Q15::ZERO, 4));
        assert!(approx_eq_q15(wf.value_q15(Phase::QUARTER), Q15::ONE, 4));
        assert!(approx_eq_q15(wf.value_q15(Phase::HALF), Q15::ZERO, 4));
        assert!(approx_eq_q15(
            wf.value_q15(Phase::THREE_QUARTERS),
            Q15::NEG_ONE,
            4
        ));
        // Sign-opposite of `BipolarSine` at the quarter points.
        assert_eq!(
            wf.value_q15(Phase::QUARTER).raw(),
            -Waveform::BipolarSine.value_q15(Phase::QUARTER).raw()
        );
    }

    #[test]
    fn autovib_square_rises_first() {
        let wf = Waveform::AutoVibSquare;
        assert_eq!(wf.value_q15(Phase::ZERO), Q15::ONE);
        assert_eq!(wf.value_q15(Phase::QUARTER), Q15::ONE);
        assert_eq!(wf.value_q15(Phase::HALF), Q15::NEG_ONE);
        assert_eq!(wf.value_q15(Phase::THREE_QUARTERS), Q15::NEG_ONE);
        // Opposite phase to the cell `BipolarSquare` (which falls
        // first): signs differ at phase 0.
        assert!(
            wf.value_q15(Phase::ZERO).raw() > 0
                && Waveform::BipolarSquare.value_q15(Phase::ZERO).raw() < 0
        );
    }

    #[test]
    fn autovib_ramp_down_peaks_at_phase_zero() {
        // Linear `+1 → −1` across the cycle, discontinuity at the
        // boundary — the IT `ramp_down_table` shape.
        let wf = Waveform::AutoVibRampDown;
        assert!(approx_eq_q15(wf.value_q15(Phase::ZERO), Q15::ONE, 2));
        assert!(approx_eq_q15(wf.value_q15(Phase::HALF), Q15::ZERO, 4));
        assert!(approx_eq_q15(
            wf.value_q15(Phase::THREE_QUARTERS),
            Q15::HALF.neg(),
            4
        ));
    }

    #[test]
    fn bipolar_ramp_up_mirrors_ramp_down() {
        // `BipolarRampUp(p) == −BipolarRampDown(p)` everywhere.
        const SAMPLES: u32 = 64;
        const STEP: u16 = ((1u32 << 16) / SAMPLES) as u16;
        for k in 0..SAMPLES {
            let phase = Phase::from_raw((k as u16).wrapping_mul(STEP));
            let up = Waveform::BipolarRampUp.value_q15(phase).raw();
            let down = Waveform::BipolarRampDown.value_q15(phase).raw();
            assert_eq!(
                up,
                down.wrapping_neg(),
                "mismatch at phase {:#x}",
                phase.raw()
            );
        }
    }
}
