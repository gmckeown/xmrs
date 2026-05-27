//! TimelineMap walker: linearises `pattern_order` to absolute ticks
//! by following navigation effects (jumps, breaks, loops, delays)
//! and tempo/BPM changes. Drives the first pass of
//! [`super::build_timeline_layer`].

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::core::daw::timeline::{TimelineEntry, TimelineMap};
use crate::core::effect::{NavigationEffect, SongLevelEffect};
use crate::core::module::Module;

use super::Pattern;

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
///
/// Returns the populated map plus, for song 0 only, the absolute
/// tick of the row the walker re-entered when it terminated on a
/// cycle (typically a `Bxx` / `Dxx` jumping back into already-walked
/// territory). The caller is expected to forward that tick to
/// [`crate::core::module::Module::song_loop_to`] so the sequencer
/// wraps to that row at end-of-timeline. `None` means the walker
/// fell off the end of the order table without ever re-entering a
/// visited row — there's no in-song loop point to honour.
pub fn build_timeline_map(
    module: &Module,
    pattern_order: &[Vec<usize>],
    pattern: &[Pattern],
) -> (TimelineMap, Option<u32>) {
    let mut entries: Vec<TimelineEntry> = Vec::new();
    let mut wrap_tick_song0: Option<u32> = None;
    for song in 0..pattern_order.len() {
        let wrap = walk_one_song(module, pattern_order, pattern, song, &mut entries);
        if song == 0 {
            wrap_tick_song0 = wrap;
        }
    }
    (TimelineMap { entries }, wrap_tick_song0)
}

/// Walk one sub-song. Returns `Some(tick)` when the walker
/// terminated on a cycle — that tick is the row the walker would
/// have re-played if it had been allowed to keep going, so it is
/// the natural end-of-timeline wrap target for that sub-song.
/// Returns `None` when the walker fell off the end of the order
/// table (no in-pattern jump pointed back into visited rows).
fn walk_one_song(
    module: &Module,
    pattern_order: &[Vec<usize>],
    patterns: &[Pattern],
    song: usize,
    out: &mut Vec<TimelineEntry>,
) -> Option<u32> {
    let order = &pattern_order[song];
    if order.is_empty() || patterns.is_empty() {
        return None;
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
    // `visited` doubles as a cycle-detector AND a (key → tick)
    // lookup: when a jump (`Bxx` / `Dxx`) sends us back into a row
    // that was already walked, the value stored against that key is
    // the absolute tick the row was emitted at on its first visit.
    // That tick is what the player should wrap to at end-of-timeline.
    let mut visited: BTreeMap<(usize, usize, Option<usize>), u32> = BTreeMap::new();
    let mut rows_processed: usize = 0;

    loop {
        if order_idx >= order.len() || rows_processed >= MAX_ROWS {
            return None;
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

        let key = (order_idx, row_idx, loop_iters_left);
        if let Some(&first_tick) = visited.get(&key) {
            // Re-entered an already-walked row — that's the natural
            // wrap target for the sub-song.
            return Some(first_tick);
        }
        visited.insert(key, tick_acc);
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
            for sle in unit.song_level.iter() {
                match sle {
                    SongLevelEffect::Speed(n) => {
                        if *n > 0 {
                            new_speed = Some(*n);
                        } else if module.quirks.speed_zero_ends_song {
                            song_end_requested = true;
                        }
                    }
                    SongLevelEffect::Bpm(n) if *n > 0 => {
                        new_bpm = Some(*n);
                    }
                    _ => {}
                }
            }
            for ne in unit.navigation.iter() {
                match ne {
                    NavigationEffect::PatternBreak(p) => do_break = Some(*p),
                    NavigationEffect::PositionJump(p) => do_jump = Some(*p),
                    NavigationEffect::PatternLoop(v) => loop_param = Some(*v),
                    NavigationEffect::PatternDelay { quantity, .. } => {
                        row_repeats = row_repeats.saturating_add(*quantity);
                    }
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
            // `F00` (or equivalent) cut the song. No wrap.
            return None;
        }

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

#[cfg(test)]
mod walker_wrap_tests {
    use super::*;
    use crate::core::effect::NavigationEffect;
    use crate::core::module::Module;
    use crate::tracker::import::unit::TrackImportUnit;
    use alloc::vec;

    fn empty_row() -> Vec<TrackImportUnit> {
        vec![TrackImportUnit::default()]
    }

    /// A pattern of `rows` empty rows.
    fn empty_pat(rows: usize) -> Pattern {
        (0..rows).map(|_| empty_row()).collect()
    }

    /// `walk_one_song` returns the tick of the row that an explicit
    /// `Bxx` (position jump) re-enters, so the caller can plumb it
    /// into `Module::song_loop_to`.
    #[test]
    fn position_jump_back_records_first_visit_tick_as_wrap() {
        // Two single-channel patterns, 4 rows each. Order = [0, 1].
        // Last row of pattern 1 has a `B01` jumping to order 1
        // (= the second order, which is pattern 1 again). The
        // intended loop is therefore "order 0 once, then order 1
        // forever".
        let mut pat_a = empty_pat(4);
        let mut pat_b = empty_pat(4);
        // Bxx on the last row of pattern 1, channel 0.
        let mut bxx_unit = TrackImportUnit::default();
        bxx_unit.navigation.push(NavigationEffect::PositionJump(1));
        pat_b[3][0] = bxx_unit;

        let module = Module::default();
        let order = vec![0usize, 1];
        let patterns = vec![pat_a.clone(), pat_b.clone()];
        let mut out = Vec::new();
        let wrap = walk_one_song(&module, &[order], &patterns, 0, &mut out);

        // 8 rows × 6 ticks/row (default tempo) = 48 ticks total.
        assert_eq!(out.len(), 8, "every order-0 + order-1 row plays once");
        // Order 1 row 0 lives at row index 4 (after the 4 rows of
        // order 0). Its tick is 4 × 6 = 24.
        let expected_wrap_tick = 4 * module.default_tempo as u32;
        assert_eq!(
            wrap,
            Some(expected_wrap_tick),
            "B01 on last row should record order-1-row-0 tick as wrap target",
        );
    }

    /// Without any in-pattern jump, the walker falls off the end of
    /// the order table and reports `None` — no in-song loop point.
    /// The caller is free to fall back to the restart-byte tick.
    #[test]
    fn linear_song_without_jumps_reports_no_wrap() {
        let module = Module::default();
        let order = vec![0usize, 1];
        let patterns = vec![empty_pat(4), empty_pat(4)];
        let mut out = Vec::new();
        let wrap = walk_one_song(&module, &[order], &patterns, 0, &mut out);
        assert_eq!(out.len(), 8);
        assert_eq!(wrap, None);
    }
}
