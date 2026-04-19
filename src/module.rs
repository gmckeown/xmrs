use serde::{Deserialize, Serialize};

use crate::instrument::Instrument;
use crate::period_helper::FrequencyType;
use crate::prelude::TrackUnit;

use alloc::string::String;
use alloc::string::ToString;
use alloc::{vec, vec::Vec};

#[cfg(target_pointer_width = "16")]
pub const MAX_NUM_ROWS: usize = 255;

#[cfg(target_pointer_width = "32")]
pub const MAX_NUM_ROWS: usize = 4095;

#[cfg(target_pointer_width = "64")]
pub const MAX_NUM_ROWS: usize = 4095;

/// A row contains its column elements
pub type Row = Vec<TrackUnit>;

/// Patterns are sequences of lines
pub type Pattern = Vec<Row>;

/// SoundTracker Module with Steroid
#[derive(Serialize, Deserialize, Debug)]
pub struct Module {
    pub name: String,
    pub comment: String,
    pub frequency_type: FrequencyType,
    /// Restart index in `pattern_order`
    pub restart_position: usize,
    pub default_tempo: usize,
    pub default_bpm: usize,
    /// Defines the exact order for the patterns playback
    /// It is possible to have several music in the same Module
    pub pattern_order: Vec<Vec<usize>>,
    pub pattern: Vec<Pattern>,
    pub pattern_names: Vec<String>,
    pub channel_names: Vec<String>,
    pub instrument: Vec<Instrument>,
}

impl Default for Module {
    fn default() -> Self {
        Module {
            name: "".to_string(),
            comment: "".to_string(),
            frequency_type: FrequencyType::LinearFrequencies,
            restart_position: 0,
            default_tempo: 6,
            default_bpm: 125,
            pattern_order: vec![],
            pattern: vec![],
            pattern_names: vec![],
            channel_names: vec![],
            instrument: vec![],
        }
    }
}

impl Module {
    /// get song length
    pub fn get_song_length(&self, song: usize) -> usize {
        self.pattern_order[song].len()
    }

    /// get number of channels
    pub fn get_num_channels(&self) -> usize {
        if let Some(first_row) = self.pattern.first().and_then(|p| p.first()) {
            first_row.len()
        } else {
            0
        }
    }

    /// get number of rows
    pub fn get_num_rows(&self, pat_idx: usize) -> usize {
        if pat_idx < self.pattern.len() {
            self.pattern[pat_idx].len()
        } else {
            0
        }
    }
}
