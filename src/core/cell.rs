//! `Cell` — one row of a [`crate::core::daw::track::Track`].
//!
//! Successor to the legacy `TrackUnit` struct which carried a
//! `(note, instrument, velocity)` triple. DAW migration Phase 1
//! collapses that triple into a single typed [`crate::core::cell::CellEvent`];
//! Phase 2 moves the per-trigger velocity into the
//! [`crate::core::cell::CellEvent::NoteOn`] /
//! [`crate::core::cell::CellEvent::NoteOnGhost`] variants where it
//! actually has meaning.
//!
//! The cell stays row-indexed — one `Cell` per Track row — and the
//! instrument is no longer stored per-cell: it lives at
//! [`crate::core::daw::track::Track::instrument`], invariant for the
//! lifetime of the Track. The replayer reads the active track's
//! instrument from a fresh per-channel state field (`Channel::
//! current_track_instrument`) rather than from the cell.
//!
//! Since DAW migration Phase 3c.3 the cell no longer carries a
//! global-effects vector — song-level globals
//! ([`crate::core::effect::SongLevelEffect`]) migrated to
//! [`crate::core::daw::automation::AutomationLane`]s, navigation globals
//! ([`crate::core::effect::NavigationEffect`]) to
//! [`crate::core::daw::timeline::TimelineMap`], and the MIDI macro to
//! `cell.effects` via [`crate::core::effect::TrackEffect::MidiMacro`].

use crate::core::cell_note::CellNote;
use crate::core::effect::TrackEffect;
use crate::core::fixed::units::Volume;
use crate::core::pitch::Pitch;
use core::fmt::*;
use serde::{Deserialize, Serialize};

use alloc::vec::Vec;

/// Unified pattern-cell event.
///
/// The variants encode every `(note column, instrument column)`
/// combination an XM/IT/S3M/MOD cell can carry, plus the
/// "ghost-vs-fresh" distinction that historically lived in the
/// `cell.instrument: Option<usize>` field.
///
/// | Variant | Note source | Instr-col source |
/// |---|---|---|
/// | [`Self::None`] | Empty | absent |
/// | [`Self::NoteOn`] | `Play(p)` | present |
/// | [`Self::NoteOnGhost`] | `Play(p)` | absent |
/// | [`Self::InstrReset`] | Empty | present |
/// | [`Self::NoteOff`] `{ retrig: false }` | `KeyOff` (`===`) | absent |
/// | [`Self::NoteOff`] `{ retrig: true }` | `KeyOff` (`===`) | present |
/// | [`Self::NoteCut`] | `^^^` | * |
/// | [`Self::NoteFade`] | `~~~` | * |
///
/// `NoteOff::retrig` is a **semantic distinction**, not a quirk —
/// the channel logic in `xmrsplayer` consults this branch without a
/// `PlaybackQuirks` gate (channel.rs:2311-2314 in the legacy code).
/// `=== xx` with an instrument column reproduces the historical
/// tracker idiom that re-triggers the instrument and then immediately
/// fires `key_off`: a one-tick attack followed by release.
#[derive(Default, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CellEvent {
    /// Nothing on this cell — both columns empty.
    #[default]
    None,
    /// Fresh trigger: play `pitch` with `velocity`, reset envelopes
    /// and sample. Instrument column is present; the actual
    /// instrument value comes from `Track.instrument`.
    NoteOn { pitch: Pitch, velocity: Volume },
    /// Ghost trigger: play `pitch` with `velocity` without resetting
    /// the live voice (`TRIGGER_KEEP_VOLUME`). Instrument column is
    /// empty; the current voice is reused for the new pitch.
    NoteOnGhost { pitch: Pitch, velocity: Volume },
    /// Instrument column present but note column empty:
    /// reload instrument defaults without re-pitching.
    /// Drives `tick0_change_instr(true)` on the channel.
    InstrReset,
    /// Release (KeyOff column, `===`).
    ///
    /// `retrig = false` (no instrument column): plain `key_off(0)`.
    /// `retrig = true` (instrument column present): re-trigger the
    /// instrument with `KEEP_PERIOD`, *then* `key_off(0)` — the
    /// tracker idiom that produces a click attack followed by
    /// release on the same row. Universal to XM/IT/S3M.
    NoteOff { retrig: bool },
    /// Hard cut (`^^^`). Volume → 0 immediately. The actual cut work
    /// happens via the row's [`TrackEffect::NoteCut`] effect; the
    /// note-column `^^^` is inert at dispatch in the historical
    /// player (channel.rs:2360-2364).
    NoteCut,
    /// Start fadeout without releasing the envelope (`~~~`). IT-
    /// specific note-column event. Identical for any instrument-column
    /// state — schism's note-fade branch never gates on `row_instr`.
    NoteFade,
}

