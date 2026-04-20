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

pub(crate) mod import_memory;
pub(crate) mod orders_helper;
pub(crate) mod patternslot;
pub(crate) mod track_import_effect;
pub(crate) mod track_import_unit;

/// impl loader to Module. See Module documentation load* fn.
mod import_loader;

/// Load historical XM files
pub mod xm;

/// Load historical Amiga MOD files
pub mod amiga;

/// Load historical IT files
pub mod it;

/// Load historical S3M files
pub mod s3m;

/// Load historical SID files
#[cfg(feature = "import_sid")]
pub mod sid;
