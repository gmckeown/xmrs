//! `TrackImportEffect` — the format-agnostic effect representation
//! every importer emits. Carries the parsed, normalised shape of a
//! pattern-cell effect column before any memory back-fill is
//! applied; `super::memory` resolves the empty-parameter cases
//! against per-channel registers, and the build / extract pipelines
//! consume the result.

use crate::core::fixed::fixed::{Q15, Q8_8};
use crate::core::fixed::from_tracker::{retrig_mul_ft2, retrig_mul_s3m, retrig_volume_delta_q15};
use crate::core::fixed::units::{ChannelVolume, Finetune, Panning, PitchDelta, RetrigMul, Volume};
use crate::prelude::*;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// FT2/ST3 multi-retrig volume modifier table.
///
/// FT2 (`doMultiRetrig`) and ST3 (`s_retrig` via `retrigvoladd`) share
/// the same shape: nibbles 1–5 subtract 1/2/4/8/16, nibbles 9–D add
/// the same, and nibbles 6/7/E/F are multiplicative. The two formats
/// disagree on the nibble-6 constant only — FT2 uses `11/16`, ST3
/// `10/16` — encoded in [`retrig_mul_ft2`] vs
/// [`retrig_mul_s3m`].
///
/// Additive deltas use `1/64` as the unit (xmrs's normalised volume
/// scale, where `Volume::SILENT` is 0 and `Volume::FULL` is 1).
fn note_retrig_volume_modifier_ft2(nibble: usize) -> NoteRetrigOperator {
    note_retrig_volume_modifier_dispatch(nibble, retrig_mul_ft2(nibble as u8))
}

fn note_retrig_volume_modifier_s3m(nibble: usize) -> NoteRetrigOperator {
    note_retrig_volume_modifier_dispatch(nibble, retrig_mul_s3m(nibble as u8))
}

#[inline]
fn note_retrig_volume_modifier_dispatch(nibble: usize, mul: RetrigMul) -> NoteRetrigOperator {
    match nibble {
        1..=5 | 9..=0xD => NoteRetrigOperator::Sum(retrig_volume_delta_q15(nibble as u8)),
        6 | 7 | 0xE | 0xF => NoteRetrigOperator::Mul(mul),
        _ => NoteRetrigOperator::None,
    }
}

