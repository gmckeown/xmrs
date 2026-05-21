//! Sort-preserving container for [`Clip`].
//!
//! The DAW layer's row-resolution and overlap-detection paths
//! binary-search the clips by `(song, target_channel)` lane and
//! then by `position_tick`. Any mutation that doesn't preserve the
//! `(song, target_channel, position_tick)` ascending order would
//! silently corrupt those lookups.
//!
//! [`SortedClips`] is the only writer for [`crate::module::Module::clips`].
//! The inner `Vec<Clip>` is private; every mutator goes through an
//! API that either preserves or re-establishes the invariant. Read
//! access is unrestricted: callers can iterate, index, slice, or
//! `partition_point` on `as_slice()`.

use alloc::vec::Vec;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::daw::clip::Clip;

/// `Vec<Clip>` with a maintained `(song, target_channel,
/// position_tick)` ascending order.
/// # Example
///
/// ```
/// use xmrs::daw::clip::Clip;
/// use xmrs::daw::sorted_clips::SortedClips;
///
/// let mk = |song, ch, tick| Clip {
///     track: 0,
///     song,
///     target_channel: ch,
///     position_tick: tick,
///     speed_at_start: 6,
///     track_row_offset: 0,
///     source_start_row: 0,
///     end_tick: tick + 6,
/// };
///
/// let mut sc = SortedClips::from_unsorted(vec![
///     mk(0, 1, 10),
///     mk(0, 0, 20),
///     mk(0, 0, 0),
/// ]);
///
/// // The contiguous `(0, 0)` lane is reachable by binary range.
/// let lane = sc.lane(0, 0);
/// assert_eq!(lane.len(), 2);
/// assert_eq!(lane[0].position_tick, 0);
/// assert_eq!(lane[1].position_tick, 20);
///
/// // The currently active clip on `(0, 0)` at tick 15.
/// let (_idx, clip) = sc.active_at(0, 0, 15).unwrap();
/// assert_eq!(clip.position_tick, 0);
///
/// // Mutate in place — the sort invariant is preserved automatically.
/// sc.modify(0, |c| c.position_tick = 100);
/// // Inserting takes care of finding the right slot.
/// let idx = sc.insert(mk(0, 0, 5));
/// assert_eq!(idx, 0);
/// ```
#[derive(Debug, Clone, Default)]
pub struct SortedClips {
    inner: Vec<Clip>,
}

impl SortedClips {
    pub fn new() -> Self {
        Self { inner: Vec::new() }
    }

    /// Build a `SortedClips` by sorting `clips` in place. Use this
    /// from importers / pipelines that emit clips in arbitrary
    /// order.
    pub fn from_unsorted(mut clips: Vec<Clip>) -> Self {
        Self::sort_in_place(&mut clips);
        Self { inner: clips }
    }

    fn sort_in_place(clips: &mut [Clip]) {
        clips.sort_by(|a, b| {
            (a.song, a.target_channel, a.position_tick).cmp(&(
                b.song,
                b.target_channel,
                b.position_tick,
            ))
        });
    }

    fn key(c: &Clip) -> (u16, u8, u32) {
        (c.song, c.target_channel, c.position_tick)
    }

    /// Number of stored clips.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Borrow the underlying slice. Useful for binary-search-based
    /// queries via `partition_point`.
    pub fn as_slice(&self) -> &[Clip] {
        &self.inner
    }

