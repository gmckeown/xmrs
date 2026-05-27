//! Editing API: atomic, validated, undoable commands.
//!
//! `Module` is mutated via [`crate::edit::EditCommand`] variants.
//! Each call to [`crate::core::module::Module::apply`] runs validation
//! first; on success it produces an [`crate::edit::EditReceipt`]
//! that carries the state needed to undo the operation via
//! [`crate::core::module::Module::undo`].
//!
//! * Commands are an enum — serialisable, for session replay or
//!   debugging.
//! * Undo state lives in [`crate::edit::EditUndoData`] inside the
//!   receipt; xmrs ships `apply` + `undo`, not the history stack
//!   itself.
//! * Atomicity: `apply` returns `Err` *before* mutating if the
//!   operation can't succeed.
//! * Notifications: `apply_with_observer` dispatches to an
//!   [`crate::edit::EditObserver`] so an editor can refresh its
//!   UI incrementally.

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::core::cell::{Cell, CellEvent};
use crate::core::compatibility::PlaybackQuirks;
use crate::core::daw::automation::{
    AutomationLane, AutomationPoint, AutomationTarget, AutomationValue, GlideEvent, LaneKind,
    LfoEvent, SlideEvent,
};
use crate::core::daw::clip::Clip;
use crate::core::daw::track::Track;
use crate::core::effect::TrackEffect;
use crate::core::fixed::fixed::Q15;
use crate::core::module::{Module, PatternHighlight};
use crate::core::pitch::Pitch;

// =====================================================================
// EditCommand
// =====================================================================

/// One atomic, undoable mutation of a [`Module`].
///
/// Every editor action is encoded as an `EditCommand` and applied
/// via [`Module::apply`]; the returned [`EditReceipt`] carries the
/// [`EditUndoData`] needed to roll back through [`Module::undo`].
/// The proptest suite under `tests/edit_proptest.rs` validates
/// apply/undo round-trip + `verify_layers_consistent` invariants
/// against thousands of generated commands.
///
/// Variants are grouped by what they mutate: Track, Track-content
/// (cells), Clip, Automation, Global settings, Instrument.
///
/// [`Module`]: crate::core::module::Module
/// [`Module::apply`]: crate::core::module::Module::apply
/// [`Module::undo`]: crate::core::module::Module::undo
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum EditCommand {
    // ---- Track ----
    CreateTrack {
        instrument: usize,
        name: String,
        length_rows: u32,
    },
    DeleteTrack {
        track: u32,
    },
    RenameTrack {
        track: u32,
        new_name: String,
    },
    SetTrackInstrument {
        track: u32,
        new_instrument: usize,
    },
    SetTrackMuted {
        track: u32,
        muted: bool,
    },
    ResizeTrack {
        track: u32,
        new_length_rows: u32,
    },
    CloneTrack {
        track: u32,
    },

    // ---- Track content (cells) ----
    InsertRow {
        track: u32,
        row_offset: u32,
        content: Cell,
    },
    DeleteRow {
        track: u32,
        row_offset: u32,
    },
    SetCell {
        track: u32,
        row_offset: u32,
        content: Cell,
    },
    AddCellEffect {
        track: u32,
        row_offset: u32,
        effect: TrackEffect,
    },
    RemoveCellEffect {
        track: u32,
        row_offset: u32,
        effect_index: usize,
    },
    SetCellEffects {
        track: u32,
        row_offset: u32,
        effects: Vec<TrackEffect>,
    },
    // `SetCellGlobalEffects` removed in DAW migration Phase 3c.3 —
    // `Cell.global_effects` no longer exists. Song-level globals
    // (`Bpm` / `Speed` / `GlobalVolume`) are edited through the
    // `AddPointEvent` / `AddSlideEvent` lane operations; navigation
    // globals (`PatternBreak` / `PositionJump` / `PatternLoop` /
    // `PatternDelay`) are absorbed into the `TimelineMap` at import
    // time and manipulated through the timeline-side edits
    // (`MoveClip` / `SplitClip` / `SetClipTrack` / …); `MidiMacro`
    // moves to `cell.effects` and rides on `AddCellEffect`.
    // ---- Clip ----
    CreateClip {
        track: u32,
        song: u16,
        target_channel: u8,
        position_tick: u32,
    },
    DeleteClip {
        clip: u32,
    },
    MoveClip {
        clip: u32,
        new_position_tick: u32,
    },
    SetClipTrack {
        clip: u32,
        new_track: u32,
    },
    SetClipTargetChannel {
        clip: u32,
        new_target_channel: u8,
    },
    DuplicateClip {
        clip: u32,
        new_position_tick: u32,
        new_target_channel: u8,
    },

    /// Cut a Clip in two at `at_tick`. The original clip stops at
    /// `at_tick` (becomes `[position_tick, at_tick)`) and a new
    /// clip is created that starts at `at_tick` and runs through
    /// the original's `end_tick`, pointing at the same Track with
    /// a `track_row_offset` adjusted to resume on the right row.
    /// DAW migration Phase 4.
    SplitClip {
        clip: u32,
        at_tick: u32,
    },
    /// Extend or shrink a Clip by moving its `end_tick`. The
    /// underlying Track is *not* mutated — when `new_end_tick`
    /// extends past the natural row count the player simply loops
    /// the Track's rows (the existing `row_at` resolution already
    /// handles this via modulo). DAW migration Phase 4.
    ResizeClip {
        clip: u32,
        new_end_tick: u32,
    },
    /// Shift the content under a Clip without moving the Clip
    /// itself: `source_start_row` and `track_row_offset` are
    /// shifted by `delta` source rows (positive = later, negative
    /// = earlier). The Clip's `[position_tick, end_tick)` window
    /// is unchanged. DAW migration Phase 4.
    SlipClip {
        clip: u32,
        delta_rows: i32,
    },

    // ---- Block editing (multi-track rectangular ops) ----
    /// Paste a rectangular block of cells (typically produced by
    /// [`crate::core::module::Module::copy_block`]) into the requested
    /// `tracks` starting at `row_offset`. The block layout is
    /// `content[i][j]` → `tracks[i].rows[row_offset + j]`. `mode`
    /// selects whether to replace target cells outright or merge
    /// the source into them. Tracks shorter than the paste window
    /// are extended with [`Cell::default()`] padding.
    ///
    /// DAW migration Phase 4.
    PasteBlock {
        tracks: Vec<u32>,
        row_offset: u32,
        content: Vec<Vec<Cell>>,
        mode: PasteMode,
    },
    /// Transpose every [`crate::core::cell::CellEvent::NoteOn`] and
    /// [`CellEvent::NoteOnGhost`] in the rectangular `(tracks,
    /// row_start..row_end)` block by `semitones`. Positive
    /// semitones move pitch up; negative moves down. Out-of-range
    /// pitches (clamping past
    /// [`crate::core::pitch::Pitch::B9`] / before [`Pitch::C0`]) cause
    /// the command to fail with [`EditError::IndexOutOfRange`]
    /// (so a partial transposition is never committed).
    ///
    /// DAW migration Phase 4.
    TransposeBlock {
        tracks: Vec<u32>,
        row_start: u32,
        row_end: u32,
        semitones: i32,
    },
    /// Linearly interpolate a scalar effect across a row window
    /// on one Track. Both `row_start` and `row_end - 1` must
    /// already carry the matching effect (the endpoints anchor
    /// the lerp); every row strictly between them has the
    /// interpolated value pushed onto its `effects` vector
    /// (existing cells of other kinds are untouched, existing
    /// instances of the same kind are replaced).
    ///
    /// DAW migration Phase 4. Supported kinds:
    /// [`InterpolateEffectKind::Volume`] /
    /// [`InterpolateEffectKind::Panning`] /
    /// [`InterpolateEffectKind::ChannelVolume`] — all Q1.15
    /// scalar values, lerped by row index.
    InterpolateEffect {
        track: u32,
        row_start: u32,
        row_end: u32,
        kind: InterpolateEffectKind,
    },

    // ---- Automation ----
    CreateAutomationLane {
        target: AutomationTarget,
    },
    DeleteAutomationLane {
        lane: u32,
    },
    SetAutomationLaneEnabled {
        lane: u32,
        enabled: bool,
    },
    AddAutomationPoint {
        lane: u32,
        point: AutomationPoint,
    },
    DeleteAutomationPoint {
        lane: u32,
        point_index: u32,
    },
    MoveAutomationPoint {
        lane: u32,
        point_index: u32,
        new_tick: u32,
        new_value: AutomationValue,
    },
    /// Append an [`LfoEvent`] to an `Lfo`-kind lane, keeping the
    /// event list sorted by tick (insertion via binary search).
    /// Rejects when the targeted lane is a different kind.
    /// DAW migration Phase 4.
    AddLfoEvent {
        lane: u32,
        event: LfoEvent,
    },
    /// Append a [`SlideEvent`] to a `Slide`-kind lane.
    AddSlideEvent {
        lane: u32,
        event: SlideEvent,
    },
    /// Append a [`GlideEvent`] to a `Glide`-kind lane.
    AddGlideEvent {
        lane: u32,
        event: GlideEvent,
    },

    // ---- Sub-song / global ----
    SetDefaultBpm {
        bpm: usize,
    },
    SetDefaultSpeed {
        speed: usize,
    },
    SetPlaybackQuirks {
        quirks: PlaybackQuirks,
    },
    SetModuleName {
        name: String,
    },
    SetPatternHighlight {
        highlight: PatternHighlight,
    },

    // ---- Track-level humanise ----
    /// Set the humanisation parameters of a [`Track::Euclidean`].
    /// Errors with [`EditError::TrackNotEuclidean`] if `track`
    /// is not a Euclidean variant.
    SetEuclideanHumanize {
        track: u32,
        advance_max_ticks: u8,
        probability: Q15,
    },
}

/// Which scalar effect [`EditCommand::InterpolateEffect`] lerps.
/// All variants are Q1.15-typed, so the interpolation is a
/// straightforward saturating linear blend on the raw `i16`
/// representation.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterpolateEffectKind {
    /// Per-cell [`TrackEffect::Volume`] (tracker `Cxx` /
    /// vol-col `vXX`). Interpolates the `value` field;
    /// `tick` is copied from the start endpoint.
    Volume,
    /// Per-cell [`TrackEffect::Panning`].
    Panning,
    /// Per-cell [`TrackEffect::ChannelVolume`] (S3M Mxx).
    ChannelVolume,
}

/// Merge policy for [`EditCommand::PasteBlock`].
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteMode {
    /// Replace each target cell outright with the source cell.
    /// The simplest, most predictable semantic.
    Overwrite,
    /// Merge each source cell into the target:
    /// - the target's `event` is replaced only when the source's
    ///   `event` is non-`None`,
    /// - the target's `effects` vector is appended with the
    ///   source's, preserving original effects.
    Mix,
}

// =====================================================================
// EditError
// =====================================================================

/// Failure mode returned by [`Module::apply`] when an edit command
/// can't be applied to the current `Module` state.
///
/// The `Module` is left unchanged when this error is returned —
/// `apply` is fail-atomic.
///
/// [`Module::apply`]: crate::core::module::Module::apply
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// An index parameter (track / clip / row / lane / point /
    /// instrument / channel) points past the end of the
    /// corresponding vector.
    IndexOutOfRange {
        what: &'static str,
        index: u32,
    },
    TrackInUse {
        track: u32,
        clips: Vec<u32>,
    },
    ClipOverlap {
        existing_clip: u32,
    },
    AutomationTypeMismatch {
        target: AutomationTarget,
        got: AutomationValue,
    },
    RowOutOfRange {
        track: u32,
        requested: u32,
        length: u32,
    },
    InstrumentDoesNotExist {
        index: usize,
    },
    /// The track exists but is not a [`Track::Notes`] — cell-editing
    /// commands cannot apply to a generator variant
    /// (e.g. [`Track::Euclidean`]).
    TrackNotNotes {
        index: u32,
    },
    /// The track exists but is not a [`Track::Euclidean`] — required
    /// by Euclidean-only commands such as
    /// [`EditCommand::SetEuclideanHumanize`].
    TrackNotEuclidean {
        index: u32,
    },
}

// =====================================================================
// EditUndoData
// =====================================================================

