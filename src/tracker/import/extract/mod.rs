//! DAW migration Phase 3c.3 — extraction of continuous effects
//! into [`crate::core::daw::automation::AutomationLane`]s, sourced
//! **upstream** from
//! [`crate::tracker::import::unit::TrackImportUnit::effects`] /
//! [`crate::tracker::import::unit::TrackImportUnit::song_level`]
//! (importer-side raw effects), before any cell materialisation.
//!
//! The runtime player consumes the lanes exclusively for the
//! continuous classes — `Cell.effects` no longer carries the migrated
//! variants (Vibrato* / Tremolo* / Panbrello* / Portamento /
//! TonePortamento / VolumeSlide / ChannelVolumeSlide / PanningSlide).
//!
//! ## Effect → lane mapping
//!
//! | TrackImportEffect family                  | Lane kind | Target                       |
//! |-------------------------------------------|-----------|------------------------------|
//! | Vibrato / VibratoFine / VibratoSpeed /    |           |                              |
//! |   VibratoDepth / VibratoDepthFxVol /      |           |                              |
//! |   VibratoWaveform                         | `Lfo`     | `TrackPitch(n)`              |
//! | Tremolo / TremoloWaveform                 | `Lfo`     | `TrackVolume(n)`             |
//! | Panbrello / PanbrelloWaveform             | `Lfo`     | `TrackPanning(n)`            |
//! | Portamento{Up,Down,FineUp,FineDown,       |           |                              |
//! |   ExtraFineUp,ExtraFineDown}              | `Slide`   | `TrackPitch(n)`              |
//! | VolumeSlide0 / VolumeSlideN               | `Slide`   | `TrackVolume(n)`             |
//! | ChannelVolumeSlide0 / ChannelVolumeSlideN | `Slide`   | `TrackChannelVolume(n)`      |
//! | PanningSlide0 / PanningSlideN             | `Slide`   | `TrackPanning(n)`            |
//! | TonePortamento / TonePortamentoFxVol      | `Glide`   | `TrackPitch(n)`              |
//! | SongLevelEffect::Bpm                      | `Points`  | `Bpm`                        |
//! | SongLevelEffect::BpmSlide                 | `Slide`   | `Bpm`                        |
//! | SongLevelEffect::Speed                    | `Points`  | `Speed`                      |
//! | SongLevelEffect::Volume                   | `Points`  | `GlobalVolume`               |
//! | SongLevelEffect::VolumeSlide              | `Slide`   | `GlobalVolume`               |
//!
//! Implementation is split across private sub-modules by class —
//! `lfo` / `slide` / `glide` / `song`. This file owns the
//! `RowVisit` / `GlobalRowVisit` shared types, the test helpers
//! `rows_at_speed*`, and the cross-class orchestrators that walk a
//! `timeline_map` to feed each per-class extractor.

use alloc::vec::Vec;

use crate::core::cell_note::CellNote;
use crate::core::compatibility::PlaybackQuirks;
use crate::core::daw::automation::{AutomationLane, AutomationTarget, LaneKind};
use crate::core::effect::SongLevelEffect;
use crate::tracker::import::effect::TrackImportEffect;

mod glide;
mod lfo;
mod slide;
mod song;

pub use glide::extract_tone_portamento_events;
pub use lfo::{extract_panbrello_events, extract_tremolo_events, extract_vibrato_events};
pub use slide::{
    extract_channel_volume_slide_events, extract_panning_slide_events, extract_portamento_events,
    extract_volume_slide_events,
};
pub use song::{
    extract_bpm_points, extract_bpm_slide_events, extract_global_volume_points,
    extract_global_volume_slide_events, extract_speed_points,
};

