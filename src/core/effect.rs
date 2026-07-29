//! Cell-level and song-level effect types — the discrete pieces
//! that ride on a [`Cell`] or `TimelineMap` entry.
//!
//! Three orthogonal families:
//!
//! - [`TrackEffect`] — per-cell, per-trigger effects (Volume,
//!   Panning, ChannelVolume, Arpeggio, Tremor, NoteCut, NoteDelay,
//!   NoteRetrig, MidiMacro, instrument knobs, …). Continuous
//!   families (Vibrato* / Tremolo* / Panbrello* / Portamento /
//!   TonePortamento / VolumeSlide / ChannelVolumeSlide /
//!   PanningSlide) do not live on the cell — they live exclusively
//!   on [`AutomationLane`]s.
//! - [`NavigationEffect`] — flow-control on the order list
//!   (PatternBreak, PatternDelay, PatternLoop, PositionJump).
//!   Absorbed into the [`TimelineMap`] at import time; never read at
//!   runtime.
//! - [`SongLevelEffect`] — song-wide rate / amplitude controls (Bpm,
//!   Speed, Volume, BpmSlide, VolumeSlide). Drives the song-wide
//!   [`AutomationLane`]s at extraction time.
//!
//! [`Cell`]: crate::core::cell::Cell
//! [`AutomationLane`]: crate::core::daw::automation::AutomationLane
//! [`TimelineMap`]: crate::core::daw::timeline::TimelineMap

use crate::core::fixed::fixed::Q15;
use crate::core::fixed::units::{ChannelVolume, Finetune, Panning, RetrigMul, Volume};
use crate::prelude::NewNoteAction;
use serde::{Deserialize, Serialize};

/// Volume-update rule applied at each retrigger of a `Qxy` note-
/// retrig effect. `Qxy` triggers the note every `y` ticks; the `x`
/// nibble selects one of these operators to scale or offset the
/// running volume between retrigs.
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