impl CellEvent {
    /// Materialise a `CellEvent` from the legacy
    /// `(note, instrument, velocity)` triple. Used by the importers,
    /// which still surface those three fields independently in
    /// [`crate::tracker::import::unit::TrackImportUnit`].
    #[inline]
    pub fn from_legacy(note: CellNote, instrument: Option<usize>, velocity: Volume) -> Self {
        match (note, instrument) {
            (CellNote::Empty, None) => Self::None,
            (CellNote::Empty, Some(_)) => Self::InstrReset,
            (CellNote::Play(p), Some(_)) => Self::NoteOn { pitch: p, velocity },
            (CellNote::Play(p), None) => Self::NoteOnGhost { pitch: p, velocity },
            (CellNote::KeyOff, None) => Self::NoteOff { retrig: false },
            (CellNote::KeyOff, Some(_)) => Self::NoteOff { retrig: true },
            (CellNote::NoteCut, _) => Self::NoteCut,
            (CellNote::NoteFade, _) => Self::NoteFade,
        }
    }

    /// Pitch carried by note-on variants; `None` otherwise.
    #[inline]
    pub fn pitch(&self) -> Option<Pitch> {
        match *self {
            Self::NoteOn { pitch, .. } | Self::NoteOnGhost { pitch, .. } => Some(pitch),
            _ => None,
        }
    }

    /// Trigger velocity for [`Self::NoteOn`] / [`Self::NoteOnGhost`].
    /// `Volume::FULL` for any other variant — non-trigger cells have
    /// no meaningful velocity, and downstream code that always
    /// multiplies by velocity sees a no-op.
    #[inline]
    pub fn velocity(&self) -> Volume {
        match *self {
            Self::NoteOn { velocity, .. } | Self::NoteOnGhost { velocity, .. } => velocity,
            _ => Volume::FULL,
        }
    }

    /// `true` when the originating cell had a non-empty instrument
    /// column — Phase 1's stand-in for the legacy
    /// `cell.instrument.is_some()` semantic flag.
    #[inline]
    pub fn has_instrument_column(&self) -> bool {
        matches!(
            self,
            Self::NoteOn { .. } | Self::InstrReset | Self::NoteOff { retrig: true }
        )
    }

    /// `true` for variants that fire a fresh playback trigger
    /// (NoteOn / NoteOnGhost).
    #[inline]
    pub fn is_trigger(&self) -> bool {
        matches!(self, Self::NoteOn { .. } | Self::NoteOnGhost { .. })
    }

    /// `true` for variants that release / cut / fade the held note
    /// (NoteOff / NoteCut / NoteFade).
    #[inline]
    pub fn is_release(&self) -> bool {
        matches!(self, Self::NoteOff { .. } | Self::NoteCut | Self::NoteFade)
    }

    /// Project back onto the legacy [`CellNote`] enum. Useful for
    /// debug printers and UI code that still surfaces the
    /// soundtracker-style `--- / C-5 / === / ^^^ / ~~~` notation.
    /// Lossy: discards the retrig flag of [`Self::NoteOff`] and
    /// flattens [`Self::InstrReset`] back onto `CellNote::Empty`.
    #[inline]
    pub fn to_cell_note(&self) -> CellNote {
        match *self {
            Self::None | Self::InstrReset => CellNote::Empty,
            Self::NoteOn { pitch, .. } | Self::NoteOnGhost { pitch, .. } => CellNote::Play(pitch),
            Self::NoteOff { .. } => CellNote::KeyOff,
            Self::NoteCut => CellNote::NoteCut,
            Self::NoteFade => CellNote::NoteFade,
        }
    }
}

impl Debug for CellEvent {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        match self {
            Self::None => write!(f, "---"),
            Self::NoteOn { pitch, .. } => write!(f, "{:?}", pitch),
            Self::NoteOnGhost { pitch, .. } => write!(f, "{:?}g", pitch),
            Self::InstrReset => write!(f, "i--"),
            Self::NoteOff { retrig: true } => write!(f, "==!"),
            Self::NoteOff { retrig: false } => write!(f, "==="),
            Self::NoteCut => write!(f, "^^^"),
            Self::NoteFade => write!(f, "~~~"),
        }
    }
}

