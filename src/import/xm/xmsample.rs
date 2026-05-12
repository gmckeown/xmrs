/// Original XM Sample
use bincode::error::{DecodeError, EncodeError};
use serde::{Deserialize, Serialize};

use alloc::string::String;
use alloc::{vec, vec::Vec};

use super::helper::*;
use super::serde_helper::{deserialize_string_22, serialize_string_22};
use crate::fixed::units::{ChannelVolume, Finetune, Panning, Volume};
use crate::instrument::{Instrument, InstrumentType};
use crate::sample::{LoopType, Sample, SampleDataType};

pub const XMSAMPLE_HEADER_SIZE: usize = 40;

#[derive(Default, Serialize, Deserialize, Debug)]
#[repr(C)]
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
    #[serde(
        deserialize_with = "deserialize_string_22",
        serialize_with = "serialize_string_22"
    )]
    name: String,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct XmSample {
    header: XmSampleHeader,
    data: Option<SampleDataType>,
}

impl XmSample {
    pub fn load(data: &[u8]) -> Result<(&[u8], XmSample), DecodeError> {
        let sh = bincode::serde::decode_from_slice::<XmSampleHeader, _>(
            data,
            bincode::config::legacy(),
        )?
        .0;
        // Now create XmSample
        let xms = XmSample {
            header: sh,
            data: None,
        };
        Ok((&data[XMSAMPLE_HEADER_SIZE..], xms))
    }

    /// Replace the sample name (used by the codepage post-load
    /// pass in `XmModule::load`; bincode produces the field as
    /// a `from_utf8_lossy` string, which we re-decode under the
    /// file-wide detected codepage).
    pub fn set_name(&mut self, name: String) {
        self.header.name = name;
    }

    pub fn add_sample<'a>(&mut self, data: &'a [u8]) -> Result<&'a [u8], DecodeError> {
        let data_len: usize = self.header.length as usize;
        if data.len() < data_len {
            return Err(DecodeError::Other("Sample data too short"));
        }
        let slice = &data[..data_len];

        let d3 = if self.header.flags & 0b0001_0000 != 0 {
            // 16 bits data
            let sample = u8_slice_to_vec_u16(slice);
            let sample2 = delta16_to_sample(sample);
            SampleDataType::Mono16(sample2)
        } else {
            // 8 bits data
            let sample = slice.to_vec();
            let sample2 = delta8_to_sample(sample);
            SampleDataType::Mono8(sample2)
        };
        self.data = Some(d3);

        Ok(&data[data_len..])
    }

    pub fn save(&mut self) -> Result<Vec<u8>, EncodeError> {
        self.header.length = match &self.data {
            Some(SampleDataType::Mono8(d)) => d.len() as u32,
            Some(SampleDataType::Mono16(d)) => {
                self.header.flags |= 0b0001_0000;
                2 * d.len() as u32
            }
            _ => 0,
        };
        let h = bincode::serde::encode_to_vec(&self.header, bincode::config::legacy())?;
        Ok(h)
    }

    /// You must call save() before to save good length size to header
    pub fn save_sample(&mut self) -> Result<Vec<u8>, EncodeError> {
        let d = match &self.data {
            Some(SampleDataType::Mono8(d)) => sample8_to_delta(d),
            Some(SampleDataType::Mono16(d)) => {
                let d = sample16_to_delta(d);
                vec_u16_to_u8_slice(d)
            }
            _ => vec![],
        };
        Ok(d)
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
            None => SampleDataType::Mono8(vec![]),
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

    pub fn from_instr(i: &Instrument) -> Vec<XmSample> {
        let mut output: Vec<XmSample> = vec![];
        if let InstrumentType::Default(id) = &i.instr_type {
            for s in id.sample.iter().flatten() {
                let mut loop_start = s.loop_start;
                let mut loop_length = s.loop_length;

                if let Some(SampleDataType::Mono16(_)) = &s.data {
                    loop_start <<= 1;
                    loop_length <<= 1;
                }

                let mut xms = XmSample::default();
                xms.header.length = match &s.data {
                    Some(SampleDataType::Mono8(d)) => d.len() as u32,
                    Some(SampleDataType::Mono16(d)) => 2 * d.len() as u32,
                    Some(SampleDataType::Stereo8(d)) => 2 * d.len() as u32,
                    Some(SampleDataType::Stereo16(d)) => 2 * 2 * d.len() as u32,
                    Some(SampleDataType::StereoFloat(d)) => 4 * 2 * d.len() as u32,
                    None => 0,
                };
                xms.header.loop_start = loop_start;
                xms.header.loop_length = loop_length;
                xms.header.volume = s.volume.to_byte_64();
                xms.header.finetune = s.finetune.to_signed_byte(127);
                xms.header.flags = s.loop_flag.into();
                if let Some(SampleDataType::Mono16(_)) = &s.data {
                    xms.header.flags |= 0b0001_0000;
                }
                xms.header.panning = s.panning.to_byte_255();
                xms.header.relative_pitch = s.relative_pitch;
                xms.header.name = s.name.clone();
                xms.data = s.data.clone();
                output.push(xms);
            }
        }
        output
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
}
