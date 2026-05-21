//! Placement of a [`crate::daw::track::Track`] on a sub-song's
//! timeline. Clips never overlap on the same `(song,
//! target_channel)`: the player's state machine assumes one voice
//! per physical channel at a time.

use serde::{Deserialize, Serialize};

use crate::daw::track::Track;
use crate::module::Module;

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct Clip {
    /// Index into `Module.tracks`.
    pub track: u32,
    /// Sub-song identifier (index into `pattern_order` historically).
    pub song: u16,
    /// Physical channel this clip drives on the player's state
    /// machine. Inherited from the source pattern channel at import.
    pub target_channel: u8,
    /// Absolute tick from the start of the sub-song.
    pub position_tick: u32,
    /// Speed (ticks per row) at the moment the clip starts. UI hint
    /// only — the player reads the effective speed from
    /// `GlobalEffect::Speed` events in the cell stream.
    pub speed_at_start: u8,
    /// Row offset into the Track at which this Clip begins playing.
    /// `0` means "start from the Track's first row". Non-zero values
    /// are produced by importers when a `PatternBreak` enters a
    /// pattern past the start of the matching track segment.
    pub track_row_offset: u32,
    /// Source-pattern row corresponding to `track.rows[0]` for this
    /// clip. The cell at source row `R` lives at
    /// `track.rows[R - source_start_row]`. Lets row resolution stay
    /// independent of effective speed inside the clip — a
    /// `GlobalEffect::Speed` that mutates the per-row tick spacing
    /// no longer aliases two rows onto the same track index.
    pub source_start_row: u32,
    /// Absolute tick at which this clip stops playing (exclusive).
    /// Computed at clip creation from the [`crate::daw::timeline::TimelineMap`]
    /// so a `GlobalEffect::Speed` change inside the clip is folded
    /// in correctly. `verify_layers_consistent` and edit-time overlap
    /// checks consume this directly — no per-row tick math needed.
    pub end_tick: u32,
}

impl Clip {
    /// Resolve the [`Track`] this clip points at, if any.
    pub fn track_in<'a>(&self, module: &'a Module) -> Option<&'a Track> {
        module.tracks.get(self.track as usize)
    }

    /// Convenience: `module` is currently unused but the signature
    /// is preserved so the call sites stay future-proof if the
    /// stored value ever needs cross-checking against the timeline.
    pub fn end_tick(&self, _module: &Module) -> u32 {
        self.end_tick
    }
}
