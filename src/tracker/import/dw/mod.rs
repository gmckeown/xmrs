#![forbid(unsafe_code)]

//! David Whittaker `.dw` importer.
//!
//! A `.dw` file is an Amiga 68000 executable that bundles the
//! replayer *and* the song data in one blob. There is no fixed
//! header layout: the data offsets are encoded as displacements
//! of a handful of init-time 68000 instructions. The importer
//! extracts those displacements by pattern-matching on 2-byte
//! opcode prefixes (no full disassembler needed), then parses the
//! tables they point to.
//!
//! Two replayer families are recognised:
//! - **new player** — the form used by most Whittaker scores,
//!   including the Xenon 2 title music. Period table is
//!   `tables::PERIODS2` or `tables::PERIODS3`.
//! - **old player** — the early "QBall" variant, period table
//!   `tables::PERIODS1`.
//!
//! The format itself has no public textual spec; this importer is
//! reverse-engineered directly from the original 68000 replayers
//! embedded in the `.dw` files (Ghidra decompilation + a Paula
//! emulation oracle). See `FORMAT.md` in this directory for the
//! long-form description and `oracle/` for the verification harness.

pub mod detect;
pub(crate) mod command_map;
pub mod dw_module;
pub mod event;
pub mod header;
pub mod runtime;
pub mod tables;
