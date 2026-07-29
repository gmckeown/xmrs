//! PCM extraction and the raw `PCM ` region (§4). Sample bytes live here — not
//! CBOR — so the region is a flat run of little-endian blobs, each padded to an
//! 8-byte boundary. A structural chunk (`INST` / `TRKS` / `ASET`) carries its
//! samples with the PCM stripped to `None` plus a list of [`PcmRef`] pointing
//! into this region.
//!
//! Two properties this layer buys that inline CBOR could not (§4):
//! - **Castable / compact.** Blobs are raw LE samples at 8-aligned offsets, so
//!   a host can `mmap` and cast `&[i16]` / `&[f32]` in place, and the bytes are
//!   exactly the sample data (no per-element integer tagging).
//! - **Dedup.** Blobs are interned by `Arc` identity, so the sharing the
//!   import-time dedup produced survives as one stored blob — and is restored
//!   on read (the resolver re-shares by offset), rather than expanding to N
//!   copies the way `serde` would.

use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::core::sample::SampleDataType;

use super::FormatError;

/// How to interpret a `PCM ` blob — the tag of [`SampleDataType`] without its
/// `Arc` payload.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcmKind {
    Mono8,
    Mono16,
    Stereo8,
    Stereo16,
    StereoFloat,
}

/// A blob's location in the `PCM ` region: how to read it, and its byte span
/// relative to the start of the region.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmRef {
    pub kind: PcmKind,
    pub offset: u32,
    pub len: u32,
}

/// A blob reference tied to an instrument sample: `instrument[i]`'s
/// `InstrDefault.sample[j]` (INST chunk).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstSampleRef {
    pub instrument: u32,
    pub sample: u32,
    pub pcm: PcmRef,
}

/// A blob reference tied to a `Track::Audio`'s inline sample: `tracks[t]`
/// (TRKS chunk).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackSampleRef {
    pub track: u32,
    pub pcm: PcmRef,
}

/// A blob reference tied to a shared asset: `assets[k]` (ASET chunk).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetSampleRef {
    pub asset: u32,
    pub pcm: PcmRef,
}

impl PcmKind {
    fn of(d: &SampleDataType) -> Self {
        match d {
            SampleDataType::Mono8(_) => PcmKind::Mono8,
            SampleDataType::Mono16(_) => PcmKind::Mono16,
            SampleDataType::Stereo8(_) => PcmKind::Stereo8,
            SampleDataType::Stereo16(_) => PcmKind::Stereo16,
            SampleDataType::StereoFloat(_) => PcmKind::StereoFloat,
        }
    }
}

/// The data-buffer address of a sample's `Arc`, used to dedup shared blobs.
/// Two clones of the same `Arc` yield the same address; two allocations that
/// are **both alive** never collide — which holds here because the write side
/// strips PCM out of *clones*, so the caller's `Module` keeps every `Arc` alive
/// for the whole of [`super::document::write`]. A freed buffer's address could
/// be reused by a later one, so this must not be cached across that boundary.
///
/// Not a key on its own: `Mono8`/`Stereo8` share `Arc<[i8]>` and
/// `Mono16`/`Stereo16` share `Arc<[i16]>`, so the same buffer can back two
/// different [`PcmKind`]s. See [`PcmRegion::intern`].
fn identity(d: &SampleDataType) -> usize {
    match d {
        SampleDataType::Mono8(v) | SampleDataType::Stereo8(v) => v.as_ptr() as usize,
        SampleDataType::Mono16(v) | SampleDataType::Stereo16(v) => v.as_ptr() as usize,
        SampleDataType::StereoFloat(v) => v.as_ptr() as usize,
    }
}

/// The raw little-endian bytes of a sample's PCM (§4: LE, integer-or-`f32`-bits,
/// no encoding).
fn to_le_bytes(d: &SampleDataType) -> Vec<u8> {
    let mut out = Vec::new();
    match d {
        SampleDataType::Mono8(v) | SampleDataType::Stereo8(v) => {
            out.extend(v.iter().map(|&x| x as u8));
        }
        SampleDataType::Mono16(v) | SampleDataType::Stereo16(v) => {
            for &x in v.iter() {
                out.extend_from_slice(&x.to_le_bytes());
            }
        }
        SampleDataType::StereoFloat(v) => {
            for &x in v.iter() {
                out.extend_from_slice(&x.to_le_bytes());
            }
        }
    }
    out
}

