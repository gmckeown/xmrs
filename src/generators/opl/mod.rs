//! OPL FM synthesis for the tracker.
//!
//! The chip core — 18 two-operator channels, the envelope/phase generators, the
//! clean-room ROM tables, the register file, OPL3 4-op mode — lives in the
//! standalone [`oplon`] crate (`Opl2`/`Opl3`), which this crate depends on. Only
//! the **tracker-facing driver** lives here: [`OplDriver`](driver::OplDriver)
//! translates tracker-channel gestures (note-on / pitch / volume / pan, the
//! `OPL_Touch` volume law, rhythm-mode placement) into chip calls on an
//! [`oplon::Opl2`], mirroring the documented AdLib programming procedure in
//! Schism's `player/snd_fm.c`.
//!
//! The player drives one `OplDriver` (owned by `Voices`, behind an `Option`, so
//! a module with no FM instrument never instantiates it).
//!
//! Scope is 2-operator OPL2 voices (exactly the S3M/IT AdLib patch shape) plus
//! OPL3 stereo panning; the underlying `oplon` chip also implements the full
//! OPL3 (18 channels, waveforms 4-7, 4-operator mode), which the tracker driver
//! does not currently exercise.

pub mod driver;

/// The register-level OPL2/OPL3 chip, re-exported from the [`oplon`] crate.
pub use oplon::Opl2;
