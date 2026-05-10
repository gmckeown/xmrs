use crate::fixed::fixed::{Q15, Q8_8};
use crate::fixed::units::{ChannelVolume, Finetune, Panning, PitchDelta, RetrigMul, Volume};
use crate::{prelude::NewNoteAction, waveform::Waveform};
use serde::{Deserialize, Serialize};

#[derive(Default, Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NoteRetrigOperator {
    #[default]
    None,
    /// Add a signed Q1.15 delta to the running volume on
    /// each retrigger. Was `f32`. Range is `[-1, +1]`; the
    /// player clamps the resulting volume to `[0, 1]`.
    Sum(Q15),
    /// Multiply the running volume on each retrigger.
    /// `RetrigMul` is Q3.13 in `[-4, +4)`, matching the IT
    /// retrigger multiplier table (`1/3`, `1/2`, `2`, `1.5`,
    /// etc.).
    Mul(RetrigMul),
}

#[repr(u8)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TrackEffect {
    /// Pitch effect
    Arpeggio {
        /// 1st halftone
        half1: usize,
        /// 2nd halftone
        half2: usize,
    },

    /// `value`, set the Channel Volume.
    /// Was `f32` — Q1.15 in `[0, 1]`. See [`ChannelVolume`].
    ChannelVolume(ChannelVolume),

    /// `(speed, fine)`, slide the current channel volume up or
    /// down. `speed` is a signed Q1.15 delta-per-tick (was
    /// `f32`); positive = up, negative = down.
    /// Channel Volume effect
    ChannelVolumeSlide {
        speed: Q15,
        /// if true, only at tick0, otherwise from tick1
        fine: bool,
    },

    /// `bool`, round to the nearest halftone when using effects
    /// Pitch effect
    Glissando(bool),

    /// `tune`, this effet should be used together with a note.
    /// It will cause another fine-tune value to be used.
    /// Was `f32` — Q1.15 in `[-1, +1]`. See [`Finetune`].
    InstrumentFineTune(Finetune),

    /// `nna` Change Instrument New Note Action
    /// Misc effect
    InstrumentNewNoteAction(NewNoteAction),

    /// `position`, change the panning envelope position
    /// Panning effect
    InstrumentPanningEnvelopePosition(usize),

    /// `activate`, set the panning envelope on
    /// Panning effect
    InstrumentPanningEnvelope(bool),

    /// `activate`, set the pitch envelope on
    /// Pitch effect
    InstrumentPitchEnvelope(bool),

    /// `offset`, this effect should be used together with a note
    /// The sample will be played from `offset` instead of zero
    /// Misc effect
    InstrumentSampleOffset(usize),

    /// `surround`
    /// Misc effect
    InstrumentSurround(bool),

    /// `position`, change the volume envelope position
    /// Volume effect
    InstrumentVolumeEnvelopePosition(usize),

    /// `activate`, set the volume envelope on
    /// Volume effect
    InstrumentVolumeEnvelope(bool),

    /// `(tick, past)`, cut the note at the specified tick.
    /// if past is true, do it for past note too
    /// Note that it will only set the volume to zero, and the sample will still be played.
    /// Volume effect
    NoteCut {
        /// cut at tick
        tick: usize,
        /// do it for past note too
        past: bool,
    },

    /// `ticks`, this effect will delay the note the selected number of ticks
    /// Misc effect
    NoteDelay(usize),

    /// `(tick, past)`, fadeout the note at the specified tick
    /// if past is true, do it for past note too
    /// Volume effect
    NoteFadeOut {
        /// fadeout the note at the specified tick
        tick: usize,
        /// do it for past note too
        past: bool,
    },

    /// `tick`, this effect will trigger a "Note Off" at the specified tick
    /// if past is true, do it for past note too
    /// Misc effect
    NoteOff {
        /// off the note at the specified tick
        tick: usize,
        /// do it for past note too
        past: bool,
    },

    /// `(speed, volume modifier)`
    /// Misc effect
    NoteRetrig {
        speed: usize,
        volume_modifier: NoteRetrigOperator,
    },

    /// `(speed, depth)`, set Panbrello.
    /// `speed` is `Q8_8` cycles/tick (was `f32`). `depth` is
    /// `Q15` signed amplitude in `[-1, 1]` of the panning
    /// modulation (was `f32`).
    /// Panning effect
    Panbrello { speed: Q8_8, depth: Q15 },

    /// `(waveform, retrig)`, change Panbrello waveform.
    /// Panning effect
    PanbrelloWaveform {
        waveform: Waveform,
        /// retrig when a new instrument is played.
        retrig: bool,
    },

    /// `position` Q1.15 in `[0, 1]`, sets the panning position.
    /// `0` is fully left, `1` fully right. Was `f32`. See
    /// [`Panning`].
    Panning(Panning),

    /// `(speed, fine)`, slide the panning position. `speed` is a
    /// signed Q1.15 delta-per-tick (was `f32`).
    /// Panning effect
    PanningSlide {
        speed: Q15,
        /// if true, only at tick0, otherwise from tick1
        fine: bool,
    },

    /// `speed` (signed period units added per application — was
    /// `f32`), `fine` flag.
    ///
    /// Pitch effect, covering every FT2 pitch-slide variant that
    /// manipulates the raw period directly (as opposed to
    /// [`Self::TonePortamento`] which slides toward a target note):
    ///
    /// * `fine = false` — *regular* slide (FT2 1xx, 2xx; S3M Exx, Fxx
    ///   with hi-nibble < 0xE; IT Exx, Fxx with hi-nibble < 0xE): the
    ///   replayer applies `speed` to the period on every non-zero
    ///   tick of the row.
    /// * `fine = true` — *fine* or *extra-fine* slide (FT2 E1x, E2x,
    ///   X1y, X2y; S3M EFx/FFx, EEx/FEx; IT EFx/FFx, EEx/FEx): the
    ///   replayer applies `speed` exactly once, at tick 0 of the row.
    ///
    /// Sign encodes direction: a negative speed shortens the period
    /// (pitch up); a positive speed lengthens it (pitch down). The
    /// importer pre-scales by the format's unit factor so `speed` is
    /// always in xmrs period units ready to be added to `period`.
    /// `i16` covers the FT2 worst-case (±255 × 4 = ±1020 LU).
    Portamento { speed: i16, fine: bool },

    /// `speed`, portamento toward the note at `speed` period
    /// units per tick (signed, was `f32`).
    /// see `::Glissando` to round to the nearest halftone
    /// Pitch effect
    TonePortamento(i16),

    /// `(speed, depth)`. `speed` Q8.8 cycles/tick, `depth`
    /// `Q15` signed amplitude in `[-1, 1]` (was `f32`).
    /// Volume effect
    Tremolo { speed: Q8_8, depth: Q15 },

    /// `(waveform, retrig)`, change Tremolo waveform.
    /// Volume effect
    TremoloWaveform {
        waveform: Waveform,
        /// retrig when a new instrument is played.
        retrig: bool,
    },

    /// `(On time, Off time)`
    /// This weird effect will set the volume to zero during `Off time` number of ticks
    /// Volume effect
    Tremor {
        on_time: usize,
        /// set the volume to zero during off time numbre of ticks
        off_time: usize,
    },

    /// `(speed, depth)`, set Vibrato. `speed` Q8.8 cycles/tick,
    /// `depth` semitones (PitchDelta, Q8.8). Was `f32`.
    /// Pitch effect
    Vibrato { speed: Q8_8, depth: PitchDelta },

    /// `speed`, set Vibrato speed (Q8.8 cycles/tick — was `f32`).
    /// Pitch effect
    VibratoSpeed(Q8_8),

    /// `depth`, set Vibrato depth (semitones, PitchDelta — was `f32`).
    /// Pitch effect
    VibratoDepth(PitchDelta),

    /// `(waveform, retrig)`, change Vibrato waveform.
    /// Pitch effect
    VibratoWaveform {
        waveform: Waveform,
        /// retrig when a new instrument is played.
        retrig: bool,
    },

    /// `(value, tick)`, set the current volume at the current tick.
    /// `value` was `f32` — Q1.15 in `[0, 1]`. See [`Volume`].
    /// Volume effect
    Volume {
        value: Volume,
        /// set at the current tick
        tick: usize,
    },

    /// `(speed, fine)`, slide the current volume up or down.
    /// `speed` is a signed Q1.15 delta-per-tick (was `f32`).
    /// Volume effect
    VolumeSlide {
        speed: Q15,
        /// if true, only at tick0, otherwise from tick1
        fine: bool,
    },
}

