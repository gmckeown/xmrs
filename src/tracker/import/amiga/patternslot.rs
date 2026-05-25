//! MOD pattern-cell byte decoder — unpacks the 4-byte
//! ProTracker `(period, instrument, effect, parameter)` quintuplet
//! into a [`PatternSlot`] then converts to the format-agnostic
//! `TrackImportUnit` shape.

use crate::core::cell_note::CellNote;
use crate::core::pitch::Pitch;
pub use crate::tracker::import::patternslot::PatternSlot;

impl PatternSlot {
    /// Standard Amiga period table (7 octaves, notes 1-84, descending periods)
    const AMIGA_PERIODS: [u16; 84] = [
        6848, 6464, 6096, 5760, 5424, 5120, 4832, 4560, 4304, 4064, 3840, 3624, 3424, 3232, 3048,
        2880, 2712, 2560, 2416, 2280, 2152, 2032, 1920, 1812, 1712, 1616, 1524, 1440, 1356, 1280,
        1208, 1140, 1076, 1016, 960, 906, 856, 808, 762, 720, 678, 640, 604, 570, 538, 508, 480,
        453, 428, 404, 381, 360, 339, 320, 302, 285, 269, 254, 240, 226, 214, 202, 190, 180, 170,
        160, 151, 143, 135, 127, 120, 113, 107, 101, 95, 90, 85, 80, 75, 71, 67, 63, 60, 56,
    ];

    fn amiga_pitch(period: u16) -> Option<u8> {
        if period == 0 {
            return Some(0);
        }
        // Exact match first
        if let Ok(i) = Self::AMIGA_PERIODS.binary_search_by(|p| period.cmp(p)) {
            return Some(i as u8 + 1);
        }
        // Nearest match for finetuned/non-standard periods
        let mut best = 0;
        let mut best_diff = u16::MAX;
        for (i, &p) in Self::AMIGA_PERIODS.iter().enumerate() {
            let diff = period.abs_diff(p);
            if diff < best_diff {
                best_diff = diff;
                best = i as u8 + 1;
            }
        }
        if best > 0 {
            Some(best)
        } else {
            None
        }
    }

    /*
        0bIIII_PPPPPPPPPPPP_IIII_EEEE_DDDDDDDD
        P: period
        I: instrument number
        E: effect number
        D: effect data
    */
    pub fn deserialize(input: u32) -> Self {
        let period = ((input >> 16) & 0x0FFF) as u16;
        let instrument_high = ((input >> (32 - 4)) & 0x000F) as u8;
        let instrument_low = ((input >> 12) & 0x000F) as u8;
        let instrument = (instrument_high << 4) | instrument_low;
        let effect = ((input >> 8) & 0x000F) as u8;
        let data = (input & 0x00FF) as u8;

        // period may not match an Amiga table entry; default to 0 in that case.
        let nu8 = Self::amiga_pitch(period).unwrap_or_default();

        let note: CellNote = if nu8 == 0 {
            CellNote::Empty
        } else {
            match Pitch::try_from(nu8 - 1) {
                Ok(p) => CellNote::Play(p),
                Err(_) => CellNote::Empty,
            }
        };

        let instrument = if instrument == 0 {
            None
        } else {
            Some(instrument as usize - 1)
        };

        Self {
            note,
            instrument,
            volume: 0,
            effect_type: effect,
            effect_parameter: data,
        }
    }
}
