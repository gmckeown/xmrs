#![forbid(unsafe_code)]

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
