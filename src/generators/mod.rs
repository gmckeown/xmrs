//! Signal / event **generators** — the runtime engines that *produce*
//! sound or note events at playback, as opposed to [`crate::core`] (the
//! format-agnostic data model) and [`crate::tracker`] (format I/O and the
//! format-tied data structures).
//!
//! Each generator is self-contained, `no_std`, integer-only DSP/logic with
//! no dependency on the player crate; the player owns instances and feeds
//! them gestures. Today this hosts the OPL FM synthesiser (the rendering
//! counterpart of [`crate::tracker::instr_opl::InstrOpl`]); future
//! generators (SID, the DW chip, Euclidean rhythm) belong here too.

/// Clean-room OPL2 (Yamaha YM3812) FM synthesiser. See
/// `OPL_SYNTHESIS_RFC.md`.
#[cfg(feature = "import_s3m")]
pub mod opl;

/// Cycle-accurate MOS 6581/8580 SID synthesiser — the runtime counterpart
/// of [`crate::tracker::instr_sid::InstrSid`], wrapping the `sidera`
/// emulator.
#[cfg(feature = "import_sid")]
pub mod sid;
