use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;
use bincode::error::DecodeError;
use serde::Deserialize;

use crate::import::patternslot::PatternSlot;
use crate::pitch::Pitch;

/// Sentinel placed in `PatternSlot::volume` for cells whose IT
/// channel-mask omits both bit 0x04 ("read vol-col byte") and 0x40
/// ("reuse last vol-col"). 0xFF is outside every valid IT vol-column
/// byte (the highest legal value is 212).
///
/// We need an explicit sentinel because IT vol-column `0` is itself
/// a real command (set per-note volume to 0). Without one, "no
/// vol-col" and "explicit silence" both end up as `slot.volume == 0`
/// and the downstream parser cannot tell them apart — forcing it to
/// emit a vol-col event on every empty cell, which silences the
/// channel.
pub(crate) const NO_VOL_COL: u8 = 0xFF;

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
        // Initialise every slot with the NO_VOL_COL sentinel so that
        // cells the IT channel-mask never touches stay distinguishable
        // from cells with an explicit vol-col of 0.
        let no_vol_default = PatternSlot {
            volume: NO_VOL_COL,
            ..PatternSlot::default()
        };
        let mut result = vec![vec![no_vol_default; 64]; self.row_count as usize];
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

                let mut slot = no_vol_default;

                if mask_variable & 0x01 != 0 {
                    let mut n = *data_iter.next().ok_or(DecodeError::LimitExceeded)?;
                    // Note bytes the IT pattern stream actually
                    // emits:
                    //   0..=119  → C-0 to B-9
                    //   254      → ^^^  (Note Cut)
                    //   255      → ===  (Note Off)
                    //
                    // 253 ("none") is rarely written: the cell
                    // typically uses the channel-mask bit to say
                    // "no note here" rather than emitting an
                    // explicit byte. Everything in 120..=253 is
                    // out-of-spec for the IT file format proper.
                    //
                    // Schism handles unexpected bytes by folding
                    // them into its internal `NOTE_FADE` state
                    // (`fmt/it.c:284`: "Impulse Tracker handles
                    // all unknown notes as fade internally"). We
                    // do the same here, mapping every byte in
                    // 120..=252 to `Pitch::Fade`. The previous
                    // code mapped them to `None`, which silently
                    // dropped notes some IT-like files emit
                    // (typically OpenMPT extensions or mildly
                    // corrupted streams).
                    //
                    // The numeric values 252/253/254/255 used
                    // below are xmrs-internal discriminants of
                    // the `Pitch` enum, NOT IT file-format codes;
                    // see `pitch.rs` for the rationale.
                    if n > 119 && n < 253 {
                        n = Pitch::Fade as u8;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal 1-row IT pattern from a raw packed-data slice
    /// and return the volume byte of slot 0 after unpack. Used to
    /// verify the NO_VOL_COL sentinel and the explicit-zero
    /// distinction (see bug 1 in the consolidated patch notes).
    fn unpack_one_row(packed: &[u8], row_count: i16) -> Vec<Vec<PatternSlot>> {
        let pat = ItPattern {
            pattern_length: packed.len() as u16,
            row_count,
            reserved: 0,
            packed_data: packed.to_vec(),
        };
        pat.unpack().expect("unpack")
    }

    #[test]
    fn empty_row_marks_volume_with_no_vol_col_sentinel() {
        // A row whose stream is just the end-of-row terminator (0x00)
        // means "no channel touched this row". Every slot must keep
        // the NO_VOL_COL (0xFF) sentinel — NOT 0, which would be
        // indistinguishable from an explicit "vol = 0" command.
        let rows = unpack_one_row(&[0x00], 1);
        for ch in 0..64 {
            assert_eq!(
                rows[0][ch].volume, NO_VOL_COL,
                "channel {ch} should keep NO_VOL_COL sentinel on an empty row"
            );
        }
    }

    #[test]
    fn explicit_zero_volume_is_preserved() {
        // Channel 1, mask byte sets only bit 0x04 ("read vol-col"),
        // value = 0. The slot's volume must be 0 (explicit silence
        // command), not the NO_VOL_COL sentinel — the parser uses
        // the difference to decide whether to emit a vol-col event.
        let packed = &[
            0x80 | 1, // channel 1, with new mask-var
            0x04,     // mask-var: only bit 0x04 (read vol-col byte)
            0x00,     // vol = 0
            0x00,     // end of row
        ];
        let rows = unpack_one_row(packed, 1);
        assert_eq!(rows[0][0].volume, 0, "explicit vol=0 must round-trip as 0");
        // Other channels stay at sentinel
        assert_eq!(rows[0][1].volume, NO_VOL_COL);
    }

    #[test]
    fn explicit_nonzero_volume_is_preserved() {
        let packed = &[
            0x80 | 1, // channel 1, with new mask-var
            0x04,     // mask-var: only bit 0x04
            40,       // vol = 40
            0x00,     // end of row
        ];
        let rows = unpack_one_row(packed, 1);
        assert_eq!(rows[0][0].volume, 40);
    }

    #[test]
    fn cached_volume_recall_uses_last_explicit_value() {
        // Two-row pattern. Row 0 sets vol = 32 explicitly on ch 1.
        // Row 1 has ch 1 with mask = 0x40 (reuse last vol-col).
        let packed = vec![
            0x80 | 1,
            0x04,
            32,
            0x00, // row 0: ch1 vol=32
            0x80 | 1,
            0x40,
            0x00, //       row 1: ch1 reuse last vol
        ];
        let rows = unpack_one_row(&packed, 2);
        assert_eq!(rows[0][0].volume, 32);
        assert_eq!(rows[1][0].volume, 32, "0x40 must recall last explicit vol");
    }
}
