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

/// Per-channel / per-song effect-memory bookkeeping shared by all
/// PCM-style importers (MOD, XM, S3M, IT, SID).
#[cfg(any(
    feature = "import_mod",
    feature = "import_xm",
    feature = "import_s3m",
    feature = "import_it",
    feature = "import_sid",
))]
pub(crate) mod import_memory;

/// Shared `PatternSlot` type used by every importer.
#[cfg(any(
    feature = "import_mod",
    feature = "import_xm",
    feature = "import_s3m",
    feature = "import_it",
    feature = "import_sid",
))]
pub(crate) mod patternslot;

/// Order-list parsing helper (used by XM/S3M/IT; MOD and SID build
/// their orders directly).
#[cfg(any(feature = "import_xm", feature = "import_s3m", feature = "import_it",))]
pub(crate) mod orders_helper;

/// Format-agnostic effect representation produced by each importer
/// and consumed by `import_memory`.
#[cfg(any(
    feature = "import_mod",
    feature = "import_xm",
    feature = "import_s3m",
    feature = "import_it",
    feature = "import_sid",
))]
pub(crate) mod track_import_effect;

#[cfg(any(
    feature = "import_mod",
    feature = "import_xm",
    feature = "import_s3m",
    feature = "import_it",
    feature = "import_sid",
))]
pub(crate) mod track_import_unit;

/// impl of `Module::load_mod` / `load_xm` / `load_s3m` / `load_it` /
/// `load`. Each method is individually gated inside. SID is not
/// auto-detected via `Module::load` (it is imported via
/// `SidModule::load` directly), so SID alone doesn't pull this in.
#[cfg(any(
    feature = "import_mod",
    feature = "import_xm",
    feature = "import_s3m",
    feature = "import_it",
))]
mod import_loader;

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
