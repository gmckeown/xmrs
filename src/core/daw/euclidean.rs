//! Strict Euclidean rhythm detection and auto-conversion at import
//! time.
//!
//! [`auto_convert_strict_euclidean_tracks`] is the last pass of
//! [`crate::tracker::import::build::build_timeline_layer`]. It scans
//! every [`Track::Notes`] and — for each one whose cell pattern is
//! bit-identical to a canonical Bjorklund(N, K) under some rotation
//! with uniform note, uniform velocity, and no per-cell effects —
//! collapses it into a [`Track::Euclidean`] carrying the same
//! instrument index and the detected Bjorklund parameters.
//!
//! The conversion is lossless: the row resolver expands the
//! Bjorklund pattern at playback time and emits the prototype on
//! each pulse position, matching the original cells row-for-row.
//! The added humanise parameters only matter when the user opts in
//! (`HumanizeMode::Live` / `Deterministic` + non-zero probability).

use alloc::vec::Vec;

use crate::core::cell::{Cell, CellEvent};
use crate::core::daw::track::Track;
use crate::core::fixed::fixed::Q15;
use crate::core::fixed::units::Volume;
use crate::core::module::Module;
use crate::core::pitch::Pitch;

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
    /// Uniform cell shared by every pulse of this rhythm. The
    /// instrument is implicit (it lives on the owning Track). The
    /// prototype's [`CellEvent`] is always a [`CellEvent::NoteOn`]
    /// (the strict criteria require every pulse to be a fresh
    /// trigger, never a ghost or release).
    pub prototype: Cell,
}

/// Walks every Track in `module` and, for each [`Track::Notes`]
/// strictly matching a Bjorklund pattern, rewrites it in place as a
/// [`Track::Euclidean`]. [`Track::Euclidean`] tracks are skipped
/// (idempotent).
pub fn auto_convert_strict_euclidean_tracks(module: &mut Module) {
    let n_tracks = module.tracks.len();
    for track_idx in 0..n_tracks {
        let params = match &module.tracks[track_idx] {
            Track::Notes { instrument, .. } => {
                // Skip tracks whose `instrument` doesn't point at a
                // real entry — either the sentinel
                // `EFFECT_ONLY_INSTRUMENT` carried by effect-only
                // tracks, or a malformed source that references an
                // instrument beyond the imported list.
                if module.instrument.get(*instrument).is_none() {
                    continue;
                }
                match detect_strict_euclidean(&module.tracks[track_idx]) {
                    Some(p) => p,
                    None => continue,
                }
            }
            Track::Euclidean { .. } => continue,
        };
        convert_track_in_place(&mut module.tracks[track_idx], params);
    }
}

