use crate::{
    effect::{GlobalEffect, TrackEffect},
    pitch::Pitch,
};
use core::fmt::*;
use serde::{Deserialize, Serialize};

use alloc::vec;
use alloc::vec::Vec;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TrackUnit {
    pub note: Pitch,
    pub velocity: f32,
    pub instrument: Option<usize>,
    pub effects: Vec<TrackEffect>,
    pub global_effects: Vec<GlobalEffect>,
}

impl Default for TrackUnit {
    fn default() -> Self {
        Self {
            note: Pitch::default(),
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
        fx || self.note.is_keyoff()
    }

    /// Returns true iff the slot carries a Kxx *effect* note-off.
    ///
    /// Unlike [`Self::has_note_off`], this does NOT match a note-column
    /// `===` (`Pitch::Off`). It's intended for replayer paths that need
    /// to apply FT2 Kxy-effect quirks without also misfiring on note-
    /// column key-offs, which have different playback semantics in FT2
    /// (see `ft2_replayer.c:keyOff()` vs. the `keyOffCmd` / K00 paths).
    pub fn has_note_off_effect(&self) -> bool {
        self.effects
            .iter()
            .any(|effect| matches!(effect, TrackEffect::NoteOff { tick: _, past: _ }))
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