/// Importer-side, format-agnostic effect representation.
///
/// Sits between the raw `(command_byte, parameter_byte)` pair every
/// tracker format encodes and the runtime [`TrackEffect`] / lane
/// representation. Every per-format effect parser (xm /  s3m / it /
/// mod / sid) emits a stream of `TrackImportEffect` values; the
/// crate-private `super::memory` helper resolves empty-parameter
/// slots against per-channel registers, then [`super::build`] /
/// [`super::extract`] consume the result to produce the final DAW
/// layer.
///
/// This step is mandatory to manage the effects memory before
/// `TrackEffect` materialisation.
///
/// [`TrackEffect`]: crate::core::effect::TrackEffect
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum TrackImportEffect {
    /// `(1st halftone, 2nd halftone)`
    /// F / XM0=0(0), XM=0(0)
    Arpeggio(usize, usize),

    /// `value`, set the Channel Volume.
    /// Was `f32` — Q1.15 in `[0, 1]`.
    ChannelVolume(ChannelVolume),

    /// Slide the Channel Volume. Speed is signed Q1.15
    /// delta-per-tick (was `f32`).
    /// Channel Volume
    ChannelVolumeSlide0(Q15),
    ChannelVolumeSlideN(Q15),

    /// `bool`, round to the nearest halftone when using effects
    /// F / XM0=0xE3(E3)
    Glissando(bool),

    /// `tune`, this effet should be used together with a note.
    /// It will cause another fine-tune value to be used.
    /// F / XM0=0xE5(E5). Was `f32` — Q1.15 in `[-1, +1]`.
    InstrumentFineTune(Finetune),

    /// `nna` Change Instrument New Note Action
    InstrumentNewNoteAction(NewNoteAction),

    /// `position`, change the panning envelope position
    /// P / XM0=0x15(L) (if instr.sustained)
    InstrumentPanningEnvelopePosition(usize),
    InstrumentPanningEnvelope(bool),

    InstrumentPitchEnvelope(bool),

    /// `offset`, this effect should be used together with a note
    /// The sample will be played from `offset` instead of zero
    /// XM0=9(9)
    InstrumentSampleOffset(usize),
    InstrumentSampleOffsetAddHigh(usize),

    /// `surround`
    InstrumentSurround(bool),

    /// `position`, change the volume envelope position
    /// V / XM0=0x15(L)
    InstrumentVolumeEnvelopePosition(usize),
    InstrumentVolumeEnvelope(bool),

    /// `(tick, past)`, cut the note at the specified tick.
    /// if past is true, do it for past note too
    /// Note that it will only set the volume to zero, and the sample will still be played.
    /// V / XM=0xEC(EC)
    NoteCut(usize, bool),

    /// `ticks`, this effect will delay the note the selected number of ticks
    /// XM0=0xED(ED), XM=0xED(ED)
    NoteDelay(usize),

    /// `(tick, past)`, fadeout the note at the specified tick
    /// if past is true, do it for past note too
    NoteFadeOut(usize, bool),

    /// `(tick, past)`, this effect will trigger a "Note Off" at the specified tick
    /// if past is true, do it for past note too
    /// XM0=0x14(K), XM=0x14(K)
    NoteOff(usize, bool),

    /// `interval`, this effect will retrigs the note with the specified interval
    /// V / XM0=0xE9(E9), XM=0xE9(E9)
    NoteRetrig(usize),

    /// `(interval, volume modifier)`
    /// Extended version of the `TrackImportEffectffect::NoteRetrig` effect
    /// V / XM0=0x1B(R), XM=0x1B(R)
    NoteRetrigExtended(usize, usize),

    /// `(interval, volume modifier)` — ST3/S3M flavour of
    /// [`Self::NoteRetrigExtended`] (command Qxy). The LUT differs
    /// from FT2's on nibble 6 only: FT2 uses `vol * 11/16` (from
    /// `doMultiRetrig` in `ft2_replayer.c`), ST3 uses `vol * 10/16 =
    /// vol * 5/8` (`retrigvoladd[22] = 10` in `digdata.c`). All other
    /// nibbles share identical semantics across the two formats, so
    /// keeping a separate variant only for this bucket is wasteful,
    /// but variant-level splitting is the simplest way to keep the
    /// format-specific constant out of the generic converter.
    NoteRetrigExtendedS3m(usize, usize),

    /// `(speed, depth)`, set Panbrello.
    /// `speed` Q8.8 cycles/tick, `depth` `Q15` signed amplitude
    /// (was `f32`).
    Panbrello(Q8_8, Q15),

    /// `(waveform, retrig)`, change Panbrello waveform.
    /// `retrig` to true to retrig when a new instrument is played.
    PanbrelloWaveform(Waveform, bool),

    /// `position` Q1.15 in `[0, 1]`, sets the panning position
    /// for the channel. `0` is fully left, `1` fully right.
    /// P / XM0=8(8), XM0=v0xC(vP). Was `f32`.
    Panning(Panning),

    /// Slide the panning position. Speed is signed Q1.15
    /// delta-per-tick (was `f32`).
    /// P / XM0=0x19(P), XM=0x19(P), XM0=v0xD(L), XM0=v0xE(R)
    PanningSlide0(Q15),
    PanningSlideN(Q15),

    /// `speed` (signed period units per tick — was `f32`).
    /// F / XM0=1(1), XM=1(1), XM0=2(2), XM=2(2), XM0=0xE1(E1), XM0=0xE2(E2), XM0=0x21(X1), XM=0x21(X2)
    PortamentoUp(i16),
    PortamentoDown(i16),

    /// `speed`
    /// F / XM0=0xE1(E1), XM0=0xE2(E2)
    PortamentoFineUp(i16),
    PortamentoFineDown(i16),

    /// `speed`
    /// F / XM0=0x21(X1), XM=0x21(X2)
    PortamentoExtraFineUp(i16),
    PortamentoExtraFineDown(i16),

    /// `speed`, see `ControlChangeEffect::Glissando` to round to the nearest halftone
    /// F / XM0=3(3), XM=3(3), XM0=5x?(5), XM=5x?(5), XM0=v0xF(vM), XM=v0xF(vM)
    TonePortamento(i16),
    TonePortamentoFxVol(i16),

    /// `(speed, depth)` Q8.8 cycles/tick, Q15 signed amplitude
    /// (was `f32`). See `ControlChangeEffect::Waveform` to
    /// change waveform.
    /// V / XM0=7(7), XM=7(7)
    Tremolo(Q8_8, Q15),

    /// `(waveform, retrig)`, change Tremolo waveform.
    /// `retrig` to true to retrig when a new instrument is played.
    /// V / XM0=0xE7(E7)
    TremoloWaveform(Waveform, bool),

    /// `(On time, Off time)`
    /// This weird effect will set the volume to zero during `Off time` number of ticks
    /// V / XM0=0x1D(T), XM=0x1D(T)
    Tremor(usize, usize),

    /// `(speed, depth)`, set Vibrato. `speed` Q8.8 cycles/tick,
    /// `depth` semitones (PitchDelta, Q8.8). Was `f32`.
    /// F / XM0=4(4), XM=4(4), XM0=6x?(6), XM=6x?(6), XM=v0xB(vV)
    Vibrato(Q8_8, PitchDelta),

    /// `(speed, depth)`, set Vibrato
    /// IT with memory
    VibratoFine(Q8_8, PitchDelta),

    /// `speed`, set Vibrato speed (Q8.8 cycles/tick).
    /// F / XM0=v0xA(S)
    VibratoSpeed(Q8_8),

    /// `depth`, set Vibrato depth (PitchDelta, semitones Q8.8).
    /// F / XM0=v0xB(V)
    VibratoDepth(PitchDelta),
    VibratoDepthFxVol(PitchDelta),

    /// `(waveform, retrig)`, change Vibrato waveform.
    /// `retrig` to true to retrig when a new instrument is played.
    /// F / XM0=0xE4(E4)
    VibratoWaveform(Waveform, bool),

    /// `(value, tick)`, sets the current volume at the current tick.
    /// V / XM0=C(C), XM0=vV1..5(V). `value` was `f32` — Q1.15
    /// in `[0, 1]`.
    Volume(Volume, usize),

    /// Slide the current volume up or down. Speed is signed
    /// Q1.15 delta-per-tick (was `f32`).
    /// V / XM0=5?y(5), XM=5?y(5), XM0=6?y(6), XM=6?y(6), XM=0xA(A), XM0=0xEA(EA), XM0=0xEB(EB), XM=v6(v6), XM=v7(v7), XM0=v8(v8), XM0=v9(v9)
    VolumeSlide0(Q15),
    VolumeSlideN(Q15),
}