    pub fn iter(&self) -> core::slice::Iter<'_, Clip> {
        self.inner.iter()
    }

    pub fn get(&self, idx: usize) -> Option<&Clip> {
        self.inner.get(idx)
    }

    /// Contiguous slice of clips assigned to the `(song,
    /// target_channel)` lane. Empty when the lane has no clips.
    pub fn lane(&self, song: u16, target_channel: u8) -> &[Clip] {
        let range = self.lane_range(song, target_channel);
        &self.inner[range]
    }

    /// Same as [`Self::lane`] but returns the slice **range** in
    /// the underlying storage. Useful when callers need to compute
    /// global indices into the collection from local offsets.
    pub fn lane_range(&self, song: u16, target_channel: u8) -> core::ops::Range<usize> {
        let key = (song, target_channel);
        let lo = self
            .inner
            .partition_point(|c| (c.song, c.target_channel) < key);
        let hi = self
            .inner
            .partition_point(|c| (c.song, c.target_channel) <= key);
        lo..hi
    }

    /// Greatest-`position_tick` clip whose `position_tick ≤ tick`
    /// on the `(song, target_channel)` lane. Returns the clip and
    /// its global index in `self`.
    pub fn active_at(&self, song: u16, target_channel: u8, tick: u32) -> Option<(usize, &Clip)> {
        let key = (song, target_channel);
        let lane_start = self
            .inner
            .partition_point(|c| (c.song, c.target_channel) < key);
        let lane_end = self
            .inner
            .partition_point(|c| (c.song, c.target_channel) <= key);
        let lane = &self.inner[lane_start..lane_end];
        let upper = lane.partition_point(|c| c.position_tick <= tick);
        if upper == 0 {
            None
        } else {
            let local = upper - 1;
            Some((lane_start + local, &lane[local]))
        }
    }

    /// Insert `clip` at the position dictated by the sort key.
    /// Returns the insertion index.
    pub fn insert(&mut self, clip: Clip) -> usize {
        let key = Self::key(&clip);
        let pos = self.inner.partition_point(|c| Self::key(c) <= key);
        self.inner.insert(pos, clip);
        pos
    }

    /// Remove and return the clip at `idx`. Sort order is preserved
    /// because `Vec::remove` shifts subsequent elements down.
    pub fn remove(&mut self, idx: usize) -> Clip {
        self.inner.remove(idx)
    }

    /// Mutate one clip via `f`, then restore the sort invariant.
    /// `f` may freely change any field including the sort keys —
    /// the resort is automatic.
    pub fn modify(&mut self, idx: usize, f: impl FnOnce(&mut Clip)) {
        if let Some(clip) = self.inner.get_mut(idx) {
            f(clip);
            Self::sort_in_place(&mut self.inner);
        }
    }

    /// Apply `f` to every clip then resort once. Cheaper than
    /// calling [`Self::modify`] in a loop when the closure rewrites
    /// many clips (e.g. shifting `clip.track` after a track
    /// removal).
    pub fn modify_all(&mut self, mut f: impl FnMut(&mut Clip)) {
        for clip in self.inner.iter_mut() {
            f(clip);
        }
        Self::sort_in_place(&mut self.inner);
    }

    /// Replace the entire collection by sorting `clips`. Equivalent
    /// to `*self = Self::from_unsorted(clips)` but avoids a
    /// `mem::replace`.
    pub fn set_from_unsorted(&mut self, clips: Vec<Clip>) {
        self.inner = clips;
        Self::sort_in_place(&mut self.inner);
    }

    /// Take a snapshot of the underlying Vec for undo storage. The
    /// snapshot is guaranteed sorted because the invariant always
    /// holds on `self`.
    pub fn snapshot(&self) -> Vec<Clip> {
        self.inner.clone()
    }

    /// Restore from a snapshot previously obtained via
    /// [`Self::snapshot`]. The snapshot is trusted to be sorted; in
    /// debug builds the assertion catches a corrupted snapshot.
    pub fn restore(&mut self, snapshot: Vec<Clip>) {
        debug_assert!(
            Self::is_sorted_slice(&snapshot),
            "restored snapshot must satisfy the canonical sort"
        );
        self.inner = snapshot;
    }

    fn is_sorted_slice(clips: &[Clip]) -> bool {
        clips
            .windows(2)
            .all(|w| Self::key(&w[0]) <= Self::key(&w[1]))
    }
}

impl<'a> IntoIterator for &'a SortedClips {
    type Item = &'a Clip;
    type IntoIter = core::slice::Iter<'a, Clip>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl core::ops::Index<usize> for SortedClips {
    type Output = Clip;
    fn index(&self, idx: usize) -> &Clip {
        &self.inner[idx]
    }
}

