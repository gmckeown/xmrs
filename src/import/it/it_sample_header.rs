use crate::prelude::*;
use alloc::string::String;

use bincode::error::DecodeError;

use alloc::vec;
use alloc::vec::Vec;
use serde::Deserialize;

use super::bitreader::BitReader;
use super::serde_helper::deserialize_string_12;
use super::serde_helper::deserialize_string_26;

// Float math backend (only needed when `std` is disabled).
// Priority: std > libm > micromath.
#[cfg(all(not(feature = "std"), not(feature = "libm"), feature = "micromath"))]
#[allow(unused_imports)]
use micromath::F32Ext;
#[cfg(all(not(feature = "std"), feature = "libm"))]
#[allow(unused_imports)]
use num_traits::float::Float;

/// Structure representing a sample header in the IMPS format.
#[derive(Deserialize, Debug, Default)]
#[repr(C)]
pub struct ItSampleHeader {
    /// "IMPS"
    pub id: [u8; 4],

    /// Name of the DOS file
    #[serde(deserialize_with = "deserialize_string_12")]
    pub dos_filename: String,

    /// Reserved byte for future use.
    /// Length: 1 byte
    pub reserved_byte: u8,

    /// Global volume for the sample (range 0-64).
    /// Length: 1 byte
    pub global_volume: u8,

    /// Flags for the sample configuration.
    /// Length: 1 byte
    /// - Bit 0: Sample associated with header if set
    /// - Bit 1: 16-bit sample if set, 8-bit if clear
    /// - Bit 2: Stereo if set, mono if clear (Stereo samples was not supported in Impulse Tracker, but later trackers added stereo support)
    /// - Bit 3: Compressed sample if set, PCM if clear
    /// - Bit 4: Use loop if set
    /// - Bit 5: Use sustain loop if set
    /// - Bit 6: Ping-Pong loop if set; forwards only if clear
    /// - Bit 7: Ping-Pong sustain loop if set; forwards only if clear
    pub flags: u8,

    /// Default volume for the sample (range 0-64).
    /// Length: 1 byte
    pub default_volume: u8,

    /// Name of the sample (null-terminated).
    /// Length: 26 bytes
    #[serde(deserialize_with = "deserialize_string_26")]
    pub sample_name: String,

    /// Convert flags for the sample.
    /// Length: 1 byte
    /// - Bit 0: Signed samples if set; unsigned if clear (Samples in version 2.01 and earlier are unsigned, samples in 2.02 and later are signed)
    /// - Bit 1: Big endian samples if set; little endian if clear
    /// - Bit 2: Delta encoded samples if set; PCM if clear (Compressed data is double delta encoded if this bit is on. IT 2.14p1 and later use this method and IT 2.14 and earlier always sets this off and delta encodes the data just once)
    /// - Bit 3: Byte delta encoded samples (for PTM loader)
    /// - Bit 4: Uses 12-bit TX-Wave values
    /// - Bit 5: Prompt for Left/Right/All stereo channels
    pub convert_flags: u8,

    /// Default pan setting (range 0-64).
    /// Length: 1 byte
    /// - Bits 0-6: Panning value (0-64)
    /// - Bit 7: Enable panning if set
    pub default_pan: u8,

    /// Length of the sample in bytes.
    /// Length: 4 bytes
    pub sample_length: u32,

    /// Beginning of the sample loop.
    /// Length: 4 bytes
    pub loop_beginning: u32,

    /// End of the sample loop (sample after loop end).
    /// Length: 4 bytes
    pub loop_end: u32,

    /// Sample rate for Middle C (range 0-9,999,999).
    /// Length: 4 bytes
    pub c5_speed: u32,

    /// Beginning of the sustain loop.
    /// Length: 4 bytes
    pub sustain_loop_beginning: u32,

    /// End of the sustain loop (sample after sustain loop end).
    /// Length: 4 bytes
    pub sustain_loop_end: u32,

    /// Pointer to the sample data within the file.
    /// Length: 4 bytes
    pub sample_pointer: u32,

    /// Vibrato speed (range 0-64), controls the frequency of vibrato.
    /// Length: 1 byte
    pub vibrato_speed: u8,

