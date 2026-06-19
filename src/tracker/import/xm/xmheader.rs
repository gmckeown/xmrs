//! Original XM Header.
//!
//! On-disk layout (80 bytes), exactly as written by FastTracker II:
//!
//! | Off | Size | Field                  |
//! |-----|------|------------------------|
//! | 0   | 17   | `id_text` ("Extended Module: ") |
//! | 17  | 20   | `name` |
//! | 37  | 1    | `0x1A` (CP/M right-arrow / EOF) |
//! | 38  | 20   | `tracker_name` |
//! | 58  | 2    | `version_number` |
//! | 60  | 4    | `header_size` (from this offset onward) |
//! | 64  | 2    | `song_length` |
//! | 66  | 2    | `restart_position` |
//! | 68  | 2    | `number_of_channels` |
//! | 70  | 2    | `number_of_patterns` |
//! | 72  | 2    | `number_of_instruments` |
//! | 74  | 2    | `flags` (0 = Amiga freq, 1 = linear) |
//! | 76  | 2    | `default_tempo` |
//! | 78  | 2    | `default_bpm` |

use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;

use crate::tracker::import::bin_reader::{bytes_to_trimmed_string, BinReader, ImportError};

#[derive(Copy, Clone, Debug)]
#[repr(u16)]
pub enum XmFlagType {
    XmAmigaFrequencies = 0,
    XmLinearFrequencies = 1,
}

impl From<XmFlagType> for u16 {
    #[inline]
    fn from(v: XmFlagType) -> u16 {
        v as u16
    }
}

impl TryFrom<u16> for XmFlagType {
    type Error = u16;
    #[inline]
    fn try_from(v: u16) -> Result<Self, u16> {
        match v {
            0 => Ok(XmFlagType::XmAmigaFrequencies),
            1 => Ok(XmFlagType::XmLinearFrequencies),
            other => Err(other),
        }
    }
}

#[derive(Debug)]
pub struct XmHeader {
    id_text: String, // "Extended Module: "
    pub name: String,
    right_arrow: u8, // 0x1A on IBM437 charset matches →
    pub tracker_name: String,
    pub version_number: u16,
    pub header_size: u32, // 20 (starting from here to PatternOrder start) + pattern size (default 256, can be less...or more)
    song_length: u16,     // pattern order table "size" in bytes
    pub restart_position: u16, // PatternOrder index
    pub number_of_channels: u16, // 0..32/64
    pub number_of_patterns: u16, // 1..256
    pub number_of_instruments: u16, // 0..128
    pub flags: XmFlagType,
    pub default_tempo: u16,
    pub default_bpm: u16,
}

impl Default for XmHeader {
    fn default() -> Self {
        Self {
            id_text: "Extended Module: ".to_string(),
            name: "".to_string(),
            right_arrow: 0x1A,
            tracker_name: "XMrs".to_string(),
            version_number: 0x0104,
            header_size: 20,
            song_length: 0,
            restart_position: 0,
            number_of_channels: 8,
            number_of_patterns: 0,
            number_of_instruments: 0,
            flags: XmFlagType::XmLinearFrequencies,
            default_tempo: 6,
            default_bpm: 125,
        }
    }
}

impl XmHeader {
    /// Returns `(remaining_bytes, parsed_header, pattern_order_table)`.
    pub fn load(data: &[u8]) -> Result<(&[u8], XmHeader, Vec<u8>), ImportError> {
        let mut r = BinReader::new(data);

        let id_bytes: [u8; 17] = r.read_array()?;
        let name_bytes: [u8; 20] = r.read_array()?;
        let right_arrow = r.read_u8()?;
        let tracker_bytes: [u8; 20] = r.read_array()?;
        let version_number = r.read_u16_le()?;
        let header_size = r.read_u32_le()?;
        let song_length = r.read_u16_le()?;
        let restart_position = r.read_u16_le()?;
        let number_of_channels = r.read_u16_le()?;
        let number_of_patterns = r.read_u16_le()?;
        let number_of_instruments = r.read_u16_le()?;
        let flags_raw = r.read_u16_le()?;
        let default_tempo = r.read_u16_le()?;
        let default_bpm = r.read_u16_le()?;

        let xmh = XmHeader {
            id_text: bytes_to_trimmed_string(&id_bytes),
            name: bytes_to_trimmed_string(&name_bytes),
            right_arrow,
            tracker_name: bytes_to_trimmed_string(&tracker_bytes),
            version_number,
            header_size,
            song_length,
            restart_position,
            number_of_channels,
            number_of_patterns,
            number_of_instruments,
            // Only bit 0 of the flags word is defined (0 = Amiga,
            // 1 = linear frequency table); the upper 15 bits are
            // reserved and some XM files in the wild leave junk there
            // (e.g. 0x4000, 0x2020). FT2 and OpenMPT both read just
            // `flags & 1`, so we mask rather than reject on the raw word.
            flags: XmFlagType::try_from(flags_raw & 1)
                .map_err(|_| ImportError::Other("XmHeader: invalid flags value"))?,
            default_tempo,
            default_bpm,
        };

        if xmh.id_text != "Extended Module:" {
            return Err(ImportError::InvalidMagic("Not an Extended Module?"));
        }

        let (tail, pattern_order) = xmh.get_pattern_order(r.tail())?;
        Ok((tail, xmh, pattern_order))
    }

    fn get_pattern_order<'a>(&self, data: &'a [u8]) -> Result<(&'a [u8], Vec<u8>), ImportError> {
        // Schism / OpenMPT defensive read: an XmHeader whose
        // `header_size` is below the 20-byte minimum makes the
        // subsequent `header_size - 20` subtraction underflow on
        // unsigned arithmetic. Treat anything < 20 as if it were
        // exactly 20 (i.e. no pattern-order extension).
        let extra = (self.header_size as usize).saturating_sub(20);
        if data.len() < extra || (self.song_length as usize) > extra {
            return Err(ImportError::OutOfRange("XmHeader.header_size too big?"));
        }
        let pattern_order = data[0..self.song_length as usize].to_vec();
        Ok((&data[extra..], pattern_order))
    }
}