/// Per-command roll-back state captured at apply time.
///
/// Stored in the returned [`EditReceipt`] and consumed by
/// [`Module::undo`] to restore the pre-mutation state. Each variant
/// holds the minimum snapshot required to invert one
/// [`EditCommand`] (an index for creations, a full snapshot for
/// content mutations, etc.).
///
/// [`Module::undo`]: crate::core::module::Module::undo
#[derive(Debug, Clone)]
pub enum EditUndoData {
    /// Created a Track at this final index.
    TrackCreated { index: u32 },
    /// Full-track snapshot before the mutation (covers Insert/Delete/Resize rows).
    TrackContentBefore { index: u32, track: Track },
    /// Created a Clip at this final position in `module.clips`.
    ClipCreated { index: u32 },
    /// Full clip snapshot + its previous position in `module.clips`.
    ClipBefore { index: u32, clip: Clip },
    /// Snapshot of the entire `module.clips` Vec, used by mutations
    /// that re-sort the list (MoveClip, SetClipTargetChannel,
    /// CreateClip, DuplicateClip). Restoring the snapshot is the
    /// simplest robust way to undo a sort-disturbing mutation.
    ClipsSnapshot { before: Vec<Clip> },
    /// Snapshot of both Vecs — used by DeleteTrack which shifts
    /// every `Clip.track` greater than the removed index.
    TracksAndClipsSnapshot {
        tracks: Vec<Track>,
        clips: Vec<Clip>,
    },
    /// Created an automation lane at this index.
    LaneCreated { index: u32 },
    /// Deleted lane; restore by re-inserting at `index`.
    LaneDeleted { index: u32, lane: AutomationLane },
    /// Full lane snapshot before the mutation.
    LaneContentBefore { index: u32, lane: AutomationLane },
    /// Per-cell snapshot.
    CellBefore { track: u32, row: u32, before: Cell },
    /// Global setting snapshot.
    GlobalSettingBefore(GlobalSettingSnapshot),
    /// Previous humanize parameters of a [`Track::Euclidean`].
    EuclideanHumanizeBefore {
        track: u32,
        advance_max_ticks: u8,
        probability: Q15,
    },
}

/// Sparse snapshot of the global settings touched by one edit
/// command. Each field is `Some(previous_value)` iff the matching
/// `Set*` command actually wrote it; `None` fields are left
/// untouched on undo.
#[derive(Debug, Clone, Default)]
pub struct GlobalSettingSnapshot {
    /// Previous `Module.name`, if it was mutated.
    pub name: Option<String>,
    /// Previous `Module.default_bpm`, if it was mutated.
    pub default_bpm: Option<usize>,
    /// Previous `Module.default_tempo`, if it was mutated.
    pub default_speed: Option<usize>,
    /// Previous `Module.quirks`, if it was mutated.
    pub quirks: Option<PlaybackQuirks>,
    /// Previous `Module.pattern_highlight`, if it was mutated.
    pub pattern_highlight: Option<PatternHighlight>,
}

// =====================================================================
// EditReceipt
// =====================================================================

/// Result of a successful [`Module::apply`].
///
/// Pairs the [`EditCommand`] that was applied with the
/// [`EditUndoData`] needed to roll it back. Storing a vector of
/// receipts lets an editor maintain its own undo / redo stack.
///
/// [`Module::apply`]: crate::core::module::Module::apply
#[derive(Debug, Clone)]
pub struct EditReceipt {
    /// The command that was successfully applied.
    pub command: EditCommand,
    /// Snapshot data sufficient to undo this command.
    pub undo_data: EditUndoData,
}

// =====================================================================
// EditObserver
// =====================================================================

/// Coarse-grained signal describing what a successfully-applied
/// edit changed. Forwarded to every registered [`EditObserver`]
/// after the mutation lands, so an editor can refresh its caches
/// or repaint affected panels without having to inspect the
/// command itself.
#[derive(Debug, Clone)]
pub enum ChangeEvent {
    /// One or more tracks were mutated (created / deleted /
    /// resized / renamed / content-edited / euclidean-tuned).
    TracksChanged {
        /// Final-state indices of the affected tracks.
        indices: Vec<u32>,
    },
    /// One or more clips were mutated.
    ClipsChanged {
        /// Final-state indices of the affected clips.
        indices: Vec<u32>,
    },
    /// One or more automation lanes were mutated.
    AutomationChanged {
        /// Final-state indices of the affected lanes.
        lane_indices: Vec<u32>,
    },
    /// A global setting (name, default BPM/speed, quirks,
    /// pattern highlight) was mutated.
    GlobalSettingChanged,
}

/// Sink the editor's edit pipeline notifies after each successful
/// command. Implementors typically debounce these signals to
/// drive UI refresh, transport scheduling, or persistence.
pub trait EditObserver {
    /// Called once per successful [`Module::apply`] with a summary
    /// of the mutation that just landed.
    ///
    /// [`Module::apply`]: crate::core::module::Module::apply
    fn on_change(&mut self, event: ChangeEvent);
}

/// No-op observer used when no callback is provided.
pub struct NullObserver;
impl EditObserver for NullObserver {
    fn on_change(&mut self, _event: ChangeEvent) {}
}

// =====================================================================
// apply / undo
// =====================================================================

impl Module {
    /// Apply a command. If `Err`, the Module is unchanged.
    pub fn apply(&mut self, cmd: EditCommand) -> Result<EditReceipt, EditError> {
        self.apply_with_observer(cmd, &mut NullObserver)
    }

    pub fn apply_with_observer(
        &mut self,
        cmd: EditCommand,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        match cmd.clone() {
            EditCommand::CreateTrack {
                instrument,
                name,
                length_rows,
            } => self.cmd_create_track(cmd, instrument, name, length_rows, observer),
            EditCommand::DeleteTrack { track } => self.cmd_delete_track(cmd, track, observer),
            EditCommand::RenameTrack { track, new_name } => {
                self.cmd_rename_track(cmd, track, new_name, observer)
            }
            EditCommand::SetTrackInstrument {
                track,
                new_instrument,
            } => self.cmd_set_track_instrument(cmd, track, new_instrument, observer),
            EditCommand::SetTrackMuted { track, muted } => {
                self.cmd_set_track_muted(cmd, track, muted, observer)
            }
            EditCommand::ResizeTrack {
                track,
                new_length_rows,
            } => self.cmd_resize_track(cmd, track, new_length_rows, observer),
            EditCommand::CloneTrack { track } => self.cmd_clone_track(cmd, track, observer),

            EditCommand::InsertRow {
                track,
                row_offset,
                content,
            } => self.cmd_insert_row(cmd, track, row_offset, content, observer),
            EditCommand::DeleteRow { track, row_offset } => {
                self.cmd_delete_row(cmd, track, row_offset, observer)
            }
            EditCommand::SetCell {
                track,
                row_offset,
                content,
            } => self.cmd_set_cell(cmd, track, row_offset, content, observer),
            EditCommand::AddCellEffect {
                track,
                row_offset,
                effect,
            } => self.cmd_add_cell_effect(cmd, track, row_offset, effect, observer),
            EditCommand::RemoveCellEffect {
                track,
                row_offset,
                effect_index,
            } => self.cmd_remove_cell_effect(cmd, track, row_offset, effect_index, observer),
            EditCommand::SetCellEffects {
                track,
                row_offset,
                effects,
            } => self.cmd_set_cell_effects(cmd, track, row_offset, effects, observer),

            EditCommand::CreateClip {
                track,
                song,
                target_channel,
                position_tick,
            } => self.cmd_create_clip(cmd, track, song, target_channel, position_tick, observer),
            EditCommand::DeleteClip { clip } => self.cmd_delete_clip(cmd, clip, observer),
            EditCommand::MoveClip {
                clip,
                new_position_tick,
            } => self.cmd_move_clip(cmd, clip, new_position_tick, observer),
            EditCommand::SetClipTrack { clip, new_track } => {
                self.cmd_set_clip_track(cmd, clip, new_track, observer)
            }
            EditCommand::SetClipTargetChannel {
                clip,
                new_target_channel,
            } => self.cmd_set_clip_target_channel(cmd, clip, new_target_channel, observer),
            EditCommand::DuplicateClip {
                clip,
                new_position_tick,
                new_target_channel,
            } => {
                self.cmd_duplicate_clip(cmd, clip, new_position_tick, new_target_channel, observer)
            }
            EditCommand::SplitClip { clip, at_tick } => {
                self.cmd_split_clip(cmd, clip, at_tick, observer)
            }
            EditCommand::ResizeClip { clip, new_end_tick } => {
                self.cmd_resize_clip(cmd, clip, new_end_tick, observer)
            }
            EditCommand::SlipClip { clip, delta_rows } => {
                self.cmd_slip_clip(cmd, clip, delta_rows, observer)
            }
            EditCommand::PasteBlock {
                tracks,
                row_offset,
                content,
                mode,
            } => self.cmd_paste_block(cmd, tracks, row_offset, content, mode, observer),
            EditCommand::TransposeBlock {
                tracks,
                row_start,
                row_end,
                semitones,
            } => self.cmd_transpose_block(cmd, tracks, row_start, row_end, semitones, observer),
            EditCommand::InterpolateEffect {
                track,
                row_start,
                row_end,
                kind,
            } => self.cmd_interpolate_effect(cmd, track, row_start, row_end, kind, observer),

            EditCommand::CreateAutomationLane { target } => {
                self.cmd_create_automation_lane(cmd, target, observer)
            }
            EditCommand::DeleteAutomationLane { lane } => {
                self.cmd_delete_automation_lane(cmd, lane, observer)
            }
            EditCommand::SetAutomationLaneEnabled { lane, enabled } => {
                self.cmd_set_automation_lane_enabled(cmd, lane, enabled, observer)
            }
            EditCommand::AddAutomationPoint { lane, point } => {
                self.cmd_add_automation_point(cmd, lane, point, observer)
            }
            EditCommand::DeleteAutomationPoint { lane, point_index } => {
                self.cmd_delete_automation_point(cmd, lane, point_index, observer)
            }
            EditCommand::MoveAutomationPoint {
                lane,
                point_index,
                new_tick,
                new_value,
            } => self.cmd_move_automation_point(
                cmd,
                lane,
                point_index,
                new_tick,
                new_value,
                observer,
            ),
            EditCommand::AddLfoEvent { lane, event } => {
                self.cmd_add_lfo_event(cmd, lane, event, observer)
            }
            EditCommand::AddSlideEvent { lane, event } => {
                self.cmd_add_slide_event(cmd, lane, event, observer)
            }
            EditCommand::AddGlideEvent { lane, event } => {
                self.cmd_add_glide_event(cmd, lane, event, observer)
            }

            EditCommand::SetDefaultBpm { bpm } => self.cmd_set_default_bpm(cmd, bpm, observer),
            EditCommand::SetDefaultSpeed { speed } => {
                self.cmd_set_default_speed(cmd, speed, observer)
            }
            EditCommand::SetPlaybackQuirks { quirks } => {
                self.cmd_set_playback_quirks(cmd, quirks, observer)
            }
            EditCommand::SetModuleName { name } => self.cmd_set_module_name(cmd, name, observer),
            EditCommand::SetPatternHighlight { highlight } => {
                self.cmd_set_pattern_highlight(cmd, highlight, observer)
            }
            EditCommand::SetEuclideanHumanize {
                track,
                advance_max_ticks,
                probability,
            } => self.cmd_set_euclidean_humanize(
                cmd,
                track,
                advance_max_ticks,
                probability,
                observer,
            ),
        }
    }

