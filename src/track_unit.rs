use crate::{
    cell_note::CellNote,
    effect::{GlobalEffect, TrackEffect},
};
use core::fmt::*;
use serde::{Deserialize, Serialize};

use alloc::vec;
use alloc::vec::Vec;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TrackUnit {
    pub note: CellNote,
    pub velocity: f32,
    pub instrument: Option<usize>,
    pub effects: Vec<TrackEffect>,
    pub global_effects: Vec<GlobalEffect>,
}

impl Default for TrackUnit {
    fn default() -> Self {
        Self {
            note: CellNote::default(),
            velocity: 1.0,
            instrument: None,
            effects: vec![],
            global_effects: vec![],
        }
    }
}

impl TrackUnit {
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
        fx || matches!(self.note, CellNote::KeyOff)
    }

    /// Returns true iff the slot carries a Kxx *effect* note-off.
    ///
    /// Unlike [`Self::has_note_off`], this does NOT match a note-column
    /// `===` (`CellNote::KeyOff`). It's intended for replayer paths
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

    pub fn has_tone_portamento(&self) -> bool {
        self.effects
            .iter()
            .any(|effect| matches!(effect, TrackEffect::TonePortamento(_)))
    }

    pub fn has_vibrato(&self) -> bool {
        self.effects
            .iter()
            .any(|effect| matches!(effect, TrackEffect::Vibrato { speed: _, depth: _ }))
    }

    /// Returns true iff the slot carries a standalone `VibratoDepth`
    /// effect (as produced by XM vol-column Bx when no main-effect 4xx
    /// is also on the row). Used by the FT2 vol-col-B quirk to keep the
    /// vibrato LFO progressing across ticks even without an accompanying
    /// main Vibrato effect.
    pub fn has_vibrato_depth(&self) -> bool {
        self.effects
            .iter()
            .any(|effect| matches!(effect, TrackEffect::VibratoDepth(_)))
    }

    pub fn has_volume_slide(&self) -> bool {
        self.effects
            .iter()
            .any(|effect| matches!(effect, TrackEffect::VolumeSlide { speed: _, fine: _ }))
    }

    pub fn has_global_volume_slide(&self) -> bool {
        self.global_effects
            .iter()
            .any(|effect| matches!(effect, GlobalEffect::VolumeSlide { speed: _, fine: _ }))
    }
}
