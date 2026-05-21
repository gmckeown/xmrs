//! Automation lanes: parameter curves through time.
//!
//! Each lane targets one DAW parameter (global volume, BPM, etc.)
//! and stores a list of `(tick, value)` points sorted by tick. The
//! value is typed via [`AutomationValue`] so that integer-domain
//! targets (BPM, Speed) can't be conflated with normalised targets
//! (volume, panning).

use crate::fixed::fixed::Q15;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// What an [`AutomationLane`] controls.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AutomationTarget {
    GlobalVolume,
    Bpm,
    Speed,
    ChannelVolume(u8),
    ChannelPanning(u8),
    TrackVolume(u32),
    TrackPanning(u32),
}

/// Typed value of an [`AutomationPoint`], matched against the
/// lane's [`AutomationTarget`] at construction time.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationValue {
    /// For targets in `[0.0, 1.0]`: volumes, panning, etc.
    Normalized(Q15),
    /// For BPM (typical range 32..=255).
    Bpm(u16),
    /// For Speed (typical range 1..=31).
    Speed(u8),
}

impl AutomationValue {
    /// `true` when the value's variant matches `target`.
    pub fn is_compatible_with(&self, target: AutomationTarget) -> bool {
        matches!(
            (self, target),
            (
                AutomationValue::Normalized(_),
                AutomationTarget::GlobalVolume
            ) | (
                AutomationValue::Normalized(_),
                AutomationTarget::ChannelVolume(_)
            ) | (
                AutomationValue::Normalized(_),
                AutomationTarget::ChannelPanning(_)
            ) | (
                AutomationValue::Normalized(_),
                AutomationTarget::TrackVolume(_)
            ) | (
                AutomationValue::Normalized(_),
                AutomationTarget::TrackPanning(_)
            ) | (AutomationValue::Bpm(_), AutomationTarget::Bpm)
                | (AutomationValue::Speed(_), AutomationTarget::Speed)
        )
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct AutomationPoint {
    pub tick: u32,
    pub value: AutomationValue,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AutomationLane {
    pub target: AutomationTarget,
    /// Sorted by tick (ascending). The API maintains this invariant.
    pub points: Vec<AutomationPoint>,
    pub enabled: bool,
}

impl AutomationLane {
    /// Build an empty lane for `target`.
    pub fn new(target: AutomationTarget) -> Self {
        Self {
            target,
            points: Vec::new(),
            enabled: true,
        }
    }

    /// Insert a point, keeping `points` sorted by tick. If a point
    /// already exists at the same tick it is replaced.
    pub fn insert_point(&mut self, point: AutomationPoint) {
        match self.points.binary_search_by_key(&point.tick, |p| p.tick) {
            Ok(idx) => self.points[idx] = point,
            Err(idx) => self.points.insert(idx, point),
        }
    }
}
