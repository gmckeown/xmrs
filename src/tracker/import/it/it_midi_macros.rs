//! IT MIDI macro tables (4896 bytes):
//! `global:[u8;32×9] parametric:[u8;32×16] fixed:[u8;32×128]`.

use crate::core::module::MidiMacros;
use crate::tracker::import::bin_reader::{BinReader, ImportError};

/// On-disk size of [`ItMidiMacros`].
pub const IT_MIDI_MACROS_SIZE: usize = 32 * (9 + 16 + 128);

#[derive(Debug)]
pub struct ItMidiMacros {
    global: [[u8; 32]; 9],
    parametric: [[u8; 32]; 16],
    fixed: [[u8; 32]; 128],
}

impl ItMidiMacros {
    pub fn read(r: &mut BinReader) -> Result<Self, ImportError> {
        let mut global = [[0u8; 32]; 9];
        for slot in &mut global {
            *slot = r.read_array()?;
        }
        let mut parametric = [[0u8; 32]; 16];
        for slot in &mut parametric {
            *slot = r.read_array()?;
        }
        let mut fixed = [[0u8; 32]; 128];
        for slot in &mut fixed {
            *slot = r.read_array()?;
        }
        Ok(Self {
            global,
            parametric,
            fixed,
        })
    }

    /// Convert the fixed-size IT macro tables into the
    /// `Vec`-based public representation attached to `Module`.
    /// Trailing zero bytes on each macro are kept — the interpreter
    /// reads each byte one-by-one and stops at its own terminator
    /// logic (the 32-byte fixed length is the IT file format, not
    /// a runtime constraint).
    pub fn to_public(&self) -> MidiMacros {
        MidiMacros {
            global: self.global.iter().map(|m| m.to_vec()).collect(),
            parametric: self.parametric.iter().map(|m| m.to_vec()).collect(),
            fixed: self.fixed.iter().map(|m| m.to_vec()).collect(),
        }
    }
}