/// One row visit feeding the per-Track LFO/Slide/Glide extractors.
/// `tick` is the absolute song-tick at which the row plays;
/// consecutive entries must be strictly increasing.
///
/// Sourced directly from
/// [`crate::tracker::import::unit::TrackImportUnit`] so the
/// extractor never depends on the runtime [`crate::core::cell::Cell`].
#[derive(Debug, Clone, Copy)]
pub struct RowVisit<'a> {
    pub tick: u32,
    pub effects: &'a [TrackImportEffect],
    /// Note column — used by the TonePortamento glide extractor to
    /// resolve the target pitch. `CellNote::Empty` for rows without
    /// a note-column event.
    pub note: CellNote,
}

/// One row visit feeding the song-level Bpm / Speed / GlobalVolume
/// / BpmSlide / GlobalVolumeSlide extractors.
#[derive(Debug, Clone, Copy)]
pub struct GlobalRowVisit<'a> {
    pub tick: u32,
    pub effects: &'a [SongLevelEffect],
}

/// Convenience constructor for unit tests: builds a synthetic
/// `RowVisit` sequence (one row per inner Vec, all with empty
/// `note`) assuming a fixed `speed` ticks per row, starting at
/// tick 0.
pub fn rows_at_speed<'a>(per_row: &'a [Vec<TrackImportEffect>], speed: u32) -> Vec<RowVisit<'a>> {
    per_row
        .iter()
        .enumerate()
        .map(|(i, v)| RowVisit {
            tick: i as u32 * speed,
            effects: v.as_slice(),
            note: CellNote::Empty,
        })
        .collect()
}

/// Convenience constructor for unit tests, with explicit per-row
/// note columns (for glide-target tests).
pub fn rows_at_speed_with_notes<'a>(
    per_row: &'a [(CellNote, Vec<TrackImportEffect>)],
    speed: u32,
) -> Vec<RowVisit<'a>> {
    per_row
        .iter()
        .enumerate()
        .map(|(i, (note, v))| RowVisit {
            tick: i as u32 * speed,
            effects: v.as_slice(),
            note: *note,
        })
        .collect()
}

/// Convenience constructor for unit tests: synthetic
/// `GlobalRowVisit` sequence.
pub fn global_rows_at_speed<'a>(
    per_row: &'a [Vec<SongLevelEffect>],
    speed: u32,
) -> Vec<GlobalRowVisit<'a>> {
    per_row
        .iter()
        .enumerate()
        .map(|(i, v)| GlobalRowVisit {
            tick: i as u32 * speed,
            effects: v.as_slice(),
        })
        .collect()
}

/// Per-track row accumulator: keyed by `track_idx` → list of
/// `(abs_tick, note, effects)` per visited row. Consumed by
/// [`extract_per_track_lanes_from_tius`] (and internally by
/// [`extract_per_track_lanes_from_patterns`] after walking the
/// timeline).
pub type PerTrackRows =
    alloc::collections::BTreeMap<u32, Vec<(u32, CellNote, Vec<TrackImportEffect>)>>;

/// Run every per-Track per-class extractor on `per_track_rows` and
/// append any non-empty lanes.
fn push_per_track_lanes(
    lanes: &mut Vec<AutomationLane>,
    per_track_rows: &PerTrackRows,
    quirks: &PlaybackQuirks,
    song: u16,
) {
    let push_lane = |lanes: &mut Vec<AutomationLane>, target, kind: LaneKind| {
        let non_empty = match &kind {
            LaneKind::Points(p) => !p.is_empty(),
            LaneKind::Lfo { events } => !events.is_empty(),
            LaneKind::Slide { events } => !events.is_empty(),
            LaneKind::Glide { events } => !events.is_empty(),
        };
        if non_empty {
            lanes.push(AutomationLane::new_with_kind(target, kind).with_song(song));
        }
    };

    for (track_idx, rows_owned) in per_track_rows {
        let rows: Vec<RowVisit<'_>> = rows_owned
            .iter()
            .map(|(t, note, effects)| RowVisit {
                tick: *t,
                effects: effects.as_slice(),
                note: *note,
            })
            .collect();

        push_lane(
            lanes,
            AutomationTarget::TrackPitch(*track_idx),
            LaneKind::Lfo {
                events: extract_vibrato_events(&rows, quirks),
            },
        );
        push_lane(
            lanes,
            AutomationTarget::TrackVolume(*track_idx),
            LaneKind::Lfo {
                events: extract_tremolo_events(&rows, quirks),
            },
        );
        push_lane(
            lanes,
            AutomationTarget::TrackPanning(*track_idx),
            LaneKind::Lfo {
                events: extract_panbrello_events(&rows, quirks),
            },
        );

        push_lane(
            lanes,
            AutomationTarget::TrackPitch(*track_idx),
            LaneKind::Slide {
                events: extract_portamento_events(&rows),
            },
        );
        push_lane(
            lanes,
            AutomationTarget::TrackVolume(*track_idx),
            LaneKind::Slide {
                events: extract_volume_slide_events(&rows),
            },
        );
        push_lane(
            lanes,
            AutomationTarget::TrackChannelVolume(*track_idx),
            LaneKind::Slide {
                events: extract_channel_volume_slide_events(&rows),
            },
        );
        push_lane(
            lanes,
            AutomationTarget::TrackPanning(*track_idx),
            LaneKind::Slide {
                events: extract_panning_slide_events(&rows),
            },
        );

        push_lane(
            lanes,
            AutomationTarget::TrackPitch(*track_idx),
            LaneKind::Glide {
                events: extract_tone_portamento_events(&rows),
            },
        );
    }
}

