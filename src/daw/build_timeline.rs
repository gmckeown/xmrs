//! Build the DAW timeline layer (`tracks`, `clips`, `timeline_map`)
//! from `pattern_order` + `pattern` inputs supplied by importers.
//!
//! Pipeline:
//! 1. [`build_timeline_map`] linearises `pattern_order` to absolute
//!    ticks via the internal walker.
//! 2. [`extract_tracks_and_clips`] splits each channel by instrument
//!    changes and emits one Clip per `(order entry × segment)`.
//! 3. [`dedupe_tracks_by_content`] fuses Tracks whose
//!    `(instrument, rows)` match bit-for-bit.
//! 4. [`crate::daw::euclidean::auto_convert_strict_euclidean_tracks`]
//!    rewrites tracks whose cells form a strict Bjorklund pattern
//!    into an `InstrumentType::Euclidean` wrapper (lossless).
//!
//! [`split_rows_by_instrument`] is also exposed for track-native
//! importers (SID) that don't go through a pattern grid.
//!
//! [`inject_restart_position_jump`] is the import-time helper for
//! the historical "restart byte" (Soundtracker MOD, FastTracker
//! XM): it adds a `GlobalEffect::PositionJump` on the last played
//! row so the existing position-jump pipeline handles the
//! end-of-song wrap without any special-cased field on `Module`.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use crate::cell_note::CellNote;
use crate::daw::clip::Clip;
use crate::daw::timeline::{TimelineEntry, TimelineMap};
use crate::daw::track::Track;
use crate::effect::GlobalEffect;
use crate::module::Module;
use crate::track_unit::TrackUnit;

/// A row of cells, one per channel, as emitted by the importers
/// before the DAW layer is built. Exists as a transient shape: the
/// importer parses its native pattern format into `Vec<Row>`, then
/// hands it to [`build_timeline_layer`] which redistributes the
/// cells into per-track [`Track::rows`].
pub type Row = Vec<TrackUnit>;

/// A pattern as the importers see it: a vector of [`Row`]s.
/// Transient, never stored on [`Module`].
pub type Pattern = Vec<Row>;

// =====================================================================
// 1. TimelineMap walker
// =====================================================================
//
// Hard upper bound on the number of rows the walker will visit
// before giving up. A 10-minute song at speed 6 / BPM 125 plays
// roughly 12 500 rows, so this cap (≈ a *month* of music) is only
// reached on a pathological or non-terminating module.
const MAX_ROWS: usize = 1 << 20;

/// Build the [`TimelineMap`] for every sub-song defined by
/// `pattern_order`, using `pattern` as the cell source. `module` is
/// read only for its `default_tempo` / `default_bpm` /
/// `profile.quirks` (the navigation effects don't depend on the DAW
/// layer).
pub fn build_timeline_map(
    module: &Module,
    pattern_order: &[Vec<usize>],
    pattern: &[Pattern],
) -> TimelineMap {
    let mut entries: Vec<TimelineEntry> = Vec::new();
    for song in 0..pattern_order.len() {
        walk_one_song(module, pattern_order, pattern, song, &mut entries);
    }
    TimelineMap { entries }
}

