//! Reusable sequence assigned to a single instrument.
//!
//! Several [`crate::core::daw::clip::Clip`]s can point at the same `Track`
//! (shared link: editing the track affects every occurrence).
//! Tracks are split per instrument at import so that the `instrument`
//! field is invariant for the track's lifetime — a change of instrument
//! starts a new Track.
//!
//! A Track has one of two shapes:
//!
//! - [`Track::Notes`] — a vector of [`Cell`]s, one per row. The
//!   classic tracker representation; covers manual editing and every
//!   format importer's output.
//! - [`Track::Euclidean`] — a generator described by Bjorklund
//!   `(events, steps, rotation)` plus a single prototype cell. The
//!   row resolver synthesises one cell per playback row from the
//!   Bjorklund pattern.

use crate::core::cell::Cell;
use crate::core::fixed::fixed::Q15;
use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};

/// One reusable cell sequence in the DAW layer.
///
/// Tracks are placed on the timeline through
/// [`Clip`](crate::core::daw::clip::Clip)s — one Track can be played
/// multiple times on multiple channels by binding multiple clips to it.
/// The instrument index is fixed for the Track's lifetime: a change of
/// instrument produces a new Track.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Track {
    /// Classic per-row cell sequence — one [`Cell`] per playback row.
    /// Produced by every format importer (XM / MOD / S3M / IT / SID)
    /// and by manual editing.
    Notes {
        name: String,
        /// Index into `Module.instrument`. Constant for the lifetime
        /// of this Track.
        ///
        /// Special value:
        /// [`EFFECT_ONLY_INSTRUMENT`](crate::tracker::import::build::EFFECT_ONLY_INSTRUMENT)
        /// marks a track that holds effect-only cells (vibrato,
        /// portamento, …) intended to modify a voice inherited from
        /// a previous pattern. The player processes only the effects.
        instrument: usize,
        /// Cells with effect memory already resolved.
        rows: Vec<Cell>,
        muted: bool,
    },
    /// Euclidean (Bjorklund) rhythm generator. The row resolver
    /// emits `prototype` on each pulse position of the canonical
    /// Bjorklund(`events`, `steps`) pattern rotated right by
    /// `rotation`, and an empty cell on the off-pulses. The track
    /// loops the pattern for the duration of every clip that
    /// references it.
    ///
    /// The Bjorklund parameters and the prototype were either
    /// authored directly in the editor or produced by the
    /// auto-conversion pass on import (a strict Bjorklund
    /// `Track::Notes` collapses to this form, lossless).
    Euclidean {
        name: String,
        /// Index into `Module.instrument`. Same constancy contract
        /// as [`Self::Notes::instrument`].
        instrument: usize,
        muted: bool,
        /// Number of pulses (`k` in Bjorklund(k, n) literature).
        events: u8,
        /// Length of one rhythm cycle in track rows
        /// (`n` in Bjorklund(k, n)).
        steps: u8,
        /// Right-rotation of the canonical Bresenham pattern.
        /// `0` = canonical (pulse on row 0).
        rotation: u8,
        /// Uniform cell rendered on every pulse position. Always a
        /// [`CellEvent::NoteOn`](crate::core::cell::CellEvent::NoteOn)
        /// in practice (the conversion pass requires fresh triggers).
        prototype: Cell,
        /// Maximum advance (in ticks) randomly applied to each
        /// pulse by the player when humanisation is on. `0` means
        /// no humanisation — cycle-exact strict.
        humanize_advance_max_ticks: u8,
        /// Probability that a given pulse is advanced.
        /// `Q15::ZERO` = never; `Q15::ONE` = every pulse. Drawn
        /// independently per pulse by the player.
        humanize_probability: Q15,
    },
}

impl Default for Track {
    fn default() -> Self {
        Track::Notes {
            name: String::new(),
            instrument: 0,
            rows: Vec::new(),
            muted: false,
        }
    }
}

impl Track {
    /// Display name, regardless of variant.
    #[inline]
    pub fn name(&self) -> &str {
        match self {
            Track::Notes { name, .. } | Track::Euclidean { name, .. } => name,
        }
    }

    /// Mutable reference to the display name.
    #[inline]
    pub fn name_mut(&mut self) -> &mut String {
        match self {
            Track::Notes { name, .. } | Track::Euclidean { name, .. } => name,
        }
    }

