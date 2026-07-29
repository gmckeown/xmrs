//! XM-format pattern-cell decoder — unpacks the compressed
//! 5-byte slot (`note, instrument, volume_column, effect_type,
//! effect_parameter`) into a `PatternSlot` then `TrackImportUnit`.
//! XM uses a per-cell mask byte to omit unchanged columns; this
//! decoder honours it.

use crate::core::cell_note::CellNote;
use crate::core::pitch::Pitch;
use crate::tracker::import::bin_reader::ImportError;
use crate::tracker::import::patternslot::PatternSlot;

impl PatternSlot {
    pub fn load_xm(src: &[u8]) -> Result<(&[u8], PatternSlot), ImportError> {
        let mut dst: [u8; 5] = [0; 5];
        let mut i = 0;

        let take = |i: &mut usize| -> Result<u8, ImportError> {
            let b = *src.get(*i).ok_or(ImportError::UnexpectedEof)?;
            *i += 1;
            Ok(b)
        };

        let note = take(&mut i)?;
        if note & 0b1000_0000 != 0 {
            for (j, mask) in [
                0b0000_0001,
                0b0000_0010,
                0b0000_0100,
                0b0000_1000,
                0b0001_0000,
            ]
            .iter()
            .enumerate()
            {
                if note & mask != 0 {
                    dst[j] = take(&mut i)?;
                }
            }
        } else {
            dst[0] = note;
            dst[1] = take(&mut i)?;
            dst[2] = take(&mut i)?;
            dst[3] = take(&mut i)?;
            dst[4] = take(&mut i)?;
        }

        Ok((
            &src[i..],
            PatternSlot {
                note: {
                    if dst[0] == 97 {
                        // Special case: we don't want to use 97,
                        // because we want more octaves...
                        CellNote::KeyOff
                    } else if dst[0] == 0 {
                        // Special case: we don't want to use 0, so
                        // pitches start at 1 in XM. 0 = empty cell.
                        CellNote::Empty
                    } else {
                        match Pitch::try_from(dst[0] - 1) {
                            Ok(p) => CellNote::Play(p),
                            Err(_) => CellNote::Empty,
                        }
                    }
                },
                instrument: {
                    if dst[1] != 0 {
                        Some(dst[1] as usize - 1)
                    } else {
                        None
                    }
                },
                volume: dst[2],
                effect_type: dst[3],
                effect_parameter: dst[4],
            },
        ))
    }
}
