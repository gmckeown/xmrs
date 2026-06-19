//! S3M Effect Parser.
//!
//! Converts S3M effects directly to TrackImportEffect / NavigationEffect /
//! SongLevelEffect / MidiMacroType (via the GlobalEffect parser-side
//! wrapper), without going through XM effect numbering.

use alloc::vec;
use alloc::vec::Vec;

use crate::core::fixed::fixed::{Q15, Q8_8};
use crate::core::fixed::from_tracker::lfo_speed_from_nibble_64;
use crate::core::fixed::units::{ChannelVolume, Finetune, Panning, PitchDelta, Volume};
use crate::prelude::*;
use crate::tracker::import::effect::TrackImportEffect;
use crate::tracker::import::memory::S3mImportFlags;
use crate::tracker::import::patternslot::PatternSlot;
use crate::tracker::import::unit::TrackImportUnit;

// `S3mImportFlags` — the two-bool POD wrapping ST3 masterflag bits
// `fastvolslide` (bit 64) and `oldstvib` (bit 1) — is defined on the
// shared `ImportMemory` helper (see `crate::tracker::import::memory`)
// rather than here, so it stays compiled in every build that enables
// any importer (not just `import_s3m`). Its fields carry the same
// semantics described there: the importer pre-absorbs ST3's tick-0
// volume-slide quirk by co-emitting `VolumeSlide0` / `ChannelVolumeSlide0`
// alongside the cascade slides, and pre-scales vibrato depth ×2 for
// ST2-style deep vibrato. Runtime-relevant flags (e.g. `amigalimits`)
// are carried separately on `ModuleFormat::S3m` so the replayer can
// consult them per-tick.

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

        let speed = |v: u8| Q15::from_ratio(-(v as i32), 64);
        let speed_up = |v: u8| Q15::from_ratio(v as i32, 64);

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

        let speed = |v: u8| Q15::from_ratio(-(v as i32), 64);
        let speed_up = |v: u8| Q15::from_ratio(v as i32, 64);

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

    /// Pxx panning slide — OpenMPT / extended-S3M convention.
    ///
    /// ST3 does NOT natively implement Pxx (both `soncejmp[16]` and
    /// `sotherjmp[16]` in `digcmd.c` point to `s_ret`, a no-op).
    /// xmrs supports Pxx as an OpenMPT/extended-S3M extension using
    /// the standard FT2 panning-slide encoding:
    ///
    /// * `P0y` → regular slide left by `y/16` per tick > 0
    /// * `Px0` → regular slide right by `x/16` per tick > 0
    /// * `PxF` → fine slide right by `x/16` at tick 0
    /// * `PFy` → fine slide left by `y/16` at tick 0
    /// * `P00` → no-op (memory handled at [`crate::tracker::import::memory`])
    /// * `P0F` / `PF0` → no-op (fine slide with amount 0 — legitimate
    ///   under FT2 semantics, see below)
    ///
    /// **Why this does NOT mirror the D0F / DF0 double-emission
    /// pattern in [`Self::s3m_volume_slide`]:** the volume slide
    /// corner case exists because ST3 natively implements Dxy AND
    /// breaks from FT2 convention — ST3's `s_volslide` fires every
    /// tick (including tick 0), so `D0F` / `DF0` encode a full-range
    /// per-tick slide rather than FT2's tick-0-only fine slide. For
    /// Pxx there is no native ST3 behaviour to reconcile, so the
    /// FT2 semantics apply unchanged: an amount of 0 is a no-op on
    /// whichever side carries it. The `fastvolslide` masterflag is
    /// intentionally NOT applied here either, for the same reason.
    fn s3m_panning_slide(param: u8) -> Option<TrackImportEffect> {
        let high = (param & 0xF0) >> 4;
        let low = param & 0x0F;

        match (high, low) {
            (0xF, f) => Some(TrackImportEffect::PanningSlide0(Q15::from_ratio(
                -(f as i32),
                16,
            ))),
            (f, 0xF) => Some(TrackImportEffect::PanningSlide0(Q15::from_ratio(
                f as i32, 16,
            ))),
            (0, f) => Some(TrackImportEffect::PanningSlideN(Q15::from_ratio(
                -(f as i32),
                16,
            ))),
            (f, 0) => Some(TrackImportEffect::PanningSlideN(Q15::from_ratio(
                f as i32, 16,
            ))),
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
        let main_scale: i16 = match freq_type {
            FrequencyType::LinearFrequencies => 4,
            FrequencyType::AmigaFrequencies => 1,
        };
        // oldstvib (masterflags bit 1) doubles the vibrato depth —
        // ST3 uses a `>> 4` shift under this flag instead of the
        // regular `>> 5`, giving 2× the depth. The same applies to
        // the fine vibrato U (`>> 6` vs `>> 7`). Absorbed into the
        // emitted depth (Q8.8 raw multiplier) so the player stays
        // agnostic.
        let vib_depth_scale: i16 = if flags.oldstvib { 2 } else { 1 };
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
                        main_scale * f as i16,
                    )]),
                    // EFy: "extra" fine slide down (4x scale, once
                    // at tick 0).
                    (0xF, f) => Some(vec![TrackImportEffect::PortamentoFineDown(
                        main_scale * 4 * f as i16,
                    )]),
                    _ => Some(vec![TrackImportEffect::PortamentoDown(
                        main_scale * param as i16,
                    )]),
                }
            }

            // F - Portamento up
            6 => {
                let high = (param & 0xF0) >> 4;
                let low = param & 0x0F;
                match (high, low) {
                    (0xE, f) => Some(vec![TrackImportEffect::PortamentoExtraFineUp(
                        -main_scale * f as i16,
                    )]),
                    (0xF, f) => Some(vec![TrackImportEffect::PortamentoFineUp(
                        -main_scale * 4 * f as i16,
                    )]),
                    _ => Some(vec![TrackImportEffect::PortamentoUp(
                        -main_scale * param as i16,
                    )]),
                }
            }

            // G - Tone portamento
            7 => {
                let speed: i16 = main_scale * param as i16;
                Some(vec![TrackImportEffect::TonePortamento(speed)])
            }

            // H - Vibrato. depth in semitones: `y/16 * vib_depth_scale`.
            // Q8.8 raw: `y * 16 * vib_depth_scale`.
            8 => {
                let speed = lfo_speed_from_nibble_64((param & 0xF0) >> 4);
                let l = (param & 0x0F) as i16;
                let depth = PitchDelta::from_q8_8_i16(l * 16 * vib_depth_scale);
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
                let mut v = vec![TrackImportEffect::Vibrato(Q8_8::ZERO, PitchDelta::ZERO)];
                v.extend(Self::s3m_volume_slide(param, flags));
                Some(v)
            }

            // L - Tone portamento + Volume slide
            //
            // The emitted `TonePortamento(0)` is NOT a no-op — it
            // signals "load from memory" in `ImportMemory`. Since G
            // writes to the same `TONE_PORTAMENTO` slot with its
            // speed (when `param > 0`) and L only reads from it, the
            // pair ends up sharing the slot exactly like ST3 shares
            // `alasteff1` between `s_toneslide` (G) and the
            // `KxyLxxVolslideType == 1` branch called from
            // `s_tonevol` (digcmd.c:s_toneslide lines 473-475).
            // L intentionally never writes to the slot — if a module
            // plays L before any G has set the memory, ST3 uses its
            // initial-zero `alasteff1` (no slide), which xmrs
            // reproduces via the zero-initialised slot.
            12 => {
                let mut v = vec![TrackImportEffect::TonePortamento(0)];
                v.extend(Self::s3m_volume_slide(param, flags));
                Some(v)
            }

            // M - Set channel volume
            13 => Some(vec![TrackImportEffect::ChannelVolume(
                ChannelVolume::from_byte_64(param.min(64)),
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

            // R - Tremolo. depth in volume amplitude: `y/16 *
            // vib_depth_scale`. Q15 raw: `y * 2048 *
            // vib_depth_scale`, saturating at Q15 max.
            18 => {
                let speed = lfo_speed_from_nibble_64((param & 0xF0) >> 4);
                let l = (param & 0x0F) as i32;
                let depth = Q15::from_i32_unsigned_sat(l * 2048 * vib_depth_scale as i32);
                Some(vec![TrackImportEffect::Tremolo(speed, depth)])
            }

            // S - Special sub-commands
            19 => {
                let sub = param >> 4;
                let val = param & 0x0F;
                match sub {
                    0x1 => Some(vec![TrackImportEffect::Glissando(val != 0)]),
                    0x2 => Some(vec![TrackImportEffect::InstrumentFineTune(
                        // S2y: same FT2-style nibble-centred-at-8
                        // mapping as XM's E5y. `(val - 8) / 8 →
                        // [-1, +1]`.
                        Finetune::from_ratio(val as i32 - 8, 8),
                    )]),
                    0x3 => {
                        let wf = match val & 3 {
                            1 => Waveform::BipolarRampDown,
                            2 => Waveform::BipolarSquare,
                            _ => Waveform::BipolarSine,
                        };
                        Some(vec![TrackImportEffect::VibratoWaveform(wf, (val & 4) == 0)])
                    }
                    0x4 => {
                        let wf = match val & 3 {
                            1 => Waveform::BipolarRampDown,
                            2 => Waveform::BipolarSquare,
                            _ => Waveform::BipolarSine,
                        };
                        Some(vec![TrackImportEffect::TremoloWaveform(wf, (val & 4) == 0)])
                    }
                    0x8 => Some(vec![TrackImportEffect::Panning(
                        // S8y: 4-bit pan, `0..=15`. Was `val/15`.
                        Panning::from_ratio(val as i32, 15),
                    )]),
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

            // U - Fine vibrato (4x finer depth than H). Q8.8 raw:
            // `y/64 * 256 = y * 4 * vib_depth_scale`.
            21 => {
                let speed = lfo_speed_from_nibble_64((param & 0xF0) >> 4);
                let l = (param & 0x0F) as i16;
                let depth = PitchDelta::from_q8_8_i16(l * 4 * vib_depth_scale);
                Some(vec![TrackImportEffect::VibratoFine(speed, depth)])
            }

            // X - Set panning
            24 => Some(vec![TrackImportEffect::Panning(Panning::from_byte_255(
                param,
            ))]),

            _ => None,
        }
    }

    // --- Volume column (raw S3M: 0-64, 0x80-0xC0 panning, or absent) ---

    fn s3m_walk_volume(current: &PatternSlot) -> Option<TrackImportEffect> {
        match current.volume {
            v if v <= 64 => Some(TrackImportEffect::Volume(Volume::from_byte_64(v), 0)),
            // MPT-style S3M panning via the volume column. 0x80 = hard left,
            // 0xC0 = hard right; 65 discrete steps in between. We scale to
            // Q1.15 panning via [`Panning::from_byte_64`].
            v if (0x80..=0xC0).contains(&v) => {
                Some(TrackImportEffect::Panning(Panning::from_byte_64(v - 0x80)))
            }
            _ => None,
        }
    }

    // --- Global effects ---

    fn s3m_walk_global_effect(current: &PatternSlot) -> Option<GlobalEffect> {
        let param = current.effect_parameter;
        match current.effect_type {
            // A - Set speed. S3M has *separate* speed (A) and tempo (T)
            // commands, so — unlike the MOD/XM combined `Fxx`, where
            // `<= 0x1F` means speed and `>= 0x20` means tempo — the FULL
            // non-zero param range is a speed here; there is no 0x1F split.
            // Schism `FX_SPEED` (player/effects.c): `if (firsttick && param)
            // current_speed = param;` — any non-zero value sets the speed.
            // (The old `param <= 0x1F` cap silently dropped e.g. `A30`
            // speed-48 rows, shortening them and de-syncing the whole song.)
            1 => {
                if param > 0 {
                    Some(SongLevelEffect::Speed(param as usize).into())
                } else {
                    None
                }
            }
            // B - Jump to order
            2 => Some(NavigationEffect::PositionJump(param as usize).into()),
            // C - Break to row
            3 => Some(NavigationEffect::PatternBreak(param as usize).into()),
            // S - Special sub-commands (global)
            19 => {
                let sub = param >> 4;
                let val = param & 0x0F;
                match sub {
                    0xB => Some(NavigationEffect::PatternLoop(val as usize).into()),
                    0xE => Some(
                        NavigationEffect::PatternDelay {
                            quantity: val as usize,
                            tempo: true,
                        }
                        .into(),
                    ),
                    _ => None,
                }
            }
            // T - Set tempo
            20 => {
                if param >= 0x20 {
                    Some(SongLevelEffect::Bpm(param as usize).into())
                } else {
                    None
                }
            }
            // V - Set global volume
            22 => {
                if param <= 0x40 {
                    Some(SongLevelEffect::Volume(Volume::from_byte_64(param)).into())
                } else {
                    None
                }
            }
            // W - Global volume slide
            23 => {
                let high = (param & 0xF0) >> 4;
                let low = param & 0x0F;
                match (high, low) {
                    (0xF, f) => Some(
                        SongLevelEffect::VolumeSlide {
                            speed: Q15::from_ratio(-(f as i32), 64),
                            fine: true,
                        }
                        .into(),
                    ),
                    (f, 0xF) => Some(
                        SongLevelEffect::VolumeSlide {
                            speed: Q15::from_ratio(f as i32, 64),
                            fine: true,
                        }
                        .into(),
                    ),
                    (0, f) => Some(
                        SongLevelEffect::VolumeSlide {
                            speed: Q15::from_ratio(-(f as i32), 64),
                            fine: false,
                        }
                        .into(),
                    ),
                    (f, 0) => Some(
                        SongLevelEffect::VolumeSlide {
                            speed: Q15::from_ratio(f as i32, 64),
                            fine: false,
                        }
                        .into(),
                    ),
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
            tiu.push_global(g);
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
