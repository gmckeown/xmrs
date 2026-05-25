//! Automation lanes: parameter curves through time.
//!
//! Each lane targets one DAW parameter (global volume, BPM, etc.)
//! and carries a typed [`LaneKind`] payload — `Points` for typed
//! `(tick, value)` curves, or a continuous-modulation kind
//! (`Lfo` / `Slide` / `Glide`) for the modulation classes extracted
//! upstream from `TrackImportUnit` by
//! [`crate::tracker::import::extract`].
//!
//! ```text
//! Module
//!  └─ automation: Vec<AutomationLane>
//!       ├─ target:  AutomationTarget   (what is being modulated)
//!       ├─ kind:    LaneKind { Points / Lfo / Slide / Glide }
//!       └─ enabled: bool
//! ```
//!
//! Since DAW migration Phase 3c.3 the lanes are the **authoritative
//! source** for every continuous modulation (Vibrato/Tremolo/Panbrello/
//! Portamento/TonePortamento/VolumeSlide/ChannelVolumeSlide/PanningSlide
//! at the per-Track level; Bpm/BpmSlide/Speed/GlobalVolume/GlobalVolumeSlide
//! at the song level). They are extracted upstream from
//! `TrackImportUnit` by [`crate::tracker::import::extract`] and consumed
//! by the player without any cell-side fallback.

use crate::core::fixed::fixed::{Q15, Q8_8};
use crate::core::pitch::Pitch;
use crate::core::waveform::Waveform;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// What an [`AutomationLane`] controls.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AutomationTarget {
    /// Mix-bus master gain — drives `SongLevelEffect::Volume` and
    /// `SongLevelEffect::VolumeSlide`.
    GlobalVolume,
    /// Song-level BPM (32..=255). Drives `SongLevelEffect::Bpm` and
    /// `SongLevelEffect::BpmSlide`.
    Bpm,
    /// Song-level speed in ticks/row (1..=31). Drives
    /// `SongLevelEffect::Speed`.
    Speed,
    /// Per-channel volume (S3M Mxx). Kept for completeness — the
    /// `TrackChannelVolume(u32)` variant is the Phase 3 successor
    /// keyed on Track index instead of channel index.
    ChannelVolume(u8),
    /// Per-channel panning. Same caveat as `ChannelVolume`.
    ChannelPanning(u8),
    /// Per-Track volume — receives Tremolo (LFO), VolumeSlide
    /// (Slide).
    TrackVolume(u32),
    /// Per-Track panning — receives Panbrello (LFO), PanningSlide
    /// (Slide).
    TrackPanning(u32),
    /// Per-Track pitch in semitones (Q8.8 delta) — receives Vibrato
    /// (LFO), Portamento (Slide), TonePortamento (Glide). Added in
    /// DAW migration Phase 3a.
    TrackPitch(u32),
    /// Per-Track channel volume — receives ChannelVolumeSlide
    /// (Slide). Added in DAW migration Phase 3a; superset of the
    /// legacy `ChannelVolume(u8)` keyed on Track index instead of
    /// the raw channel byte.
    TrackChannelVolume(u32),
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
            ) | (
                AutomationValue::Normalized(_),
                AutomationTarget::TrackPitch(_)
            ) | (
                AutomationValue::Normalized(_),
                AutomationTarget::TrackChannelVolume(_)
            ) | (AutomationValue::Bpm(_), AutomationTarget::Bpm)
                | (AutomationValue::Speed(_), AutomationTarget::Speed)
        )
    }
}

/// One typed point on a [`LaneKind::Points`] curve.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutomationPoint {
    pub tick: u32,
    pub value: AutomationValue,
}