/// MIDI-macro effect variant. IT's macro system has two user-
/// visible dispatch axes and a per-channel selector register; the
/// three variants below capture them distinctly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MidiMacroType {
    /// `SFx` — set the channel's active parametric-macro slot to
    /// `idx` (0..15). Does not invoke the macro; the next `Zxx`
    /// with `xx < 0x80` on this channel will use it.
    SelectParametric(usize),
    /// `Zxx` with `xx < 0x80` — invoke the channel's currently
    /// selected parametric macro, substituting the byte `z` (0x7A)
    /// in the macro string with the `xx` parameter.
    Parametric(u8),
    /// `Zxx` with `xx >= 0x80` — invoke fixed macro at `idx =
    /// xx - 0x80`. The `z` byte (if present in the macro string)
    /// is substituted with the literal `xx` parameter, matching
    /// OpenMPT's interpretation.
    Fixed { idx: usize, z: u8 },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum GlobalEffect {
    /// `bpm` set the BPM of the song
    Bpm(usize),

    /// `value` slide the BPM on every tick of the row
    BpmSlide(isize),

    /// Midi macro
    MidiMacro(MidiMacroType),

    /// `pattern-position`, jump to the next pattern and play from the specified position.
    PatternBreak(usize),

    /// `(quantity, unit)`, this effect will delay the row the selected number of units.
    /// if `unit` is true, `unit` is on tempo, `else` unit is on tick
    /// If multiple commands are on the same row, the sum of their parameters is used.
    ///
    /// All effects in the row will be repeated for each row delayed.
    ///
    /// if `unit` is true (tempo unit)
    ///     does not delay for the sum of all commands in in the row
    /// else
    ///     delays by the sum of that row
    PatternDelay {
        quantity: usize,
        /// if true, `unit` is on tempo, `else` unit is on tick
        tempo: bool,
    },

    /// `value`, if value is zero, set the loopbackpoint
    /// is value is a non-zero, loop from previous loopback point value times
    PatternLoop(usize),

    /// `position`, jump to the selected song position and play the pattern from the beginning
    PositionJump(usize),

    /// `speed` set the speed of the song
    Speed(usize),

    /// `value`, set the global Volume. Was `f32` — Q1.15 in
    /// `[0, 1]`. See [`Volume`].
    Volume(Volume),

    /// `(speed, fine)`, slide the global volume up or down.
    /// `speed` is a signed Q1.15 delta-per-tick (was `f32`).
    VolumeSlide { speed: Q15, fine: bool },
}