/// One row of a [`crate::core::daw::track::Track`] — a single pattern
/// cell. Carries a typed [`CellEvent`] plus any effects pinned to
/// this row.
///
/// The instrument is **not** stored here — it lives on
/// [`crate::core::daw::track::Track::instrument`] (invariant for the
/// Track's lifetime). The replayer pushes the track's instrument
/// alongside the cell through its per-row dispatch.
///
/// Globals were removed from the runtime [`Cell`] in
/// DAW migration Phase 3c.3: continuous globals
/// ([`crate::core::effect::SongLevelEffect`] —
/// Bpm/Speed/GlobalVolume + slides) became
/// [`crate::core::daw::automation::AutomationLane`]s, navigation globals
/// ([`crate::core::effect::NavigationEffect`]) were absorbed into
/// [`crate::core::daw::timeline::TimelineMap`] at import time, and the
/// MIDI macro relocated to `cell.effects` as
/// [`TrackEffect::MidiMacro`].
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Cell {
    pub event: CellEvent,
    pub effects: Vec<TrackEffect>,
}

impl Cell {
    /// Construct a cell from the legacy `(note, instrument, velocity,
    /// effects)` shape. The importers still surface the three header
    /// fields independently in
    /// [`crate::tracker::import::unit::TrackImportUnit`]; this
    /// helper is the single conversion site.
    #[inline]
    pub fn from_legacy(
        note: CellNote,
        instrument: Option<usize>,
        velocity: Volume,
        effects: Vec<TrackEffect>,
    ) -> Self {
        Self {
            event: CellEvent::from_legacy(note, instrument, velocity),
            effects,
        }
    }

    /// Pitch carried by note-on variants. Forwards to
    /// [`CellEvent::pitch`].
    #[inline]
    pub fn pitch(&self) -> Option<Pitch> {
        self.event.pitch()
    }

    /// Velocity attached to a trigger. Forwards to
    /// [`CellEvent::velocity`].
    #[inline]
    pub fn velocity(&self) -> Volume {
        self.event.velocity()
    }

    /// `true` when the originating cell carried an instrument column.
    /// Forwards to [`CellEvent::has_instrument_column`].
    #[inline]
    pub fn has_instrument_column(&self) -> bool {
        self.event.has_instrument_column()
    }

    pub fn has_arpeggio(&self) -> bool {
        self.effects
            .iter()
            .any(|effect| matches!(effect, TrackEffect::Arpeggio { half1: _, half2: _ }))
    }

    pub fn has_delay(&self) -> bool {
        self.effects
            .iter()
            .any(|effect| matches!(effect, TrackEffect::NoteDelay(_)))
    }

    pub fn get_delay(&self) -> usize {
        self.effects
            .iter()
            .find_map(|effect| {
                if let TrackEffect::NoteDelay(delay) = effect {
                    Some(*delay)
                } else {
                    None
                }
            })
            .unwrap_or(0)
    }

    pub fn has_note_off(&self) -> bool {
        let fx = self
            .effects
            .iter()
            .any(|effect| matches!(effect, TrackEffect::NoteOff { tick: _, past: _ }));
        fx || matches!(self.event, CellEvent::NoteOff { .. })
    }

    /// Returns true iff the slot carries a Kxx *effect* note-off.
    ///
    /// Unlike [`Self::has_note_off`], this does NOT match a note-column
    /// `===` ([`CellEvent::NoteOff`]). It's intended for replayer paths
    /// that need to apply FT2 Kxy-effect quirks without also misfiring
    /// on note-column key-offs, which have different playback
    /// semantics in FT2 (see `ft2_replayer.c:keyOff()` vs. the
    /// `keyOffCmd` / K00 paths).
    pub fn has_note_off_effect(&self) -> bool {
        self.effects
            .iter()
            .any(|effect| matches!(effect, TrackEffect::NoteOff { tick: _, past: _ }))
    }

    /// Returns true iff the slot carries a Kxx *effect* note-off whose
    /// trigger tick is 0 — i.e. specifically K00, not K01..K1F.
    ///
    /// Used by the FT2 "K00 eats note" quirk: in FT2, only K00 is handled
    /// at tickZero via the special branch in `getNewNote`, and this is
    /// what causes the note column to be skipped. Non-zero Kxy effects go
    /// through the normal note trigger path and must not short-circuit it.
    pub fn has_note_off_at_tick_zero(&self) -> bool {
        self.effects
            .iter()
            .any(|effect| matches!(effect, TrackEffect::NoteOff { tick: 0, past: _ }))
    }

    // `has_tone_portamento` / `has_vibrato` / `has_vibrato_depth` /
    // `has_volume_slide` were removed in DAW migration
    // Phase 3c.3. The cell no longer carries the corresponding
    // `TrackEffect` variants — they migrated to `AutomationLane`s.
    // The runtime quirks that previously consulted these helpers now
    // query the lane events at the current row's tick directly (see
    // `Channel::row_has_*_lane()` in `xmrsplayer`).
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pitch() -> Pitch {
        Pitch::C5
    }

