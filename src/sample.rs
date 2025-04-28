use num_enum::{IntoPrimitive, TryFromPrimitive};
use serde::{Deserialize, Serialize};

use alloc::string::String;
use alloc::vec::Vec;

/// How to play sample
#[derive(Default, Serialize, Deserialize, Copy, Clone, IntoPrimitive, TryFromPrimitive, Debug, PartialEq)]
#[repr(u8)]
pub enum LoopType {
    #[default]
    No = 0,
    Forward = 1,
    PingPong = 2,
}

/// is sample recorded with 8 or 16 bits depth
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum SampleDataType {
    Mono8(Vec<i8>),
    Mono16(Vec<i16>),
    Stereo8(Vec<i8>),
    Stereo16(Vec<i16>),
    StereoFloat(Vec<f32>),
}

impl SampleDataType {
    pub fn len(&self) -> usize {
        match &self {
            SampleDataType::Mono8(v) => v.len(),
            SampleDataType::Mono16(v) => v.len(),
            SampleDataType::Stereo8(v) => v.len() / 2,
            SampleDataType::Stereo16(v) => v.len() / 2,
            SampleDataType::StereoFloat(v) => v.len() / 2,
        }
    }
}

/// A Real Data sample
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Sample {
    /// Name
    pub name: String,
    /// [-96..95] with 0 <=> C-4
    pub relative_pitch: i8,
    /// [-1..1]
    pub finetune: f32,
    /// [0..1] linear value
    pub volume: f32,
    /// [0..1] <=> [left..right]
    pub panning: f32,

    /// loop type
    pub loop_flag: LoopType,
    /// 0 <= loop_start < len()
    pub loop_start: u32,
    /// 1 <= loop_length <= len() - loop_start
    pub loop_length: u32,

    /// sustain loop type
    pub sustain_loop_flag: LoopType,
    /// 0 <= sustain_loop_start < len()
    pub sustain_loop_start: u32,
    /// 1 <= sustain_loop_length <= len() - sustain_loop_start
    pub sustain_loop_length: u32,

    /// wave data
    pub data: Option<SampleDataType>,
}

impl Sample {
    /// return sample length
    pub fn len(&self) -> usize {
        if let Some(d) = &self.data {
            d.len()
        } else {
            0
        }
    }

    /// return sample at seek
    pub fn at(&self, seek: usize) -> (f32, f32) {
        match &self.data {
            Some(SampleDataType::Mono8(v)) => (v[seek] as f32 / 128.0, v[seek] as f32 / 128.0),
            Some(SampleDataType::Mono16(v)) => (v[seek] as f32 / 32768.0, v[seek] as f32 / 32768.0),
            Some(SampleDataType::Stereo8(v)) => {
                (v[seek * 2] as f32 / 128.0, v[seek * 2 + 1] as f32 / 128.0)
            }
            Some(SampleDataType::Stereo16(v)) => (
                v[seek * 2] as f32 / 32768.0,
                v[seek * 2 + 1] as f32 / 32768.0,
            ),
            Some(SampleDataType::StereoFloat(v)) => (v[seek * 2], v[seek * 2 + 1]),
            None => (0.0, 0.0),
        }
    }

    pub fn clamp(&mut self) {
        self.volume = self.volume.clamp(0.0, 1.0);
        self.panning = self.panning.clamp(0.0, 1.0);
        self.finetune = self.finetune.clamp(-1.0, 1.0);
        self.relative_pitch = self.relative_pitch.clamp(-95, 96);

        if self.sustain_loop_start as usize > self.len() {
            self.sustain_loop_start = 0;
        }
        if self.sustain_loop_start as usize + self.sustain_loop_length as usize > self.len() {
            self.sustain_loop_length = self.len() as u32 - self.sustain_loop_start;
        }

        if self.loop_start as usize > self.len() {
            self.loop_start = 0;
        }
        if self.loop_start as usize + self.loop_length as usize > self.len() {
            self.loop_length = self.len() as u32 - self.loop_start;
        }
    }


    fn calculate_loop(&self, pos: usize, start: usize, length: usize, loop_type: LoopType) -> usize {
        let end = start + length;
        match loop_type {
            LoopType::No => {
                if pos < self.len() {
                    pos
                } else {
                    self.len() - 1
                }
            }
            LoopType::Forward => {
                if pos < end {
                    pos
                } else {
                    start + (pos - start) % length
                }
            }
            LoopType::PingPong => {
                if pos < end {
                    pos
                } else {
                    let total_length = 2 * length;
                    let mod_pos = (pos - start) % total_length;
                    if mod_pos < length {
                        start + mod_pos
                    } else {
                        end - (mod_pos - length) - 1
                    }
                }
            }
        }
    }

    /// Returns the real position in the sample
    /// The calling function must save the real position at sustain end to avoid problems
    pub fn meta_seek(&self, pos: usize, sustain: bool) -> usize {
        if sustain && self.sustain_loop_flag != LoopType::No {
            self.calculate_loop(pos, self.sustain_loop_start as usize, self.sustain_loop_length as usize, self.sustain_loop_flag)
        } else {
            self.calculate_loop(pos, self.loop_start as usize, self.loop_length as usize, self.loop_flag)
        }
    }

    /// return sample size (8 or 16 bits)
    pub fn bits(&self) -> u8 {
        match &self.data {
            Some(SampleDataType::Mono8(_)) => 8,
            Some(SampleDataType::Mono16(_)) => 16,
            Some(SampleDataType::Stereo8(_)) => 8,
            Some(SampleDataType::Stereo16(_)) => 16,
            Some(SampleDataType::StereoFloat(_)) => 32,
            None => 0,
        }
    }
}
