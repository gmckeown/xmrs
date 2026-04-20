use crate::import::import_memory::{ImportMemory, MemoryType};
use crate::import::orders_helper;
use crate::import::patternslot::PatternSlot;
use crate::prelude::*;

use bincode::error::DecodeError;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use super::it_edit_history::{ItEditHistory, ItEditHistoryEntry};
use super::it_header::ItHeader;
use super::it_instrument::ItInstrument;
use super::it_midi_macros::ItMidiMacros;
use super::it_pattern::ItPattern;
use super::it_plugins::Plugins;
use super::it_sample_header::ItSampleHeader;
use super::it_x_names::ItXNames;

#[derive(Debug)]
#[repr(C)]
pub struct ItModule {
    header: ItHeader,
    orders: Vec<u8>,
    edit_history: Option<Vec<ItEditHistoryEntry>>,
    midi_macros: Option<ItMidiMacros>,
    pattern_names: Vec<String>,
    channel_names: Vec<String>,
    plugins: Option<Plugins>,
    message: String,
    instruments: Vec<ItInstrument>,
    samples_header: Vec<ItSampleHeader>,
    patterns: Vec<Vec<Vec<PatternSlot>>>,
    samples: Vec<Option<SampleDataType>>,
}

impl ItModule {
    pub fn load(ser_it_module: &[u8]) -> Result<Self, DecodeError> {
        let data = ser_it_module;

        // === ItHeader =====================================================

        if data.len() < ItHeader::get_size() {
            return Err(DecodeError::ArrayLengthMismatch {
                required: ItHeader::get_size(),
                found: data.len(),
            });
        }

        let header = ItHeader::load(data)?;
        let data = &data[header.1..];

        // === Orders =======================================================

        if data.len() < header.0.order_number as usize {
            return Err(DecodeError::ArrayLengthMismatch {
                required: header.0.order_number as usize,
                found: data.len(),
            });
        }

        let orders = &data[..header.0.order_number as usize];
        let data = &data[header.0.order_number as usize..];

        // === Instruments Offsets ==========================================

        if data.len() < 4 * header.0.instrument_number as usize {
            return Err(DecodeError::ArrayLengthMismatch {
                required: 4 * header.0.instrument_number as usize,
                found: data.len(),
            });
        }

        let instrument_offsets_u8 = &data[..4 * header.0.instrument_number as usize];
        let instrument_offsets: Vec<u32> = instrument_offsets_u8
            .chunks_exact(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect();
        let data = &data[4 * header.0.instrument_number as usize..];

        // === Samples Header Offsets =======================================

        if data.len() < 4 * header.0.sample_number as usize {
            return Err(DecodeError::ArrayLengthMismatch {
                required: 4 * header.0.sample_number as usize,
                found: data.len(),
            });
        }

        let sample_header_offsets_u8 = &data[..4 * header.0.sample_number as usize];
        let sample_header_offsets: Vec<u32> = sample_header_offsets_u8
            .chunks_exact(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect();
        let data = &data[4 * header.0.sample_number as usize..];

        // === Patterns Offsets ==========================================

        if data.len() < 4 * header.0.pattern_number as usize {
            return Err(DecodeError::ArrayLengthMismatch {
                required: 4 * header.0.pattern_number as usize,
                found: data.len(),
            });
        }

        let pattern_offsets_u8 = &data[..4 * header.0.pattern_number as usize];
        let pattern_offsets: Vec<u32> = pattern_offsets_u8
            .chunks_exact(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect();
        let mut data = &data[4 * header.0.pattern_number as usize..];

        // === Edit History =================================================

        let edit_history = if header.0.is_edit_history_embedded() {
            let (edit_history, l) = ItEditHistory::load(data)?;
            data = &data[l..];
            edit_history
        } else {
            None
        };

        // === Midi Macros ==================================================

        let midi_macros = if header.0.is_embedded_midi_macro() || header.0.is_embedded_midi_macros()
        {
            let r = bincode::serde::decode_from_slice::<ItMidiMacros, _>(
                data,
                bincode::config::legacy(),
            )?;
            data = &data[r.1..];
            Some(r.0)
        } else {
            None
        };

        // === Pattern names ================================================

        let pattern_names = if ItXNames::is_pnam(data) {
            data = &data[4..];
            let (pattern_names, l) = ItXNames::load(data, 32)?;
            data = &data[l..];
            pattern_names
        } else {
            vec![]
        };

        // === Channel names ================================================

        let channel_names = if ItXNames::is_cnam(data) {
            data = &data[4..];
            let (channel_names, l) = ItXNames::load(data, 20)?;
            data = &data[l..];
            channel_names
        } else {
            vec![]
        };

        // === Mix Plugins ==================================================

        let (plugins, _l) = if let Ok(p) = Plugins::load(data) {
            (Some(p.0), p.1)
        } else {
            (None, 0)
        };
        // data = &data[l..];

        // === Message ======================================================

        let message = if header.0.is_song_message_attached() && header.0.message_length != 0 {
            let start = header.0.message_offset as usize;
            let end = start + header.0.message_length as usize;
            if ser_it_module[start..].len() < header.0.message_length as usize {
                return Err(DecodeError::ArrayLengthMismatch {
                    required: header.0.message_length as usize,
                    found: ser_it_module[start..].len(),
                });
            }
            let src = &ser_it_module[start..end];
            String::from_utf8_lossy(src).trim().to_string()
        } else {
            String::new()
        };

        // === Instruments ==================================================

        let mut instruments: Vec<ItInstrument> = vec![];
        for i in 0..header.0.instrument_number {
            let i_seek = instrument_offsets[i as usize] as usize;

            if ser_it_module.len() < i_seek {
                return Err(DecodeError::LimitExceeded);
            }

            let data = &ser_it_module[i_seek..];
            if !header.0.is_post20() {
                instruments.push(ItInstrument::load_pre2(data)?);
            } else {
                instruments.push(ItInstrument::load_post2(data)?);
            }
        }

        // === Samples Header ===============================================

        let mut samples_header: Vec<ItSampleHeader> = vec![];
        for i in 0..header.0.sample_number {
            let i_seek = sample_header_offsets[i as usize] as usize;

            if ser_it_module.len() < i_seek {
                return Err(DecodeError::LimitExceeded);
            }

            let data = &ser_it_module[i_seek..];
            let sample_h = bincode::serde::decode_from_slice::<ItSampleHeader, _>(
                data,
                bincode::config::legacy(),
            )?;
            samples_header.push(sample_h.0);
        }

        // === Patterns =====================================================

        let mut patterns: Vec<Vec<Vec<PatternSlot>>> = vec![];
        for pattern_seek in &pattern_offsets {
            if ser_it_module.len() < *pattern_seek as usize {
                return Err(DecodeError::LimitExceeded);
            }

            if *pattern_seek != 0 {
                let data = &ser_it_module[*pattern_seek as usize..];
                let itpattern = ItPattern::load(data)?;
                let pattern = itpattern.unpack()?;
                patterns.push(pattern);
            } else {
                patterns.push(vec![vec![]]);
            }
        }

        // === Samples ======================================================

        let mut samples = vec![];
        for sh in &samples_header {
            if sh.is_associated_sample() {
                let start = sh.sample_pointer as usize;

                if ser_it_module.len() < start {
                    return Err(DecodeError::LimitExceeded);
                }

                let sample = sh.get_sample_data(&ser_it_module[start..])?;
                if !sample.is_empty() {
                    samples.push(Some(sample));
                } else {
                    samples.push(None);
                }
            } else {
                samples.push(None);
            }
        }

        // === All in ItModule ==============================================

        let it = Self {
            header: header.0,
            orders: orders.to_vec(),
            edit_history,
            midi_macros,
            pattern_names,
            channel_names,
            plugins,
            message,
            instruments,
            samples_header,
            patterns,
            samples,
        };

        Ok(it)
    }

    pub fn to_module(&self) -> Module {
        let it_type = if self.header.is_ompt() { "MP" } else { "IT" };
        let mut module = Module {
            name: self.header.song_name.clone(),
            comment: format!(
                "{}: {}.{} (compatibility: {}.{})\n\n{}",
                it_type,
                self.header.created_with_tracker >> 8,
                self.header.created_with_tracker & 0x00FF,
                self.header.compatible_with_tracker >> 8,
                self.header.compatible_with_tracker & 0x00FF,
                self.message
            ),
            frequency_type: if self.header.is_linear_slides() {
                FrequencyType::LinearFrequencies
            } else {
                FrequencyType::AmigaFrequencies
            },
            restart_position: 0,
            default_tempo: self.header.initial_speed as usize,
            default_bpm: self.header.initial_bpm as usize,
            pattern_order: orders_helper::parse_orders(&self.orders),
            pattern: vec![],
            pattern_names: self.pattern_names.clone(),
            channel_names: self.channel_names.clone(),
            instrument: vec![],
        };

        let mut im = ImportMemory::default();
        module.pattern = im.unpack_patterns(
            module.frequency_type,
            MemoryType::It,
            &module.pattern_order,
            &self.patterns,
        );

        // Prepare Samples
        let mut samples: Vec<Sample> = vec![];
        let mut vibratos: Vec<Vibrato> = vec![];
        for (i, sh) in self.samples_header.iter().enumerate() {
            let pdata = if sh.is_associated_sample() && i < self.samples.len() {
                &self.samples[i]
            } else {
                &None
            };

            let s = sh.to_sample(pdata);
            samples.push(s);

            let vibrato = sh.to_vibrato();
            vibratos.push(vibrato);
        }

        if self.header.is_instruments_used() {
            // Prepare Instrument then add samples
            for i in self.instruments.iter() {
                let mut instrument = i.prepare_instrument();
                if let InstrumentType::Default(instrdef) = &mut instrument.instr_type {
                    // Find first referenced sample and use its vibrato
                    let first_sample_idx = instrdef.sample_for_pitch.iter().find_map(|s| *s);
                    if let Some(si) = first_sample_idx {
                        if si < vibratos.len() {
                            instrdef.vibrato = vibratos[si];
                        }
                    }

                    for i2 in instrdef.sample_for_pitch {
                        if let Some(i3) = i2 {
                            // I must add i3 sample to instrdef
                            if instrdef.sample.len() <= i3 {
                                instrdef.sample.resize_with(i3 + 1, || None);
                            }
                            if instrdef.sample[i3].is_none() && samples.len() > i3 {
                                instrdef.sample[i3] = Some(samples[i3].clone());
                            }
                        }
                    }
                }
                module.instrument.push(instrument);
            }
        } else {
            // Easy addition of samples with default Instrument
            for (index, s) in samples.iter().enumerate() {
                let mut instrdef: InstrDefault = InstrDefault::default();
                instrdef.vibrato = vibratos[index];
                instrdef.sample.push(Some(s.clone()));
                instrdef.change_all_sample_for_pitch(0);
                let instrument = Instrument {
                    name: s.name.clone(),
                    muted: false,
                    instr_type: InstrumentType::Default(instrdef),
                };
                module.instrument.push(instrument);
            }
        }
        module
    }
}
