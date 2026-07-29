//! XM-format sample header + PCM-payload decoder. XM stores PCM
//! as 8-bit or 16-bit signed delta-encoded values; this module
//! undoes the delta encoding and assembles the per-sample
//! metadata (loop window, volume, panning, finetune, relative
//! note number) into a [`crate::core::sample::Sample`].

use alloc::string::String;
use alloc::vec;

use super::helper::*;
use crate::core::fixed::units::{ChannelVolume, Finetune, Panning, Volume};
use crate::core::sample::{LoopType, Sample, SampleDataType};
use crate::tracker::import::bin_reader::{bytes_to_trimmed_string, BinReader, ImportError};

pub const XMSAMPLE_HEADER_SIZE: usize = 40;

/// On-disk XM sample header (40 bytes):
/// `length:u32 loop_start:u32 loop_length:u32 volume:u8 finetune:i8
///  flags:u8 panning:u8 relative_pitch:i8 reserved:u8 name:[u8;22]`.
#[derive(Default, Debug)]
pub struct XmSampleHeader {
    length: u32,
    loop_start: u32,
    loop_length: u32,
    volume: u8,
    finetune: i8,
    flags: u8,
    panning: u8,
    relative_pitch: i8,
    reserved: u8,
    name: String,
}

impl XmSampleHeader {
    fn read(r: &mut BinReader) -> Result<Self, ImportError> {
        let length = r.read_u32_le()?;
        let loop_start = r.read_u32_le()?;
        let loop_length = r.read_u32_le()?;
        let volume = r.read_u8()?;
        let finetune = r.read_i8()?;
        let flags = r.read_u8()?;
        let panning = r.read_u8()?;
        let relative_pitch = r.read_i8()?;
        let reserved = r.read_u8()?;
        let name_bytes: [u8; 22] = r.read_array()?;
        Ok(Self {
            length,
            loop_start,
            loop_length,
            volume,
            finetune,
            flags,
            panning,
            relative_pitch,
            reserved,
            name: bytes_to_trimmed_string(&name_bytes),
        })
    }
}

#[derive(Debug, Default)]
pub struct XmSample {
    header: XmSampleHeader,
    data: Option<SampleDataType>,
}

impl XmSample {
    pub fn load(data: &[u8]) -> Result<(&[u8], XmSample), ImportError> {
        let mut r = BinReader::new(data);
        let header = XmSampleHeader::read(&mut r)?;
        let xms = XmSample { header, data: None };
        Ok((r.tail(), xms))
    }

    /// Replace the sample name (used by the codepage post-load
    /// pass in `XmModule::load`: the initial parse decodes with
    /// `from_utf8_lossy`, then this re-applies the file-wide
    /// detected codepage).
    pub fn set_name(&mut self, name: String) {
        self.header.name = name;
    }

    pub fn add_sample<'a>(&mut self, data: &'a [u8]) -> Result<&'a [u8], ImportError> {
        let data_len: usize = self.header.length as usize;
        if data.len() < data_len {
            return Err(ImportError::Other("Sample data too short"));
        }
        let slice = &data[..data_len];

        let d3 = if self.header.flags & 0b0001_0000 != 0 {
            // 16 bits data
            let sample = u8_slice_to_vec_u16(slice);
            let sample2 = delta16_to_sample(sample);
            SampleDataType::Mono16(sample2.into())
        } else {
            // 8 bits data
            let sample = slice.to_vec();
            let sample2 = delta8_to_sample(sample);
            SampleDataType::Mono8(sample2.into())
        };
        self.data = Some(d3);

        Ok(&data[data_len..])
    }

    pub fn to_sample(&self) -> Sample {
        let mut loop_start = self.header.loop_start;
        let mut loop_length = self.header.loop_length;

        if let Some(SampleDataType::Mono16(_)) = &self.data {
            loop_start >>= 1;
            loop_length >>= 1;
        }

        /* Fix invalid loop definitions */
        let sample_length = self.len();
        if sample_length == 0 {
            loop_start = 0;
            loop_length = 0;
        } else {
            if loop_start >= sample_length {
                loop_start = sample_length - 1;
            }
            if loop_length > sample_length - loop_start {
                loop_length = sample_length - loop_start;
            }
        }

        let data: SampleDataType = match &self.data {
            Some(d) => d.clone(),
            None => SampleDataType::Mono8(vec![].into()),
        };

        Sample {
            name: self.header.name.clone(),
            relative_pitch: self.header.relative_pitch,
            // XM finetune is `i8 / 127`; the previous f32
            // `.clamp(-1.0, 1.0)` is folded into Finetune's
            // saturating storage.
            finetune: Finetune::from_ratio(self.header.finetune as i32, 127),
            volume: ChannelVolume::from_byte_64(self.header.volume),
            // XM has no separate per-sample default note volume —
            // the sample's own `volume` scale is the only gain knob.
            default_note_volume: Volume::FULL,
            panning: Panning::from_byte_255(self.header.panning),
            loop_flag: match self.header.flags & 0b0000_0011 {
                1 => LoopType::Forward,
                2 => LoopType::PingPong,
                3 => LoopType::PingPong,
                _ => LoopType::No,
            },
            loop_start,
            loop_length,
            sustain_loop_flag: LoopType::No,
            sustain_loop_start: 0,
            sustain_loop_length: 0,
            data: Some(data),
        }
    }

    pub fn len(&self) -> u32 {
        match &self.data {
            Some(SampleDataType::Mono8(d)) => d.len() as u32,
            Some(SampleDataType::Mono16(d)) => d.len() as u32,
            _ => 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Sample name as stored, for the pre-DAW
    /// [`RawModule`](crate::tracker::import::raw::RawModule) view.
    pub fn name(&self) -> &str {
        &self.header.name
    }

    /// The decoded PCM, or `None` for a header with no body.
    pub fn data(&self) -> Option<&SampleDataType> {
        self.data.as_ref()
    }

    /// Loop start **in frames**. XM stores it in bytes, so a 16-bit
    /// sample halves it — the same shift `to_sample()` applies,
    /// without that method's out-of-range clamping.
    pub fn loop_start_frames(&self) -> u32 {
        match &self.data {
            Some(SampleDataType::Mono16(_)) => self.header.loop_start >> 1,
            _ => self.header.loop_start,
        }
    }

    /// Loop length in frames. Same conversion as
    /// [`Self::loop_start_frames`].
    pub fn loop_length_frames(&self) -> u32 {
        match &self.data {
            Some(SampleDataType::Mono16(_)) => self.header.loop_length >> 1,
            _ => self.header.loop_length,
        }
    }
}