fn walk_one_song(
    module: &Module,
    pattern_order: &[Vec<usize>],
    patterns: &[Pattern],
    song: usize,
    out: &mut Vec<TimelineEntry>,
) {
    let order = &pattern_order[song];
    if order.is_empty() || patterns.is_empty() {
        return;
    }
    let song_u16 = song.min(u16::MAX as usize) as u16;

    let mut speed: usize = module.default_tempo.max(1);
    let mut bpm: u32 = module.default_bpm.max(1) as u32;
    let mut order_idx: usize = 0;
    let mut row_idx: usize = 0;
    let mut loop_anchor: usize = 0;
    let mut loop_iters_left: Option<usize> = None;
    let mut loop_iter_count: u16 = 0;
    let mut tick_acc: u32 = 0;
    let mut visited: BTreeSet<(usize, usize, Option<usize>)> = BTreeSet::new();
    let mut rows_processed: usize = 0;

    loop {
        if order_idx >= order.len() || rows_processed >= MAX_ROWS {
            break;
        }
        let pat_idx = order[order_idx];
        if pat_idx >= patterns.len() {
            order_idx += 1;
            row_idx = 0;
            loop_iters_left = None;
            loop_iter_count = 0;
            continue;
        }
        let pattern = &patterns[pat_idx];
        if pattern.is_empty() || row_idx >= pattern.len() {
            order_idx += 1;
            row_idx = 0;
            loop_iters_left = None;
            loop_iter_count = 0;
            continue;
        }

        // Natural loop detection: a (order, row, loop-state) revisit
        // means a future iteration would re-trace the past.
        let key = (order_idx, row_idx, loop_iters_left);
        if !visited.insert(key) {
            break;
        }
        rows_processed += 1;

        let row = &pattern[row_idx];
        let mut new_speed: Option<usize> = None;
        let mut new_bpm: Option<usize> = None;
        let mut do_break: Option<usize> = None;
        let mut do_jump: Option<usize> = None;
        let mut loop_param: Option<usize> = None;
        let mut row_repeats: usize = 1;
        let mut song_end_requested = false;

        for unit in row.iter() {
            for ge in unit.global_effects.iter() {
                match ge {
                    GlobalEffect::Speed(n) => {
                        if *n > 0 {
                            new_speed = Some(*n);
                        } else if module.profile.quirks.speed_zero_ends_song {
                            song_end_requested = true;
                        }
                    }
                    GlobalEffect::Bpm(n) if *n > 0 => {
                        new_bpm = Some(*n);
                    }
                    GlobalEffect::PatternBreak(p) => do_break = Some(*p),
                    GlobalEffect::PositionJump(p) => do_jump = Some(*p),
                    GlobalEffect::PatternLoop(v) => loop_param = Some(*v),
                    GlobalEffect::PatternDelay { quantity, .. } => {
                        row_repeats = row_repeats.saturating_add(*quantity);
                    }
                    _ => {}
                }
            }
        }

        if let Some(s) = new_speed {
            speed = s.max(1);
        }
        if let Some(b) = new_bpm {
            bpm = b.max(1) as u32;
        }

        let speed_u8 = speed.min(255) as u8;
        let bpm_u16 = bpm.min(u16::MAX as u32) as u16;
        let row_repeats_u8 = row_repeats.min(255) as u8;
        for repeat in 0..row_repeats_u8 {
            out.push(TimelineEntry {
                song: song_u16,
                order_idx: order_idx as u32,
                pattern_idx: pat_idx as u32,
                row_idx: row_idx as u32,
                loop_iter: loop_iter_count,
                tick: tick_acc + repeat as u32 * speed as u32,
                speed_at_row: speed_u8,
                bpm_at_row: bpm_u16,
            });
        }
        tick_acc = tick_acc.saturating_add(speed as u32 * row_repeats as u32);

        if song_end_requested {
            break;
        }

        // Flow control: jump > break > loop > advance.
        if let Some(pos) = do_jump {
            order_idx = pos;
            row_idx = 0;
            loop_iters_left = None;
            loop_iter_count = 0;
            loop_anchor = 0;
            continue;
        }
        if let Some(target) = do_break {
            order_idx += 1;
            row_idx = target;
            loop_iters_left = None;
            loop_iter_count = 0;
            loop_anchor = 0;
            continue;
        }
        if let Some(v) = loop_param {
            if v == 0 {
                loop_anchor = row_idx;
                row_idx += 1;
            } else {
                match loop_iters_left {
                    None => {
                        loop_iters_left = Some(v - 1);
                        loop_iter_count = loop_iter_count.saturating_add(1);
                        row_idx = loop_anchor;
                    }
                    Some(0) => {
                        loop_iters_left = None;
                        loop_iter_count = 0;
                        row_idx += 1;
                    }
                    Some(n) => {
                        loop_iters_left = Some(n - 1);
                        loop_iter_count = loop_iter_count.saturating_add(1);
                        row_idx = loop_anchor;
                    }
                }
            }
            continue;
        }

        row_idx += 1;
        if row_idx >= pattern.len() {
            order_idx += 1;
            row_idx = 0;
            loop_iters_left = None;
            loop_iter_count = 0;
            loop_anchor = 0;
        }
    }
}

// =====================================================================
// 2. Track/Clip extraction
// =====================================================================