/// Reconstruct a [`SampleDataType`] from a blob's raw LE bytes.
fn from_le_bytes(kind: PcmKind, b: &[u8]) -> Result<SampleDataType, FormatError> {
    Ok(match kind {
        PcmKind::Mono8 => SampleDataType::Mono8(b.iter().map(|&x| x as i8).collect()),
        PcmKind::Stereo8 => SampleDataType::Stereo8(b.iter().map(|&x| x as i8).collect()),
        PcmKind::Mono16 | PcmKind::Stereo16 => {
            if !b.len().is_multiple_of(2) {
                return Err(FormatError::Pcm("16-bit blob length not a multiple of 2"));
            }
            let it = b.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]));
            match kind {
                PcmKind::Mono16 => SampleDataType::Mono16(it.collect()),
                _ => SampleDataType::Stereo16(it.collect()),
            }
        }
        PcmKind::StereoFloat => {
            if !b.len().is_multiple_of(4) {
                return Err(FormatError::Pcm("f32 blob length not a multiple of 4"));
            }
            SampleDataType::StereoFloat(
                b.chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect(),
            )
        }
    })
}

/// Accumulates unique PCM blobs into the raw `PCM ` region, deduplicating by
/// `Arc` identity and keeping every blob 8-byte aligned. Used at write time.
#[derive(Default)]
pub struct PcmRegion {
    bytes: Vec<u8>,
    // (identity, kind, ref) — linear; sample counts are small (tens/hundreds).
    seen: Vec<(usize, PcmKind, PcmRef)>,
}

impl PcmRegion {
    /// Intern a sample's PCM, returning its ref. An `Arc` already interned
    /// returns its existing ref (one stored blob for shared data).
    ///
    /// Keyed on buffer *and* [`PcmKind`], not the buffer alone: one
    /// `Arc<[i16]>` shared by a `Mono16` and a `Stereo16` is the same bytes but
    /// two different readings, and returning the first's ref would hand the
    /// second the wrong variant back on read. Same bytes, two refs — the region
    /// still stores the blob once.
    pub fn intern(&mut self, d: &SampleDataType) -> PcmRef {
        let id = identity(d);
        let kind = PcmKind::of(d);
        if let Some((_, _, r)) = self.seen.iter().find(|(i, k, _)| *i == id && *k == kind) {
            return *r;
        }
        if let Some((_, _, r)) = self.seen.iter().find(|(i, _, _)| *i == id) {
            // Same buffer, different reading: reuse the stored bytes, new ref.
            let r = PcmRef { kind, ..*r };
            self.seen.push((id, kind, r));
            return r;
        }
        let raw = to_le_bytes(d);
        let r = PcmRef {
            kind,
            offset: self.bytes.len() as u32,
            len: raw.len() as u32,
        };
        self.bytes.extend_from_slice(&raw);
        // Pad so the next blob starts 8-aligned; the region itself sits at an
        // 8-aligned chunk payload, so every blob ends up 8-aligned in the file.
        let pad = (8 - (self.bytes.len() % 8)) % 8;
        self.bytes.resize(self.bytes.len() + pad, 0);
        self.seen.push((id, kind, r));
        r
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Resolves [`PcmRef`]s against a `PCM ` region at read time. Refs sharing an
/// offset reconstruct the blob once and clone the `Arc`, so the write-side
/// dedup is restored as shared allocations rather than N copies.
pub struct PcmResolver<'a> {
    region: &'a [u8],
    // Keyed on (offset, kind) for the reason [`PcmRegion::intern`] is: one blob
    // can legitimately be read as two different [`PcmKind`]s, and a file from
    // anywhere but this writer can say so too.
    cache: Vec<((u32, PcmKind), SampleDataType)>,
}

impl<'a> PcmResolver<'a> {
    pub fn new(region: &'a [u8]) -> Self {
        Self {
            region,
            cache: Vec::new(),
        }
    }

