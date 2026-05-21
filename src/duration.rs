//! Playback-duration estimator.
//!
//! Sums per-row tick costs from [`crate::daw::timeline::TimelineMap`],
//! which already encodes the resolved order walk (jumps, breaks,
//! loops, pattern delays). Bpm/Speed snapshots are taken at the
//! start of each row — intra-row `BpmSlide` is approximated by the
//! row's start-bpm, which matches the contract of a duration
//! *estimator* (the cycle-exact playback path lives in xmrsplayer).
//!
//! # Example
//!
//! ```ignore
//! use xmrs::prelude::*;
//! use xmrs::duration::ModuleDuration;
//!
//! let module = Module::load(&bytes)?;
//! let d = module.duration(0);
//! println!("{:>2}:{:02}", d.as_secs() / 60, d.as_secs() % 60);
//! ```

use core::time::Duration;

use crate::module::Module;

#[cfg(not(feature = "std"))]
mod no_std_math {
    #[inline]
    pub fn round_f64(x: f64) -> f64 {
        if x >= 0.0 {
            (x + 0.5) as i128 as f64
        } else {
            (x - 0.5) as i128 as f64
        }
    }

    #[inline]
    pub fn is_finite_f64(x: f64) -> bool {
        if x != x {
            return false;
        }
        x - x == 0.0
    }
}

#[cfg(not(feature = "std"))]
use no_std_math::{is_finite_f64, round_f64};

#[cfg(feature = "std")]
#[inline]
fn round_f64(x: f64) -> f64 {
    x.round()
}

#[cfg(feature = "std")]
#[inline]
fn is_finite_f64(x: f64) -> bool {
    x.is_finite()
}

/// Extension trait adding duration estimates to [`Module`].
///
/// ```ignore
/// use xmrs::prelude::*;
/// use xmrs::duration::ModuleDuration;
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
    let song_u16 = song.min(u16::MAX as usize) as u16;
    let mut total_secs: f64 = 0.0;
    for e in &module.timeline_map.entries {
        if e.song != song_u16 {
            continue;
        }
        let bpm = e.bpm_at_row.max(1) as f64;
        total_secs += e.speed_at_row as f64 * 2.5 / bpm;
    }
    secs_to_duration(total_secs)
}

#[inline]
fn secs_to_duration(secs: f64) -> Duration {
    if !is_finite_f64(secs) || secs <= 0.0 {
        return Duration::ZERO;
    }
    let nanos_total = round_f64(secs * 1_000_000_000.0);
    if nanos_total >= (u64::MAX as f64) * 1_000_000_000.0 {
        return Duration::new(u64::MAX, 999_999_999);
    }
    let nanos_total = nanos_total as u128;
    let secs = (nanos_total / 1_000_000_000) as u64;
    let nanos = (nanos_total % 1_000_000_000) as u32;
    Duration::new(secs, nanos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daw::build_timeline::build_timeline_layer;
    use crate::prelude::*;
    use alloc::vec;
    use alloc::vec::Vec;

    fn row_with(n: usize, ge: Vec<GlobalEffect>) -> Row {
        let mut r: Row = (0..n).map(|_| TrackUnit::default()).collect();
        r[0].global_effects = ge;
        r
    }

    fn empty_row(n: usize) -> Row {
        (0..n).map(|_| TrackUnit::default()).collect()
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
        let expected = Duration::from_secs_f64(64.0 * 6.0 * 2.5 / 125.0);
        assert!(close_to(m.duration(0), expected), "got {:?}", m.duration(0));
    }

    #[test]
    fn speed_change_doubles_time() {
        let mut pat: Pattern = (0..64).map(|_| empty_row(4)).collect();
        pat[0] = row_with(4, vec![GlobalEffect::Speed(12)]);
        let m = build_module(pat, vec![0]);
        let expected = Duration::from_secs_f64(64.0 * 12.0 * 2.5 / 125.0);
        assert!(close_to(m.duration(0), expected));
    }

    #[test]
    fn bpm_change_halves_time() {
        let mut pat: Pattern = (0..64).map(|_| empty_row(4)).collect();
        pat[0] = row_with(4, vec![GlobalEffect::Bpm(250)]);
        let m = build_module(pat, vec![0]);
        let expected = Duration::from_secs_f64(64.0 * 6.0 * 2.5 / 250.0);
        assert!(close_to(m.duration(0), expected));
    }

    #[test]
    fn position_jump_back_to_start_stops_at_one_pass() {
        let mut pat: Pattern = (0..4).map(|_| empty_row(4)).collect();
        pat[3] = row_with(4, vec![GlobalEffect::PositionJump(0)]);
        let m = build_module(pat, vec![0]);
        let expected = Duration::from_secs_f64(4.0 * 6.0 * 2.5 / 125.0);
        assert!(close_to(m.duration(0), expected));
    }

    #[test]
    fn pattern_break_skips_remaining_rows() {
        let mut pat0: Pattern = (0..16).map(|_| empty_row(4)).collect();
        pat0[7] = row_with(4, vec![GlobalEffect::PatternBreak(0)]);
        let pat1: Pattern = (0..64).map(|_| empty_row(4)).collect();
        let mut m = Module::default();
        build_timeline_layer(&mut m, &[vec![0, 1]], &[pat0, pat1]);
        let expected = Duration::from_secs_f64((8 + 64) as f64 * 6.0 * 2.5 / 125.0);
        assert!(close_to(m.duration(0), expected));
    }

    #[test]
    fn pattern_delay_multiplies_row_time() {
        let mut pat: Pattern = (0..4).map(|_| empty_row(4)).collect();
        pat[1] = row_with(
            4,
            vec![GlobalEffect::PatternDelay {
                quantity: 3,
                tempo: false,
            }],
        );
        let m = build_module(pat, vec![0]);
        let expected = Duration::from_secs_f64(7.0 * 6.0 * 2.5 / 125.0);
        assert!(close_to(m.duration(0), expected));
    }

    #[test]
    fn pattern_loop_runs_n_plus_one_times() {
        let mut pat: Pattern = (0..4).map(|_| empty_row(4)).collect();
        pat[0] = row_with(4, vec![GlobalEffect::PatternLoop(0)]);
        pat[3] = row_with(4, vec![GlobalEffect::PatternLoop(2)]);
        let m = build_module(pat, vec![0]);
        let expected = Duration::from_secs_f64(12.0 * 6.0 * 2.5 / 125.0);
        assert!(close_to(m.duration(0), expected));
    }

    #[test]
    fn speed_zero_ends_song_when_quirk_on() {
        let mut pat: Pattern = vec![empty_row(4); 4];
        pat[2] = row_with(4, vec![GlobalEffect::Speed(0)]);
        // Without the quirk: all 4 rows play.
        let m = build_module(pat.clone(), vec![0]);
        let four_rows = Duration::from_secs_f64(4.0 * 6.0 * 2.5 / 125.0);
        assert!(close_to(m.duration(0), four_rows));

        // With the quirk: stop after row 2.
        let mut m = Module {
            profile: CompatibilityProfile::pt(),
            ..Default::default()
        };
        build_timeline_layer(&mut m, &[vec![0]], &[pat]);
        let three_rows = Duration::from_secs_f64(3.0 * 6.0 * 2.5 / 125.0);
        assert!(close_to(m.duration(0), three_rows));
    }
}