/// A segment of a single-channel row sequence with a coherent
/// instrument. Public so track-native importers can plug into the
/// DAW layer without re-implementing the split logic.
pub struct TrackSegment {
    pub instrument: usize,
    pub rows: Vec<TrackUnit>,
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
/// Honour the historical "restart byte" (Soundtracker MOD,
/// FastTracker XM) by attaching a `GlobalEffect::PositionJump`
/// to the last row that the order will play. The existing
/// position-jump pipeline then handles the end-of-song wrap
/// without any restart-specific field on `Module`.
///
/// Defensive interpretation of `restart_byte`:
///
/// * `0` — no-op. The default wrap-to-order-0 already does this.
/// * `>= order.len()` — no-op. Covers ProTracker's `0x7F` sentinel
///   and any other out-of-range byte.
/// * otherwise — inject. If the last pattern is also referenced
///   earlier in the order, the function clones it and rewires the
///   last slot to the clone so only the final playback fires.
pub fn inject_restart_position_jump(
    order: &mut [usize],
    patterns: &mut Vec<Pattern>,
    restart_byte: usize,
) {
    if order.is_empty() || restart_byte == 0 || restart_byte >= order.len() {
        return;
    }
    let last_order_pos = order.len() - 1;
    let last_pat_idx = order[last_order_pos];
    if last_pat_idx >= patterns.len() {
        return;
    }

    let shared = order.iter().filter(|&&p| p == last_pat_idx).count() > 1;
    let pat_to_modify = if shared {
        let cloned = patterns[last_pat_idx].clone();
        patterns.push(cloned);
        let new_idx = patterns.len() - 1;
        order[last_order_pos] = new_idx;
        new_idx
    } else {
        last_pat_idx
    };

    if let Some(last_row) = patterns[pat_to_modify].last_mut() {
        if let Some(first_cell) = last_row.first_mut() {
            first_cell
                .global_effects
                .push(GlobalEffect::PositionJump(restart_byte));
        }
    }
}

/// Sentinel used as [`TrackSegment::instrument`] (and therefore
/// [`crate::daw::track::Track::instrument`]) for an "effect-only"
/// segment — a stretch of cells that carry meaningful effects
/// (vibrato, portamento, …) but no explicit instrument trigger of
/// their own. These effects are intended to modify a voice that's
/// still playing from a previous pattern, so the player processes
/// the cell's effects without performing an instrument load.
///
/// The player never reads `Track.instrument` at row materialisation
/// time — cells carry their own `cell.instrument`, and effect-only
/// cells have `instrument = None`, so no instrument lookup happens
/// with the sentinel value.
pub const EFFECT_ONLY_INSTRUMENT: usize = usize::MAX;

pub fn split_rows_by_instrument(rows: &[TrackUnit]) -> Vec<TrackSegment> {
    let mut segments: Vec<TrackSegment> = Vec::new();
    let mut current_instrument: Option<usize> = None;
    let mut current_rows: Vec<TrackUnit> = Vec::new();
    let mut current_start_row: u32 = 0;
    // Leading effect-only run: cells before any explicit
    // instrument trigger that carry meaningful effects intended
    // for a voice inherited from the previous pattern.
    let mut leading_rows: Vec<TrackUnit> = Vec::new();
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
                // First explicit instrument trigger — flush any
                // accumulated leading effect-only run.
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
            // Effect-only meaningful cell with no instrument context
            // yet — buffer it. Subsequent empty cells are kept too
            // so the segment row indices align with source rows.
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
        // Pattern had effect-only cells throughout, no explicit
        // instrument anywhere.
        segments.push(TrackSegment {
            instrument: EFFECT_ONLY_INSTRUMENT,
            rows: leading_rows,
            start_row: start,
        });
    }

    segments
}

/// Pattern-grid wrapper: extract column `channel` from `pattern`
/// (padding `Empty` where the row is shorter) then call
/// [`split_rows_by_instrument`].
fn split_channel_by_instrument(pattern: &[Row], channel: usize) -> Vec<TrackSegment> {
    let column: Vec<TrackUnit> = pattern
        .iter()
        .map(|row| row.get(channel).cloned().unwrap_or_default())
        .collect();
    split_rows_by_instrument(&column)
}

