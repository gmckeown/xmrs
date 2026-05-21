//! Strict Euclidean rhythm detection and auto-conversion at import
//! time.
//!
//! [`auto_convert_strict_euclidean_tracks`] is the last pass of
//! [`crate::daw::build_timeline::build_timeline_layer`]. It scans every
//! Track and — for each one whose cell pattern is bit-identical to
//! a canonical Bjorklund(N, K) under some rotation with uniform
//! note, uniform velocity, and no per-cell effects — wraps the
//! original instrument in an `InstrumentType::Euclidean` and
//! rewires the track + its Play cells to point at the wrapper.
//!
//! The conversion is lossless: the player's renderer follows
//! `InstrEkn::instr` back to the original sample-based instrument
//! and plays the same notes at the same ticks. The added
//! `InstrEkn` metadata only matters when the user opts into
//! humanisation (`HumanizeMode::Live` / `Deterministic` + non-zero
//! `humanize_probability`).

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;

use crate::cell_note::CellNote;
use crate::daw::track::Track;
use crate::fixed::fixed::Q15;
use crate::instr_ekn::InstrEkn;
use crate::instrument::{Instrument, InstrumentType};
use crate::module::Module;
use crate::pitch::Pitch;
use crate::track_unit::TrackUnit;

/// Detected Bjorklund parameters of a Track plus the uniform cell
/// prototype that every pulse renders at playback.
#[derive(Debug, Clone)]
pub struct EuclideanParams {
    /// Number of pulses (`k` in Bjorklund(k, n) literature).
    pub events: u8,
    /// Length of one rhythm cycle in track rows (`n` in Bjorklund(k, n)).
    pub steps: u8,
    /// Right-rotation of the canonical Bresenham pattern. `0` =
    /// canonical (pulse on row 0).
    pub rotation: u8,
    /// Uniform cell shared by every pulse of this rhythm: note,
    /// velocity, original-instrument reference, and empty effect
    /// vectors (the strict criteria forbid per-cell effects).
    /// [`auto_convert_strict_euclidean_tracks`] rewrites
    /// `prototype.instrument` to the wrapper index when it commits
    /// the conversion.
    pub prototype: TrackUnit,
}

/// Walks every Track in `module` and, for each one strictly
/// matching a Bjorklund pattern, rewrites it into an
/// `InstrumentType::Euclidean` wrapper. Idempotent: a Track whose
/// instrument is already an `Euclidean` is skipped.
pub fn auto_convert_strict_euclidean_tracks(module: &mut Module) {
    let n_tracks = module.tracks.len();
    for track_idx in 0..n_tracks {
        let cur_instr_idx = module.tracks[track_idx].instrument;
        // Skip tracks whose `instrument` doesn't point at a real
        // entry of `module.instrument` — either the sentinel
        // `EFFECT_ONLY_INSTRUMENT` carried by effect-only tracks,
        // or a malformed source that references an instrument
        // beyond the imported list. In both cases there's no
        // sample instrument to wrap.
        let Some(instr) = module.instrument.get(cur_instr_idx) else {
            continue;
        };
        if matches!(instr.instr_type, InstrumentType::Euclidean(_)) {
            continue;
        }
        if let Some(params) = detect_strict_euclidean(&module.tracks[track_idx]) {
            convert_track_to_euclidean_in_place(module, track_idx, params);
        }
    }
}

/// `Some(params)` when the Track's cells form a strict Bjorklund
/// pattern with uniform pitch, uniform velocity, no per-cell
/// effects, length in `[4, 255]`, and at least 2 events.
///
/// Returns `None` otherwise — the track is left alone by
/// [`auto_convert_strict_euclidean_tracks`].
pub fn detect_strict_euclidean(track: &Track) -> Option<EuclideanParams> {
    let k = track.rows.len();
    if !(4..=u8::MAX as usize).contains(&k) {
        return None;
    }

    let mut positions: Vec<usize> = Vec::new();
    let mut first_pitch: Option<Pitch> = None;
    let mut first_velocity = None;
    let mut first_instrument: Option<usize> = None;

    for (i, cell) in track.rows.iter().enumerate() {
        if !cell.effects.is_empty() || !cell.global_effects.is_empty() {
            return None;
        }
        match cell.note {
            CellNote::Empty => continue,
            CellNote::Play(p) => {
                if let Some(fp) = first_pitch {
                    if fp != p {
                        return None;
                    }
                } else {
                    first_pitch = Some(p);
                }
                if let Some(fv) = first_velocity {
                    if fv != cell.velocity {
                        return None;
                    }
                } else {
                    first_velocity = Some(cell.velocity);
                }
                // Record the instrument of the first play cell —
                // every other play cell is required to match
                // (either the same `Some(i)` or `None`, inheriting
                // the track's instrument).
                match (first_instrument, cell.instrument) {
                    (None, Some(i)) => first_instrument = Some(i),
                    (Some(prev), Some(i)) if prev != i => return None,
                    _ => {}
                }
                positions.push(i);
            }
            // Pure rhythm: any other note variant disqualifies.
            CellNote::KeyOff | CellNote::NoteCut | CellNote::NoteFade => return None,
        }
    }

    let n = positions.len();
    if n < 2 {
        return None;
    }
    let n_u8 = n as u8;
    let k_u8 = k as u8;
    let pitch = first_pitch.expect("first_pitch set when n >= 2");
    let velocity = first_velocity.expect("first_velocity set when n >= 2");
    // Fall back to the track's instrument when no cell carried an
    // explicit one (all pulses inheriting from the track header).
    let instrument = first_instrument.unwrap_or(track.instrument);

    // Canonical Bjorklund(n, k) — bit-pattern of length k with n
    // Trues distributed as evenly as possible.
    let canonical = euclidean_pattern(n_u8, k_u8);

    // Try every rotation. The count of trues is constant under
    // rotation, so checking "every track-Play position lands on a
    // canonical True" plus "same number of trues on both sides"
    // proves set equality.
    for rotation in 0..k {
        let all_match = positions.iter().all(|&p| canonical[(p + k - rotation) % k]);
        if all_match {
            let prototype = TrackUnit {
                note: CellNote::Play(pitch),
                velocity,
                instrument: Some(instrument),
                effects: Vec::new(),
                global_effects: Vec::new(),
            };
            return Some(EuclideanParams {
                events: n_u8,
                steps: k_u8,
                rotation: rotation as u8,
                prototype,
            });
        }
    }
    None
}