impl TrackImportEffect {
    /// Map a single [`TrackImportEffect`] to its runtime
    /// [`TrackEffect`] counterpart, or return `None` when the variant
    /// has migrated to an [`AutomationLane`] and no longer reaches
    /// the runtime cell (DAW migration Phase 3c.3).
    ///
    /// The lane-bound families ignored here (Vibrato* / Tremolo* /
    /// Panbrello* / Portamento* / TonePortamento* / VolumeSlide* /
    /// ChannelVolumeSlide* / PanningSlide*) are consumed directly
    /// from `&[TrackImportEffect]` by
    /// [`crate::tracker::import::extract`].
    ///
    /// [`AutomationLane`]: crate::core::daw::automation::AutomationLane
    fn to_track_effect(self) -> Option<TrackEffect> {
        match self {
            TrackImportEffect::Arpeggio(h1, h2) => {
                if h1 != 0 || h2 != 0 {
                    Some(TrackEffect::Arpeggio {
                        half1: h1,
                        half2: h2,
                    })
                } else {
                    None
                }
            }
            TrackImportEffect::Panning(pos) => Some(TrackEffect::Panning(pos)),
            TrackImportEffect::ChannelVolume(pos) => Some(TrackEffect::ChannelVolume(pos)),
            TrackImportEffect::Glissando(value) => Some(TrackEffect::Glissando(value)),
            TrackImportEffect::InstrumentFineTune(tune) => {
                Some(TrackEffect::InstrumentFineTune(tune))
            }
            TrackImportEffect::InstrumentNewNoteAction(nna) => {
                Some(TrackEffect::InstrumentNewNoteAction(nna))
            }
            TrackImportEffect::InstrumentPanningEnvelopePosition(pos) => {
                Some(TrackEffect::InstrumentPanningEnvelopePosition(pos))
            }
            TrackImportEffect::InstrumentPanningEnvelope(set) => {
                Some(TrackEffect::InstrumentPanningEnvelope(set))
            }
            TrackImportEffect::InstrumentPitchEnvelope(set) => {
                Some(TrackEffect::InstrumentPitchEnvelope(set))
            }
            TrackImportEffect::InstrumentSampleOffset(offset) => {
                Some(TrackEffect::InstrumentSampleOffset(offset))
            }
            TrackImportEffect::InstrumentSurround(set) => {
                Some(TrackEffect::InstrumentSurround(set))
            }
            TrackImportEffect::InstrumentVolumeEnvelopePosition(pos) => {
                Some(TrackEffect::InstrumentVolumeEnvelopePosition(pos))
            }
            TrackImportEffect::InstrumentVolumeEnvelope(set) => {
                Some(TrackEffect::InstrumentVolumeEnvelope(set))
            }
            TrackImportEffect::NoteCut(tick, past) => Some(TrackEffect::NoteCut { tick, past }),
            TrackImportEffect::NoteDelay(ticks) => {
                if ticks != 0 {
                    Some(TrackEffect::NoteDelay(ticks))
                } else {
                    None
                }
            }
            TrackImportEffect::NoteFadeOut(tick, past) => {
                Some(TrackEffect::NoteFadeOut { tick, past })
            }
            TrackImportEffect::NoteOff(tick, past) => Some(TrackEffect::NoteOff { tick, past }),
            TrackImportEffect::NoteRetrig(interval) => Some(TrackEffect::NoteRetrig {
                speed: interval,
                volume_modifier: NoteRetrigOperator::None,
            }),
            TrackImportEffect::NoteRetrigExtended(interval, vol) => Some(TrackEffect::NoteRetrig {
                speed: interval,
                volume_modifier: note_retrig_volume_modifier_ft2(vol),
            }),
            TrackImportEffect::NoteRetrigExtendedS3m(interval, vol) => {
                Some(TrackEffect::NoteRetrig {
                    speed: interval,
                    volume_modifier: note_retrig_volume_modifier_s3m(vol),
                })
            }
            TrackImportEffect::Tremor(on, off) => Some(TrackEffect::Tremor {
                on_time: on,
                off_time: off,
            }),
            TrackImportEffect::Volume(value, tick) => Some(TrackEffect::Volume { value, tick }),
            // Migrated to AutomationLane — never reaches the cell.
            TrackImportEffect::Panbrello(_, _)
            | TrackImportEffect::PanbrelloWaveform(_, _)
            | TrackImportEffect::PanningSlide0(_)
            | TrackImportEffect::PanningSlideN(_)
            | TrackImportEffect::ChannelVolumeSlide0(_)
            | TrackImportEffect::ChannelVolumeSlideN(_)
            | TrackImportEffect::PortamentoUp(_)
            | TrackImportEffect::PortamentoDown(_)
            | TrackImportEffect::PortamentoFineUp(_)
            | TrackImportEffect::PortamentoFineDown(_)
            | TrackImportEffect::PortamentoExtraFineUp(_)
            | TrackImportEffect::PortamentoExtraFineDown(_)
            | TrackImportEffect::TonePortamento(_)
            | TrackImportEffect::TonePortamentoFxVol(_)
            | TrackImportEffect::Tremolo(_, _)
            | TrackImportEffect::TremoloWaveform(_, _)
            | TrackImportEffect::Vibrato(_, _)
            | TrackImportEffect::VibratoFine(_, _)
            | TrackImportEffect::VibratoSpeed(_)
            | TrackImportEffect::VibratoDepth(_)
            | TrackImportEffect::VibratoDepthFxVol(_)
            | TrackImportEffect::VibratoWaveform(_, _)
            | TrackImportEffect::VolumeSlide0(_)
            | TrackImportEffect::VolumeSlideN(_)
            | TrackImportEffect::InstrumentSampleOffsetAddHigh(_) => None,
        }
    }