/// Returns true iff this cell carries a non-Empty note or any effect —
/// i.e. it's not a pure "blank" continuation cell.
fn is_cell_meaningful(cell: &TrackUnit) -> bool {
    !matches!(cell.note, CellNote::Empty)
        || !cell.effects.is_empty()
        || !cell.global_effects.is_empty()
}

/// Key into the `segments_map` for a given (song, pattern, channel,
/// segment index inside the channel).
#[derive(Eq, PartialEq, Ord, PartialOrd, Debug, Clone, Copy)]
struct SegmentKey {
    song: u16,
    pattern_idx: u32,
    channel: u32,
    segment_idx: u32,
}

/// Output of the per-pattern extraction. Keyed by SegmentKey, the
/// value is a tuple `(track_idx_in_module_tracks, start_row, length)`.
struct ExtractedSegment {
    track_idx: u32,
    start_row: u32,
    length: u32,
}

/// Extract Tracks and Clips from a pattern grid.
///
/// Returns `(tracks, clips)` ready to be assigned to `module.tracks`
/// / `module.clips`.
pub fn extract_tracks_and_clips(
    pattern: &[Pattern],
    timeline_map: &TimelineMap,
) -> (Vec<Track>, Vec<Clip>) {
    let mut tracks: Vec<Track> = Vec::new();
    let mut segments_map: BTreeMap<SegmentKey, ExtractedSegment> = BTreeMap::new();

    // -- Pass 1 : extract one segment per (song, pattern, channel,
    //    instrument-run). Indexed by (song, pattern, channel,
    //    segment_idx) so structural dedup (same pattern referenced
    //    twice in pattern_order) is implicit. --
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
                tracks.push(Track {
                    name,
                    instrument: seg.instrument,
                    rows: seg.rows,
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

    // -- Pass 2 : emit one Clip per (song, order_idx) entry point ×
    //    segment that lives in the corresponding pattern. --

    // Find the first TimelineEntry for each (song, order_idx) with
    // loop_iter == 0. That's the canonical entry row (may be > 0 if a
    // PatternBreak landed mid-pattern).
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
        // Iterate every segment that belongs to this entry's pattern.
        // Iterate in (channel, segment_idx) order.
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
                // segment starts at or after the entry → clip at offset 0.
                // Pull `position_tick` straight from the timeline so a
                // `GlobalEffect::Speed` between the entry and this
                // segment's start row doesn't skew the placement.
                // Use `find_entry_at_order` so a pattern visited at
                // two order positions doesn't alias their position
                // ticks together.
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
                // segment straddles the entry → clip enters mid-track
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
            // else: segment entirely before entry → skipped
        }
    }

    // The returned `clips` Vec is intentionally not sorted here —
    // `SortedClips::from_unsorted` (in `build_timeline_layer`)
    // establishes the canonical invariant.
    (tracks, clips)
}

/// Compute the absolute tick at which a clip stops playing,
/// using the timeline map so per-row speed changes are reflected
/// exactly. The lookup needs `order_idx` (not just `pattern_idx`)
/// because the same pattern can appear at multiple order positions
/// with different per-row ticks.
///
/// Tries `(song, order, seg_end_row)` first; if that row isn't
/// visited (segment runs to the end of the pattern or beyond), it
/// walks backward to the latest visited row in the same order and
/// bumps its tick by that row's `speed_at_row`. `fallback_speed`
/// is used only when no entry of the order matches at all
/// (defensive against unusual modules).
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
    // Walk backwards looking for the latest visited row in this
    // order strictly below `seg_end_row`.
    let mut best: Option<(u32, u32, u32)> = None; // (row, tick, speed)
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

// =====================================================================
// 3. Content dedup (passe 3)
// =====================================================================

/// Hash a `Track`'s identity for content dedup. Two tracks with the
/// same `(instrument, rows)` produce the same hash.
fn hash_track(track: &Track) -> u64 {
    // Use a deterministic FNV-1a 64-bit hash over a structural
    // serialisation of (instrument, rows). FNV is no_std-friendly
    // (no external crate, no RNG, no allocator state) and gives
    // collision rates well below what we need for fusing identical
    // tracker patterns.
    let mut h: u64 = 0xcbf29ce484222325;
    fnv_mix_usize(&mut h, track.instrument);
    fnv_mix_usize(&mut h, track.rows.len());
    for unit in &track.rows {
        hash_track_unit(&mut h, unit);
    }
    h
}

