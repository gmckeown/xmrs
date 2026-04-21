/// S3M Effect Parser
///
/// Converts S3M effects directly to TrackImportEffect/GlobalEffect,
/// without going through XM effect numbering.
use alloc::vec;
use alloc::vec::Vec;

use crate::import::patternslot::PatternSlot;
use crate::import::track_import_effect::TrackImportEffect;
use crate::import::track_import_unit::TrackImportUnit;
use crate::prelude::*;

/// S3M shared effect memory and direct effect parser
#[derive(Debug)]
pub struct S3mEffect {
    /// Last non-zero parameter for effects D/E/F/I/J/K/L/Q/R/S (shared memory per channel)
    alastnfo: [u8; 32],
    /// Last non-zero parameter for vibrato effects H/U (separate memory per channel)
    alastvibnfo: [u8; 32],
    /// Last instrument used with tone portamento (G)
    s3m_last_g_instrument: [Option<usize>; 32],
}

impl S3mEffect {
    fn new() -> Self {
        Self {
            alastnfo: [0; 32],
            alastvibnfo: [0; 32],
            s3m_last_g_instrument: [None; 32],
        }
    }

    /// Resolve S3M shared effect memory for a pattern.
    /// In S3M, effects D/E/F/I/J/K/L/Q/R/S share a single "last info" memory per channel.
    /// H/U (vibrato) have their own shared memory.
    /// G (tone portamento) does NOT share memory with others.
    pub fn resolve_pattern_memory(pattern: &mut [Vec<PatternSlot>]) {
        let mut sfx = Self::new();
        sfx.resolve_memory(pattern);
    }

    fn resolve_memory(&mut self, pattern: &mut [Vec<PatternSlot>]) {
        for line in pattern {
            for (ch, slot) in line.iter_mut().enumerate() {
                let efx = slot.effect_type;
                let param = slot.effect_parameter;

                // Store non-zero parameters. ST3 has THREE memory pools per
                // channel, and they MUST NOT cross-pollute:
                //
                //   * alastnfo   — shared across D/E/F/I/J/K/L/Q/R/S
                //   * alastvibnfo — H and U (vibrato variants)
                //   * G (tone portamento) has its own memory, handled later
                //     in import_memory.rs via `TrackImportEffect::TonePortamento`
                //
                // The previous implementation used `(4..=12)` to write
                // alastnfo, which wrongly included G (7) and H (8). That
                // caused, for example, a G05 followed by F00 to "recall"
                // 0x05 — ST3 actually keeps those pools disjoint.
                if param > 0 {
                    if efx == 8 || efx == 21 {
                        self.alastvibnfo[ch] = param;
                    } else if matches!(efx, 4 | 5 | 6 | 9 | 10 | 11 | 12 | 17 | 18 | 19) {
                        self.alastnfo[ch] = param;
                    }
                }

                // Recall from memory when parameter is zero.
                // `efx != 7` excludes G from the recall path — G's memory
                // is handled by import_memory.rs.
                if param == 0 && efx != 7 {
                    if efx == 8 || efx == 21 {
                        slot.effect_parameter = self.alastvibnfo[ch];
                    } else if matches!(efx, 4 | 5 | 6 | 9 | 10 | 11 | 12 | 17 | 18 | 19) {
                        slot.effect_parameter = self.alastnfo[ch];
                    }
                }

                // Track last instrument for G effect
                if slot.instrument.is_some() && efx != 7 {
                    self.s3m_last_g_instrument[ch] = slot.instrument;
                }
            }
        }
    }

    // --- Volume slide helpers ---

    fn s3m_volume_slide(param: u8) -> Option<TrackImportEffect> {
        let high = (param & 0xF0) >> 4;
        let low = param & 0x0F;

        match (high, low) {
            (0xF, f) => Some(TrackImportEffect::VolumeSlide0(-(f as f32) / 64.0)),
            (f, 0xF) => Some(TrackImportEffect::VolumeSlide0(f as f32 / 64.0)),
            (0, f) => Some(TrackImportEffect::VolumeSlideN(-(f as f32) / 64.0)),
            (f, 0) => Some(TrackImportEffect::VolumeSlideN(f as f32 / 64.0)),
            // ST3: when both nibbles non-zero, last nibble has priority (slide down)
            (_, f) => Some(TrackImportEffect::VolumeSlideN(-(f as f32) / 64.0)),
        }
    }

