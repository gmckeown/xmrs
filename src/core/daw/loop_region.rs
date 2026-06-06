//! Per-lane looping for the DAW layer.
//!
//! Most tracker formats (MOD / XM / IT / S3M) share a single global
//! playback timeline: every channel advances in lockstep and the song
//! wraps as a whole via [`crate::core::module::Module::song_loop_to`].
//!
//! Some replayers don't work that way. The David Whittaker `.dw` driver
//! — and most custom C64 / Amiga sound engines — give **each voice its
//! own sequence with its own loop point**. The four voices of
//! `xenon2 (title).dw`, for instance, loop at 9888 / 9888 / 9912 / 10014
//! frames and drift against each other forever; the original hardware
//! never realigns them. A single global wrap can't represent that.
//!
//! A [`ChannelLoop`] expresses one voice's loop as a half-open tick
//! region `[start_tick, end_tick)` on its lane `(song, channel)`: when
//! that lane's local playhead reaches `end_tick` it wraps back to
//! `start_tick`, independently of the other lanes. This is the same
//! abstraction modern DAWs expose as a per-track loop region (the
//! Ableton / Bitwig clip-loop brace) — distinct per-track loop lengths
//! are exactly how polymeters are built.
//!
//! The loop lives on the **lane**, not on a [`crate::core::daw::track::Track`]
//! (reusable content, shared across clips) nor on a single
//! [`crate::core::daw::clip::Clip`] (a Whittaker voice's loop spans its
//! whole sequence of segment-clips, not one clip). The player honours
//! these regions in [`crate::core::module::Module::row_at`]; when a
//! module declares no `ChannelLoop`, behaviour is byte-for-byte the
//! global-wrap path, so the tracker formats are untouched.

use serde::{Deserialize, Serialize};

/// One voice's independent loop on its `(song, channel)` lane.
///
/// Ticks are in the same space as [`crate::core::daw::clip::Clip::position_tick`]
/// and [`crate::core::daw::timeline::TimelineEntry::tick`] — absolute
/// ticks from the start of the sub-song. The region is half-open:
/// playback covers `[start_tick, end_tick)` then wraps to `start_tick`.
///
/// Invariant: `start_tick < end_tick`. `start_tick` encodes the
/// driver's loop target (the position-list `loop_to`, `0` when the
/// voice loops from its very start, as every analysed `.dw` does so
/// far); `end_tick` is one tick past the voice's last rendered tick
/// (its pass length).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelLoop {
    /// Sub-song this lane belongs to (matches [`crate::core::daw::clip::Clip::song`]).
    pub song: u16,
    /// Physical channel this lane drives (matches
    /// [`crate::core::daw::clip::Clip::target_channel`]).
    pub channel: u8,
    /// First tick of the loop body (inclusive). The wrap target.
    pub start_tick: u32,
    /// One tick past the loop body (exclusive). Reaching this tick
    /// wraps the lane back to `start_tick`.
    pub end_tick: u32,
}

impl ChannelLoop {
    /// Length of the loop body in ticks (`end_tick - start_tick`).
    /// Always `> 0` for a well-formed region.
    pub fn len(&self) -> u32 {
        self.end_tick.saturating_sub(self.start_tick)
    }

    /// `true` for a degenerate region (`end_tick <= start_tick`), which
    /// violates the `start_tick < end_tick` invariant — such a region
    /// loops nothing and [`Self::fold_tick`] passes ticks through
    /// unchanged. Well-formed regions are never empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Map an absolute (possibly far-future) tick onto the
    /// corresponding tick inside the loop body. Ticks before
    /// `start_tick` (the lane's intro, if any) pass through unchanged;
    /// ticks at or past `start_tick` fold into `[start_tick, end_tick)`.
    ///
    /// This is the single operation the player applies before resolving
    /// which clip / cell a looping lane plays at a given playhead tick.
    pub fn fold_tick(&self, abs_tick: u32) -> u32 {
        let len = self.len();
        if len == 0 || abs_tick < self.start_tick {
            return abs_tick;
        }
        self.start_tick + (abs_tick - self.start_tick) % len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_tick_wraps_into_body() {
        // Loop body [10, 16) → length 6.
        let l = ChannelLoop {
            song: 0,
            channel: 0,
            start_tick: 10,
            end_tick: 16,
        };
        assert_eq!(l.len(), 6);
        // Before the body (intro) → unchanged.
        assert_eq!(l.fold_tick(0), 0);
        assert_eq!(l.fold_tick(9), 9);
        // Inside the first pass → identity.
        assert_eq!(l.fold_tick(10), 10);
        assert_eq!(l.fold_tick(15), 15);
        // Past the body → folds back.
        assert_eq!(l.fold_tick(16), 10); // 10 + (16-10)%6
        assert_eq!(l.fold_tick(17), 11);
        assert_eq!(l.fold_tick(22), 10); // two bodies later
        assert_eq!(l.fold_tick(28), 10);
    }

    #[test]
    fn fold_tick_zero_length_is_identity() {
        let l = ChannelLoop {
            song: 0,
            channel: 0,
            start_tick: 5,
            end_tick: 5,
        };
        assert_eq!(l.len(), 0);
        assert_eq!(l.fold_tick(7), 7);
    }
}
