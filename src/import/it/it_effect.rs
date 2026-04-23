/*
InitCommandTable and CommandTable (see `IT_MUSIC.ASM`):

T | Effect (letter and params)  | Code inside IT file     | InitCommandTable | CommandTable | Comment                                           |
- |-----------------------------|-------------------------|------------------|--------------|---------------------------------------------------|
V | Axx                         | 0x01                    | true             | false        | Set song speed (xx = speed)                       |
V | Bxx                         | 0x02                    | true             | false        | Jump to order xx                                  |
V | Cxx                         | 0x03                    | true             | false        | Break to row xx                                   |
V | Dxy                         | 0x04                    | true             | true         | Volume slide                                      |
V | Exy                         | 0x05                    | true             | true         | Pitch slide down                                  |
V | Fxy                         | 0x06                    | true             | true         | Pitch slide up                                    |
V | Gxx                         | 0x07                    | true             | true         | Slide to note                                     |
V | Hxy                         | 0x08                    | true             | true         | Vibrato                                           |
V | Ixy                         | 0x09                    | true             | true         | Tremor                                            |
V | Jxy                         | 0x0A                    | true             | true         | Arpeggio                                          |
V | Kxy                         | 0x0B                    | true             | true         | H00 Vibrato + Dxy Volume Slide                    |
V | Lxy                         | 0x0C                    | true             | true         | G00 Portamento + Dxy Volume slide                 |
V | Mxx                         | 0x0D                    | true             | false        | Set channel volume                                |
V | Nxy                         | 0x0E                    | true             | true         | Channel volume slide                              |
V | Oxx                         | 0x0F                    | true             | false        | Set sample offset                                 |
V | Pxx                         | 0x10                    | true             | true         | Panning slide                                     |
V | Qxy                         | 0x11                    | true             | true         | Retriggers a note after y ticks w/ vol modifier x |
V | Rxy                         | 0x12                    | true             | true         | Tremolo                                           |
V | Sxx                         | 0x13                    | true             | true         | Special commands (voir S-effects ci-dessous)      |
V | Txy                         | 0x14                    | true             | true         | Tempo slide                                       |
V | Uxy                         | 0x15                    | true             | true (Hxx!)  | Fine vibrato with speed x, depth y                |
V | Vxx                         | 0x16                    | true             | false        | Set global volume                                 |
V | Wxx                         | 0x17                    | true             | true         | Global volume slide                               |
V | Xxx                         | 0x18                    | true             | false        | Set panning position                              |
V | Yxy                         | 0x19                    | true             | true         | Panbrello                                         |
V | Zxx                         | 0x1A                    | true             | false        | Midi macro                                        |

CommandSTable (see `IT_M_EFF.INC`):

| Sub-Effect Sxx             | Code inside IT file     | Comment                                   |
|---------------------------|-------------------------|-------------------------------------------|
| S3x                       | 0x03                    | Set vibrato waveform                      |
| S4x                       | 0x04                    | Set tremolo waveform                      |
| S5x                       | 0x05                    | Set panbrello waveform                    |
| S6x                       | 0x06                    | Extra delay of x frames                   |
| S7x                       | 0x07                    | Instrument functions (see CommandS7Table) |
| S8x                       | 0x08                    | Set pan                                   |
| S9x                       | 0x09                    | Set surround                              |
| SAx                       | 0x0A                    | Set high order offset                     |
| SBx                       | 0x0B                    | Loop control                              |
| SCx                       | 0x0C                    | Note cut                                  |
| SDx                       | 0x0D                    | Note delay                                |
| SEx                       | 0x0E                    | Pattern delay                             |
| SFx                       | 0x0F                    | MIDI macro select                         |


CommandS7Table (see `IT_M_EFF.INC`):

| Sub-Effect S7x              | Code inside IT file     | Comment                                         |
|----------------------------|-------------------------|-------------------------------------------------|
| S70                        | 0x70                    | Past note cut (coupe la note précédente)        |
| S71                        | 0x71                    | Past note off (arrête la note précédente)       |
| S72                        | 0x72                    | Past note fade (fait fondre la note précédente) |
| S73                        | 0x73                    | Set NNA to cut (note suivante est coupée)       |
| S74                        | 0x74                    | Set NNA to continue (note suivante continue)    |
| S75                        | 0x75                    | Set NNA to off (désactive la note suivante)     |
| S76                        | 0x76                    | Set NNA to fade (fond la note suivante)         |
| S77                        | 0x77                    | Enable volume envelope                          |
| S78                        | 0x78                    | Disable volume envelope                         |
| S79                        | 0x79                    | Enable panning envelope                         |
| S7A                        | 0x7A                    | Disable panning envelope                        |
| S7B                        | 0x7B                    | Enable pitch envelope                           |
| S7C                        | 0x7C                    | Disable pitch envelope                          |


| Code inside IT file | Nearest in XM file  |
|---------------------|---------------------|
| 0x01                | 0x0F                |
| 0x02                | 0x0B                |
| 0x03                | 0x0D                |
| 0x04                | 0x0A                |
| 0x05                | 0x02,0x0E2y,0x212y  |
| 0x06                | 0x01,0x0E1y,0x211y  |
| 0x07                | 0x03                |
| 0x08                | 0x04                |
| 0x09                | 0x1D                |
| 0x0A                | 0x00                |
| 0x0B                | 0x06                |
| 0x0C                | 0x05                |
| 0x0D                | 0x0C                |
| 0x0E                | None                |
| 0x0F                | 0x09                |
| 0x10                | 0x19                |
| 0x11                | 0x1B                |
| 0x12                | 0x07                |
| 0x13                | 0x14                |
| 0x14                | 0x0F                |
| 0x15                | None                |
| 0x16                | 0x10                |
| 0x17                | None                |
| 0x18                | 0x15                |
| 0x19                | 0x17                |
| 0x1A                | 0x18                |

*/