    /// Undo a previously applied command using its receipt.
    pub fn undo(&mut self, receipt: &EditReceipt) -> Result<(), EditError> {
        match (&receipt.command, &receipt.undo_data) {
            // Track creation: remove the inserted track. Matches both
            // CreateTrack and CloneTrack (which also produces TrackCreated).
            (_, EditUndoData::TrackCreated { index }) => {
                let idx = *index as usize;
                if idx >= self.tracks.len() {
                    return Err(EditError::IndexOutOfRange {
                        what: "track",
                        index: *index,
                    });
                }
                self.tracks.remove(idx);
                Ok(())
            }
            // Rename/SetInstr/SetMuted/Resize/InsertRow/DeleteRow/SetCell*/CloneTrack
            // are all covered by a content snapshot of the track.
            (_, EditUndoData::TrackContentBefore { index, track }) => {
                let idx = *index as usize;
                if idx >= self.tracks.len() {
                    return Err(EditError::IndexOutOfRange {
                        what: "track",
                        index: *index,
                    });
                }
                self.tracks[idx] = track.clone();
                Ok(())
            }
            // CloneTrack: a TrackCreated undo (we cloned into a new slot).
            // Already handled by the TrackCreated arm above.

            // Cell-only mutation undo.
            (_, EditUndoData::CellBefore { track, row, before }) => {
                let t = self
                    .tracks
                    .get_mut(*track as usize)
                    .ok_or(EditError::IndexOutOfRange {
                        what: "track",
                        index: *track,
                    })?;
                let rows = t
                    .rows_mut()
                    .ok_or(EditError::TrackNotNotes { index: *track })?;
                if (*row as usize) >= rows.len() {
                    return Err(EditError::RowOutOfRange {
                        track: *track,
                        requested: *row,
                        length: rows.len() as u32,
                    });
                }
                rows[*row as usize] = before.clone();
                Ok(())
            }
            // Clip created: remove from clips.
            (_, EditUndoData::ClipCreated { index }) => {
                let idx = *index as usize;
                if idx >= self.clips.len() {
                    return Err(EditError::IndexOutOfRange {
                        what: "clip",
                        index: *index,
                    });
                }
                self.clips.remove(idx);
                Ok(())
            }
            // Snapshot-based restore for sort-disturbing mutations.
            (_, EditUndoData::ClipsSnapshot { before }) => {
                self.clips.restore(before.clone());
                Ok(())
            }
            (_, EditUndoData::TracksAndClipsSnapshot { tracks, clips }) => {
                self.tracks = tracks.clone();
                self.clips.restore(clips.clone());
                Ok(())
            }
            // Clip mutation that doesn't disturb sort order
            // (currently SetClipTrack only — every other clip
            // mutation goes through ClipsSnapshot).
            (_, EditUndoData::ClipBefore { index, clip }) => {
                let idx = *index as usize;
                if idx >= self.clips.len() {
                    return Err(EditError::IndexOutOfRange {
                        what: "clip",
                        index: *index,
                    });
                }
                let restored = *clip;
                self.clips.modify(idx, |c| *c = restored);
                Ok(())
            }
            // Lane creation/deletion.
            (_, EditUndoData::LaneCreated { index }) => {
                let idx = *index as usize;
                if idx >= self.automation.len() {
                    return Err(EditError::IndexOutOfRange {
                        what: "lane",
                        index: *index,
                    });
                }
                self.automation.remove(idx);
                Ok(())
            }
            (_, EditUndoData::LaneDeleted { index, lane }) => {
                let idx = (*index as usize).min(self.automation.len());
                self.automation.insert(idx, lane.clone());
                Ok(())
            }
            (_, EditUndoData::LaneContentBefore { index, lane }) => {
                let idx = *index as usize;
                if idx >= self.automation.len() {
                    return Err(EditError::IndexOutOfRange {
                        what: "lane",
                        index: *index,
                    });
                }
                self.automation[idx] = lane.clone();
                Ok(())
            }
            (_, EditUndoData::GlobalSettingBefore(snap)) => {
                if let Some(name) = &snap.name {
                    self.name = name.clone();
                }
                if let Some(bpm) = snap.default_bpm {
                    self.default_bpm = bpm;
                }
                if let Some(speed) = snap.default_speed {
                    self.default_tempo = speed;
                }
                if let Some(quirks) = &snap.quirks {
                    self.quirks = *quirks;
                }
                if let Some(highlight) = &snap.pattern_highlight {
                    self.pattern_highlight = *highlight;
                }
                Ok(())
            }
            (
                _,
                EditUndoData::EuclideanHumanizeBefore {
                    track,
                    advance_max_ticks,
                    probability,
                },
            ) => {
                let t = self
                    .tracks
                    .get_mut(*track as usize)
                    .ok_or(EditError::IndexOutOfRange {
                        what: "track",
                        index: *track,
                    })?;
                match t {
                    Track::Euclidean {
                        humanize_advance_max_ticks,
                        humanize_probability,
                        ..
                    } => {
                        *humanize_advance_max_ticks = *advance_max_ticks;
                        *humanize_probability = *probability;
                        Ok(())
                    }
                    _ => Err(EditError::TrackNotEuclidean { index: *track }),
                }
            }
        }
    }
}

// =====================================================================
// Command implementations
// =====================================================================

impl Module {
    fn cmd_create_track(
        &mut self,
        cmd: EditCommand,
        instrument: usize,
        name: String,
        length_rows: u32,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        if instrument >= self.instrument.len() {
            return Err(EditError::InstrumentDoesNotExist { index: instrument });
        }
        let track = Track::Notes {
            name,
            instrument,
            rows: alloc::vec![Cell::default(); length_rows as usize],
            muted: false,
        };
        let idx = self.tracks.len() as u32;
        self.tracks.push(track);
        observer.on_change(ChangeEvent::TracksChanged {
            indices: alloc::vec![idx],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::TrackCreated { index: idx },
        })
    }