    fn s3m_channel_volume_slide(param: u8) -> Option<TrackImportEffect> {
        let high = (param & 0xF0) >> 4;
        let low = param & 0x0F;

        match (high, low) {
            (0xF, f) => Some(TrackImportEffect::ChannelVolumeSlide0(-(f as f32) / 64.0)),
            (f, 0xF) => Some(TrackImportEffect::ChannelVolumeSlide0(f as f32 / 64.0)),
            (0, f) => Some(TrackImportEffect::ChannelVolumeSlideN(-(f as f32) / 64.0)),
            (f, 0) => Some(TrackImportEffect::ChannelVolumeSlideN(f as f32 / 64.0)),
            (_, f) => Some(TrackImportEffect::ChannelVolumeSlideN(-(f as f32) / 64.0)),
        }
    }

    fn s3m_panning_slide(param: u8) -> Option<TrackImportEffect> {
        let high = (param & 0xF0) >> 4;
        let low = param & 0x0F;

        match (high, low) {
            (0xF, f) => Some(TrackImportEffect::PanningSlide0(-(f as f32) / 16.0)),
            (f, 0xF) => Some(TrackImportEffect::PanningSlide0(f as f32 / 16.0)),
            (0, f) => Some(TrackImportEffect::PanningSlideN(-(f as f32) / 16.0)),
            (f, 0) => Some(TrackImportEffect::PanningSlideN(f as f32 / 16.0)),
            _ => None,
        }
    }

    // --- Effect parser (S3M numbering -> TrackImportEffect) ---

