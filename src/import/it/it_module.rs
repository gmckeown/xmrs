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
        //
        // Per-channel pan / channel volume / mute / surround used to
        // ride this same path (Xxx / Mxx / S91 on row 0), but they
        // now live on [`Module::channel_defaults`] — see
        // [`Self::header_channel_defaults`]. That's the cleaner
        // header-side data path that doesn't depend on row 0
        // having a free slot.
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
    }

    /// Build the per-channel default state from IT's header bytes.
    /// Returns one entry per channel slot in
    /// `initial_channel_pan` / `initial_channel_volume`. Matches
    /// what schism's `fmt/it.c` initialises on its `voices` array
    /// at song load.
    ///
    /// The pan byte encodes four states:
    /// - bit 0x80 set → channel disabled (muted)
    /// - byte == 100 → IT surround sentinel (S91 mode)
    /// - 0..64 → regular pan (0 = full left, 64 = full right)
    /// - other values → ignored (default to centre)
    ///
    /// The author's `pan_separation` (header field, 0..128) scales
    /// each channel's distance from centre at import time:
    /// 128 = full separation (identity), 0 = everything collapses
    /// to mono. Dynamic pan effects (Pxx, panning envelope) bypass
    /// this scaling at runtime — acceptable because most IT files
    /// use `pan_separation` as a one-time mix-width knob.
    fn header_channel_defaults(&self) -> Vec<ChannelDefault> {
        let pan_sep = self.header.pan_separation.min(128) as i32;
        self.header
            .initial_channel_pan
            .iter()
            .zip(self.header.initial_channel_volume.iter())
            .map(|(&pan_raw, &vol_raw)| {
                let mut entry = ChannelDefault::default();

                // --- Volume ---
                // IT stores 0..64; we normalise to 0..1. Full volume
                // (64) is the default, so leave `None` to keep the
                // player at its 1.0 default.
                let vol = vol_raw.min(64);
                if vol != 64 {
                    entry.volume = Some(vol as f32 / 64.0);
                }

                // --- Pan / mute / surround ---
                if pan_raw & 0x80 != 0 {
                    // Disabled channel.
                    entry.muted = true;
                } else if pan_raw == 100 {
                    // Surround sentinel.
                    entry.surround = true;
                } else {
                    let pan = (pan_raw & 0x7F) as i32;
                    // Apply pan_separation: scale distance from
                    // centre 32 by pan_sep/128, then re-centre.
                    let pan = 32 + ((pan - 32) * pan_sep / 128);
                    let pan = pan.clamp(0, 64) as u32;
                    if pan != 32 {
                        // Map 0..64 → 0..1.
                        entry.panning = Some(pan as f32 / 64.0);
                    }
                }

                entry
            })
            .collect()
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
            // IT quirks. Start from the IT 2.14 baseline (tremor
            // state persists across rows — ST3 semantics) and
            // overlay the two header-driven flags on top.
            profile: {
                let mut p = CompatibilityProfile::it214();
                p.quirks.it_old_effects = self.header.is_old_effects();
                p.quirks.it_link_gxx_memory = self.header.is_g_linked_with_e_f();
                p
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
            // Per-channel pan / volume / mute / surround defaults
            // built from the IT header. Replaces the previous
            // approach of injecting Xxx/Mxx/S91 effects in row 0
            // of the first-played pattern, which was fragile when
            // row 0 already carried explicit effects.
            channel_defaults: self.header_channel_defaults(),
            instrument: vec![],
            // MIDI macros: propagate the IT-file table through to
            // the module so the replayer's macro interpreter can
            // reach it at `Zxx` time. `None` when the IT file's
            // header didn't flag macros as embedded.
            midi_macros: self.midi_macros.as_ref().map(|m| m.to_public()),
            // Pattern highlight period (rows per beat / measure)
            // — drawn from the IT header. Clamp the raw bytes to
            // ≥ 1 because a value of 0 would produce a divide-by-
            // zero in any editor that derives bar lines from these.
            // Falling back to the defaults (4/16) when zero was
            // written (some old IT files do this).
            pattern_highlight: {
                let beat = self.header.rows_per_beat;
                let measure = self.header.rows_per_measure;
                if beat == 0 || measure == 0 {
                    PatternHighlight::default()
                } else {
                    PatternHighlight { beat, measure }
                }
            },
            // Mix plugins: OpenMPT extension carrying VST / DSP
            // routing. Only attached when the file actually had
            // plugins parsed AND the table isn't degenerate (all-
            // zero) — keeps the public field at `None` for the
            // common case where the section was absent or empty.
            mix_plugins: self
                .plugins
                .as_ref()
                .filter(|p| !p.is_empty())
                .map(|p| p.to_public()),
            // Pitch-wheel depth (ITTECH:94). Header byte is u8 but
            // ITTECH defines the valid range as 0..=15; clamp here so
            // a malformed file can't propagate an out-of-spec value
            // to downstream MIDI-out code.
            pitch_wheel_depth: self.header.pitch_wheel_depth.min(15),
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
                    let first_sample_idx =
                        instrdef.keyboard.sample_for_pitch.iter().find_map(|s| *s);
                    if let Some(si) = first_sample_idx {
                        if si < vibratos.len() {
                            instrdef.voice.vibrato = vibratos[si];
                        }
                    }

                    for i3 in instrdef.keyboard.sample_for_pitch.into_iter().flatten() {
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
                instrdef.voice.vibrato = vibratos[index];
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
