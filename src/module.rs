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

/// Which tracker convention this module expects at playback.
///
/// Different trackers have different playback semantics even when the
/// underlying data shapes look similar. The most common example:
/// a note-column `===` on an instrument without a volume envelope
/// CUTS the note in FT2/XM but FADES (via `volume_fadeout`) in IT.
/// Neither is a "bug"; they are the canonical behaviours of their
/// respective formats. Importers set this field and the player uses
/// it to pick the right replay semantics, including format-specific
/// quirks (e.g. FT2's arpeggio LUT, portamento-down signed overflow,
/// K00-eats-note) that XM modules are authored against.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModuleFormat {
    /// Unknown / hand-constructed / non-tracker source. Conservative
    /// defaults — no format-specific semantics are applied.
    #[default]
    Unknown,
    /// ProTracker-style MOD (Amiga Soundtracker family).
    Mod,
    /// ScreamTracker III (S3M).
    S3m,
    /// Fasttracker II (XM). Selecting this format activates the
    /// canonical FT2 replay behaviour, including the historical FT2
    /// quirks that XM modules are authored against (arpeggio LUT,
    /// E60 pattern-loop, arpeggio period clamp, portamento-down
    /// signed overflow, K00-eats-note, vol-col `Bx` advances vibrato,
    /// ...). FT2's behaviour is the format's reference, so there is
    /// no separate "quirks" toggle — the format IS the switch.
    Xm,
    /// Impulse Tracker (IT), including OpenMPT's IT extensions.
    It,
}

/// SoundTracker Module with Steroid
#[derive(Serialize, Deserialize, Debug)]
pub struct Module {
    pub name: String,
    pub comment: String,
    /// Canonical playback semantics for this module. Set by the
    /// importer that produced the `Module` (XM importer sets `Xm`,
    /// IT importer sets `It`, …). Hand-constructed `Module`s default
    /// to `Unknown`.
    pub format: ModuleFormat,
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
            format: ModuleFormat::Unknown,
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