/// LFO modulation events — vibrato / tremolo / panbrello. The
/// runtime keeps the phase state machine; events tell it when to
/// arm / update / clear that machine. See DAW migration Phase 3a.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum LfoEvent {
    /// (Re-)initialise every LFO parameter. Emitted by
    /// `Vibrato{speed,depth}`, `Tremolo`, `Panbrello`, or by a
    /// fusion of a sub-effect (`VibratoSpeed` etc.) with the
    /// main effect on the same row.
    Set {
        tick: u32,
        speed: Q8_8,
        depth: Q15,
        waveform: Waveform,
        retrig: bool,
    },
    /// Update partial : only the depth changes. Emitted by a
    /// standalone `VibratoDepth` row under the XM
    /// `volcol_b_advances_vibrato` quirk.
    DepthOnly { tick: u32, depth: Q15 },
    /// Update partial : only the speed changes. Emitted by a
    /// standalone `VibratoSpeed`.
    SpeedOnly { tick: u32, speed: Q8_8 },
    /// Update partial : only the waveform shape (and the
    /// per-channel retrig-on-new-note flag) change. Emitted by a
    /// standalone `VibratoWaveform` / `TremoloWaveform` /
    /// `PanbrelloWaveform` row — i.e. a row that touches the LFO
    /// shape without also re-arming the modulation. The
    /// runtime's `WaveformState` is updated immediately;
    /// subsequent `Set`s on the same lane will carry the new
    /// waveform in their `Set::waveform` slot.
    ///
    /// Added in DAW migration Phase 3c.1 to close a divergence
    /// between the cell-driven runtime (which updates waveform
    /// inline) and the lane mirror (which previously stashed the
    /// new waveform until the next `Set`).
    WaveformOnly {
        tick: u32,
        waveform: Waveform,
        retrig: bool,
    },
    /// Explicit deactivation. Whether the phase resets or carries
    /// over depends on the `retrig` of the last `Set`.
    Clear { tick: u32 },
}

impl LfoEvent {
    #[inline]
    pub fn tick(&self) -> u32 {
        match *self {
            Self::Set { tick, .. } => tick,
            Self::DepthOnly { tick, .. } => tick,
            Self::SpeedOnly { tick, .. } => tick,
            Self::WaveformOnly { tick, .. } => tick,
            Self::Clear { tick } => tick,
        }
    }
}

/// Linear-slide modulation events — VolumeSlide / Portamento /
/// PanningSlide / ChannelVolumeSlide. The runtime accumulates `rate`
/// at every tick of the row that emitted the `Set`, then `Clear`
/// stops the slide.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum SlideEvent {
    Set { tick: u32, rate: Q15, fine: bool },
    Clear { tick: u32 },
}

impl SlideEvent {
    #[inline]
    pub fn tick(&self) -> u32 {
        match *self {
            Self::Set { tick, .. } => tick,
            Self::Clear { tick } => tick,
        }
    }
}

/// Glide-towards-target events — TonePortamento (`Gxx`). Each
/// `Set` rebinds the target pitch the runtime should slide toward.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum GlideEvent {
    Set { tick: u32, target: Pitch, rate: i16 },
    Clear { tick: u32 },
}

impl GlideEvent {
    #[inline]
    pub fn tick(&self) -> u32 {
        match *self {
            Self::Set { tick, .. } => tick,
            Self::Clear { tick } => tick,
        }
    }
}

/// The payload of an [`AutomationLane`]. One variant per modulation
/// class; the lane's `target` constrains which variants are
/// semantically valid. See DAW migration Phase 3a.
///
/// `Points` is the original (Phase 0) typed-value curve.
/// `Lfo` / `Slide` / `Glide` are the continuous-modulation payloads
/// extracted upstream from `TrackImportUnit.effects` /
/// `TrackImportUnit.song_level` by
/// [`crate::tracker::import::extract`].
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum LaneKind {
    Points(Vec<AutomationPoint>),
    Lfo { events: Vec<LfoEvent> },
    Slide { events: Vec<SlideEvent> },
    Glide { events: Vec<GlideEvent> },
}

impl Default for LaneKind {
    fn default() -> Self {
        LaneKind::Points(Vec::new())
    }
}