/// Canonical Euclidean(events, steps) bit pattern via Bresenham's
/// error-term algorithm. Returns a `Vec<bool>` of length `steps`
/// with `events` Trues spread as evenly as possible — index 0
/// always True when `events > 0`. Used both by `detect_strict_euclidean`
/// and by `Module::row_at`'s generative branch.
pub fn euclidean_pattern(events: u8, steps: u8) -> Vec<bool> {
    let s = steps as i32;
    let mut result: Vec<bool> = Vec::with_capacity(steps as usize);
    if events == 0 {
        result.resize(steps as usize, false);
        return result;
    }
    let e = events as i32;
    let mut error: i32 = 0;
    for _ in 0..steps {
        if error < 0 {
            result.push(false);
            error += e;
        } else {
            result.push(true);
            error += e - s;
        }
    }
    result
}

fn convert_track_to_euclidean_in_place(
    module: &mut Module,
    track_idx: usize,
    params: EuclideanParams,
) {
    let original_instr_idx = module.tracks[track_idx].instrument;

    let ekn = InstrEkn {
        events: params.events,
        steps: params.steps,
        rotation: params.rotation,
        instr: Some(original_instr_idx),
        humanize_advance_max_ticks: 0,
        humanize_probability: Q15::ZERO,
    };

    let mut name = String::new();
    let _ = write!(
        &mut name,
        "Euclidean({},{}) of {}",
        params.events, params.steps, module.instrument[original_instr_idx].name
    );

    // Reuse the first unnamed `InstrumentType::Empty` slot if any
    // (S3M and IT files often ship instrument tables padded with
    // empty placeholders); fall back to appending. Avoids stranding
    // the wrapper at index 99 in a module that has 56 real and 43
    // placeholder instruments.
    let empty_slot = module
        .instrument
        .iter()
        .position(|i| i.name.is_empty() && matches!(i.instr_type, InstrumentType::Empty));
    let new_instr_idx = match empty_slot {
        Some(idx) => {
            module.instrument[idx] = Instrument {
                name,
                instr_type: InstrumentType::Euclidean(ekn),
                muted: false,
            };
            idx
        }
        None => {
            module.instrument.push(Instrument {
                name,
                instr_type: InstrumentType::Euclidean(ekn),
                muted: false,
            });
            module.instrument.len() - 1
        }
    };

    // Collapse the track to a single prototype cell. From this point
    // on the row resolver (`Module::row_at`) recognises an Euclidean
    // wrapper and generates the per-row cell via the Bjorklund
    // pattern carried by the `InstrEkn` — playback time and length
    // are bounded by the referencing `Clip`'s `[position_tick,
    // end_tick)` range.
    let mut prototype = params.prototype;
    prototype.instrument = Some(new_instr_idx);
    let track = &mut module.tracks[track_idx];
    track.instrument = new_instr_idx;
    track.rows = vec![prototype];
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use alloc::vec;

    #[test]
    fn bjorklund_3_8_is_canonical() {
        // Toussaint's canonical (3,8) = positions 0, 3, 6.
        let p = euclidean_pattern(3, 8);
        assert_eq!(p, vec![true, false, false, true, false, false, true, false]);
    }

    #[test]
    fn bjorklund_5_8() {
        // (5, 8) = positions 0, 2, 3, 5, 6 — common cinquillo
        // arrangement (one of the canonical Bresenham forms).
        let p = euclidean_pattern(5, 8);
        assert_eq!(p.iter().filter(|&&b| b).count(), 5);
        assert!(p[0]);
    }

    fn play_cell(pitch: Pitch, instr: usize) -> TrackUnit {
        TrackUnit {
            note: CellNote::Play(pitch),
            instrument: Some(instr),
            ..TrackUnit::default()
        }
    }

    fn track_with_pulses(positions: &[usize], length: usize) -> Track {
        let mut rows: Vec<TrackUnit> = (0..length).map(|_| TrackUnit::default()).collect();
        for &p in positions {
            rows[p] = play_cell(Pitch::C4, 0);
        }
        Track {
            name: "t".into(),
            instrument: 0,
            rows,
            muted: false,
        }
    }

    #[test]
    fn detect_canonical_3_8_rotation_zero() {
        let track = track_with_pulses(&[0, 3, 6], 8);
        let params = detect_strict_euclidean(&track).expect("should match");
        assert_eq!(params.events, 3);
        assert_eq!(params.steps, 8);
        assert_eq!(params.rotation, 0);
    }

    #[test]
    fn detect_canonical_3_8_rotation_one() {
        // Pulses at [1, 4, 7] = canonical rotated right by 1.
        let track = track_with_pulses(&[1, 4, 7], 8);
        let params = detect_strict_euclidean(&track).expect("should match");
        assert_eq!(params.events, 3);
        assert_eq!(params.steps, 8);
        assert_eq!(params.rotation, 1);
    }

    #[test]
    fn reject_non_euclidean_pattern() {
        // Pulses at [0, 1, 2] — not evenly distributed.
        let track = track_with_pulses(&[0, 1, 2], 8);
        assert!(detect_strict_euclidean(&track).is_none());
    }

    #[test]
    fn reject_short_track() {
        // Length 3 < 4 minimum.
        let track = track_with_pulses(&[0, 2], 3);
        assert!(detect_strict_euclidean(&track).is_none());
    }

    #[test]
    fn reject_single_pulse() {
        // Only 1 event < 2 minimum.
        let track = track_with_pulses(&[0], 8);
        assert!(detect_strict_euclidean(&track).is_none());
    }

    #[test]
    fn reject_non_uniform_pitch() {
        let mut track = track_with_pulses(&[0, 3, 6], 8);
        track.rows[3].note = CellNote::Play(Pitch::D4); // different pitch
        assert!(detect_strict_euclidean(&track).is_none());
    }

    #[test]
    fn reject_per_cell_effect() {
        let mut track = track_with_pulses(&[0, 3, 6], 8);
        track.rows[0]
            .effects
            .push(TrackEffect::Arpeggio { half1: 4, half2: 7 });
        assert!(detect_strict_euclidean(&track).is_none());
    }

    #[test]
    fn reject_keyoff_cell() {
        let mut track = track_with_pulses(&[0, 3, 6], 8);
        track.rows[1].note = CellNote::KeyOff;
        assert!(detect_strict_euclidean(&track).is_none());
    }

    #[test]
    fn auto_convert_rewrites_track_and_cells() {
        let mut module = Module::default();
        module.instrument.push(Instrument {
            name: "snare".into(),
            instr_type: InstrumentType::Default(InstrDefault::default()),
            muted: false,
        });
        let track = track_with_pulses(&[0, 3, 6], 8);
        module.tracks.push(track);

        auto_convert_strict_euclidean_tracks(&mut module);

        // A new Euclidean wrapper has been added.
        assert_eq!(module.instrument.len(), 2);
        assert!(matches!(
            module.instrument[1].instr_type,
            InstrumentType::Euclidean(_)
        ));
        // The Track now points at the wrapper.
        assert_eq!(module.tracks[0].instrument, 1);
        // Every Play cell has been rewired to the wrapper.
        for cell in &module.tracks[0].rows {
            if matches!(cell.note, CellNote::Play(_)) {
                assert_eq!(cell.instrument, Some(1));
            }
        }
        // Wrapper's params reflect detection.
        if let InstrumentType::Euclidean(ekn) = &module.instrument[1].instr_type {
            assert_eq!(ekn.events, 3);
            assert_eq!(ekn.steps, 8);
            assert_eq!(ekn.rotation, 0);
            assert_eq!(ekn.instr, Some(0)); // wraps the original
        }
    }

    #[test]
    fn auto_convert_is_idempotent() {
        // A second pass skips tracks whose instrument is already
        // an Euclidean wrapper.
        let mut module = Module::default();
        module.instrument.push(Instrument {
            name: "snare".into(),
            instr_type: InstrumentType::Default(InstrDefault::default()),
            muted: false,
        });
        module.tracks.push(track_with_pulses(&[0, 3, 6], 8));

        auto_convert_strict_euclidean_tracks(&mut module);
        let after_first = module.instrument.len();
        let track_instr_first = module.tracks[0].instrument;

        auto_convert_strict_euclidean_tracks(&mut module);
        assert_eq!(module.instrument.len(), after_first, "no duplicate wrapper");
        assert_eq!(
            module.tracks[0].instrument, track_instr_first,
            "track unchanged"
        );
    }
}
