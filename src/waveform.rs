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