use crate::effect::MidiMacroType;
use crate::import::patternslot::PatternSlot;
use crate::import::track_import_effect::TrackImportEffect;
use crate::import::track_import_unit::TrackImportUnit;
use crate::prelude::*;
use alloc::vec;
use alloc::vec::Vec;

pub struct ItEffect;

/// The idea here is to achieve a first level of abstraction
/// to make the data accessible via rust typing.
/// The data remains raw and is not interpreted.
impl ItEffect {
    // Volume slide helper
    // D0x Volume slide down
    // Dx0 Volume slide up
    // DFx Fine volume slide down
    // DxF Fine volume slide up
    //
    // ITTECH.TXT defines the test order as "Dx0, D0x, DxF, DFx"
    // (IT_MUSIC.ASM enforces the same order). For ambiguous values
    // where both nibbles are `F` (DFF / NFF / WFF), this ordering
    // resolves to the fine-slide-UP reading — so (f, 0xF) must be
    // tested before (0xF, f) in the Rust match, not after, or DFF
    // silently flips to a fine-slide-DOWN.
    fn it_volume_slide(fx: u8) -> Option<TrackImportEffect> {
        let nibble_high = (fx & 0xF0) >> 4;
        let nibble_low = fx & 0x0F;

        match (nibble_high, nibble_low) {
            (f, 0) => Some(TrackImportEffect::VolumeSlideN(f as f32 / 64.0)),
            (0, f) => Some(TrackImportEffect::VolumeSlideN(-(f as f32) / 64.0)),
            (f, 0xF) => Some(TrackImportEffect::VolumeSlide0(f as f32 / 64.0)),
            (0xF, f) => Some(TrackImportEffect::VolumeSlide0(-(f as f32) / 64.0)),
            _ => None,
        }
    }