    /// Vibrato depth (range 0-64), determines how much the vibrato can shift pitch.
    /// Length: 1 byte
    pub vibrato_depth: u8,

    /// Vibrato sweep (range 0-64), delays the vibrato gain.
    /// Higher values make the vibrato reach peak depth faster.
    /// Length: 1 byte
    pub vibrato_sweep: u8,

    /// Vibrato waveform type.
    /// Length: 1 byte
    /// - 0: Sine
    /// - 1: Ramp down (Sawtooth)
    /// - 2: Square
    /// - 3: Random sine (possibly single purlin noise channel)
    pub vibrato_waveform: u8,
}

impl ItSampleHeader {
    pub fn is_associated_sample(&self) -> bool {
        self.flags & 0b0000_0001 != 0
    }

    pub fn is_16bits(&self) -> bool {
        self.flags & 0b0000_0010 != 0
    }

    pub fn is_stereo(&self) -> bool {
        self.flags & 0b0000_0100 != 0
    }

    pub fn is_compressed(&self) -> bool {
        self.flags & 0b0000_1000 != 0
    }

    pub fn is_use_loop(&self) -> bool {
        self.flags & 0b0001_0000 != 0
    }

    pub fn is_use_sustain_loop(&self) -> bool {
        self.flags & 0b0010_0000 != 0
    }

    pub fn is_pingpong(&self) -> bool {
        self.is_use_loop() && self.flags & 0b0100_0000 != 0
    }

    pub fn is_pingpong_sustain_loop(&self) -> bool {
        self.is_use_sustain_loop() && self.flags & 0b1000_0000 != 0
    }

    pub fn is_samples_signed(&self) -> bool {
        self.convert_flags & 0b0000_0001 != 0
    }

    pub fn is_samples_be(&self) -> bool {
        self.convert_flags & 0b0000_0010 != 0
    }

    pub fn is_double_delta_encoded(&self) -> bool {
        self.convert_flags & 0b0000_0100 != 0
    }

    pub fn is_byte_delta_encoded(&self) -> bool {
        self.convert_flags & 0b0000_1000 != 0
    }

    pub fn use_12bit_tx_wave(&self) -> bool {
        self.convert_flags & 0b0001_0000 != 0
    }

    pub fn prompt_stereo(&self) -> bool {
        self.convert_flags & 0b0010_0000 != 0
    }

    fn get_sample_data_from_compressed_values(
        &self,
        data: &[u8],
    ) -> Result<SampleDataType, DecodeError> {
        let dst = if self.is_16bits() {
            if self.is_stereo() {
                let sample_len = 2 * self.sample_length as usize;
                let output = self.it_unpack_16bit(data, sample_len)?;
                SampleDataType::Stereo16(self.convert_16bit_sample(output.as_slice()))
            } else {
                let sample_len = self.sample_length as usize;
                let output = self.it_unpack_16bit(data, sample_len)?;
                SampleDataType::Mono16(self.convert_16bit_sample(output.as_slice()))
            }
        } else {
            if self.is_stereo() {
                let sample_len = 2 * self.sample_length as usize;
                let output = self.it_unpack_8bit(data, sample_len)?;
                SampleDataType::Stereo8(self.convert_8bit_sample(output.as_slice()))
            } else {
                let sample_len = self.sample_length as usize;
                let output = self.it_unpack_8bit(data, sample_len)?;
                SampleDataType::Mono8(self.convert_8bit_sample(output.as_slice()))
            }
        };

        Ok(dst)
    }

    fn get_sample_data_from_values(&self, data: &[u8]) -> Result<SampleDataType, DecodeError> {
        let dst = if self.is_16bits() {
            if self.is_stereo() {
                let sample_len = 2 * 2 * self.sample_length as usize;
                let src = self.convert_u8_to_i16_vec(&data[0..sample_len])?;
                let stereo = self.convert_16bit_sample(src.as_slice());
                SampleDataType::Stereo16(stereo)
            } else {
                let sample_len = 2 * self.sample_length as usize;
                let src = self.convert_u8_to_i16_vec(&data[0..sample_len])?;
                SampleDataType::Mono16(self.convert_16bit_sample(src.as_slice()))
            }
        } else {
            if self.is_stereo() {
                let sample_len = 2 * self.sample_length as usize;
                let src: Vec<i8> = data
                    .iter()
                    .take(sample_len)
                    .map(|&byte| byte as i8)
                    .collect();
                SampleDataType::Stereo8(self.convert_8bit_sample(src.as_slice()))
            } else {
                let sample_len = self.sample_length as usize;
                let src: Vec<i8> = data
                    .iter()
                    .take(sample_len)
                    .map(|&byte| byte as i8)
                    .collect();
                SampleDataType::Mono8(self.convert_8bit_sample(src.as_slice()))
            }
        };

        Ok(dst)
    }

