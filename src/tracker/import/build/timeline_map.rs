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
    // Pattern-loop state is **per channel** (E6x / SBx counters live on
    // the channel struct in every reference replayer: FT2
    // `patternLoopStartRow/Counter`, IT `patloop_row/cd_patloop`, PT
    // `n_pattpos/n_loopcount`). The shared row pointer is jumped by
    // whichever channel asserts last (ascending order). With a single
    // looping channel — the overwhelming common case — only one slot is
    // ever non-default, so the emitted sequence is byte-identical to the
    // previous global model.
    // Channel count comes from the pattern grid, NOT
    // `module.get_num_channels()` — the walker runs before clips exist,
    // so the module would report 0 here (it derives the count from
    // clips). Channel width is uniform within a pattern; take the max
    // first-row width across patterns.
    let nch = patterns
        .iter()
        .filter_map(|p| p.first())
        .map(|row| row.len())
        .max()
        .unwrap_or(1)
        .max(1);
    let mut loop_anchor: alloc::vec::Vec<usize> = alloc::vec![0; nch];
    let mut loop_counter: alloc::vec::Vec<Option<usize>> = alloc::vec![None; nch];
    let mut loop_iter_count: u16 = 0;
    // FT2 `e60_leaks_to_next_pattern`: an E6x jump leaves its loop-start
    // row in `pBreakPos`, which FT2 (unlike PT) does NOT clear in the
    // loop branch — so when the pattern ends *naturally* the next one
    // starts at that row (clamped to its length). `None` = no leak
    // pending. Only ever set when the quirk is on, so non-FT2 is
    // untouched. ft2_replayer.c:`patternLoop` + row-advance `:2311`/`:2344`.
    let e60_leaks = module.quirks.e60_leaks_to_next_pattern;
    let mut ft2_leak_row: Option<usize> = None;
    let mut tick_acc: u32 = 0;
    // `visited` doubles as a cycle-detector AND a (key → tick)
    // lookup: when a jump (`Bxx` / `Dxx`) sends us back into a row
    // that was already walked, the value stored against that key is
    // the absolute tick the row was emitted at on its first visit.
    // That tick is what the player should wrap to at end-of-timeline.
    // The per-channel loop counters are part of the key so two visits
    // to the same row under different loop state aren't a false cycle.
    let mut visited: BTreeMap<(usize, usize, alloc::vec::Vec<Option<usize>>), u32> =
        BTreeMap::new();
    let mut rows_processed: usize = 0;

    loop {
        if order_idx >= order.len() || rows_processed >= MAX_ROWS {
            return None;
        }
        let pat_idx = order[order_idx];
        if pat_idx >= patterns.len() {
            order_idx += 1;
            row_idx = 0;
            loop_counter.fill(None);
            loop_iter_count = 0;
            ft2_leak_row = None;
            continue;
        }
        let pattern = &patterns[pat_idx];
        if pattern.is_empty() || row_idx >= pattern.len() {
            order_idx += 1;
            row_idx = 0;
            loop_counter.fill(None);
            loop_iter_count = 0;
            ft2_leak_row = None;
            continue;
        }

        let key = (order_idx, row_idx, loop_counter.clone());
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
        let mut loop_params: alloc::vec::Vec<Option<usize>> = alloc::vec![None; nch];
        let mut any_loop_param = false;
        let mut row_repeats: usize = 1;
        let mut song_end_requested = false;

        for (ch, unit) in row.iter().enumerate() {
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
                    NavigationEffect::PatternLoop(v) => {
                        if ch < nch {
                            loop_params[ch] = Some(*v);
                            any_loop_param = true;
                        }
                    }
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

        // PositionJump (Bxx) — highest precedence, clears all loop state.
        if let Some(pos) = do_jump {
            order_idx = pos;
            row_idx = 0;
            loop_counter.fill(None);
            loop_anchor.fill(0);
            loop_iter_count = 0;
            ft2_leak_row = None;
            continue;
        }

        // Per-channel pattern loops (E6x / SBx). Each channel keeps its
        // own start row + counter; the shared row pointer is redirected
        // by whichever channel asserts a jump last (ascending order =
        // highest channel wins, matching the reference replayers where
        // the later channel overwrites the shared break position). The
        // None / Some(0) / Some(n) counter transitions are identical to
        // the former global model, so a single looping channel emits a
        // byte-identical sequence.
        let mut loop_jump: Option<usize> = None;
        let mut any_expired = false;
        if any_loop_param {
            for ch in 0..nch {
                let Some(v) = loop_params[ch] else { continue };
                if v == 0 {
                    loop_anchor[ch] = row_idx; // set loop start
                    continue;
                }
                match loop_counter[ch] {
                    None => {
                        loop_counter[ch] = Some(v - 1);
                        loop_jump = Some(loop_anchor[ch]);
                    }
                    Some(0) => {
                        // Counter expired: stop looping on this channel.
                        // S3M/IT re-anchor past the loop so a later loop
                        // in the same pattern resumes after it
                        // (`pattern_loop_resumes`); FT2/PT leave the
                        // origin untouched.
                        loop_counter[ch] = None;
                        any_expired = true;
                        if module.quirks.pattern_loop_resumes {
                            loop_anchor[ch] = row_idx + 1;
                        }
                    }
                    Some(n) => {
                        loop_counter[ch] = Some(n - 1);
                        loop_jump = Some(loop_anchor[ch]);
                    }
                }
            }
        }

        // PatternBreak (Cxx / Dxx). IT suppresses it while any channel's
        // loop is still iterating (`active_loop_suppresses_break`);
        // FT2/PT/S3M let the break win.
        if let Some(target) = do_break {
            let any_active = loop_counter.iter().any(|c| c.is_some());
            let suppressed = module.quirks.active_loop_suppresses_break && any_active;
            if !suppressed {
                // PT clears the shared break position in its loop branch,
                // so a same-row loop-jump + break lands at row 0 rather
                // than the break target; FT2 keeps the target.
                let landing = if module.quirks.pattern_loop_break_resets_row && loop_jump.is_some()
                {
                    0
                } else {
                    target
                };
                order_idx += 1;
                row_idx = landing;
                loop_counter.fill(None);
                loop_anchor.fill(0);
                loop_iter_count = 0;
                ft2_leak_row = None;
                continue;
            }
        }

        if let Some(anchor) = loop_jump {
            loop_iter_count = loop_iter_count.saturating_add(1);
            row_idx = anchor;
            // FT2: the jump leaves its start row in `pBreakPos` and
            // never clears it in the loop branch (so a later natural
            // pattern-end leaks it). No-op when the quirk is off.
            if e60_leaks {
                ft2_leak_row = Some(anchor);
            }
            continue;
        }
        if any_expired {
            // Falling through past a completed loop → back to first-pass
            // tagging for the post-loop rows.
            loop_iter_count = 0;
        }

        row_idx += 1;
        if row_idx >= pattern.len() {
            order_idx += 1;
            // FT2 `e60_leaks_to_next_pattern`: a natural pattern-end
            // consumes the leaked `pBreakPos`, so the next order starts
            // at that row instead of 0 — clamped to the next pattern's
            // length (FT2's `row >= currNumRows → 0` guard). One-shot.
            let leaked = ft2_leak_row.take();
            row_idx = match leaked {
                Some(r) if e60_leaks => {
                    let next_len = order
                        .get(order_idx)
                        .and_then(|&pi| patterns.get(pi))
                        .map(|p| p.len())
                        .unwrap_or(0);
                    if r < next_len {
                        r
                    } else {
                        0
                    }
                }
                _ => 0,
            };
            loop_counter.fill(None);
            loop_anchor.fill(0);
            loop_iter_count = 0;
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
        let pat_a = empty_pat(4);
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

    /// An `nch`-channel pattern of `rows` empty rows.
    fn pat_nch(rows: usize, nch: usize) -> Pattern {
        (0..rows)
            .map(|_| (0..nch).map(|_| TrackImportUnit::default()).collect())
            .collect()
    }

    /// A pattern loop whose `E60`/`E6x` live on a non-zero channel
    /// still expands. Guards the `nch` derivation: the walker must
    /// size its per-channel state from the pattern width, not from
    /// `module.get_num_channels()` (which is 0 before clips exist).
    #[test]
    fn pattern_loop_on_nonzero_channel_still_repeats() {
        let mut pat = pat_nch(6, 4);
        pat[1][2].navigation.push(NavigationEffect::PatternLoop(0)); // E60 @ row 1
        pat[4][2].navigation.push(NavigationEffect::PatternLoop(1)); // E61 @ row 4
        let module = Module::default();
        let mut out = Vec::new();
        walk_one_song(&module, &[vec![0usize]], &[pat], 0, &mut out);
        let repeats: Vec<u32> = out
            .iter()
            .filter(|e| e.loop_iter == 1)
            .map(|e| e.row_idx)
            .collect();
        assert_eq!(
            repeats,
            vec![1, 2, 3, 4],
            "loop body rows 1..=4 replay once"
        );
    }

    /// IT suppresses a same-row `PatternBreak` while a loop is still
    /// iterating; FT2/PT/S3M let the break win. Two channels: ch0
    /// carries the loop, ch1 a break on the very row that closes it.
    #[test]
    fn it_active_loop_suppresses_same_row_break() {
        let mut pat0 = pat_nch(4, 2);
        pat0[0][0].navigation.push(NavigationEffect::PatternLoop(0)); // E60
        pat0[2][0].navigation.push(NavigationEffect::PatternLoop(1)); // E61
        pat0[2][1]
            .navigation
            .push(NavigationEffect::PatternBreak(0)); // Cxx, same row
        let patterns = vec![pat0, empty_pat(4)];
        let order = vec![0usize, 1];

        // FT2/PT/S3M: break wins → the loop never runs.
        let plain = Module::default();
        let mut out_plain = Vec::new();
        walk_one_song(
            &plain,
            core::slice::from_ref(&order),
            &patterns,
            0,
            &mut out_plain,
        );
        assert!(
            !out_plain.iter().any(|e| e.loop_iter == 1),
            "without the quirk the same-row break preempts the loop",
        );

        // IT: break suppressed → the body replays once before it fires.
        let mut it = Module::default();
        it.quirks.active_loop_suppresses_break = true;
        let mut out_it = Vec::new();
        walk_one_song(&it, &[order], &patterns, 0, &mut out_it);
        assert!(
            out_it.iter().any(|e| e.loop_iter == 1),
            "IT suppresses the break so the loop runs once",
        );
    }

    /// FT2 `e60_leaks_to_next_pattern`: after a loop completes, the
    /// leaked `pBreakPos` (the loop-start row) makes the NEXT pattern
    /// start there instead of row 0. Off → starts at 0.
    #[test]
    fn ft2_e60_leaks_loop_start_row_to_next_pattern() {
        // pat0: E60 @ row 2 (anchor), E61 @ row 4 (loop rows 2..4 once),
        // trailing row 5, then natural end. Leaked row = 2.
        let mut pat0 = empty_pat(6);
        pat0[2][0].navigation.push(NavigationEffect::PatternLoop(0)); // E60
        pat0[4][0].navigation.push(NavigationEffect::PatternLoop(1)); // E61
        let patterns = vec![pat0, empty_pat(6)];
        let order = vec![0usize, 1];

        let first_order1_row = |m: &Module| -> u32 {
            let mut out = Vec::new();
            walk_one_song(m, core::slice::from_ref(&order), &patterns, 0, &mut out);
            out.iter()
                .find(|e| e.order_idx == 1)
                .map(|e| e.row_idx)
                .expect("order 1 is reached")
        };

        // Off (default): next pattern starts at row 0.
        assert_eq!(first_order1_row(&Module::default()), 0);

        // FT2: next pattern starts at the leaked loop-start row 2.
        let mut ft2 = Module::default();
        ft2.quirks.e60_leaks_to_next_pattern = true;
        assert_eq!(first_order1_row(&ft2), 2);
    }

    /// A same-row loop-jump + `PatternBreak`: PT lands the next pattern
    /// at row 0 (it clears the shared break position), FT2 keeps the
    /// break's target row.
    #[test]
    fn pt_loop_plus_break_lands_at_row_zero() {
        // pat0 row 2: ch0 E61 (jumps to anchor 0), ch1 break → row 3.
        let mut pat0 = pat_nch(4, 2);
        pat0[0][0].navigation.push(NavigationEffect::PatternLoop(0)); // E60
        pat0[2][0].navigation.push(NavigationEffect::PatternLoop(1)); // E61 → jump
        pat0[2][1]
            .navigation
            .push(NavigationEffect::PatternBreak(3)); // Dxx → row 3
        let patterns = vec![pat0, empty_pat(6)];
        let order = vec![0usize, 1];

        let first_order1_row = |m: &Module| -> u32 {
            let mut out = Vec::new();
            walk_one_song(m, core::slice::from_ref(&order), &patterns, 0, &mut out);
            out.iter()
                .find(|e| e.order_idx == 1)
                .map(|e| e.row_idx)
                .expect("order 1 is reached")
        };

        // FT2 (default): the break target row 3 survives.
        assert_eq!(first_order1_row(&Module::default()), 3);

        // PT: the loop branch clears the break position → row 0.
        let mut pt = Module::default();
        pt.quirks.pattern_loop_break_resets_row = true;
        assert_eq!(first_order1_row(&pt), 0);
    }

    /// The three tracker dialects whose pattern-loop semantics diverge.
    #[derive(Clone, Copy)]
    enum RefDialect {
        Pt,
        Ft2,
        It,
    }

    const REF_MAXCH: usize = 8;

    /// Read `(loop_effect, break_target)` from one crafted cell.
    /// `loop_effect`: `None` = no `E6x`; `Some(0)` = `E60` (set loop start);
    /// `Some(x>0)` = `E6x` loop count.
    fn ref_cell_fx(cell: &TrackImportUnit) -> (Option<usize>, Option<usize>) {
        let (mut e6, mut brk) = (None, None);
        for nav in &cell.navigation {
            match nav {
                NavigationEffect::PatternLoop(v) => e6 = Some(*v),
                NavigationEffect::PatternBreak(t) => brk = Some(*t),
                _ => {}
            }
        }
        (e6, brk)
    }

    /// Independent reference walker — a direct state-machine implementation
    /// of each dialect's documented `E6x` pattern-loop / pattern-break /
    /// row-advance behaviour, deliberately kept SEPARATE from the
    /// production `walk_one_song` so the two cross-check each other. The
    /// behaviours modelled (and only the behaviours, not anyone's code) are
    /// the well-known divergences: PT clears the break on a loop jump and
    /// resumes the next order at row 0; FT2 leaks the loop-start row into
    /// the next order; IT suppresses a same-row break while a loop counter
    /// is still active. Returns the visited `(order_idx, row_idx)` sequence.
    fn reference_walk(d: RefDialect, order: &[usize], pats: &[Pattern]) -> Vec<(u32, u32)> {
        let rows = |o: usize| pats[order[o]].len();
        let cols = |o: usize, r: usize| pats[order[o]][r].len();
        let fx = |o: usize, r: usize, ch: usize| ref_cell_fx(&pats[order[o]][r][ch]);
        let mut out = Vec::new();
        let (mut sp, mut row, mut guard) = (0usize, 0usize, 0usize);
        match d {
            RefDialect::Pt => {
                let (mut np, mut lc) = ([0usize; REF_MAXCH], [0usize; REF_MAXCH]);
                let (mut tgt, mut jump, mut order_adv) = (0usize, false, false);
                while sp < order.len() && guard < 4000 {
                    guard += 1;
                    let nr = rows(sp);
                    if row >= nr {
                        sp += 1;
                        row = 0;
                        continue;
                    }
                    out.push((sp as u32, row as u32));
                    for ch in 0..cols(sp, row) {
                        let (e6, brk) = fx(sp, row, ch);
                        match e6 {
                            Some(0) => np[ch] = row,
                            Some(x) => {
                                let do_jump = if lc[ch] == 0 {
                                    lc[ch] = x;
                                    true
                                } else {
                                    lc[ch] -= 1;
                                    lc[ch] != 0
                                };
                                if do_jump {
                                    tgt = np[ch];
                                    jump = true;
                                }
                            }
                            None => {}
                        }
                        if let Some(t) = brk {
                            tgt = if t > 63 { 0 } else { t };
                            order_adv = true;
                        }
                    }
                    row += 1;
                    if jump {
                        row = tgt;
                        tgt = 0;
                        jump = false;
                    }
                    if row >= nr || order_adv {
                        row = tgt;
                        tgt = 0;
                        order_adv = false;
                        sp += 1;
                    }
                }
            }
            RefDialect::Ft2 => {
                let (mut plsr, mut plc) = ([0usize; REF_MAXCH], [0usize; REF_MAXCH]);
                let (mut tgt, mut jump, mut order_adv) = (0usize, false, false);
                while sp < order.len() && guard < 4000 {
                    guard += 1;
                    let nr = rows(sp);
                    if row >= nr {
                        sp += 1;
                        row = 0;
                        continue;
                    }
                    out.push((sp as u32, row as u32));
                    for ch in 0..cols(sp, row) {
                        let (e6, brk) = fx(sp, row, ch);
                        match e6 {
                            Some(0) => plsr[ch] = row,
                            Some(x) => {
                                if plc[ch] == 0 {
                                    plc[ch] = x;
                                    tgt = plsr[ch];
                                    jump = true;
                                } else {
                                    plc[ch] -= 1;
                                    if plc[ch] > 0 {
                                        tgt = plsr[ch];
                                        jump = true;
                                    }
                                }
                            }
                            None => {}
                        }
                        if let Some(t) = brk {
                            tgt = t;
                            order_adv = true;
                        }
                    }
                    row += 1;
                    if jump {
                        jump = false;
                        row = tgt;
                    }
                    if row >= nr || order_adv {
                        row = tgt;
                        tgt = 0;
                        order_adv = false;
                        sp += 1;
                        let nn = if sp < order.len() { rows(sp) } else { 64 };
                        if row >= nn {
                            row = 0;
                        }
                    }
                }
            }
            RefDialect::It => {
                let (mut plr, mut cd) = ([0usize; REF_MAXCH], [0usize; REF_MAXCH]);
                while sp < order.len() && guard < 4000 {
                    guard += 1;
                    let nr = rows(sp);
                    if row >= nr {
                        sp += 1;
                        row = 0;
                        continue;
                    }
                    out.push((sp as u32, row as u32));
                    let (mut loop_to, mut brk) = (None, None);
                    for ch in 0..cols(sp, row) {
                        if let Some(x) = fx(sp, row, ch).0 {
                            if x == 0 {
                                plr[ch] = row;
                            } else if cd[ch] == 0 {
                                cd[ch] = x;
                                loop_to = Some(plr[ch]);
                            } else {
                                cd[ch] -= 1;
                                if cd[ch] == 0 {
                                    plr[ch] = row + 1;
                                } else {
                                    loop_to = Some(plr[ch]);
                                }
                            }
                        }
                    }
                    let active = (0..cols(sp, row)).any(|ch| cd[ch] != 0);
                    for ch in 0..cols(sp, row) {
                        if let Some(t) = fx(sp, row, ch).1 {
                            if !active {
                                brk = Some(t);
                            }
                        }
                    }
                    if let Some(l) = loop_to {
                        row = l;
                    } else if let Some(b) = brk {
                        sp += 1;
                        row = b;
                    } else {
                        row += 1;
                        if row >= nr {
                            sp += 1;
                            row = 0;
                        }
                    }
                }
            }
        }
        out
    }

    /// Tier-2 cross-check: the production walker's `(order_idx, row_idx)`
    /// visit sequence matches an **independent in-crate reference walker**
    /// (`reference_walk`, a direct transcription of each dialect's
    /// documented `E6x` loop/break semantics) on three crafted cases.
    /// Exercises the single loop, the FT2 E60-leak, the PT/FT2 loop+break
    /// clear, and the IT break-suppression — all three dialect divergences.
    /// Both implementations are checked against the same documented
    /// sequence, so neither can silently drift.
    ///
    /// Gated on the importer features: it uses `tracker::profiles`, which is
    /// itself `#[cfg]`-gated on them, so the test only compiles where the
    /// quirk presets it exercises actually exist.
    #[cfg(any(
        feature = "import_mod",
        feature = "import_xm",
        feature = "import_s3m",
        feature = "import_it",
        feature = "import_sid",
        feature = "import_dw",
    ))]
    #[test]
    fn walker_matches_reference_replayer_oracle() {
        // Imported here rather than at module scope: this is the only user, and
        // it is gated on "any importer", so a `--features format`-only test
        // build would flag the import as unused.
        use crate::core::compatibility::PlaybackQuirks;
        use crate::tracker::profiles;
        let seq = |q: &PlaybackQuirks, order: Vec<usize>, pats: Vec<Pattern>| -> Vec<(u32, u32)> {
            let mut m = Module::default();
            m.quirks = *q;
            let mut out = Vec::new();
            walk_one_song(&m, &[order], &pats, 0, &mut out);
            out.iter().map(|e| (e.order_idx, e.row_idx)).collect()
        };
        // Assert that BOTH the independent reference walker and the
        // production walker reproduce `expected` for dialect `d`.
        let check = |d: RefDialect,
                     order: Vec<usize>,
                     pats: Vec<Pattern>,
                     expected: Vec<(u32, u32)>,
                     label: &str| {
            let q = match d {
                RefDialect::Pt => profiles::pt(),
                RefDialect::Ft2 => profiles::ft2(),
                RefDialect::It => profiles::it214(),
            };
            assert_eq!(
                reference_walk(d, &order, &pats),
                expected,
                "{label}: independent reference walker"
            );
            assert_eq!(seq(&q, order, pats), expected, "{label}: production walker");
        };
        let lp = |p: &mut Pattern, r: usize, ch: usize, v: usize| {
            p[r][ch].navigation.push(NavigationEffect::PatternLoop(v))
        };
        let br = |p: &mut Pattern, r: usize, ch: usize, t: usize| {
            p[r][ch].navigation.push(NavigationEffect::PatternBreak(t))
        };
        let row = |o: u32, r: u32| (o, r);

        // Case A — single loop E60@1 / E61@4. All dialects identical.
        let mut a = pat_nch(6, 1);
        lp(&mut a, 1, 0, 0);
        lp(&mut a, 4, 0, 1);
        let exp_a = vec![
            row(0, 0),
            row(0, 1),
            row(0, 2),
            row(0, 3),
            row(0, 4),
            row(0, 1),
            row(0, 2),
            row(0, 3),
            row(0, 4),
            row(0, 5),
        ];
        for d in [RefDialect::Pt, RefDialect::Ft2, RefDialect::It] {
            check(d, vec![0], vec![a.clone()], exp_a.clone(), "case A");
        }

        // Case B — E60@2 / E61@4, trailing row, order [0,1]. FT2 leaks
        // the loop-start row (next order starts at row 2); PT/IT do not.
        let mut b = pat_nch(6, 1);
        lp(&mut b, 2, 0, 0);
        lp(&mut b, 4, 0, 1);
        let head_b = vec![
            row(0, 0),
            row(0, 1),
            row(0, 2),
            row(0, 3),
            row(0, 4),
            row(0, 2),
            row(0, 3),
            row(0, 4),
            row(0, 5),
        ];
        let mut exp_b_noleak = head_b.clone();
        exp_b_noleak.extend([
            row(1, 0),
            row(1, 1),
            row(1, 2),
            row(1, 3),
            row(1, 4),
            row(1, 5),
        ]);
        let mut exp_b_leak = head_b;
        exp_b_leak.extend([row(1, 2), row(1, 3), row(1, 4), row(1, 5)]);
        check(
            RefDialect::Pt,
            vec![0, 1],
            vec![b.clone(), pat_nch(6, 1)],
            exp_b_noleak.clone(),
            "case B PT",
        );
        check(
            RefDialect::It,
            vec![0, 1],
            vec![b.clone(), pat_nch(6, 1)],
            exp_b_noleak,
            "case B IT",
        );
        check(
            RefDialect::Ft2,
            vec![0, 1],
            vec![b, pat_nch(6, 1)],
            exp_b_leak,
            "case B FT2",
        );

        // Case C — loop-jump + same-row break. PT → next order row 0;
        // FT2 → break target row 3; IT → break suppressed, loop runs,
        // then break fires at row 3.
        let mut c = pat_nch(4, 2);
        lp(&mut c, 0, 0, 0);
        lp(&mut c, 2, 0, 1);
        br(&mut c, 2, 1, 3);
        check(
            RefDialect::Pt,
            vec![0, 1],
            vec![c.clone(), pat_nch(6, 2)],
            vec![
                row(0, 0),
                row(0, 1),
                row(0, 2),
                row(1, 0),
                row(1, 1),
                row(1, 2),
                row(1, 3),
                row(1, 4),
                row(1, 5),
            ],
            "case C PT",
        );
        check(
            RefDialect::Ft2,
            vec![0, 1],
            vec![c.clone(), pat_nch(6, 2)],
            vec![
                row(0, 0),
                row(0, 1),
                row(0, 2),
                row(1, 3),
                row(1, 4),
                row(1, 5),
            ],
            "case C FT2",
        );
        check(
            RefDialect::It,
            vec![0, 1],
            vec![c, pat_nch(6, 2)],
            vec![
                row(0, 0),
                row(0, 1),
                row(0, 2),
                row(0, 0),
                row(0, 1),
                row(0, 2),
                row(1, 3),
                row(1, 4),
                row(1, 5),
            ],
            "case C IT",
        );
    }
}
