use serde::Deserialize;
use serde_big_array::BigArray;

use crate::module::MidiMacros;

#[derive(Deserialize, Debug)]
#[repr(C)]
pub struct ItMidiMacros {
    global: [[u8; 32]; 9],
    parametric: [[u8; 32]; 16],
    #[serde(with = "BigArray")]
    fixed: [[u8; 32]; 128],
}

impl ItMidiMacros {
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
