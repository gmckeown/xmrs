use super::xorshift::XorShift32;
use serde::{Deserialize, Serialize};

#[cfg(feature = "micromath")]
#[allow(unused_imports)]
use micromath::F32Ext;
#[cfg(feature = "libm")]
#[allow(unused_imports)]
use num_traits::float::Float;

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
    fn value(&self, step: f32) -> f32 {
        let step = step % 1.0;
        return match self {
            Waveform::TranslatedSine => 0.5 + 0.5 * (core::f32::consts::TAU * (step + 0.25)).sin(),
            Waveform::TranslatedSquare => {
                if step < 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
            Waveform::TranslatedRampUp => {
                if step < 0.5 {
                    0.5 + step
                } else {
                    step - 0.5
                }
            }
            Waveform::TranslatedRampDown => {
                if step < 0.5 {
                    0.5 - step
                } else {
                    1.5 - step
                }
            }
            Waveform::Sine => -(core::f32::consts::TAU * step).sin(),
            Waveform::RampDown => -2.0 * step + if step < 0.5 { 0.0 } else { 2.0 },
            Waveform::Square => {
                if step < 0.5 {
                    -1.0
                } else {
                    1.0
                }
            }
            Waveform::Random => 0.0,
        };
    }
}

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

    pub fn value(&mut self, step: f32) -> f32 {
        if let Waveform::Random = self.wf {
            self.rng.next_f32()
        } else {
            self.wf.value(step)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn translated_sine_range() {
        let wf = Waveform::TranslatedSine;
        for i in 0..100 {
            let v = wf.value(i as f32 / 100.0);
            assert!(v >= 0.0 && v <= 1.0, "TranslatedSine({}) = {}", i, v);
        }
    }

    #[test]
    fn translated_sine_key_points() {
        let wf = Waveform::TranslatedSine;
        assert!(approx_eq(wf.value(0.0), 1.0)); // starts at max
        assert!(approx_eq(wf.value(0.25), 0.5)); // crosses center
        assert!(approx_eq(wf.value(0.5), 0.0)); // at min
        assert!(approx_eq(wf.value(0.75), 0.5)); // crosses center back
    }

    #[test]
    fn translated_ramp_down_matches_translated_rampdown() {
        // TranslatedRampDown should equal (RampDown + 1) / 2
        let rd = Waveform::RampDown;
        let trd = Waveform::TranslatedRampDown;
        for i in 0..100 {
            let step = i as f32 / 100.0;
            let expected = (rd.value(step) + 1.0) / 2.0;
            let actual = trd.value(step);
            assert!(
                approx_eq(expected, actual),
                "step={}: expected={}, actual={}",
                step,
                expected,
                actual
            );
        }
    }

    #[test]
    fn translated_ramp_up_matches_translated_rampup() {
        // TranslatedRampUp should equal (-RampDown + 1) / 2
        // because RampUp = -RampDown
        let rd = Waveform::RampDown;
        let tru = Waveform::TranslatedRampUp;
        for i in 0..100 {
            let step = i as f32 / 100.0;
            let expected = (-rd.value(step) + 1.0) / 2.0;
            let actual = tru.value(step);
            assert!(
                approx_eq(expected, actual),
                "step={}: expected={}, actual={}",
                step,
                expected,
                actual
            );
        }
    }

    #[test]
    fn translated_ramp_center() {
        // Both ramps should start at 0.5
        assert!(approx_eq(Waveform::TranslatedRampUp.value(0.0), 0.5));
        assert!(approx_eq(Waveform::TranslatedRampDown.value(0.0), 0.5));
    }

    #[test]
    fn random_waveform_not_stuck() {
        let mut ws = WaveformState::new(Waveform::Random);
        let first = ws.value(0.0);
        let second = ws.value(0.0);
        // With a proper RNG, two consecutive values should differ
        assert_ne!(first, second, "Random waveform appears stuck");
    }
}
