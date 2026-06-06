//! `TimelineMap` — derived table mapping each visited row to an
//! absolute tick. Produced by
//! [`crate::tracker::import::build::build_timeline_map`] when an
//! importer finishes parsing a Module. Serialised alongside the
//! rest of the DAW layer so it doesn't have to be rebuilt at load
//! time.

use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// Linearised playback order for every sub-song of a [`Module`].
///
/// One [`TimelineEntry`] per visited `(song, pattern, row,
/// loop_iter)`, in playback order — the result of the navigation
/// walk that absorbs `PositionJump` / `PatternBreak` /
/// `PatternLoop` / `PatternDelay` at import time. Lets the
/// sequencer drive playback as a flat `idx` step instead of
/// re-running flow-control effects per tick.
///
/// [`Module`]: crate::core::module::Module
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct TimelineMap {
    /// Visited `(song, pattern, row)` coordinates in playback
    /// order. `Self::find_entry*` indexes into this vector.
    pub entries: Vec<TimelineEntry>,
}

/// One visited row in playback order.
///
/// Carries the absolute `tick` at which the row plays plus the
/// effective `speed_at_row` / `bpm_at_row` snapshot at that row —
/// so the duration estimator and the runtime can derive per-row
/// timing without re-walking flow control.
#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct TimelineEntry {
    /// Sub-song index (0..pattern_order.len()).
    pub song: u16,
    /// Position in the sub-song's order list at which this row
    /// plays. Distinguishes two visits of the same pattern from
    /// different order positions.
    pub order_idx: u32,
    /// Pattern index this entry came from.
    pub pattern_idx: u32,
    /// Source-row index inside that pattern.
    pub row_idx: u32,
    /// Iteration counter for `PatternLoop`-driven revisits. `0`
    /// means "first time through"; higher values are loop replays.
    pub loop_iter: u16,
    /// Absolute song-tick at which this row starts playing.
    pub tick: u32,
    /// Effective speed (ticks per row) at this row, post tempo /
    /// `SongLevelEffect::Speed` changes.
    pub speed_at_row: u8,
    /// Effective BPM at this row, post `SongLevelEffect::Bpm`
    /// changes.
    pub bpm_at_row: u16,
}

impl TimelineMap {
    /// First-iteration entry at `(song, pattern_idx, row_idx)`.
    /// Returns `None` if the coordinate was never visited by the
    /// linearisation walker.
    pub fn find_entry(
        &self,
        song: usize,
        pattern_idx: usize,
        row_idx: usize,
    ) -> Option<&TimelineEntry> {
        self.entries.iter().find(|e| {
            e.song as usize == song
                && e.pattern_idx as usize == pattern_idx
                && e.row_idx as usize == row_idx
                && e.loop_iter == 0
        })
    }

    /// First-iteration entry whose absolute `tick` equals `tick` on
    /// `song`. The reverse of reading `entry.tick`: maps a tick back to
    /// its `(pattern_idx, row_idx)` row. Used by per-lane looping
    /// ([`crate::core::daw::loop_region::ChannelLoop`]) to find the row
    /// a folded loop-local tick lands on. Loop regions are row-aligned,
    /// so a folded tick always matches a row-start tick exactly.
    pub fn entry_at_tick(&self, song: usize, tick: u32) -> Option<&TimelineEntry> {
        self.entries
            .iter()
            .find(|e| e.song as usize == song && e.tick == tick && e.loop_iter == 0)
    }

    /// First-iteration entry at `(song, order_idx)`. Used to resolve
    /// "what pattern plays at this order position?"
    pub fn find_order_entry(&self, song: usize, order_idx: usize) -> Option<&TimelineEntry> {
        self.entries.iter().find(|e| {
            e.song as usize == song && e.order_idx as usize == order_idx && e.loop_iter == 0
        })
    }

    /// First-iteration entry at `(song, order_idx, row_idx)`. Unlike
    /// [`Self::find_entry`] this disambiguates between two visits of
    /// the same pattern at different order positions — required when
    /// a song references the same pattern twice and the per-row
    /// ticks differ.
    pub fn find_entry_at_order(
        &self,
        song: usize,
        order_idx: usize,
        row_idx: usize,
    ) -> Option<&TimelineEntry> {
        self.entries.iter().find(|e| {
            e.song as usize == song
                && e.order_idx as usize == order_idx
                && e.row_idx as usize == row_idx
                && e.loop_iter == 0
        })
    }

    /// Highest `order_idx` ever reached on first iteration of `song`,
    /// plus one. Equivalent to the song's "length" in order entries.
    pub fn order_count(&self, song: usize) -> usize {
        self.entries
            .iter()
            .filter(|e| e.song as usize == song && e.loop_iter == 0)
            .map(|e| e.order_idx)
            .max()
            .map(|m| m as usize + 1)
            .unwrap_or(0)
    }

    /// Highest `row_idx` ever played on `pattern_idx`, plus one.
    /// Patterns reached via `PatternBreak` may report a smaller
    /// value than their source row count — only rows that the
    /// linearisation walker actually visited are counted.
    pub fn row_count_in_pattern(&self, pattern_idx: usize) -> usize {
        self.entries
            .iter()
            .filter(|e| e.pattern_idx as usize == pattern_idx)
            .map(|e| e.row_idx)
            .max()
            .map(|m| m as usize + 1)
            .unwrap_or(0)
    }
}