/// Per-Track lane extraction sourced directly from importer-side
/// patterns. Walks `timeline_map.entries`, looks up each visited
/// [`TrackImportUnit`] and routes its effects through the per-class
/// extractors keyed by the active Clip's Track.
///
/// DAW migration Phase 3c.3 — fully decoupled from the runtime
/// [`crate::core::cell::Cell`]; no intermediate `TrackEffect` materialisation.
///
/// [`TrackImportUnit`]: crate::tracker::import::unit::TrackImportUnit
pub fn extract_per_track_lanes_from_patterns(
    pattern: &[crate::tracker::import::build::Pattern],
    timeline_map: &crate::core::daw::timeline::TimelineMap,
    clips: &crate::core::daw::sorted_clips::SortedClips,
    quirks: &PlaybackQuirks,
) -> Vec<AutomationLane> {
    // Group rows per **sub-song**: each sub-song's timeline restarts at
    // tick 0, so a Track played in two sub-songs yields two independent
    // tick streams. Merging them (the old single-map behaviour) made the
    // resulting lane non-monotonic and let one sub-song's modulation
    // alias into another's identical tick window.
    let mut per_song: alloc::collections::BTreeMap<u16, PerTrackRows> =
        alloc::collections::BTreeMap::new();

    for entry in &timeline_map.entries {
        let Some(pat) = pattern.get(entry.pattern_idx as usize) else {
            continue;
        };
        let Some(row) = pat.get(entry.row_idx as usize) else {
            continue;
        };
        for (ch_idx, tiu) in row.iter().enumerate() {
            let Some((_, clip)) = clips.active_at(entry.song, ch_idx as u8, entry.tick) else {
                continue;
            };
            per_song
                .entry(entry.song)
                .or_default()
                .entry(clip.track)
                .or_default()
                .push((entry.tick, tiu.note, tiu.effects.clone()));
        }
    }

    let mut lanes = Vec::new();
    for (song, per_track_rows) in &per_song {
        push_per_track_lanes(&mut lanes, per_track_rows, quirks, *song);
    }
    lanes
}

/// Per-Track lane extraction for track-native importers that don't
/// build a pattern grid (e.g. SID). The caller supplies the
/// per-track row stream directly: for each Track index, a flat
/// vector of `(abs_tick, note, effects)`. DAW migration
/// Phase 3c.3.
pub fn extract_per_track_lanes_from_tius(
    per_track_rows: &PerTrackRows,
    quirks: &PlaybackQuirks,
) -> Vec<AutomationLane> {
    let mut lanes = Vec::new();
    // Track-native importers (SID) currently project one sub-song.
    push_per_track_lanes(&mut lanes, per_track_rows, quirks, 0);
    lanes
}

