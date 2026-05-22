//! Cursor-based binary reader/writer for historical tracker formats.
//!
//! Replaces the previous `bincode::serde::decode_from_slice` /
//! `encode_to_vec` calls used in the importer chain. Tracker files
//! store fixed-layout C-packed structs (little-endian primitives,
//! `[u8; N]` for fixed-size name fields, no length prefixes), which
//! is not bincode's actual wire format — `bincode::config::legacy()`
//! happened to match by coincidence for simple headers, but the
//! mapping is fragile and ties the importer's correctness to
//! bincode's internal layout. Direct LE parsing is both lossless
//! and self-documenting: a header definition becomes a sequence of
//! `read_u16_le()` / `read_array::<22>()` calls that mirror the
//! tracker format spec line-for-line.
//!
//! ```
//! use xmrs::import::bin_reader::{BinReader, ImportError};
//!
//! let bytes: &[u8] = &[0x34, 0x12, 0xff, b'a', b'b', b'c', 0, 0];
//! let mut r = BinReader::new(bytes);
//! assert_eq!(r.read_u16_le()?, 0x1234);
//! assert_eq!(r.read_u8()?, 0xff);
//! let name: [u8; 5] = r.read_array()?;
//! assert_eq!(&name, b"abc\0\0");
//! # Ok::<(), ImportError>(())
//! ```

use alloc::string::String;
use alloc::vec::Vec;

/// Errors emitted by the import-time binary parsers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// The reader was asked for more bytes than remain in the input.
    UnexpectedEof,
    /// A magic / signature field did not match the expected value.
    /// The string identifies which check failed (e.g. `"XmHeader id_text"`).
    InvalidMagic(&'static str),
    /// A length / index / count field exceeds the slice it indexes
    /// into (e.g. an `XmHeader::header_size` that overshoots).
    OutOfRange(&'static str),
    /// Catch-all for format-specific consistency failures. The
    /// string is informational — kept `&'static str` to remain
    /// allocation-free on the hot import path.
    Other(&'static str),
}

impl core::fmt::Display for ImportError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ImportError::UnexpectedEof => f.write_str("unexpected end of input"),
            ImportError::InvalidMagic(s) => write!(f, "invalid magic: {s}"),
            ImportError::OutOfRange(s) => write!(f, "out of range: {s}"),
            ImportError::Other(s) => write!(f, "{s}"),
        }
    }
}

/// Forward-only cursor over a byte slice with little-endian
/// primitive readers. The cursor is always `<= data.len()`; every
/// read advances it on success and leaves it untouched on failure.
pub struct BinReader<'a> {
    data: &'a [u8],
    cursor: usize,
}

impl<'a> BinReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, cursor: 0 }
    }

    /// Bytes remaining ahead of the cursor.
    pub fn remaining(&self) -> usize {
        self.data.len() - self.cursor
    }

    /// Bytes already consumed (i.e. cursor position).
    pub fn consumed(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// Borrow the slice that has not yet been consumed. Useful for
    /// handing off the remainder to a sub-parser that does not need
    /// the cursor.
    pub fn tail(&self) -> &'a [u8] {
        &self.data[self.cursor..]
    }

    pub fn skip(&mut self, n: usize) -> Result<(), ImportError> {
        if n > self.remaining() {
            return Err(ImportError::UnexpectedEof);
        }
        self.cursor += n;
        Ok(())
    }

    pub fn read_u8(&mut self) -> Result<u8, ImportError> {
        if self.remaining() < 1 {
            return Err(ImportError::UnexpectedEof);
        }
        let v = self.data[self.cursor];
        self.cursor += 1;
        Ok(v)
    }

    pub fn read_i8(&mut self) -> Result<i8, ImportError> {
        self.read_u8().map(|b| b as i8)
    }

    pub fn read_u16_le(&mut self) -> Result<u16, ImportError> {
        let a: [u8; 2] = self.read_array()?;
        Ok(u16::from_le_bytes(a))
    }

    pub fn read_u32_le(&mut self) -> Result<u32, ImportError> {
        let a: [u8; 4] = self.read_array()?;
        Ok(u32::from_le_bytes(a))
    }

    /// Big-endian variants for Motorola/68k-era formats. The Amiga
    /// MOD header stores all multi-byte integers big-endian; every
    /// other tracker format in this crate is little-endian.
    pub fn read_u16_be(&mut self) -> Result<u16, ImportError> {
        let a: [u8; 2] = self.read_array()?;
        Ok(u16::from_be_bytes(a))
    }

    /// Read exactly `N` bytes into a fixed-size array. Common for
    /// tracker name fields stored as `[u8; 20]` / `[u8; 22]`.
    pub fn read_array<const N: usize>(&mut self) -> Result<[u8; N], ImportError> {
        if self.remaining() < N {
            return Err(ImportError::UnexpectedEof);
        }
        let mut out = [0u8; N];
        out.copy_from_slice(&self.data[self.cursor..self.cursor + N]);
        self.cursor += N;
        Ok(out)
    }

    /// Read `n` bytes as a borrowed slice (no allocation).
    pub fn read_slice(&mut self, n: usize) -> Result<&'a [u8], ImportError> {
        if self.remaining() < n {
            return Err(ImportError::UnexpectedEof);
        }
        let s = &self.data[self.cursor..self.cursor + n];
        self.cursor += n;
        Ok(s)
    }

    /// Read `n` bytes into an owned `Vec`. Use [`Self::read_slice`]
    /// when the caller can hold a borrow.
    pub fn read_vec(&mut self, n: usize) -> Result<Vec<u8>, ImportError> {
        Ok(self.read_slice(n)?.to_vec())
    }
}