    pub fn get_sample_data(&self, data: &[u8]) -> Result<SampleDataType, DecodeError> {
        if self.is_compressed() {
            self.get_sample_data_from_compressed_values(data)
        } else {
            self.get_sample_data_from_values(data)
        }
    }

    fn convert_u8_to_i16_vec(&self, input: &[u8]) -> Result<Vec<i16>, DecodeError> {
        if !input.len().is_multiple_of(2) {
            return Err(DecodeError::Other("input is odd!"));
        }

        let mut output = Vec::with_capacity(input.len() / 2);

        for chunk in input.chunks_exact(2) {
            let value = if self.is_samples_be() {
                i16::from_be_bytes([chunk[0], chunk[1]])
            } else {
                i16::from_le_bytes([chunk[0], chunk[1]])
            };
            output.push(value);
        }
        Ok(output)
    }

    fn convert_8bit_sample(&self, p: &[i8]) -> Vec<i8> {
        let length = p.len();
        let mut dst: Vec<i8> = vec![];
        let xor = if !self.is_samples_signed() {
            0x80u8 as i8
        } else {
            0
        };

        if self.is_stereo() {
            let half_length = length / 2;
            let (left_channel, right_channel) = p.split_at(half_length);

            for i in 0..half_length {
                let l = left_channel[i] ^ xor;
                let r = right_channel[i] ^ xor;
                dst.push(l);
                dst.push(r);
            }
        } else {
            for &v in p.iter().take(length) {
                dst.push(v ^ xor)
            }
        }
        dst
    }

    fn convert_16bit_sample(&self, p: &[i16]) -> Vec<i16> {
        let length = p.len();
        let mut dst: Vec<i16> = vec![];
        let xor = if !self.is_samples_signed() {
            0x8000u16 as i16
        } else {
            0
        };

        if self.is_stereo() {
            let half_length = length / 2;
            let (left_channel, right_channel) = p.split_at(half_length);

            for i in 0..half_length {
                let l = left_channel[i] ^ xor;
                let r = right_channel[i] ^ xor;
                dst.push(l);
                dst.push(r);
            }
        } else {
            for &v in p.iter().take(length) {
                dst.push(v ^ xor)
            }
        }
        dst
    }

    fn it_unpack_8bit(&self, input: &[u8], output_len: usize) -> Result<Vec<i8>, DecodeError> {
        let mut output = Vec::new();
        let mut p_src = input;
        while output.len() < output_len {
            if p_src.len() < 2 {
                return Err(DecodeError::LimitExceeded);
            }

            let block_len = u16::from_le_bytes([p_src[0], p_src[1]]) as usize;
            p_src = &p_src[2..];
            let mut block_output_len = 0;
            let mut left: u8 = 9;
            let mut temp: u8 = 0;
            let mut temp2: u8 = 0;

            if p_src.len() < block_len {
                return Err(DecodeError::LimitExceeded);
            }

            let mut bit_reader = BitReader::new(&p_src[..block_len]);
            p_src = &p_src[block_len..];

            loop {
                if bit_reader.is_empty() {
                    // first exit case: no more input data
                    break;
                }

                if block_output_len == 0x8000 {
                    // second exit case: maximum block size
                    break;
                }

                if output.len() >= output_len {
                    // last exit case: we have all output data
                    break;
                }

                let mut bits: u16 = bit_reader
                    .read_bits(left)
                    .ok_or(DecodeError::LimitExceeded)? as u16;

                if left < 7 {
                    // Type A
                    if 1_u16 << (left - 1) == bits {
                        bits = bit_reader.read_bits(3).ok_or(DecodeError::LimitExceeded)? as u16;
                        left = if bits as u8 + 1 < left {
                            bits as u8 + 1
                        } else {
                            bits as u8 + 1 + 1
                        };
                        continue;
                    }
                } else if left < 9 {
                    // Type B
                    let i: u16 = (0xFF >> (9 - left)) + 4;
                    let j: u16 = i - 8;
                    if bits > j && bits <= i {
                        bits -= j;
                        left = if (bits as u8) < left {
                            bits as u8
                        } else {
                            (bits + 1) as u8
                        };
                        continue;
                    }
                } else if left >= 10 {
                    output.push(0);
                    block_output_len += 1;
                    continue;
                } else if bits >= 256 {
                    left = (bits + 1) as u8;
                    continue;
                }

                // Unpack byte with sign extension
                if left < 8 {
                    let shift: u8 = 8 - left;
                    let mut c: i8 = (bits << shift) as i8;
                    c >>= shift;
                    bits = c as u16;
                }
                bits = bits.wrapping_add(temp as u16);
                temp = bits as u8;
                temp2 = temp2.wrapping_add(temp);

                let value = if self.is_double_delta_encoded() {
                    temp2
                } else {
                    temp
                };
                output.push(value as i8);
                block_output_len += 1;
            }
        }

        Ok(output)
    }

