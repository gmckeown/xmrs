use super::track_import_effect::TrackImportEffect;
use crate::fixed::units::Volume;
use crate::prelude::TrackUnit;
use crate::{cell_note::CellNote, effect::GlobalEffect};
use core::fmt::*;
use serde::{Deserialize, Serialize};

use alloc::vec;
use alloc::vec::Vec;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TrackImportUnit {
    pub note: CellNote,
    /// Note-trigger velocity, Q1.15 in `[0, 1]`. Was `f32`.
    pub velocity: Volume,
    pub instrument: Option<usize>,
    pub effects: Vec<TrackImportEffect>,
    pub global_effects: Vec<GlobalEffect>,
}

impl Default for TrackImportUnit {
    fn default() -> Self {
        Self {
            note: CellNote::default(),
            velocity: Volume::FULL,
            instrument: None,
            effects: vec![],
            global_effects: vec![],
        }
    }
}

impl TrackImportUnit {
    pub fn prepare_track_unit(&self) -> TrackUnit {
        TrackUnit {
            note: self.note,
            velocity: self.velocity,
            instrument: self.instrument,
            effects: vec![],
            global_effects: self.global_effects.clone(),
        }
    }
}
