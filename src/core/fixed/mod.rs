//! Fixed-point primitives for the xmrs ecosystem.
//!
//! Substrate on which `xmrs` and `xmrsplayer` run without `f32`
//! arithmetic, so they ship to processors without an FPU
//! (Cortex-M0, classic 68000, eventually Z80 / 6502 with the
//! right toolchain).
//!
//! The module is split into four layers:
//!
//! * [`crate::core::fixed::fixed`] — generic `Q<T, FRAC>` types. All arithmetic
//!   primitives, rounding rules and saturation rules live here.
//!   No domain knowledge.
//!
//! * [`crate::core::fixed::units`] — newtypes for the player's domain concepts:
//!   [`Volume`](crate::core::fixed::units::Volume), [`Panning`](crate::core::fixed::units::Panning),
//!   [`Pitch`](crate::core::fixed::units::Pitch), [`Frequency`](crate::core::fixed::units::Frequency),
//!   etc. These wrap the [`crate::core::fixed::fixed`] types and only expose
//!   operations that make musical sense, so mixing them up
//!   is a compile error.
//!
//! * [`crate::core::fixed::tables`] — compile-time lookup tables built with
//!   `const fn` (sine, equal-power-pan sqrt, period→frequency,
//!   Amiga period). These replace `sin`, `sqrt`, `powf`, `exp`,
//!   `log2` calls; the module has no dependency on `libm`,
//!   `micromath`, or any soft-float runtime.
//!
//! * [`crate::core::fixed::from_tracker`] — a small helper module that converts
//!   the raw bytes / nibbles found in `.mod` / `.xm` / `.s3m` /
//!   `.it` files directly into the domain newtypes, without
//!   ever constructing an intermediate `f32`.
//!
//! Plus the optional `float_helpers` module, gated behind the
//! `float-helpers` cargo feature: `f32 ↔ Q` conversions for the
//! editor / desktop side. Never enabled on embedded builds.
//!
//! ## Discipline
//!
//! Outside the [`crate::core::fixed::fixed`] submodule, code should never write
//! `as i16`, `>> 15`, `<< 9` and similar bit hacks against
//! Q-formatted values. If you find yourself wanting to, the
//! corresponding method is missing from [`crate::core::fixed::units`] and should
//! be added there. This is what keeps the rest of the player
//! readable.
//!
//! Outside [`crate::core::fixed::from_tracker`] (load-time only), code should
//! never see an `f32` either. The audio hot path is integer-only.

#![warn(missing_docs)]

// `crate::core::fixed::fixed::*` holds the raw Q-format newtypes (Q15,
// Q8_8, etc.); the outer `fixed` module groups them with the
// domain newtypes in `units`, the LUTs in `tables`, and the
// load-time converters in `from_tracker`. Renaming would churn
// 28 import sites for no functional gain.
#[allow(clippy::module_inception)]
pub mod fixed;
pub mod from_tracker;
pub mod tables;
pub mod units;

#[cfg(feature = "float-helpers")]
pub mod float_helpers;

// Convenience prelude — pull this into a player file and you
// have everything in one go.
pub mod prelude {
    //! One-stop import for the player layer.
    pub use super::fixed::{Q15, Q16_16, Q24_8, Q7_25, Q8_8};
    pub use super::units::{
        Amp, Amplification, ChannelVolume, EnvValue, Finetune, Frequency, GlobalVolume, Panning,
        Period, Pitch, PitchAcc, PitchDelta, RetrigMul, SampleRate, SampleStep, Volume,
    };
}
