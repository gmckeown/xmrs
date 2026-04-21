use serde::{Deserialize, Serialize};

use crate::waveform::Waveform;

// Float math backend (only needed when `std` is disabled).
// Priority: std > libm > micromath.
#[cfg(all(not(feature = "std"), not(feature = "libm"), feature = "micromath"))]
#[allow(unused_imports)]
use micromath::F32Ext;
#[cfg(all(not(feature = "std"), feature = "libm"))]
#[allow(unused_imports)]
use num_traits::float::Float;

/// Instrument Vibrato
#[derive(Default, Serialize, Deserialize, Clone, Copy, Debug)]
pub struct Vibrato {
    pub waveform: Waveform,
    pub speed: f32,
    pub depth: f32,
    pub sweep: f32,
}
