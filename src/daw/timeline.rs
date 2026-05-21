//! `TimelineMap` — derived table mapping each visited row to an
//! absolute tick. Produced by
//! [`crate::daw::build_timeline::build_timeline_map`] when an
//! importer finishes parsing a Module. Serialised alongside the
//! rest of the DAW layer so it doesn't have to be rebuilt at load
//! time.

use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct TimelineMap {
    pub entries: Vec<TimelineEntry>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct TimelineEntry {
    pub song: u16,
    pub order_idx: u32,
    pub pattern_idx: u32,
    pub row_idx: u32,
    pub loop_iter: u16,
    pub tick: u32,
    pub speed_at_row: u8,
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
