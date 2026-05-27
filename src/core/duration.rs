//! Playback-duration estimator.
//!
//! Sums per-row tick costs from [`crate::core::daw::timeline::TimelineMap`],
//! which already encodes the resolved order walk (jumps, breaks,
//! loops, pattern delays). Bpm/Speed snapshots are taken at the
//! start of each row — intra-row `BpmSlide` is approximated by the
//! row's start-bpm, which matches the contract of a duration
//! *estimator* (the cycle-exact playback path lives in xmrsplayer).
//!
//! The accumulator is integer nanoseconds (`u64`): the per-row
//! formula `seconds = speed × 2.5 / bpm` rewrites exactly as
//! `nanos = speed × 2_500_000_000 / bpm`, with `speed ∈ 0..=255`
//! (u8) and `bpm ∈ 1..=65535` (u16). The numerator peaks at
//! `255 × 2.5e9 ≈ 6.4e11` — well inside `u64`, and the final
//! total fits the same range for any realistically-long song.
//! No floats anywhere on this path.
//!
//! # Example
//!
//! ```ignore
//! use xmrs::prelude::*;
//! use xmrs::core::duration::ModuleDuration;
//!
//! let module = Module::load(&bytes)?;
//! let d = module.duration(0);
//! println!("{:>2}:{:02}", d.as_secs() / 60, d.as_secs() % 60);
//! ```

use core::time::Duration;

use crate::core::module::Module;

/// Extension trait adding duration estimates to [`Module`].
///
/// ```ignore
/// use xmrs::prelude::*;
/// use xmrs::core::duration::ModuleDuration;
///
/// let module = Module::load(&bytes)?;
/// let secs = module.duration(0).as_secs_f64();
/// ```
pub trait ModuleDuration {
    fn duration(&self, song: usize) -> Duration;
}

impl ModuleDuration for Module {
    fn duration(&self, song: usize) -> Duration {
        compute_duration(self, song)
    }
}

