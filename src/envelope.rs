use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::fixed::fixed::Q15;
use crate::fixed::units::EnvValue;

/// Envelope Point, frame for the abscissa, value for the ordinate
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug)]
pub struct EnvelopePoint {
    /// Frame number of the point (X-coordinate)
    pub frame: usize,
    /// Value of the point (Y-coordinate). [`EnvValue`] wraps a
    /// Q1.15 in `[0, 1]` (volume / pitch envelopes) or
    /// `[-1, 1]` (signed-magnitude pan envelopes in some
    /// formats). The previous `f32` field was always populated
    /// with a value in those ranges, so the change is exact —
    /// modulo one LSB of Q1.15 quantisation, well below the
    /// envelope's audible resolution.
    pub value: EnvValue,
}

impl EnvelopePoint {
    /// Linear interpolation between two envelope points, in
    /// Q1.15. Equivalent to the previous `f32` lerp:
    ///   `a.value * (1.0 - p) + b.value * p`
    /// with `p = (pos - a.frame) / (b.frame - a.frame)`,
    /// expressed in saturating fixed-point arithmetic. The
    /// boundary cases (`pos <= a.frame`, `pos >= b.frame`,
    /// `a.frame == b.frame`) are handled the same way as
    /// before.
    pub fn lerp(a: &EnvelopePoint, b: &EnvelopePoint, pos: usize) -> EnvValue {
        if pos <= a.frame {
            a.value
        } else if pos >= b.frame {
            b.value
        } else {
            // (pos - a.frame) and (b.frame - a.frame) both
            // fit comfortably in i32 for any realistic
            // envelope (frames are bounded by the module's
            // pattern length × tempo).
            let num = (pos - a.frame) as i32;
            let den = (b.frame - a.frame) as i32;
            // p = num / den, in Q1.15.
            let p = Q15::from_ratio(num, den);
            // Linear interpolation between the two envelope
            // points, in the typed Q-format domain.
            EnvValue::lerp(a.value, b.value, p)
        }
    }
}

/// Envelope
#[derive(Default, Serialize, Deserialize, Clone, Debug)]
pub struct Envelope {
    pub enabled: bool,

    pub point: Vec<EnvelopePoint>,

    pub sustain_enabled: bool,
    /// index in `point`
    pub sustain_start_point: usize,
    /// index in `point`
    pub sustain_end_point: usize,

    pub loop_enabled: bool,
    /// index in `point`
    pub loop_start_point: usize,
    /// index in `point`
    pub loop_end_point: usize,
}

impl Envelope {
    /// Returns `true` when this envelope has a sane, playable shape:
    /// at least 2 points (a single point is degenerate — FT2/IT ignore it),
    /// no more than 12 for XM compatibility, and every declared sustain /
    /// loop index in range.
    ///
    /// Used by importers as a sanity check before handing an envelope to
    /// the replayer. A parsed envelope that returns `false` here should
    /// be replaced by `Envelope::default()` (which leaves `enabled = false`
    /// and ignores the envelope at runtime).
    pub fn is_valid(&self) -> bool {
        self.point.len() >= 2
            && self.point.len() <= 12
            && self.sustain_start_point < self.point.len()
            && self.sustain_end_point < self.point.len()
            && self.loop_start_point < self.point.len()
            && self.loop_end_point < self.point.len()
            && self.loop_start_point <= self.loop_end_point
            && self.sustain_start_point <= self.sustain_end_point
    }

    pub fn loop_in_sustain(&self, frame: usize) -> usize {
        if self.sustain_enabled
            && self.sustain_end_point < self.point.len()
            && self.sustain_start_point < self.point.len()
        {
            let sustain_end = self.point[self.sustain_end_point].frame;
            if frame > sustain_end {
                return self.point[self.sustain_start_point].frame;
            }
        }
        frame
    }

    pub fn loop_in_loop(&self, frame: usize) -> usize {
        if self.loop_enabled
            && self.loop_end_point < self.point.len()
            && self.loop_start_point < self.point.len()
        {
            let loop_end = self.point[self.loop_end_point].frame;
            if frame > loop_end {
                return self.point[self.loop_start_point].frame;
            }
        }
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Helper: build an envelope value from an integer ratio,
    /// mirroring the float-era literals: `ev(0, 1) == 0.0`,
    /// `ev(1, 1) == 1.0`, `ev(1, 2) == 0.5`.
    fn ev(num: i32, den: i32) -> EnvValue {
        EnvValue::from_q15(Q15::from_ratio(num, den))
    }

    #[test]
    fn lerp_at_boundaries() {
        let a = EnvelopePoint {
            frame: 0,
            value: ev(0, 1),
        };
        let b = EnvelopePoint {
            frame: 10,
            value: ev(1, 1),
        };
        assert_eq!(EnvelopePoint::lerp(&a, &b, 0), ev(0, 1));
        assert_eq!(EnvelopePoint::lerp(&a, &b, 10), ev(1, 1));
        // Midpoint should be approximately 0.5. Exact equality
        // is not guaranteed because `Q15::ONE` is `0x7FFF`, so
        // `0x7FFF >> 1 = 0x3FFF`, one LSB short of `HALF`
        // (`0x4000`). The previous f32 test used `< 1e-5`; we
        // tolerate a few raw Q15 LSBs (≈ 1.5e-4 ≈ -76 dBFS).
        let mid = EnvelopePoint::lerp(&a, &b, 5);
        let drift = (mid.raw_q15().raw() - Q15::HALF.raw()).abs();
        assert!(drift <= 4, "midpoint drift {} LSB > 4", drift);
    }

    #[test]
    fn lerp_before_and_after() {
        let a = EnvelopePoint {
            frame: 5,
            value: ev(0, 1),
        };
        let b = EnvelopePoint {
            frame: 15,
            value: ev(1, 1),
        };
        assert_eq!(EnvelopePoint::lerp(&a, &b, 0), ev(0, 1)); // before a
        assert_eq!(EnvelopePoint::lerp(&a, &b, 20), ev(1, 1)); // after b
    }

    #[test]
    fn loop_in_sustain_empty_points_no_panic() {
        let e = Envelope {
            sustain_enabled: true,
            sustain_start_point: 5,
            sustain_end_point: 10,
            point: vec![], // empty — out of bounds
            ..Default::default()
        };
        // Should not panic, just return frame
        assert_eq!(e.loop_in_sustain(100), 100);
    }

    #[test]
    fn loop_in_loop_empty_points_no_panic() {
        let e = Envelope {
            loop_enabled: true,
            loop_start_point: 0,
            loop_end_point: 5,
            point: vec![], // empty — out of bounds
            ..Default::default()
        };
        assert_eq!(e.loop_in_loop(100), 100);
    }

    #[test]
    fn loop_in_sustain_wraps() {
        let e = Envelope {
            sustain_enabled: true,
            sustain_start_point: 1,
            sustain_end_point: 2,
            point: vec![
                EnvelopePoint {
                    frame: 0,
                    value: ev(0, 1),
                },
                EnvelopePoint {
                    frame: 10,
                    value: ev(1, 2),
                },
                EnvelopePoint {
                    frame: 20,
                    value: ev(1, 1),
                },
            ],
            ..Default::default()
        };
        assert_eq!(e.loop_in_sustain(15), 15); // before sustain_end
        assert_eq!(e.loop_in_sustain(25), 10); // past sustain_end → sustain_start
    }
}
