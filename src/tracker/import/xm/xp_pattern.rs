//! FastTracker II Extended Pattern (`.xp`) loader / serializer —
//! the single-pattern XM sidecar format that FT2 produced when
//! you copy-pasted a pattern out of the editor.

use crate::tracker::import::bin_reader::write_u16_le;
use crate::tracker::import::patternslot::PatternSlot;
use alloc::{vec, vec::Vec};

pub struct XpPattern;

impl XpPattern {
    /// XP file must have 32 tracks per row
    pub fn save(pattern: &[Vec<PatternSlot>]) -> Option<Vec<u8>> {
        let mut data: Vec<u8> = vec![];
        let version: u16 = 1;
        let nrow: u16 = pattern.len() as u16;

        if pattern[0].len() != 32 {
            return None;
        }

        write_u16_le(&mut data, version);
        write_u16_le(&mut data, nrow);

        for row in pattern {
            for ps in row {
                let mut d = ps.save_xm_unpack();
                data.append(&mut d);
            }
        }

        Some(data)
    }
}