/// `Some(params)` when the Track's cells form a strict Bjorklund
/// pattern with uniform pitch, uniform velocity, no per-cell effects,
/// length in `[4, 255]`, and at least 2 events. Returns `None`
/// otherwise (including on a [`Track::Euclidean`]).
pub fn detect_strict_euclidean(track: &Track) -> Option<EuclideanParams> {
    let rows = match track {
        Track::Notes { rows, .. } => rows,
        Track::Euclidean { .. } => return None,
    };
    let k = rows.len();
    if !(4..=u8::MAX as usize).contains(&k) {
        return None;
    }

    let mut positions: Vec<usize> = Vec::new();
    let mut first_pitch: Option<Pitch> = None;
    let mut first_velocity: Option<Volume> = None;

    for (i, cell) in rows.iter().enumerate() {
        if !cell.effects.is_empty() {
            return None;
        }
        match cell.event {
            CellEvent::None => continue,
            // Every pulse must be a fresh trigger of the track's
            // instrument — `NoteOn` is the only acceptable variant.
            // `NoteOnGhost` would imply pulses without an instrument
            // column and is also rejected (a strict Euclidean
            // rhythm wraps the *same* instrument on every step).
            CellEvent::NoteOn {
                pitch: p,
                velocity: v,
            } => {
                if let Some(fp) = first_pitch {
                    if fp != p {
                        return None;
                    }
                } else {
                    first_pitch = Some(p);
                }
                if let Some(fv) = first_velocity {
                    if fv != v {
                        return None;
                    }
                } else {
                    first_velocity = Some(v);
                }
                positions.push(i);
            }
            // Any other variant disqualifies the strict criterion.
            _ => return None,
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
            let prototype = Cell {
                event: CellEvent::NoteOn { pitch, velocity },
                effects: Vec::new(),
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

fn convert_track_in_place(track: &mut Track, params: EuclideanParams) {
    // Snapshot the common fields from the Notes variant, then
    // replace in place.
    let (name, instrument, muted) = match track {
        Track::Notes {
            name,
            instrument,
            muted,
            ..
        } => (core::mem::take(name), *instrument, *muted),
        Track::Euclidean { .. } => return,
    };

    *track = Track::Euclidean {
        name,
        instrument,
        muted,
        events: params.events,
        steps: params.steps,
        rotation: params.rotation,
        prototype: params.prototype,
        humanize_advance_max_ticks: 0,
        humanize_probability: Q15::ZERO,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use alloc::string::String;
    use alloc::vec;

    #[test]
    fn bjorklund_3_8_is_canonical() {
        // Toussaint's canonical (3,8) = positions 0, 3, 6.
        let p = euclidean_pattern(3, 8);
        assert_eq!(p, vec![true, false, false, true, false, false, true, false]);
    }

    #[test]
    fn bjorklund_5_8_bresenham_canonical() {
        // Bresenham(5, 8) — `T F T F T T F T`. The
        // detector tries every rotation so the cuban tresillo
        // `T F T T F T T F` is still recognised as a rotation of
        // this canonical form (cf. `detect_3_8_rotated`).
        let p = euclidean_pattern(5, 8);
        assert_eq!(p, vec![true, false, true, false, true, true, false, true]);
    }

    fn note_on_cell() -> Cell {
        Cell {
            event: CellEvent::NoteOn {
                pitch: Pitch::C5,
                velocity: Volume::FULL,
            },
            effects: vec![],
        }
    }

    fn track_with_pulses(positions: &[usize], length: usize) -> Track {
        let mut rows = vec![Cell::default(); length];
        for &p in positions {
            rows[p] = note_on_cell();
        }
        Track::Notes {
            name: String::new(),
            instrument: 0,
            rows,
            muted: false,
        }
    }

    #[test]
    fn detect_3_8_canonical() {
        let track = track_with_pulses(&[0, 3, 6], 8);
        let params = detect_strict_euclidean(&track).expect("should match");
        assert_eq!(params.events, 3);
        assert_eq!(params.steps, 8);
        assert_eq!(params.rotation, 0);
    }

    #[test]
    fn detect_3_8_rotated() {
        // Rotation by 2: positions are (0,3,6) shifted +2 → (2,5,0)
        let track = track_with_pulses(&[0, 2, 5], 8);
        let params = detect_strict_euclidean(&track).expect("should match");
        assert_eq!(params.events, 3);
        assert_eq!(params.steps, 8);
        assert_eq!(params.rotation, 2);
    }

    #[test]
    fn reject_non_euclidean_pattern() {
        // Two pulses at non-Bjorklund spacing of 8 cells.
        let track = track_with_pulses(&[0, 1], 8);
        assert!(detect_strict_euclidean(&track).is_none());
    }

    #[test]
    fn reject_under_4_rows() {
        let track = track_with_pulses(&[0, 2], 3);
        assert!(detect_strict_euclidean(&track).is_none());
    }

    #[test]
    fn reject_fewer_than_2_pulses() {
        let track = track_with_pulses(&[0], 8);
        assert!(detect_strict_euclidean(&track).is_none());
    }

    #[test]
    fn reject_non_uniform_pitch() {
        let mut track = track_with_pulses(&[0, 3, 6], 8);
        if let Track::Notes { rows, .. } = &mut track {
            rows[3].event = CellEvent::NoteOn {
                pitch: Pitch::D5,
                velocity: Volume::FULL,
            };
        }
        assert!(detect_strict_euclidean(&track).is_none());
    }

    #[test]
    fn reject_with_per_cell_effect() {
        let mut track = track_with_pulses(&[0, 3, 6], 8);
        if let Track::Notes { rows, .. } = &mut track {
            rows[0].effects.push(TrackEffect::Volume {
                value: Volume::FULL,
                tick: 0,
            });
        }
        assert!(detect_strict_euclidean(&track).is_none());
    }

    #[test]
    fn reject_with_note_off() {
        let mut track = track_with_pulses(&[0, 3, 6], 8);
        if let Track::Notes { rows, .. } = &mut track {
            rows[1].event = CellEvent::NoteOff { retrig: false };
        }
        assert!(detect_strict_euclidean(&track).is_none());
    }

    #[test]
    fn euclidean_variant_is_skipped() {
        let track = Track::Euclidean {
            name: String::new(),
            instrument: 0,
            muted: false,
            events: 3,
            steps: 8,
            rotation: 0,
            prototype: note_on_cell(),
            humanize_advance_max_ticks: 0,
            humanize_probability: Q15::ZERO,
        };
        assert!(detect_strict_euclidean(&track).is_none());
    }

    #[test]
    fn auto_convert_replaces_track_variant() {
        let mut module = Module::default();
        // Two instruments (one is the real sample) — auto-conversion
        // no longer touches the instrument bank.
        module.instrument.push(Instrument::default());
        module.instrument.push(Instrument::default());
        module.instrument[0].name = String::from("kick");
        // One Bjorklund(3,8) Notes track binding instrument 0.
        module.tracks.push(track_with_pulses(&[0, 3, 6], 8));

        auto_convert_strict_euclidean_tracks(&mut module);

        // The instrument bank is unchanged.
        assert_eq!(module.instrument.len(), 2);
        assert_eq!(module.instrument[0].name, "kick");
        // The track became a Euclidean variant binding the same
        // instrument.
        match &module.tracks[0] {
            Track::Euclidean {
                instrument,
                events,
                steps,
                rotation,
                prototype,
                ..
            } => {
                assert_eq!(*instrument, 0);
                assert_eq!(*events, 3);
                assert_eq!(*steps, 8);
                assert_eq!(*rotation, 0);
                assert!(matches!(prototype.event, CellEvent::NoteOn { .. }));
            }
            _ => panic!("track should be Euclidean after conversion"),
        }
    }

    #[test]
    fn auto_convert_is_idempotent() {
        let mut module = Module::default();
        module.instrument.push(Instrument::default());
        module.tracks.push(track_with_pulses(&[0, 3, 6], 8));

        auto_convert_strict_euclidean_tracks(&mut module);
        let snapshot_instr_count = module.instrument.len();
        let snapshot_track_instr = module.tracks[0].instrument();

        auto_convert_strict_euclidean_tracks(&mut module);

        assert_eq!(module.instrument.len(), snapshot_instr_count);
        assert_eq!(module.tracks[0].instrument(), snapshot_track_instr);
        assert!(module.tracks[0].is_euclidean());
    }
}
