#![forbid(unsafe_code)]

pub mod s3m_effect;
pub mod s3m_module;

/// The ST3 masterflag bits the effect translation depends on. Defined next to
/// the shared effect-memory bookkeeping (`import::memory`, `pub(crate)`) and
/// re-exported here because it is reachable from public S3M signatures — a
/// type a caller can receive must be a type they can name.
pub use crate::tracker::import::memory::S3mImportFlags;
