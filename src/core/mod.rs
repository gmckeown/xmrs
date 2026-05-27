//! Core data model — format-agnostic.
//!
//! Self-sufficient for a modern DAW editor that never imports historical
//! tracker files. Format-tied items (importers, presets, period helper,
//! historical-only instrument types) live under [`crate::tracker`].
//!
//! ## Module groups
//!
//! - **Patterns & events**: [`crate::core::cell`], [`crate::core::cell_note`],
//!   [`crate::core::pitch`], [`crate::core::effect`]
//! - **Sound shaping**: [`crate::core::envelope`], [`crate::core::vibrato`],
//!   [`crate::core::waveform`], [`crate::core::sample`]
//! - **Voice & instruments**: [`crate::core::voice_setup`],
//!   [`crate::core::keyboard`], [`crate::core::instrument_behavior`],
//!   [`crate::core::instrument`], [`crate::core::instr_default`],
//!   [`crate::core::instr_midi`]
//! - **Top-level container**: [`crate::core::module`],
//!   [`crate::core::compatibility`]
//! - **DAW timeline**: [`crate::core::daw`] (clips, tracks, automation,
//!   timeline map, Euclidean)
//! - **Primitives**: [`crate::core::duration`], [`crate::core::xorshift`],
//!   [`crate::core::fixed`]

pub mod cell;
pub mod cell_note;
pub mod compatibility;
pub mod daw;
pub mod duration;
pub mod effect;
pub mod envelope;
pub mod fixed;
pub mod instr_default;
pub mod instr_midi;
pub mod instrument;
pub mod instrument_behavior;
pub mod keyboard;
pub mod module;
pub mod pitch;
pub mod sample;
pub mod vibrato;
pub mod voice_setup;
pub mod waveform;
pub mod xorshift;
