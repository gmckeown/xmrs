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

/// ST3 masterflags that have to be resolved at import time.
///
/// Only flags whose effect can be fully absorbed into the emitted
/// `TrackImportEffect`s live here. Runtime-relevant flags (currently
/// `amigalimits`) are stored separately on [`ModuleFormat::S3m`] so
/// the replayer can consult them per-tick.
///
/// * `fastvolslide` (masterflags bit 64) — ST3 applies Dxy and Nxy
///   (channel vol slide) on *every* tick including tick 0 when this
///   is set. Without it, they only apply from tick 1. The importer
///   handles this by also emitting a `VolumeSlide0` / `ChannelVolumeSlide0`
///   alongside the usual `*SlideN`, so the tick-0 dose is added to
///   the regular tick>0 cascade.
///
/// * `oldstvib` (masterflags bit 1) — ST2-compatibility vibrato. The
///   depth is scaled `>>4` instead of `>>5` (H/K), and `>>6` instead
///   of `>>7` (U fine vibrato) — i.e. twice as deep. The importer
///   pre-multiplies the emitted `Vibrato`/`VibratoFine` depth by 2.
#[derive(Debug, Clone, Copy, Default)]
pub struct S3mImportFlags {
    pub fastvolslide: bool,
    pub oldstvib: bool,
}

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

    /// Convert a Dxy volume-slide param into zero, one, or two
    /// `TrackImportEffect`s. Returns a `Vec` (not `Option`) because
    /// two ST3 corner cases need a simultaneous tick-0 + tick>0 pair:
    ///
    /// * **`D0F` / `DF0`** — `infolo == 0xF, infohi == 0` and the
    ///   mirror. These look like fine slides but ST3 actually fires
    ///   `avol -= 0xF` (resp. `+=`) *unconditionally* on every tick
    ///   including tick 0 (`digcmd.c:s_volslide` lines 433-446, not
    ///   gated on `song.musiccount`). xmrs models this by emitting
    ///   both `VolumeSlide0` and `VolumeSlideN` with the same speed.
    ///
    /// * **fastvolslide** (masterflags bit 64) — when set, ST3 runs
    ///   regular `Dx0`/`D0y` on every tick too. Same fix: emit both
    ///   `VolumeSlide0` and `VolumeSlideN`.
    ///
    /// Genuine fine slides (`DxF`, `DFy` with y != 0) stay tick-0-
    /// only and keep a single `VolumeSlide0`.
    fn s3m_volume_slide(param: u8, flags: S3mImportFlags) -> Vec<TrackImportEffect> {
        let high = (param & 0xF0) >> 4;
        let low = param & 0x0F;

        let speed = |v: u8| -(v as f32) / 64.0;
        let speed_up = |v: u8| v as f32 / 64.0;

        match (high, low) {
            // D0F: slide down by 0xF every tick including tick 0.
            // Note: xmrs ordering `(0, 0xF)` has the low-nibble-F
            // priority, matching ST3's `if (infolo == 0xF) { if
            // (infohi == 0) avol -= infolo; ... }` branch.
            (0, 0xF) => vec![
                TrackImportEffect::VolumeSlide0(speed(0xF)),
                TrackImportEffect::VolumeSlideN(speed(0xF)),
            ],
            // DF0: slide up by 0xF every tick including tick 0.
            (0xF, 0) => vec![
                TrackImportEffect::VolumeSlide0(speed_up(0xF)),
                TrackImportEffect::VolumeSlideN(speed_up(0xF)),
            ],
            // DxF (fine up): tick 0 only.
            (f, 0xF) => vec![TrackImportEffect::VolumeSlide0(speed_up(f))],
            // DFy (fine down): tick 0 only.
            (0xF, f) => vec![TrackImportEffect::VolumeSlide0(speed(f))],
            // Regular D0y (slide down): tick > 0. With fastvolslide,
            // also tick 0.
            (0, f) => {
                if flags.fastvolslide {
                    vec![
                        TrackImportEffect::VolumeSlide0(speed(f)),
                        TrackImportEffect::VolumeSlideN(speed(f)),
                    ]
                } else {
                    vec![TrackImportEffect::VolumeSlideN(speed(f))]
                }
            }
            // Regular Dx0 (slide up): tick > 0. With fastvolslide,
            // also tick 0.
            (f, 0) => {
                if flags.fastvolslide {
                    vec![
                        TrackImportEffect::VolumeSlide0(speed_up(f)),
                        TrackImportEffect::VolumeSlideN(speed_up(f)),
                    ]
                } else {
                    vec![TrackImportEffect::VolumeSlideN(speed_up(f))]
                }
            }
            // Both nibbles non-zero and neither is 0xF: ST3 treats
            // this as illegal at tick 0 (returns) but still slides
            // down by infolo at tick > 0 (or every tick with
            // fastvolslide). Last-nibble-wins priority.
            (_, f) => {
                if flags.fastvolslide {
                    vec![
                        TrackImportEffect::VolumeSlide0(speed(f)),
                        TrackImportEffect::VolumeSlideN(speed(f)),
                    ]
                } else {
                    vec![TrackImportEffect::VolumeSlideN(speed(f))]
                }
            }
        }
    }

    /// Mirror of [`Self::s3m_volume_slide`] for Nxy (channel volume
    /// slide). Same D0F/DF0 corner cases, same fastvolslide behaviour.
    fn s3m_channel_volume_slide(param: u8, flags: S3mImportFlags) -> Vec<TrackImportEffect> {
        let high = (param & 0xF0) >> 4;
        let low = param & 0x0F;

        let speed = |v: u8| -(v as f32) / 64.0;
        let speed_up = |v: u8| v as f32 / 64.0;

        match (high, low) {
            (0, 0xF) => vec![
                TrackImportEffect::ChannelVolumeSlide0(speed(0xF)),
                TrackImportEffect::ChannelVolumeSlideN(speed(0xF)),
            ],
            (0xF, 0) => vec![
                TrackImportEffect::ChannelVolumeSlide0(speed_up(0xF)),
                TrackImportEffect::ChannelVolumeSlideN(speed_up(0xF)),
            ],
            (f, 0xF) => vec![TrackImportEffect::ChannelVolumeSlide0(speed_up(f))],
            (0xF, f) => vec![TrackImportEffect::ChannelVolumeSlide0(speed(f))],
            (0, f) => {
                if flags.fastvolslide {
                    vec![
                        TrackImportEffect::ChannelVolumeSlide0(speed(f)),
                        TrackImportEffect::ChannelVolumeSlideN(speed(f)),
                    ]
                } else {
                    vec![TrackImportEffect::ChannelVolumeSlideN(speed(f))]
                }
            }
            (f, 0) => {
                if flags.fastvolslide {
                    vec![
                        TrackImportEffect::ChannelVolumeSlide0(speed_up(f)),
                        TrackImportEffect::ChannelVolumeSlideN(speed_up(f)),
                    ]
                } else {
                    vec![TrackImportEffect::ChannelVolumeSlideN(speed_up(f))]
                }
            }
            (_, f) => {
                if flags.fastvolslide {
                    vec![
                        TrackImportEffect::ChannelVolumeSlide0(speed(f)),
                        TrackImportEffect::ChannelVolumeSlideN(speed(f)),
                    ]
                } else {
                    vec![TrackImportEffect::ChannelVolumeSlideN(speed(f))]
                }
            }
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
        flags: S3mImportFlags,
        current: &PatternSlot,
    ) -> Option<Vec<TrackImportEffect>> {
        let param = current.effect_parameter;
        // Scale factor for pitch slides. ST3's raw `param` operates
        // on ST3's `aspd` which is 4× finer than xmrs's Amiga period,
        // so in Amiga mode we use `1.0` and in Linear mode (which
        // has 4× finer period resolution than xmrs's Amiga periods)
        // we use `4.0`. This scale also applies to the fine/extra-
        // fine sub-commands: previously only the main slide respected
        // it, which left E/F high-nibble-F fine slides 4× too fast
        // in Amiga mode.
        let main_scale = match freq_type {
            FrequencyType::LinearFrequencies => 4.0,
            FrequencyType::AmigaFrequencies => 1.0,
        };
        // oldstvib (masterflags bit 1) doubles the vibrato depth —
        // ST3 uses a `>> 4` shift under this flag instead of the
        // regular `>> 5`, giving 2× the depth. The same applies to
        // the fine vibrato U (`>> 6` vs `>> 7`). Absorbed into the
        // emitted depth so the player stays agnostic.
        let vib_depth_scale: f32 = if flags.oldstvib { 2.0 } else { 1.0 };
        match current.effect_type {
            // D - Volume slide
            4 => {
                let v = Self::s3m_volume_slide(param, flags);
                if v.is_empty() {
                    None
                } else {
                    Some(v)
                }
            }

            // E - Portamento down
            5 => {
                let high = (param & 0xF0) >> 4;
                let low = param & 0x0F;
                match (high, low) {
                    // EEy: extra-fine slide down (ST3: `info & 0xF`
                    // subtracted from aspd at tick 0 only). 1x scale
                    // in ST3 units = `main_scale` in xmrs period
                    // units.
                    (0xE, f) => Some(vec![TrackImportEffect::PortamentoExtraFineDown(
                        main_scale * f as f32,
                    )]),
                    // EFy: "extra" fine slide down (4x scale, once
                    // at tick 0).
                    (0xF, f) => Some(vec![TrackImportEffect::PortamentoFineDown(
                        main_scale * 4.0 * f as f32,
                    )]),
                    _ => Some(vec![TrackImportEffect::PortamentoDown(
                        main_scale * param as f32,
                    )]),
                }
            }

            // F - Portamento up
            6 => {
                let high = (param & 0xF0) >> 4;
                let low = param & 0x0F;
                match (high, low) {
                    (0xE, f) => Some(vec![TrackImportEffect::PortamentoExtraFineUp(
                        -main_scale * f as f32,
                    )]),
                    (0xF, f) => Some(vec![TrackImportEffect::PortamentoFineUp(
                        -main_scale * 4.0 * f as f32,
                    )]),
                    _ => Some(vec![TrackImportEffect::PortamentoUp(
                        -main_scale * param as f32,
                    )]),
                }
            }

            // G - Tone portamento
            7 => {
                let speed = main_scale * param as f32;
                Some(vec![TrackImportEffect::TonePortamento(speed)])
            }

            // H - Vibrato
            8 => {
                let speed = ((param & 0xF0) >> 4) as f32 / 64.0;
                let depth = (param & 0x0F) as f32 / 16.0 * vib_depth_scale;
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
                let mut v = vec![TrackImportEffect::Vibrato(0.0, 0.0)];
                v.extend(Self::s3m_volume_slide(param, flags));
                Some(v)
            }

            // L - Tone portamento + Volume slide
            12 => {
                let mut v = vec![TrackImportEffect::TonePortamento(0.0)];
                v.extend(Self::s3m_volume_slide(param, flags));
                Some(v)
            }

            // M - Set channel volume
            13 => Some(vec![TrackImportEffect::ChannelVolume(
                param.min(64) as f32 / 64.0,
            )]),

            // N - Channel volume slide
            14 => {
                let v = Self::s3m_channel_volume_slide(param, flags);
                if v.is_empty() {
                    None
                } else {
                    Some(v)
                }
            }

            // O - Sample offset
            15 => Some(vec![TrackImportEffect::InstrumentSampleOffset(
                param as usize * 256,
            )]),

            // P - Panning slide
            16 => Self::s3m_panning_slide(param).map(|e| vec![e]),

            // Q - Retrigger. ST3's Qxy uses a LUT that differs from
            // FT2's Rxy on nibble 6 (`10/16` vs `11/16`); the
            // dedicated `NoteRetrigExtendedS3m` variant keeps that
            // split out of the generic converter.
            17 => {
                let vol = (param >> 4) as usize;
                let speed = (param & 0x0F) as usize;
                Some(vec![TrackImportEffect::NoteRetrigExtendedS3m(speed, vol)])
            }

            // R - Tremolo
            18 => {
                let speed = ((param & 0xF0) >> 4) as f32 / 64.0;
                let depth = (param & 0x0F) as f32 / 16.0 * vib_depth_scale;
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
                let depth = (param & 0x0F) as f32 / 64.0 * vib_depth_scale;
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

    pub fn s3m_unpack(
        freq_type: FrequencyType,
        flags: S3mImportFlags,
        current: &PatternSlot,
    ) -> TrackImportUnit {
        let te = Self::s3m_walk_effect(freq_type, flags, current);
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
        flags: S3mImportFlags,
        pattern: &[Vec<PatternSlot>],
    ) -> Vec<Vec<TrackImportUnit>> {
        pattern
            .iter()
            .map(|row| {
                row.iter()
                    .map(|slot| Self::s3m_unpack(freq_type, flags, slot))
                    .collect()
            })
            .collect()
    }
}