/// Decode a fixed-size byte array into a UTF-8-lossy `String`,
/// stripping trailing NULs and ASCII whitespace. Matches the
/// behaviour of the previous `serde_helper::deserialize_string_*`
/// family.
pub fn bytes_to_trimmed_string<const N: usize>(bytes: &[u8; N]) -> String {
    let s = String::from_utf8_lossy(bytes);
    s.trim_matches(char::from(0)).trim().into()
}

// =====================================================================
// Write side
// =====================================================================

/// Width of a UTF-8 character whose first byte is `ch`. Returns 0
/// for invalid leading bytes (continuation / reserved); the writer
/// uses this to avoid splitting a multi-byte codepoint when the
/// fixed-size name field can only fit a partial sequence.
fn utf8_char_width(ch: u8) -> usize {
    match ch {
        0..=127 => 1,
        128..=191 => 0,
        192..=223 => 2,
        224..=239 => 3,
        240..=247 => 4,
        248..=255 => 0,
    }
}

/// Encode `s` into a fixed-size byte array, truncating at the last
/// complete UTF-8 codepoint that fits in `N`. NUL-padded on the
/// right. Matches `serde_helper::serialize_string_*`.
pub fn string_to_fixed_array<const N: usize>(s: &str) -> [u8; N] {
    let bytes = s.as_bytes();
    let mut count = 0usize;
    let mut i = 0usize;
    while i < bytes.len() && count < N {
        let w = utf8_char_width(bytes[i]);
        if w == 0 || count + w > N {
            break;
        }
        count += w;
        i += w;
    }
    let mut out = [0u8; N];
    out[..i].copy_from_slice(&bytes[..i]);
    out
}

/// Append the little-endian bytes of `v` to `out`.
pub fn write_u8(out: &mut Vec<u8>, v: u8) {
    out.push(v);
}

pub fn write_i8(out: &mut Vec<u8>, v: i8) {
    out.push(v as u8);
}

pub fn write_u16_le(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub fn write_u32_le(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub fn write_bytes(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(b);
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn read_primitives_roundtrip() {
        let mut buf: Vec<u8> = Vec::new();
        write_u8(&mut buf, 0x12);
        write_u16_le(&mut buf, 0xBEEF);
        write_u32_le(&mut buf, 0xDEAD_C0DE);
        write_i8(&mut buf, -42);

        let mut r = BinReader::new(&buf);
        assert_eq!(r.read_u8().unwrap(), 0x12);
        assert_eq!(r.read_u16_le().unwrap(), 0xBEEF);
        assert_eq!(r.read_u32_le().unwrap(), 0xDEAD_C0DE);
        assert_eq!(r.read_i8().unwrap(), -42);
        assert!(r.is_empty());
    }

    #[test]
    fn fixed_array_read() {
        let buf = [0x00, 0x01, 0x02, 0x03, 0x04];
        let mut r = BinReader::new(&buf);
        let arr: [u8; 5] = r.read_array().unwrap();
        assert_eq!(arr, [0, 1, 2, 3, 4]);
        assert!(r.is_empty());
    }

    #[test]
    fn unexpected_eof_does_not_advance_cursor() {
        let buf = [0xAA];
        let mut r = BinReader::new(&buf);
        assert_eq!(r.read_u8().unwrap(), 0xAA);
        assert_eq!(r.read_u8().unwrap_err(), ImportError::UnexpectedEof);
        assert_eq!(r.consumed(), 1); // cursor stayed put on failure
    }

    #[test]
    fn string_round_trip_trims_nul_and_whitespace() {
        let arr = string_to_fixed_array::<10>("Hi");
        assert_eq!(&arr, b"Hi\0\0\0\0\0\0\0\0");
        assert_eq!(bytes_to_trimmed_string(&arr), "Hi");
    }

    #[test]
    fn string_encoder_respects_utf8_boundary() {
        // "café" = 5 bytes in UTF-8 (c, a, f, é=0xc3 0xa9).
        // Asked to fit in 4 → must truncate before the `é` to avoid
        // splitting the multi-byte codepoint.
        let arr = string_to_fixed_array::<4>("café");
        assert_eq!(&arr, b"caf\0");
    }

    #[test]
    fn read_slice_borrows_input_lifetime() {
        let buf = [1u8, 2, 3, 4, 5];
        let mut r = BinReader::new(&buf);
        let s = r.read_slice(3).unwrap();
        assert_eq!(s, &[1, 2, 3]);
        assert_eq!(r.remaining(), 2);
    }

    #[test]
    fn skip_advances_cursor_then_reads() {
        let buf = [1u8, 2, 3, 4];
        let mut r = BinReader::new(&buf);
        r.skip(2).unwrap();
        assert_eq!(r.read_u8().unwrap(), 3);
    }

    #[test]
    fn skip_past_end_fails() {
        let buf = [1u8, 2];
        let mut r = BinReader::new(&buf);
        assert_eq!(r.skip(3).unwrap_err(), ImportError::UnexpectedEof);
    }

    #[test]
    fn import_error_display() {
        assert_eq!(
            ImportError::InvalidMagic("XmHeader").to_string(),
            "invalid magic: XmHeader"
        );
    }
}
