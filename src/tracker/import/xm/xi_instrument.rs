//! Fast Tracker II XI instrument file.
//!
//! Header layout (66 bytes):
//! ```text
//! [u8;21]  id_text ("Extended Instrument: ")
//! [u8;22]  name
//! u8       0x1A (CP/M EOF marker)
//! [u8;20]  tracker_name
//! u16      version_number
//! ```
//! followed by an [`XmInstrDefault`] (215 bytes), 15 reserved bytes,
//! `num_samples:u16`, then `num_samples × XmSampleHeader` + bodies.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::string::ToString;
use alloc::vec;

use crate::tracker::import::bin_reader::{bytes_to_trimmed_string, BinReader, ImportError};

use super::xminstrument::{
    XmInstrDefault, XmInstrument, XmInstrumentHeader, XmInstrumentType, XMINSTRUMENT_HEADER_SIZE,
};
use super::xmsample::{XmSample, XMSAMPLE_HEADER_SIZE};

#[derive(Debug)]
pub struct XiInstrumentHeader {
    id_text: String, // "Extended Instrument: "
    name: String,
    right_arrow: u8, // 0x1A CP/M EOF marker
    tracker_name: String,
    version_number: u16,
}

impl Default for XiInstrumentHeader {
    fn default() -> Self {
        Self {
            id_text: "Extended Instrument: ".to_string(),
            name: "".to_string(),
            right_arrow: 0x1A,
            tracker_name: "XMrs".to_string(),
            version_number: 0x0104,
        }
    }
}

#[derive(Debug)]
pub struct XiInstrument {
    header: XiInstrumentHeader,
    instr: XmInstrDefault,
    reserved: [u8; 15],
    num_samples: u16,
}

impl XiInstrument {
    pub fn load(data: &[u8]) -> Result<XmInstrument, ImportError> {
        let mut r = BinReader::new(data);

        // XiInstrumentHeader (66 bytes).
        let id_bytes: [u8; 21] = r.read_array()?;
        let name_bytes: [u8; 22] = r.read_array()?;
        let right_arrow = r.read_u8()?;
        let tracker_bytes: [u8; 20] = r.read_array()?;
        let version_number = r.read_u16_le()?;

        let header = XiInstrumentHeader {
            id_text: bytes_to_trimmed_string(&id_bytes),
            name: bytes_to_trimmed_string(&name_bytes),
            right_arrow,
            tracker_name: bytes_to_trimmed_string(&tracker_bytes),
            version_number,
        };
        if header.id_text != "Extended Instrument:" {
            return Err(ImportError::InvalidMagic("Not an Extended Instrument?"));
        }

        let instr = XmInstrDefault::read(&mut r)?;
        let _reserved: [u8; 15] = r.read_array()?;
        let num_samples = r.read_u16_le()?;
        let data_after_header = r.tail();

        let xm_header = XmInstrumentHeader {
            name: header.name,
            instr_type: 0,
            num_samples,
        };

        if num_samples == 0 {
            return Ok(XmInstrument {
                instrument_header_len: 4 + XMINSTRUMENT_HEADER_SIZE as u32,
                header: xm_header,
                sample_header_size: XMSAMPLE_HEADER_SIZE as u32,
                instr: XmInstrumentType::Default(Box::new(instr)),
                sample: vec![],
            });
        }

        // all sample headers, then bodies
        let mut sample = vec![];
        let mut d3 = data_after_header;
        for _ in 0..num_samples {
            // Same rule as the XM path: `num_samples` is file
            // data, so stop at EOF instead of minting up to 65535
            // zeroed headers from nothing (see `XmSample::load`).
            if d3.is_empty() {
                break;
            }
            let (d, s) = XmSample::load(d3)?;
            sample.push(s);
            d3 = d;
        }
        for s in &mut sample {
            let d = s.add_sample(d3)?;
            d3 = d;
        }

        Ok(XmInstrument {
            instrument_header_len: 4 + XMINSTRUMENT_HEADER_SIZE as u32,
            header: xm_header,
            sample_header_size: XMSAMPLE_HEADER_SIZE as u32,
            instr: XmInstrumentType::Default(Box::new(instr)),
            sample,
        })
    }
}
