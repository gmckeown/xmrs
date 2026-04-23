use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// Envelope Point, frame for the abscissa, value for the ordinate
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug)]
pub struct EnvelopePoint {
    /// Frame number of the point (X-coordinate)
    pub frame: usize,
    /// Value of the point (Y-coordinate) [0..1.0]
    pub value: f32,
}

impl EnvelopePoint {
    /// Linear interpolation between two envelope points
    pub fn lerp(a: &EnvelopePoint, b: &EnvelopePoint, pos: usize) -> f32 {
        if pos <= a.frame {
            a.value
        } else if pos >= b.frame {
            b.value
        } else {
            let p: f32 = (pos - a.frame) as f32 / (b.frame - a.frame) as f32;
            a.value * (1.0 - p) + b.value * p
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

    #[test]
    fn lerp_at_boundaries() {
        let a = EnvelopePoint {
            frame: 0,
            value: 0.0,
        };
        let b = EnvelopePoint {
            frame: 10,
            value: 1.0,
        };
        assert_eq!(EnvelopePoint::lerp(&a, &b, 0), 0.0);
        assert_eq!(EnvelopePoint::lerp(&a, &b, 10), 1.0);
        assert!((EnvelopePoint::lerp(&a, &b, 5) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn lerp_before_and_after() {
        let a = EnvelopePoint {
            frame: 5,
            value: 0.0,
        };
        let b = EnvelopePoint {
            frame: 15,
            value: 1.0,
        };
        assert_eq!(EnvelopePoint::lerp(&a, &b, 0), 0.0); // before a
        assert_eq!(EnvelopePoint::lerp(&a, &b, 20), 1.0); // after b
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
                    value: 0.0,
                },
                EnvelopePoint {
                    frame: 10,
                    value: 0.5,
                },
                EnvelopePoint {
                    frame: 20,
                    value: 1.0,
                },
            ],
            ..Default::default()
        };
        assert_eq!(e.loop_in_sustain(15), 15); // before sustain_end
        assert_eq!(e.loop_in_sustain(25), 10); // past sustain_end → sustain_start
    }
}
