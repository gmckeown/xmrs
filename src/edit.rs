//! Editing API: atomic, validated, undoable commands.
//!
//! `Module` is mutated via [`crate::edit::EditCommand`] variants.
//! Each call to [`crate::module::Module::apply`] runs validation
//! first; on success it produces an [`crate::edit::EditReceipt`]
//! that carries the state needed to undo the operation via
//! [`crate::module::Module::undo`].
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

use crate::compatibility_profile::CompatibilityProfile;
use crate::daw::automation::{AutomationLane, AutomationPoint, AutomationTarget, AutomationValue};
use crate::daw::clip::Clip;
use crate::daw::track::Track;
use crate::effect::{GlobalEffect, TrackEffect};
use crate::fixed::fixed::Q15;
use crate::instrument::InstrumentType;
use crate::module::{Module, PatternHighlight};
use crate::track_unit::TrackUnit;

// =====================================================================
// EditCommand
// =====================================================================

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
        content: TrackUnit,
    },
    DeleteRow {
        track: u32,
        row_offset: u32,
    },
    SetCell {
        track: u32,
        row_offset: u32,
        content: TrackUnit,
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
    SetCellGlobalEffects {
        track: u32,
        row_offset: u32,
        global_effects: Vec<GlobalEffect>,
    },

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

    // ---- Sub-song / global ----
    SetDefaultBpm {
        bpm: usize,
    },
    SetDefaultSpeed {
        speed: usize,
    },
    SetCompatibilityProfile {
        profile: CompatibilityProfile,
    },
    SetModuleName {
        name: String,
    },
    SetPatternHighlight {
        highlight: PatternHighlight,
    },

    // ---- Instrument ----
    /// Set the humanisation parameters of an `InstrEkn`. Errors if
    /// `instrument` isn't an `InstrumentType::Euclidean`.
    SetInstrumentEuclideanHumanize {
        instrument: usize,
        advance_max_ticks: u8,
        probability: Q15,
    },
}

// =====================================================================
// EditError
// =====================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
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
    /// The instrument exists but is not an `InstrumentType::Euclidean`.
    InstrumentNotEuclidean {
        index: usize,
    },
}

// =====================================================================
// EditUndoData
// =====================================================================

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
    CellBefore {
        track: u32,
        row: u32,
        before: TrackUnit,
    },
    /// Global setting snapshot.
    GlobalSettingBefore(GlobalSettingSnapshot),
    /// Previous humanize parameters of an `InstrEkn`.
    InstrumentEuclideanHumanizeBefore {
        instrument: usize,
        advance_max_ticks: u8,
        probability: Q15,
    },
}

#[derive(Debug, Clone, Default)]
pub struct GlobalSettingSnapshot {
    pub name: Option<String>,
    pub default_bpm: Option<usize>,
    pub default_speed: Option<usize>,
    pub profile: Option<CompatibilityProfile>,
    pub pattern_highlight: Option<PatternHighlight>,
}

// =====================================================================
// EditReceipt
// =====================================================================

#[derive(Debug, Clone)]
pub struct EditReceipt {
    pub command: EditCommand,
    pub undo_data: EditUndoData,
}

// =====================================================================
// EditObserver
// =====================================================================

#[derive(Debug, Clone)]
pub enum ChangeEvent {
    TracksChanged { indices: Vec<u32> },
    ClipsChanged { indices: Vec<u32> },
    AutomationChanged { lane_indices: Vec<u32> },
    GlobalSettingChanged,
}

pub trait EditObserver {
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
            EditCommand::SetCellGlobalEffects {
                track,
                row_offset,
                global_effects,
            } => self.cmd_set_cell_global_effects(cmd, track, row_offset, global_effects, observer),

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