    fn s3m_walk_effect(
        freq_type: FrequencyType,
        current: &PatternSlot,
    ) -> Option<Vec<TrackImportEffect>> {
        let param = current.effect_parameter;
        match current.effect_type {
            // D - Volume slide
            4 => Self::s3m_volume_slide(param).map(|e| vec![e]),

            // E - Portamento down
            5 => {
                let high = (param & 0xF0) >> 4;
                let low = param & 0x0F;
                // Main portamento is mode-aware. Both FT2 and ST3 apply
                // `delta = param * 4` per tick in the native Amiga-period
                // scale (where C-4 = 1712). xmrs internally stores Amiga
                // periods at 1/4 of that scale (C-4 ≈ 427, verify with
                // `amiga_pitch_to_period(48)` ≈ 427, ft2-clone LUT entry
                // for C-4 = 1712 — the ratio is exactly 4). So in xmrs's
                // Amiga mode, the same audible slide speed requires the
                // raw `param` multiplier, not `param * 4`. Linear mode
                // has 4× finer period resolution in the other direction,
                // and keeps the traditional `param * 4` scaling.
                //
                // Fine/extra-fine sub-commands (Fxy/Exy high nibble) are
                // left at their historical scalings — they're small
                // corrections where the off-by-4× isn't audible.
                let main_scale = match freq_type {
                    FrequencyType::LinearFrequencies => 4.0,
                    FrequencyType::AmigaFrequencies => 1.0,
                };
                match (high, low) {
                    (0xE, f) => Some(vec![TrackImportEffect::PortamentoExtraFineDown(f as f32)]),
                    (0xF, f) => Some(vec![TrackImportEffect::PortamentoFineDown(4.0 * f as f32)]),
                    _ => Some(vec![TrackImportEffect::PortamentoDown(
                        main_scale * param as f32,
                    )]),
                }
            }

            // F - Portamento up
            6 => {
                let high = (param & 0xF0) >> 4;
                let low = param & 0x0F;
                let main_scale = match freq_type {
                    FrequencyType::LinearFrequencies => 4.0,
                    FrequencyType::AmigaFrequencies => 1.0,
                };
                match (high, low) {
                    (0xE, f) => Some(vec![TrackImportEffect::PortamentoExtraFineUp(-(f as f32))]),
                    (0xF, f) => Some(vec![TrackImportEffect::PortamentoFineUp(-4.0 * f as f32)]),
                    _ => Some(vec![TrackImportEffect::PortamentoUp(
                        -main_scale * param as f32,
                    )]),
                }
            }

            // G - Tone portamento
            7 => {
                let speed = match freq_type {
                    FrequencyType::LinearFrequencies => 4.0 * param as f32,
                    FrequencyType::AmigaFrequencies => param as f32,
                };
                Some(vec![TrackImportEffect::TonePortamento(speed)])
            }

            // H - Vibrato
            8 => {
                let speed = ((param & 0xF0) >> 4) as f32 / 64.0;
                let depth = (param & 0x0F) as f32 / 16.0;
                Some(vec![TrackImportEffect::Vibrato(speed, depth)])
            }

            // I - Tremor
            9 => {
                let on = (param >> 4) as usize;
                let off = (param & 0x0F) as usize;
                Some(vec![TrackImportEffect::Tremor(on, off)])
            }

            // J - Arpeggio
            10 => {
                if param > 0 {
                    let v1 = (param >> 4) as usize;
                    let v2 = (param & 0x0F) as usize;
                    Some(vec![TrackImportEffect::Arpeggio(v1, v2)])
                } else {
                    None
                }
            }

            // K - Vibrato + Volume slide
            11 => {
                let vs =
                    Self::s3m_volume_slide(param).unwrap_or(TrackImportEffect::VolumeSlideN(0.0));
                Some(vec![TrackImportEffect::Vibrato(0.0, 0.0), vs])
            }

            // L - Tone portamento + Volume slide
            12 => {
                let vs =
                    Self::s3m_volume_slide(param).unwrap_or(TrackImportEffect::VolumeSlideN(0.0));
                Some(vec![TrackImportEffect::TonePortamento(0.0), vs])
            }

            // M - Set channel volume
            13 => Some(vec![TrackImportEffect::ChannelVolume(
                param.min(64) as f32 / 64.0,
            )]),

            // N - Channel volume slide
            14 => Self::s3m_channel_volume_slide(param).map(|e| vec![e]),

            // O - Sample offset
            15 => Some(vec![TrackImportEffect::InstrumentSampleOffset(
                param as usize * 256,
            )]),

            // P - Panning slide
            16 => Self::s3m_panning_slide(param).map(|e| vec![e]),

            // Q - Retrigger
            17 => {
                let vol = (param >> 4) as usize;
                let speed = (param & 0x0F) as usize;
                Some(vec![TrackImportEffect::NoteRetrigExtended(speed, vol)])
            }

            // R - Tremolo
            18 => {
                let speed = ((param & 0xF0) >> 4) as f32 / 64.0;
                let depth = (param & 0x0F) as f32 / 16.0;
                Some(vec![TrackImportEffect::Tremolo(speed, depth)])
            }

            // S - Special sub-commands
            19 => {
                let sub = param >> 4;
                let val = param & 0x0F;
                match sub {
                    0x1 => Some(vec![TrackImportEffect::Glissando(val != 0)]),
                    0x2 => Some(vec![TrackImportEffect::InstrumentFineTune(
                        val as f32 / 8.0 - 1.0,
                    )]),
                    0x3 => {
                        let wf = match val & 3 {
                            1 => Waveform::RampDown,
                            2 => Waveform::Square,
                            _ => Waveform::Sine,
                        };
                        Some(vec![TrackImportEffect::VibratoWaveform(wf, (val & 4) == 0)])
                    }
                    0x4 => {
                        let wf = match val & 3 {
                            1 => Waveform::RampDown,
                            2 => Waveform::Square,
                            _ => Waveform::Sine,
                        };
                        Some(vec![TrackImportEffect::TremoloWaveform(wf, (val & 4) == 0)])
                    }
                    0x8 => Some(vec![TrackImportEffect::Panning(val as f32 / 15.0)]),
                    0x9 => Some(vec![TrackImportEffect::NoteRetrig(val as usize)]),
                    0xC => {
                        if val > 0 {
                            Some(vec![TrackImportEffect::NoteCut(val as usize, false)])
                        } else {
                            None
                        }
                    }
                    0xD => {
                        if val > 0 {
                            Some(vec![TrackImportEffect::NoteDelay(val as usize)])
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            }

            // U - Fine vibrato (4x finer depth than H)
            21 => {
                let speed = ((param & 0xF0) >> 4) as f32 / 64.0;
                let depth = (param & 0x0F) as f32 / 64.0;
                Some(vec![TrackImportEffect::VibratoFine(speed, depth)])
            }

            // X - Set panning
            24 => Some(vec![TrackImportEffect::Panning(param as f32 / 255.0)]),

            _ => None,
        }
    }

    // --- Volume column (raw S3M: 0-64, 0x80-0xC0 panning, or absent) ---

    fn s3m_walk_volume(current: &PatternSlot) -> Option<TrackImportEffect> {
        match current.volume {
            v if v <= 64 => Some(TrackImportEffect::Volume(v as f32 / 64.0, 0)),
            // MPT-style S3M panning via the volume column. 0x80 = hard left,
            // 0xC0 = hard right; 65 discrete steps in between. We scale to
            // the [0.0, 1.0] range used by TrackImportEffect::Panning.
            v if (0x80..=0xC0).contains(&v) => {
                Some(TrackImportEffect::Panning((v - 0x80) as f32 / 64.0))
            }
            _ => None,
        }
    }

    // --- Global effects ---

    fn s3m_walk_global_effect(current: &PatternSlot) -> Option<GlobalEffect> {
        let param = current.effect_parameter;
        match current.effect_type {
            // A - Set speed
            1 => {
                if param > 0 && param <= 0x1F {
                    Some(GlobalEffect::Speed(param as usize))
                } else {
                    None
                }
            }
            // B - Jump to order
            2 => Some(GlobalEffect::PositionJump(param as usize)),
            // C - Break to row
            3 => Some(GlobalEffect::PatternBreak(param as usize)),
            // S - Special sub-commands (global)
            19 => {
                let sub = param >> 4;
                let val = param & 0x0F;
                match sub {
                    0xB => Some(GlobalEffect::PatternLoop(val as usize)),
                    0xE => Some(GlobalEffect::PatternDelay {
                        quantity: val as usize,
                        tempo: true,
                    }),
                    _ => None,
                }
            }
            // T - Set tempo
            20 => {
                if param >= 0x20 {
                    Some(GlobalEffect::Bpm(param as usize))
                } else {
                    None
                }
            }
            // V - Set global volume
            22 => {
                if param <= 0x40 {
                    Some(GlobalEffect::Volume(param as f32 / 64.0))
                } else {
                    None
                }
            }
            // W - Global volume slide
            23 => {
                let high = (param & 0xF0) >> 4;
                let low = param & 0x0F;
                match (high, low) {
                    (0xF, f) => Some(GlobalEffect::VolumeSlide {
                        speed: -(f as f32) / 64.0,
                        fine: true,
                    }),
                    (f, 0xF) => Some(GlobalEffect::VolumeSlide {
                        speed: f as f32 / 64.0,
                        fine: true,
                    }),
                    (0, f) => Some(GlobalEffect::VolumeSlide {
                        speed: -(f as f32) / 64.0,
                        fine: false,
                    }),
                    (f, 0) => Some(GlobalEffect::VolumeSlide {
                        speed: f as f32 / 64.0,
                        fine: false,
                    }),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    // --- Public API ---

    pub fn s3m_unpack(freq_type: FrequencyType, current: &PatternSlot) -> TrackImportUnit {
        let te = Self::s3m_walk_effect(freq_type, current);
        let ve = Self::s3m_walk_volume(current);
        let ge = Self::s3m_walk_global_effect(current);

        let mut tiu = TrackImportUnit::default();
        tiu.note = current.note;
        tiu.instrument = current.instrument;
        if let Some(e) = ve {
            tiu.effects.push(e);
        }
        if let Some(e) = te {
            tiu.effects.extend(e);
        }
        if let Some(g) = ge {
            tiu.global_effects.push(g);
        }
        tiu
    }

    pub fn s3m_unpack_pattern(
        freq_type: FrequencyType,
        pattern: &[Vec<PatternSlot>],
    ) -> Vec<Vec<TrackImportUnit>> {
        pattern
            .iter()
            .map(|row| {
                row.iter()
                    .map(|slot| Self::s3m_unpack(freq_type, slot))
                    .collect()
            })
            .collect()
    }
}
