//! Shared `PatternSlot` — the raw `(note, instrument, volume,
//! effect_type, effect_parameter)` quintuplet every tracker format
//! decodes its pattern bytes into before normalising into a
//! `TrackImportUnit`. Format-specific `patternslot.rs` files under
//! each importer (amiga/, xm/) provide the parser per the file
//! layout; this one holds the shared type.

use crate::core::cell_note::CellNote;
use alloc::format;
use alloc::string::ToString;
use core::fmt::*;
use serde::{Deserialize, Serialize};

/// A typical pattern slot
#[derive(Serialize, Deserialize, Copy, Clone, Eq, Hash, PartialEq)]
#[repr(C)]
pub struct PatternSlot {
    pub note: CellNote,
    pub instrument: Option<usize>,
    pub volume: u8,
    pub effect_type: u8,
    pub effect_parameter: u8,
}

impl Default for PatternSlot {
    fn default() -> Self {
        PatternSlot {
            note: CellNote::Empty,
            instrument: None,
            volume: 0,
            effect_type: 0,
            effect_parameter: 0,
        }
    }
}

impl Debug for PatternSlot {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        let v = if self.volume == 0 {
            '-'
        } else {
            core::char::from_digit(u32::from(self.volume & 0x0f), 16).unwrap()
        };
        let ninstr = if let Some(instr) = self.instrument {
            format!("{:>2X}", instr)
        } else {
            "  ".to_string()
        };
        write!(
            f,
            "[{:?} {} {:>1}{} {}{:02X}]",
            self.note,
            ninstr,
            self.volume_letter(),
            v,
            self.effect_letter(),
            self.effect_parameter
        )
    }
}

impl PatternSlot {
    pub fn has_arpeggio(&self) -> bool {
        self.effect_type == 0 && self.effect_parameter != 0
    }

    pub fn has_delay(&self) -> bool {
        self.effect_type == 0xE && (self.effect_parameter >> 4) == 0xD
    }

    pub fn has_retrigger_empty(&self) -> bool {
        self.effect_type == 0xE && self.effect_parameter == 0x90
    }
    pub fn has_tone_portamento(&self) -> bool {
        self.effect_type == 3 || self.effect_type == 5 || self.volume >> 4 == 0x0F
    }

    pub fn has_vibrato(&self) -> bool {
        self.effect_type == 4 || self.effect_type == 6 || self.volume >> 4 == 0x0B
    }

    pub fn has_volume_slide(&self) -> bool {
        self.effect_type == 5
            || self.effect_type == 6
            || self.effect_type == 0xA
            || (self.effect_type == 0xE && (self.effect_parameter >> 4) == 0xA)
            || (self.effect_type == 0xE && (self.effect_parameter >> 4) == 0xB)
    }

    pub fn volume_letter(&self) -> char {
        match self.volume >> 4 {
            0x0 => '-',
            0x1 => '0',
            0x2 => '1',
            0x3 => '2',
            0x4 => '3',
            0x5 => '4',
            0x6 => '-',
            0x7 => '+',
            0x8 => 'D',
            0x9 => 'U',
            0xA => 'S',
            0xB => 'V',
            0xC => 'P',
            0xD => 'L',
            0xE => 'R',
            0xF => 'M',
            _ => ' ',
        }
    }

    pub fn effect_letter(&self) -> char {
        match self.effect_type {
            0x0 => '0',
            0x1 => '1',
            0x2 => '2',
            0x3 => '3',
            0x4 => '4',
            0x5 => '5',
            0x6 => '6',
            0x7 => '7',
            0x8 => '8',
            0x9 => '9',
            0xA => 'A',
            0xB => 'B',
            0xC => 'C',
            0xD => 'D',
            0xE => 'E',
            0xF => 'F',
            0x10 => 'G',
            0x11 => 'H',
            0x12 => 'I',
            0x13 => 'J',
            0x14 => 'K',
            0x15 => 'L',
            0x16 => 'M',
            0x17 => 'N',
            0x18 => 'O',
            0x19 => 'P',
            0x1A => 'Q',
            0x1B => 'R',
            0x1C => 'S',
            0x1D => 'T',
            0x1E => 'U',
            0x1F => 'V',
            0x20 => 'W',
            0x21 => 'X',
            _ => '0',
        }
    }
}
