//! Content-dedup pass: fuse Tracks whose `(instrument, rows)` match
//! bit-for-bit so identical patterns in the source aren't replicated
//! across the Track list. Cycle-exact: the player consumes the same
//! Track contents whether or not dedup ran.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use crate::core::cell::Cell;
use crate::core::daw::track::Track;
use crate::core::module::Module;

/// Fuse Tracks with bit-identical (instrument, rows) into a single
/// representative, rewriting `Clip.track` indices accordingly.
pub fn dedupe_tracks_by_content(module: &mut Module) {
    if module.tracks.is_empty() {
        return;
    }

    let mut buckets: BTreeMap<u64, Vec<u32>> = BTreeMap::new();
    for (idx, track) in module.tracks.iter().enumerate() {
        buckets
            .entry(hash_track(track))
            .or_default()
            .push(idx as u32);
    }

    let mut remap: BTreeMap<u32, u32> = BTreeMap::new();
    let mut to_delete: BTreeSet<u32> = BTreeSet::new();

    for group in buckets.values() {
        if group.len() < 2 {
            continue;
        }
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

    module.clips.modify_all(|clip| {
        if let Some(&rep) = remap.get(&clip.track) {
            clip.track = rep;
        }
    });

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
            final_index[i as usize] = u32::MAX;
        } else {
            final_index[i as usize] = i - shift;
        }
    }
    let final_index_snapshot = final_index.clone();
    module.clips.modify_all(|clip| {
        let cur = clip.track as usize;
        if cur < final_index_snapshot.len() && final_index_snapshot[cur] != u32::MAX {
            clip.track = final_index_snapshot[cur];
        }
    });

    for &idx in to_delete_vec.iter().rev() {
        module.tracks.remove(idx as usize);
    }
}

fn hash_track(track: &Track) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    match track {
        Track::Notes {
            instrument, rows, ..
        } => {
            fnv_mix_byte(&mut h, 0);
            fnv_mix_usize(&mut h, *instrument);
            fnv_mix_usize(&mut h, rows.len());
            for cell in rows {
                hash_cell(&mut h, cell);
            }
        }
        Track::Euclidean {
            instrument,
            events,
            steps,
            rotation,
            prototype,
            humanize_advance_max_ticks,
            humanize_probability,
            ..
        } => {
            fnv_mix_byte(&mut h, 1);
            fnv_mix_usize(&mut h, *instrument);
            fnv_mix_byte(&mut h, *events);
            fnv_mix_byte(&mut h, *steps);
            fnv_mix_byte(&mut h, *rotation);
            fnv_mix_byte(&mut h, *humanize_advance_max_ticks);
            let raw = humanize_probability.raw().to_le_bytes();
            for b in raw {
                fnv_mix_byte(&mut h, b);
            }
            hash_cell(&mut h, prototype);
        }
        Track::Audio { source, .. } => {
            use crate::core::daw::track::AudioSource;
            fnv_mix_byte(&mut h, 2);
            match source {
                AudioSource::Inline(sample) => {
                    fnv_mix_byte(&mut h, 0);
                    fnv_mix_usize(&mut h, sample.len());
                }
                AudioSource::Pooled(i) => {
                    fnv_mix_byte(&mut h, 1);
                    fnv_mix_usize(&mut h, *i);
                }
            }
        }
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

fn hash_cell(h: &mut u64, cell: &Cell) {
    use crate::core::cell::CellEvent;

    match cell.event {
        CellEvent::None => fnv_mix_byte(h, 0),
        CellEvent::NoteOn { pitch, velocity } => {
            fnv_mix_byte(h, 1);
            fnv_mix_u32(h, pitch.value() as u32);
            fnv_mix_u32(h, velocity.to_byte_64() as u32);
        }
        CellEvent::NoteOnGhost { pitch, velocity } => {
            fnv_mix_byte(h, 2);
            fnv_mix_u32(h, pitch.value() as u32);
            fnv_mix_u32(h, velocity.to_byte_64() as u32);
        }
        CellEvent::InstrReset => fnv_mix_byte(h, 3),
        CellEvent::NoteOff { retrig: false } => fnv_mix_byte(h, 4),
        CellEvent::NoteOff { retrig: true } => fnv_mix_byte(h, 5),
        CellEvent::NoteCut => fnv_mix_byte(h, 6),
        CellEvent::NoteFade => fnv_mix_byte(h, 7),
    }

    fnv_mix_u32(h, cell.effects.len() as u32);
    // Effect discriminants are enough to bucket cells; effect-level
    // equality is re-checked by `cells_equal` so false collisions
    // here are cheap.
    for fx in &cell.effects {
        use core::hash::Hash;
        let mut sub = FnvHasher(*h);
        core::mem::discriminant(fx).hash(&mut sub);
        *h = sub.0;
    }
}

struct FnvHasher(u64);
impl core::hash::Hasher for FnvHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }
}

fn tracks_equal_content(a: &Track, b: &Track) -> bool {
    match (a, b) {
        (
            Track::Notes {
                instrument: ia,
                rows: ra,
                ..
            },
            Track::Notes {
                instrument: ib,
                rows: rb,
                ..
            },
        ) => {
            if ia != ib || ra.len() != rb.len() {
                return false;
            }
            for (ua, ub) in ra.iter().zip(rb.iter()) {
                if !cells_equal(ua, ub) {
                    return false;
                }
            }
            true
        }
        (
            Track::Euclidean {
                instrument: ia,
                events: ea,
                steps: sa,
                rotation: rota,
                prototype: pa,
                humanize_advance_max_ticks: ha,
                humanize_probability: pra,
                ..
            },
            Track::Euclidean {
                instrument: ib,
                events: eb,
                steps: sb,
                rotation: rotb,
                prototype: pb,
                humanize_advance_max_ticks: hb,
                humanize_probability: prb,
                ..
            },
        ) => {
            ia == ib
                && ea == eb
                && sa == sb
                && rota == rotb
                && ha == hb
                && pra == prb
                && cells_equal(pa, pb)
        }
        _ => false,
    }
}

fn cells_equal(a: &Cell, b: &Cell) -> bool {
    a == b
}