    pub fn parse_it_effect(
        freq_type: FrequencyType,
        current: &PatternSlot,
    ) -> Option<Vec<TrackImportEffect>> {
        match current.effect_type {
            // Effect Volume Slide (Dxy)
            0x04 => Self::it_volume_slide(current.effect_parameter).map(|tie| vec![tie]),
            // Pitch slide down (Exy)
            // Exx Pitch slide down
            // EFx Fine pitch slide down
            // EEx Extra fine pitch slide down
            0x05 => {
                let fx = current.effect_parameter;
                let nibble_high = (fx & 0xF0) >> 4;
                let nibble_low = fx & 0x0F;

                match (nibble_high, nibble_low) {
                    (0xE, low) => {
                        Some(vec![TrackImportEffect::PortamentoExtraFineDown(low as f32)])
                    }
                    (0xF, low) => Some(vec![TrackImportEffect::PortamentoFineDown(
                        4.0 * low as f32,
                    )]),
                    (_high, _low) => Some(vec![TrackImportEffect::PortamentoDown(4.0 * fx as f32)]),
                }
            }

            // Pitch slide up (Fxy)
            // Fxx Pitch Slide up
            // FFx Fine Pitch slide up
            // FEx Extra fine pitch slide up
            0x06 => {
                let fx = current.effect_parameter;
                let nibble_high = (fx & 0xF0) >> 4;
                let nibble_low = fx & 0x0F;

                match (nibble_high, nibble_low) {
                    (0xE, low) => Some(vec![TrackImportEffect::PortamentoExtraFineUp(
                        -(low as f32),
                    )]),
                    (0xF, low) => {
                        Some(vec![TrackImportEffect::PortamentoFineUp(-4.0 * low as f32)])
                    }
                    (_high, _low) => Some(vec![TrackImportEffect::PortamentoUp(-4.0 * fx as f32)]),
                }
            }

            // Slide to note (Gxx)
            0x07 => {
                let param = current.effect_parameter as f32;
                let speed = match freq_type {
                    FrequencyType::LinearFrequencies => 4.0 * param,
                    FrequencyType::AmigaFrequencies => param,
                };
                Some(vec![TrackImportEffect::TonePortamento(speed)])
            }

            // Effect Vibrato (Hxy)
            0x08 => {
                let param = current.effect_parameter;
                let speed = ((param & 0xF0) >> 4) as f32 / 64.0;
                let depth = (param & 0x0F) as f32 * 4.0 / 16.0;
                Some(vec![TrackImportEffect::Vibrato(speed, depth)])
            }

            // Effect Tremor (Ixy)
            0x09 => {
                let param = current.effect_parameter;
                let on_time = param >> 4;
                let off_time = param & 0x0F;
                Some(vec![TrackImportEffect::Tremor(
                    on_time as usize,
                    off_time as usize,
                )])
            }

            // Effect Arpeggio (Jxy)
            0x0A => {
                let param = current.effect_parameter;
                if param > 0 {
                    let v1 = (param >> 4) as usize;
                    let v2 = (param & 0x0F) as usize;
                    Some(vec![TrackImportEffect::Arpeggio(v1, v2)])
                } else {
                    None
                }
            }

            // Kxy = H00 Vibrato + Dxy Volume Slide
            0x0B => {
                if let Some(vste) = Self::it_volume_slide(current.effect_parameter) {
                    Some(vec![TrackImportEffect::Vibrato(0.0, 0.0), vste])
                } else {
                    Some(vec![
                        TrackImportEffect::Vibrato(0.0, 0.0),
                        TrackImportEffect::VolumeSlideN(0.0),
                    ])
                }
            }

            // Lxy = G00 Portamento + Dxy Volume slide
            0x0C => {
                if let Some(vste) = Self::it_volume_slide(current.effect_parameter) {
                    Some(vec![TrackImportEffect::TonePortamento(0.0), vste])
                } else {
                    Some(vec![
                        TrackImportEffect::TonePortamento(0.0),
                        TrackImportEffect::VolumeSlideN(0.0),
                    ])
                }
            }

            // Set Channel Volume (Mxx)
            0x0D => {
                let volume = current.effect_parameter.min(64) as f32 / 64.0;
                Some(vec![TrackImportEffect::ChannelVolume(volume)])
            }

            // Effect Channel Volume Slide (Nxy)
            // N0x Channel volume slide down
            // Nx0 Channel volume slide up
            // NFx Fine channel volume slide down
            // NxF Fine channel volume slide up
            //
            // Same test-order as Dxy (see `it_volume_slide`).
            0x0E => {
                let fx = current.effect_parameter;
                let nibble_high = (fx & 0xF0) >> 4;
                let nibble_low = fx & 0x0F;

                match (nibble_high, nibble_low) {
                    (f, 0) => Some(vec![TrackImportEffect::ChannelVolumeSlideN(
                        f as f32 / 64.0,
                    )]),
                    (0, low) => Some(vec![TrackImportEffect::ChannelVolumeSlideN(
                        -(low as f32) / 64.0,
                    )]),
                    (f, 0xF) => Some(vec![TrackImportEffect::ChannelVolumeSlide0(
                        f as f32 / 64.0,
                    )]),
                    (0xF, low) => Some(vec![TrackImportEffect::ChannelVolumeSlide0(
                        -(low as f32) / 64.0,
                    )]),
                    _ => None,
                }
            }

            // Effect Set Sample Offset (Oxx)
            0x0F => Some(vec![TrackImportEffect::InstrumentSampleOffset(
                current.effect_parameter as usize * 256,
            )]),

            // Effect Panning Slide (Pxx)
            // P0x Pan slide left (subtract from panning)
            // Px0 Pan slide right (add to panning)
            // PFx Fine pan slide left
            // PxF Fine pan slide right
            //
            // Same test-order constraint as Dxy (see `it_volume_slide`):
            // (f, 0xF) is placed before (0xF, f) so PFF resolves to
            // fine-right, not fine-left. Also the upper nibble is
            // shifted down before matching, otherwise a literal `0xF`
            // would never equal the unshifted upper half (0xF0).
            0x10 => {
                let fx = current.effect_parameter;
                let nibble_high = (fx & 0xF0) >> 4;
                let nibble_low = fx & 0x0F;

                match (nibble_high, nibble_low) {
                    (f, 0) => Some(vec![TrackImportEffect::PanningSlideN(f as f32 / 16.0)]),
                    (0, f) => Some(vec![TrackImportEffect::PanningSlideN(-(f as f32) / 16.0)]),
                    (f, 0xF) => Some(vec![TrackImportEffect::PanningSlide0(f as f32 / 16.0)]),
                    (0xF, f) => Some(vec![TrackImportEffect::PanningSlide0(-(f as f32) / 16.0)]),
                    _ => None,
                }
            }

            // Effect Retrigger Note (Qxy) -> Retriggers a note after y ticks with volume modifier x
            0x11 => {
                let param = current.effect_parameter;
                let vol = param >> 4;
                let speed = param & 0x0F;
                Some(vec![TrackImportEffect::NoteRetrigExtended(
                    speed as usize,
                    vol as usize,
                )])
            }

            // Effect Tremolo (Rxy)
            0x12 => {
                let param = current.effect_parameter;
                let speed = ((param & 0xF0) >> 4) as f32 / 64.0;
                let depth = (param & 0x0F) as f32 / 16.0;
                Some(vec![TrackImportEffect::Tremolo(speed, depth)])
            }

            // Effect Special Commands (Sxx)
            0x13 => {
                let fx = current.effect_parameter >> 4;
                let param = current.effect_parameter & 0xF;
                match fx {
                    // Set vibrato waveform (S3x)
                    0x3 => {
                        let waveform = match param & 0b0000_0011 {
                            1 => Waveform::RampDown,
                            2 => Waveform::Square,
                            3 => Waveform::Random,
                            _ => Waveform::Sine,
                        };
                        Some(vec![TrackImportEffect::VibratoWaveform(waveform, true)])
                    }
                    // Set tremolo waveform (S4x)
                    0x4 => {
                        let waveform = match param & 0b0000_0011 {
                            1 => Waveform::RampDown,
                            2 => Waveform::Square,
                            3 => Waveform::Random,
                            _ => Waveform::Sine,
                        };
                        Some(vec![TrackImportEffect::TremoloWaveform(waveform, true)])
                    }
                    // Set panbrello waveform (S5x)
                    0x5 => {
                        let waveform = match param & 0b0000_0011 {
                            1 => Waveform::RampDown,
                            2 => Waveform::Square,
                            3 => Waveform::Random,
                            _ => Waveform::Sine,
                        };
                        Some(vec![TrackImportEffect::PanbrelloWaveform(waveform, true)])
                    }
                    0x7 => {
                        match param {
                            // S70 Past note cut
                            // **Cuts** all notes playing as a result of New Note Actions on the current channel
                            0x0 => Some(vec![TrackImportEffect::NoteCut(0, true)]),
                            // S71 Past note off
                            // **Sends a Note Off** to all notes playing as a result of New Note Actions on the current channel
                            0x1 => Some(vec![TrackImportEffect::NoteOff(0, true)]),
                            // S72 Past note fade
                            // **Fades out** all notes playing as a result of New Note Actions on the current channel
                            0x2 => Some(vec![TrackImportEffect::NoteFadeOut(0, true)]),
                            // S73 Set NNA to cut
                            0x3 => Some(vec![TrackImportEffect::InstrumentNewNoteAction(
                                NewNoteAction::NoteCut,
                            )]),
                            // S74 Set NNA to continue
                            0x4 => Some(vec![TrackImportEffect::InstrumentNewNoteAction(
                                NewNoteAction::Continue,
                            )]),
                            // S75 Set NNA to off
                            0x5 => Some(vec![TrackImportEffect::InstrumentNewNoteAction(
                                NewNoteAction::NoteOff,
                            )]),
                            // S76 Set NNA to fade
                            0x6 => Some(vec![TrackImportEffect::InstrumentNewNoteAction(
                                NewNoteAction::NoteFadeOut,
                            )]),
                            // S77 Set volume envelope on
                            0x7 => Some(vec![TrackImportEffect::InstrumentVolumeEnvelope(true)]),
                            // S78 Set volume envelope off
                            0x8 => Some(vec![TrackImportEffect::InstrumentVolumeEnvelope(false)]),
                            // S79 Set panning envelope on
                            0x9 => Some(vec![TrackImportEffect::InstrumentPanningEnvelope(true)]),
                            // S7A Set panning envelope off
                            0xA => Some(vec![TrackImportEffect::InstrumentPanningEnvelope(false)]),
                            // S7B Set pitch envelope on
                            0xB => Some(vec![TrackImportEffect::InstrumentPitchEnvelope(true)]),
                            // S7C Set pitch envelope off
                            0xC => Some(vec![TrackImportEffect::InstrumentPitchEnvelope(false)]),
                            _ => None,
                        }
                    }
                    0x8 => Some(vec![TrackImportEffect::Panning((param as f32) / 15.0)]),
                    0x9 => Some(vec![TrackImportEffect::InstrumentSurround(param == 1)]),
                    0xA => Some(vec![TrackImportEffect::InstrumentSampleOffsetAddHigh(
                        param as usize * 65536,
                    )]),
                    0xC => Some(vec![TrackImportEffect::NoteCut(param as usize, false)]),
                    0xD => Some(vec![TrackImportEffect::NoteDelay(param as usize)]),
                    _ => None,
                }
            }

            // Effect Fine Vibrato (Uxy)
            //
            // ITTECH: Uxy speed = 4*x (same as Hxy), depth = y (4x
            // finer than Hxy's depth = y*4). Same speed-unit reasoning
            // as Hxy — 4x/256-entry-table = x/64 per tick, not x/256.
            0x15 => {
                let param = current.effect_parameter;
                let speed = ((param & 0xF0) >> 4) as f32 / 64.0;
                let depth = (param & 0x0F) as f32 / 16.0;
                Some(vec![TrackImportEffect::Vibrato(speed, depth)])
            }

            // Effect Set panning position (Xxx)
            // Full 8-bit range 0..=255 maps to [0.0, 1.0]; divide by
            // 255, not 256, otherwise 0xFF (full right) renders as
            // 0.9961 and true full-right becomes unreachable.
            0x18 => Some(vec![TrackImportEffect::Panning(
                (current.effect_parameter as f32) / 255.0,
            )]),

            // Effect Panbrello (Yxy)
            0x19 => {
                let param = current.effect_parameter;
                let speed = ((param & 0xF0) >> 4) as f32 / 256.0;
                let depth = (param & 0x0F) as f32 * 4.0 / 16.0;
                Some(vec![TrackImportEffect::Panbrello(speed, depth)])
            }

            _ => None,
        }
    }

