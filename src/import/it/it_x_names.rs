use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use bincode::error::DecodeError;
use serde::Deserialize;

#[derive(Deserialize, Debug, Default)]
#[repr(C)]
pub struct ItXNames;

impl ItXNames {
    pub fn load(source: &[u8], chunk_size: usize) -> Result<(Vec<String>, usize), DecodeError> {
        let data = source;

        if data.len() < 4 {
            return Err(DecodeError::LimitExceeded);
        }

        let length = u32::from_le_bytes(data[0..4].try_into().unwrap());
        if length == 0 {
            return Ok((vec![], 4));
        }
        let data = &data[4..4 + length as usize];

        if data.len() < length as usize {
            return Err(DecodeError::ArrayLengthMismatch {
                required: length as usize,
                found: data.len(),
            });
        }

        let dest: Vec<String> = data
            .chunks(chunk_size)
            .map(|chunk| {
                String::from_utf8_lossy(chunk)
                    .trim_matches(char::from(0))
                    .trim()
                    .to_string()
            }) // Convertit chaque chunk en String
            .collect();

        Ok((dest, 4 + length as usize))
    }

    pub fn is_pnam(data: &[u8]) -> bool {
        data[0] == b'P' && data[1] == b'N' && data[2] == b'A' && data[3] == b'M'
    }

    pub fn is_cnam(data: &[u8]) -> bool {
        data[0] == b'C' && data[1] == b'N' && data[2] == b'A' && data[3] == b'M'
    }
}
