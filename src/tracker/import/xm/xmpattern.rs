//! Original XM Pattern.
//!
//! Pattern header layout (9 bytes):
//! `pattern_header_len:u32 packing_type:u8 num_rows:u16 pattern_data_size:u16`.
//!
//! Pattern body: `pattern_data_size` bytes of packed `PatternSlot`s,
//! parsed by [`crate::tracker::import::patternslot::PatternSlot::load_xm`].

use alloc::{vec, vec::Vec};

use crate::tracker::import::bin_reader::{BinReader, ImportError};
use crate::tracker::import::patternslot::PatternSlot;

/// Largest row count accepted for one pattern. FT2 stops at 256;
/// this is OpenMPT's `MAX_PATTERN_ROWS`, i.e. the widest value any
/// mainstream player will read back.
pub(crate) const XM_MAX_PATTERN_ROWS: u16 = 4096;

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
        // FT2 itself never writes more than 256 rows, but the XM
        // field is a `u16` and several trackers (the ModPlug /
        // OpenMPT lineage) do write longer patterns — those files
        // play everywhere except in FT2, so rejecting them loses
        // real music. OpenMPT clamps instead of rejecting
        // (`Load_xm.cpp:263`, `MAX_PATTERN_ROWS`); do the same.
        // The clamp still bounds the empty-row padding in
        // `get_slots` (which pads up to `num_rows` rows of
        // `number_of_channels` slots) — and `XmModule::load` adds a
        // whole-module slot budget on top, because a declared row
        // count costs only 9 header bytes in the file.
        let num_rows = num_rows.min(XM_MAX_PATTERN_ROWS);
        // FT2 has no zero-row pattern: one that declares 0 rows is
        // loaded as an empty 64-row pattern (ft2-clone
        // `ft2_load_xm.c:481` gives *any* empty pattern 64 rows;
        // OpenMPT `Load_xm.cpp:261` does it for the 0 case).
        // Keeping 0 gives the player a pattern it steps through in
        // no time at all — an audible skip where FT2 plays a bar.
        let num_rows = if num_rows == 0 { 64 } else { num_rows };
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