/// Free-function form for callers that prefer not to import the
/// extension trait. Returns [`Duration::ZERO`] when `song` has no
/// entries in `module.timeline_map`.
pub fn compute_duration(module: &Module, song: usize) -> Duration {
    // `song` is u16-wide in `TimelineMap`; anything past u16::MAX is
    // an out-of-range request and yields ZERO (no such song).
    if song > u16::MAX as usize {
        return Duration::ZERO;
    }
    let song_u16 = song as u16;
    let mut total_nanos: u64 = 0;
    for e in &module.timeline_map.entries {
        if e.song != song_u16 {
            continue;
        }
        let bpm = e.bpm_at_row.max(1) as u64;
        // row_seconds = speed × 2.5 / bpm
        //             = speed × 2_500_000_000 / bpm   (ns)
        //
        // With `speed: u8` and `bpm: u16 ≥ 1`, the numerator fits
        // u64 by ~7 orders of magnitude. Round-to-nearest by adding
        // `bpm / 2` before dividing — the accumulated error across
        // a 100k-row song is bounded by 50 µs, far below the
        // estimator's tolerance.
        let scaled = (e.speed_at_row as u64) * 2_500_000_000;
        let row_nanos = (scaled + bpm / 2) / bpm;
        total_nanos = total_nanos.saturating_add(row_nanos);
    }
    let secs = total_nanos / 1_000_000_000;
    let nanos = (total_nanos % 1_000_000_000) as u32;
    Duration::new(secs, nanos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use crate::tracker::import::build::{build_timeline_layer, Pattern, Row};
    use alloc::vec;
    use alloc::vec::Vec;

    /// Helper used by the navigation-flow tests below — each
    /// test row only ever carries navigation effects.
    fn nav_row(n: usize, ne: Vec<NavigationEffect>) -> Row {
        let mut r: Row = (0..n)
            .map(|_| crate::tracker::import::unit::TrackImportUnit::default())
            .collect();
        r[0].navigation = ne;
        r
    }

    /// Helper used by the BPM/Speed tests — each test row only ever
    /// carries song-level effects.
    fn song_row(n: usize, sle: Vec<SongLevelEffect>) -> Row {
        let mut r: Row = (0..n)
            .map(|_| crate::tracker::import::unit::TrackImportUnit::default())
            .collect();
        r[0].song_level = sle;
        r
    }

    fn empty_row(n: usize) -> Row {
        (0..n)
            .map(|_| crate::tracker::import::unit::TrackImportUnit::default())
            .collect()
    }

    /// Build a Module from a synthetic pattern + order pair and
    /// populate its DAW layer (timeline_map etc.). Returns the
    /// Module ready for `duration()`.
    fn build_module(pattern: Pattern, order: Vec<usize>) -> Module {
        let mut m = Module::default();
        let pattern_order = vec![order];
        let patterns = vec![pattern];
        build_timeline_layer(&mut m, &pattern_order, &patterns);
        m
    }

    /// Integer-arithmetic equivalent of `seconds × 2.5 / bpm` →
    /// `Duration`. Lets the tests express expected values without
    /// reintroducing `Duration::from_secs_f64` (which would pull
    /// floats back into the test path that the implementation just
    /// purged).
    fn expected_duration(rows: u64, speed: u64, bpm: u64) -> Duration {
        let total_nanos = (rows * speed * 2_500_000_000 + bpm / 2) / bpm;
        let secs = total_nanos / 1_000_000_000;
        let nanos = (total_nanos % 1_000_000_000) as u32;
        Duration::new(secs, nanos)
    }

    fn close_to(d: Duration, expected: Duration) -> bool {
        let delta = d.abs_diff(expected);
        delta < Duration::from_millis(1)
    }

    #[test]
    fn empty_module_is_zero() {
        let m = Module::default();
        assert_eq!(m.duration(0), Duration::ZERO);
    }

    #[test]
    fn out_of_range_song_is_zero() {
        let m = build_module(vec![empty_row(4); 8], vec![0]);
        assert_eq!(m.duration(42), Duration::ZERO);
    }

    #[test]
    fn defaults_one_pattern_64_rows() {
        let pat: Pattern = (0..64).map(|_| empty_row(4)).collect();
        let m = build_module(pat, vec![0]);
        let expected = expected_duration(64, 6, 125);
        assert!(close_to(m.duration(0), expected), "got {:?}", m.duration(0));
    }

    #[test]
    fn speed_change_doubles_time() {
        let mut pat: Pattern = (0..64).map(|_| empty_row(4)).collect();
        pat[0] = song_row(4, vec![SongLevelEffect::Speed(12)]);
        let m = build_module(pat, vec![0]);
        let expected = expected_duration(64, 12, 125);
        assert!(close_to(m.duration(0), expected));
    }

    #[test]
    fn bpm_change_halves_time() {
        let mut pat: Pattern = (0..64).map(|_| empty_row(4)).collect();
        pat[0] = song_row(4, vec![SongLevelEffect::Bpm(250)]);
        let m = build_module(pat, vec![0]);
        let expected = expected_duration(64, 6, 250);
        assert!(close_to(m.duration(0), expected));
    }

    #[test]
    fn position_jump_back_to_start_stops_at_one_pass() {
        let mut pat: Pattern = (0..4).map(|_| empty_row(4)).collect();
        pat[3] = nav_row(4, vec![NavigationEffect::PositionJump(0)]);
        let m = build_module(pat, vec![0]);
        let expected = expected_duration(4, 6, 125);
        assert!(close_to(m.duration(0), expected));
    }

    #[test]
    fn pattern_break_skips_remaining_rows() {
        let mut pat0: Pattern = (0..16).map(|_| empty_row(4)).collect();
        pat0[7] = nav_row(4, vec![NavigationEffect::PatternBreak(0)]);
        let pat1: Pattern = (0..64).map(|_| empty_row(4)).collect();
        let mut m = Module::default();
        build_timeline_layer(&mut m, &[vec![0, 1]], &[pat0, pat1]);
        let expected = expected_duration(8 + 64, 6, 125);
        assert!(close_to(m.duration(0), expected));
    }

    #[test]
    fn pattern_delay_multiplies_row_time() {
        let mut pat: Pattern = (0..4).map(|_| empty_row(4)).collect();
        pat[1] = nav_row(
            4,
            vec![NavigationEffect::PatternDelay {
                quantity: 3,
                tempo: false,
            }],
        );
        let m = build_module(pat, vec![0]);
        let expected = expected_duration(7, 6, 125);
        assert!(close_to(m.duration(0), expected));
    }

    #[test]
    fn pattern_loop_runs_n_plus_one_times() {
        let mut pat: Pattern = (0..4).map(|_| empty_row(4)).collect();
        pat[0] = nav_row(4, vec![NavigationEffect::PatternLoop(0)]);
        pat[3] = nav_row(4, vec![NavigationEffect::PatternLoop(2)]);
        let m = build_module(pat, vec![0]);
        let expected = expected_duration(12, 6, 125);
        assert!(close_to(m.duration(0), expected));
    }

    #[test]
    fn speed_zero_ends_song_when_quirk_on() {
        let mut pat: Pattern = vec![empty_row(4); 4];
        pat[2] = song_row(4, vec![SongLevelEffect::Speed(0)]);
        // Without the quirk: all 4 rows play.
        let m = build_module(pat.clone(), vec![0]);
        let four_rows = expected_duration(4, 6, 125);
        assert!(close_to(m.duration(0), four_rows));

        // With the quirk: stop after row 2.
        let mut m = Module {
            quirks: crate::core::compatibility::PlaybackQuirks {
                speed_zero_ends_song: true,
                ..Default::default()
            },
            ..Default::default()
        };
        build_timeline_layer(&mut m, &[vec![0]], &[pat]);
        let three_rows = expected_duration(3, 6, 125);
        assert!(close_to(m.duration(0), three_rows));
    }
}