/// Per-cell effect catalogue.
///
/// The continuous families (Vibrato* / Tremolo* / Panbrello* /
/// Portamento / TonePortamento / VolumeSlide / ChannelVolumeSlide /
/// PanningSlide) are **not** in this enum — they live exclusively on
/// [`crate::core::daw::automation::AutomationLane`]s. The runtime
/// player consults the lanes for those; the cell only carries the
/// discrete / per-trigger effects below.
#[repr(u8)]
/// What happens to one of an instrument's modulators when a note starts.
///
/// A synth voice re-arms its modulators on every note by default — the sweep
/// restarts, the envelope attacks. Some music wants otherwise on a PARTICULAR
/// note: a filter that keeps opening across a phrase instead of snapping back,
/// or one that starts from a chosen point. That is a property of the note, not
/// of the patch, which is why it rides [`TrackEffect::NoteArming`].
#[derive(Default, Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ArmMode {
    /// Restart from the instrument's own value — what a note normally does.
    #[default]
    Reseed,
    /// Leave the modulator running where the previous note left it, so a sweep
    /// continues across the note boundary instead of snapping back.
    Hold,
    /// Start it at this value instead of the instrument's.
    ///
    /// The unit is the target modulator's OWN unit, deliberately not
    /// normalised: a SID cutoff runs on an 11-bit non-linear scale, and
    /// flattening it to a 0..1 range would misrepresent every value in it.
    At(u16),
}

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

    /// Per-note control over how the instrument's modulators are (re)armed
    /// when this note starts — the general form of "tie / legato / don't
    /// restart the sweep".
    ///
    /// None of this is chip-specific: "change the pitch without re-triggering"
    /// is legato on any synth, and "re-arm this modulator or let it run" is the
    /// retrigger/free-run choice every synth offers — only here it is made per
    /// NOTE rather than once per patch. Rob Hubbard's last C64 player is simply
    /// the first format we import that writes it per note (its `0x84`, `0x87`
    /// and `0x88` markers, plus the tie bit of its length byte).
    ///
    /// Honoured today by the SID driver; the OPL and sample voices can adopt it
    /// without any change here.
    NoteArming {
        /// Change the pitch without re-triggering the voice: the envelope and
        /// the gate carry on untouched. A tie.
        legato: bool,
        /// The filter sweep.
        filter: ArmMode,
        /// The pulse-width sweep.
        pulse_width: ArmMode,
        /// The pitch LFO. [`ArmMode::At`] carries the modulator's own parameter
        /// word rather than a starting value — for a SID voice, the byte that
        /// packs the swing's amplitude and its per-step shift — so this note
        /// vibrates differently from the way its instrument would.
        vibrato: ArmMode,
        /// The arpeggio. [`ArmMode::At`] carries the modulator's own parameter
        /// word — for a SID voice the packed nibble pair, high nibble first —
        /// so this note arpeggiates differently from the way its instrument
        /// would.
        ///
        /// It rides the NOTE rather than the cell's [`TrackEffect::Arpeggio`]
        /// because the two run on different clocks: a tracker arpeggio rotates
        /// on the row's ticks and stops at the row boundary, whereas a chip
        /// replayer keeps its phase in a per-voice slot armed at note-on and
        /// steps it every frame for the whole note, however many rows that
        /// spans. The cell effect stays the editor-facing representation.
        arpeggio: ArmMode,
        /// Frames from this note-on until its release envelope takes over, for
        /// instruments whose release is a fixed ramp rather than the patch's own
        /// (see `ReleaseMode::Ramp`). `0` = never, which is every note that does
        /// not use one.
        ///
        /// It lives on the note because it depends on how long THIS note lasts,
        /// which the instrument cannot know.
        release_in: u16,
    },

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

    /// `position` Q1.15 in `[0, 1]`, sets the panning position.
    /// `0` is fully left, `1` fully right. Was `f32`. See
    /// [`Panning`].
    Panning(Panning),

    /// `(On time, Off time)`
    /// This weird effect will set the volume to zero during `Off time` number of ticks
    /// Volume effect
    Tremor {
        on_time: usize,
        /// set the volume to zero during off time numbre of ticks
        off_time: usize,
    },

    /// `(value, tick)`, set the current volume at the current tick.
    /// `value` was `f32` — Q1.15 in `[0, 1]`. See [`Volume`].
    /// Volume effect
    Volume {
        value: Volume,
        /// set at the current tick
        tick: usize,
    },

    /// MIDI macro invocation. Despite IT's classification of the
    /// macro effect as global, it is **per-channel-scoped at
    /// runtime**: the macro byte stream substitutes the channel's
    /// note / volume / pan / instrument fields, and writes through
    /// to the channel's filter envelope or MIDI-out port. The
    /// importer
    /// ([`crate::tracker::import::unit::TrackImportUnit::prepare_cell`])
    /// is the only producer.
    MidiMacro(MidiMacroType),
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

/// Navigation effects — control which row/order/pattern plays next.
///
/// Consumed exclusively by
/// [`crate::tracker::import::build::build_timeline_map`]: the walker
/// absorbs them into the
/// [`crate::core::daw::timeline::TimelineMap`], after which they are
/// dropped. Runtime navigation is driven by the timeline map plus
/// [`crate::core::module::Module::song_loop_to`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NavigationEffect {
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
}

