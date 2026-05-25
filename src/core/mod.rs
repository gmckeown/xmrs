//! Core data model — format-agnostic. Suffisant pour un éditeur
//! DAW moderne qui n'importe jamais de fichiers historiques.
pub mod cell;
pub mod cell_note;
pub mod compatibility;
pub mod daw;
pub mod duration;
pub mod effect;
pub mod envelope;
pub mod fixed;
pub mod instr_default;
pub mod instr_ekn;
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