    /// Reconstruct the sample data a ref points at.
    pub fn resolve(&mut self, r: PcmRef) -> Result<SampleDataType, FormatError> {
        if let Some((_, d)) = self.cache.iter().find(|(k, _)| *k == (r.offset, r.kind)) {
            return Ok(d.clone());
        }
        let start = r.offset as usize;
        let end = start
            .checked_add(r.len as usize)
            .ok_or(FormatError::Pcm("blob span overflows"))?;
        if end > self.region.len() {
            return Err(FormatError::Pcm("blob span past end of PCM region"));
        }
        let d = from_le_bytes(r.kind, &self.region[start..end])?;
        self.cache.push(((r.offset, r.kind), d.clone()));
        Ok(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;

    #[test]
    fn round_trips_each_kind_bit_exact() {
        let m8 = SampleDataType::Mono8(alloc::vec![-128i8, 0, 127].into());
        let m16 = SampleDataType::Mono16(alloc::vec![-32768i16, 0, 32767].into());
        let sf = SampleDataType::StereoFloat(alloc::vec![-1.0f32, 0.0, 0.5, f32::NAN].into());
        for d in [m8, m16, sf] {
            let mut region = PcmRegion::default();
            let r = region.intern(&d);
            let bytes = region.into_bytes();
            let mut res = PcmResolver::new(&bytes);
            let back = res.resolve(r).unwrap();
            // Compare raw LE bytes (NaN != NaN, so structural eq won't do).
            assert_eq!(to_le_bytes(&d), to_le_bytes(&back));
        }
    }

    #[test]
    fn shared_arc_interns_once() {
        let arc: Arc<[i16]> = alloc::vec![1i16, 2, 3, 4].into();
        let a = SampleDataType::Mono16(arc.clone());
        let b = SampleDataType::Mono16(arc); // same allocation
        let mut region = PcmRegion::default();
        let ra = region.intern(&a);
        let rb = region.intern(&b);
        assert_eq!(ra, rb, "shared Arc must intern to one blob");
        assert_eq!(
            region.into_bytes().len(),
            8,
            "one 8-byte blob, no duplicate"
        );
    }

    /// One buffer, two readings. `Mono16` and `Stereo16` are both `Arc<[i16]>`,
    /// so a shared allocation used as each is the same bytes but not the same
    /// sample: keyed on the pointer alone, the second would come back as the
    /// first's variant.
    #[test]
    fn one_buffer_read_two_ways_keeps_both_kinds() {
        let arc: Arc<[i16]> = alloc::vec![1i16, 2, 3, 4].into();
        let mono = SampleDataType::Mono16(arc.clone());
        let stereo = SampleDataType::Stereo16(arc);

        let mut region = PcmRegion::default();
        let rm = region.intern(&mono);
        let rs = region.intern(&stereo);
        assert_eq!(rm.offset, rs.offset, "one shared buffer, one stored blob");
        assert_eq!((rm.kind, rs.kind), (PcmKind::Mono16, PcmKind::Stereo16));
        assert_eq!(region.bytes.len(), 8, "no duplicate copy of the payload");

        let bytes = region.into_bytes();
        let mut res = PcmResolver::new(&bytes);
        assert!(matches!(
            res.resolve(rm).unwrap(),
            SampleDataType::Mono16(_)
        ));
        assert!(matches!(
            res.resolve(rs).unwrap(),
            SampleDataType::Stereo16(_)
        ));
    }

    #[test]
    fn distinct_blobs_are_eight_aligned() {
        let a = SampleDataType::Mono8(alloc::vec![1i8, 2, 3].into()); // 3 bytes → pad to 8
        let b = SampleDataType::Mono8(alloc::vec![4i8, 5].into());
        let mut region = PcmRegion::default();
        let _ = region.intern(&a);
        let rb = region.intern(&b);
        assert_eq!(rb.offset, 8, "second blob starts 8-aligned after padding");
    }
}
