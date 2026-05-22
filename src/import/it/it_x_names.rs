use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::import::bin_reader::ImportError;

#[derive(Debug, Default)]
pub struct ItXNames;

impl ItXNames {
    pub fn load(source: &[u8], chunk_size: usize) -> Result<(Vec<String>, usize), ImportError> {
        let data = source;

        if data.len() < 4 {
            return Err(ImportError::UnexpectedEof);
        }

        let length = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
        if length == 0 {
            return Ok((vec![], 4));
        }
        if data.len() < 4 + length {
            return Err(ImportError::OutOfRange("ItXNames length overshoots input"));
        }
        let body = &data[4..4 + length];

        let dest: Vec<String> = body
            .chunks(chunk_size)
            .map(|chunk| {
                String::from_utf8_lossy(chunk)
                    .trim_matches(char::from(0))
                    .trim()
                    .to_string()
            })
            .collect();

        Ok((dest, 4 + length))
    }

    pub fn is_pnam(data: &[u8]) -> bool {
        data[0] == b'P' && data[1] == b'N' && data[2] == b'A' && data[3] == b'M'
    }

    pub fn is_cnam(data: &[u8]) -> bool {
        data[0] == b'C' && data[1] == b'N' && data[2] == b'A' && data[3] == b'M'
    }
}
