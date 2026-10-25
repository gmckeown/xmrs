#![forbid(unsafe_code)]
// The importer modules build Module/Instrument/Sample structs stepwise:
// a `Default::default()` value is mutated via field assignments and
// `&mut field` borrows spanning several lines. Clippy suggests a single
// struct-literal with `..Default::default()`, but that rewrite cannot
// express the mix of direct assignments and intermediate `&mut` borrows
// of the half-built value, and would require extracting every sub-block
// into a helper. The current style is consistent across the crate and
// matches the layout of the historical tracker file formats being parsed.
#![allow(clippy::field_reassign_with_default)]

// ---- Shared helpers ----
//
// These are tiny, format-agnostic utilities used by every format
// importer. They are gated as a group on "any importer enabled" so
// that a `--no-default-features` build without any format stays
// lean (these helpers disappear too).

/// Cursor-based binary reader (LE primitives + fixed-size name
/// fields). Replaces the bincode-via-serde decoding shim used by
/// every importer for its C-packed header structs.
#[cfg(any(
    feature = "import_mod",
    feature = "import_xm",
    feature = "import_s3m",
    feature = "import_it",
    feature = "import_sid",
    feature = "import_dw",
    feature = "import_tfmx",
))]
pub mod bin_reader;

/// Per-channel / per-song effect-memory bookkeeping shared by all
/// PCM-style importers (MOD, XM, S3M, IT, SID).
#[cfg(any(
    feature = "import_mod",
    feature = "import_xm",
    feature = "import_s3m",
    feature = "import_it",
    feature = "import_sid",
))]
pub(crate) mod memory;

/// Shared `PatternSlot` type used by every importer.
#[cfg(any(
    feature = "import_mod",
    feature = "import_xm",
    feature = "import_s3m",
    feature = "import_it",
    feature = "import_sid",
))]
pub(crate) mod patternslot;

/// Read-only view of the pre-DAW, importer-side state (`RawModule`),
/// for consumers that want what the file said rather than what the
/// player needs. Also the supported re-export path for
/// [`patternslot::PatternSlot`], whose own module stays `pub(crate)`.
#[cfg(any(
    feature = "import_mod",
    feature = "import_xm",
    feature = "import_s3m",
    feature = "import_it",
))]
pub mod raw;

/// Order-list parsing helper (used by XM/S3M/IT; MOD and SID build
/// their orders directly).
#[cfg(any(feature = "import_xm", feature = "import_s3m", feature = "import_it",))]
pub(crate) mod orders_helper;

/// Format-agnostic effect representation produced by each importer
/// and consumed by `import_memory`.
///
/// DAW migration Phase 1+2 promoted this to an always-compiled
/// shared module: `import::build` now consumes
/// `TrackImportEffect` (the runtime `Cell`'s effect list is
/// materialised at segment-build time), so the DAW layer needs it
/// even on `--no-default-features` builds without any format reader.
pub mod effect;

/// Pre-`Cell` cell representation produced by every format importer
/// and consumed by `import::build`. Unconditional for the
/// same reason as [`effect`].
pub mod unit;

/// Pattern → (tracks, clips, timeline_map) build pipeline. Always
/// compiled — the SID importer is track-native and only uses the
/// `split_rows_by_instrument` helper; the pattern-grid path is used
/// by MOD/XM/S3M/IT. Lives under `import/` because it consumes the
/// importer-side `TrackImportUnit` (DAW migration Phase 3c.3).
pub mod build;

/// `TrackImportUnit` → `AutomationLane` extractors (LFO, slide,
/// glide, song-level points/slides). Lives under `import/` for the
/// same reason as [`build`].
pub mod extract;

/// impl of `Module::load_mod` / `load_xm` / `load_s3m` / `load_it` /
/// `load_dw` / `load_sid` / `load`. Each method is individually gated
/// inside. SID *is* auto-detected via `Module::load` — off its PSID/RSID
/// magic, first and exclusively — so `import_sid` alone pulls this in.
/// DW is auto-detected even though it has no magic header: its
/// probe quadruple (`LEA / LEA / MOVEQ / DBF`) is unique enough
/// that scanning a few hundred bytes can tell a `.dw` apart from
/// random data.
#[cfg(any(
    feature = "import_mod",
    feature = "import_xm",
    feature = "import_s3m",
    feature = "import_it",
    feature = "import_dw",
    feature = "import_sid",
    feature = "import_tfmx",
))]
mod loader;

// ---- Per-format importers ----

/// Load historical Amiga MOD files.
#[cfg(feature = "import_mod")]
pub mod amiga;

/// Load historical XM files (Fast Tracker II).
///
/// `mod_xm_effect` inside this tree is also reused by the Amiga MOD
/// importer, so the module is compiled whenever either `import_mod`
/// or `import_xm` is enabled. Items specific to the XM file format
/// stay gated on `import_xm` alone.
#[cfg(any(feature = "import_mod", feature = "import_xm"))]
pub mod xm;

/// Load historical S3M files (ScreamTracker 3).
#[cfg(feature = "import_s3m")]
pub mod s3m;

/// Load historical IT files (Impulse Tracker).
#[cfg(feature = "import_it")]
pub mod it;

/// Load historical SID files (Commodore 64 / MOS6581).
#[cfg(feature = "import_sid")]
pub mod sid;

/// Load David Whittaker custom Amiga player files (`.dw`).
///
/// Not autodetected via `Module::load` (no reliable magic header
/// — the file is a 68000 executable that opens with a jump
/// table). Use [`dw::dw_module::DwModule::load`] directly, then
/// `.to_module()` for the editor representation. Same convention
/// as `import_sid`.
#[cfg(feature = "import_dw")]
pub mod dw;

/// Load Chris Hülsbeck / Jochen Hippel TFMX modules (`mdat.*` / `smpl.*` or `.tfx` / `.sam`).
#[cfg(feature = "import_tfmx")]
pub mod tfmx;