/// Snapshot of an LFO state machine — what an [`LfoEvent`] stream
/// resolves to at a specific tick. `armed = false` corresponds to
/// "the LFO has been cleared (or never set)", in which case the
/// other fields are last-known values that should not be read.
///
/// Used by [`AutomationLane::lfo_state_at`] to reconstruct, from
/// the event stream alone, the equivalent of what the legacy
/// cell-side Vibrato / Tremolo / Panbrello effects would have set
/// — the runtime player consumes it directly (no cell fallback)
/// since DAW migration Phase 3c.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LfoState {
    pub armed: bool,
    pub speed: Q8_8,
    pub depth: Q15,
    pub waveform: Waveform,
    pub retrig: bool,
}

impl Default for LfoState {
    fn default() -> Self {
        Self {
            armed: false,
            speed: Q8_8::ZERO,
            depth: Q15::ZERO,
            waveform: Waveform::Sine,
            retrig: true,
        }
    }
}

/// Snapshot of a linear-slide state machine. `armed = false`
/// corresponds to "no slide active at this tick".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SlideState {
    pub armed: bool,
    pub rate: Q15,
    pub fine: bool,
}

/// Snapshot of a glide-toward-target state machine. `armed = false`
/// = no glide active. When armed, `target` is the most recent
/// `Pitch` requested by a `Set` event and `rate` the period-unit
/// speed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlideState {
    pub armed: bool,
    pub target: Pitch,
    pub rate: i16,
}

impl Default for GlideState {
    fn default() -> Self {
        Self {
            armed: false,
            target: Pitch::C5,
            rate: 0,
        }
    }
}

