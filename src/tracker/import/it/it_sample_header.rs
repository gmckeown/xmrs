//! IT sample-header (`IMPS`) parser — decodes one sample's
//! metadata (volume, default pan, finetune, sustain loop, IT-style
//! global volume, vibrato params) plus the offset into the file
//! where its PCM payload lives. The payload itself is loaded by
//! the caller via raw read or `BitReader` decompression.

use crate::core::fixed::fixed::Q8_8;
use crate::core::fixed::units::{ChannelVolume, Finetune, Panning, PitchDelta, Volume};
use crate::prelude::*;
use crate::tracker::import::bin_reader::{bytes_to_trimmed_string, BinReader, ImportError};
use alloc::string::String;

use alloc::vec;
use alloc::vec::Vec;

use super::bitreader::BitReader;

/// On-disk size of an IT sample header.
pub const IT_SAMPLE_HEADER_SIZE: usize = 80;

/// Structure representing a sample header in the IMPS format.
#[derive(Debug, Default)]
pub struct ItSampleHeader {
    /// "IMPS"
    pub id: [u8; 4],

    /// Name of the DOS file
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

/// Accumulate a stream of `u8` deltas into absolute signed `i8`
/// PCM. Wrapping arithmetic mirrors schism's `csf_decode_delta_
/// 8bit_1chn` (`csndfile.c:734-748`).
fn decode_delta_8(input: &[u8]) -> Vec<i8> {
    let mut out: Vec<i8> = Vec::with_capacity(input.len());
    let mut acc: u8 = 0;
    for &b in input {
        acc = acc.wrapping_add(b);
        out.push(acc as i8);
    }
    out
}

/// Accumulate a stream of `u16` deltas (read with the supplied
/// endianness) into absolute signed `i16` PCM. Mirrors schism's
/// 16-bit PCMD branch in `csf_read_sample` (`csndfile.c:954`).
/// Caller is responsible for slicing one channel's bytes when
/// decoding split-stereo.
fn decode_delta_16(bytes: &[u8], frame_count: usize, big_endian: bool) -> Vec<i16> {
    let mut out: Vec<i16> = Vec::with_capacity(frame_count);
    let mut acc: u16 = 0;
    for i in 0..frame_count {
        let raw = if big_endian {
            u16::from_be_bytes([bytes[i * 2], bytes[i * 2 + 1]])
        } else {
            u16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]])
        };
        acc = acc.wrapping_add(raw);
        out.push(acc as i16);
    }
    out
}