// `Serialize` writes the inner Vec as-is. `Deserialize` re-sorts so
// a serde sink that drops the invariant (legacy data, hand-crafted
// JSON) round-trips into a valid `SortedClips`.
impl Serialize for SortedClips {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.inner.serialize(s)
    }
}

impl<'de> Deserialize<'de> for SortedClips {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Vec::<Clip>::deserialize(d).map(Self::from_unsorted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn c(song: u16, ch: u8, tick: u32) -> Clip {
        Clip {
            track: 0,
            song,
            target_channel: ch,
            position_tick: tick,
            speed_at_start: 6,
            track_row_offset: 0,
            source_start_row: 0,
            end_tick: tick + 1,
        }
    }

    fn keys(clips: &SortedClips) -> Vec<(u16, u8, u32)> {
        clips.iter().map(SortedClips::key).collect()
    }

    #[test]
    fn from_unsorted_orders_by_song_then_channel_then_tick() {
        let v = vec![c(1, 0, 0), c(0, 1, 10), c(0, 0, 20), c(0, 0, 0), c(0, 1, 0)];
        let sc = SortedClips::from_unsorted(v);
        assert_eq!(
            keys(&sc),
            vec![(0, 0, 0), (0, 0, 20), (0, 1, 0), (0, 1, 10), (1, 0, 0)]
        );
    }

    #[test]
    fn insert_finds_correct_position() {
        let mut sc = SortedClips::from_unsorted(vec![c(0, 0, 0), c(0, 0, 20)]);
        let idx = sc.insert(c(0, 0, 10));
        assert_eq!(idx, 1);
        assert_eq!(keys(&sc), vec![(0, 0, 0), (0, 0, 10), (0, 0, 20)]);
    }

    #[test]
    fn lane_returns_only_matching_clips() {
        let sc = SortedClips::from_unsorted(vec![c(0, 0, 0), c(0, 1, 0), c(0, 0, 10), c(0, 1, 10)]);
        let lane0 = sc.lane(0, 0);
        assert_eq!(lane0.len(), 2);
        assert!(lane0.iter().all(|c| c.song == 0 && c.target_channel == 0));
    }

    #[test]
    fn active_at_picks_greatest_tick_le_query() {
        let sc = SortedClips::from_unsorted(vec![c(0, 0, 0), c(0, 0, 10), c(0, 0, 20)]);
        let (idx, clip) = sc.active_at(0, 0, 15).unwrap();
        assert_eq!(idx, 1);
        assert_eq!(clip.position_tick, 10);
        assert!(sc.active_at(0, 0, u32::MAX).is_some());
        assert!(sc.active_at(0, 1, 0).is_none());
    }

    #[test]
    fn modify_resorts_after_key_change() {
        let mut sc = SortedClips::from_unsorted(vec![c(0, 0, 0), c(0, 0, 10)]);
        sc.modify(0, |c| c.position_tick = 100);
        // The mutated clip should be at the end now.
        assert_eq!(sc.get(1).unwrap().position_tick, 100);
    }

    #[test]
    fn modify_all_resorts_once() {
        let mut sc = SortedClips::from_unsorted(vec![c(0, 0, 0), c(0, 1, 0)]);
        sc.modify_all(|c| c.target_channel = 1 - c.target_channel);
        // Channels swapped; sort re-established.
        assert_eq!(keys(&sc), vec![(0, 0, 0), (0, 1, 0)]);
    }

    #[test]
    fn remove_preserves_sort() {
        let mut sc = SortedClips::from_unsorted(vec![c(0, 0, 0), c(0, 0, 10), c(0, 0, 20)]);
        sc.remove(1);
        assert_eq!(keys(&sc), vec![(0, 0, 0), (0, 0, 20)]);
    }

    #[test]
    fn snapshot_and_restore_round_trips() {
        let sc = SortedClips::from_unsorted(vec![c(0, 0, 0), c(0, 1, 10)]);
        let snap = sc.snapshot();
        let mut sc2 = SortedClips::new();
        sc2.restore(snap);
        assert_eq!(keys(&sc2), vec![(0, 0, 0), (0, 1, 10)]);
    }
}
