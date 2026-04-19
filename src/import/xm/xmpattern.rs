/// Original XM Pattern
use bincode::error::DecodeError;
use serde::{Deserialize, Serialize};

use alloc::{vec, vec::Vec};

use crate::import::patternslot::PatternSlot;

#[derive(Serialize, Deserialize, Debug, Clone)]
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
        let mut ph = XmPatternHeader::default();
        ph.num_rows = size as u16;
        ph
    }

    pub fn load(data: &[u8]) -> Result<(&[u8], XmPatternHeader), DecodeError> {
        match bincode::serde::decode_from_slice::<XmPatternHeader, _>(
            data,
            bincode::config::legacy(),
        ) {
            Ok((xmph, _)) => {
                let hl = xmph.pattern_header_len as usize;
                Ok((&data[hl..], xmph))
            }
            Err(e) => Err(e),
        }
    }
}

#[derive(Default, Serialize, Deserialize, Debug, Clone)]
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

    pub fn load(data: &[u8], number_of_channels: u16) -> Result<(&[u8], XmPattern), DecodeError> {
        let (data, xmph) = XmPatternHeader::load(data)?;
        let (_data_out, xmps) = Self::get_slots(
            &data[0..xmph.pattern_data_size as usize],
            number_of_channels as usize,
            xmph.num_rows as usize,
        )?;
        let seek = xmph.pattern_data_size as usize;

        let xmp = Self {
            header: xmph,
            pattern: xmps,
        };

        Ok((&data[seek..], xmp))
    }

    fn get_empty_line(number_of_channels: usize) -> Vec<PatternSlot> {
        vec![PatternSlot::default(); number_of_channels]
    }

    fn get_slots(
        data: &[u8],
        number_of_channels: usize,
        number_of_rows: usize,
    ) -> Result<(&[u8], Vec<Vec<PatternSlot>>), DecodeError> {
        let mut lines: Vec<Vec<PatternSlot>> = vec![];
        let mut row: Vec<PatternSlot> = vec![];

        let mut d2 = data;
        loop {
            if d2.is_empty() {
                break;
            }
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
