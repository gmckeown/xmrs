//! The container grammar of §2: a 16-byte header followed by a stream of
//! `[id:4][len:u32][payload][pad→8]` chunks. This layer is pure byte-shuffling
//! — it knows the framing, the version fields, and the alignment invariant,
//! but nothing about `Module` or CBOR. The wire layer builds on top.
//!
//! **Alignment invariant.** The header is 16 bytes (8-aligned), and every
//! chunk is `id(4)+len(4)=8` of framing plus a payload padded up to 8. So
//! every chunk *starts* 8-aligned and every *payload* starts 8-aligned — the
//! property §4 leans on so a `PCM ` blob of `i16`/`f32` can be cast in place.

use alloc::vec::Vec;

use super::{align_up, ChunkId, FormatError, CONTAINER_VERSION, MAGIC, SCHEMA_VERSION};

/// Length of the fixed header, in bytes.
pub const HEADER_LEN: usize = 16;

/// The parsed 16-byte header (§2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Grammar version of the container (`== CONTAINER_VERSION` for files this
    /// build writes).
    pub container_version: u16,
    /// Reserved; always 0 in a well-formed file.
    pub flags: u16,
    /// What this file *is* — the migration clock (§6).
    pub schema_version: u32,
    /// The oldest reader that can interpret this file correctly (§6).
    pub min_reader_version: u32,
}

/// One chunk parsed from a container, its payload borrowed from the input
/// buffer (zero-copy — no allocation per chunk).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk<'a> {
    pub id: ChunkId,
    pub payload: &'a [u8],
}

/// A parsed container: the header plus every chunk in file order, payloads
/// borrowed from the source bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container<'a> {
    pub header: Header,
    pub chunks: Vec<Chunk<'a>>,
}

impl<'a> Container<'a> {
    /// Look up the first chunk with the given id, if present.
    pub fn chunk(&self, id: ChunkId) -> Option<&Chunk<'a>> {
        self.chunks.iter().find(|c| c.id == id)
    }

    /// Enforce §2.1's critical-chunk rule: return [`FormatError::UnknownCritical`]
    /// for the first chunk whose id is **critical** yet absent from `known`.
    /// Ancillary chunks are never fatal. The container parse itself cannot do
    /// this — only a caller holding the id registry knows what "unknown" means.
    pub fn require_known(&self, known: &[ChunkId]) -> Result<(), FormatError> {
        for c in &self.chunks {
            if c.id.is_critical() && !known.contains(&c.id) {
                return Err(FormatError::UnknownCritical(c.id.0));
            }
        }
        Ok(())
    }
}

fn read_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn read_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Parse a `.xmr` container from a byte buffer, validating the header, the
/// version gate (§6), 8-byte alignment, zero padding, and uniqueness of chunk
/// ids. Payloads are borrowed, not copied.
///
/// This does **not** enforce §2.1 (unknown-critical refusal): that needs the
/// caller's id registry — see [`Container::require_known`].
pub fn read(bytes: &[u8]) -> Result<Container<'_>, FormatError> {
    if bytes.len() < HEADER_LEN {
        return Err(FormatError::Truncated);
    }
    if bytes[0..4] != MAGIC {
        return Err(FormatError::BadMagic);
    }
    let container_version = read_u16(bytes, 4);
    if container_version > CONTAINER_VERSION {
        return Err(FormatError::UnsupportedContainer(container_version));
    }
    let flags = read_u16(bytes, 6);
    if flags != 0 {
        return Err(FormatError::ReservedFlags(flags));
    }
    let schema_version = read_u32(bytes, 8);
    let min_reader_version = read_u32(bytes, 12);
    if min_reader_version > SCHEMA_VERSION {
        return Err(FormatError::ReaderTooOld {
            required: min_reader_version,
            have: SCHEMA_VERSION,
        });
    }

    let header = Header {
        container_version,
        flags,
        schema_version,
        min_reader_version,
    };

    let mut chunks: Vec<Chunk<'_>> = Vec::new();
    let mut off = HEADER_LEN;
    while off < bytes.len() {
        // Every chunk starts 8-aligned; the writer guarantees it, so a
        // misaligned offset here means a corrupt or hand-mangled file.
        if !off.is_multiple_of(8) {
            return Err(FormatError::BadPadding);
        }
        // Need the 8-byte framing (id + len) before the payload.
        if off + 8 > bytes.len() {
            return Err(FormatError::Truncated);
        }
        let id = ChunkId([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]]);
        let len = read_u32(bytes, off + 4) as usize;
        let payload_start = off + 8;
        let payload_end = payload_start
            .checked_add(len)
            .ok_or(FormatError::Truncated)?;
        if payload_end > bytes.len() {
            return Err(FormatError::Truncated);
        }
        if chunks.iter().any(|c| c.id == id) {
            return Err(FormatError::DuplicateChunk(id.0));
        }
        chunks.push(Chunk {
            id,
            payload: &bytes[payload_start..payload_end],
        });

        // Skip the zero padding up to the next 8-byte boundary. Padding must
        // be zero; the last chunk may legally end the file before its pad.
        let padded_end = align_up(payload_end);
        let pad_check_end = padded_end.min(bytes.len());
        if bytes[payload_end..pad_check_end].iter().any(|&b| b != 0) {
            return Err(FormatError::BadPadding);
        }
        off = padded_end;
    }

    Ok(Container { header, chunks })
}

