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

    /// Materialise the IT header's startup state (initial global
    /// volume, initial channel pan, initial channel volume) as
    /// pattern-row-0 effects on the first-played pattern. Mirrors
    /// the approach `S3mModule::inject_header_defaults` uses —
    /// keeping these as effect-column injections means the player
    /// needs no extra "initial state" API; the sequencer simply
    /// plays row 0 and those effects fire like any other.
    ///
    /// Only GV (the Vxx-animated global volume register) is
    /// injected. MV (mix volume) is a separate constant handled via
    /// `Module::mix_volume` on the final-mix path — combining the
    /// two into a single `Vxx` injection was wrong because a later
    /// `Vxx` effect in the pattern would overwrite both instead of
    /// just animating GV.
    fn inject_header_defaults(&self, patterns: &mut [Vec<Vec<PatternSlot>>]) {
        let first_pat = match self
            .orders
            .iter()
            .find(|&&p| p < 200) // skip end/skip markers
        {
            Some(&p) => p as usize,
            None => return,
        };
        let row = match patterns.get_mut(first_pat).and_then(|p| p.first_mut()) {
            Some(r) if !r.is_empty() => r,
            _ => return,
        };

        // --- Global volume as Vxx. ---
        // IT's GV is 0..128. The Vxx effect parameter is also 0..128
        // on IT (see `it_effect.rs` — `0x16` handler clamps to 128).
        // Only inject when GV is non-default (≠ 128) — if the module
        // plays at full GV there's nothing to bias.
        let gv = self.header.global_volume.min(128);
        if gv != 128 {
            if let Some(slot) = row
                .iter_mut()
                .find(|s| s.effect_type == 0 && s.effect_parameter == 0)
            {
                slot.effect_type = 0x16; // Vxx
                slot.effect_parameter = gv;
            }
        }

        // --- Initial channel pan (Xxx) per channel. ---
        // IT stores `initial_channel_pan[ch]` as 0..64 (left..right),
        // 100 = surround, +128 = disabled channel. Xxx takes a
        // 0..255 parameter (full 8-bit range, post-fix). Scale 0..64
        // → 0..252 (multiply by 4, round-friendly on 0..64 inputs
        // and still cleanly reaches both extremes). Only inject for
        // channels whose pan differs from the default center (32).
        //
        // `pan_separation` (header field, 0..128) scales each
        // channel's distance from centre: 128 = full separation
        // (identity), 0 = everything collapses to mono. Applied at
        // injection time so that static pans respect the author's
        // chosen stereo width. Dynamic pan effects (Pxx, panning
        // envelope) bypass this scaling — acceptable because most
        // IT files use pan_separation as a one-time mix-width knob.
        let pan_sep = self.header.pan_separation.min(128) as i32;
        for (ch, &pan_raw) in self.header.initial_channel_pan.iter().enumerate() {
            if ch >= row.len() {
                break;
            }
            // Disabled channel (+128) — skip entirely. Player-side
            // "muted" handling is a different axis and the module
            // loader doesn't touch it.
            if pan_raw & 0x80 != 0 {
                continue;
            }
            // Surround sentinel (100): the channel should start in
            // IT's pseudo-stereo "surround" mode (right channel
            // phase-inverted at mix time). The player implements
            // this as an S91 effect on the channel. We don't mess
            // with Xxx here — S91 is orthogonal to pan and the
            // channel will be re-panned by any later Xxx / Pxx.
            if pan_raw == 100 {
                let slot = &mut row[ch];
                if slot.effect_type == 0 && slot.effect_parameter == 0 {
                    slot.effect_type = 0x13; // Sxx (special)
                    slot.effect_parameter = 0x91; // S91 = surround on
                }
                continue;
            }
            let pan = (pan_raw & 0x7F) as i32;
            // Scale distance from centre by pan_separation / 128,
            // then re-centre. With pan_sep == 128 this is a no-op;
            // with pan_sep == 0 every channel collapses to 32
            // (centre) and we skip the injection entirely below.
            let pan = 32 + ((pan - 32) * pan_sep / 128);
            let pan = pan.clamp(0, 64) as u32;
            if pan == 32 {
                continue; // center default (or collapsed to centre)
            }
            let slot = &mut row[ch];
            if slot.effect_type == 0 && slot.effect_parameter == 0 {
                slot.effect_type = 0x18; // Xxx — Set Panning
                                         // 0..64 → 0..255 (x * 4 clamped; 64 maps to 255).
                let p = (pan * 255 / 64).min(255) as u8;
                slot.effect_parameter = p;
            }
        }

        // --- Initial channel volume (Mxx) per channel. ---
        // IT stores `initial_channel_volume[ch]` as 0..64; the Mxx
        // effect takes 0..64 directly.
        for (ch, &vol) in self.header.initial_channel_volume.iter().enumerate() {
            if ch >= row.len() {
                break;
            }
            let vol = vol.min(64);
            if vol == 64 {
                continue; // full-volume default
            }
            let slot = &mut row[ch];
            if slot.effect_type == 0 && slot.effect_parameter == 0 {
                slot.effect_type = 0x0D; // Mxx — Set Channel Volume
                slot.effect_parameter = vol;
            }
        }
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
            format: ModuleFormat::It,
            // IT quirks. ITTECH specifies:
            // - `tremor_state_persists`: Ixy counter is shared
            //   across rows (ST3 semantics — consecutive Ixy rows
            //   form a continuous on/off cycle rather than
            //   retriggering the phase per row).
            // The two header-driven flags ride alongside.
            quirks: PlaybackQuirks {
                tremor_state_persists: true,
                it_old_effects: self.header.is_old_effects(),
                it_link_gxx_memory: self.header.is_g_linked_with_e_f(),
                ..PlaybackQuirks::default()
            },
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
            // MIDI macros: propagate the IT-file table through to
            // the module so the replayer's macro interpreter can
            // reach it at `Zxx` time. `None` when the IT file's
            // header didn't flag macros as embedded.
            midi_macros: self.midi_macros.as_ref().map(|m| m.to_public()),
            // Mix volume: IT-specific constant (0..128 in the file,
            // normalised to 0..1 here). Applied as a final-mix
            // multiplier by the player, separate from the Vxx-
            // animated global volume.
            mix_volume: (self.header.mix_volume.min(128) as f32) / 128.0,
        };

        let mut im = ImportMemory::default();
        // IT-specific import-time flags. The link-Gxx-with-E/F flag
        // must be set BEFORE unpack so the memory-resolution path
        // sees it. See `ImportMemory::it_flags`.
        im.it_flags.link_gxx_ef_memory = self.header.is_g_linked_with_e_f();
        // Inject header defaults (global*mix volume, per-channel pan
        // and vol) into row 0 of the first-played pattern BEFORE
        // unpack, so they pass through the normal IT effect parser.
        let mut patterns_for_unpack = self.patterns.clone();
        self.inject_header_defaults(&mut patterns_for_unpack);
        module.pattern = im.unpack_patterns(
            module.frequency_type,
            MemoryType::It,
            &module.pattern_order,
            &patterns_for_unpack,
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

                    for i3 in instrdef.sample_for_pitch.into_iter().flatten() {
                        // I must add i3 sample to instrdef
                        if instrdef.sample.len() <= i3 {
                            instrdef.sample.resize_with(i3 + 1, || None);
                        }
                        if instrdef.sample[i3].is_none() && samples.len() > i3 {
                            instrdef.sample[i3] = Some(samples[i3].clone());
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
