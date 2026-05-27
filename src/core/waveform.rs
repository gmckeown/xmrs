//! [`Waveform`] — LFO waveform shapes used by vibrato / tremolo /
//! panbrello (cell-level and instrument-level alike).
//!
//! Five shapes — `Sine`, `RampDown`, `Square`, `RampUp`, `Random` —
//! evaluated at a phase angle in Q1.15. [`WaveformState`] holds
//! the running phase and the RNG seed for the `Random` waveform,
//! advancing one step per tick.

use super::xorshift::XorShift32;
use crate::core::fixed::fixed::Q15;
use crate::core::fixed::tables::sine;
use crate::core::fixed::units::Phase;
use serde::{Deserialize, Serialize};

/// LFO waveform shape — the phase → amplitude mapping every
/// vibrato / tremolo / panbrello LFO consults.
///
/// Each variant pair (`Translated*` vs. its non-translated sibling)
/// exists for FT2 compatibility: FT2 evaluates its tables with the
/// phase pre-translated by π/2, which matters for cycle-exact
/// playback of XM modules. Editor-authored content typically uses
/// the non-translated variants.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, Eq, Hash, PartialEq)]
pub enum Waveform {
    #[default]
    TranslatedSine,
    TranslatedSquare,
    TranslatedRampUp,
    TranslatedRampDown,
    Sine,
    RampDown,
    Square,
    Random,
}

impl Waveform {
    /// Evaluate the waveform at a given cycle [`Phase`]. Returns
    /// a [`Q15`]:
    ///
    /// * Audio shapes ([`Self::Sine`], [`Self::RampDown`],
    ///   [`Self::Square`]) span the full `[-1, +1]` range.
    /// * Translated shapes (used by envelope-style LFOs) span
    ///   only the positive half `[0, 1]`.
    ///
    /// [`Self::Random`] returns [`Q15::ZERO`]; the actual
    /// RNG-driven value lives on [`WaveformState`].
    pub fn value_q15(&self, phase: Phase) -> Q15 {
        match self {
            // 0.5 + 0.5 · cos(τ · step)
            //   = (cos(τ · step) + 1) / 2
            // `cos = sin(· + π/2)` → quarter-cycle phase shift.
            // Halve the cosine, re-centre to `[0, 1]`.
            Waveform::TranslatedSine => sine(phase.shifted(Phase::QUARTER)).halved() + Q15::HALF,

            // Half-cycle high then half-cycle low.
            Waveform::TranslatedSquare => {
                if phase.is_first_half() {
                    Q15::ONE
                } else {
                    Q15::ZERO
                }
            }

            // Sawtooth in `[0, 1]`. A half-cycle phase shift moves
            // the discontinuity from the cycle boundary to the
            // mid-point — that's the envelope LFO convention.
            Waveform::TranslatedRampUp => phase.shifted(Phase::HALF).to_q15_unsigned(),

            // Mirror of `TranslatedRampUp` about `Q15::HALF`:
            // `down(p) = 1 − up(p)`. (Off by 1 LSB at the
            // saturation boundary, well below the quantisation
            // floor — within the existing test tolerance.)
            Waveform::TranslatedRampDown => Q15::ONE - Waveform::TranslatedRampUp.value_q15(phase),

            // `−sin(τ · step)` — invert the LUT result. The
            // saturating `Neg` impl on `Q15` clips the single
            // problematic case (`−NEG_ONE`) to `ONE` for us.
            Waveform::Sine => -sine(phase),

            // Sawtooth in `[−1, +1]`. The `i16` reinterpretation
            // of the cycle position followed by `wrapping_neg`
            // produces:
            //   phase 0          →  0
            //   phase ≈ HALF     → ≈ −1
            //   phase HALF       → −1   (i16 wrap)
            //   phase ≈ 1 cycle  → ≈  0+
            // — i.e. ramp 0 → −1 over the first half, then jump
            // to +1 and ramp back to 0 over the second half.
            Waveform::RampDown => Q15::from_raw(phase.raw_as_i16().wrapping_neg()),

            Waveform::Square => {
                if phase.is_first_half() {
                    Q15::NEG_ONE
                } else {
                    Q15::ONE
                }
            }

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
    /// per-instance RNG and returns a value in `[0, ONE]`
    /// (positive Q15 only).
    pub fn value_q15(&mut self, phase: Phase) -> Q15 {
        if let Waveform::Random = self.wf {
            // Pure-integer RNG → unsigned Q15. Mask to the
            // positive Q15 range.
            let n = self.rng.next().unwrap() as i16;
            Q15::from_raw(n & Q15::ONE.raw())
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
    fn translated_sine_range() {
        // Sweep a full cycle. 256 samples is more than enough to
        // catch any negative excursion that would mean we broke
        // the unsigned shape.
        const SAMPLES_PER_CYCLE: u32 = 256;
        const STEP: u16 = ((1u32 << 16) / SAMPLES_PER_CYCLE) as u16;
        let wf = Waveform::TranslatedSine;
        for k in 0..SAMPLES_PER_CYCLE {
            let phase = Phase::from_raw((k as u16).wrapping_mul(STEP));
            let v = wf.value_q15(phase);
            assert!(
                v.raw() >= 0,
                "TranslatedSine should be unsigned, got {} at phase {:#x}",
                v.raw(),
                phase.raw()
            );
        }
    }

    #[test]
    fn translated_sine_key_points() {
        let wf = Waveform::TranslatedSine;
        // Cosine tour: 0 → max ≈ ONE, 1/4 → centre, 1/2 → 0,
        // 3/4 → centre.
        assert!(approx_eq_q15(wf.value_q15(Phase::ZERO), Q15::ONE, 4));
        assert!(approx_eq_q15(wf.value_q15(Phase::QUARTER), Q15::HALF, 4));
        assert!(approx_eq_q15(wf.value_q15(Phase::HALF), Q15::ZERO, 4));
        assert!(approx_eq_q15(
            wf.value_q15(Phase::THREE_QUARTERS),
            Q15::HALF,
            4
        ));
    }

    #[test]
    fn translated_ramps_centre_at_phase_zero() {
        // Both translated ramps start at the cycle midpoint (= 0.5).
        assert!(approx_eq_q15(
            Waveform::TranslatedRampUp.value_q15(Phase::ZERO),
            Q15::HALF,
            2
        ));
        assert!(approx_eq_q15(
            Waveform::TranslatedRampDown.value_q15(Phase::ZERO),
            Q15::HALF,
            2
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
}
