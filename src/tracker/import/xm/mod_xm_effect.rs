//! Effect parser shared by XM and the Amiga MOD importer (which
//! reuses the XM `(effect_type, parameter)` numbering). Converts
//! the raw `(eff, prm)` byte pair into the format-agnostic
//! `TrackImportEffect` / `NavigationEffect` / `SongLevelEffect`
//! triple, with the per-channel memory back-fill applied in a
//! later pass by the importer-private `memory` helper.

use crate::core::fixed::fixed::{Q15, Q8_8};
use crate::core::fixed::from_tracker::{
    lfo_depth_from_nibble_16, lfo_speed_from_nibble_64, vibrato_depth_from_nibble_16,
};
use crate::core::fixed::units::{Finetune, Panning, PitchDelta, Volume};
use crate::prelude::*;
use crate::tracker::import::effect::TrackImportEffect;
use crate::tracker::import::patternslot::PatternSlot;
use crate::tracker::import::unit::TrackImportUnit;
use alloc::vec;
use alloc::vec::Vec;

pub struct ModXmEffect;

impl ModXmEffect {
    fn mod_xm_volume_slide(param: u8) -> Option<Q15> {
        let upper_nibble = param & 0xF0;
        let lower_nibble = param & 0x0F;

        match (upper_nibble, lower_nibble) {
            (f, 0) => Some(Q15::from_ratio((f >> 4) as i32, 64)), // Slide up
            (0, f) => Some(Q15::from_ratio(-(f as i32), 64)),     // Slide down
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
                //
                // PortamentoUp speed is the *signed* delta added to
                // period each tick — pitch up means period goes down
                // (period and pitch are inverse), so we use a negative
                // delta. Range: linear `-4*255 = -1020`, fits in `i16`.
                let param = current.effect_parameter as i16;
                let speed: i16 = match freq_type {
                    FrequencyType::LinearFrequencies => -4 * param,
                    FrequencyType::AmigaFrequencies => -param,
                };
                Some(vec![TrackImportEffect::PortamentoUp(speed)])
            }
            0x02 => {
                let param = current.effect_parameter as i16;
                let speed: i16 = match freq_type {
                    FrequencyType::LinearFrequencies => 4 * param,
                    FrequencyType::AmigaFrequencies => param,
                };
                Some(vec![TrackImportEffect::PortamentoDown(speed)])
            }
            0x03 => {
                let param = current.effect_parameter as i16;
                let speed: i16 = match freq_type {
                    FrequencyType::LinearFrequencies => 4 * param,
                    FrequencyType::AmigaFrequencies => param,
                };
                Some(vec![TrackImportEffect::TonePortamento(speed)])
            }
            0x04 => {
                // XM 4xy: x = speed nibble (1/64 cycles/tick),
                // y = depth nibble (1/16 semitones).
                let param = current.effect_parameter;
                let speed = lfo_speed_from_nibble_64((param & 0xF0) >> 4);
                let depth = vibrato_depth_from_nibble_16(param & 0x0F);
                Some(vec![TrackImportEffect::Vibrato(speed, depth)])
            }
            0x05 => {
                let vs = Self::mod_xm_volume_slide(current.effect_parameter);
                if let Some(vste) = vs {
                    Some(vec![
                        TrackImportEffect::TonePortamento(0),
                        TrackImportEffect::VolumeSlideN(vste),
                    ])
                } else {
                    Some(vec![
                        TrackImportEffect::TonePortamento(0),
                        TrackImportEffect::VolumeSlideN(Q15::ZERO),
                    ])
                }
            }
            0x06 => {
                if let Some(vste) = Self::mod_xm_volume_slide(current.effect_parameter) {
                    Some(vec![
                        TrackImportEffect::Vibrato(Q8_8::ZERO, PitchDelta::ZERO),
                        TrackImportEffect::VolumeSlideN(vste),
                    ])
                } else {
                    Some(vec![
                        TrackImportEffect::Vibrato(Q8_8::ZERO, PitchDelta::ZERO),
                        TrackImportEffect::VolumeSlideN(Q15::ZERO),
                    ])
                }
            }
            0x07 => {
                // XM 7xy: x = speed nibble, y = depth nibble (1/16
                // amplitude). Tremolo modulates volume in `[-1, 1]`,
                // so depth is `Q15`.
                let param = current.effect_parameter;
                let speed = lfo_speed_from_nibble_64((param & 0xF0) >> 4);
                let depth = lfo_depth_from_nibble_16(param & 0x0F);
                Some(vec![TrackImportEffect::Tremolo(speed, depth)])
            }
            0x08 => Some(vec![TrackImportEffect::Panning(Panning::from_byte_255(
                current.effect_parameter,
            ))]),
            0x09 => Some(vec![TrackImportEffect::InstrumentSampleOffset(
                current.effect_parameter as usize * 256,
            )]),
            0x0A => Self::mod_xm_volume_slide(current.effect_parameter)
                .map(|vste| vec![TrackImportEffect::VolumeSlideN(vste)]),
            0x0C => Some(vec![TrackImportEffect::Volume(
                Volume::from_byte_64(current.effect_parameter.min(64)),
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
                        let p = param as i16;
                        let speed: i16 = match freq_type {
                            FrequencyType::LinearFrequencies => -4 * p,
                            FrequencyType::AmigaFrequencies => -p,
                        };
                        vec![TrackImportEffect::PortamentoFineUp(speed)]
                    }),
                    0x2 => (param != 0).then(|| {
                        let p = param as i16;
                        let speed: i16 = match freq_type {
                            FrequencyType::LinearFrequencies => 4 * p,
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
                        // E5y: signed nibble centred at 8 (E58 =
                        // zero); `(y - 8) / 8` maps to `[-1, +1]`.
                        // Was `param as f32 / 8.0 - 1.0`; the
                        // equivalent integer form is `(y - 8) / 8`.
                        Finetune::from_ratio(param as i32 - 8, 8),
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
                    0xA => Some(vec![TrackImportEffect::VolumeSlide0(Q15::from_ratio(
                        param as i32,
                        64,
                    ))]),
                    0xB => Some(vec![TrackImportEffect::VolumeSlide0(Q15::from_ratio(
                        -(param as i32),
                        64,
                    ))]),
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
                    (f, 0) => Some(vec![TrackImportEffect::PanningSlideN(Q15::from_ratio(
                        (f >> 4) as i32,
                        16,
                    ))]), // Slide up
                    (0, f) => Some(vec![TrackImportEffect::PanningSlideN(Q15::from_ratio(
                        -(f as i32),
                        16,
                    ))]), // Slide down
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
                        // 4× coarser).
                        //
                        // PHASE 2 PRECISION NOTE: the previous f32
                        // speed could carry the 0.25 amiga factor
                        // exactly. With `i16` we truncate via `/4`,
                        // so amiga X1/X2 with `y < 4` becomes a no-op
                        // (loss of 0.25/0.5/0.75 sub-period steps).
                        // Practical impact is limited — extra-fine
                        // amiga sliding is rare in MOD modules — and
                        // honest integer truncation matches `Period`'s
                        // current `u16` storage.
                        let p = param as i16;
                        let speed: i16 = match freq_type {
                            FrequencyType::LinearFrequencies => -p,
                            FrequencyType::AmigaFrequencies => -p / 4,
                        };
                        Some(vec![TrackImportEffect::PortamentoExtraFineUp(speed)])
                    }
                    2 => {
                        // X2y: Extra fine portamento down. Same
                        // reasoning as X1y above.
                        let p = param as i16;
                        let speed: i16 = match freq_type {
                            FrequencyType::LinearFrequencies => p,
                            FrequencyType::AmigaFrequencies => p / 4,
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
                Some(TrackImportEffect::Volume(
                    Volume::from_byte_64(clamped - 0x10),
                    0,
                ))
            }
            // - - Volume slide down (0..15). Regular slide: applies at
            //     ticks > 0, i.e. VolumeSlideN in our vocabulary
            //     (FT2 VJumpTab_TickNonZero[6] = v_VolSlideDown).
            0x6 => Some(TrackImportEffect::VolumeSlideN(Q15::from_ratio(
                -((current.volume & 0x0F) as i32),
                64,
            ))),
            // + - Volume slide up (0..15). Same tick scope as 0x6.
            0x7 => Some(TrackImportEffect::VolumeSlideN(Q15::from_ratio(
                (current.volume & 0x0F) as i32,
                64,
            ))),
            // D - Fine volume slide down (0..15). Fine slide: applies once
            //     at tick 0, i.e. VolumeSlide0
            //     (FT2 VJumpTab_TickZero[8] = v_FineVolSlideDown).
            0x8 => Some(TrackImportEffect::VolumeSlide0(Q15::from_ratio(
                -((current.volume & 0x0F) as i32),
                64,
            ))),
            // U - Fine volume slide up (0..15). Same tick scope as 0x8.
            0x9 => Some(TrackImportEffect::VolumeSlide0(Q15::from_ratio(
                (current.volume & 0x0F) as i32,
                64,
            ))),
            // S - Vibrato speed (0..15)
            0xA => {
                let s = current.volume & 0x0F;
                if s != 0 {
                    Some(TrackImportEffect::VibratoSpeed(lfo_speed_from_nibble_64(s)))
                } else {
                    None
                }
            }
            // V - Vibrato depth (0..15)
            0xB => {
                let d = current.volume & 0x0F;
                if d != 0 {
                    Some(TrackImportEffect::VibratoDepthFxVol(
                        vibrato_depth_from_nibble_16(d),
                    ))
                } else {
                    None
                }
            }
            // P - Set panning
            0xC => Some(TrackImportEffect::Panning(
                // Volume column nibble `0..=15` → pan position.
                // Was `nibble as f32 / 16.0` — direct ratio.
                Panning::from_ratio((current.volume & 0x0F) as i32, 16),
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
                    // this as an explicit `Panning::LEFT` keeps the
                    // replayer path simple.
                    Some(TrackImportEffect::Panning(Panning::LEFT))
                } else {
                    Some(TrackImportEffect::PanningSlideN(Q15::from_ratio(
                        -(nibble as i32),
                        16,
                    )))
                }
            }
            // R - Panning slide right
            0xE => Some(TrackImportEffect::PanningSlideN(Q15::from_ratio(
                (current.volume & 0x0F) as i32,
                16,
            ))),
            // M - Tone portamento (0..15)
            0xF => {
                let speed = (current.volume & 0x0F) as i16 * 16;
                let speed: i16 = match freq_type {
                    FrequencyType::LinearFrequencies => 4 * speed,
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
            0x0B => Some(NavigationEffect::PositionJump(current.effect_parameter as usize).into()),
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
                Some(NavigationEffect::PatternBreak(target).into())
            }
            // EXy: Extended command
            0x0E => {
                let param = current.effect_parameter & 0x0F;
                match current.effect_parameter >> 4 {
                    // E6y: Pattern loop
                    0x6 => Some(NavigationEffect::PatternLoop(param as usize).into()),
                    // EEy: Pattern delay
                    0xE => Some(
                        NavigationEffect::PatternDelay {
                            quantity: param as usize,
                            tempo: true,
                        }
                        .into(),
                    ),
                    _ => None,
                }
            }
            // Fxx: Set speed/BPM
            0x0F => {
                let param = current.effect_parameter;
                if param < 32 {
                    Some(SongLevelEffect::Speed(param as usize).into())
                } else {
                    Some(SongLevelEffect::Bpm(param as usize).into())
                }
            }
            // Gxx: Set global volume
            0x10 => Some(
                SongLevelEffect::Volume(Volume::from_byte_64(current.effect_parameter.min(64)))
                    .into(),
            ),
            // Hxy: Global volume slide
            0x11 => {
                let param = current.effect_parameter;
                let upper_nibble = param & 0xF0;
                let lower_nibble = param & 0x0F;

                match (upper_nibble, lower_nibble) {
                    (0, f) => Some(
                        SongLevelEffect::VolumeSlide {
                            speed: Q15::from_ratio(-(f as i32), 64),
                            fine: false,
                        }
                        .into(),
                    ), // Slide down
                    (f, 0) => Some(
                        SongLevelEffect::VolumeSlide {
                            speed: Q15::from_ratio((f >> 4) as i32, 64),
                            fine: false,
                        }
                        .into(),
                    ), // Slide up
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
            tiu.push_global(c);
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
