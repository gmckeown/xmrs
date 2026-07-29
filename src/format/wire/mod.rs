//! The **wire types** — the frozen, on-disk shape of each chunk payload
//! (§5.1). They mirror [`crate::core::module::Module`] but are deliberately
//! *not* the model: a wire type never `#[cfg]`-gates a field, never carries
//! `Arc` / borrows / `usize`, and — once a `vN` ships — never changes. That
//! decoupling is what lets `core::Module` keep being refactored while old
//! files stay readable (the migration chain maps `vN → vN+1`).
//!
//! Each chunk payload is CBOR (`ciborium`) — self-describing and
//! length-delimited, so `#[serde(default)]` / `#[serde(alias)]` on the wire
//! types actually buy the forward/backward compatibility the model's own
//! annotations only *claim* under a positional codec (§0).
//!
//! Only structural payloads pass through CBOR; the `PCM ` blob region is raw
//! bytes and never touches serde (§4).

use alloc::string::ToString;
use alloc::vec::Vec;
use serde::de::DeserializeOwned;
use serde::Serialize;

use super::FormatError;

pub mod v1;

/// Encode a wire value to a CBOR chunk payload.
pub(crate) fn to_cbor<T: Serialize>(value: &T) -> Result<Vec<u8>, FormatError> {
    let mut buf = Vec::new();
    ciborium::into_writer(value, &mut buf).map_err(|e| FormatError::Cbor(e.to_string()))?;
    Ok(buf)
}

/// Decode a wire value from a CBOR chunk payload.
pub(crate) fn from_cbor<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, FormatError> {
    ciborium::from_reader(bytes).map_err(|e| FormatError::Cbor(e.to_string()))
}