impl ItSampleHeader {
    pub fn read(r: &mut BinReader) -> Result<Self, ImportError> {
        let id: [u8; 4] = r.read_array()?;
        let dos_filename_bytes: [u8; 12] = r.read_array()?;
        let reserved_byte = r.read_u8()?;
        let global_volume = r.read_u8()?;
        let flags = r.read_u8()?;
        let default_volume = r.read_u8()?;
        let sample_name_bytes: [u8; 26] = r.read_array()?;
        let convert_flags = r.read_u8()?;
        let default_pan = r.read_u8()?;
        let sample_length = r.read_u32_le()?;
        let loop_beginning = r.read_u32_le()?;
        let loop_end = r.read_u32_le()?;
        let c5_speed = r.read_u32_le()?;
        let sustain_loop_beginning = r.read_u32_le()?;
        let sustain_loop_end = r.read_u32_le()?;
        let sample_pointer = r.read_u32_le()?;
        let vibrato_speed = r.read_u8()?;
        let vibrato_depth = r.read_u8()?;
        let vibrato_sweep = r.read_u8()?;
        let vibrato_waveform = r.read_u8()?;
        Ok(Self {
            id,
            dos_filename: bytes_to_trimmed_string(&dos_filename_bytes),
            reserved_byte,
            global_volume,
            flags,
            default_volume,
            sample_name: bytes_to_trimmed_string(&sample_name_bytes),
            convert_flags,
            default_pan,
            sample_length,
            loop_beginning,
            loop_end,
            c5_speed,
            sustain_loop_beginning,
            sustain_loop_end,
            sample_pointer,
            vibrato_speed,
            vibrato_depth,
            vibrato_sweep,
            vibrato_waveform,
        })
    }

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
    ) -> Result<SampleDataType, ImportError> {
        let dst = if self.is_16bits() {
            if self.is_stereo() {
                let sample_len = 2 * self.sample_length as usize;
                let output = self.it_unpack_16bit(data, sample_len)?;
                SampleDataType::Stereo16(self.convert_16bit_sample(output.as_slice()).into())
            } else {
                let sample_len = self.sample_length as usize;
                let output = self.it_unpack_16bit(data, sample_len)?;
                SampleDataType::Mono16(self.convert_16bit_sample(output.as_slice()).into())
            }
        } else {
            if self.is_stereo() {
                let sample_len = 2 * self.sample_length as usize;
                let output = self.it_unpack_8bit(data, sample_len)?;
                SampleDataType::Stereo8(self.convert_8bit_sample(output.as_slice()).into())
            } else {
                let sample_len = self.sample_length as usize;
                let output = self.it_unpack_8bit(data, sample_len)?;
                SampleDataType::Mono8(self.convert_8bit_sample(output.as_slice()).into())
            }
        };

        Ok(dst)
    }

    fn get_sample_data_from_values(&self, data: &[u8]) -> Result<SampleDataType, ImportError> {
        let dst = if self.is_16bits() {
            if self.is_stereo() {
                let sample_len = 2 * 2 * self.sample_length as usize;
                let src = self.convert_u8_to_i16_vec(&data[0..sample_len])?;
                let stereo = self.convert_16bit_sample(src.as_slice());
                SampleDataType::Stereo16(stereo.into())
            } else {
                let sample_len = 2 * self.sample_length as usize;
                let src = self.convert_u8_to_i16_vec(&data[0..sample_len])?;
                SampleDataType::Mono16(self.convert_16bit_sample(src.as_slice()).into())
            }
        } else {
            if self.is_stereo() {
                let sample_len = 2 * self.sample_length as usize;
                let src: Vec<i8> = data
                    .iter()
                    .take(sample_len)
                    .map(|&byte| byte as i8)
                    .collect();
                SampleDataType::Stereo8(self.convert_8bit_sample(src.as_slice()).into())
            } else {
                let sample_len = self.sample_length as usize;
                let src: Vec<i8> = data
                    .iter()
                    .take(sample_len)
                    .map(|&byte| byte as i8)
                    .collect();
                SampleDataType::Mono8(self.convert_8bit_sample(src.as_slice()).into())
            }
        };

        Ok(dst)
    }

    pub fn get_sample_data(&self, data: &[u8]) -> Result<SampleDataType, ImportError> {
        // Note on bits 3 (PTM byte-delta) and 4 (12-bit TX-Wave)
        // of `convert_flags`: schism's IT loader (`fmt/its.c:254`)
        // does not consult either bit — it dispatches solely on
        // bits 0 (signed) and 2 (delta), so a real IT file with
        // bits 3 / 4 set is silently decoded as ordinary PCM by
        // schism. We do better for bit 3 (we have a PTM decoder
        // below) and we deliberately ignore bit 4: no public
        // reference implementation has a 12-bit-TX-Wave decoder,
        // and matching schism's behaviour means files with the
        // bit set still load, just as raw PCM the way every
        // mainstream player would have rendered them anyway.

        if self.is_compressed() {
            // In compressed mode bit 2 of `convert_flags` selects
            // IT215-vs-IT214 — handled inside the compressed
            // decoder. Single-delta / byte-delta flags do not
            // apply here.
            return self.get_sample_data_from_compressed_values(data);
        }

        if self.is_byte_delta_encoded() {
            // PTM byte-delta. Schism only supports the 16-bit-
            // mono form (`csndfile.c:1281`, `SF(16,M,LE,PTM)`);
            // no real-world file uses other widths.
            return self.get_sample_data_from_byte_delta(data);
        }

        if self.is_double_delta_encoded() {
            // In **uncompressed** mode bit 2 means PCMD —
            // each value is a delta from the previous, accumulate
            // with wrapping arithmetic to recover absolute PCM.
            // The method name reads "double" because the bit
            // means double-delta in compressed mode (handled
            // above); here, with `is_compressed()` already false,
            // the same bit means simple single-delta.
            // Source: schism `fmt/its.c:254` selects SF_PCMD on
            // `cvt & 4`, decoder at `csndfile.c:852` (8-bit) /
            // `csndfile.c:954` (16-bit).
            return self.get_sample_data_from_delta(data);
        }

        self.get_sample_data_from_values(data)
    }

    /// PTM byte-delta decoder (IT `convert_flags` bit 3, schism
    /// `SF(16,M,LE,PTM)`). Reads `sample_length * 2` signed-byte
    /// deltas, accumulates them with byte-wrapping arithmetic
    /// into a transient byte stream, then reinterprets that
    /// stream as little-endian `i16` frames.
    fn get_sample_data_from_byte_delta(&self, data: &[u8]) -> Result<SampleDataType, ImportError> {
        if !self.is_16bits() || self.is_stereo() {
            return Err(ImportError::Other(
                "IT byte-delta-encoded samples are only defined for 16-bit mono",
            ));
        }
        let frame_count = self.sample_length as usize;
        let bytes_needed = frame_count * 2;
        if data.len() < bytes_needed {
            return Err(ImportError::Other("IT byte-delta sample truncated"));
        }

        // Stage 1: accumulate signed-byte deltas.
        let mut bytes: Vec<u8> = Vec::with_capacity(bytes_needed);
        let mut acc: u8 = 0;
        for &b in data.iter().take(bytes_needed) {
            acc = acc.wrapping_add(b);
            bytes.push(acc);
        }
        // Stage 2: reinterpret pairs as little-endian i16.
        let mut out: Vec<i16> = Vec::with_capacity(frame_count);
        for pair in bytes.chunks_exact(2) {
            out.push(i16::from_le_bytes([pair[0], pair[1]]));
        }
        Ok(SampleDataType::Mono16(out.into()))
    }

    /// PCMD single-delta decoder (IT `convert_flags` bit 2 in
    /// uncompressed mode). Each value is a delta from the
    /// previous; accumulating with wrapping arithmetic recovers
    /// the absolute signed PCM regardless of how the original
    /// stream was nominally signed/unsigned. Source: schism
    /// `csndfile.c:852` (8-bit, mono / split-stereo) and
    /// `csndfile.c:954` (16-bit, mono / split-stereo, with
    /// optional big-endian byte order). IT samples only use the
    /// "split" stereo layout (left block then right block),
    /// matching schism's `SF_SS` path.
    fn get_sample_data_from_delta(&self, data: &[u8]) -> Result<SampleDataType, ImportError> {
        let frame_count = self.sample_length as usize;
        let be = self.is_samples_be();

        if self.is_16bits() {
            let bytes_per_chan = frame_count * 2;
            let total = if self.is_stereo() {
                bytes_per_chan * 2
            } else {
                bytes_per_chan
            };
            if data.len() < total {
                return Err(ImportError::Other(
                    "IT delta-encoded 16-bit sample truncated",
                ));
            }

            if self.is_stereo() {
                let l = decode_delta_16(&data[..bytes_per_chan], frame_count, be);
                let r = decode_delta_16(&data[bytes_per_chan..total], frame_count, be);
                let mut out: Vec<i16> = Vec::with_capacity(frame_count * 2);
                for i in 0..frame_count {
                    out.push(l[i]);
                    out.push(r[i]);
                }
                Ok(SampleDataType::Stereo16(out.into()))
            } else {
                Ok(SampleDataType::Mono16(
                    decode_delta_16(&data[..bytes_per_chan], frame_count, be).into(),
                ))
            }
        } else {
            let total = if self.is_stereo() {
                frame_count * 2
            } else {
                frame_count
            };
            if data.len() < total {
                return Err(ImportError::Other(
                    "IT delta-encoded 8-bit sample truncated",
                ));
            }

            if self.is_stereo() {
                let l = decode_delta_8(&data[..frame_count]);
                let r = decode_delta_8(&data[frame_count..total]);
                let mut out: Vec<i8> = Vec::with_capacity(frame_count * 2);
                for i in 0..frame_count {
                    out.push(l[i]);
                    out.push(r[i]);
                }
                Ok(SampleDataType::Stereo8(out.into()))
            } else {
                Ok(SampleDataType::Mono8(
                    decode_delta_8(&data[..frame_count]).into(),
                ))
            }
        }
    }

    fn convert_u8_to_i16_vec(&self, input: &[u8]) -> Result<Vec<i16>, ImportError> {
        if !input.len().is_multiple_of(2) {
            return Err(ImportError::Other("input is odd!"));
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

    fn it_unpack_8bit(&self, input: &[u8], output_len: usize) -> Result<Vec<i8>, ImportError> {
        let mut output = Vec::new();
        let mut p_src = input;
        while output.len() < output_len {
            if p_src.len() < 2 {
                return Err(ImportError::Other("IT sample decoder bit budget exceeded"));
            }

            let block_len = u16::from_le_bytes([p_src[0], p_src[1]]) as usize;
            p_src = &p_src[2..];
            let mut block_output_len = 0;
            let mut left: u8 = 9;
            let mut temp: u8 = 0;
            let mut temp2: u8 = 0;

            if p_src.len() < block_len {
                return Err(ImportError::Other("IT sample decoder bit budget exceeded"));
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
                    .ok_or(ImportError::Other("IT sample decoder bit budget exceeded"))?
                    as u16;

                if (1..7).contains(&left) {
                    // Type A
                    if 1_u16 << (left - 1) == bits {
                        bits = bit_reader
                            .read_bits(3)
                            .ok_or(ImportError::Other("IT sample decoder bit budget exceeded"))?
                            as u16;
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

    fn it_unpack_16bit(&self, input: &[u8], output_len: usize) -> Result<Vec<i16>, ImportError> {
        let mut output = Vec::new();
        let mut p_src = input;
        while output.len() < output_len {
            if p_src.len() < 2 {
                return Err(ImportError::Other("IT sample decoder bit budget exceeded"));
            }

            let block_len = u16::from_le_bytes([p_src[0], p_src[1]]) as usize;
            p_src = &p_src[2..];
            let mut block_output_len = 0;
            let mut left: u8 = 17;
            let mut temp: i16 = 0;
            let mut temp2: i16 = 0;

            if p_src.len() < block_len {
                return Err(ImportError::Other("IT sample decoder bit budget exceeded"));
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
                    .ok_or(ImportError::Other("IT sample decoder bit budget exceeded"))?;

                if (1..7).contains(&left) {
                    // Type A
                    if 1_u32 << (left - 1) == bits {
                        bits = bit_reader
                            .read_bits(4)
                            .ok_or(ImportError::Other("IT sample decoder bit budget exceeded"))?;
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

    fn c5_speed_to_finetune(&self) -> (i8, Finetune) {
        if self.c5_speed == 0 {
            return (0, Finetune::ZERO);
        }
        // Pure-integer load-time math. The historical IT
        // implementation computed `12 × log2(c5_speed / 16726)`
        // in `f32`. That `16726` came from `8363 × 2` (the FT2
        // C-4 reference doubled to anchor C-5), but the
        // runtime's pitch tables are MIDI-aligned at
        // `C4_FREQ_HZ = 8372 Hz`, so the float import was
        // ~1.9 cents off the actual playback frequency anyway.
        //
        // We anchor the conversion to the same tables the runtime
        // uses: `linear_frequency_to_period` (binary search,
        // ≤13 iter, no float, no `log2`) gives the pitch grid
        // position; `linear_period_to_pitch` then yields a
        // Q8.8 semitone count from C0. Subtracting C5 (= note
        // 60 = `60 × 256` in Q8.8) gives the relative pitch.
        // This makes the load-time `(relative_pitch, finetune)`
        // round-trip exactly through the playback path.
        let f = crate::core::fixed::units::Frequency::from_hz(self.c5_speed);
        let period = crate::core::fixed::tables::linear_frequency_to_period(f);
        // `period ∈ [0, 7680]`, with `period = 0` ↔ B9
        // (note 119) and `period = 7680` ↔ C0 (note 0).
        // `pitch_q8_8_from_c0 = (7680 - period) × 4` (1 unit
        // of period = 1/64 semitone; 256/64 = 4 to widen to
        // Q8.8).
        let pitch_q8_8_from_c0 = (7680 - period.raw() as i32) * 4;
        // C5 = 60 semitones above C0 = `60 × 256 = 15360` in Q8.8.
        let semitones_q8_8 = pitch_q8_8_from_c0 - 15360;

        // Round-half-away-from-zero: `relative_pitch =
        // round(semitones / 256)`.
        let bias = if semitones_q8_8 >= 0 { 128 } else { -128 };
        let rel_i32 = (semitones_q8_8 + bias) >> 8;
        let relative_pitch = rel_i32.clamp(i8::MIN as i32, i8::MAX as i32) as i8;

        // Residual fractional semitone, Q8.8 in `[-128, +127]`
        // (max amplitude is one half-step). Convert Q8.8 → Q1.15
        // by `<< 7` (256→32768 shift = 7 bits), result in
        // `[-16384, +16256]` ⊂ `(-1, +1)` Q1.15.
        let frac_q8_8 = semitones_q8_8 - (relative_pitch as i32) * 256;
        let frac_q15 = frac_q8_8 << 7;
        let finetune = Finetune::from_q15_i32_sat(frac_q15);
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
            Panning::from_byte_64(self.default_pan & 0x7F)
        } else {
            Panning::CENTER
        };

        let llength = self.loop_end.saturating_sub(self.loop_beginning);

        let sllength = self
            .sustain_loop_end
            .saturating_sub(self.sustain_loop_beginning);

        // IT splits the sample's level into two registers:
        //   * GvL (`global_volume`, 0..64): always applies — a static
        //     scale baked into the sample's playback level. Maps to
        //     `Sample.volume` (a `ChannelVolume` semantically).
        //   * Vol (`default_volume`, 0..64): the default NOTE volume,
        //     used only when the pattern cell has no volume-column
        //     override. Maps to `Sample.default_note_volume`.
        // A pattern cell's `Vxx` column overrides the note's starting
        // volume but does NOT clobber GvL — GvL keeps scaling the
        // sample regardless.
        let gvl = ChannelVolume::from_byte_64(self.global_volume);
        let default_note_vol = Volume::from_byte_64(self.default_volume.min(64));

        Sample {
            name,
            relative_pitch,
            finetune,
            volume: gvl,
            default_note_volume: default_note_vol,
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
        // IT auto-vibrato shapes (`IT_MUSIC.ASM` / Schism
        // `rn_sample_vibrato`): 0 = sine (`+sin`, rises first),
        // 1 = ramp down (`ramp_down_table`: linear `+1 → −1`, peak at
        // phase 0, discontinuity at the cycle boundary), 2 = square
        // (rises first), 3 = random (bipolar). The `AutoVib*` shapes
        // are bipolar — IT applies them through linear frequency
        // slides on both sides of the base pitch.
        let wf = match self.vibrato_waveform {
            1 => Waveform::AutoVibRampDown,
            2 => Waveform::AutoVibSquare,
            3 => Waveform::Random,
            _ => Waveform::AutoVibSine,
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
        // (`Q8_8::from_ratio` returns saturating-positive on
        // `den == 0`, so we map that to `Q8_8::ZERO` explicitly to
        // get the "no ramp" path in `StateAutoVibrato::tick`, which
        // short-circuits on `sweep <= 0`).
        let xmrs_sweep = if self.vibrato_sweep == 0 {
            Q8_8::ZERO
        } else {
            Q8_8::from_ratio(self.vibrato_speed as i16, self.vibrato_sweep as i16)
        };

        Vibrato {
            waveform: wf,
            speed: Q8_8::from_ratio(self.vibrato_speed as i16, 64),
            depth: PitchDelta::from_ratio(self.vibrato_depth as i16, 64),
            sweep: xmrs_sweep,
        }
    }
}
