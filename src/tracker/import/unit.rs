//! `TrackImportUnit` — the importer-side, pre-`Cell` row record.
//!
//! Every format importer (MOD / XM / S3M / IT / SID) emits a stream
//! of `TrackImportUnit`s. Each one carries the raw note column +
//! velocity + instrument index + the per-row `TrackImportEffect` /
//! `NavigationEffect` / `SongLevelEffect` / MIDI macros, all in the
//! shape the build / extract pipelines expect.
//!
//! `TrackImportUnit::prepare_cell` is the single conversion site
//! that folds the importer-side row into the runtime
//! [`crate::core::cell::Cell`] consumed by the player
//! (DAW migration Phase 3c.3).

use super::effect::TrackImportEffect;
use crate::core::cell::{Cell, CellEvent};
use crate::core::cell_note::CellNote;
use crate::core::effect::{
    GlobalEffect, MidiMacroType, NavigationEffect, SongLevelEffect, TrackEffect,
};
use crate::core::fixed::units::Volume;
use core::fmt::*;
use serde::{Deserialize, Serialize};

use alloc::vec;
use alloc::vec::Vec;

/// Importer-side mirror of a cell, still carrying the legacy
/// `(note, instrument, velocity)` triple AND the parsed
/// global-effect streams. The importers (XM/IT/S3M/MOD/SID) decode
/// their on-disk formats into this shape; the DAW build pipeline
/// (`build_timeline_layer`) then materialises each `TrackImportUnit`
/// into a typed [`Cell`] via [`Self::prepare_cell`].
///
/// Globals live **only** on the importer side. The runtime [`Cell`]
/// no longer carries them — song-level globals
/// ([`SongLevelEffect`], i.e. Bpm / Speed / GlobalVolume + slides)
/// are extracted into [`AutomationLane`]s at build time, navigation
/// globals ([`NavigationEffect`], i.e. PatternBreak / PositionJump /
/// PatternLoop / PatternDelay) are absorbed into the `TimelineMap`,
/// and MIDI macros are relocated to `cell.effects` as
/// [`TrackEffect::MidiMacro`] inside [`Self::prepare_cell`].
///
/// DAW migration "split de GlobalEffect" splits the former
/// `global_effects: Vec<GlobalEffect>` field into three typed
/// vectors — one per consumer pipeline. Use
/// [`Self::push_global`] to dispatch a parser-side [`GlobalEffect`]
/// into the right vec.
///
/// The split-per-instrument pass (`split_rows_by_instrument`) needs
/// to see the explicit `instrument: Option<usize>` to choose where
/// segment boundaries fall, so it consumes `TrackImportUnit`s
/// directly. `prepare_cell` is therefore called *after* segments are
/// computed, on the per-segment row sequence.
///
/// [`AutomationLane`]: crate::core::daw::automation::AutomationLane
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TrackImportUnit {
    pub note: CellNote,
    /// Note-trigger velocity, Q1.15 in `[0, 1]`. Was `f32`.
    pub velocity: Volume,
    pub instrument: Option<usize>,
    pub effects: Vec<TrackImportEffect>,
    /// Navigation globals (PatternBreak / PatternDelay /
    /// PatternLoop / PositionJump). Consumed exclusively by
    /// `build_timeline_map`; dropped at `prepare_cell` time.
    pub navigation: Vec<NavigationEffect>,
    /// Song-level globals (Bpm / BpmSlide / Speed / Volume /
    /// VolumeSlide). Consumed by `extract_song_lanes_from_patterns`
    /// and `build_timeline_map`'s per-row tick computation (the
    /// `Bpm` / `Speed` variants set per-row tick spacing); also
    /// read by `import_memory` for slide-memory back-fill. Dropped
    /// at `prepare_cell` time.
    pub song_level: Vec<SongLevelEffect>,
    /// MIDI macros relocated onto `cell.effects` as
    /// [`TrackEffect::MidiMacro`] at `prepare_cell` time.
    pub midi_macros: Vec<MidiMacroType>,
}

impl Default for TrackImportUnit {
    fn default() -> Self {
        Self {
            note: CellNote::default(),
            velocity: Volume::FULL,
            instrument: None,
            effects: vec![],
            navigation: vec![],
            song_level: vec![],
            midi_macros: vec![],
        }
    }
}

impl TrackImportUnit {
    /// Bake this import-side cell into a runtime
    /// [`crate::core::cell::Cell`] in one step:
    ///
    /// * `(note, instrument, velocity)` folds into a single
    ///   [`CellEvent`] via [`CellEvent::from_columns`].
    /// * `self.effects` are converted to runtime [`TrackEffect`]s
    ///   via [`TrackImportEffect::to_track_effects`], which omits the
    ///   automation-only families (Vibrato* / Tremolo* / Panbrello* /
    ///   Portamento / TonePortamento / VolumeSlide /
    ///   ChannelVolumeSlide / PanningSlide). Those live exclusively
    ///   on [`crate::core::daw::automation::AutomationLane`]s.
    /// * Every MIDI macro in `self.midi_macros` is appended to
    ///   `cell.effects` as [`TrackEffect::MidiMacro`] — the runtime
    ///   reads MidiMacro from `cell.effects` exclusively.
    /// * `navigation` and `song_level` are dropped from the cell:
    ///   they are consumed upstream by `build_timeline_map`
    ///   (navigation, and Bpm/Speed for tick spacing) and
    ///   `extract_song_lanes_from_patterns` (the full set of
    ///   `SongLevelEffect`).
    pub fn prepare_cell(&self) -> Cell {
        let mut effects = TrackImportEffect::to_track_effects(&self.effects);
        for m in &self.midi_macros {
            effects.push(TrackEffect::MidiMacro(m.clone()));
        }
        // A note-column `^^^` (`CellNote::NoteCut`) is inert at dispatch
        // (see `CellEvent::NoteCut` docs) — the cut is carried by a
        // `TrackEffect::NoteCut` on the same row. The per-format effect
        // parsers only emit that effect for the *effect-column* cut
        // (IT `SCx`, XM `ECx`, S3M `SCx`); a bare note-column cut would
        // otherwise never silence its voice (the note rings forever).
        // Co-emit it here, the single unit→cell site, so every format is
        // covered uniformly. tick 0 = cut immediately at row start;
        // past = false = cut the live voice only (S7x handles ghosts).
        if matches!(self.note, CellNote::NoteCut) {
            effects.push(TrackEffect::NoteCut {
                tick: 0,
                past: false,
            });
        }
        Cell {
            event: CellEvent::from_columns(self.note, self.instrument, self.velocity),
            effects,
            expression: crate::core::cell::NoteExpression::NEUTRAL,
        }
    }

    /// Dispatch a parser-side [`GlobalEffect`] into the matching
    /// typed vec on this import unit. Single entry point used by
    /// every importer (XM / IT / S3M / MOD) — keeps the per-format
    /// parsers single-pass and the typed split internal.
    pub fn push_global(&mut self, ge: GlobalEffect) {
        match ge {
            GlobalEffect::Navigation(n) => self.navigation.push(n),
            GlobalEffect::SongLevel(s) => self.song_level.push(s),
            GlobalEffect::MidiMacro(m) => self.midi_macros.push(m),
        }
    }

    /// `true` iff this unit carries any global effect of any
    /// category. Replaces the previous `!global_effects.is_empty()`
    /// check used by `is_cell_meaningful` and friends.
    #[inline]
    pub fn has_any_global(&self) -> bool {
        !self.navigation.is_empty() || !self.song_level.is_empty() || !self.midi_macros.is_empty()
    }
}
