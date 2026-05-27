//! Per-channel instrument-coherent segmentation and Clip emission.
//! Pass 2 of [`super::build_timeline_layer`]: each pattern channel
//! is split into segments by instrument changes and rendered into a
//! [`Track`] + one [`Clip`] per `(order entry × segment)`.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::core::cell::Cell;
use crate::core::cell_note::CellNote;
use crate::core::daw::clip::Clip;
use crate::core::daw::timeline::{TimelineEntry, TimelineMap};
use crate::core::daw::track::Track;
use crate::tracker::import::unit::TrackImportUnit;

use super::{Pattern, Row, EFFECT_ONLY_INSTRUMENT};

/// A segment of a single-channel row sequence with a coherent
/// instrument. Public so track-native importers can plug into the
/// DAW layer without re-implementing the split logic.
pub struct TrackSegment {
    pub instrument: usize,
    pub rows: Vec<TrackImportUnit>,
    pub start_row: u32,
}

/// Split a flat row sequence into instrument-coherent segments.
/// Cells with no explicit instrument inherit the current run's
/// instrument; a cell whose explicit instrument differs from the
/// current one starts a new segment. Empty cells are tolerated
/// inside a segment.
///
/// Exposed for track-native importers (e.g. the SID importer)
/// that produce a per-voice row stream directly and don't go
/// through the pattern-grid path.
pub fn split_rows_by_instrument(rows: &[TrackImportUnit]) -> Vec<TrackSegment> {
    let mut segments: Vec<TrackSegment> = Vec::new();
    let mut current_instrument: Option<usize> = None;
    let mut current_rows: Vec<TrackImportUnit> = Vec::new();
    let mut current_start_row: u32 = 0;
    let mut leading_rows: Vec<TrackImportUnit> = Vec::new();
    let mut leading_start: Option<u32> = None;

    for (r_idx, cell) in rows.iter().enumerate() {
        let r_idx = r_idx as u32;
        let new_instr_explicit = cell.instrument;
        let triggers_split = matches!(
            (current_instrument, new_instr_explicit),
            (Some(cur), Some(new)) if cur != new && is_cell_meaningful(cell)
        );

        if triggers_split {
            if let Some(instr) = current_instrument {
                segments.push(TrackSegment {
                    instrument: instr,
                    rows: core::mem::take(&mut current_rows),
                    start_row: current_start_row,
                });
            }
            current_instrument = new_instr_explicit;
            current_start_row = r_idx;
        }

        if current_instrument.is_none() {
            if let Some(new) = new_instr_explicit {
                if let Some(start) = leading_start.take() {
                    segments.push(TrackSegment {
                        instrument: EFFECT_ONLY_INSTRUMENT,
                        rows: core::mem::take(&mut leading_rows),
                        start_row: start,
                    });
                }
                current_instrument = Some(new);
                current_start_row = r_idx;
            }
        }

        if current_instrument.is_some() {
            current_rows.push(cell.clone());
        } else if is_cell_meaningful(cell) {
            if leading_start.is_none() {
                leading_start = Some(r_idx);
            }
            leading_rows.push(cell.clone());
        } else if leading_start.is_some() {
            leading_rows.push(cell.clone());
        }
    }

    if let Some(instr) = current_instrument {
        if !current_rows.is_empty() {
            segments.push(TrackSegment {
                instrument: instr,
                rows: current_rows,
                start_row: current_start_row,
            });
        }
    } else if let Some(start) = leading_start {
        segments.push(TrackSegment {
            instrument: EFFECT_ONLY_INSTRUMENT,
            rows: leading_rows,
            start_row: start,
        });
    }

    segments
}

fn split_channel_by_instrument(pattern: &[Row], channel: usize) -> Vec<TrackSegment> {
    let column: Vec<TrackImportUnit> = pattern
        .iter()
        .map(|row| row.get(channel).cloned().unwrap_or_default())
        .collect();
    split_rows_by_instrument(&column)
}

fn is_cell_meaningful(cell: &TrackImportUnit) -> bool {
    !matches!(cell.note, CellNote::Empty) || !cell.effects.is_empty() || cell.has_any_global()
}

fn materialise_segment_rows(rows: Vec<TrackImportUnit>) -> Vec<Cell> {
    rows.into_iter().map(|tiu| tiu.prepare_cell()).collect()
}

#[derive(Eq, PartialEq, Ord, PartialOrd, Debug, Clone, Copy)]
struct SegmentKey {
    song: u16,
    pattern_idx: u32,
    channel: u32,
    segment_idx: u32,
}

struct ExtractedSegment {
    track_idx: u32,
    start_row: u32,
    length: u32,
}