    fn it_unpack_16bit(&self, input: &[u8], output_len: usize) -> Result<Vec<i16>, DecodeError> {
        let mut output = Vec::new();
        let mut p_src = input;
        while output.len() < output_len {
            if p_src.len() < 2 {
                return Err(DecodeError::LimitExceeded);
            }

            let block_len = u16::from_le_bytes([p_src[0], p_src[1]]) as usize;
            p_src = &p_src[2..];
            let mut block_output_len = 0;
            let mut left: u8 = 17;
            let mut temp: i16 = 0;
            let mut temp2: i16 = 0;

            if p_src.len() < block_len {
                return Err(DecodeError::LimitExceeded);
            }

            let mut bit_reader = BitReader::new(&p_src[..block_len]);
            p_src = &p_src[block_len..];

            loop {
                if bit_reader.is_empty() {
                    // first exit case: no more input data
                    break;
                }

                if block_output_len == 0x4000 {
                    // second exit case: maximum block size
                    break;
                }

                if output.len() >= output_len {
                    // last exit case: we have all output data
                    break;
                }

                let mut bits = bit_reader
                    .read_bits(left)
                    .ok_or(DecodeError::LimitExceeded)?;

                if left < 7 {
                    // Type A
                    if 1_u32 << (left - 1) == bits {
                        bits = bit_reader.read_bits(4).ok_or(DecodeError::LimitExceeded)?;
                        left = if bits as u8 + 1 < left {
                            bits as u8 + 1
                        } else {
                            bits as u8 + 1 + 1
                        };
                        continue;
                    }
                } else if left < 17 {
                    // Type B
                    let i: u32 = (0xFFFF >> (17 - left)) + 8;
                    let j: u32 = (i - 16) & 0xFFFF;
                    if bits > j && bits <= (i & 0xFFFF) {
                        bits -= j;
                        left = if (bits as u8) < left {
                            bits as u8
                        } else {
                            (bits + 1) as u8
                        };
                        continue;
                    }
                } else if left >= 18 {
                    output.push(0);
                    block_output_len += 1;
                    continue;
                } else if bits >= 0x10000 {
                    left = (bits + 1) as u8;
                    continue;
                }

                // Unpack byte with sign extension
                if left < 16 {
                    let shift: u8 = 16 - left;
                    let mut c: i16 = (bits << shift) as i16;
                    c >>= shift;
                    bits = c as u32;
                }
                bits = bits.wrapping_add(temp as u32);
                temp = bits as i16;
                temp2 = temp2.wrapping_add(temp);

                let value = if self.is_double_delta_encoded() {
                    temp2
                } else {
                    temp
                };
                output.push(value);
                block_output_len += 1;
            }
        }

        Ok(output)
    }

