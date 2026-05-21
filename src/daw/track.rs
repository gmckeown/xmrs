//! Reusable sequence of cells assigned to a single instrument.
//!
//! Several [`crate::daw::clip::Clip`]s can point at the same `Track`
//! (shared link: editing the track affects every occurrence).
//! Tracks are split per instrument at import so that the
//! `instrument` field is invariant for the track's lifetime — a
//! change of instrument starts a new Track.

use crate::prelude::TrackUnit;
use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Track {
    pub name: String,
    /// Index into `Module.instrument`. Constant for the lifetime of
    /// this Track — cells don't carry their own instrument; they
    /// inherit it.
    ///
    /// Two special values:
    ///
    /// * [`EFFECT_ONLY_INSTRUMENT`](crate::daw::build_timeline::EFFECT_ONLY_INSTRUMENT)
    ///   — the track holds effect-only cells (vibrato, portamento,
    ///   …) intended to modify a voice inherited from a previous
    ///   pattern. The player skips the instrument load on those
    ///   cells and processes only the effects.
    /// * Index into an [`InstrumentType::Euclidean`](crate::instrument::InstrumentType::Euclidean)
    ///   — the track has been collapsed to a single prototype cell;
    ///   [`crate::module::Module::row_at`] generates the per-row
    ///   content from the wrapper's `(events, steps, rotation)`.
    pub instrument: usize,
    /// Cells with effect memory already resolved. Reuses
    /// [`TrackUnit`] so the player dispatches this content through
    /// the same effect machinery as any other source.
    ///
    /// `rows.len()` is the natural length of the Track, except for
    /// Euclidean-wrapped tracks where it's reduced to `1`
    /// (the prototype) — see the `instrument` field.
    pub rows: Vec<TrackUnit>,
    pub muted: bool,
}