    #[test]
    fn from_legacy_maps_every_arm() {
        // Play + instr → NoteOn
        assert!(matches!(
            CellEvent::from_legacy(CellNote::Play(pitch()), Some(3), Volume::FULL),
            CellEvent::NoteOn { .. }
        ));
        // Play, no instr → NoteOnGhost
        assert!(matches!(
            CellEvent::from_legacy(CellNote::Play(pitch()), None, Volume::FULL),
            CellEvent::NoteOnGhost { .. }
        ));
        // Empty + instr → InstrReset
        assert!(matches!(
            CellEvent::from_legacy(CellNote::Empty, Some(0), Volume::FULL),
            CellEvent::InstrReset
        ));
        // Empty, no instr → None
        assert!(matches!(
            CellEvent::from_legacy(CellNote::Empty, None, Volume::FULL),
            CellEvent::None
        ));
        // KeyOff, no instr → NoteOff{retrig:false}
        assert_eq!(
            CellEvent::from_legacy(CellNote::KeyOff, None, Volume::FULL),
            CellEvent::NoteOff { retrig: false }
        );
        // KeyOff + instr → NoteOff{retrig:true}
        assert_eq!(
            CellEvent::from_legacy(CellNote::KeyOff, Some(0), Volume::FULL),
            CellEvent::NoteOff { retrig: true }
        );
        // NoteCut / NoteFade collapse irrespective of instrument
        assert_eq!(
            CellEvent::from_legacy(CellNote::NoteCut, None, Volume::FULL),
            CellEvent::NoteCut
        );
        assert_eq!(
            CellEvent::from_legacy(CellNote::NoteCut, Some(5), Volume::FULL),
            CellEvent::NoteCut
        );
        assert_eq!(
            CellEvent::from_legacy(CellNote::NoteFade, None, Volume::FULL),
            CellEvent::NoteFade
        );
        assert_eq!(
            CellEvent::from_legacy(CellNote::NoteFade, Some(5), Volume::FULL),
            CellEvent::NoteFade
        );
    }

    #[test]
    fn pitch_velocity_extraction() {
        let e = CellEvent::NoteOn {
            pitch: pitch(),
            velocity: Volume::FULL,
        };
        assert_eq!(e.pitch(), Some(pitch()));
        assert_eq!(e.velocity(), Volume::FULL);

        let e = CellEvent::NoteOnGhost {
            pitch: pitch(),
            velocity: Volume::SILENT,
        };
        assert_eq!(e.pitch(), Some(pitch()));
        assert_eq!(e.velocity(), Volume::SILENT);

        // Non-trigger variants — pitch is None, velocity is FULL.
        for e in [
            CellEvent::None,
            CellEvent::InstrReset,
            CellEvent::NoteOff { retrig: false },
            CellEvent::NoteOff { retrig: true },
            CellEvent::NoteCut,
            CellEvent::NoteFade,
        ] {
            assert_eq!(e.pitch(), None);
            assert_eq!(e.velocity(), Volume::FULL);
        }
    }

    #[test]
    fn has_instrument_column_matches_legacy() {
        // Legacy: instr.is_some() AND not a pure ghost note.
        assert!(CellEvent::NoteOn {
            pitch: pitch(),
            velocity: Volume::FULL
        }
        .has_instrument_column());
        assert!(CellEvent::InstrReset.has_instrument_column());
        assert!(CellEvent::NoteOff { retrig: true }.has_instrument_column());

        assert!(!CellEvent::NoteOnGhost {
            pitch: pitch(),
            velocity: Volume::FULL
        }
        .has_instrument_column());
        assert!(!CellEvent::NoteOff { retrig: false }.has_instrument_column());
        assert!(!CellEvent::None.has_instrument_column());
        assert!(!CellEvent::NoteCut.has_instrument_column());
        assert!(!CellEvent::NoteFade.has_instrument_column());
    }

    #[test]
    fn to_cell_note_round_trip() {
        // The lossy projection must at least preserve the
        // "shape" — Empty/Play/KeyOff/NoteCut/NoteFade.
        assert_eq!(CellEvent::None.to_cell_note(), CellNote::Empty);
        assert_eq!(CellEvent::InstrReset.to_cell_note(), CellNote::Empty);
        assert_eq!(
            CellEvent::NoteOn {
                pitch: pitch(),
                velocity: Volume::FULL
            }
            .to_cell_note(),
            CellNote::Play(pitch())
        );
        assert_eq!(
            CellEvent::NoteOff { retrig: false }.to_cell_note(),
            CellNote::KeyOff
        );
        assert_eq!(
            CellEvent::NoteOff { retrig: true }.to_cell_note(),
            CellNote::KeyOff
        );
        assert_eq!(CellEvent::NoteCut.to_cell_note(), CellNote::NoteCut);
        assert_eq!(CellEvent::NoteFade.to_cell_note(), CellNote::NoteFade);
    }
}
