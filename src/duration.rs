//! Estimation of a song's playback duration.
//!
//! Walks the pattern order of a [`Module`] sub-song row by row,
//! simulating the row scheduler that a real player would use, and
//! accumulates seconds. The simulation honours:
//!
//! * `GlobalEffect::Speed(n)`  — ticks/row
//! * `GlobalEffect::Bpm(n)`    — beats per minute
//! * `GlobalEffect::BpmSlide(d)` — applied per tick from tick 1
//! * `GlobalEffect::PatternBreak(row)` — jump to next order at `row`
//! * `GlobalEffect::PositionJump(pos)` — jump to order `pos`
//! * `GlobalEffect::PatternLoop(v)` — anchor (`v == 0`) / loop `v` times
//! * `GlobalEffect::PatternDelay { quantity, .. }` — row plays `q + 1` times
//!
//! The simulation stops when:
//! * the pattern order is exhausted,
//! * a `(order_idx, row_idx, loop_iters_left)` state is revisited
//!   (the song closes its natural loop), or
//! * a configurable hard cap on processed rows is reached.
//!
//! For the common case where a song ends with a `PositionJump` back to
//! its restart position, the function returns the duration of a single
//! play-through (from start to the moment the loop would re-start).
//!
//! # Example
//!
//! ```ignore
//! use xmrs::prelude::*;
//! use xmrs::duration::ModuleDuration;
//!
//! let module = Module::load(&bytes)?;
//! let d = module.duration(0); // first sub-song
//! println!("{:>2}:{:02}", d.as_secs() / 60, d.as_secs() % 60);
//! ```

use core::time::Duration;

use alloc::collections::BTreeSet;

use crate::effect::GlobalEffect;
use crate::module::Module;

/// Hard upper bound on the number of rows the simulator will visit
/// before giving up. A 10-minute song at speed 6 / BPM 125 plays
/// roughly 12 500 rows, so this cap (≈ a *month* of music at that
/// rate) is only ever reached by a pathological or genuinely
/// non-terminating module.
pub const DEFAULT_MAX_ROWS: usize = 1 << 20;

/// Configurable knobs for the duration walker. The defaults match
/// the convenience method [`ModuleDuration::duration`].
#[derive(Debug, Clone, Copy)]
pub struct DurationOptions {
    /// Hard cap on processed rows; stops runaway simulation.
    pub max_rows: usize,
    /// If `true`, the simulator treats a `(order, row, loop-state)`
    /// revisit as the natural end of the song (one play-through).
    /// If `false`, it keeps going until `max_rows` — useful if you
    /// want the total time of N consecutive plays without thinking
    /// about restart points.
    pub stop_on_loop: bool,
}

impl Default for DurationOptions {
    fn default() -> Self {
        Self {
            max_rows: DEFAULT_MAX_ROWS,
            stop_on_loop: true,
        }
    }
}

/// Extension trait that adds duration-estimation methods to
/// [`Module`]. Brought into scope with
/// `use xmrs::duration::ModuleDuration;`.
///
/// # Example
///
/// ```ignore
/// use xmrs::prelude::*;
/// use xmrs::duration::ModuleDuration;
///
/// let module = Module::load(&bytes)?;
/// let d = module.duration(0); // first sub-song
/// println!("{}:{:02}", d.as_secs() / 60, d.as_secs() % 60);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub trait ModuleDuration {
    /// Duration of one play-through of sub-song `song`, with default
    /// options. Returns [`Duration::ZERO`] for out-of-range / empty
    /// inputs.
    ///
    /// "One play-through" means: from the start of the order list
    /// until the first state revisit (the song's natural loop point)
    /// or the end of the order list, whichever comes first.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use xmrs::prelude::*;
    /// use xmrs::duration::ModuleDuration;
    ///
    /// let module = Module::load(&bytes)?;
    /// let secs = module.duration(0).as_secs_f64();
    /// println!("{secs:.2}s");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    fn duration(&self, song: usize) -> Duration;

    /// Same as [`Self::duration`] but with explicit options. Useful
    /// to measure several consecutive plays (set `stop_on_loop:
    /// false`) or to tighten the runaway cap on memory-constrained
    /// targets.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use xmrs::prelude::*;
    /// use xmrs::duration::{ModuleDuration, DurationOptions};
    ///
    /// let module = Module::load(&bytes)?;
    ///
    /// // Total time of the song playing through to a hard cap of
    /// // 10 000 rows, ignoring natural loop points.
    /// let d = module.duration_with(0, DurationOptions {
    ///     max_rows: 10_000,
    ///     stop_on_loop: false,
    /// });
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    fn duration_with(&self, song: usize, opts: DurationOptions) -> Duration;
}