/// Append the 5 song-level lanes (Bpm Points / Bpm Slide /
/// Speed Points / GlobalVolume Points / GlobalVolume Slide) extracted
/// from `rows`. Pushes nothing for classes whose extractor returned
/// empty.
fn push_song_lanes(lanes: &mut Vec<AutomationLane>, rows: &[GlobalRowVisit<'_>], song: u16) {
    let bpm_points = extract_bpm_points(rows);
    if !bpm_points.is_empty() {
        lanes.push(
            AutomationLane::new_with_kind(AutomationTarget::Bpm, LaneKind::Points(bpm_points))
                .with_song(song),
        );
    }
    let bpm_slides = extract_bpm_slide_events(rows);
    if !bpm_slides.is_empty() {
        lanes.push(
            AutomationLane::new_with_kind(
                AutomationTarget::Bpm,
                LaneKind::Slide { events: bpm_slides },
            )
            .with_song(song),
        );
    }
    let speed_points = extract_speed_points(rows);
    if !speed_points.is_empty() {
        lanes.push(
            AutomationLane::new_with_kind(AutomationTarget::Speed, LaneKind::Points(speed_points))
                .with_song(song),
        );
    }
    let gvol_points = extract_global_volume_points(rows);
    if !gvol_points.is_empty() {
        lanes.push(
            AutomationLane::new_with_kind(
                AutomationTarget::GlobalVolume,
                LaneKind::Points(gvol_points),
            )
            .with_song(song),
        );
    }
    let gvol_slides = extract_global_volume_slide_events(rows);
    if !gvol_slides.is_empty() {
        lanes.push(
            AutomationLane::new_with_kind(
                AutomationTarget::GlobalVolume,
                LaneKind::Slide {
                    events: gvol_slides,
                },
            )
            .with_song(song),
        );
    }
}

/// Song-level lane extraction sourced directly from importer-side
/// patterns (`TrackImportUnit.song_level`), bypassing the runtime
/// [`crate::core::cell::Cell`] entirely. Used by the pattern-based
/// importers (XM/IT/S3M/MOD) so song-level globals survive the
/// removal of cell-side globals (DAW migration Phase 3c.3).
///
/// Walks `timeline_map.entries` in playback order so per-visit
/// ticks are correct even when a pattern is referenced multiple
/// times in the order or revisited through a `PatternLoop`.
pub fn extract_song_lanes_from_patterns(
    pattern: &[crate::tracker::import::build::Pattern],
    timeline_map: &crate::core::daw::timeline::TimelineMap,
) -> Vec<AutomationLane> {
    // Per **sub-song** (see `extract_per_track_lanes_from_patterns`):
    // Bpm / Speed / GlobalVolume live in each sub-song's own tick space.
    let mut per_song: alloc::collections::BTreeMap<u16, Vec<(u32, Vec<SongLevelEffect>)>> =
        alloc::collections::BTreeMap::new();
    for entry in &timeline_map.entries {
        let pat = match pattern.get(entry.pattern_idx as usize) {
            Some(p) => p,
            None => continue,
        };
        let row = match pat.get(entry.row_idx as usize) {
            Some(r) => r,
            None => continue,
        };
        for tiu in row {
            if !tiu.song_level.is_empty() {
                per_song
                    .entry(entry.song)
                    .or_default()
                    .push((entry.tick, tiu.song_level.clone()));
            }
        }
    }
    let mut lanes = Vec::new();
    for (song, song_rows_owned) in &per_song {
        let song_rows: Vec<GlobalRowVisit<'_>> = song_rows_owned
            .iter()
            .map(|(t, ge)| GlobalRowVisit {
                tick: *t,
                effects: ge.as_slice(),
            })
            .collect();
        push_song_lanes(&mut lanes, &song_rows, *song);
    }
    lanes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::daw::automation::{AutomationLane, AutomationTarget, LaneKind, LfoEvent};
    use crate::core::fixed::fixed::Q8_8;
    use crate::core::fixed::units::PitchDelta;
    use crate::core::pitch::Pitch;
    use alloc::vec;

    fn empty() -> Vec<TrackImportEffect> {
        Vec::new()
    }

    fn xm_quirks() -> PlaybackQuirks {
        let mut q = PlaybackQuirks::default();
        q.volcol_b_advances_vibrato = true;
        q
    }

    fn modern_quirks() -> PlaybackQuirks {
        PlaybackQuirks::default()
    }

    fn vib(speed: i16, depth: i16) -> TrackImportEffect {
        TrackImportEffect::Vibrato(Q8_8::from_int(speed), PitchDelta::from_q8_8_i16(depth))
    }

    /// DAW migration Phase 3c.3: `TrackImportUnit::prepare_cell`
    /// relocates every entry in `TrackImportUnit::midi_macros` to
    /// `cell.effects` as `TrackEffect::MidiMacro`. No
    /// post-materialisation pass needed.
    #[test]
    fn prepare_cell_relocates_midi_macro_to_track_effects() {
        use crate::core::effect::MidiMacroType;
        use crate::tracker::import::unit::TrackImportUnit;

        let mut tiu = TrackImportUnit::default();
        tiu.midi_macros.push(MidiMacroType::Parametric(0));
        let cell = tiu.prepare_cell();
        assert!(
            cell.effects
                .iter()
                .any(|fx| matches!(fx, crate::core::effect::TrackEffect::MidiMacro(_))),
            "MidiMacro must be relocated to cell.effects"
        );
    }

    // ----- Lane replay round-trips ----------------------------------------

    #[test]
    fn lane_replay_vibrato_then_clear() {
        let per_row = vec![vec![vib(3, 6)], vec![vib(3, 6)], empty()];
        let rows = rows_at_speed(&per_row, 6);
        let events = extract_vibrato_events(&rows, &modern_quirks());
        let lane = AutomationLane::new_with_kind(
            AutomationTarget::TrackPitch(0),
            LaneKind::Lfo { events },
        );
        let s = lane.lfo_state_at(0).unwrap();
        assert!(s.armed);
        let s = lane.lfo_state_at(6).unwrap();
        assert!(s.armed);
        let s = lane.lfo_state_at(12).unwrap();
        assert!(!s.armed);
    }

    #[test]
    fn lane_replay_vibrato_intermittent() {
        let per_row = vec![
            vec![vib(4, 8)],
            empty(),
            vec![vib(4, 8)],
            empty(),
            vec![vib(4, 8)],
        ];
        let rows = rows_at_speed(&per_row, 6);
        let events = extract_vibrato_events(&rows, &modern_quirks());
        let lane = AutomationLane::new_with_kind(
            AutomationTarget::TrackPitch(0),
            LaneKind::Lfo { events },
        );
        let s = lane.lfo_state_at(0).unwrap();
        assert!(s.armed);
        let s = lane.lfo_state_at(6).unwrap();
        assert!(!s.armed);
        let s = lane.lfo_state_at(12).unwrap();
        assert!(s.armed);
        let s = lane.lfo_state_at(18).unwrap();
        assert!(!s.armed);
    }

    /// Spot-check end-to-end: load `note.xm` through the full
    /// importer + DAW build pipeline, then look at `module.automation`
    /// — the extracted lanes must be consistent (sorted ticks,
    /// monotonic by tick within each lane).
    #[cfg(feature = "import_xm")]
    #[test]
    fn note_xm_extraction_is_well_formed() {
        let data = include_bytes!("../../../../examples/note.xm");
        let module = crate::core::module::Module::load_xm(data).expect("load note.xm");
        for lane in &module.automation {
            match &lane.kind {
                LaneKind::Points(p) => {
                    for w in p.windows(2) {
                        assert!(
                            w[0].tick <= w[1].tick,
                            "Points lane unsorted: {:?}",
                            lane.target
                        );
                    }
                }
                LaneKind::Lfo { events } => {
                    for w in events.windows(2) {
                        assert!(
                            w[0].tick() <= w[1].tick(),
                            "Lfo lane unsorted: {:?}",
                            lane.target
                        );
                    }
                }
                LaneKind::Slide { events } => {
                    for w in events.windows(2) {
                        assert!(
                            w[0].tick() <= w[1].tick(),
                            "Slide lane unsorted: {:?}",
                            lane.target
                        );
                    }
                }
                LaneKind::Glide { events } => {
                    for w in events.windows(2) {
                        assert!(
                            w[0].tick() <= w[1].tick(),
                            "Glide lane unsorted: {:?}",
                            lane.target
                        );
                    }
                }
            }
            match lane.target {
                AutomationTarget::TrackVolume(i)
                | AutomationTarget::TrackPanning(i)
                | AutomationTarget::TrackPitch(i)
                | AutomationTarget::TrackChannelVolume(i) => {
                    assert!(
                        (i as usize) < module.tracks.len(),
                        "lane targets out-of-range Track {}",
                        i
                    );
                }
                _ => {}
            }
        }
        module
            .verify_layers_consistent()
            .expect("DAW layer consistent");
    }

    #[test]
    fn lane_replay_volcol_b_xm() {
        let per_row = vec![
            vec![vib(4, 8)],
            vec![TrackImportEffect::VibratoDepth(PitchDelta::from_q8_8_i16(
                16,
            ))],
        ];
        let rows = rows_at_speed(&per_row, 6);
        let events = extract_vibrato_events(&rows, &xm_quirks());
        let lane = AutomationLane::new_with_kind(
            AutomationTarget::TrackPitch(0),
            LaneKind::Lfo { events },
        );
        let s = lane.lfo_state_at(0).unwrap();
        assert!(s.armed);
        assert_eq!(s.speed, Q8_8::from_int(4));
        assert_eq!(s.depth.raw(), 8);
        let s = lane.lfo_state_at(6).unwrap();
        assert!(s.armed);
        assert_eq!(s.speed, Q8_8::from_int(4));
        assert_eq!(s.depth.raw(), 16);
    }

    // ----- Phase 3b v2 — multi-traversal extraction ------------------------

    /// A single Track visited at two distinct ticks (e.g. a Clip
    /// playing the same pattern twice) must produce lane events at
    /// both visits, not just the first one. DAW migration
    /// Phase 3b v2 — multi-traversal invariant.
    #[test]
    fn extract_lanes_multi_clip_traversal() {
        let mut per_track_rows: PerTrackRows = alloc::collections::BTreeMap::new();
        per_track_rows.insert(
            0,
            vec![
                (0, CellNote::Empty, vec![vib(3, 6)]),
                (6, CellNote::Empty, empty()),
                (12, CellNote::Empty, empty()),
                (18, CellNote::Empty, empty()),
                (100, CellNote::Empty, vec![vib(3, 6)]),
                (106, CellNote::Empty, empty()),
                (112, CellNote::Empty, empty()),
                (118, CellNote::Empty, empty()),
            ],
        );

        let _ = Pitch::C5;
        let lanes = extract_per_track_lanes_from_tius(&per_track_rows, &modern_quirks());
        let pitch_lane = lanes
            .iter()
            .find(|l| {
                matches!(l.target, AutomationTarget::TrackPitch(0))
                    && matches!(l.kind, LaneKind::Lfo { .. })
            })
            .expect("Lfo lane on TrackPitch(0)");
        let events = match &pitch_lane.kind {
            LaneKind::Lfo { events } => events,
            _ => panic!(),
        };
        let set_ticks: Vec<u32> = events
            .iter()
            .filter_map(|e| {
                if let LfoEvent::Set { tick, .. } = e {
                    Some(*tick)
                } else {
                    None
                }
            })
            .collect();
        assert!(
            set_ticks.contains(&0),
            "first clip's row 0 should produce Set at tick 0 — got {:?}",
            set_ticks
        );
        assert!(
            set_ticks.contains(&100),
            "second clip's row 0 should produce Set at tick 100 — got {:?}",
            set_ticks
        );
    }
}