impl TrackEffect {
    pub fn merge(&self, other: &Self) -> Option<Self> {
        match (self, other) {
            (
                TrackEffect::Arpeggio {
                    half1: h1,
                    half2: h2,
                },
                TrackEffect::Arpeggio {
                    half1: o1,
                    half2: o2,
                },
            ) => Some(TrackEffect::Arpeggio {
                half1: h1 + o1,
                half2: h2 + o2,
            }),
            (TrackEffect::ChannelVolume(value1), TrackEffect::ChannelVolume(value2)) => {
                // Two `ChannelVolume` events on one row: cumulative
                // sum (saturating in `+`), then clamped to `[0, 1]`.
                let sum = (*value1 + *value2).clamp(ChannelVolume::SILENT, ChannelVolume::FULL);
                Some(TrackEffect::ChannelVolume(sum))
            }
            (
                TrackEffect::ChannelVolumeSlide {
                    speed: h1,
                    fine: h_fine,
                },
                TrackEffect::ChannelVolumeSlide {
                    speed: o1,
                    fine: o_fine,
                },
            ) if h_fine == o_fine => Some(TrackEffect::ChannelVolumeSlide {
                speed: *h1 + *o1,
                fine: *o_fine,
            }),
            (TrackEffect::Glissando(_h1), TrackEffect::Glissando(o1)) => {
                Some(TrackEffect::Glissando(*o1)) // overwrite glissando value.
            }
            (TrackEffect::InstrumentFineTune(h1), TrackEffect::InstrumentFineTune(o1)) => {
                // Two finetunes on one row → typed saturating
                // sum (kept inside Q1.15 by `Finetune`'s `+`).
                Some(TrackEffect::InstrumentFineTune(*h1 + *o1))
            }
            (
                TrackEffect::InstrumentPanningEnvelopePosition(h1),
                TrackEffect::InstrumentPanningEnvelopePosition(o1),
            ) => Some(TrackEffect::InstrumentPanningEnvelopePosition(h1 + o1)),
            (TrackEffect::InstrumentSampleOffset(h1), TrackEffect::InstrumentSampleOffset(o1)) => {
                Some(TrackEffect::InstrumentSampleOffset(h1 + o1))
            }
            (
                TrackEffect::InstrumentVolumeEnvelopePosition(h1),
                TrackEffect::InstrumentVolumeEnvelopePosition(o1),
            ) => Some(TrackEffect::InstrumentVolumeEnvelopePosition(h1 + o1)),
            (
                TrackEffect::NoteCut {
                    tick: h1,
                    past: _h2,
                },
                TrackEffect::NoteCut { tick: o1, past: o2 },
            ) => Some(TrackEffect::NoteCut {
                tick: h1 + o1,
                past: *o2,
            }),
            (TrackEffect::NoteDelay(h1), TrackEffect::NoteDelay(o1)) => {
                Some(TrackEffect::NoteDelay(h1 + o1))
            }
            (
                TrackEffect::NoteFadeOut {
                    tick: h1,
                    past: _h2,
                },
                TrackEffect::NoteFadeOut { tick: o1, past: o2 },
            ) => Some(TrackEffect::NoteFadeOut {
                tick: h1 + o1,
                past: *o2,
            }),
            (
                TrackEffect::NoteOff {
                    tick: h1,
                    past: _h2,
                },
                TrackEffect::NoteOff { tick: o1, past: o2 },
            ) => Some(TrackEffect::NoteOff {
                tick: h1 + o1,
                past: *o2,
            }),
            (
                TrackEffect::NoteRetrig {
                    speed: h1,
                    volume_modifier: h2,
                },
                TrackEffect::NoteRetrig {
                    speed: o1,
                    volume_modifier: o2,
                },
            ) => Some(TrackEffect::NoteRetrig {
                speed: h1 + o1,
                volume_modifier: match o2 {
                    NoteRetrigOperator::None => h2.clone(),
                    _ => o2.clone(),
                },
            }),
            (
                TrackEffect::Panbrello {
                    speed: h1,
                    depth: h2,
                },
                TrackEffect::Panbrello {
                    speed: o1,
                    depth: o2,
                },
            ) => Some(TrackEffect::Panbrello {
                speed: *h1 + *o1,
                depth: *h2 + *o2,
            }),
            (
                TrackEffect::PanbrelloWaveform {
                    waveform: _h1,
                    retrig: _h2,
                },
                TrackEffect::PanbrelloWaveform {
                    waveform: o1,
                    retrig: o2,
                },
            ) => {
                Some(TrackEffect::PanbrelloWaveform {
                    waveform: *o1,
                    retrig: *o2,
                }) // overwrite values
            }
            (TrackEffect::Panning(h1), TrackEffect::Panning(o1)) => {
                // Two `Panning` events on one row → saturating
                // sum (corner case to begin with).
                Some(TrackEffect::Panning(*h1 + *o1))
            }
            (
                TrackEffect::PanningSlide {
                    speed: h1,
                    fine: h_fine,
                },
                TrackEffect::PanningSlide {
                    speed: o1,
                    fine: o_fine,
                },
            ) if h_fine == o_fine => Some(TrackEffect::PanningSlide {
                speed: *h1 + *o1,
                fine: *o_fine,
            }),
            (
                TrackEffect::Portamento {
                    speed: h1,
                    fine: h_fine,
                },
                TrackEffect::Portamento {
                    speed: o1,
                    fine: o_fine,
                },
            ) if h_fine == o_fine => Some(TrackEffect::Portamento {
                // `Portamento.speed` is plain `i16` (signed period
                // delta-per-tick); no Q-format Add — keep the
                // explicit saturating call.
                speed: h1.saturating_add(*o1),
                fine: *o_fine,
            }),
            (TrackEffect::TonePortamento(h1), TrackEffect::TonePortamento(o1)) => {
                // Plain `i16` — no Q-format Add.
                Some(TrackEffect::TonePortamento(h1.saturating_add(*o1)))
            }
            (
                TrackEffect::Tremolo {
                    speed: h1,
                    depth: h2,
                },
                TrackEffect::Tremolo {
                    speed: o1,
                    depth: o2,
                },
            ) => Some(TrackEffect::Tremolo {
                speed: *h1 + *o1,
                depth: *h2 + *o2,
            }),
            (
                TrackEffect::TremoloWaveform {
                    waveform: _h1,
                    retrig: _h2,
                },
                TrackEffect::TremoloWaveform {
                    waveform: o1,
                    retrig: o2,
                },
            ) => {
                Some(TrackEffect::TremoloWaveform {
                    waveform: *o1,
                    retrig: *o2,
                }) // overwrite values
            }
            (
                TrackEffect::Tremor {
                    on_time: h1,
                    off_time: h2,
                },
                TrackEffect::Tremor {
                    on_time: o1,
                    off_time: o2,
                },
            ) => Some(TrackEffect::Tremor {
                on_time: h1 + o1,
                off_time: h2 + o2,
            }),
            (
                TrackEffect::Vibrato {
                    speed: h1,
                    depth: h2,
                },
                TrackEffect::Vibrato {
                    speed: o1,
                    depth: o2,
                },
            ) => Some(TrackEffect::Vibrato {
                speed: *h1 + *o1,
                depth: *h2 + *o2,
            }),
            (TrackEffect::VibratoSpeed(h1), TrackEffect::VibratoSpeed(o1)) => {
                Some(TrackEffect::VibratoSpeed(*h1 + *o1))
            }
            (TrackEffect::VibratoDepth(h1), TrackEffect::VibratoDepth(o1)) => {
                Some(TrackEffect::VibratoDepth(*h1 + *o1))
            }
            (
                TrackEffect::VibratoWaveform {
                    waveform: _h1,
                    retrig: _h2,
                },
                TrackEffect::VibratoWaveform {
                    waveform: o1,
                    retrig: o2,
                },
            ) => {
                Some(TrackEffect::VibratoWaveform {
                    waveform: *o1,
                    retrig: *o2,
                }) // overwrite values
            }
            (
                TrackEffect::Volume {
                    value: value1,
                    tick: tick1,
                },
                TrackEffect::Volume {
                    value: value2,
                    tick: tick2,
                },
            ) => {
                // Two `Volume` events on one row → cumulative
                // sum, clamped to `[0, 1]`.
                let sum = (*value1 + *value2).clamp(Volume::SILENT, Volume::FULL);
                Some(TrackEffect::Volume {
                    value: sum,
                    tick: tick1 + tick2,
                })
            }
            (
                TrackEffect::VolumeSlide {
                    speed: speed1,
                    fine: h_fine,
                },
                TrackEffect::VolumeSlide {
                    speed: speed2,
                    fine: o_fine,
                },
            ) if h_fine == o_fine => {
                // Only merge slides that share the same tick scope
                // (both fine OR both regular). A row may legitimately
                // carry one of each (e.g. XM main-effect Axy + vol-col
                // D/U), and `remove_duplicates_and_merge` keeps them as
                // parallel entries when `merge` returns `None`.
                Some(TrackEffect::VolumeSlide {
                    speed: *speed1 + *speed2,
                    fine: *o_fine,
                })
            }
            _ => None,
        }
    }
}