impl ModuleDuration for Module {
    #[inline]
    fn duration(&self, song: usize) -> Duration {
        self.duration_with(song, DurationOptions::default())
    }

    fn duration_with(&self, song: usize, opts: DurationOptions) -> Duration {
        compute_duration(self, song, opts)
    }
}

/// Free-function form for callers that prefer not to import the
/// extension trait.
pub fn compute_duration(module: &Module, song: usize, opts: DurationOptions) -> Duration {
    // -- validate inputs --
    if song >= module.pattern_order.len() {
        return Duration::ZERO;
    }
    let order = &module.pattern_order[song];
    if order.is_empty() || module.pattern.is_empty() {
        return Duration::ZERO;
    }

    // -- player state --
    // Speed defaults to 6, BPM to 125 — but we read the module's
    // declared values and clamp to at least 1 to avoid /0.
    let mut speed: usize = module.default_tempo.max(1);
    let mut bpm: f64 = module.default_bpm.max(1) as f64;

    let mut order_idx: usize = 0;
    let mut row_idx: usize = 0;

    // Pattern-loop state. Real trackers keep one of these *per
    // channel*; collapsing to song-level is enough for a duration
    // estimate and handles the textbook case where loop markers live
    // on a single channel.
    let mut loop_anchor: usize = 0;
    let mut loop_iters_left: Option<usize> = None;

    // Accumulator in f64 for precision; we cast once at the end.
    let mut total_secs: f64 = 0.0;

    // Revisit detection. Encoding `loop_iters_left` into the key
    // lets a legitimate pattern-loop iterate without being mistaken
    // for the song's natural loop point.
    let mut visited: BTreeSet<(usize, usize, Option<usize>)> = BTreeSet::new();
    let mut rows_processed: usize = 0;

    loop {
        // -- bounds & order advance --
        if order_idx >= order.len() {
            break; // song reached the end of the order list
        }
        if rows_processed >= opts.max_rows {
            break; // safety cap
        }

        let pat_idx = order[order_idx];
        if pat_idx >= module.pattern.len() {
            // unplayable order entry; treat as an empty pattern
            order_idx += 1;
            row_idx = 0;
            loop_iters_left = None;
            continue;
        }
        let pattern = &module.pattern[pat_idx];
        if pattern.is_empty() || row_idx >= pattern.len() {
            // out-of-range row (e.g. a PatternBreak past the
            // pattern's end) → fall through to the next order
            order_idx += 1;
            row_idx = 0;
            loop_iters_left = None;
            continue;
        }

        // Natural-loop detection — same (order, row, loop-state)
        // means a future iteration would re-trace the past, so
        // stop here.
        if opts.stop_on_loop {
            let key = (order_idx, row_idx, loop_iters_left);
            if !visited.insert(key) {
                break;
            }
        }
        rows_processed += 1;

        // -- scan all channels of this row for global effects --
        let row = &pattern[row_idx];

        let mut new_speed: Option<usize> = None;
        let mut new_bpm: Option<usize> = None;
        let mut bpm_slide: isize = 0;
        let mut do_break: Option<usize> = None;
        let mut do_jump: Option<usize> = None;
        let mut loop_param: Option<usize> = None;
        let mut row_repeats: usize = 1;
        let mut song_end_requested: bool = false;

        for unit in row.iter() {
            for ge in unit.global_effects.iter() {
                match ge {
                    GlobalEffect::Speed(n) => {
                        if *n > 0 {
                            new_speed = Some(*n);
                        } else if module.profile.quirks.speed_zero_ends_song {
                            // MOD-style `F00` = end of song. Mark
                            // for termination after this row's
                            // tick run completes — matches the
                            // sequencer's "row plays, then
                            // SongEnd before the next row would
                            // start" behaviour.
                            song_end_requested = true;
                        }
                    }
                    GlobalEffect::Bpm(n) => {
                        if *n > 0 {
                            new_bpm = Some(*n);
                        }
                    }
                    GlobalEffect::BpmSlide(d) => {
                        // Multiple slides on one row accumulate.
                        bpm_slide = bpm_slide.saturating_add(*d);
                    }
                    GlobalEffect::PatternBreak(p) => do_break = Some(*p),
                    GlobalEffect::PositionJump(p) => do_jump = Some(*p),
                    GlobalEffect::PatternLoop(v) => loop_param = Some(*v),
                    GlobalEffect::PatternDelay { quantity, .. } => {
                        // "If multiple commands are on the same row,
                        // the sum of their parameters is used." A
                        // delay parameter of N means the row plays
                        // N+1 times (the original pass + N repeats).
                        row_repeats = row_repeats.saturating_add(*quantity);
                    }
                    _ => {}
                }
            }
        }

        // Speed / BPM changes apply *before* this row is timed,
        // matching tracker semantics (Fxx on a row takes effect on
        // that row, not the next).
        if let Some(s) = new_speed {
            speed = s.max(1);
        }
        if let Some(b) = new_bpm {
            bpm = b.max(1) as f64;
        }

        // -- accumulate row time, tick by tick, applying any
        //    intra-row BPM slide --
        for _ in 0..row_repeats {
            for tick in 0..speed {
                if tick > 0 && bpm_slide != 0 {
                    let next = (bpm as isize).saturating_add(bpm_slide);
                    // Clamp to a sane BPM range; real trackers
                    // typically allow 32..=255, but we just guard
                    // against /0 here.
                    bpm = next.clamp(1, 1000) as f64;
                }
                total_secs += 2.5 / bpm;
            }
        }

        // -- F00 / Speed(0) under the MOD quirk: stop after this
        //    row completes its tick run. Tie-break order is
        //    deliberate: a `PositionJump` or `PatternBreak` on the
        //    same row could otherwise stay queued forever — by
        //    placing the end check here, between the timing
        //    accumulation and the flow-control resolution, the
        //    song-end signal wins. ProTracker behaves the same
        //    way: the `mt_PosJumpAssert` test fires later in the
        //    same tick chain than the `mt_fxx` speed setter that
        //    clears the playback flag.
        if song_end_requested {
            break;
        }

        // -- flow control: jump > break > loop > advance --
        if let Some(pos) = do_jump {
            order_idx = pos;
            row_idx = 0;
            loop_iters_left = None;
            loop_anchor = 0;
            continue;
        }
        if let Some(target) = do_break {
            order_idx += 1;
            row_idx = target;
            loop_iters_left = None;
            loop_anchor = 0;
            continue;
        }
        if let Some(v) = loop_param {
            if v == 0 {
                // anchor: mark this row as the loop start
                loop_anchor = row_idx;
                row_idx += 1;
            } else {
                // E6x / SBx with non-zero parameter: jump back to
                // the anchor `v` times. Total passes over the loop
                // body = `v + 1` (one initial + v repeats), matching
                // canonical XM/IT semantics.
                match loop_iters_left {
                    None => {
                        // first hit: this *is* one of the v repeats,
                        // so initialise the counter to `v - 1` and
                        // jump. (v >= 1 here — v == 0 is the anchor
                        // branch above.)
                        loop_iters_left = Some(v - 1);
                        row_idx = loop_anchor;
                    }
                    Some(0) => {
                        // counter exhausted: fall through
                        loop_iters_left = None;
                        row_idx += 1;
                    }
                    Some(n) => {
                        loop_iters_left = Some(n - 1);
                        row_idx = loop_anchor;
                    }
                }
            }
            continue;
        }

        // -- default: next row, possibly spilling into next order --
        row_idx += 1;
        if row_idx >= pattern.len() {
            order_idx += 1;
            row_idx = 0;
            loop_iters_left = None;
            loop_anchor = 0;
        }
    }

    secs_to_duration(total_secs)
}