/// Extract Tracks and Clips from a pattern grid.
pub fn extract_tracks_and_clips(
    pattern: &[Pattern],
    timeline_map: &TimelineMap,
) -> (Vec<Track>, Vec<Clip>) {
    let mut tracks: Vec<Track> = Vec::new();
    let mut segments_map: BTreeMap<SegmentKey, ExtractedSegment> = BTreeMap::new();

    let mut song_patterns: BTreeMap<(u16, u32), ()> = BTreeMap::new();
    for entry in &timeline_map.entries {
        song_patterns.insert((entry.song, entry.pattern_idx), ());
    }

    for (song, pattern_idx) in song_patterns.keys().copied() {
        let pat_usize = pattern_idx as usize;
        if pat_usize >= pattern.len() {
            continue;
        }
        let pat = &pattern[pat_usize];
        if pat.is_empty() {
            continue;
        }
        let num_channels = pat[0].len();
        for ch in 0..num_channels {
            let segments = split_channel_by_instrument(pat, ch);
            for (seg_idx, seg) in segments.into_iter().enumerate() {
                let track_idx = tracks.len() as u32;
                let length = seg.rows.len() as u32;
                let name = make_track_name(song, pattern_idx, ch as u32, seg_idx as u32);
                tracks.push(Track::Notes {
                    name,
                    instrument: seg.instrument,
                    rows: materialise_segment_rows(seg.rows),
                    muted: false,
                });
                segments_map.insert(
                    SegmentKey {
                        song,
                        pattern_idx,
                        channel: ch as u32,
                        segment_idx: seg_idx as u32,
                    },
                    ExtractedSegment {
                        track_idx,
                        start_row: seg.start_row,
                        length,
                    },
                );
            }
        }
    }

    let mut entry_points: BTreeMap<(u16, u32), TimelineEntry> = BTreeMap::new();
    for entry in &timeline_map.entries {
        if entry.loop_iter != 0 {
            continue;
        }
        let key = (entry.song, entry.order_idx);
        entry_points.entry(key).or_insert(*entry);
    }

    let mut clips: Vec<Clip> = Vec::new();
    for entry in entry_points.values() {
        for ((_, _, ch_key, _), seg) in segments_map.iter().filter_map(|(k, v)| {
            if k.song == entry.song && k.pattern_idx == entry.pattern_idx {
                Some(((k.song, k.pattern_idx, k.channel, k.segment_idx), v))
            } else {
                None
            }
        }) {
            let entry_row = entry.row_idx;
            let speed_fallback = entry.speed_at_row as u32;
            let song = entry.song;
            let order = entry.order_idx;
            let pat_u32 = entry.pattern_idx;
            let seg_end_row = seg.start_row + seg.length;
            let end_tick = clip_end_tick_from_timeline(
                timeline_map,
                song,
                order,
                pat_u32,
                seg_end_row,
                speed_fallback,
            );
            if seg.start_row >= entry_row {
                let position_tick = timeline_map
                    .find_entry_at_order(song as usize, order as usize, seg.start_row as usize)
                    .map(|e| e.tick)
                    .unwrap_or_else(|| {
                        entry
                            .tick
                            .saturating_add((seg.start_row - entry_row) * speed_fallback)
                    });
                clips.push(Clip {
                    track: seg.track_idx,
                    song,
                    target_channel: ch_key as u8,
                    position_tick,
                    speed_at_start: entry.speed_at_row,
                    track_row_offset: 0,
                    source_start_row: seg.start_row,
                    end_tick,
                });
            } else if seg_end_row > entry_row {
                clips.push(Clip {
                    track: seg.track_idx,
                    song,
                    target_channel: ch_key as u8,
                    position_tick: entry.tick,
                    speed_at_start: entry.speed_at_row,
                    track_row_offset: entry_row - seg.start_row,
                    source_start_row: seg.start_row,
                    end_tick,
                });
            }
        }
    }

    (tracks, clips)
}

fn clip_end_tick_from_timeline(
    timeline_map: &TimelineMap,
    song: u16,
    order_idx: u32,
    pattern_idx: u32,
    seg_end_row: u32,
    fallback_speed: u32,
) -> u32 {
    if let Some(e) =
        timeline_map.find_entry_at_order(song as usize, order_idx as usize, seg_end_row as usize)
    {
        return e.tick;
    }
    let mut best: Option<(u32, u32, u32)> = None;
    for e in &timeline_map.entries {
        if e.song != song || e.order_idx != order_idx || e.loop_iter != 0 {
            continue;
        }
        if e.row_idx >= seg_end_row {
            continue;
        }
        let candidate = (e.row_idx, e.tick, e.speed_at_row as u32);
        if best.is_none_or(|(r, _, _)| candidate.0 > r) {
            best = Some(candidate);
        }
    }
    let _ = pattern_idx;
    match best {
        Some((_, tick, speed)) => tick.saturating_add(speed),
        None => seg_end_row.saturating_mul(fallback_speed),
    }
}

fn make_track_name(song: u16, pattern_idx: u32, channel: u32, segment_idx: u32) -> String {
    use core::fmt::Write;
    let mut s = String::new();
    let _ = write!(
        &mut s,
        "s{}p{}c{}#{}",
        song, pattern_idx, channel, segment_idx
    );
    s
}
