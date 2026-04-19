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
            return a.value;
        } else if pos >= b.frame {
            return b.value;
        } else {
            let p: f32 = (pos - a.frame) as f32 / (b.frame - a.frame) as f32;
            return a.value * (1.0 - p) + b.value * p;
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