    /// Instrument index this track binds to.
    #[inline]
    pub fn instrument(&self) -> usize {
        match self {
            Track::Notes { instrument, .. } | Track::Euclidean { instrument, .. } => *instrument,
        }
    }

    /// Set the instrument index. Use sparingly — the field is
    /// expected to be constant for the lifetime of the track.
    #[inline]
    pub fn set_instrument(&mut self, idx: usize) {
        match self {
            Track::Notes { instrument, .. } | Track::Euclidean { instrument, .. } => {
                *instrument = idx;
            }
        }
    }

    /// Per-track mute flag.
    #[inline]
    pub fn muted(&self) -> bool {
        match self {
            Track::Notes { muted, .. } | Track::Euclidean { muted, .. } => *muted,
        }
    }

    /// Set the mute flag.
    #[inline]
    pub fn set_muted(&mut self, m: bool) {
        match self {
            Track::Notes { muted, .. } | Track::Euclidean { muted, .. } => {
                *muted = m;
            }
        }
    }

    /// `true` if this is a [`Self::Euclidean`] variant.
    #[inline]
    pub fn is_euclidean(&self) -> bool {
        matches!(self, Track::Euclidean { .. })
    }

    /// Direct view on the per-row cell storage — `Some` only for
    /// [`Self::Notes`]. Generator variants synthesise cells via
    /// [`Self::cell_at`] instead.
    #[inline]
    pub fn rows(&self) -> Option<&[Cell]> {
        match self {
            Track::Notes { rows, .. } => Some(rows),
            Track::Euclidean { .. } => None,
        }
    }

    /// Mutable access to the per-row cell storage — `Some` only for
    /// [`Self::Notes`]. Cell-editing commands gate on this returning
    /// `Some`; on a generator variant the editor must first convert
    /// to [`Self::Notes`] (expansion of the rhythm into explicit
    /// cells) before editing rows.
    #[inline]
    pub fn rows_mut(&mut self) -> Option<&mut Vec<Cell>> {
        match self {
            Track::Notes { rows, .. } => Some(rows),
            Track::Euclidean { .. } => None,
        }
    }

    /// Like [`Self::rows`] but panics if the track is not
    /// [`Self::Notes`]. Test-helper for code paths that know they
    /// just created a Notes track.
    #[doc(hidden)]
    #[track_caller]
    pub fn expect_notes_rows(&self) -> &[Cell] {
        self.rows().expect("Track is not Track::Notes")
    }

    /// Like [`Self::rows_mut`] but panics if the track is not
    /// [`Self::Notes`].
    #[doc(hidden)]
    #[track_caller]
    pub fn expect_notes_rows_mut(&mut self) -> &mut Vec<Cell> {
        self.rows_mut().expect("Track is not Track::Notes")
    }

    /// Cell visible at row index `row` after expansion of any
    /// generator variant. For [`Self::Notes`] this is just
    /// `rows.get(row).cloned().unwrap_or_default()`. For
    /// [`Self::Euclidean`] the Bjorklund pattern is applied: the
    /// prototype is returned on pulse positions, `Cell::default()`
    /// elsewhere. The Euclidean rhythm loops every `steps` rows.
    pub fn cell_at(&self, row: u32) -> Cell {
        match self {
            Track::Notes { rows, .. } => rows.get(row as usize).cloned().unwrap_or_default(),
            Track::Euclidean {
                events,
                steps,
                rotation,
                prototype,
                ..
            } => {
                let steps_u32 = (*steps).max(1) as u32;
                let pos = row % steps_u32;
                let rot = (*rotation as u32) % steps_u32;
                let bjork_idx = ((pos + steps_u32 - rot) % steps_u32) as usize;
                let canonical = crate::core::daw::euclidean::euclidean_pattern(*events, *steps);
                if canonical.get(bjork_idx).copied().unwrap_or(false) {
                    prototype.clone()
                } else {
                    Cell::default()
                }
            }
        }
    }

    /// Natural playback length of the track in rows. For
    /// [`Self::Notes`] this is `rows.len()`. For [`Self::Euclidean`]
    /// the pattern loops; this returns the *cycle length* (`steps`).
    pub fn natural_length(&self) -> usize {
        match self {
            Track::Notes { rows, .. } => rows.len(),
            Track::Euclidean { steps, .. } => *steps as usize,
        }
    }
}
