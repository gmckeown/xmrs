use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;
use bincode::error::DecodeError;
use serde::Deserialize;

use crate::import::patternslot::PatternSlot;
use crate::pitch::Pitch;

/// Structure representing a pattern in a musical tracker format.
/// Note: The entire `Pattern` struct is limited to a maximum size of 0xFFFF (64 kilobytes).
#[derive(Deserialize, Debug, Default)]
#[repr(C)]
pub struct ItPattern {
    /// Length of the packed data in bytes.
    /// Length: 2 bytes
    pattern_length: u16,

    /// Number of rows in the pattern.
    /// Length: 2 bytes (signed)
    /// - IT format allows between 32 and 200 rows.
    /// - OpenMPT may support a larger row count.
    row_count: i16,

    /// Reserved bytes for future use.
    /// Length: 4 bytes
    reserved: u32,

    /// Packed data of the pattern.
    /// Length: Defined by `pattern_length`
    packed_data: Vec<u8>,
}

impl ItPattern {
    pub fn load(source: &[u8]) -> Result<Self, DecodeError> {
        let mut data = source;

        if data.len() < 8 {
            return Err(DecodeError::LimitExceeded);
        }

        let pattern_length: u16 = u16::from_le_bytes(data[0..2].try_into().unwrap());
        let row_count: i16 = i16::from_le_bytes(data[2..4].try_into().unwrap());
        let reserved: u32 = u32::from_le_bytes(data[4..8].try_into().unwrap());

        data = &data[8..];

        if data.len() < pattern_length as usize {
            return Err(DecodeError::LimitExceeded);
        }

        if pattern_length == 0 {
            return Ok(Self {
                pattern_length,
                row_count,
                reserved,
                packed_data: vec![],
            });
        }

        Ok(Self {
            pattern_length,
            row_count,
            reserved,
            packed_data: data[..pattern_length as usize].to_vec(),
        })
    }

    pub fn unpack(&self) -> Result<Vec<Vec<PatternSlot>>, DecodeError> {
        if self.row_count <= 0 {
            return Ok(vec![]);
        }
        let mut result = vec![vec![PatternSlot::default(); 64]; self.row_count as usize];
        let mut last_mask_vars = [0u8; 64];
        // Per-channel last-READ values. ITTECH specifies that the
        // "use last <field> for channel" mask bits (0x10 note, 0x20
        // instrument, 0x40 volume, 0x80 effect) consult the last
        // value the channel actually RECEIVED FROM THE FILE — not
        // the last populated value in any particular row. Populating
        // from the previous row's slot (as a naive implementation
        // might) breaks the chain the first time the channel is
        // absent from a row, because that row's slot stays at
        // PatternSlot::default().
        let mut last_note = [Pitch::None; 64];
        let mut last_instrument: [Option<usize>; 64] = [None; 64];
        let mut last_volume = [0u8; 64];
        let mut last_effect_type = [0u8; 64];
        let mut last_effect_parameter = [0u8; 64];
        let mut data_iter = self.packed_data.iter();

        for row in 0..self.row_count as usize {
            let mut channel_mask = match data_iter.next() {
                Some(&mask) => mask,
                None => break,
            };

            while channel_mask > 0 {
                let channel = (channel_mask - 1) & 63;
                let ch = channel as usize;

                let mask_variable = if channel_mask & 0x80 != 0 {
                    let var = *data_iter.next().ok_or(DecodeError::LimitExceeded)?;
                    last_mask_vars[ch] = var;
                    var
                } else {
                    last_mask_vars[ch]
                };

                let mut slot = PatternSlot::default();

                if mask_variable & 0x01 != 0 {
                    let mut n = *data_iter.next().ok_or(DecodeError::LimitExceeded)?;
                    // Valid note values: 0-119 (C-0 to B-9), 253 (None), 254 (Cut), 255 (Off)
                    // Values 120-252 are out of range (possibly OpenMPT extensions or corrupted data)
                    if n > 119 && n < 253 {
                        n = 253; // None
                    }
                    let note = n.try_into().map_err(|_| {
                        DecodeError::OtherString("Failed to convert note".to_string())
                    })?;
                    slot.note = note;
                    last_note[ch] = note;
                } else if mask_variable & 0x10 != 0 {
                    slot.note = last_note[ch];
                }

                if mask_variable & 0x02 != 0 {
                    let instr = *data_iter.next().ok_or(DecodeError::LimitExceeded)?;
                    let instr_idx = if instr != 0 {
                        Some(instr as usize - 1)
                    } else {
                        None
                    };
                    slot.instrument = instr_idx;
                    last_instrument[ch] = instr_idx;
                } else if mask_variable & 0x20 != 0 {
                    slot.instrument = last_instrument[ch];
                }

                if mask_variable & 0x04 != 0 {
                    let vol = *data_iter.next().ok_or(DecodeError::LimitExceeded)?;
                    slot.volume = vol;
                    last_volume[ch] = vol;
                } else if mask_variable & 0x40 != 0 {
                    slot.volume = last_volume[ch];
                }

                if mask_variable & 0x08 != 0 {
                    let et = *data_iter.next().ok_or(DecodeError::LimitExceeded)?;
                    let ep = *data_iter.next().ok_or(DecodeError::LimitExceeded)?;
                    slot.effect_type = et;
                    slot.effect_parameter = ep;
                    last_effect_type[ch] = et;
                    last_effect_parameter[ch] = ep;
                } else if mask_variable & 0x80 != 0 {
                    slot.effect_type = last_effect_type[ch];
                    slot.effect_parameter = last_effect_parameter[ch];
                }

                result[row][ch] = slot;

                channel_mask = match data_iter.next() {
                    Some(&mask) => mask,
                    None => break,
                };
            }
        }
        Ok(result)
    }
}
