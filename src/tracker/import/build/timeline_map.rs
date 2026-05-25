//! TimelineMap walker: linearises `pattern_order` to absolute ticks
//! by following navigation effects (jumps, breaks, loops, delays)
//! and tempo/BPM changes. Drives the first pass of
//! [`super::build_timeline_layer`].

use alloc::collections::BTreeSet;
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
            break;
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