    fn cmd_delete_track(
        &mut self,
        cmd: EditCommand,
        track: u32,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let idx = track as usize;
        if idx >= self.tracks.len() {
            return Err(EditError::IndexOutOfRange {
                what: "track",
                index: track,
            });
        }
        // Check no clip references this track
        let blockers: Vec<u32> = self
            .clips
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                if c.track == track {
                    Some(i as u32)
                } else {
                    None
                }
            })
            .collect();
        if !blockers.is_empty() {
            return Err(EditError::TrackInUse {
                track,
                clips: blockers,
            });
        }
        let before_tracks = self.tracks.clone();
        let before_clips = self.clips.snapshot();
        self.tracks.remove(idx);
        // Shift down clip.track > track. `clip.track` isn't a sort
        // key, so `modify_all`'s post-pass resort is a no-op.
        self.clips.modify_all(|clip| {
            if clip.track > track {
                clip.track -= 1;
            }
        });
        observer.on_change(ChangeEvent::TracksChanged {
            indices: alloc::vec![track],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::TracksAndClipsSnapshot {
                tracks: before_tracks,
                clips: before_clips,
            },
        })
    }

    /// Shared envelope for "look up a track, snapshot, mutate, emit
    /// `TracksChanged`, return TrackContentBefore". `apply` returns
    /// either `Ok(())` (the track was mutated) or `Err(EditError)`
    /// (e.g. wrong track kind for the operation — the snapshot is
    /// discarded; the function caller must guarantee no partial
    /// mutation has happened before returning `Err`).
    fn with_track_mut<F>(
        &mut self,
        cmd: EditCommand,
        track: u32,
        observer: &mut dyn EditObserver,
        apply: F,
    ) -> Result<EditReceipt, EditError>
    where
        F: FnOnce(&mut Track) -> Result<(), EditError>,
    {
        let t = self
            .tracks
            .get_mut(track as usize)
            .ok_or(EditError::IndexOutOfRange {
                what: "track",
                index: track,
            })?;
        let before = t.clone();
        apply(t)?;
        observer.on_change(ChangeEvent::TracksChanged {
            indices: alloc::vec![track],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::TrackContentBefore {
                index: track,
                track: before,
            },
        })
    }

    fn cmd_rename_track(
        &mut self,
        cmd: EditCommand,
        track: u32,
        new_name: String,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        self.with_track_mut(cmd, track, observer, |t| {
            *t.name_mut() = new_name;
            Ok(())
        })
    }

    fn cmd_set_track_instrument(
        &mut self,
        cmd: EditCommand,
        track: u32,
        new_instrument: usize,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        if new_instrument >= self.instrument.len() {
            return Err(EditError::InstrumentDoesNotExist {
                index: new_instrument,
            });
        }
        self.with_track_mut(cmd, track, observer, |t| {
            t.set_instrument(new_instrument);
            Ok(())
        })
    }

    fn cmd_set_track_muted(
        &mut self,
        cmd: EditCommand,
        track: u32,
        muted: bool,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        self.with_track_mut(cmd, track, observer, |t| {
            t.set_muted(muted);
            Ok(())
        })
    }

    fn cmd_resize_track(
        &mut self,
        cmd: EditCommand,
        track: u32,
        new_length_rows: u32,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        self.with_track_mut(cmd, track, observer, |t| {
            let rows = t
                .rows_mut()
                .ok_or(EditError::TrackNotNotes { index: track })?;
            let n = new_length_rows as usize;
            if n > rows.len() {
                rows.resize(n, Cell::default());
            } else {
                rows.truncate(n);
            }
            Ok(())
        })
    }

    fn cmd_clone_track(
        &mut self,
        cmd: EditCommand,
        track: u32,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let source = self
            .tracks
            .get(track as usize)
            .ok_or(EditError::IndexOutOfRange {
                what: "track",
                index: track,
            })?
            .clone();
        let idx = self.tracks.len() as u32;
        self.tracks.push(source);
        observer.on_change(ChangeEvent::TracksChanged {
            indices: alloc::vec![idx],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::TrackCreated { index: idx },
        })
    }

    fn cmd_insert_row(
        &mut self,
        cmd: EditCommand,
        track: u32,
        row_offset: u32,
        content: Cell,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let t = self
            .tracks
            .get_mut(track as usize)
            .ok_or(EditError::IndexOutOfRange {
                what: "track",
                index: track,
            })?;
        let before = t.clone();
        let rows = t
            .rows_mut()
            .ok_or(EditError::TrackNotNotes { index: track })?;
        if (row_offset as usize) > rows.len() {
            return Err(EditError::RowOutOfRange {
                track,
                requested: row_offset,
                length: rows.len() as u32,
            });
        }
        rows.insert(row_offset as usize, content);
        observer.on_change(ChangeEvent::TracksChanged {
            indices: alloc::vec![track],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::TrackContentBefore {
                index: track,
                track: before,
            },
        })
    }

    fn cmd_delete_row(
        &mut self,
        cmd: EditCommand,
        track: u32,
        row_offset: u32,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let t = self
            .tracks
            .get_mut(track as usize)
            .ok_or(EditError::IndexOutOfRange {
                what: "track",
                index: track,
            })?;
        let before = t.clone();
        let rows = t
            .rows_mut()
            .ok_or(EditError::TrackNotNotes { index: track })?;
        if (row_offset as usize) >= rows.len() {
            return Err(EditError::RowOutOfRange {
                track,
                requested: row_offset,
                length: rows.len() as u32,
            });
        }
        rows.remove(row_offset as usize);
        observer.on_change(ChangeEvent::TracksChanged {
            indices: alloc::vec![track],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::TrackContentBefore {
                index: track,
                track: before,
            },
        })
    }

    /// Shared envelope for the four "mutate one cell" commands.
    /// Validates track index, track kind, and row offset; clones the
    /// pre-mutation cell for undo; runs `apply` on the cell; emits a
    /// `TracksChanged` event and returns the receipt. `apply` may
    /// reject (e.g. `cmd_remove_cell_effect` rejects an
    /// out-of-range effect index) and the snapshot is discarded.
    fn with_cell_mut<F>(
        &mut self,
        cmd: EditCommand,
        track: u32,
        row_offset: u32,
        observer: &mut dyn EditObserver,
        apply: F,
    ) -> Result<EditReceipt, EditError>
    where
        F: FnOnce(&mut Cell) -> Result<(), EditError>,
    {
        let t = self
            .tracks
            .get_mut(track as usize)
            .ok_or(EditError::IndexOutOfRange {
                what: "track",
                index: track,
            })?;
        let rows = t
            .rows_mut()
            .ok_or(EditError::TrackNotNotes { index: track })?;
        if (row_offset as usize) >= rows.len() {
            return Err(EditError::RowOutOfRange {
                track,
                requested: row_offset,
                length: rows.len() as u32,
            });
        }
        let cell = &mut rows[row_offset as usize];
        let before = cell.clone();
        apply(cell)?;
        observer.on_change(ChangeEvent::TracksChanged {
            indices: alloc::vec![track],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::CellBefore {
                track,
                row: row_offset,
                before,
            },
        })
    }

    fn cmd_set_cell(
        &mut self,
        cmd: EditCommand,
        track: u32,
        row_offset: u32,
        content: Cell,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        self.with_cell_mut(cmd, track, row_offset, observer, |cell| {
            *cell = content;
            Ok(())
        })
    }

    fn cmd_add_cell_effect(
        &mut self,
        cmd: EditCommand,
        track: u32,
        row_offset: u32,
        effect: TrackEffect,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        self.with_cell_mut(cmd, track, row_offset, observer, |cell| {
            cell.effects.push(effect);
            Ok(())
        })
    }

    fn cmd_remove_cell_effect(
        &mut self,
        cmd: EditCommand,
        track: u32,
        row_offset: u32,
        effect_index: usize,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        self.with_cell_mut(cmd, track, row_offset, observer, |cell| {
            if effect_index >= cell.effects.len() {
                return Err(EditError::IndexOutOfRange {
                    what: "effect",
                    index: effect_index as u32,
                });
            }
            cell.effects.remove(effect_index);
            Ok(())
        })
    }

    fn cmd_set_cell_effects(
        &mut self,
        cmd: EditCommand,
        track: u32,
        row_offset: u32,
        effects: Vec<TrackEffect>,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        self.with_cell_mut(cmd, track, row_offset, observer, |cell| {
            cell.effects = effects;
            Ok(())
        })
    }

    fn cmd_create_clip(
        &mut self,
        cmd: EditCommand,
        track: u32,
        song: u16,
        target_channel: u8,
        position_tick: u32,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        if track as usize >= self.tracks.len() {
            return Err(EditError::IndexOutOfRange {
                what: "track",
                index: track,
            });
        }
        let track_len = self
            .tracks
            .get(track as usize)
            .map(|t| t.natural_length() as u32)
            .unwrap_or(0);
        let speed = (self.default_tempo as u32).max(1);
        let new_clip = Clip {
            track,
            song,
            target_channel,
            position_tick,
            speed_at_start: self.default_tempo as u8,
            track_row_offset: 0,
            // Synthetic clip — plays its track from the start, and
            // by convention represents source row 0 of an imagined
            // pattern. Editors can override later.
            source_start_row: 0,
            end_tick: position_tick.saturating_add(track_len.saturating_mul(speed)),
        };
        if let Some(idx) = self.find_overlapping_clip(&new_clip, &[]) {
            return Err(EditError::ClipOverlap { existing_clip: idx });
        }
        let before = self.clips.snapshot();
        self.clips.insert(new_clip);
        observer.on_change(ChangeEvent::ClipsChanged {
            indices: alloc::vec![],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::ClipsSnapshot { before },
        })
    }

    fn cmd_delete_clip(
        &mut self,
        cmd: EditCommand,
        clip: u32,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let idx = clip as usize;
        if idx >= self.clips.len() {
            return Err(EditError::IndexOutOfRange {
                what: "clip",
                index: clip,
            });
        }
        let before = self.clips.snapshot();
        self.clips.remove(idx);
        observer.on_change(ChangeEvent::ClipsChanged {
            indices: alloc::vec![clip],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::ClipsSnapshot { before },
        })
    }

    fn cmd_move_clip(
        &mut self,
        cmd: EditCommand,
        clip: u32,
        new_position_tick: u32,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let idx = clip as usize;
        let original = *self.clips.get(idx).ok_or(EditError::IndexOutOfRange {
            what: "clip",
            index: clip,
        })?;
        // Preserve duration: shift end_tick by the same signed delta
        // as position_tick. Saturating arithmetic keeps the clip
        // non-degenerate on overflow.
        let duration = original.end_tick.saturating_sub(original.position_tick);
        let new_end_tick = new_position_tick.saturating_add(duration);
        let mut candidate = original;
        candidate.position_tick = new_position_tick;
        candidate.end_tick = new_end_tick;
        if let Some(i) = self.find_overlapping_clip(&candidate, &[clip]) {
            return Err(EditError::ClipOverlap { existing_clip: i });
        }
        let before = self.clips.snapshot();
        self.clips.modify(idx, |c| {
            c.position_tick = new_position_tick;
            c.end_tick = new_end_tick;
        });
        observer.on_change(ChangeEvent::ClipsChanged {
            indices: alloc::vec![],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::ClipsSnapshot { before },
        })
    }

    fn cmd_set_clip_track(
        &mut self,
        cmd: EditCommand,
        clip: u32,
        new_track: u32,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        if new_track as usize >= self.tracks.len() {
            return Err(EditError::IndexOutOfRange {
                what: "track",
                index: new_track,
            });
        }
        let idx = clip as usize;
        let original = *self.clips.get(idx).ok_or(EditError::IndexOutOfRange {
            what: "clip",
            index: clip,
        })?;
        // `clip.track` isn't a sort key — the resort inside
        // `modify` is harmless.
        self.clips.modify(idx, |c| c.track = new_track);
        observer.on_change(ChangeEvent::ClipsChanged {
            indices: alloc::vec![clip],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::ClipBefore {
                index: clip,
                clip: original,
            },
        })
    }

    fn cmd_set_clip_target_channel(
        &mut self,
        cmd: EditCommand,
        clip: u32,
        new_target_channel: u8,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let idx = clip as usize;
        let original = *self.clips.get(idx).ok_or(EditError::IndexOutOfRange {
            what: "clip",
            index: clip,
        })?;
        let mut candidate = original;
        candidate.target_channel = new_target_channel;
        if let Some(i) = self.find_overlapping_clip(&candidate, &[clip]) {
            return Err(EditError::ClipOverlap { existing_clip: i });
        }
        let before = self.clips.snapshot();
        self.clips
            .modify(idx, |c| c.target_channel = new_target_channel);
        observer.on_change(ChangeEvent::ClipsChanged {
            indices: alloc::vec![],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::ClipsSnapshot { before },
        })
    }

    fn cmd_duplicate_clip(
        &mut self,
        cmd: EditCommand,
        clip: u32,
        new_position_tick: u32,
        new_target_channel: u8,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let source = *self
            .clips
            .get(clip as usize)
            .ok_or(EditError::IndexOutOfRange {
                what: "clip",
                index: clip,
            })?;
        let duration = source.end_tick.saturating_sub(source.position_tick);
        let new_clip = Clip {
            track: source.track,
            song: source.song,
            target_channel: new_target_channel,
            position_tick: new_position_tick,
            speed_at_start: source.speed_at_start,
            track_row_offset: 0,
            // Duplicate inherits the source's mapping — same track,
            // same source-row alignment.
            source_start_row: source.source_start_row,
            // Duplicate occupies the same duration on the timeline
            // as the source clip.
            end_tick: new_position_tick.saturating_add(duration),
        };
        if let Some(i) = self.find_overlapping_clip(&new_clip, &[]) {
            return Err(EditError::ClipOverlap { existing_clip: i });
        }
        let before = self.clips.snapshot();
        self.clips.insert(new_clip);
        observer.on_change(ChangeEvent::ClipsChanged {
            indices: alloc::vec![],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::ClipsSnapshot { before },
        })
    }

    /// Split a Clip in two at `at_tick`. The Track behind it stays
    /// untouched — the two resulting clips reference the same
    /// Track, with the second clip's `source_start_row` /
    /// `track_row_offset` shifted by `(at_tick − position_tick) /
    /// speed_at_start` rows so the source-row → playback-tick
    /// alignment is preserved across the cut.
    fn cmd_split_clip(
        &mut self,
        cmd: EditCommand,
        clip: u32,
        at_tick: u32,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let idx = clip as usize;
        let original = *self.clips.get(idx).ok_or(EditError::IndexOutOfRange {
            what: "clip",
            index: clip,
        })?;
        // `at_tick` must fall strictly inside the clip's open
        // half-open range — splitting at the boundaries is a no-op
        // by construction and is rejected to keep semantics tight.
        if at_tick <= original.position_tick || at_tick >= original.end_tick {
            return Err(EditError::IndexOutOfRange {
                what: "at_tick (must be strictly inside the clip)",
                index: at_tick,
            });
        }
        let speed = original.speed_at_start.max(1) as u32;
        let delta_ticks = at_tick - original.position_tick;
        // Integer division: drops the partial row, matching the
        // player's row-aligned resolution. The split snaps to the
        // nearest row start at or before `at_tick`.
        let delta_rows = delta_ticks / speed;
        // The second clip resumes the same Track at the
        // post-split source row. `track_row_offset` is reset to 0
        // — the new clip's "row 0" is the source row at the split.
        let second = Clip {
            track: original.track,
            song: original.song,
            target_channel: original.target_channel,
            position_tick: at_tick,
            speed_at_start: original.speed_at_start,
            track_row_offset: 0,
            source_start_row: original.source_start_row.saturating_add(delta_rows),
            end_tick: original.end_tick,
        };
        // Snapshot BEFORE any mutation so undo restores the full
        // pre-split state in one shot.
        let before = self.clips.snapshot();
        // Verify the inserted second clip wouldn't overlap a
        // sibling on the same `(song, target_channel)` lane.
        // (Can't happen against the original, since `second` lives
        // in `[at_tick, end_tick)` while the shortened original
        // lives in `[position_tick, at_tick)` — disjoint by
        // construction. But a clip past the original on the same
        // lane could theoretically overlap if someone resized
        // creatively, so the check is defensive.)
        if let Some(i) = self.find_overlapping_clip(&second, &[clip]) {
            return Err(EditError::ClipOverlap { existing_clip: i });
        }
        self.clips.modify(idx, |c| c.end_tick = at_tick);
        self.clips.insert(second);
        observer.on_change(ChangeEvent::ClipsChanged {
            indices: alloc::vec![],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::ClipsSnapshot { before },
        })
    }

    /// Move a Clip's `end_tick`. Rejects an `end` ≤ `position_tick`
    /// (zero or negative duration) and any overlap with a
    /// downstream clip on the same `(song, target_channel)`. The
    /// underlying Track is *not* mutated — a `new_end_tick` past
    /// the natural row count loops the Track via the existing
    /// `Module::row_at` modulo path.
    fn cmd_resize_clip(
        &mut self,
        cmd: EditCommand,
        clip: u32,
        new_end_tick: u32,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let idx = clip as usize;
        let original = *self.clips.get(idx).ok_or(EditError::IndexOutOfRange {
            what: "clip",
            index: clip,
        })?;
        if new_end_tick <= original.position_tick {
            return Err(EditError::IndexOutOfRange {
                what: "new_end_tick (must be > position_tick)",
                index: new_end_tick,
            });
        }
        let mut candidate = original;
        candidate.end_tick = new_end_tick;
        if let Some(i) = self.find_overlapping_clip(&candidate, &[clip]) {
            return Err(EditError::ClipOverlap { existing_clip: i });
        }
        self.clips.modify(idx, |c| c.end_tick = new_end_tick);
        observer.on_change(ChangeEvent::ClipsChanged {
            indices: alloc::vec![clip],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::ClipBefore {
                index: clip,
                clip: original,
            },
        })
    }

    /// Shift the content under a Clip without moving the Clip
    /// itself: `source_start_row` advances by `delta_rows`
    /// (positive = later in the Track, negative = earlier).
    /// `track_row_offset` is held at the same alignment by also
    /// adjusting it, so the clip's first playback row stays at
    /// `source_start_row + track_row_offset`.
    fn cmd_slip_clip(
        &mut self,
        cmd: EditCommand,
        clip: u32,
        delta_rows: i32,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let idx = clip as usize;
        let original = *self.clips.get(idx).ok_or(EditError::IndexOutOfRange {
            what: "clip",
            index: clip,
        })?;
        // Saturating shift in u32 land. Negative deltas that would
        // underflow are rejected — the Track has no negative rows.
        let new_source_start_row = if delta_rows >= 0 {
            original.source_start_row.saturating_add(delta_rows as u32)
        } else {
            let abs = (-delta_rows) as u32;
            if abs > original.source_start_row {
                return Err(EditError::IndexOutOfRange {
                    what: "delta_rows (would underflow source_start_row)",
                    index: abs,
                });
            }
            original.source_start_row - abs
        };
        self.clips.modify(idx, |c| {
            c.source_start_row = new_source_start_row;
        });
        observer.on_change(ChangeEvent::ClipsChanged {
            indices: alloc::vec![clip],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::ClipBefore {
                index: clip,
                clip: original,
            },
        })
    }

    /// Paste a rectangular block of cells into the requested
    /// tracks. The block's [i][j] entry lands at
    /// `tracks[i].rows[row_offset + j]`. Tracks shorter than the
    /// paste window are extended with `Cell::default()` padding.
    fn cmd_paste_block(
        &mut self,
        cmd: EditCommand,
        tracks: Vec<u32>,
        row_offset: u32,
        content: Vec<Vec<Cell>>,
        mode: PasteMode,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        if tracks.len() != content.len() {
            return Err(EditError::IndexOutOfRange {
                what: "content (length must match `tracks.len()`)",
                index: content.len() as u32,
            });
        }
        // Validate every track index AND track kind before touching
        // anything — the function documents "Module unchanged on
        // Err", so we cannot start mutating and discover halfway
        // through that a target track has no rows.
        for &t in &tracks {
            let track = self
                .tracks
                .get(t as usize)
                .ok_or(EditError::IndexOutOfRange {
                    what: "track",
                    index: t,
                })?;
            if track.rows().is_none() {
                return Err(EditError::TrackNotNotes { index: t });
            }
        }
        // Snapshot every touched Track up-front so undo is a
        // straight per-track restore. (Avoids the `Module::Clone`
        // we don't have.)
        let before: Vec<Track> = tracks
            .iter()
            .map(|&t| self.tracks[t as usize].clone())
            .collect();

        for (i, &t) in tracks.iter().enumerate() {
            let track = &mut self.tracks[t as usize];
            let rows = track
                .rows_mut()
                .ok_or(EditError::TrackNotNotes { index: t })?;
            let cells = &content[i];
            let need = (row_offset as usize) + cells.len();
            if rows.len() < need {
                rows.resize(need, Cell::default());
            }
            for (j, src) in cells.iter().enumerate() {
                let row = row_offset as usize + j;
                match mode {
                    PasteMode::Overwrite => {
                        rows[row] = src.clone();
                    }
                    PasteMode::Mix => {
                        let tgt = &mut rows[row];
                        if !matches!(src.event, CellEvent::None) {
                            tgt.event = src.event;
                        }
                        tgt.effects.extend_from_slice(&src.effects);
                    }
                }
            }
        }
        observer.on_change(ChangeEvent::TracksChanged {
            indices: tracks.clone(),
        });
        // Reuse `TracksAndClipsSnapshot` for undo: we touch tracks
        // only, but the existing variant restores both vectors.
        // Clips are unchanged so this is benign.
        let before_clips = self.clips.snapshot();
        // Reconstruct a full `tracks` snapshot by patching the
        // touched indices into the current vector cloned for the
        // others.
        let mut full_before = self.tracks.clone();
        for (i, &t) in tracks.iter().enumerate() {
            full_before[t as usize] = before[i].clone();
        }
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::TracksAndClipsSnapshot {
                tracks: full_before,
                clips: before_clips,
            },
        })
    }

    /// Transpose every NoteOn / NoteOnGhost in the block. Atomic:
    /// validates the entire window first; any pitch that would
    /// fall outside the `Pitch` 0..=119 range aborts the command
    /// with `IndexOutOfRange`. No partial state is committed on
    /// failure.
    fn cmd_transpose_block(
        &mut self,
        cmd: EditCommand,
        tracks: Vec<u32>,
        row_start: u32,
        row_end: u32,
        semitones: i32,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        if row_end <= row_start {
            return Err(EditError::IndexOutOfRange {
                what: "row_end (must be > row_start)",
                index: row_end,
            });
        }
        for &t in &tracks {
            if t as usize >= self.tracks.len() {
                return Err(EditError::IndexOutOfRange {
                    what: "track",
                    index: t,
                });
            }
        }
        // Pre-validation pass: build the (track, row, new_pitch)
        // list. Bails on the first out-of-range pitch.
        let mut updates: Vec<(usize, usize, Pitch, crate::core::fixed::units::Volume, bool)> =
            Vec::new();
        for &t in &tracks {
            let track = &self.tracks[t as usize];
            let rows = match track.rows() {
                Some(r) => r,
                None => return Err(EditError::TrackNotNotes { index: t }),
            };
            #[allow(clippy::needless_range_loop)] // `r` is pushed as an index into `updates`
            for r in (row_start as usize)..(row_end as usize).min(rows.len()) {
                let cell = &rows[r];
                let (pitch, vel, ghost) = match cell.event {
                    CellEvent::NoteOn { pitch, velocity } => (pitch, velocity, false),
                    CellEvent::NoteOnGhost { pitch, velocity } => (pitch, velocity, true),
                    _ => continue,
                };
                let new_val = pitch.value() as i32 + semitones;
                if !(0..=119).contains(&new_val) {
                    return Err(EditError::IndexOutOfRange {
                        what: "pitch (transpose pushes pitch out of [C0, B9])",
                        index: new_val.max(0) as u32,
                    });
                }
                let new_pitch =
                    Pitch::try_from(new_val as u8).map_err(|_| EditError::IndexOutOfRange {
                        what: "pitch (Pitch::try_from failed)",
                        index: new_val as u32,
                    })?;
                updates.push((t as usize, r, new_pitch, vel, ghost));
            }
        }
        // Snapshot the touched tracks for undo.
        let mut touched_idx: Vec<usize> = tracks.iter().map(|&t| t as usize).collect();
        touched_idx.sort_unstable();
        touched_idx.dedup();
        let mut full_before = self.tracks.clone();
        // Apply.
        for (t, r, p, v, ghost) in updates {
            // `rows_mut()` cannot fail here — the validation pass
            // above already rejected non-Notes tracks.
            let rows = self.tracks[t]
                .rows_mut()
                .expect("validated to be Track::Notes above");
            rows[r].event = if ghost {
                CellEvent::NoteOnGhost {
                    pitch: p,
                    velocity: v,
                }
            } else {
                CellEvent::NoteOn {
                    pitch: p,
                    velocity: v,
                }
            };
        }
        // Restore order in `full_before`: nothing else changed,
        // so it's already the pre-state for all other tracks.
        let _ = &mut full_before;
        observer.on_change(ChangeEvent::TracksChanged {
            indices: tracks.clone(),
        });
        let before_clips = self.clips.snapshot();
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::TracksAndClipsSnapshot {
                tracks: full_before,
                clips: before_clips,
            },
        })
    }

    /// Pull the scalar value of `kind` from `cell.effects`, if
    /// present. The first matching effect wins. Used as the
    /// endpoint sampler for [`Self::cmd_interpolate_effect`].
    fn pluck_scalar(cell: &Cell, kind: InterpolateEffectKind) -> Option<i16> {
        for fx in &cell.effects {
            match (kind, fx) {
                (InterpolateEffectKind::Volume, TrackEffect::Volume { value, .. }) => {
                    return Some(value.as_q15_i16());
                }
                (InterpolateEffectKind::Panning, TrackEffect::Panning(p)) => {
                    return Some(p.as_q15_i16());
                }
                (InterpolateEffectKind::ChannelVolume, TrackEffect::ChannelVolume(cv)) => {
                    return Some(cv.as_q15_i16());
                }
                _ => {}
            }
        }
        None
    }

    /// Linearly interpolate the chosen scalar effect across
    /// `[row_start, row_end)` of `track`. Endpoints (`row_start`
    /// and `row_end - 1`) must already carry the effect — they
    /// anchor the lerp. Existing instances of the same kind on
    /// interior rows are replaced; other effects are preserved.
    fn cmd_interpolate_effect(
        &mut self,
        cmd: EditCommand,
        track: u32,
        row_start: u32,
        row_end: u32,
        kind: InterpolateEffectKind,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        if row_end <= row_start + 1 {
            return Err(EditError::IndexOutOfRange {
                what: "row_end (must be > row_start + 1 for a non-empty interior)",
                index: row_end,
            });
        }
        let row_end_idx = row_end as usize - 1;

        // Full validation up-front (no mutation, no clone) so an
        // early Err doesn't pay for a Vec<Cell> allocation.
        let t_ref = self
            .tracks
            .get(track as usize)
            .ok_or(EditError::IndexOutOfRange {
                what: "track",
                index: track,
            })?;
        let rows_ref = t_ref
            .rows()
            .ok_or(EditError::TrackNotNotes { index: track })?;
        if row_end_idx >= rows_ref.len() {
            return Err(EditError::RowOutOfRange {
                track,
                requested: row_end,
                length: rows_ref.len() as u32,
            });
        }
        let start_val = Self::pluck_scalar(&rows_ref[row_start as usize], kind).ok_or(
            EditError::IndexOutOfRange {
                what: "row_start (endpoint must carry the matching effect)",
                index: row_start,
            },
        )?;
        let end_val =
            Self::pluck_scalar(&rows_ref[row_end_idx], kind).ok_or(EditError::IndexOutOfRange {
                what: "row_end - 1 (endpoint must carry the matching effect)",
                index: row_end - 1,
            })?;

        // All checks passed — now we commit (clone for undo + mutate).
        let t = &mut self.tracks[track as usize];
        let before = t.clone();
        let rows = t.rows_mut().expect("checked above");

        let span = (row_end_idx - row_start as usize) as i32; // ≥ 1
        #[allow(clippy::needless_range_loop)]
        // `r` is used both to index `rows` and to compute the lerp offset
        for r in (row_start as usize + 1)..row_end_idx {
            let i = (r - row_start as usize) as i32;
            let val_i32 = start_val as i32 + ((end_val as i32 - start_val as i32) * i) / span;
            let val = val_i32.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            let new_fx = Self::build_interpolated_effect(kind, val);
            // Replace any existing effect of the same kind on this
            // row; otherwise push.
            let row = &mut rows[r];
            let mut replaced = false;
            for fx in row.effects.iter_mut() {
                if Self::effect_matches_kind(fx, kind) {
                    *fx = new_fx.clone();
                    replaced = true;
                    break;
                }
            }
            if !replaced {
                row.effects.push(new_fx);
            }
        }
        observer.on_change(ChangeEvent::TracksChanged {
            indices: alloc::vec![track],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::TrackContentBefore {
                index: track,
                track: before,
            },
        })
    }

    #[inline]
    fn build_interpolated_effect(kind: InterpolateEffectKind, val: i16) -> TrackEffect {
        match kind {
            InterpolateEffectKind::Volume => TrackEffect::Volume {
                value: crate::core::fixed::units::Volume::from_q15_i16(val),
                tick: 0,
            },
            InterpolateEffectKind::Panning => {
                TrackEffect::Panning(crate::core::fixed::units::Panning::from_q15_i16(val))
            }
            InterpolateEffectKind::ChannelVolume => TrackEffect::ChannelVolume(
                crate::core::fixed::units::ChannelVolume::from_q15_i16(val),
            ),
        }
    }

    #[inline]
    fn effect_matches_kind(fx: &TrackEffect, kind: InterpolateEffectKind) -> bool {
        matches!(
            (kind, fx),
            (InterpolateEffectKind::Volume, TrackEffect::Volume { .. })
                | (InterpolateEffectKind::Panning, TrackEffect::Panning(_))
                | (
                    InterpolateEffectKind::ChannelVolume,
                    TrackEffect::ChannelVolume(_),
                )
        )
    }

    fn cmd_create_automation_lane(
        &mut self,
        cmd: EditCommand,
        target: AutomationTarget,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let lane = AutomationLane::new(target);
        let idx = self.automation.len() as u32;
        self.automation.push(lane);
        observer.on_change(ChangeEvent::AutomationChanged {
            lane_indices: alloc::vec![idx],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::LaneCreated { index: idx },
        })
    }

    fn cmd_delete_automation_lane(
        &mut self,
        cmd: EditCommand,
        lane: u32,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let idx = lane as usize;
        if idx >= self.automation.len() {
            return Err(EditError::IndexOutOfRange {
                what: "lane",
                index: lane,
            });
        }
        let removed = self.automation.remove(idx);
        observer.on_change(ChangeEvent::AutomationChanged {
            lane_indices: alloc::vec![lane],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::LaneDeleted {
                index: lane,
                lane: removed,
            },
        })
    }

    fn cmd_set_automation_lane_enabled(
        &mut self,
        cmd: EditCommand,
        lane: u32,
        enabled: bool,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let l = self
            .automation
            .get_mut(lane as usize)
            .ok_or(EditError::IndexOutOfRange {
                what: "lane",
                index: lane,
            })?;
        let before = l.clone();
        l.enabled = enabled;
        observer.on_change(ChangeEvent::AutomationChanged {
            lane_indices: alloc::vec![lane],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::LaneContentBefore {
                index: lane,
                lane: before,
            },
        })
    }

    fn cmd_add_automation_point(
        &mut self,
        cmd: EditCommand,
        lane: u32,
        point: AutomationPoint,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let l = self
            .automation
            .get_mut(lane as usize)
            .ok_or(EditError::IndexOutOfRange {
                what: "lane",
                index: lane,
            })?;
        if !point.value.is_compatible_with(l.target) {
            return Err(EditError::AutomationTypeMismatch {
                target: l.target,
                got: point.value,
            });
        }
        let before = l.clone();
        l.insert_point(point);
        observer.on_change(ChangeEvent::AutomationChanged {
            lane_indices: alloc::vec![lane],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::LaneContentBefore {
                index: lane,
                lane: before,
            },
        })
    }

    fn cmd_delete_automation_point(
        &mut self,
        cmd: EditCommand,
        lane: u32,
        point_index: u32,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let l = self
            .automation
            .get_mut(lane as usize)
            .ok_or(EditError::IndexOutOfRange {
                what: "lane",
                index: lane,
            })?;
        // The point-level commands only apply to `LaneKind::Points`
        // — Lfo / Slide / Glide kinds have their own event mutators
        // (Phase 3b API surface).
        let points_len = l
            .points()
            .ok_or(EditError::IndexOutOfRange {
                what: "point (lane is not a Points lane)",
                index: point_index,
            })?
            .len();
        if (point_index as usize) >= points_len {
            return Err(EditError::IndexOutOfRange {
                what: "point",
                index: point_index,
            });
        }
        let before = l.clone();
        l.points_mut()
            .expect("kind is Points (checked above)")
            .remove(point_index as usize);
        observer.on_change(ChangeEvent::AutomationChanged {
            lane_indices: alloc::vec![lane],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::LaneContentBefore {
                index: lane,
                lane: before,
            },
        })
    }

    fn cmd_move_automation_point(
        &mut self,
        cmd: EditCommand,
        lane: u32,
        point_index: u32,
        new_tick: u32,
        new_value: AutomationValue,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let l = self
            .automation
            .get_mut(lane as usize)
            .ok_or(EditError::IndexOutOfRange {
                what: "lane",
                index: lane,
            })?;
        let points_len = l
            .points()
            .ok_or(EditError::IndexOutOfRange {
                what: "point (lane is not a Points lane)",
                index: point_index,
            })?
            .len();
        if (point_index as usize) >= points_len {
            return Err(EditError::IndexOutOfRange {
                what: "point",
                index: point_index,
            });
        }
        if !new_value.is_compatible_with(l.target) {
            return Err(EditError::AutomationTypeMismatch {
                target: l.target,
                got: new_value,
            });
        }
        let before = l.clone();
        l.points_mut()
            .expect("kind is Points (checked above)")
            .remove(point_index as usize);
        l.insert_point(AutomationPoint {
            tick: new_tick,
            value: new_value,
        });
        observer.on_change(ChangeEvent::AutomationChanged {
            lane_indices: alloc::vec![lane],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::LaneContentBefore {
                index: lane,
                lane: before,
            },
        })
    }

    /// Insert an [`LfoEvent`] into an `Lfo`-kind lane,
    /// maintaining the by-tick sort. Rejects when the lane is a
    /// different kind.
    /// Shared body of the three `cmd_add_X_event` helpers
    /// (`X ∈ {lfo, slide, glide}`). `extract` returns the inner
    /// event Vec for the expected lane kind (or `None` on mismatch);
    /// `tick_of` reads the per-element tick used for sorted insertion.
    #[allow(clippy::too_many_arguments)] // Three of these are closures wiring the dispatch.
    fn cmd_add_lane_event<E, F, T>(
        &mut self,
        cmd: EditCommand,
        lane: u32,
        event: E,
        kind_label: &'static str,
        extract: F,
        tick_of: T,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError>
    where
        F: FnOnce(&mut LaneKind) -> Option<&mut Vec<E>>,
        T: Fn(&E) -> u32,
    {
        let l = self
            .automation
            .get_mut(lane as usize)
            .ok_or(EditError::IndexOutOfRange {
                what: "lane",
                index: lane,
            })?;
        let before = l.clone();
        let new_tick = tick_of(&event);
        let events = extract(&mut l.kind).ok_or(EditError::IndexOutOfRange {
            what: kind_label,
            index: lane,
        })?;
        let pos = events
            .binary_search_by_key(&new_tick, &tick_of)
            .unwrap_or_else(|i| i);
        events.insert(pos, event);
        observer.on_change(ChangeEvent::AutomationChanged {
            lane_indices: alloc::vec![lane],
        });
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::LaneContentBefore {
                index: lane,
                lane: before,
            },
        })
    }

    fn cmd_add_lfo_event(
        &mut self,
        cmd: EditCommand,
        lane: u32,
        event: LfoEvent,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        self.cmd_add_lane_event(
            cmd,
            lane,
            event,
            "lane (kind is not Lfo)",
            |k| match k {
                LaneKind::Lfo { events } => Some(events),
                _ => None,
            },
            |e| e.tick(),
            observer,
        )
    }

    /// Insert a [`SlideEvent`] into a `Slide`-kind lane.
    fn cmd_add_slide_event(
        &mut self,
        cmd: EditCommand,
        lane: u32,
        event: SlideEvent,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        self.cmd_add_lane_event(
            cmd,
            lane,
            event,
            "lane (kind is not Slide)",
            |k| match k {
                LaneKind::Slide { events } => Some(events),
                _ => None,
            },
            |e| e.tick(),
            observer,
        )
    }

    /// Insert a [`GlideEvent`] into a `Glide`-kind lane.
    fn cmd_add_glide_event(
        &mut self,
        cmd: EditCommand,
        lane: u32,
        event: GlideEvent,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        self.cmd_add_lane_event(
            cmd,
            lane,
            event,
            "lane (kind is not Glide)",
            |k| match k {
                LaneKind::Glide { events } => Some(events),
                _ => None,
            },
            |e| e.tick(),
            observer,
        )
    }

    /// Shared envelope for the five "set one global field" commands.
    /// Captures the snapshot of the affected field, applies the
    /// mutation, then emits a `GlobalSettingChanged` event. All five
    /// callers below are one-liners over this helper.
    fn cmd_set_global<F, G>(
        &mut self,
        cmd: EditCommand,
        observer: &mut dyn EditObserver,
        snap_old: F,
        apply: G,
    ) -> Result<EditReceipt, EditError>
    where
        F: FnOnce(&Self) -> GlobalSettingSnapshot,
        G: FnOnce(&mut Self),
    {
        let snap = snap_old(self);
        apply(self);
        observer.on_change(ChangeEvent::GlobalSettingChanged);
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::GlobalSettingBefore(snap),
        })
    }

    fn cmd_set_default_bpm(
        &mut self,
        cmd: EditCommand,
        bpm: usize,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        self.cmd_set_global(
            cmd,
            observer,
            |m| GlobalSettingSnapshot {
                default_bpm: Some(m.default_bpm),
                ..Default::default()
            },
            |m| m.default_bpm = bpm,
        )
    }

    fn cmd_set_default_speed(
        &mut self,
        cmd: EditCommand,
        speed: usize,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        self.cmd_set_global(
            cmd,
            observer,
            |m| GlobalSettingSnapshot {
                default_speed: Some(m.default_tempo),
                ..Default::default()
            },
            |m| m.default_tempo = speed,
        )
    }

    fn cmd_set_playback_quirks(
        &mut self,
        cmd: EditCommand,
        quirks: PlaybackQuirks,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        self.cmd_set_global(
            cmd,
            observer,
            |m| GlobalSettingSnapshot {
                quirks: Some(m.quirks),
                ..Default::default()
            },
            |m| m.quirks = quirks,
        )
    }

    fn cmd_set_module_name(
        &mut self,
        cmd: EditCommand,
        name: String,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        self.cmd_set_global(
            cmd,
            observer,
            |m| GlobalSettingSnapshot {
                name: Some(m.name.clone()),
                ..Default::default()
            },
            |m| m.name = name,
        )
    }

    fn cmd_set_pattern_highlight(
        &mut self,
        cmd: EditCommand,
        highlight: PatternHighlight,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        self.cmd_set_global(
            cmd,
            observer,
            |m| GlobalSettingSnapshot {
                pattern_highlight: Some(m.pattern_highlight),
                ..Default::default()
            },
            |m| m.pattern_highlight = highlight,
        )
    }

    fn cmd_set_euclidean_humanize(
        &mut self,
        cmd: EditCommand,
        track: u32,
        advance_max_ticks: u8,
        probability: Q15,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let t = self
            .tracks
            .get_mut(track as usize)
            .ok_or(EditError::IndexOutOfRange {
                what: "track",
                index: track,
            })?;
        let (max_ticks, prob) = match t {
            Track::Euclidean {
                humanize_advance_max_ticks,
                humanize_probability,
                ..
            } => (humanize_advance_max_ticks, humanize_probability),
            _ => return Err(EditError::TrackNotEuclidean { index: track }),
        };
        let before = EditUndoData::EuclideanHumanizeBefore {
            track,
            advance_max_ticks: *max_ticks,
            probability: *prob,
        };
        *max_ticks = advance_max_ticks;
        *prob = probability;
        observer.on_change(ChangeEvent::GlobalSettingChanged);
        Ok(EditReceipt {
            command: cmd,
            undo_data: before,
        })
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use alloc::vec;

    fn fresh_module() -> Module {
        let mut m = Module::default();
        // Add one Instrument so create_track checks pass.
        m.instrument.push(Instrument {
            name: "test".into(),
            instr_type: InstrumentType::Empty,
            muted: false,
        });
        m
    }

    fn module_signature(m: &Module) -> alloc::string::String {
        use core::fmt::Write;
        let mut s = alloc::string::String::new();
        let _ = write!(
            &mut s,
            "name={} bpm={} speed={} tracks={} clips={} lanes={}",
            m.name,
            m.default_bpm,
            m.default_tempo,
            m.tracks.len(),
            m.clips.len(),
            m.automation.len(),
        );
        for (i, t) in m.tracks.iter().enumerate() {
            let _ = write!(
                &mut s,
                " t{}={{name={} instr={} rows={} muted={}}}",
                i,
                t.name(),
                t.instrument(),
                t.natural_length(),
                t.muted(),
            );
        }
        for (i, c) in m.clips.iter().enumerate() {
            let _ = write!(
                &mut s,
                " c{}={{track={} song={} ch={} t={} off={}}}",
                i, c.track, c.song, c.target_channel, c.position_tick, c.track_row_offset,
            );
        }
        for (i, l) in m.automation.iter().enumerate() {
            let points_len = l.points().map(|p| p.len()).unwrap_or(0);
            let _ = write!(
                &mut s,
                " l{}={{target={:?} points={} enabled={}}}",
                i, l.target, points_len, l.enabled,
            );
        }
        s
    }

    fn check_apply_undo_identity(m: &Module, cmds: Vec<EditCommand>) {
        let before = module_signature(m);
        for cmd in cmds {
            let mut copy = clone_module(m);
            let receipt = copy.apply(cmd.clone()).expect("apply");
            copy.undo(&receipt).expect("undo");
            let after = module_signature(&copy);
            assert_eq!(before, after, "apply/undo not identity for {:?}", cmd);
        }
    }

    fn clone_module(m: &Module) -> Module {
        // Module doesn't impl Clone (would require Instrument: Clone),
        // so we hand-copy the fields the tests actually touch.
        let mut c = Module {
            name: m.name.clone(),
            default_bpm: m.default_bpm,
            default_tempo: m.default_tempo,
            quirks: m.quirks,
            tracks: m.tracks.clone(),
            clips: m.clips.clone(),
            automation: m.automation.clone(),
            timeline_map: m.timeline_map.clone(),
            ..Module::default()
        };
        // instrument is not Clone; carry an empty fake to keep
        // command validation happy.
        for _ in &m.instrument {
            c.instrument.push(Instrument {
                name: alloc::string::String::new(),
                instr_type: InstrumentType::Empty,
                muted: false,
            });
        }
        c
    }

    #[test]
    fn create_then_undo_track() {
        let mut m = fresh_module();
        let receipt = m
            .apply(EditCommand::CreateTrack {
                instrument: 0,
                name: "lead".into(),
                length_rows: 16,
            })
            .unwrap();
        assert_eq!(m.tracks.len(), 1);
        m.undo(&receipt).unwrap();
        assert_eq!(m.tracks.len(), 0);
    }

    #[test]
    fn create_clip_then_delete_then_undo() {
        let mut m = fresh_module();
        m.apply(EditCommand::CreateTrack {
            instrument: 0,
            name: "lead".into(),
            length_rows: 16,
        })
        .unwrap();
        let r1 = m
            .apply(EditCommand::CreateClip {
                track: 0,
                song: 0,
                target_channel: 0,
                position_tick: 0,
            })
            .unwrap();
        assert_eq!(m.clips.len(), 1);
        let r2 = m.apply(EditCommand::DeleteClip { clip: 0 }).unwrap();
        assert_eq!(m.clips.len(), 0);
        m.undo(&r2).unwrap();
        assert_eq!(m.clips.len(), 1);
        m.undo(&r1).unwrap();
        assert_eq!(m.clips.len(), 0);
    }

    #[test]
    fn overlap_rejected_on_same_channel() {
        let mut m = fresh_module();
        m.apply(EditCommand::CreateTrack {
            instrument: 0,
            name: "a".into(),
            length_rows: 8,
        })
        .unwrap();
        m.apply(EditCommand::CreateTrack {
            instrument: 0,
            name: "b".into(),
            length_rows: 8,
        })
        .unwrap();
        m.apply(EditCommand::CreateClip {
            track: 0,
            song: 0,
            target_channel: 0,
            position_tick: 0,
        })
        .unwrap();
        let err = m.apply(EditCommand::CreateClip {
            track: 1,
            song: 0,
            target_channel: 0,
            position_tick: 0,
        });
        assert!(matches!(err, Err(EditError::ClipOverlap { .. })));
    }

    #[test]
    fn cannot_delete_track_in_use() {
        let mut m = fresh_module();
        m.apply(EditCommand::CreateTrack {
            instrument: 0,
            name: "a".into(),
            length_rows: 8,
        })
        .unwrap();
        m.apply(EditCommand::CreateClip {
            track: 0,
            song: 0,
            target_channel: 0,
            position_tick: 0,
        })
        .unwrap();
        let err = m.apply(EditCommand::DeleteTrack { track: 0 });
        assert!(matches!(err, Err(EditError::TrackInUse { .. })));
    }

    #[test]
    fn automation_value_type_mismatch_rejected() {
        let mut m = fresh_module();
        m.apply(EditCommand::CreateAutomationLane {
            target: AutomationTarget::Bpm,
        })
        .unwrap();
        let err = m.apply(EditCommand::AddAutomationPoint {
            lane: 0,
            point: AutomationPoint {
                tick: 0,
                value: AutomationValue::Speed(6),
            },
        });
        assert!(matches!(err, Err(EditError::AutomationTypeMismatch { .. })));
    }

    #[test]
    fn apply_then_undo_is_identity_on_representative_commands() {
        let m = fresh_module();
        // Build a starting module with a track + clip + lane.
        let mut prepared = clone_module(&m);
        prepared
            .apply(EditCommand::CreateTrack {
                instrument: 0,
                name: "lead".into(),
                length_rows: 16,
            })
            .unwrap();
        prepared
            .apply(EditCommand::CreateClip {
                track: 0,
                song: 0,
                target_channel: 0,
                position_tick: 0,
            })
            .unwrap();
        prepared
            .apply(EditCommand::CreateAutomationLane {
                target: AutomationTarget::Bpm,
            })
            .unwrap();

        let cmds: Vec<EditCommand> = vec![
            EditCommand::RenameTrack {
                track: 0,
                new_name: "bass".into(),
            },
            EditCommand::SetTrackMuted {
                track: 0,
                muted: true,
            },
            EditCommand::ResizeTrack {
                track: 0,
                new_length_rows: 32,
            },
            EditCommand::SetCell {
                track: 0,
                row_offset: 5,
                content: Cell::default(),
            },
            EditCommand::SetDefaultBpm { bpm: 200 },
            EditCommand::SetDefaultSpeed { speed: 4 },
            EditCommand::SetModuleName {
                name: "hello".into(),
            },
            EditCommand::AddAutomationPoint {
                lane: 0,
                point: AutomationPoint {
                    tick: 100,
                    value: AutomationValue::Bpm(180),
                },
            },
        ];

        check_apply_undo_identity(&prepared, cmds);
    }

    #[test]
    fn humanize_set_then_undo_round_trips() {
        let mut m = Module::default();
        m.instrument.push(Instrument::default());
        m.tracks.push(Track::Euclidean {
            name: "ekn".into(),
            instrument: 0,
            muted: false,
            events: 3,
            steps: 8,
            rotation: 0,
            prototype: Cell::default(),
            humanize_advance_max_ticks: 0,
            humanize_probability: Q15::ZERO,
        });

        let receipt = m
            .apply(EditCommand::SetEuclideanHumanize {
                track: 0,
                advance_max_ticks: 4,
                probability: Q15::HALF,
            })
            .unwrap();
        if let Track::Euclidean {
            humanize_advance_max_ticks,
            humanize_probability,
            ..
        } = &m.tracks[0]
        {
            assert_eq!(*humanize_advance_max_ticks, 4);
            assert_eq!(*humanize_probability, Q15::HALF);
        } else {
            panic!("expected Euclidean");
        }
        m.undo(&receipt).unwrap();
        if let Track::Euclidean {
            humanize_advance_max_ticks,
            humanize_probability,
            ..
        } = &m.tracks[0]
        {
            assert_eq!(*humanize_advance_max_ticks, 0);
            assert_eq!(*humanize_probability, Q15::ZERO);
        } else {
            panic!("expected Euclidean after undo");
        }
    }

    #[test]
    fn humanize_rejects_non_euclidean_track() {
        let mut m = fresh_module();
        // CreateTrack always produces Track::Notes.
        m.apply(EditCommand::CreateTrack {
            instrument: 0,
            name: "n".into(),
            length_rows: 4,
        })
        .unwrap();
        let err = m.apply(EditCommand::SetEuclideanHumanize {
            track: 0,
            advance_max_ticks: 4,
            probability: Q15::HALF,
        });
        assert!(matches!(err, Err(EditError::TrackNotEuclidean { .. })));
    }

    #[test]
    fn humanize_rejects_missing_track() {
        let mut m = Module::default();
        let err = m.apply(EditCommand::SetEuclideanHumanize {
            track: 0,
            advance_max_ticks: 4,
            probability: Q15::HALF,
        });
        assert!(matches!(err, Err(EditError::IndexOutOfRange { .. })));
    }

    // =====================================================================
    // Phase 4 — Clip editing: Split / Resize / Slip
    // =====================================================================

    /// Build a module with one Track and one Clip placed at tick
    /// 0 with `end_tick = 100`, speed 5. Useful base for the
    /// Split / Resize / Slip tests.
    fn module_with_one_clip() -> Module {
        let mut m = fresh_module();
        m.apply(EditCommand::CreateTrack {
            instrument: 0,
            name: "t0".into(),
            length_rows: 32,
        })
        .expect("create_track");
        m.apply(EditCommand::CreateClip {
            track: 0,
            song: 0,
            target_channel: 0,
            position_tick: 0,
        })
        .expect("create_clip");
        // The created clip's end_tick is computed from track length
        // × default speed inside `cmd_create_clip`. Pull it via
        // ResizeClip to a stable 100 so the tests can reason about
        // tick positions.
        m.apply(EditCommand::ResizeClip {
            clip: 0,
            new_end_tick: 100,
        })
        .expect("resize_clip");
        // Pin a known `speed_at_start` for split's row math by
        // directly mutating (no edit command exposes this — the
        // value is set once at clip creation).
        m.clips.modify(0, |c| c.speed_at_start = 5);
        m
    }

    #[test]
    fn split_clip_creates_two_segments_at_tick() {
        let mut m = module_with_one_clip();
        let before_count = m.clips.len();
        m.apply(EditCommand::SplitClip {
            clip: 0,
            at_tick: 50,
        })
        .expect("split");
        assert_eq!(m.clips.len(), before_count + 1);
        let first = m.clips.get(0).unwrap();
        let second = m
            .clips
            .iter()
            .find(|c| c.position_tick == 50)
            .expect("second segment starts at 50");
        assert_eq!(first.position_tick, 0);
        assert_eq!(first.end_tick, 50);
        assert_eq!(second.end_tick, 100);
        assert_eq!(second.track, first.track);
        assert_eq!(second.target_channel, first.target_channel);
        // 50 ticks / speed=5 = 10 source rows skipped at the split.
        assert_eq!(second.source_start_row, first.source_start_row + 10);
    }

    #[test]
    fn split_clip_rejects_boundary_ticks() {
        let mut m = module_with_one_clip();
        // at_tick == position_tick → reject.
        assert!(m
            .apply(EditCommand::SplitClip {
                clip: 0,
                at_tick: 0
            })
            .is_err());
        // at_tick == end_tick → reject.
        assert!(m
            .apply(EditCommand::SplitClip {
                clip: 0,
                at_tick: 100
            })
            .is_err());
    }

    #[test]
    fn split_clip_undo_restores_single_clip() {
        let mut m = module_with_one_clip();
        let before = module_signature(&m);
        let r = m
            .apply(EditCommand::SplitClip {
                clip: 0,
                at_tick: 50,
            })
            .expect("split");
        m.undo(&r).expect("undo");
        assert_eq!(module_signature(&m), before);
    }

    #[test]
    fn resize_clip_extends_end_tick() {
        let mut m = module_with_one_clip();
        m.apply(EditCommand::ResizeClip {
            clip: 0,
            new_end_tick: 200,
        })
        .expect("resize");
        assert_eq!(m.clips.get(0).unwrap().end_tick, 200);
    }

    #[test]
    fn resize_clip_rejects_invalid_end_tick() {
        let mut m = module_with_one_clip();
        // end <= position → reject.
        assert!(m
            .apply(EditCommand::ResizeClip {
                clip: 0,
                new_end_tick: 0,
            })
            .is_err());
    }

    #[test]
    fn resize_clip_rejects_overlap() {
        let mut m = module_with_one_clip();
        // Add a second clip on the same channel further out.
        m.apply(EditCommand::CreateClip {
            track: 0,
            song: 0,
            target_channel: 0,
            position_tick: 200,
        })
        .expect("create second clip");
        // Trying to extend the first clip past 200 must overlap.
        let res = m.apply(EditCommand::ResizeClip {
            clip: 0,
            new_end_tick: 250,
        });
        assert!(matches!(res, Err(EditError::ClipOverlap { .. })));
    }

    #[test]
    fn resize_clip_undo() {
        let mut m = module_with_one_clip();
        let before = module_signature(&m);
        let r = m
            .apply(EditCommand::ResizeClip {
                clip: 0,
                new_end_tick: 200,
            })
            .expect("resize");
        m.undo(&r).expect("undo");
        assert_eq!(module_signature(&m), before);
    }

    #[test]
    fn slip_clip_advances_source_start_row() {
        let mut m = module_with_one_clip();
        let orig_row = m.clips.get(0).unwrap().source_start_row;
        m.apply(EditCommand::SlipClip {
            clip: 0,
            delta_rows: 4,
        })
        .expect("slip");
        assert_eq!(m.clips.get(0).unwrap().source_start_row, orig_row + 4);
    }

    #[test]
    fn slip_clip_negative_underflow_rejected() {
        let mut m = module_with_one_clip();
        // source_start_row starts at 0 → any negative delta
        // underflows.
        let res = m.apply(EditCommand::SlipClip {
            clip: 0,
            delta_rows: -1,
        });
        assert!(matches!(res, Err(EditError::IndexOutOfRange { .. })));
    }

    #[test]
    fn slip_clip_undo() {
        let mut m = module_with_one_clip();
        let before = module_signature(&m);
        let r = m
            .apply(EditCommand::SlipClip {
                clip: 0,
                delta_rows: 4,
            })
            .expect("slip");
        m.undo(&r).expect("undo");
        assert_eq!(module_signature(&m), before);
    }

    // =====================================================================
    // Phase 4 — Block editing: copy_block + PasteBlock + TransposeBlock
    // =====================================================================

    fn module_with_two_tracks() -> Module {
        let mut m = fresh_module();
        m.apply(EditCommand::CreateTrack {
            instrument: 0,
            name: "a".into(),
            length_rows: 8,
        })
        .unwrap();
        m.apply(EditCommand::CreateTrack {
            instrument: 0,
            name: "b".into(),
            length_rows: 8,
        })
        .unwrap();
        // Seed row 0 of track 0 with a NoteOn C5.
        let c5 = Cell {
            event: CellEvent::NoteOn {
                pitch: Pitch::C5,
                velocity: crate::core::fixed::units::Volume::FULL,
            },
            ..Cell::default()
        };
        m.tracks[0].expect_notes_rows_mut()[0] = c5;
        // Seed row 1 of track 0 with NoteOff.
        m.tracks[0].expect_notes_rows_mut()[1].event = CellEvent::NoteOff { retrig: false };
        // Seed row 0 of track 1 with NoteOnGhost E5.
        m.tracks[1].expect_notes_rows_mut()[0] = Cell {
            event: CellEvent::NoteOnGhost {
                pitch: Pitch::E5,
                velocity: crate::core::fixed::units::Volume::FULL,
            },
            ..Cell::default()
        };
        m
    }

    #[test]
    fn copy_block_extracts_rectangular_window() {
        let m = module_with_two_tracks();
        let block = m.copy_block(&[0, 1], 0, 2);
        assert_eq!(block.len(), 2, "two tracks");
        assert_eq!(block[0].len(), 2, "two rows per track");
        assert!(matches!(block[0][0].event, CellEvent::NoteOn { .. }));
        assert!(matches!(block[0][1].event, CellEvent::NoteOff { .. }));
        assert!(matches!(block[1][0].event, CellEvent::NoteOnGhost { .. }));
        assert!(matches!(block[1][1].event, CellEvent::None));
    }

    #[test]
    fn copy_block_past_end_returns_default_padding() {
        let m = module_with_two_tracks();
        let block = m.copy_block(&[0], 7, 10);
        assert_eq!(block[0].len(), 3);
        // Row 7 exists (default). Rows 8 and 9 are past end → default.
        for c in &block[0] {
            assert!(matches!(c.event, CellEvent::None));
        }
    }

    #[test]
    fn paste_block_overwrite_replaces_cells() {
        let mut m = module_with_two_tracks();
        let src = m.copy_block(&[0], 0, 2);
        let r = m
            .apply(EditCommand::PasteBlock {
                tracks: vec![1],
                row_offset: 2,
                content: src,
                mode: PasteMode::Overwrite,
            })
            .expect("paste");
        assert!(matches!(
            m.tracks[1].expect_notes_rows_mut()[2].event,
            CellEvent::NoteOn { .. }
        ));
        assert!(matches!(
            m.tracks[1].expect_notes_rows_mut()[3].event,
            CellEvent::NoteOff { .. }
        ));
        // Undo restores the original cells.
        let before = module_signature(&m);
        let r2 = m
            .apply(EditCommand::PasteBlock {
                tracks: vec![1],
                row_offset: 4,
                content: vec![vec![Cell {
                    event: CellEvent::InstrReset,
                    ..Cell::default()
                }]],
                mode: PasteMode::Overwrite,
            })
            .expect("paste 2");
        assert!(matches!(
            m.tracks[1].expect_notes_rows_mut()[4].event,
            CellEvent::InstrReset
        ));
        m.undo(&r2).expect("undo paste 2");
        assert_eq!(module_signature(&m), before);
        let _ = r;
    }

    #[test]
    fn paste_block_mix_keeps_target_event_when_source_is_none() {
        let mut m = module_with_two_tracks();
        // Target row 0 of track 1 has NoteOnGhost(E5). Source is
        // a None cell with an effect. Mix should keep NoteOnGhost
        // AND append the effect.
        let mixed_src = Cell {
            event: CellEvent::None,
            effects: vec![TrackEffect::Arpeggio { half1: 1, half2: 2 }],
        };
        m.apply(EditCommand::PasteBlock {
            tracks: vec![1],
            row_offset: 0,
            content: vec![vec![mixed_src]],
            mode: PasteMode::Mix,
        })
        .expect("mix");
        // Event preserved.
        assert!(matches!(
            m.tracks[1].expect_notes_rows_mut()[0].event,
            CellEvent::NoteOnGhost { .. }
        ));
        // Effect appended.
        assert!(matches!(
            m.tracks[1].expect_notes_rows_mut()[0].effects[0],
            TrackEffect::Arpeggio { half1: 1, half2: 2 }
        ));
    }

    #[test]
    fn paste_block_extends_short_track() {
        let mut m = module_with_two_tracks();
        let original_len = m.tracks[1].expect_notes_rows().len();
        // Paste a 4-row block starting at row 6 (last index is 7).
        // The paste extends to 6 + 4 = 10 rows.
        let block: Vec<Vec<Cell>> = vec![vec![Cell::default(); 4]];
        m.apply(EditCommand::PasteBlock {
            tracks: vec![1],
            row_offset: 6,
            content: block,
            mode: PasteMode::Overwrite,
        })
        .expect("paste extending");
        assert!(m.tracks[1].expect_notes_rows().len() >= 10);
        assert!(m.tracks[1].expect_notes_rows().len() > original_len);
    }

    #[test]
    fn paste_block_rejects_mismatched_lengths() {
        let mut m = module_with_two_tracks();
        let res = m.apply(EditCommand::PasteBlock {
            tracks: vec![0, 1],
            row_offset: 0,
            content: vec![vec![Cell::default()]], // only one column for two tracks
            mode: PasteMode::Overwrite,
        });
        assert!(matches!(res, Err(EditError::IndexOutOfRange { .. })));
    }

    #[test]
    fn transpose_block_shifts_pitches_up() {
        let mut m = module_with_two_tracks();
        m.apply(EditCommand::TransposeBlock {
            tracks: vec![0, 1],
            row_start: 0,
            row_end: 2,
            semitones: 12, // +1 octave
        })
        .expect("transpose");
        // Track 0 row 0 was C5 (=60). After +12, should be C6 (=72).
        match m.tracks[0].expect_notes_rows_mut()[0].event {
            CellEvent::NoteOn { pitch, .. } => assert_eq!(pitch.value(), Pitch::C6.value()),
            _ => panic!("expected NoteOn"),
        }
        // Track 1 row 0 was NoteOnGhost(E5 = 64). After +12 → E6 (76).
        match m.tracks[1].expect_notes_rows_mut()[0].event {
            CellEvent::NoteOnGhost { pitch, .. } => {
                assert_eq!(pitch.value(), Pitch::E6.value())
            }
            _ => panic!("expected NoteOnGhost"),
        }
        // Track 0 row 1 was NoteOff — must stay NoteOff.
        assert!(matches!(
            m.tracks[0].expect_notes_rows_mut()[1].event,
            CellEvent::NoteOff { .. }
        ));
    }

    #[test]
    fn transpose_block_rejects_out_of_range() {
        let mut m = module_with_two_tracks();
        // C5 (60) + 80 = 140 > 119 → reject.
        let res = m.apply(EditCommand::TransposeBlock {
            tracks: vec![0],
            row_start: 0,
            row_end: 2,
            semitones: 80,
        });
        assert!(matches!(res, Err(EditError::IndexOutOfRange { .. })));
        // The cell wasn't touched.
        match m.tracks[0].expect_notes_rows_mut()[0].event {
            CellEvent::NoteOn { pitch, .. } => assert_eq!(pitch.value(), Pitch::C5.value()),
            _ => panic!("expected unchanged NoteOn"),
        }
    }

    #[test]
    fn transpose_block_undo_restores_pitches() {
        let mut m = module_with_two_tracks();
        let before = module_signature(&m);
        let r = m
            .apply(EditCommand::TransposeBlock {
                tracks: vec![0, 1],
                row_start: 0,
                row_end: 2,
                semitones: 5,
            })
            .expect("transpose");
        m.undo(&r).expect("undo");
        assert_eq!(module_signature(&m), before);
    }

    // =====================================================================
    // Phase 4 — InterpolateEffect
    // =====================================================================

    fn module_with_volume_endpoints() -> Module {
        let mut m = fresh_module();
        m.apply(EditCommand::CreateTrack {
            instrument: 0,
            name: "t".into(),
            length_rows: 8,
        })
        .unwrap();
        // Endpoints: row 0 = Volume 0.0, row 4 = Volume 1.0.
        m.tracks[0].expect_notes_rows_mut()[0]
            .effects
            .push(TrackEffect::Volume {
                value: crate::core::fixed::units::Volume::SILENT,
                tick: 0,
            });
        m.tracks[0].expect_notes_rows_mut()[4]
            .effects
            .push(TrackEffect::Volume {
                value: crate::core::fixed::units::Volume::FULL,
                tick: 0,
            });
        m
    }

    #[test]
    fn interpolate_volume_lerps_interior_rows() {
        let mut m = module_with_volume_endpoints();
        m.apply(EditCommand::InterpolateEffect {
            track: 0,
            row_start: 0,
            row_end: 5,
            kind: InterpolateEffectKind::Volume,
        })
        .expect("interpolate");
        // Row 0: endpoint, untouched (volume 0).
        let v0 = m.tracks[0].expect_notes_rows_mut()[0]
            .effects
            .iter()
            .find_map(|fx| {
                if let TrackEffect::Volume { value, .. } = fx {
                    Some(value.as_q15_i16())
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(v0, 0);
        // Row 4: endpoint, untouched (volume Q15::ONE raw = i16::MAX = 32767).
        let v4 = m.tracks[0].expect_notes_rows_mut()[4]
            .effects
            .iter()
            .find_map(|fx| {
                if let TrackEffect::Volume { value, .. } = fx {
                    Some(value.as_q15_i16())
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(v4, i16::MAX);
        // Interior rows 1, 2, 3 should be monotonically increasing
        // and roughly equal to i/4 of the way.
        let mut prev = 0i16;
        for r in 1..4 {
            let val = m.tracks[0].expect_notes_rows_mut()[r]
                .effects
                .iter()
                .find_map(|fx| {
                    if let TrackEffect::Volume { value, .. } = fx {
                        Some(value.as_q15_i16())
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| panic!("row {} should have an interpolated Volume", r));
            assert!(val > prev, "row {} val {} not > prev {}", r, val, prev);
            prev = val;
        }
    }

    #[test]
    fn interpolate_rejects_missing_endpoint() {
        let mut m = fresh_module();
        m.apply(EditCommand::CreateTrack {
            instrument: 0,
            name: "t".into(),
            length_rows: 8,
        })
        .unwrap();
        // No Volume effect on either endpoint → reject.
        let res = m.apply(EditCommand::InterpolateEffect {
            track: 0,
            row_start: 0,
            row_end: 5,
            kind: InterpolateEffectKind::Volume,
        });
        assert!(matches!(res, Err(EditError::IndexOutOfRange { .. })));
    }

    #[test]
    fn interpolate_rejects_empty_interior() {
        let mut m = module_with_volume_endpoints();
        // row_end = row_start + 1 → no interior → reject.
        let res = m.apply(EditCommand::InterpolateEffect {
            track: 0,
            row_start: 0,
            row_end: 1,
            kind: InterpolateEffectKind::Volume,
        });
        assert!(matches!(res, Err(EditError::IndexOutOfRange { .. })));
    }

    #[test]
    fn interpolate_replaces_existing_same_kind() {
        let mut m = module_with_volume_endpoints();
        // Pre-seed row 2 with a Volume effect to verify it gets
        // replaced, not duplicated.
        m.tracks[0].expect_notes_rows_mut()[2]
            .effects
            .push(TrackEffect::Volume {
                value: crate::core::fixed::units::Volume::FULL,
                tick: 0,
            });
        m.apply(EditCommand::InterpolateEffect {
            track: 0,
            row_start: 0,
            row_end: 5,
            kind: InterpolateEffectKind::Volume,
        })
        .unwrap();
        // Row 2 should now carry exactly one Volume effect (replaced).
        let volume_count = m.tracks[0].expect_notes_rows_mut()[2]
            .effects
            .iter()
            .filter(|fx| matches!(fx, TrackEffect::Volume { .. }))
            .count();
        assert_eq!(volume_count, 1);
    }

    #[test]
    fn interpolate_preserves_other_effects() {
        let mut m = module_with_volume_endpoints();
        // Pre-seed row 2 with a non-Volume effect.
        m.tracks[0].expect_notes_rows_mut()[2]
            .effects
            .push(TrackEffect::Arpeggio { half1: 4, half2: 7 });
        m.apply(EditCommand::InterpolateEffect {
            track: 0,
            row_start: 0,
            row_end: 5,
            kind: InterpolateEffectKind::Volume,
        })
        .unwrap();
        // Arpeggio survives.
        assert!(m.tracks[0].expect_notes_rows_mut()[2]
            .effects
            .iter()
            .any(|fx| matches!(fx, TrackEffect::Arpeggio { .. })));
        // Volume was added.
        assert!(m.tracks[0].expect_notes_rows_mut()[2]
            .effects
            .iter()
            .any(|fx| matches!(fx, TrackEffect::Volume { .. })));
    }

    // =====================================================================
    // Phase 4 — Lane event edits (AddLfo/Slide/GlideEvent)
    // =====================================================================

    use crate::core::daw::automation::{AutomationLane, LaneKind};
    use crate::core::fixed::fixed::Q8_8;
    use crate::core::waveform::Waveform;

    /// Build a module with a pre-existing Lfo lane on TrackPitch(0)
    /// with two `Set` events at ticks 0 and 100.
    fn module_with_lfo_lane() -> Module {
        let mut m = fresh_module();
        m.automation.push(AutomationLane {
            target: AutomationTarget::TrackPitch(0),
            kind: LaneKind::Lfo {
                events: vec![
                    LfoEvent::Set {
                        tick: 0,
                        speed: Q8_8::from_int(3),
                        depth: Q15::from_ratio(1, 4),
                        waveform: Waveform::Sine,
                        retrig: true,
                    },
                    LfoEvent::Set {
                        tick: 100,
                        speed: Q8_8::from_int(5),
                        depth: Q15::from_ratio(1, 2),
                        waveform: Waveform::Sine,
                        retrig: true,
                    },
                ],
            },
            enabled: true,
        });
        m
    }

    #[test]
    fn add_lfo_event_inserts_sorted() {
        let mut m = module_with_lfo_lane();
        // Insert a Clear at tick 50 — should land between the two Sets.
        m.apply(EditCommand::AddLfoEvent {
            lane: 0,
            event: LfoEvent::Clear { tick: 50 },
        })
        .expect("add lfo event");
        let events = match &m.automation[0].kind {
            LaneKind::Lfo { events } => events,
            _ => panic!("kind"),
        };
        assert_eq!(events.len(), 3);
        let ticks: Vec<u32> = events.iter().map(|e| e.tick()).collect();
        assert_eq!(ticks, vec![0, 50, 100]);
    }

    #[test]
    fn add_lfo_event_rejects_wrong_kind() {
        let mut m = fresh_module();
        m.apply(EditCommand::CreateAutomationLane {
            target: AutomationTarget::Bpm,
        })
        .unwrap();
        // The created lane is Points-kind; AddLfoEvent must reject.
        let res = m.apply(EditCommand::AddLfoEvent {
            lane: 0,
            event: LfoEvent::Clear { tick: 0 },
        });
        assert!(matches!(res, Err(EditError::IndexOutOfRange { .. })));
    }

    #[test]
    fn add_lfo_event_undo_restores_lane() {
        let mut m = module_with_lfo_lane();
        let before = module_signature(&m);
        let r = m
            .apply(EditCommand::AddLfoEvent {
                lane: 0,
                event: LfoEvent::Clear { tick: 50 },
            })
            .expect("add");
        m.undo(&r).expect("undo");
        assert_eq!(module_signature(&m), before);
    }

    #[test]
    fn add_slide_event_inserts_sorted_and_rejects_wrong_kind() {
        let mut m = fresh_module();
        m.automation.push(AutomationLane {
            target: AutomationTarget::TrackVolume(0),
            kind: LaneKind::Slide {
                events: vec![SlideEvent::Set {
                    tick: 0,
                    rate: Q15::from_ratio(1, 32),
                    fine: false,
                }],
            },
            enabled: true,
        });
        m.apply(EditCommand::AddSlideEvent {
            lane: 0,
            event: SlideEvent::Clear { tick: 24 },
        })
        .expect("add slide");
        let events = match &m.automation[0].kind {
            LaneKind::Slide { events } => events,
            _ => panic!(),
        };
        assert_eq!(events.len(), 2);
        // Rejected on the Lfo lane (none exists here, but on a Points lane).
        m.apply(EditCommand::CreateAutomationLane {
            target: AutomationTarget::Bpm,
        })
        .unwrap();
        let res = m.apply(EditCommand::AddSlideEvent {
            lane: 1,
            event: SlideEvent::Clear { tick: 0 },
        });
        assert!(matches!(res, Err(EditError::IndexOutOfRange { .. })));
    }

    #[test]
    fn add_glide_event_inserts_sorted() {
        let mut m = fresh_module();
        m.automation.push(AutomationLane {
            target: AutomationTarget::TrackPitch(0),
            kind: LaneKind::Glide { events: vec![] },
            enabled: true,
        });
        m.apply(EditCommand::AddGlideEvent {
            lane: 0,
            event: GlideEvent::Set {
                tick: 100,
                target: crate::core::pitch::Pitch::C5,
                rate: 5,
            },
        })
        .expect("add 1");
        m.apply(EditCommand::AddGlideEvent {
            lane: 0,
            event: GlideEvent::Set {
                tick: 50,
                target: crate::core::pitch::Pitch::E5,
                rate: 4,
            },
        })
        .expect("add 2");
        let events = match &m.automation[0].kind {
            LaneKind::Glide { events } => events,
            _ => panic!(),
        };
        let ticks: Vec<u32> = events.iter().map(|e| e.tick()).collect();
        assert_eq!(ticks, vec![50, 100]);
    }

    #[test]
    fn interpolate_undo_restores_track() {
        let mut m = module_with_volume_endpoints();
        let before = module_signature(&m);
        let r = m
            .apply(EditCommand::InterpolateEffect {
                track: 0,
                row_start: 0,
                row_end: 5,
                kind: InterpolateEffectKind::Volume,
            })
            .expect("interpolate");
        m.undo(&r).expect("undo");
        assert_eq!(module_signature(&m), before);
    }
}