/// Song-level effects — drive the song-wide [`AutomationLane`]s
/// ([`Bpm`] / [`Speed`] / [`GlobalVolume`]) at extraction time.
///
/// Consumed exclusively by [`crate::tracker::import::extract`] (and
/// by `import::memory` for the slide-memory back-fill, a
/// `pub(crate)` helper). The build-timeline pass reads `Speed` and
/// `Bpm` too — they set per-row tick spacing alongside their lane
/// representation.
///
/// [`Bpm`]: crate::core::daw::automation::AutomationTarget::Bpm
/// [`Speed`]: crate::core::daw::automation::AutomationTarget::Speed
/// [`GlobalVolume`]: crate::core::daw::automation::AutomationTarget::GlobalVolume
/// [`AutomationLane`]: crate::core::daw::automation::AutomationLane
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SongLevelEffect {
    /// `bpm` set the BPM of the song
    Bpm(usize),

    /// `value` slide the BPM on every tick of the row
    BpmSlide(isize),

    /// `speed` set the speed of the song
    Speed(usize),

    /// `value`, set the global Volume. Was `f32` — Q1.15 in
    /// `[0, 1]`. See [`Volume`].
    Volume(Volume),

    /// `(speed, fine)`, slide the global volume up or down.
    /// `speed` is a signed Q1.15 delta-per-tick (was `f32`).
    VolumeSlide { speed: Q15, fine: bool },
}

/// Parser-side wrapper used by the importers (XM / IT / S3M / MOD)
/// to return a single typed effect from a row-byte parse. The
/// [`crate::tracker::import::unit::TrackImportUnit::push_global`]
/// helper routes each variant into its dedicated typed `Vec` on the
/// import unit:
///
/// * [`Self::Navigation`] → `TrackImportUnit::navigation`
/// * [`Self::SongLevel`]  → `TrackImportUnit::song_level`
/// * [`Self::MidiMacro`]  → `TrackImportUnit::midi_macros` (and
///   then relocated onto `cell.effects` as
///   [`TrackEffect::MidiMacro`] by `prepare_cell`).
///
/// This wrapper exists only at the parser / TIU boundary;
/// downstream consumers (`build_timeline_map`,
/// `automation_extract`, `import_memory`) see the dedicated
/// [`NavigationEffect`] / [`SongLevelEffect`] / [`MidiMacroType`]
/// types directly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum GlobalEffect {
    Navigation(NavigationEffect),
    SongLevel(SongLevelEffect),
    MidiMacro(MidiMacroType),
}

impl From<NavigationEffect> for GlobalEffect {
    fn from(n: NavigationEffect) -> Self {
        GlobalEffect::Navigation(n)
    }
}

impl From<SongLevelEffect> for GlobalEffect {
    fn from(s: SongLevelEffect) -> Self {
        GlobalEffect::SongLevel(s)
    }
}

impl From<MidiMacroType> for GlobalEffect {
    fn from(m: MidiMacroType) -> Self {
        GlobalEffect::MidiMacro(m)
    }
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
            // NoteCut / NoteFadeOut / NoteOff: ticks accumulate
            // additively, `past` flags OR together (a `past:true`
            // operand wins).
            (
                TrackEffect::NoteCut { tick: h1, past: h2 },
                TrackEffect::NoteCut { tick: o1, past: o2 },
            ) => Some(TrackEffect::NoteCut {
                tick: h1 + o1,
                past: *h2 || *o2,
            }),
            (
                TrackEffect::NoteFadeOut { tick: h1, past: h2 },
                TrackEffect::NoteFadeOut { tick: o1, past: o2 },
            ) => Some(TrackEffect::NoteFadeOut {
                tick: h1 + o1,
                past: *h2 || *o2,
            }),
            (
                TrackEffect::NoteOff { tick: h1, past: h2 },
                TrackEffect::NoteOff { tick: o1, past: o2 },
            ) => Some(TrackEffect::NoteOff {
                tick: h1 + o1,
                past: *h2 || *o2,
            }),
            (TrackEffect::NoteDelay(h1), TrackEffect::NoteDelay(o1)) => {
                Some(TrackEffect::NoteDelay(h1 + o1))
            }
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
            (TrackEffect::Panning(h1), TrackEffect::Panning(o1)) => {
                // Two `Panning` events on one row → saturating
                // sum (corner case to begin with).
                Some(TrackEffect::Panning(*h1 + *o1))
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
            _ => None,
        }
    }
}