            EditCommand::SetDefaultBpm { bpm } => self.cmd_set_default_bpm(cmd, bpm, observer),
            EditCommand::SetDefaultSpeed { speed } => {
                self.cmd_set_default_speed(cmd, speed, observer)
            }
            EditCommand::SetCompatibilityProfile { profile } => {
                self.cmd_set_compatibility_profile(cmd, profile, observer)
            }
            EditCommand::SetModuleName { name } => self.cmd_set_module_name(cmd, name, observer),
            EditCommand::SetPatternHighlight { highlight } => {
                self.cmd_set_pattern_highlight(cmd, highlight, observer)
            }
            EditCommand::SetInstrumentEuclideanHumanize {
                instrument,
                advance_max_ticks,
                probability,
            } => self.cmd_set_instrument_euclidean_humanize(
                cmd,
                instrument,
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
                if (*row as usize) >= t.rows.len() {
                    return Err(EditError::RowOutOfRange {
                        track: *track,
                        requested: *row,
                        length: t.rows.len() as u32,
                    });
                }
                t.rows[*row as usize] = before.clone();
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
                if let Some(profile) = &snap.profile {
                    self.profile = *profile;
                }
                if let Some(highlight) = &snap.pattern_highlight {
                    self.pattern_highlight = *highlight;
                }
                Ok(())
            }
            (
                _,
                EditUndoData::InstrumentEuclideanHumanizeBefore {
                    instrument,
                    advance_max_ticks,
                    probability,
                },
            ) => {
                let instr = self
                    .instrument
                    .get_mut(*instrument)
                    .ok_or(EditError::InstrumentDoesNotExist { index: *instrument })?;
                if let InstrumentType::Euclidean(ekn) = &mut instr.instr_type {
                    ekn.humanize_advance_max_ticks = *advance_max_ticks;
                    ekn.humanize_probability = *probability;
                    Ok(())
                } else {
                    Err(EditError::InstrumentNotEuclidean { index: *instrument })
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
        if !self.instrument.is_empty() && instrument >= self.instrument.len() {
            return Err(EditError::InstrumentDoesNotExist { index: instrument });
        }
        let track = Track {
            name,
            instrument,
            rows: alloc::vec![TrackUnit::default(); length_rows as usize],
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

    fn cmd_rename_track(
        &mut self,
        cmd: EditCommand,
        track: u32,
        new_name: String,
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
        t.name = new_name;
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

    fn cmd_set_track_instrument(
        &mut self,
        cmd: EditCommand,
        track: u32,
        new_instrument: usize,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        if !self.instrument.is_empty() && new_instrument >= self.instrument.len() {
            return Err(EditError::InstrumentDoesNotExist {
                index: new_instrument,
            });
        }
        let t = self
            .tracks
            .get_mut(track as usize)
            .ok_or(EditError::IndexOutOfRange {
                what: "track",
                index: track,
            })?;
        let before = t.clone();
        t.instrument = new_instrument;
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

    fn cmd_set_track_muted(
        &mut self,
        cmd: EditCommand,
        track: u32,
        muted: bool,
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
        t.muted = muted;
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

    fn cmd_resize_track(
        &mut self,
        cmd: EditCommand,
        track: u32,
        new_length_rows: u32,
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
        let n = new_length_rows as usize;
        if n > t.rows.len() {
            t.rows.resize(n, TrackUnit::default());
        } else {
            t.rows.truncate(n);
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
        content: TrackUnit,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let t = self
            .tracks
            .get_mut(track as usize)
            .ok_or(EditError::IndexOutOfRange {
                what: "track",
                index: track,
            })?;
        if (row_offset as usize) > t.rows.len() {
            return Err(EditError::RowOutOfRange {
                track,
                requested: row_offset,
                length: t.rows.len() as u32,
            });
        }
        let before = t.clone();
        t.rows.insert(row_offset as usize, content);
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
        if (row_offset as usize) >= t.rows.len() {
            return Err(EditError::RowOutOfRange {
                track,
                requested: row_offset,
                length: t.rows.len() as u32,
            });
        }
        let before = t.clone();
        t.rows.remove(row_offset as usize);
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

    fn cmd_set_cell(
        &mut self,
        cmd: EditCommand,
        track: u32,
        row_offset: u32,
        content: TrackUnit,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let t = self
            .tracks
            .get_mut(track as usize)
            .ok_or(EditError::IndexOutOfRange {
                what: "track",
                index: track,
            })?;
        if (row_offset as usize) >= t.rows.len() {
            return Err(EditError::RowOutOfRange {
                track,
                requested: row_offset,
                length: t.rows.len() as u32,
            });
        }
        let before = t.rows[row_offset as usize].clone();
        t.rows[row_offset as usize] = content;
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

    fn cmd_add_cell_effect(
        &mut self,
        cmd: EditCommand,
        track: u32,
        row_offset: u32,
        effect: TrackEffect,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let t = self
            .tracks
            .get_mut(track as usize)
            .ok_or(EditError::IndexOutOfRange {
                what: "track",
                index: track,
            })?;
        if (row_offset as usize) >= t.rows.len() {
            return Err(EditError::RowOutOfRange {
                track,
                requested: row_offset,
                length: t.rows.len() as u32,
            });
        }
        let before = t.rows[row_offset as usize].clone();
        t.rows[row_offset as usize].effects.push(effect);
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

    fn cmd_remove_cell_effect(
        &mut self,
        cmd: EditCommand,
        track: u32,
        row_offset: u32,
        effect_index: usize,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let t = self
            .tracks
            .get_mut(track as usize)
            .ok_or(EditError::IndexOutOfRange {
                what: "track",
                index: track,
            })?;
        if (row_offset as usize) >= t.rows.len() {
            return Err(EditError::RowOutOfRange {
                track,
                requested: row_offset,
                length: t.rows.len() as u32,
            });
        }
        let cell = &mut t.rows[row_offset as usize];
        if effect_index >= cell.effects.len() {
            return Err(EditError::IndexOutOfRange {
                what: "effect",
                index: effect_index as u32,
            });
        }
        let before = cell.clone();
        cell.effects.remove(effect_index);
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

    fn cmd_set_cell_effects(
        &mut self,
        cmd: EditCommand,
        track: u32,
        row_offset: u32,
        effects: Vec<TrackEffect>,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let t = self
            .tracks
            .get_mut(track as usize)
            .ok_or(EditError::IndexOutOfRange {
                what: "track",
                index: track,
            })?;
        if (row_offset as usize) >= t.rows.len() {
            return Err(EditError::RowOutOfRange {
                track,
                requested: row_offset,
                length: t.rows.len() as u32,
            });
        }
        let before = t.rows[row_offset as usize].clone();
        t.rows[row_offset as usize].effects = effects;
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

    fn cmd_set_cell_global_effects(
        &mut self,
        cmd: EditCommand,
        track: u32,
        row_offset: u32,
        global_effects: Vec<GlobalEffect>,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let t = self
            .tracks
            .get_mut(track as usize)
            .ok_or(EditError::IndexOutOfRange {
                what: "track",
                index: track,
            })?;
        if (row_offset as usize) >= t.rows.len() {
            return Err(EditError::RowOutOfRange {
                track,
                requested: row_offset,
                length: t.rows.len() as u32,
            });
        }
        let before = t.rows[row_offset as usize].clone();
        t.rows[row_offset as usize].global_effects = global_effects;
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
            .map(|t| t.rows.len() as u32)
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
        let mut candidate = original;
        candidate.position_tick = new_position_tick;
        if let Some(i) = self.find_overlapping_clip(&candidate, &[clip]) {
            return Err(EditError::ClipOverlap { existing_clip: i });
        }
        let before = self.clips.snapshot();
        self.clips
            .modify(idx, |c| c.position_tick = new_position_tick);
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
        if (point_index as usize) >= l.points.len() {
            return Err(EditError::IndexOutOfRange {
                what: "point",
                index: point_index,
            });
        }
        let before = l.clone();
        l.points.remove(point_index as usize);
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
        if (point_index as usize) >= l.points.len() {
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
        l.points.remove(point_index as usize);
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

    fn cmd_set_default_bpm(
        &mut self,
        cmd: EditCommand,
        bpm: usize,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let snap = GlobalSettingSnapshot {
            default_bpm: Some(self.default_bpm),
            ..Default::default()
        };
        self.default_bpm = bpm;
        observer.on_change(ChangeEvent::GlobalSettingChanged);
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::GlobalSettingBefore(snap),
        })
    }

    fn cmd_set_default_speed(
        &mut self,
        cmd: EditCommand,
        speed: usize,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let snap = GlobalSettingSnapshot {
            default_speed: Some(self.default_tempo),
            ..Default::default()
        };
        self.default_tempo = speed;
        observer.on_change(ChangeEvent::GlobalSettingChanged);
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::GlobalSettingBefore(snap),
        })
    }

    fn cmd_set_compatibility_profile(
        &mut self,
        cmd: EditCommand,
        profile: CompatibilityProfile,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let snap = GlobalSettingSnapshot {
            profile: Some(self.profile),
            ..Default::default()
        };
        self.profile = profile;
        observer.on_change(ChangeEvent::GlobalSettingChanged);
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::GlobalSettingBefore(snap),
        })
    }

    fn cmd_set_module_name(
        &mut self,
        cmd: EditCommand,
        name: String,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let snap = GlobalSettingSnapshot {
            name: Some(self.name.clone()),
            ..Default::default()
        };
        self.name = name;
        observer.on_change(ChangeEvent::GlobalSettingChanged);
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::GlobalSettingBefore(snap),
        })
    }

    fn cmd_set_pattern_highlight(
        &mut self,
        cmd: EditCommand,
        highlight: PatternHighlight,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let snap = GlobalSettingSnapshot {
            pattern_highlight: Some(self.pattern_highlight),
            ..Default::default()
        };
        self.pattern_highlight = highlight;
        observer.on_change(ChangeEvent::GlobalSettingChanged);
        Ok(EditReceipt {
            command: cmd,
            undo_data: EditUndoData::GlobalSettingBefore(snap),
        })
    }

    fn cmd_set_instrument_euclidean_humanize(
        &mut self,
        cmd: EditCommand,
        instrument: usize,
        advance_max_ticks: u8,
        probability: Q15,
        observer: &mut dyn EditObserver,
    ) -> Result<EditReceipt, EditError> {
        let instr = self
            .instrument
            .get_mut(instrument)
            .ok_or(EditError::InstrumentDoesNotExist { index: instrument })?;
        let ekn = match &mut instr.instr_type {
            InstrumentType::Euclidean(e) => e,
            _ => return Err(EditError::InstrumentNotEuclidean { index: instrument }),
        };
        let before = EditUndoData::InstrumentEuclideanHumanizeBefore {
            instrument,
            advance_max_ticks: ekn.humanize_advance_max_ticks,
            probability: ekn.humanize_probability,
        };
        ekn.humanize_advance_max_ticks = advance_max_ticks;
        ekn.humanize_probability = probability;
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
                t.name,
                t.instrument,
                t.rows.len(),
                t.muted,
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
            let _ = write!(
                &mut s,
                " l{}={{target={:?} points={} enabled={}}}",
                i,
                l.target,
                l.points.len(),
                l.enabled,
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
        // so we use serde round-trip via bincode in std-only tests
        // or a manual copy here for the fields we actually touch.
        let mut c = Module {
            name: m.name.clone(),
            default_bpm: m.default_bpm,
            default_tempo: m.default_tempo,
            profile: m.profile,
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
                content: TrackUnit::default(),
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
        use crate::instr_ekn::InstrEkn;
        let mut m = Module::default();
        m.instrument.push(Instrument {
            name: "ekn".into(),
            instr_type: InstrumentType::Euclidean(InstrEkn::default()),
            muted: false,
        });

        let receipt = m
            .apply(EditCommand::SetInstrumentEuclideanHumanize {
                instrument: 0,
                advance_max_ticks: 4,
                probability: Q15::HALF,
            })
            .unwrap();
        if let InstrumentType::Euclidean(e) = &m.instrument[0].instr_type {
            assert_eq!(e.humanize_advance_max_ticks, 4);
            assert_eq!(e.humanize_probability, Q15::HALF);
        } else {
            panic!("expected Euclidean");
        }
        m.undo(&receipt).unwrap();
        if let InstrumentType::Euclidean(e) = &m.instrument[0].instr_type {
            assert_eq!(e.humanize_advance_max_ticks, 0);
            assert_eq!(e.humanize_probability, Q15::ZERO);
        } else {
            panic!("expected Euclidean after undo");
        }
    }

    #[test]
    fn humanize_rejects_non_euclidean_instrument() {
        let mut m = fresh_module(); // instrument 0 is InstrumentType::Empty
        let err = m.apply(EditCommand::SetInstrumentEuclideanHumanize {
            instrument: 0,
            advance_max_ticks: 4,
            probability: Q15::HALF,
        });
        assert!(matches!(err, Err(EditError::InstrumentNotEuclidean { .. })));
    }

    #[test]
    fn humanize_rejects_missing_instrument() {
        let mut m = Module::default();
        let err = m.apply(EditCommand::SetInstrumentEuclideanHumanize {
            instrument: 0,
            advance_max_ticks: 4,
            probability: Q15::HALF,
        });
        assert!(matches!(err, Err(EditError::InstrumentDoesNotExist { .. })));
    }
}
