//! Original XM Pattern.
//!
//! Pattern header layout (9 bytes):
//! `pattern_header_len:u32 packing_type:u8 num_rows:u16 pattern_data_size:u16`.
//!
//! Pattern body: `pattern_data_size` bytes of packed `PatternSlot`s,
//! parsed by [`crate::import::patternslot::PatternSlot::load_xm`].

use alloc::{vec, vec::Vec};

use crate::import::bin_reader::{BinReader, ImportError};
use crate::import::patternslot::PatternSlot;

#[derive(Debug, Clone)]
pub struct XmPatternHeader {
    pattern_header_len: u32,
    packing_type: u8,
    num_rows: u16,
    pattern_data_size: u16,
}

impl Default for XmPatternHeader {
    fn default() -> Self {
        XmPatternHeader {
            pattern_header_len: 9,
            packing_type: 0,
            num_rows: 0,
            pattern_data_size: 0,
        }
    }
}

impl XmPatternHeader {
    pub fn new(size: usize) -> Self {
        XmPatternHeader {
            num_rows: size as u16,
            ..Self::default()
        }
    }

    pub fn load(data: &[u8]) -> Result<(&[u8], XmPatternHeader), ImportError> {
        let mut r = BinReader::new(data);
        let pattern_header_len = r.read_u32_le()?;
        let packing_type = r.read_u8()?;
        let num_rows = r.read_u16_le()?;
        let pattern_data_size = r.read_u16_le()?;
        let xmph = XmPatternHeader {
            pattern_header_len,
            packing_type,
            num_rows,
            pattern_data_size,
        };
        // The declared header length can be > 9 if a future XM
        // revision adds trailing bytes; honor it by skipping ahead
        // exactly that many bytes from the start of the header.
        let hl = xmph.pattern_header_len as usize;
        if hl > data.len() {
            return Err(ImportError::OutOfRange(
                "XmPatternHeader.pattern_header_len overshoots input",
            ));
        }
        Ok((&data[hl..], xmph))
    }
}

#[derive(Default, Debug, Clone)]
pub struct XmPattern {
    pub header: XmPatternHeader,
    pub pattern: Vec<Vec<PatternSlot>>,
}

impl XmPattern {
    pub fn new(rows: usize, noc: usize) -> Self {
        Self {
            header: XmPatternHeader::new(rows),
            pattern: vec![vec![PatternSlot::default(); noc]; rows],
        }
    }

    pub fn load(data: &[u8], number_of_channels: u16) -> Result<(&[u8], XmPattern), ImportError> {
        let (data, xmph) = XmPatternHeader::load(data)?;
        let body_len = xmph.pattern_data_size as usize;
        if body_len > data.len() {
            return Err(ImportError::OutOfRange(
                "XmPattern.pattern_data_size overshoots input",
            ));
        }
        let (_data_out, xmps) = Self::get_slots(
            &data[0..body_len],
            number_of_channels as usize,
            xmph.num_rows as usize,
        )?;
        let xmp = Self {
            header: xmph,
            pattern: xmps,
        };
        Ok((&data[body_len..], xmp))
    }

    fn get_empty_line(number_of_channels: usize) -> Vec<PatternSlot> {
        vec![PatternSlot::default(); number_of_channels]
    }

    fn get_slots(
        data: &[u8],
        number_of_channels: usize,
        number_of_rows: usize,
    ) -> Result<(&[u8], Vec<Vec<PatternSlot>>), ImportError> {
        let mut lines: Vec<Vec<PatternSlot>> = vec![];
        let mut row: Vec<PatternSlot> = vec![];

        let mut d2 = data;
        while !d2.is_empty() {
            let (d3, xps) = PatternSlot::load_xm(d2)?;
            d2 = d3;
            row.push(xps);
            if row.len() == number_of_channels {
                lines.push(row);
                row = vec![];
            }
        }

        while lines.len() < number_of_rows {
            lines.push(Self::get_empty_line(number_of_channels));
        }

        Ok((d2, lines))
    }
}
