#![forbid(unsafe_code)]

//! TFMX (The Final Musicsystem eXtended) structures and header definitions.
//!
//! Formats supported:
//! - Standard TFMX 1.5 (`TFMX-SONG `)
//! - TFMX Pro / 7-Voice (`TFMX-SONG ` with 7V trackstep or prescale)
//! - TFHD single-file container (extracts mdat + smpl sections)

use alloc::string::String;
use alloc::vec::Vec;

/// TFMX magic identifiers.
pub const MAGIC_TFMX_SONG: &[u8; 10] = b"TFMX-SONG ";
pub const MAGIC_TFMX_SONG_CR: &[u8; 10] = b"TFMX-SONG\r";
pub const MAGIC_TFMXSONG: &[u8; 10] = b"tfmxsong\0\0";
pub const MAGIC_TFHD: &[u8; 4] = b"TFHD";
pub const MAGIC_TFMX_MOD: &[u8; 8] = b"TFMX-MOD";

/// A single trackstep instruction row (16 bytes = 8 big-endian 16-bit words).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TfmxTrackStep {
    pub words: [u16; 8],
}

impl TfmxTrackStep {
    #[inline]
    pub fn is_command(&self) -> bool {
        self.words[0] == 0xEFFE
    }

    #[inline]
    pub fn command_type(&self) -> u16 {
        self.words[1]
    }
}

/// A parsed subsong descriptor in the TFMX header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TfmxSubSong {
    pub index: usize,
    pub start_pos: u16,
    pub end_pos: u16,
    pub tempo: u16,
}

/// A single 4-byte sound macro step instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TfmxMacroStep {
    pub op: u8,
    pub b1: u8,
    pub b2: u8,
    pub b3: u8,
}

impl TfmxMacroStep {
    #[inline]
    pub fn from_bytes(bytes: [u8; 4]) -> Self {
        Self {
            op: bytes[0],
            b1: bytes[1],
            b2: bytes[2],
            b3: bytes[3],
        }
    }
}

/// Sound macro definition (sequence of 4-byte steps).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TfmxMacro {
    pub index: usize,
    pub offset: u32,
    pub steps: Vec<TfmxMacroStep>,
}

/// Pattern command entry (up to 4 bytes: note, macro, data/wait).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TfmxPatternCmd {
    pub note: u8,
    pub macro_num: u8,
    pub detune_or_wait: u8,
}

/// Parsed pattern containing raw bytecode bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TfmxPattern {
    pub index: usize,
    pub offset: u32,
    pub bytes: Vec<u8>,
}

/// Parsed TFMX module (header, tracksteps, patterns, macros, and sample PCM).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TfmxModule {
    pub title: String,
    pub text_lines: Vec<String>,
    pub is_7voice: bool,
    pub subsongs: Vec<TfmxSubSong>,
    pub tracksteps: Vec<TfmxTrackStep>,
    pub pat_offsets: Vec<u32>,
    pub mac_offsets: Vec<u32>,
    pub patterns: Vec<TfmxPattern>,
    pub macros: Vec<TfmxMacro>,
    pub mdat_bytes: Vec<u8>,
    pub sample_data: Vec<u8>,
}

impl TfmxModule {
    /// Returns `true` if `data` starts with a single-file TFMX container magic (`TFHD` or `TFMX-MOD`).
    #[inline]
    pub fn is_single_file_container(data: &[u8]) -> bool {
        data.starts_with(MAGIC_TFHD) || data.starts_with(MAGIC_TFMX_MOD)
    }

    /// Returns `true` if `data` begins with any recognized TFMX header or container signature.
    #[inline]
    pub fn is_tfmx_magic(data: &[u8]) -> bool {
        Self::is_single_file_container(data)
            || data.starts_with(MAGIC_TFMX_SONG)
            || data.starts_with(MAGIC_TFMX_SONG_CR)
            || data.starts_with(MAGIC_TFMXSONG)
            || data.starts_with(b"TFMX_SONG")
            || data.starts_with(b"TFMX_1.5")
    }

    /// Returns `true` if `filename` follows a standard TFMX naming convention (`.tfx`, `.tfm`, `mdat.*`).
    #[inline]
    pub fn is_tfmx_filename(filename: &str) -> bool {
        let trimmed = filename.trim();
        let name = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
        name.ends_with(".tfx")
            || name.ends_with(".TFX")
            || name.ends_with(".tfm")
            || name.ends_with(".TFM")
            || name.starts_with("mdat.")
            || name.starts_with("MDAT.")
    }

    /// Returns `true` if either `data` has a TFMX magic signature or `filename` has a TFMX naming pattern.
    #[inline]
    pub fn is_tfmx(data: &[u8], filename: Option<&str>) -> bool {
        Self::is_tfmx_magic(data) || filename.is_some_and(Self::is_tfmx_filename)
    }

    #[inline]
    pub fn read_pattern_step(&self, pat_num: usize, step_num: usize) -> Option<[u8; 4]> {
        let &off = self.pat_offsets.get(pat_num)?;
        let start = (off as usize).checked_add(step_num.checked_mul(4)?)?;
        self.mdat_bytes.get(start..start + 4)?.try_into().ok()
    }

    #[inline]
    pub fn read_macro_step(&self, mac_num: usize, step_num: usize) -> Option<[u8; 4]> {
        let &off = self.mac_offsets.get(mac_num)?;
        let start = (off as usize).checked_add(step_num.checked_mul(4)?)?;
        self.mdat_bytes.get(start..start + 4)?.try_into().ok()
    }
}