#[inline]
fn secs_to_duration(secs: f64) -> Duration {
    if !secs.is_finite() || secs <= 0.0 {
        return Duration::ZERO;
    }
    // Saturate at u64::MAX seconds to avoid overflow on absurd inputs.
    let nanos_total = (secs * 1_000_000_000.0).round();
    if nanos_total >= (u64::MAX as f64) * 1_000_000_000.0 {
        return Duration::new(u64::MAX, 999_999_999);
    }
    let nanos_total = nanos_total as u128;
    let secs = (nanos_total / 1_000_000_000) as u64;
    let nanos = (nanos_total % 1_000_000_000) as u32;
    Duration::new(secs, nanos)
}

// ---------------------------------------------------------------
// Tests
// ---------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use alloc::vec;
    use alloc::vec::Vec;

    /// Build a row with `n` empty units plus an optional list of
    /// global effects placed on channel 0.
    fn row_with(n: usize, ge: Vec<GlobalEffect>) -> Row {
        let mut r: Row = (0..n).map(|_| TrackUnit::default()).collect();
        r[0].global_effects = ge;
        r
    }

    fn empty_row(n: usize) -> Row {
        (0..n).map(|_| TrackUnit::default()).collect()
    }

    fn make_module(pattern: Pattern, order: Vec<usize>) -> Module {
        let mut m = Module::default();
        m.pattern = vec![pattern];
        m.pattern_order = vec![order];
        m
    }

    #[test]
    fn empty_module_is_zero() {
        let m = Module::default();
        assert_eq!(m.duration(0), Duration::ZERO);
    }

    #[test]
    fn out_of_range_song_is_zero() {
        let m = make_module(vec![empty_row(4); 8], vec![0]);
        assert_eq!(m.duration(42), Duration::ZERO);
    }

    #[test]
    fn defaults_one_pattern_64_rows() {
        // 64 rows * 6 ticks * 2.5 / 125 BPM = 7.68 s exactly.
        let pat: Pattern = (0..64).map(|_| empty_row(4)).collect();
        let m = make_module(pat, vec![0]);

        let d = m.duration(0);
        let expected = Duration::from_secs_f64(64.0 * 6.0 * 2.5 / 125.0);
        // Allow 1 ms slack for f64 round-trip.
        let delta = if d > expected {
            d - expected
        } else {
            expected - d
        };
        assert!(
            delta < Duration::from_millis(1),
            "got {d:?}, expected {expected:?}"
        );
    }

    #[test]
    fn speed_change_doubles_time() {
        // Row 0 sets speed to 12 (was 6) → all 64 rows now take twice
        // as long. Expected: 64 * 12 * 2.5 / 125 = 15.36 s.
        let mut pat: Pattern = (0..64).map(|_| empty_row(4)).collect();
        pat[0] = row_with(4, vec![GlobalEffect::Speed(12)]);
        let m = make_module(pat, vec![0]);

        let d = m.duration(0);
        let expected = Duration::from_secs_f64(64.0 * 12.0 * 2.5 / 125.0);
        let delta = if d > expected {
            d - expected
        } else {
            expected - d
        };
        assert!(
            delta < Duration::from_millis(1),
            "got {d:?}, expected {expected:?}"
        );
    }

    #[test]
    fn bpm_change_halves_time() {
        // Row 0 sets BPM to 250 → all 64 rows are twice as fast.
        // Expected: 64 * 6 * 2.5 / 250 = 3.84 s.
        let mut pat: Pattern = (0..64).map(|_| empty_row(4)).collect();
        pat[0] = row_with(4, vec![GlobalEffect::Bpm(250)]);
        let m = make_module(pat, vec![0]);

        let d = m.duration(0);
        let expected = Duration::from_secs_f64(64.0 * 6.0 * 2.5 / 250.0);
        let delta = if d > expected {
            d - expected
        } else {
            expected - d
        };
        assert!(delta < Duration::from_millis(1));
    }

    #[test]
    fn position_jump_back_to_start_stops_at_one_pass() {
        // 4-row pattern, last row jumps back to order 0.
        // One play-through = 4 rows = 4 * 6 * 2.5 / 125 = 0.48 s.
        let mut pat: Pattern = (0..4).map(|_| empty_row(4)).collect();
        pat[3] = row_with(4, vec![GlobalEffect::PositionJump(0)]);
        let m = make_module(pat, vec![0]);

        let d = m.duration(0);
        let expected = Duration::from_secs_f64(4.0 * 6.0 * 2.5 / 125.0);
        let delta = if d > expected {
            d - expected
        } else {
            expected - d
        };
        assert!(delta < Duration::from_millis(1));
    }

    #[test]
    fn pattern_break_skips_remaining_rows() {
        // Two distinct patterns. Pattern 0 has 16 rows, row 7 breaks
        // to row 0 of the next order. Pattern 1 has 64 rows, no
        // break. Order = [0, 1]. We expect 8 + 64 = 72 rows.
        let mut pat0: Pattern = (0..16).map(|_| empty_row(4)).collect();
        pat0[7] = row_with(4, vec![GlobalEffect::PatternBreak(0)]);
        let pat1: Pattern = (0..64).map(|_| empty_row(4)).collect();

        let mut m = Module::default();
        m.pattern = vec![pat0, pat1];
        m.pattern_order = vec![vec![0, 1]];

        let d = m.duration(0);
        let expected = Duration::from_secs_f64((8 + 64) as f64 * 6.0 * 2.5 / 125.0);
        let delta = if d > expected {
            d - expected
        } else {
            expected - d
        };
        assert!(
            delta < Duration::from_millis(1),
            "got {d:?}, expected {expected:?}"
        );
    }

    #[test]
    fn pattern_delay_multiplies_row_time() {
        // 4 rows, row 1 has PatternDelay { quantity: 3, tempo: false }
        // → that row is played 4 times. Total row-equivalents = 7.
        let mut pat: Pattern = (0..4).map(|_| empty_row(4)).collect();
        pat[1] = row_with(
            4,
            vec![GlobalEffect::PatternDelay {
                quantity: 3,
                tempo: false,
            }],
        );
        let m = make_module(pat, vec![0]);

        let d = m.duration(0);
        let expected = Duration::from_secs_f64(7.0 * 6.0 * 2.5 / 125.0);
        let delta = if d > expected {
            d - expected
        } else {
            expected - d
        };
        assert!(
            delta < Duration::from_millis(1),
            "got {d:?}, expected {expected:?}"
        );
    }

    #[test]
    fn pattern_loop_runs_n_plus_one_times() {
        // Rows 0..4. Row 0: E60 anchor. Row 3: E62 → loop runs 3
        // times total (initial + 2 repeats). Then we advance to row
        // 4 (which doesn't exist) → end of song.
        //
        //   pass 1: rows 0,1,2,3      (4 rows)
        //   pass 2: rows 0,1,2,3      (4 rows)
        //   pass 3: rows 0,1,2,3      (4 rows)  ← counter exhausts here
        //   then fall through to row 4 → end
        //
        // total = 12 rows.
        let mut pat: Pattern = (0..4).map(|_| empty_row(4)).collect();
        pat[0] = row_with(4, vec![GlobalEffect::PatternLoop(0)]);
        pat[3] = row_with(4, vec![GlobalEffect::PatternLoop(2)]);
        let m = make_module(pat, vec![0]);

        let d = m.duration(0);
        let expected = Duration::from_secs_f64(12.0 * 6.0 * 2.5 / 125.0);
        let delta = if d > expected {
            d - expected
        } else {
            expected - d
        };
        assert!(
            delta < Duration::from_millis(1),
            "got {d:?}, expected {expected:?}"
        );
    }

    #[test]
    fn runaway_is_capped() {
        // Row 0 jumps back to itself — without any pattern-loop state
        // change. With stop_on_loop = true we should terminate
        // immediately after one row. With stop_on_loop = false we
        // should hit the max_rows cap.
        let mut pat: Pattern = vec![empty_row(4)];
        pat[0] = row_with(4, vec![GlobalEffect::PositionJump(0)]);
        let m = make_module(pat, vec![0]);

        let d_default = m.duration(0);
        let one_row = Duration::from_secs_f64(6.0 * 2.5 / 125.0);
        let delta = if d_default > one_row {
            d_default - one_row
        } else {
            one_row - d_default
        };
        assert!(delta < Duration::from_millis(1));

        let opts = DurationOptions {
            max_rows: 100,
            stop_on_loop: false,
        };
        let d_capped = m.duration_with(0, opts);
        let cap_expected = Duration::from_secs_f64(100.0 * 6.0 * 2.5 / 125.0);
        let delta = if d_capped > cap_expected {
            d_capped - cap_expected
        } else {
            cap_expected - d_capped
        };
        assert!(delta < Duration::from_millis(1));
    }

    #[test]
    fn speed_zero_ends_song_when_quirk_on() {
        // 4-row pattern with `F00` on row 2. With the MOD quirk
        // active the simulation must terminate after row 2 plays
        // its tick run — so total duration = 3 rows × speed × tick
        // time (1 = row 0, 2 = row 1, 3 = row 2 with F00).
        let mut pat: Pattern = vec![empty_row(4); 4];
        pat[2] = row_with(4, vec![GlobalEffect::Speed(0)]);
        let mut m = make_module(pat, vec![0]);
        // Default profile has the quirk off — the song should
        // simply play all 4 rows.
        let d_no_quirk = m.duration(0);
        let four_rows = Duration::from_secs_f64(4.0 * 6.0 * 2.5 / 125.0);
        let delta = if d_no_quirk > four_rows {
            d_no_quirk - four_rows
        } else {
            four_rows - d_no_quirk
        };
        assert!(
            delta < Duration::from_millis(1),
            "no-quirk: got {d_no_quirk:?}, expected {four_rows:?}",
        );

        // With the MOD quirk on, the song stops after row 2.
        m.profile = CompatibilityProfile::pt();
        let d_quirk = m.duration(0);
        let three_rows = Duration::from_secs_f64(3.0 * 6.0 * 2.5 / 125.0);
        let delta = if d_quirk > three_rows {
            d_quirk - three_rows
        } else {
            three_rows - d_quirk
        };
        assert!(
            delta < Duration::from_millis(1),
            "with-quirk: got {d_quirk:?}, expected {three_rows:?}",
        );
    }

    #[test]
    fn speed_zero_with_quirk_off_keeps_old_speed() {
        // When the quirk is OFF, `Speed(0)` is silently ignored
        // (defensive guard at the parsing site) and the song
        // continues at its existing speed. This regression-tests
        // the pre-quirk behaviour so we don't change it on
        // non-MOD formats.
        let mut pat: Pattern = vec![empty_row(4); 3];
        pat[1] = row_with(4, vec![GlobalEffect::Speed(0)]);
        let m = make_module(pat, vec![0]);
        // Default Module (no quirks): three rows at speed 6,
        // BPM 125.
        let d = m.duration(0);
        let three_rows = Duration::from_secs_f64(3.0 * 6.0 * 2.5 / 125.0);
        let delta = if d > three_rows {
            d - three_rows
        } else {
            three_rows - d
        };
        assert!(delta < Duration::from_millis(1));
    }
}