    pub fn parse_global_it_effect(current: &PatternSlot) -> Option<GlobalEffect> {
        match current.effect_type {
            // Effect Set Song Speed (Axx)
            0x01 => {
                let speed = current.effect_parameter as usize;
                Some(GlobalEffect::Speed(speed))
            }
            // Effect Jump to Order (Bxx)
            0x02 => Some(GlobalEffect::PositionJump(
                current.effect_parameter as usize,
            )),
            // Effect Break to Row (Cxx)
            0x03 => Some(GlobalEffect::PatternBreak(
                current.effect_parameter as usize,
            )),

            // Effect Special Commands (Sxx)
            0x13 => {
                let fx = current.effect_parameter >> 4;
                let param = current.effect_parameter & 0xF;
                match fx {
                    // Extra delay of x frames
                    // Extends the current row by x ticks.
                    // Note: if multiple S6x commands are on the same row, the player should sum their parameters.
                    0x6 => Some(GlobalEffect::PatternDelay {
                        quantity: param as usize,
                        tempo: false,
                    }),
                    // SB0 Set loopback point
                    // SBx Loop from previous SB0 x times
                    0xB => Some(GlobalEffect::PatternLoop(param as usize)),
                    // SEx Pattern delay for x rows.
                    // All effects in the row will be repeated for each row delayed.
                    0xE => Some(GlobalEffect::PatternDelay {
                        quantity: param as usize,
                        tempo: true,
                    }),
                    // SFx Sets the current channel's active parametered macro.
                    0xF => Some(GlobalEffect::MidiMacro(MidiMacroType::SelectParametric(
                        param as usize,
                    ))),
                    _ => None,
                }
            }

            // Effect Set Tempo (Txx)
            // Txx Set tempo to xx
            // T0x Tempo slide down
            // T1x Tempo slide up
            0x14 => {
                let param = current.effect_parameter;
                let upper_nibble = param & 0xF0;
                let lower_nibble = param & 0x0F;

                match (upper_nibble, lower_nibble) {
                    (0, f) => Some(GlobalEffect::BpmSlide(-(f as isize))),
                    (1, f) => Some(GlobalEffect::BpmSlide(f as isize)),
                    _ => Some(GlobalEffect::Bpm(param as usize)),
                }
            }

            // Effect Set Global Volume (Vxx)
            // Per ITTECH GV ranges 0–128 (not 0–64 like the MOD/XM/S3M
            // families). Clamping to 64 would cap every IT module at
            // half global volume.
            0x16 => Some(GlobalEffect::Volume(
                (current.effect_parameter.min(128) as f32) / 128.0,
            )),

            // Effect Global Volume Slide (Wxy)
            // W0x Global volume slide down
            // Wx0 Global volume slide up
            // WFx Fine global volume slide down
            // WxF Fine global volume slide up
            //
            // Same test-order as Dxy (see `it_volume_slide`): test
            // (f, 0xF) before (0xF, f) so WFF resolves to fine-up.
            //
            // Scale is /128 (not /64 as in Dxy) because IT global
            // volume is 0–128, per ITTECH.
            0x17 => {
                let fx = current.effect_parameter;
                let nibble_high = (fx & 0xF0) >> 4;
                let nibble_low = fx & 0x0F;

                match (nibble_high, nibble_low) {
                    (f, 0) => Some(GlobalEffect::VolumeSlide {
                        speed: f as f32 / 128.0,
                        fine: false,
                    }),
                    (0, f) => Some(GlobalEffect::VolumeSlide {
                        speed: -(f as f32) / 128.0,
                        fine: false,
                    }),
                    (f, 0xF) => Some(GlobalEffect::VolumeSlide {
                        speed: f as f32 / 128.0,
                        fine: true,
                    }),
                    (0xF, f) => Some(GlobalEffect::VolumeSlide {
                        speed: -(f as f32) / 128.0,
                        fine: true,
                    }),
                    _ => None,
                }
            }

            // Effect Midi macro (Zxx)
            // Per ITTECH: `xx < 0x80` invokes the channel's
            // currently-selected parametric macro with `z` = xx;
            // `xx >= 0x80` invokes fixed macro `xx - 0x80`.
            0x1A => {
                let param = current.effect_parameter;
                if param & 0x80 == 0 {
                    Some(GlobalEffect::MidiMacro(MidiMacroType::Parametric(param)))
                } else {
                    Some(GlobalEffect::MidiMacro(MidiMacroType::Fixed {
                        idx: (param - 0x80) as usize,
                        z: param,
                    }))
                }
            }

            _ => None,
        }
    }