#[inline]
fn fnv_mix_byte(h: &mut u64, b: u8) {
    *h ^= b as u64;
    *h = h.wrapping_mul(0x100000001b3);
}

#[inline]
fn fnv_mix_u32(h: &mut u64, v: u32) {
    for b in v.to_le_bytes() {
        fnv_mix_byte(h, b);
    }
}

#[inline]
fn fnv_mix_u64(h: &mut u64, v: u64) {
    for b in v.to_le_bytes() {
        fnv_mix_byte(h, b);
    }
}

#[inline]
fn fnv_mix_usize(h: &mut u64, v: usize) {
    fnv_mix_u64(h, v as u64);
}

fn hash_track_unit(h: &mut u64, unit: &TrackUnit) {
    // Discriminate by `note`, then velocity raw, then instrument,
    // then effects (debug-format hashed byte-by-byte as a tag-only
    // discrimination). A simple debug-string hash is sufficient:
    // deterministic across runs, process-independent (does not
    // depend on heap addresses), and `TrackEffect` / `GlobalEffect`
    // Debug output is stable.
    use core::fmt::Write;

    // note
    match unit.note {
        CellNote::Empty => fnv_mix_byte(h, 0),
        CellNote::Play(pitch) => {
            fnv_mix_byte(h, 1);
            // Pitch is a Q8_8 newtype around an i16; we hash its raw
            // bytes from the Debug output to remain stable across
            // crate versions without coupling to internals.
            let mut s = String::new();
            let _ = write!(&mut s, "{:?}", pitch);
            for b in s.bytes() {
                fnv_mix_byte(h, b);
            }
        }
        CellNote::KeyOff => fnv_mix_byte(h, 2),
        CellNote::NoteCut => fnv_mix_byte(h, 3),
        CellNote::NoteFade => fnv_mix_byte(h, 4),
    }

    // velocity (Q1.15 raw)
    let mut s = String::new();
    let _ = write!(&mut s, "{:?}", unit.velocity);
    for b in s.bytes() {
        fnv_mix_byte(h, b);
    }

    // instrument
    match unit.instrument {
        None => fnv_mix_byte(h, 0xFE),
        Some(idx) => {
            fnv_mix_byte(h, 0xFF);
            fnv_mix_usize(h, idx);
        }
    }

    // effects
    fnv_mix_u32(h, unit.effects.len() as u32);
    for fx in &unit.effects {
        let mut s = String::new();
        let _ = write!(&mut s, "{:?}", fx);
        for b in s.bytes() {
            fnv_mix_byte(h, b);
        }
    }

    fnv_mix_u32(h, unit.global_effects.len() as u32);
    for fx in &unit.global_effects {
        let mut s = String::new();
        let _ = write!(&mut s, "{:?}", fx);
        for b in s.bytes() {
            fnv_mix_byte(h, b);
        }
    }
}

/// Deeper equality check used to confirm a hash hit is genuine.
fn tracks_equal_content(a: &Track, b: &Track) -> bool {
    if a.instrument != b.instrument {
        return false;
    }
    if a.rows.len() != b.rows.len() {
        return false;
    }
    for (ua, ub) in a.rows.iter().zip(b.rows.iter()) {
        if !track_units_equal(ua, ub) {
            return false;
        }
    }
    true
}

