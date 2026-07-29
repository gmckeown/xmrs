//! Native `.xmr` file format — the chunked container, the frozen `wire::vN`
//! types, the migration chain, and the CBOR codec. See
//! `src/core/FORMAT_RFC.md` for the specification and the reasoning.
//!
//! `no_std` + `alloc`, and independent of any importer: the format is the
//! project's own archival representation of [`crate::core::module::Module`],
//! distinct from the live model (which stays free to be refactored) and from
//! the derived serde repr the README used to recommend.
//!
//! This module is built in bottom-up layers, each testable on its own:
//!
//! - [`container`] — the byte grammar of §2: the 16-byte header and the
//!   `[id][len][payload]` chunk stream, 8-byte aligned, with the PNG-style
//!   critical / safe-to-copy case bits. Knows nothing of `Module`.
//! - [`wire`] — the frozen per-schema payload types ([`wire::v1`]) and the
//!   CBOR codec. A shipped `vN` is never edited; a later schema redefines only
//!   what changed (§6.4).
//! - [`pcm`] — the raw `PCM ` blob region (§4): sample bytes live outside CBOR,
//!   8-aligned and deduplicated by `Arc` identity.
//! - [`document`] — the assembler on top: [`document::write`] /
//!   [`document::read`] turn a [`Module`](crate::core::module::Module) into
//!   `.xmr` bytes and back, reporting what a read had to skip (§8).
//!
//! [`container`]: crate::format::container
//! [`wire`]: crate::format::wire
//! [`wire::v1`]: crate::format::wire::v1
//! [`pcm`]: crate::format::pcm
//! [`document`]: crate::format::document
//! [`document::write`]: crate::format::document::write
//! [`document::read`]: crate::format::document::read

pub mod container;
pub mod document;
pub mod pcm;
pub mod wire;

/// Magic at byte 0 of every `.xmr` file: ASCII `"XMRS"`.
///
/// Four bytes where the extension has three, deliberately: a container magic
/// is a fixed-width field, an extension is a name. They do not have to match —
/// a `.png` starts with `\x89PNG`.
pub const MAGIC: [u8; 4] = *b"XMRS";

/// Grammar version of the container itself (§2). Bumps ~never — only if the
/// byte layout of the header / chunk framing changes, which the in-band
/// `schema_version` is designed to make unnecessary.
pub const CONTAINER_VERSION: u16 = 1;

/// The highest `schema_version` this build implements — the reader's own
/// version (§6). A file whose `min_reader_version` exceeds this cannot be
/// interpreted correctly and is refused at the header.
pub const SCHEMA_VERSION: u32 = 1;

/// A 4-byte chunk id carrying the PNG-style case bits (§2.1). The two bits
/// are the entire forward-compatibility policy, with no table to maintain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChunkId(pub [u8; 4]);

impl ChunkId {
    /// Construct from a 4-byte literal, e.g. `ChunkId::new(b"MHDR")`.
    pub const fn new(id: &[u8; 4]) -> Self {
        ChunkId(*id)
    }

    /// Byte 0 uppercase ⇒ **critical**: a reader that does not recognise this
    /// id MUST refuse the file (§2.1). Rendering-affecting chunks are critical.
    pub fn is_critical(&self) -> bool {
        self.0[0].is_ascii_uppercase()
    }

    /// The complement of [`Self::is_critical`]: an unknown ancillary chunk is
    /// skipped, not fatal.
    pub fn is_ancillary(&self) -> bool {
        !self.is_critical()
    }

    /// Byte 3 lowercase ⇒ **safe to copy** verbatim into a rewritten file
    /// even when the chunk is not understood (§2.1). Uppercase ⇒ it may
    /// depend on data that changed, so a rewrite MUST NOT copy it.
    pub fn is_safe_to_copy(&self) -> bool {
        self.0[3].is_ascii_lowercase()
    }
}

/// Everything that can go wrong reading a `.xmr` container. Structural only;
/// payload-decode (CBOR) errors join this enum with the wire layer.
///
/// `#[non_exhaustive]`: a format that will gain schema versions will gain
/// failure modes, and on a published API a new variant breaks every exhaustive
/// `match` downstream. Callers need a wildcard arm; that is the price of the
/// enum being able to grow at all.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FormatError {
    /// The first four bytes are not [`MAGIC`].
    BadMagic,
    /// The header is shorter than 16 bytes, or a chunk's declared length runs
    /// past end-of-file.
    Truncated,
    /// `container_version` is newer than this build's [`CONTAINER_VERSION`].
    UnsupportedContainer(u16),
    /// The reserved `flags` header field is non-zero (§2: MUST be 0).
    ReservedFlags(u16),
    /// The file's `min_reader_version` exceeds this build's [`SCHEMA_VERSION`]
    /// (§6): it uses a feature this reader does not implement.
    ReaderTooOld { required: u32, have: u32 },
    /// The same chunk id appears more than once (§2).
    DuplicateChunk([u8; 4]),
    /// A chunk did not start on an 8-byte boundary, or inter-chunk padding was
    /// not zero (§2).
    BadPadding,
    /// A **critical** chunk id this reader does not recognise (§2.1). Raised
    /// by [`container::Container::require_known`], not by the parse itself.
    UnknownCritical([u8; 4]),
    /// A chunk payload failed to encode to or decode from CBOR. Carries the
    /// codec's message (the concrete `ciborium` error is not `PartialEq`, so
    /// it is flattened to a string at the boundary).
    Cbor(alloc::string::String),
    /// The `PCM ` region or a blob reference into it is malformed (§4).
    Pcm(&'static str),
}

/// Round a length up to the next multiple of the 8-byte chunk alignment.
pub(crate) const fn align_up(n: usize) -> usize {
    (n + 7) & !7
}