    fn parse_it_volume(freq_type: FrequencyType, slot: &PatternSlot) -> Option<TrackImportEffect> {
        // Set Volume (vxx)
        if slot.volume <= 64 {
            return Some(TrackImportEffect::ChannelVolume(slot.volume as f32 / 64.0));
        }

        // Set Panning (pxx)
        if (slot.volume & 0x7F) <= 64 {
            let p = (slot.volume & 0x7F) as f32 / 64.0;
            return Some(TrackImportEffect::Panning(p));
        }

        let temp = if slot.volume & 0x80 == 0 {
            // ['A' 'B' 'C' 'D' 'E' 'F'] -> [0 1 2 3 4 5] (A B C D E F effects)
            slot.volume - b'A'
        } else {
            // [11+k*128 12+k*128]=[6 7] (G H effects)
            (slot.volume & 0x7F) - 5
        };

        let fx = temp / 10;
        let param = temp % 10;

        match fx {
            // Fine Volume Slide Up (a0x)
            0x0 => return Some(TrackImportEffect::VolumeSlide0(param as f32 / 16.0)),
            // Fine Volume Slide Down (b0x)
            0x1 => return Some(TrackImportEffect::VolumeSlide0(-(param as f32) / 16.0)),
            // Volume Slide Up (c0x)
            0x2 => return Some(TrackImportEffect::VolumeSlideN(param as f32 / 16.0)),
            // Volume Slide Down (d0x)
            0x3 => return Some(TrackImportEffect::VolumeSlideN(-(param as f32) / 16.0)),
            // Portamento Down (e0x)
            // The rest of the crate's sign convention is:
            //   PortamentoDown receives positive speed (period grows).
            //   PortamentoUp   receives negative speed (period shrinks).
            // The main-column Exx / Fxx arms above already follow this;
            // the vol-column pair had them reversed, which made a
            // vol-column e0x bend UP and f0x bend DOWN.
            0x4 => return Some(TrackImportEffect::PortamentoDown((param as f32) / 16.0)),
            // Portamento Up (f0x)
            0x5 => return Some(TrackImportEffect::PortamentoUp(-(param as f32) / 16.0)),
            // Tone Portamento (g0x)
            0x6 => {
                let param = match param {
                    1 => 1,
                    2 => 4,
                    3 => 8,
                    4 => 16,
                    5 => 32,
                    6 => 64,
                    7 => 96,
                    8 => 128,
                    9 => 255,
                    _ => 0,
                } as f32;
                let speed = match freq_type {
                    FrequencyType::LinearFrequencies => 4.0 * param,
                    FrequencyType::AmigaFrequencies => param,
                };
                return Some(TrackImportEffect::TonePortamento(speed));
            }
            // Vibrato Depth (h0x)
            0x7 => {
                let depth = (param & 0x0F) as f32 * 4.0 / 16.0;
                return Some(TrackImportEffect::Vibrato(0.0, depth));
            }
            _ => {}
        }
        None
    }

    pub fn it_unpack_pattern(
        freq_type: FrequencyType,
        pattern: &[Vec<PatternSlot>],
    ) -> Vec<Vec<TrackImportUnit>> {
        pattern
            .iter()
            .map(|channel| {
                channel
                    .iter()
                    .map(|slot| {
                        let te = Self::parse_it_effect(freq_type, slot);
                        let ve = Self::parse_it_volume(freq_type, slot);
                        let cc = Self::parse_global_it_effect(slot);

                        let mut tiu = TrackImportUnit::default();
                        tiu.note = slot.note;
                        tiu.instrument = slot.instrument;
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
                    })
                    .collect::<Vec<TrackImportUnit>>() // Convertir les slots d'un canal
            })
            .collect::<Vec<Vec<TrackImportUnit>>>() // Convertir tous les canaux
    }
}