/// Unified read view of one event in a lane, regardless of kind.
/// Used by [`AutomationLane::events_in`] so the runtime can iterate
/// every kind through the same interface.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LaneEvent<'a> {
    Point(&'a AutomationPoint),
    Lfo(&'a LfoEvent),
    Slide(&'a SlideEvent),
    Glide(&'a GlideEvent),
}

impl<'a> LaneEvent<'a> {
    #[inline]
    pub fn tick(&self) -> u32 {
        match self {
            Self::Point(p) => p.tick,
            Self::Lfo(e) => e.tick(),
            Self::Slide(e) => e.tick(),
            Self::Glide(e) => e.tick(),
        }
    }
}

/// One automation curve: a target parameter, a payload of either
/// typed `(tick, value)` points or a stream of continuous-modulation
/// events, and an enable flag.
///
/// Lanes are stored in [`crate::core::module::Module::automation`]
/// and looked up at runtime by [`Module::lane_for`] /
/// [`Module::lanes_for`]. The player reads the active lane each
/// tick to drive the corresponding parameter; lanes are also the
/// editor's single source of truth for continuous effects (Vibrato
/// / Tremolo / Panbrello, slides, glides, song-level Bpm / Speed /
/// GlobalVolume).
///
/// Lane events / points are kept sorted by tick. The edit API and
/// the importer-side [`crate::tracker::import::extract`] pipeline
/// both preserve this invariant; consumers can rely on it for
/// binary search and incremental playback.
///
/// [`Module::lane_for`]: crate::core::module::Module::lane_for
/// [`Module::lanes_for`]: crate::core::module::Module::lanes_for
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AutomationLane {
    /// Which parameter this curve drives. See [`AutomationTarget`]
    /// for the full list (song-wide `Bpm` / `Speed` / `GlobalVolume`
    /// vs. per-Track `TrackPitch(n)` / `TrackVolume(n)` /
    /// `TrackPanning(n)` / `TrackChannelVolume(n)`).
    pub target: AutomationTarget,
    /// Payload — one variant per modulation class. Events inside
    /// each variant are sorted by tick (ascending); the API
    /// maintains this invariant.
    pub kind: LaneKind,
    /// Mute switch. When `false`, the player skips this lane —
    /// useful from the editor for A/B comparisons without
    /// destroying the curve.
    pub enabled: bool,
}

impl AutomationLane {
    /// Build an empty [`LaneKind::Points`] lane for `target`. The
    /// historical (Phase 0) entry point; preserved for backwards
    /// compatibility with the `edit.rs` API surface.
    pub fn new(target: AutomationTarget) -> Self {
        Self {
            target,
            kind: LaneKind::default(),
            enabled: true,
        }
    }

    /// Build an empty lane of the given kind for `target`. Used by
    /// the Phase 3b extractor to seed Lfo / Slide / Glide lanes.
    pub fn new_with_kind(target: AutomationTarget, kind: LaneKind) -> Self {
        Self {
            target,
            kind,
            enabled: true,
        }
    }

    /// Read-only accessor onto a Points lane's points.
    /// `None` for any other [`LaneKind`].
    #[inline]
    pub fn points(&self) -> Option<&[AutomationPoint]> {
        if let LaneKind::Points(p) = &self.kind {
            Some(p)
        } else {
            None
        }
    }

    /// Mutable accessor onto a Points lane's points.
    /// `None` for any other [`LaneKind`].
    #[inline]
    pub fn points_mut(&mut self) -> Option<&mut Vec<AutomationPoint>> {
        if let LaneKind::Points(p) = &mut self.kind {
            Some(p)
        } else {
            None
        }
    }

    /// Insert a point, keeping the Points-payload sorted by tick.
    /// If a point already exists at the same tick it is replaced.
    /// Panics if the lane is not a [`LaneKind::Points`].
    pub fn insert_point(&mut self, point: AutomationPoint) {
        let points = self
            .points_mut()
            .expect("insert_point called on non-Points lane");
        match points.binary_search_by_key(&point.tick, |p| p.tick) {
            Ok(idx) => points[idx] = point,
            Err(idx) => points.insert(idx, point),
        }
    }

    /// Iterate every event of the lane whose `tick ∈ [start_tick,
    /// end_tick)`, sorted ascending by tick. Used by the runtime
    /// (Phase 3c) to advance its state machines tick-by-tick.
    ///
    /// Allocates a `Vec` of references for the half-open window —
    /// the caller is expected to consume it before the next call;
    /// the runtime typically reads a single-tick window and so
    /// pays for ≤ 1-2 elements per call in practice.
    pub fn events_in(&self, start_tick: u32, end_tick: u32) -> alloc::vec::Vec<LaneEvent<'_>> {
        let mut out: alloc::vec::Vec<LaneEvent<'_>> = alloc::vec::Vec::new();
        match &self.kind {
            LaneKind::Points(points) => {
                for p in points {
                    if p.tick >= start_tick && p.tick < end_tick {
                        out.push(LaneEvent::Point(p));
                    }
                }
            }
            LaneKind::Lfo { events } => {
                for e in events {
                    let t = e.tick();
                    if t >= start_tick && t < end_tick {
                        out.push(LaneEvent::Lfo(e));
                    }
                }
            }
            LaneKind::Slide { events } => {
                for e in events {
                    let t = e.tick();
                    if t >= start_tick && t < end_tick {
                        out.push(LaneEvent::Slide(e));
                    }
                }
            }
            LaneKind::Glide { events } => {
                for e in events {
                    let t = e.tick();
                    if t >= start_tick && t < end_tick {
                        out.push(LaneEvent::Glide(e));
                    }
                }
            }
        }
        out
    }

    /// For [`LaneKind::Points`] only: returns the value at `tick`
    /// — the latest point whose `tick <= tick`, or `None` if no
    /// point precedes it. Returns `None` for non-Points kinds.
    pub fn value_at(&self, tick: u32) -> Option<AutomationValue> {
        let points = self.points()?;
        if points.is_empty() {
            return None;
        }
        // points are sorted ascending by tick.
        let idx = match points.binary_search_by_key(&tick, |p| p.tick) {
            Ok(i) => i,
            Err(0) => return None,
            Err(i) => i - 1,
        };
        Some(points[idx].value)
    }

    /// Replay a [`LaneKind::Lfo`] event stream up to (and including)
    /// `tick`, returning the resulting [`LfoState`] — i.e. what an
    /// LFO state machine fed by this lane would be holding at that
    /// tick. Returns `None` for any non-Lfo lane.
    ///
    /// `tick` is inclusive: events emitted *at* `tick` count.
    pub fn lfo_state_at(&self, tick: u32) -> Option<LfoState> {
        let events = if let LaneKind::Lfo { events } = &self.kind {
            events
        } else {
            return None;
        };
        let mut s = LfoState::default();
        for e in events {
            if e.tick() > tick {
                break;
            }
            match *e {
                LfoEvent::Set {
                    speed,
                    depth,
                    waveform,
                    retrig,
                    ..
                } => {
                    s.armed = true;
                    s.speed = speed;
                    s.depth = depth;
                    s.waveform = waveform;
                    s.retrig = retrig;
                }
                LfoEvent::DepthOnly { depth, .. } => {
                    s.armed = true;
                    s.depth = depth;
                }
                LfoEvent::SpeedOnly { speed, .. } => {
                    s.armed = true;
                    s.speed = speed;
                }
                LfoEvent::WaveformOnly {
                    waveform, retrig, ..
                } => {
                    // Updates the shape without touching `armed`,
                    // `speed`, or `depth`. Matches the runtime's
                    // cell-driven dispatch where `*Waveform` rows
                    // mutate `effect_*.data.waveform` immediately
                    // without re-arming the LFO.
                    s.waveform = waveform;
                    s.retrig = retrig;
                }
                LfoEvent::Clear { .. } => {
                    s.armed = false;
                }
            }
        }
        Some(s)
    }

    /// Replay a [`LaneKind::Slide`] event stream up to `tick` and
    /// return the resulting [`SlideState`]. `None` for non-Slide
    /// lanes.
    pub fn slide_state_at(&self, tick: u32) -> Option<SlideState> {
        let events = if let LaneKind::Slide { events } = &self.kind {
            events
        } else {
            return None;
        };
        let mut s = SlideState::default();
        for e in events {
            if e.tick() > tick {
                break;
            }
            match *e {
                SlideEvent::Set { rate, fine, .. } => {
                    s.armed = true;
                    s.rate = rate;
                    s.fine = fine;
                }
                SlideEvent::Clear { .. } => {
                    s.armed = false;
                }
            }
        }
        Some(s)
    }

    /// Replay a [`LaneKind::Glide`] event stream up to `tick` and
    /// return the resulting [`GlideState`]. `None` for non-Glide
    /// lanes.
    pub fn glide_state_at(&self, tick: u32) -> Option<GlideState> {
        let events = if let LaneKind::Glide { events } = &self.kind {
            events
        } else {
            return None;
        };
        let mut s = GlideState::default();
        for e in events {
            if e.tick() > tick {
                break;
            }
            match *e {
                GlideEvent::Set { target, rate, .. } => {
                    s.armed = true;
                    s.target = target;
                    s.rate = rate;
                }
                GlideEvent::Clear { .. } => {
                    s.armed = false;
                }
            }
        }
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn pts_lane() -> AutomationLane {
        let mut lane = AutomationLane::new(AutomationTarget::Bpm);
        lane.insert_point(AutomationPoint {
            tick: 0,
            value: AutomationValue::Bpm(125),
        });
        lane.insert_point(AutomationPoint {
            tick: 96,
            value: AutomationValue::Bpm(140),
        });
        lane.insert_point(AutomationPoint {
            tick: 192,
            value: AutomationValue::Bpm(100),
        });
        lane
    }

    #[test]
    fn value_at_returns_latest_point() {
        let lane = pts_lane();
        assert_eq!(lane.value_at(0), Some(AutomationValue::Bpm(125)));
        assert_eq!(lane.value_at(50), Some(AutomationValue::Bpm(125)));
        assert_eq!(lane.value_at(96), Some(AutomationValue::Bpm(140)));
        assert_eq!(lane.value_at(150), Some(AutomationValue::Bpm(140)));
        assert_eq!(lane.value_at(192), Some(AutomationValue::Bpm(100)));
        assert_eq!(lane.value_at(1000), Some(AutomationValue::Bpm(100)));
    }

    #[test]
    fn value_at_returns_none_before_first_point() {
        let mut lane = AutomationLane::new(AutomationTarget::Bpm);
        lane.insert_point(AutomationPoint {
            tick: 100,
            value: AutomationValue::Bpm(120),
        });
        assert_eq!(lane.value_at(0), None);
        assert_eq!(lane.value_at(99), None);
        assert_eq!(lane.value_at(100), Some(AutomationValue::Bpm(120)));
    }

    #[test]
    fn value_at_is_none_for_non_points_kind() {
        let lane = AutomationLane::new_with_kind(
            AutomationTarget::TrackPitch(0),
            LaneKind::Lfo { events: vec![] },
        );
        assert_eq!(lane.value_at(0), None);
    }

    #[test]
    fn events_in_filters_window_for_points() {
        let lane = pts_lane();
        let evs: Vec<u32> = lane.events_in(50, 200).iter().map(|e| e.tick()).collect();
        assert_eq!(evs, vec![96, 192]);
    }

    #[test]
    fn events_in_filters_window_for_lfo() {
        let lane = AutomationLane::new_with_kind(
            AutomationTarget::TrackPitch(0),
            LaneKind::Lfo {
                events: vec![
                    LfoEvent::Set {
                        tick: 0,
                        speed: Q8_8::from_int(3),
                        depth: Q15::from_ratio(1, 4),
                        waveform: Waveform::Sine,
                        retrig: true,
                    },
                    LfoEvent::Clear { tick: 24 },
                ],
            },
        );
        let evs: Vec<u32> = lane.events_in(0, 48).iter().map(|e| e.tick()).collect();
        assert_eq!(evs, vec![0, 24]);
        let evs: Vec<u32> = lane.events_in(0, 24).iter().map(|e| e.tick()).collect();
        assert_eq!(evs, vec![0]); // end is exclusive
    }

    #[test]
    fn new_targets_are_compatible_with_normalized_values() {
        let v = AutomationValue::Normalized(Q15::ZERO);
        assert!(v.is_compatible_with(AutomationTarget::TrackPitch(7)));
        assert!(v.is_compatible_with(AutomationTarget::TrackChannelVolume(3)));
    }

    // ----- Lane state replay (Phase 3c.1 prep) -----------------------------

    fn lfo_lane(events: Vec<LfoEvent>) -> AutomationLane {
        AutomationLane::new_with_kind(AutomationTarget::TrackPitch(0), LaneKind::Lfo { events })
    }

    #[test]
    fn lfo_state_at_pre_first_event_is_default() {
        let lane = lfo_lane(vec![LfoEvent::Set {
            tick: 100,
            speed: Q8_8::from_int(3),
            depth: Q15::from_ratio(1, 4),
            waveform: Waveform::Square,
            retrig: false,
        }]);
        let s = lane.lfo_state_at(0).expect("lfo lane");
        assert!(!s.armed);
        // Default values: speed=0, depth=0, waveform=Sine, retrig=true.
        assert_eq!(s.speed, Q8_8::ZERO);
        assert_eq!(s.depth, Q15::ZERO);
        assert_eq!(s.waveform, Waveform::Sine);
        assert!(s.retrig);
    }

    #[test]
    fn lfo_state_at_set_arms_and_carries_params() {
        let lane = lfo_lane(vec![LfoEvent::Set {
            tick: 100,
            speed: Q8_8::from_int(3),
            depth: Q15::from_ratio(1, 4),
            waveform: Waveform::Square,
            retrig: false,
        }]);
        let s = lane.lfo_state_at(100).unwrap();
        assert!(s.armed);
        assert_eq!(s.speed, Q8_8::from_int(3));
        assert_eq!(s.depth, Q15::from_ratio(1, 4));
        assert_eq!(s.waveform, Waveform::Square);
        assert!(!s.retrig);
        // Past the Set, before any further event — state holds.
        let s2 = lane.lfo_state_at(1000).unwrap();
        assert!(s2.armed);
        assert_eq!(s2, s);
    }

    #[test]
    fn lfo_state_at_clear_disarms_but_keeps_params() {
        let lane = lfo_lane(vec![
            LfoEvent::Set {
                tick: 0,
                speed: Q8_8::from_int(3),
                depth: Q15::from_ratio(1, 4),
                waveform: Waveform::Sine,
                retrig: true,
            },
            LfoEvent::Clear { tick: 12 },
        ]);
        let s = lane.lfo_state_at(11).unwrap();
        assert!(s.armed);
        let s = lane.lfo_state_at(12).unwrap();
        assert!(!s.armed, "Clear should disarm at its tick");
        // Params survive the clear (last-known wins) — the runtime
        // can re-arm without re-providing them via a fresh Set.
        assert_eq!(s.speed, Q8_8::from_int(3));
        assert_eq!(s.depth, Q15::from_ratio(1, 4));
    }

    #[test]
    fn lfo_state_at_depth_only_updates_only_depth() {
        let lane = lfo_lane(vec![
            LfoEvent::Set {
                tick: 0,
                speed: Q8_8::from_int(3),
                depth: Q15::from_ratio(1, 4),
                waveform: Waveform::Sine,
                retrig: true,
            },
            LfoEvent::DepthOnly {
                tick: 6,
                depth: Q15::from_ratio(1, 2),
            },
        ]);
        let s = lane.lfo_state_at(6).unwrap();
        assert!(s.armed);
        assert_eq!(s.speed, Q8_8::from_int(3));
        assert_eq!(s.depth, Q15::from_ratio(1, 2));
    }

    #[test]
    fn lfo_state_at_speed_only_updates_only_speed() {
        let lane = lfo_lane(vec![
            LfoEvent::Set {
                tick: 0,
                speed: Q8_8::from_int(3),
                depth: Q15::from_ratio(1, 4),
                waveform: Waveform::Sine,
                retrig: true,
            },
            LfoEvent::SpeedOnly {
                tick: 6,
                speed: Q8_8::from_int(7),
            },
        ]);
        let s = lane.lfo_state_at(6).unwrap();
        assert_eq!(s.speed, Q8_8::from_int(7));
        assert_eq!(s.depth, Q15::from_ratio(1, 4));
    }

    #[test]
    fn lfo_state_at_returns_none_for_non_lfo_kind() {
        let lane = AutomationLane::new(AutomationTarget::Bpm);
        assert!(lane.lfo_state_at(0).is_none());
    }

    fn slide_lane(events: Vec<SlideEvent>) -> AutomationLane {
        AutomationLane::new_with_kind(AutomationTarget::TrackVolume(0), LaneKind::Slide { events })
    }

    #[test]
    fn slide_state_replays_set_and_clear() {
        let lane = slide_lane(vec![
            SlideEvent::Set {
                tick: 0,
                rate: Q15::from_raw(5),
                fine: false,
            },
            SlideEvent::Clear { tick: 12 },
        ]);
        let s = lane.slide_state_at(0).unwrap();
        assert!(s.armed && s.rate == Q15::from_raw(5) && !s.fine);
        let s = lane.slide_state_at(12).unwrap();
        assert!(!s.armed);
        // Pre-first-event = default (not armed).
        let s = lane.slide_state_at(0).unwrap();
        assert!(s.armed); // Set was at tick 0
    }

    fn glide_lane(events: Vec<GlideEvent>) -> AutomationLane {
        AutomationLane::new_with_kind(AutomationTarget::TrackPitch(0), LaneKind::Glide { events })
    }

    #[test]
    fn glide_state_replays_set_retarget_clear() {
        let lane = glide_lane(vec![
            GlideEvent::Set {
                tick: 0,
                target: Pitch::C5,
                rate: 5,
            },
            GlideEvent::Set {
                tick: 6,
                target: Pitch::E5,
                rate: 5,
            },
            GlideEvent::Clear { tick: 12 },
        ]);
        let s = lane.glide_state_at(0).unwrap();
        assert!(s.armed && s.target == Pitch::C5);
        let s = lane.glide_state_at(6).unwrap();
        assert!(s.armed && s.target == Pitch::E5);
        let s = lane.glide_state_at(12).unwrap();
        assert!(!s.armed);
    }
}