fn track_units_equal(a: &TrackUnit, b: &TrackUnit) -> bool {
    use core::fmt::Write;
    if !matches!(
        (a.note, b.note),
        (CellNote::Empty, CellNote::Empty)
            | (CellNote::KeyOff, CellNote::KeyOff)
            | (CellNote::NoteCut, CellNote::NoteCut)
            | (CellNote::NoteFade, CellNote::NoteFade)
    ) {
        match (&a.note, &b.note) {
            (CellNote::Play(pa), CellNote::Play(pb)) => {
                let mut sa = String::new();
                let mut sb = String::new();
                let _ = write!(&mut sa, "{:?}", pa);
                let _ = write!(&mut sb, "{:?}", pb);
                if sa != sb {
                    return false;
                }
            }
            _ => return false,
        }
    }
    if a.instrument != b.instrument {
        return false;
    }
    let mut sa = String::new();
    let mut sb = String::new();
    let _ = write!(&mut sa, "{:?}", a.velocity);
    let _ = write!(&mut sb, "{:?}", b.velocity);
    if sa != sb {
        return false;
    }
    if a.effects.len() != b.effects.len() {
        return false;
    }
    for (ea, eb) in a.effects.iter().zip(b.effects.iter()) {
        let mut sa = String::new();
        let mut sb = String::new();
        let _ = write!(&mut sa, "{:?}", ea);
        let _ = write!(&mut sb, "{:?}", eb);
        if sa != sb {
            return false;
        }
    }
    if a.global_effects.len() != b.global_effects.len() {
        return false;
    }
    for (ea, eb) in a.global_effects.iter().zip(b.global_effects.iter()) {
        let mut sa = String::new();
        let mut sb = String::new();
        let _ = write!(&mut sa, "{:?}", ea);
        let _ = write!(&mut sb, "{:?}", eb);
        if sa != sb {
            return false;
        }
    }
    true
}

/// Fuse Tracks with bit-identical (instrument, rows) into a single
/// representative, rewriting `Clip.track` indices accordingly.
///
/// Cycle-exactness: the player consumes the same Track contents
/// whether or not dedup ran. The fusion does not affect playback.
pub fn dedupe_tracks_by_content(module: &mut Module) {
    if module.tracks.is_empty() {
        return;
    }

    // hash → list of track indices that share it
    let mut buckets: BTreeMap<u64, Vec<u32>> = BTreeMap::new();
    for (idx, track) in module.tracks.iter().enumerate() {
        buckets
            .entry(hash_track(track))
            .or_default()
            .push(idx as u32);
    }

    // For each bucket with ≥ 2 tracks, confirm equality then pick a
    // representative (lowest index) and build a remap of duplicates
    // → representative.
    let mut remap: BTreeMap<u32, u32> = BTreeMap::new();
    let mut to_delete: alloc::collections::BTreeSet<u32> = alloc::collections::BTreeSet::new();

    for group in buckets.values() {
        if group.len() < 2 {
            continue;
        }
        // The representative is the first one ; remap others if they
        // are truly equal.
        let representative = group[0];
        for &dup in &group[1..] {
            if tracks_equal_content(
                &module.tracks[representative as usize],
                &module.tracks[dup as usize],
            ) {
                remap.insert(dup, representative);
                to_delete.insert(dup);
            }
        }
    }

    if remap.is_empty() {
        return;
    }

    // Rewrite clip.track to point to the representative.
    // `clip.track` isn't part of the sort key so `modify_all`'s
    // post-mutation resort is effectively a no-op here.
    module.clips.modify_all(|clip| {
        if let Some(&rep) = remap.get(&clip.track) {
            clip.track = rep;
        }
    });

    // Compute index shift after removing duplicates. Walk to_delete
    // in ascending order and produce a final-index map for survivors.
    let mut final_index: Vec<u32> = (0..module.tracks.len() as u32).collect();
    let mut shift: u32 = 0;
    let to_delete_vec: Vec<u32> = to_delete.iter().copied().collect();
    let mut del_iter = to_delete_vec.iter().peekable();
    for i in 0..final_index.len() as u32 {
        while let Some(&&next_del) = del_iter.peek() {
            if next_del < i {
                shift += 1;
                del_iter.next();
            } else {
                break;
            }
        }
        if to_delete.contains(&i) {
            // marker for "deleted" — its row in final_index is
            // unused, the clip remap step above already redirected
            // anyone pointing to it.
            final_index[i as usize] = u32::MAX;
        } else {
            final_index[i as usize] = i - shift;
        }
    }
    // Translate clip.track via final_index. The dup→representative
    // remap has already redirected clips; we must now also shift the
    // surviving representative's index down by the number of deletes
    // that preceded it. `modify_all` resorts at the end — harmless
    // because `clip.track` isn't a sort key.
    let final_index_snapshot = final_index.clone();
    module.clips.modify_all(|clip| {
        let cur = clip.track as usize;
        if cur < final_index_snapshot.len() && final_index_snapshot[cur] != u32::MAX {
            clip.track = final_index_snapshot[cur];
        }
    });

    // Remove the deleted tracks in descending order so indices stay
    // valid during the loop.
    for &idx in to_delete_vec.iter().rev() {
        module.tracks.remove(idx as usize);
    }
}

