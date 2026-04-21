use crate::import::patternslot::PatternSlot;
use crate::import::track_import_effect::TrackImportEffect;
use crate::import::track_import_unit::TrackImportUnit;
use crate::prelude::*;
use alloc::vec;
use alloc::vec::Vec;

pub struct ModXmEffect;

impl ModXmEffect {
    fn mod_xm_volume_slide(param: u8) -> Option<f32> {
        let upper_nibble = param & 0xF0;
        let lower_nibble = param & 0x0F;

        match (upper_nibble, lower_nibble) {
            (f, 0) => Some((f >> 4) as f32 / 64.0), // Slide up
            (0, f) => Some(-(f as f32) / 64.0),     // Slide down
            _ => None,
        }
    }

    fn mod_xm_walk_effect(
        freq_type: FrequencyType,
        current: &PatternSlot,
    ) -> Option<Vec<TrackImportEffect>> {
        match current.effect_type {
            0x00 => {
                let param = current.effect_parameter;
                if param > 0 {
                    let v1 = (param >> 4) as usize;
                    let v2 = (param & 0x0F) as usize;
                    Some(vec![TrackImportEffect::Arpeggio(v1, v2)])
                } else {
                    None
                }
            }
            0x01 => {
                // Mode-aware scaling. xmrs stores Amiga periods at 1/4
                // of the native scale (C-4 ≈ 427 vs ft2-clone's 1712),
                // so FT2's `realPeriod -= param * 4` in real Amiga units
                // equals `period -= param * 1` in xmrs units. Linear
                // mode keeps the classic ×4 multiplier because its
                // period resolution is 4× finer than Amiga's the other
                // way around.
                let param = current.effect_parameter as f32;
                let speed = match freq_type {
                    FrequencyType::LinearFrequencies => -4.0 * param,
                    FrequencyType::AmigaFrequencies => -param,
                };
                Some(vec![TrackImportEffect::PortamentoUp(speed)])
            }
            0x02 => {
                let param = current.effect_parameter as f32;
                let speed = match freq_type {
                    FrequencyType::LinearFrequencies => 4.0 * param,
                    FrequencyType::AmigaFrequencies => param,
                };
                Some(vec![TrackImportEffect::PortamentoDown(speed)])
            }
            0x03 => {
                let param = current.effect_parameter as f32;
                let speed = match freq_type {
                    FrequencyType::LinearFrequencies => 4.0 * param,
                    FrequencyType::AmigaFrequencies => param,
                };
                Some(vec![TrackImportEffect::TonePortamento(speed)])
            }
            0x04 => {
                let param = current.effect_parameter;
                let speed = ((param & 0xF0) >> 4) as f32 / 64.0;
                let depth = (param & 0x0F) as f32 / 16.0;
                Some(vec![TrackImportEffect::Vibrato(speed, depth)])
            }
            0x05 => {
                let vs = Self::mod_xm_volume_slide(current.effect_parameter);
                if let Some(vste) = vs {
                    Some(vec![
                        TrackImportEffect::TonePortamento(0.0),
                        TrackImportEffect::VolumeSlideN(vste),
                    ])
                } else {
                    Some(vec![
                        TrackImportEffect::TonePortamento(0.0),
                        TrackImportEffect::VolumeSlideN(0.0),
                    ])
                }
            }
            0x06 => {
                if let Some(vste) = Self::mod_xm_volume_slide(current.effect_parameter) {
                    Some(vec![
                        TrackImportEffect::Vibrato(0.0, 0.0),
                        TrackImportEffect::VolumeSlideN(vste),
                    ])
                } else {
                    Some(vec![
                        TrackImportEffect::Vibrato(0.0, 0.0),
                        TrackImportEffect::VolumeSlideN(0.0),
                    ])
                }
            }
            0x07 => {
                let param = current.effect_parameter;
                let speed = ((param & 0xF0) >> 4) as f32 / 64.0;
                let depth = (param & 0x0F) as f32 / 16.0;
                Some(vec![TrackImportEffect::Tremolo(speed, depth)])
            }
            0x08 => Some(vec![TrackImportEffect::Panning(
                (current.effect_parameter as f32) / 255.0,
            )]),
            0x09 => Some(vec![TrackImportEffect::InstrumentSampleOffset(
                current.effect_parameter as usize * 256,
            )]),
            0x0A => Self::mod_xm_volume_slide(current.effect_parameter)
                .map(|vste| vec![TrackImportEffect::VolumeSlideN(vste)]),
            0x0C => Some(vec![TrackImportEffect::Volume(
                (current.effect_parameter.min(64) as f32) / 64.0,
                0,
            )]),
            0x0E => {
                let param = current.effect_parameter & 0x0F;
                match current.effect_parameter >> 4 {
                    // E1y / E2y: Fine portamento up / down. Same
                    // scaling as regular 1xx/2xx (FT2 `realPeriod -=
                    // param * 4`), just tick-0-only. Mode-aware:
                    // Linear uses `* 4`, Amiga uses `* 1` because
                    // xmrs stores Amiga periods at 1/4 of FT2's
                    // native scale (see the regular Portamento arm
                    // above for the same reasoning). ProTracker MOD
                    // is always Amiga, so the * 1 path matches it
                    // directly.
                    0x1 => (param != 0).then(|| {
                        let p = param as f32;
                        let speed = match freq_type {
                            FrequencyType::LinearFrequencies => -4.0 * p,
                            FrequencyType::AmigaFrequencies => -p,
                        };
                        vec![TrackImportEffect::PortamentoFineUp(speed)]
                    }),
                    0x2 => (param != 0).then(|| {
                        let p = param as f32;
                        let speed = match freq_type {
                            FrequencyType::LinearFrequencies => 4.0 * p,
                            FrequencyType::AmigaFrequencies => p,
                        };
                        vec![TrackImportEffect::PortamentoFineDown(speed)]
                    }),
                    0x3 => Some(vec![TrackImportEffect::Glissando(
                        current.effect_parameter != 0,
                    )]),
                    0x4 => {
                        let waveform = match param & 0b0000_0011 {
                            1 => Waveform::RampDown,
                            2 => Waveform::Square,
                            _ => Waveform::Sine,
                        };
                        let retrig = (param & 0b0000_0100) == 0;
                        Some(vec![TrackImportEffect::VibratoWaveform(waveform, retrig)])
                    }
                    0x5 => Some(vec![TrackImportEffect::InstrumentFineTune(
                        param as f32 / 8.0 - 1.0,
                    )]),
                    0x7 => {
                        let waveform = match param & 0b0000_0011 {
                            1 => Waveform::RampDown,
                            2 => Waveform::Square,
                            _ => Waveform::Sine,
                        };
                        let retrig = (param & 0b0000_0100) == 0;
                        Some(vec![TrackImportEffect::TremoloWaveform(waveform, retrig)])
                    }
                    0x9 => Some(vec![TrackImportEffect::NoteRetrig(param as usize)]),
                    0xA => Some(vec![TrackImportEffect::VolumeSlide0((param as f32) / 64.0)]),
                    0xB => Some(vec![TrackImportEffect::VolumeSlide0(
                        -(param as f32) / 64.0,
                    )]),
                    0xC => Some(vec![TrackImportEffect::NoteCut(param as usize, false)]),
                    0xD => Some(vec![TrackImportEffect::NoteDelay(param as usize)]),
                    _ => None,
                }
            }
            // Kxx: Key off
            0x14 => Some(vec![TrackImportEffect::NoteOff(
                current.effect_parameter as usize,
                false,
            )]),
            // Lxx: Set envelope position
            0x15 => Some(vec![TrackImportEffect::InstrumentVolumeEnvelopePosition(
                current.effect_parameter as usize,
            )]),
            // Pxy: Panning slide
            0x19 => {
                let param = current.effect_parameter;
                let upper_nibble = param & 0xF0;
                let lower_nibble = param & 0x0F;

                match (upper_nibble, lower_nibble) {
                    (f, 0) => Some(vec![TrackImportEffect::PanningSlideN(
                        (f >> 4) as f32 / 16.0,
                    )]), // Slide up
                    (0, f) => Some(vec![TrackImportEffect::PanningSlideN(-(f as f32) / 16.0)]), // Slide down
                    _ => None,
                }
            }
            // Rxy: Multi retrig note
            0x1B => {
                let param = current.effect_parameter;
                let vol = param >> 4;
                let speed = param & 0x0F;
                Some(vec![TrackImportEffect::NoteRetrigExtended(
                    speed as usize,
                    vol as usize,
                )])
            }
            // Txy: Tremor
            // Rapidly switches the sample volume on and off on every tick of the row except the first.
            // Volume is on for x + 1 ticks and off for y + 1 ticks.
            // tremor_on: bool = [(T-1) % (X+1+Y+1) ] > X
            0x1D => {
                let param = current.effect_parameter;
                let on_time = param >> 4;
                let off_time = param & 0x0F;
                Some(vec![TrackImportEffect::Tremor(
                    on_time as usize,
                    off_time as usize,
                )])
            }
            // X1 & X2
            0x21 => {
                let effect_case = current.effect_parameter >> 4;
                let param = current.effect_parameter & 0x0F;

                match effect_case {
                    1 => {
                        // X1y: Extra fine portamento up. FT2 uses
                        // `realPeriod -= param * 1` (no *4), hence
                        // "extra fine" — 4× finer than regular 1xx.
                        // Mode-aware: Linear uses `* 1` (matches
                        // FT2), Amiga uses `* 0.25` (xmrs Amiga is
                        // 4× coarser). This applies at tick 0 only.
                        let p = param as f32;
                        let speed = match freq_type {
                            FrequencyType::LinearFrequencies => -p,
                            FrequencyType::AmigaFrequencies => -p * 0.25,
                        };
                        Some(vec![TrackImportEffect::PortamentoExtraFineUp(speed)])
                    }
                    2 => {
                        // X2y: Extra fine portamento down. Same
                        // reasoning as X1y above.
                        let p = param as f32;
                        let speed = match freq_type {
                            FrequencyType::LinearFrequencies => p,
                            FrequencyType::AmigaFrequencies => p * 0.25,
                        };
                        Some(vec![TrackImportEffect::PortamentoExtraFineDown(speed)])
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn mod_xm_walk_volume(
        freq_type: FrequencyType,
        current: &PatternSlot,
    ) -> Option<TrackImportEffect> {
        match current.volume >> 4 {
            0x0 => None,
            // V - Set volume (FT2 VJumpTab_TickZero[1..=5] = v_SetVolume).
            // The volume byte range is 0x10..=0x5F: subtract 0x10 to
            // get the XM volume in 0..=79, then clamp to 64 (FT2's
            // `if (*volColumnData > 64) *volColumnData = 64`). The
            // `0x50` byte is the common idiom for "set volume to
            // max" — dropping it would silently discard a very
            // frequent vol-column command.
            0x1..=0x5 => {
                let clamped = current.volume.min(0x50);
                Some(TrackImportEffect::Volume((clamped - 0x10) as f32 / 64.0, 0))
            }
            // - - Volume slide down (0..15). Regular slide: applies at
            //     ticks > 0, i.e. VolumeSlideN in our vocabulary
            //     (FT2 VJumpTab_TickNonZero[6] = v_VolSlideDown).
            0x6 => Some(TrackImportEffect::VolumeSlideN(
                -((current.volume & 0x0F) as f32) / 64.0,
            )),
            // + - Volume slide up (0..15). Same tick scope as 0x6.
            0x7 => Some(TrackImportEffect::VolumeSlideN(
                ((current.volume & 0x0F) as f32) / 64.0,
            )),
            // D - Fine volume slide down (0..15). Fine slide: applies once
            //     at tick 0, i.e. VolumeSlide0
            //     (FT2 VJumpTab_TickZero[8] = v_FineVolSlideDown).
            0x8 => Some(TrackImportEffect::VolumeSlide0(
                -((current.volume & 0x0F) as f32) / 64.0,
            )),
            // U - Fine volume slide up (0..15). Same tick scope as 0x8.
            0x9 => Some(TrackImportEffect::VolumeSlide0(
                ((current.volume & 0x0F) as f32) / 64.0,
            )),
            // S - Vibrato speed (0..15)
            0xA => {
                let s = current.volume & 0x0F;
                if s != 0 {
                    Some(TrackImportEffect::VibratoSpeed((s as f32) / 64.0))
                } else {
                    None
                }
            }
            // V - Vibrato depth (0..15)
            0xB => {
                let d = current.volume & 0x0F;
                if d != 0 {
                    Some(TrackImportEffect::VibratoDepthFxVol((d as f32) / 16.0))
                } else {
                    None
                }
            }
            // P - Set panning
            0xC => Some(TrackImportEffect::Panning(
                ((current.volume & 0x0F) as f32) / 16.0,
            )),
            // L - Panning slide left
            0xD => {
                let nibble = current.volume & 0x0F;
                if nibble == 0 {
                    // FT2 quirk: pan-slide-left with nibble 0 sets the pan
                    // to full-left (0). In ft2_replayer.c:v_PanSlideLeft
                    // the check `if (tmp16 < 256) tmp16 = 0;` fires
                    // unconditionally when nibble == 0, because tmp16 then
                    // equals outPan (always < 256). Documented there as an
                    // FT2 bug that some XM modules rely on. Materializing
                    // this as an explicit `Panning(0.0)` keeps the replayer
                    // path simple.
                    Some(TrackImportEffect::Panning(0.0))
                } else {
                    Some(TrackImportEffect::PanningSlideN(-(nibble as f32) / 16.0))
                }
            }
            // R - Panning slide right
            0xE => Some(TrackImportEffect::PanningSlideN(
                ((current.volume & 0x0F) as f32) / 16.0,
            )),
            // M - Tone portamento (0..15)
            0xF => {
                let speed = (current.volume & 0x0F) as f32 * 16.0;
                let speed = match freq_type {
                    FrequencyType::LinearFrequencies => 4.0 * speed,
                    FrequencyType::AmigaFrequencies => speed,
                };
                Some(TrackImportEffect::TonePortamentoFxVol(speed))
            }
            _ => None,
        }
    }

    fn mod_xm_walk_global_effect(current: &PatternSlot) -> Option<GlobalEffect> {
        match current.effect_type {
            // Bxx: Position jump
            0x0B => Some(GlobalEffect::PositionJump(
                current.effect_parameter as usize,
            )),
            // Dxx: Pattern break
            0x0D => {
                let param = current.effect_parameter;
                let ten = (param >> 4) as usize * 10;
                let unit = (param & 0x0F) as usize;
                let total = ten + unit;
                // FT2 clamps the break target: any BCD value greater than
                // 63 is silently converted to 0 (ft2_replayer.c:patternBreak).
                // Without this, malformed Dxx values (e.g. D64..D99, D9F)
                // would point at a non-existent row.
                let target = if total > 63 { 0 } else { total };
                Some(GlobalEffect::PatternBreak(target))
            }
            // EXy: Extended command
            0x0E => {
                let param = current.effect_parameter & 0x0F;
                match current.effect_parameter >> 4 {
                    // E6y: Pattern loop
                    0x6 => Some(GlobalEffect::PatternLoop(param as usize)),
                    // EEy: Pattern delay
                    0xE => Some(GlobalEffect::PatternDelay {
                        quantity: param as usize,
                        tempo: true,
                    }),
                    _ => None,
                }
            }
            // Fxx: Set speed/BPM
            0x0F => {
                let param = current.effect_parameter;
                if param < 32 {
                    Some(GlobalEffect::Speed(param as usize))
                } else {
                    Some(GlobalEffect::Bpm(param as usize))
                }
            }
            // Gxx: Set global volume
            0x10 => Some(GlobalEffect::Volume(
                (current.effect_parameter.min(64) as f32) / 64.0,
            )),
            // Hxy: Global volume slide
            0x11 => {
                let param = current.effect_parameter;
                let upper_nibble = param & 0xF0;
                let lower_nibble = param & 0x0F;

                match (upper_nibble, lower_nibble) {
                    (0, f) => Some(GlobalEffect::VolumeSlide {
                        speed: -(f as f32) / 64.0,
                        fine: false,
                    }), // Slide down
                    (f, 0) => Some(GlobalEffect::VolumeSlide {
                        speed: (f >> 4) as f32 / 64.0,
                        fine: false,
                    }), // Slide up
                    _ => None,
                }
            }
            _ => None,
        }
    }

    pub fn mod_xm_unpack(freq_type: FrequencyType, current: &PatternSlot) -> TrackImportUnit {
        let te = Self::mod_xm_walk_effect(freq_type, current);
        let ve = Self::mod_xm_walk_volume(freq_type, current);
        let cc = Self::mod_xm_walk_global_effect(current);

        let mut tiu = TrackImportUnit::default();
        tiu.note = current.note;
        tiu.instrument = current.instrument;
        if let Some(e) = ve {
            tiu.effects.push(e);
        }
        if let Some(e) = te {
            tiu.effects.extend(e)
        }

        if let Some(c) = cc {
            tiu.global_effects.push(c);
        }
        tiu
    }

    pub fn mod_xm_unpack_row(
        freq_type: FrequencyType,
        row: &[PatternSlot],
    ) -> Vec<TrackImportUnit> {
        row.iter()
            .map(|pattern_slot| ModXmEffect::mod_xm_unpack(freq_type, pattern_slot))
            .collect()
    }

    pub fn mod_xm_unpack_pattern(
        freq_type: FrequencyType,
        pattern: &[Vec<PatternSlot>],
    ) -> Vec<Vec<TrackImportUnit>> {
        pattern
            .iter()
            .map(|row| Self::mod_xm_unpack_row(freq_type, row))
            .collect()
    }
}