/// Builds a container byte-for-byte. Chunks are appended in order and each is
/// padded to the 8-byte boundary, so payloads stay castable (§4).
#[derive(Debug, Clone)]
pub struct Writer {
    schema_version: u32,
    min_reader_version: u32,
    chunks: Vec<(ChunkId, Vec<u8>)>,
}

impl Writer {
    /// Start a container. `min_reader_version` is normally computed from the
    /// chunks actually written (§6.1); this layer just records what it is told.
    pub fn new(schema_version: u32, min_reader_version: u32) -> Self {
        Self {
            schema_version,
            min_reader_version,
            chunks: Vec::new(),
        }
    }

    /// Append a chunk. Errors with [`FormatError::DuplicateChunk`] if `id` was
    /// already pushed — the format forbids duplicate ids (§2).
    pub fn push(&mut self, id: ChunkId, payload: Vec<u8>) -> Result<(), FormatError> {
        if self.chunks.iter().any(|(cid, _)| *cid == id) {
            return Err(FormatError::DuplicateChunk(id.0));
        }
        self.chunks.push((id, payload));
        Ok(())
    }

    /// Serialise the whole container to bytes. The result length is always a
    /// multiple of 8.
    pub fn into_bytes(self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&CONTAINER_VERSION.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // flags: reserved, 0
        out.extend_from_slice(&self.schema_version.to_le_bytes());
        out.extend_from_slice(&self.min_reader_version.to_le_bytes());
        debug_assert_eq!(out.len(), HEADER_LEN);

        for (id, payload) in &self.chunks {
            out.extend_from_slice(&id.0);
            // `len` is a u32 by §2; payloads above 4 GiB are unrepresentable.
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            out.extend_from_slice(payload);
            let pad = align_up(out.len()) - out.len();
            out.resize(out.len() + pad, 0);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const MHDR: ChunkId = ChunkId::new(b"MHDR");
    const PCM: ChunkId = ChunkId::new(b"PCM ");
    const ORGN: ChunkId = ChunkId::new(b"orgn");

    fn sample_container() -> Vec<u8> {
        let mut w = Writer::new(1, 1);
        w.push(MHDR, vec![1, 2, 3]).unwrap(); // len 3 → 5 bytes padding
        w.push(PCM, vec![9, 9, 9, 9, 9, 9, 9, 9]).unwrap(); // len 8 → no padding
        w.push(ORGN, vec![7]).unwrap();
        w.into_bytes()
    }

    #[test]
    fn round_trip_preserves_header_and_chunks() {
        let bytes = sample_container();
        let c = read(&bytes).unwrap();
        assert_eq!(c.header.schema_version, 1);
        assert_eq!(c.header.min_reader_version, 1);
        assert_eq!(c.header.flags, 0);
        assert_eq!(c.chunks.len(), 3);
        assert_eq!(c.chunk(MHDR).unwrap().payload, &[1, 2, 3]);
        assert_eq!(c.chunk(PCM).unwrap().payload, &[9; 8]);
        assert_eq!(c.chunk(ORGN).unwrap().payload, &[7]);
    }

    #[test]
    fn every_payload_is_eight_byte_aligned() {
        let bytes = sample_container();
        assert!(
            bytes.len().is_multiple_of(8),
            "total length must stay a multiple of 8"
        );
        let c = read(&bytes).unwrap();
        for chunk in &c.chunks {
            // The payload slice must begin at an 8-aligned offset into `bytes`
            // — the invariant a zero-copy PCM cast relies on (§4).
            let off = chunk.payload.as_ptr() as usize - bytes.as_ptr() as usize;
            assert!(
                off.is_multiple_of(8),
                "payload of {:?} not 8-aligned",
                chunk.id
            );
        }
    }

    #[test]
    fn chunk_order_is_preserved() {
        let bytes = sample_container();
        let c = read(&bytes).unwrap();
        let ids: Vec<_> = c.chunks.iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![MHDR, PCM, ORGN]);
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut bytes = sample_container();
        bytes[0] = b'Z';
        assert_eq!(read(&bytes), Err(FormatError::BadMagic));
    }

    #[test]
    fn short_header_is_truncated() {
        assert_eq!(read(b"XMR"), Err(FormatError::Truncated));
        assert_eq!(read(&MAGIC), Err(FormatError::Truncated)); // 4 < 16
    }

    #[test]
    fn declared_length_past_eof_is_truncated() {
        let mut bytes = sample_container();
        // Inflate MHDR's length (u32 at offset 20 = HEADER_LEN + 4) past EOF.
        let len_at = HEADER_LEN + 4;
        bytes[len_at..len_at + 4].copy_from_slice(&9999u32.to_le_bytes());
        assert_eq!(read(&bytes), Err(FormatError::Truncated));
    }

    #[test]
    fn non_zero_flags_is_rejected() {
        let mut bytes = sample_container();
        bytes[6] = 1; // flags low byte
        assert_eq!(read(&bytes), Err(FormatError::ReservedFlags(1)));
    }

    #[test]
    fn reader_too_old_is_rejected() {
        // Hand-set min_reader_version (u32 at offset 12) one past this build.
        let mut bytes = sample_container();
        let future = SCHEMA_VERSION + 1;
        bytes[12..16].copy_from_slice(&future.to_le_bytes());
        assert_eq!(
            read(&bytes),
            Err(FormatError::ReaderTooOld {
                required: future,
                have: SCHEMA_VERSION,
            })
        );
    }

    #[test]
    fn newer_container_version_is_rejected() {
        let mut bytes = sample_container();
        bytes[4..6].copy_from_slice(&(CONTAINER_VERSION + 1).to_le_bytes());
        assert_eq!(
            read(&bytes),
            Err(FormatError::UnsupportedContainer(CONTAINER_VERSION + 1))
        );
    }

    #[test]
    fn non_zero_padding_is_rejected() {
        let mut bytes = sample_container();
        // MHDR payload is 3 bytes at offset 24; its padding is bytes 27..32.
        // Poke a non-zero byte into that pad region.
        bytes[27] = 0xFF;
        assert_eq!(read(&bytes), Err(FormatError::BadPadding));
    }

    #[test]
    fn writer_rejects_duplicate_id() {
        let mut w = Writer::new(1, 1);
        w.push(MHDR, vec![0]).unwrap();
        assert_eq!(
            w.push(MHDR, vec![1]),
            Err(FormatError::DuplicateChunk(*b"MHDR"))
        );
    }

    #[test]
    fn empty_payload_round_trips() {
        // schema_version may be anything (not gated on read); min_reader_version
        // must stay ≤ SCHEMA_VERSION or the file is legitimately refused.
        let mut w = Writer::new(3, 1);
        w.push(MHDR, vec![]).unwrap();
        let bytes = w.into_bytes();
        let c = read(&bytes).unwrap();
        assert_eq!(c.header.schema_version, 3);
        assert_eq!(c.header.min_reader_version, 1);
        assert_eq!(c.chunk(MHDR).unwrap().payload, &[] as &[u8]);
    }

    #[test]
    fn case_bits_classify_ids() {
        assert!(MHDR.is_critical() && !MHDR.is_ancillary());
        assert!(!MHDR.is_safe_to_copy()); // 'R' uppercase
        assert!(ORGN.is_ancillary() && !ORGN.is_critical());
        assert!(ORGN.is_safe_to_copy()); // 'n' lowercase
        assert!(PCM.is_critical()); // 'P' uppercase
        assert!(!PCM.is_safe_to_copy()); // ' ' is neither, so not safe
    }

    #[test]
    fn require_known_flags_unknown_critical_only() {
        let bytes = sample_container();
        let c = read(&bytes).unwrap();
        // MHDR + PCM are critical; ORGN is ancillary. Knowing only MHDR+PCM
        // must pass (ORGN is ancillary, ignored).
        assert!(c.require_known(&[MHDR, PCM]).is_ok());
        // Knowing only MHDR must flag PCM (critical, unknown).
        assert_eq!(
            c.require_known(&[MHDR]),
            Err(FormatError::UnknownCritical(*b"PCM "))
        );
    }
}
