//! Core data model — format-agnostic.
//!
//! Self-sufficient for a modern DAW editor that never imports historical
//! tracker files — including the chip-synthesis instrument types
//! ([`crate::core::instr_opl`], [`crate::core::instr_sid`],
//! [`crate::core::instr_robsid`]), which are authoring
//! primitives rather than import artifacts. Format-tied items (importers,
//! presets, period helper) live under [`crate::tracker`].
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
//!   [`crate::core::instr_midi`]; chip-synthesis voices
//!   [`crate::core::instr_opl`], [`crate::core::instr_sid`] and
//!   [`crate::core::instr_robsid`] — all three **always compiled**; it is their
//!   render engines (`generators::opl`, `generators::sid`)
//!   that gate on `synth_opl` / `synth_sid`
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
/// Yamaha OPL2/OPL3 FM voice — a pure data type (register layout, integer
/// only, no `oplon` dependency), compiled in **every** build so
/// [`InstrumentType`](instrument::InstrumentType)'s variant set is stable
/// regardless of features. Its render engine `generators::opl` is
/// what gates on `synth_opl`.
pub mod instr_opl;
/// Rob Hubbard-style SID instrument, layered on [`crate::core::instr_sid`].
/// A data type, always compiled (see [`instr_opl`]).
pub mod instr_robsid;
/// MOS6581/8580 SID voice — a data type, always compiled (see [`instr_opl`]);
/// its render engine `generators::sid` gates on `synth_sid`.
pub mod instr_sid;
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
