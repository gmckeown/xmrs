//! Signal / event **generators** — the runtime engines that *produce*
//! sound or note events at playback, as opposed to [`crate::core`] (the
//! format-agnostic data model) and [`crate::tracker`] (format I/O and the
//! format-tied data structures).
//!
//! Each generator is self-contained, `no_std`, integer-only DSP/logic with
//! no dependency on the player crate; the player owns instances and feeds
//! them gestures. Today this hosts the OPL FM synthesiser (the rendering
//! counterpart of [`crate::core::instr_opl::InstrOpl`]) and the SID
//! synthesiser (counterpart of [`crate::core::instr_sid::InstrSid`]); future
//! generators (the DW chip, Euclidean rhythm) belong here too.

/// Clean-room OPL2 (Yamaha YM3812) FM synthesiser. See
/// `OPL_SYNTHESIS_RFC.md`.
#[cfg(feature = "synth_opl")]
pub mod opl;

/// Cycle-accurate MOS 6581/8580 SID synthesiser — the runtime counterpart
/// of [`crate::core::instr_sid::InstrSid`], wrapping the `sidera`
/// emulator.
#[cfg(feature = "synth_sid")]
pub mod sid;