    fn c5_speed_to_finetune(&self) -> (i8, f32) {
        if self.c5_speed == 0 {
            return (0, 0.0);
        }
        let ratio = self.c5_speed as f32 / 16726.0;
        let semitones = 12.0 * ratio.log2();
        let relative_pitch = semitones.round() as i8;
        let finetune = semitones.fract();
        (relative_pitch, finetune)
    }

    pub fn to_sample(&self, data: &Option<SampleDataType>) -> Sample {
        let name = if !self.sample_name.is_empty() {
            self.sample_name.clone()
        } else {
            self.dos_filename.clone()
        };

        let loop_flag = if self.is_pingpong() {
            LoopType::PingPong
        } else if self.is_use_loop() {
            LoopType::Forward
        } else {
            LoopType::No
        };

        let sustain_loop_flag = if self.is_pingpong_sustain_loop() {
            LoopType::PingPong
        } else if self.is_use_sustain_loop() {
            LoopType::Forward
        } else {
            LoopType::No
        };

        let (relative_pitch, finetune) = self.c5_speed_to_finetune();

        let panning = if self.default_pan & 0x80 != 0 {
            (self.default_pan & 0x7F) as f32 / 64.0
        } else {
            0.5
        };

        let llength = self.loop_end.saturating_sub(self.loop_beginning);

        let sllength = self
            .sustain_loop_end
            .saturating_sub(self.sustain_loop_beginning);

        // IT splits the sample's level into two registers:
        //   * GvL (`global_volume`, 0..64): always applies — a static
        //     scale baked into the sample's playback level.
        //   * Vol (`default_volume`, 0..64): the default NOTE volume,
        //     used only when the pattern cell has no volume-column
        //     override.
        // xmrs's `Sample` has a single `volume` field. Putting GvL
        // alone here is the correct call: it's the always-applies
        // scalar. A pattern cell's `Vxx` column, when present,
        // overrides the note's starting volume but must NOT clobber
        // GvL — so they live in different layers. Vol (the default
        // note volume) is not applied in this importer today; modules
        // that rely on per-sample default-volume tuning for silent-
        // cell notes will ignore it. Fixing this properly needs a
        // `default_note_volume` field on xmrs's Sample or an
        // instrument-trigger hook in the player; neither is worth
        // the breaking change today.
        let gvl_only = self.global_volume as f32 / 64.0;

        Sample {
            name,
            relative_pitch,
            finetune,
            volume: gvl_only.clamp(0.0, 1.0),
            panning,
            loop_start: self.loop_beginning,
            loop_length: llength,
            loop_flag,
            sustain_loop_flag,
            sustain_loop_start: self.sustain_loop_beginning,
            sustain_loop_length: sllength,
            data: data.clone(),
        }
    }

    pub fn to_vibrato(&self) -> Vibrato {
        let wf = match self.vibrato_waveform {
            1 => Waveform::TranslatedRampDown,
            2 => Waveform::TranslatedSquare,
            3 => Waveform::Random,
            _ => Waveform::TranslatedSine,
        };

        // IT `vibrato_sweep` is a depth-increment RATE (how fast to
        // ramp up per tick). xmrs's `Vibrato.sweep` is a phase
        // THRESHOLD — phase must cross it to reach full depth. These
        // are inverse conventions: high IT sweep = fast ramp; high
        // xmrs sweep = slow ramp.
        //
        // Converting: IT ramps from 0 to max in `64 / vibrato_sweep`
        // ticks (approximately). To ramp over the same duration in
        // xmrs, the threshold must equal `speed * ticks_to_full` —
        // which simplifies to `vibrato_speed / vibrato_sweep` after
        // both are normalised by `/64`.
        //
        // `vibrato_sweep = 0` means "instant full depth" per IT2.14
        // (and is what `StateAutoVibrato::tick` already short-
        // circuits on `sweep <= 0`). Map to 0 explicitly to avoid
        // the division by zero.
        let xmrs_sweep = if self.vibrato_sweep == 0 {
            0.0
        } else {
            self.vibrato_speed as f32 / self.vibrato_sweep as f32
        };

        Vibrato {
            waveform: wf,
            speed: self.vibrato_speed as f32 / 64.0,
            depth: self.vibrato_depth as f32 / 64.0,
            sweep: xmrs_sweep,
        }
    }
}