// =====================================================================
// Public pipeline entrypoint
// =====================================================================

/// Build (or rebuild) the DAW timeline layer of `module` from its
/// (synthetic) `pattern_order` + `pattern` inputs. Idempotent.
///
/// The Module struct itself does not carry a pattern grid;
/// importers build it locally and pass it here. `automation` is
/// left untouched — populated only via the [`crate::edit`] API.
///
/// The 4th pass
/// ([`crate::daw::euclidean::auto_convert_strict_euclidean_tracks`])
/// runs after dedup: any Track whose cells form a strict Bjorklund
/// pattern is rewritten lossless to an `InstrumentType::Euclidean`
/// wrapper.
pub fn build_timeline_layer(
    module: &mut Module,
    pattern_order: &[Vec<usize>],
    pattern: &[Pattern],
) {
    module.timeline_map = build_timeline_map(module, pattern_order, pattern);
    let (tracks, clips) = extract_tracks_and_clips(pattern, &module.timeline_map);
    module.tracks = tracks;
    module.clips = crate::daw::sorted_clips::SortedClips::from_unsorted(clips);
    dedupe_tracks_by_content(module);
    crate::daw::euclidean::auto_convert_strict_euclidean_tracks(module);
}

#[cfg(test)]
mod restart_tests {
    use super::*;
    use alloc::vec;

    fn empty_pattern(channels: usize, rows: usize) -> Pattern {
        (0..rows)
            .map(|_| (0..channels).map(|_| TrackUnit::default()).collect())
            .collect()
    }

    fn pattern_has_jump(pattern: &Pattern) -> Option<usize> {
        for row in pattern.iter().rev() {
            for cell in row {
                for ge in &cell.global_effects {
                    if let GlobalEffect::PositionJump(p) = ge {
                        return Some(*p);
                    }
                }
            }
        }
        None
    }

    #[test]
    fn restart_zero_is_noop() {
        let mut order = vec![0, 1, 2];
        let mut patterns = vec![empty_pattern(1, 4); 3];
        inject_restart_position_jump(&mut order, &mut patterns, 0);
        assert_eq!(order, vec![0, 1, 2]);
        for p in &patterns {
            assert!(pattern_has_jump(p).is_none());
        }
    }

    #[test]
    fn protracker_sentinel_is_noop() {
        let mut order = vec![0, 1, 2];
        let mut patterns = vec![empty_pattern(1, 4); 3];
        inject_restart_position_jump(&mut order, &mut patterns, 0x7F);
        assert!(patterns.iter().all(|p| pattern_has_jump(p).is_none()));
    }

    #[test]
    fn unshared_last_pattern_is_mutated_in_place() {
        // Order [0, 1, 2]: pattern 2 is only used once, so we can
        // mutate it directly without cloning.
        let mut order = vec![0, 1, 2];
        let mut patterns = vec![empty_pattern(1, 4); 3];
        inject_restart_position_jump(&mut order, &mut patterns, 1);
        assert_eq!(order, vec![0, 1, 2]);
        assert_eq!(patterns.len(), 3);
        assert_eq!(pattern_has_jump(&patterns[2]), Some(1));
        assert!(pattern_has_jump(&patterns[0]).is_none());
        assert!(pattern_has_jump(&patterns[1]).is_none());
    }

    #[test]
    fn shared_last_pattern_is_cloned() {
        // Pattern 0 is used at order positions 0 AND 2 (last).
        // Mutating pattern 0 would fire the jump at position 0 too,
        // which is wrong — the function must clone it.
        let mut order = vec![0, 1, 0];
        let mut patterns = vec![empty_pattern(1, 4); 2];
        inject_restart_position_jump(&mut order, &mut patterns, 1);
        assert_eq!(order.len(), 3);
        assert_ne!(order[2], 0, "last slot should point at the clone");
        assert_eq!(patterns.len(), 3, "a clone was appended");
        // The clone (at order[2]) carries the jump.
        let clone_idx = order[2];
        assert_eq!(pattern_has_jump(&patterns[clone_idx]), Some(1));
        // The original pattern 0 (still at order position 0) stays
        // clean.
        assert!(pattern_has_jump(&patterns[0]).is_none());
    }
}
