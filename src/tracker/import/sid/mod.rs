#![forbid(unsafe_code)]

/// Byte offset of the 6502 image inside a `.sid` file: the 124-byte PSID/RSID
/// header, then the two-byte load address the image itself carries.
pub(crate) const PSID_DATA: usize = 126;

pub mod detect;
pub mod g2;
pub(crate) mod instr_helper;
// `one_sid`, `voices` and `sound_fx` are public because `SidModule`'s fields
// are: a caller could already reach an `OneSid` through `SidModule::sid`, but
// with the modules private the type had no nameable path, so no downstream
// signature could mention it. Reachable-but-unnameable is the wart; this is
// not new API surface.
pub mod one_sid;
pub mod sid_module;
/// The bundled `.sid` images — third-party binaries, `sid_songs` feature,
/// off by default. The reverse-engineered records live in `one_sid` and are
/// always compiled; only the payload is optional.
#[cfg(feature = "sid_songs")]
pub mod songs;
pub mod sound_fx;
pub mod voices;