    /// Collapse effects that can be merged via [`TrackEffect::merge`];
    /// keep parallel entries where merging is undefined (returns
    /// `None`).
    fn remove_duplicates_and_merge(mut effects: Vec<TrackEffect>) -> Vec<TrackEffect> {
        let mut unique_effects: Vec<TrackEffect> = Vec::new();

        for effect in effects.drain(..) {
            let merge_target = unique_effects.iter().enumerate().find_map(|(idx, e)| {
                if core::mem::discriminant(e) == core::mem::discriminant(&effect) {
                    e.merge(&effect).map(|m| (idx, m))
                } else {
                    None
                }
            });
            match merge_target {
                Some((idx, merged)) => unique_effects[idx] = merged,
                None => unique_effects.push(effect),
            }
        }

        unique_effects
    }

    /// Convert a per-row [`TrackImportEffect`] slice to the runtime
    /// [`TrackEffect`] representation, dropping the families that
    /// migrated to [`AutomationLane`]s in DAW migration
    /// Phase 3c.3 — the lane extractor consumes
    /// `&[TrackImportEffect]` directly.
    ///
    /// [`AutomationLane`]: crate::core::daw::automation::AutomationLane
    pub fn to_track_effects(effects: &[TrackImportEffect]) -> Vec<TrackEffect> {
        let mut vte: Vec<TrackEffect> = Vec::new();
        for fx in effects {
            if let Some(te) = Self::to_track_effect(*fx) {
                vte.push(te);
            }
        }
        Self::remove_duplicates_and_merge(vte)
    }
}
