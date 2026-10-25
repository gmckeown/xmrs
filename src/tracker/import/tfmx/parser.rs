#![forbid(unsafe_code)]

//! TFMX binary parser for `mdat.*` (or `.tfx`) and `smpl.*` (or `.sam`).

use alloc::string::String;
use alloc::vec::Vec;

use super::header::{
    TfmxMacro, TfmxMacroStep, TfmxModule, TfmxPattern, TfmxSubSong, TfmxTrackStep,
    MAGIC_TFHD, MAGIC_TFMXSONG, MAGIC_TFMX_MOD, MAGIC_TFMX_SONG, MAGIC_TFMX_SONG_CR,
};
use crate::tracker::import::bin_reader::ImportError;

/// Binary reader helper for big-endian TFMX data streams.
pub struct TfmxReader<'a> {
    data: &'a [u8],
}

impl<'a> TfmxReader<'a> {
    #[inline]
    pub fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    #[inline]
    pub fn u16_be(&self, offset: usize) -> Result<u16, ImportError> {
        if offset + 2 > self.data.len() {
            return Err(ImportError::UnexpectedEof);
        }
        Ok(u16::from_be_bytes([self.data[offset], self.data[offset + 1]]))
    }

    #[inline]
    pub fn u32_be(&self, offset: usize) -> Result<u32, ImportError> {
        if offset + 4 > self.data.len() {
            return Err(ImportError::UnexpectedEof);
        }
        Ok(u32::from_be_bytes([
            self.data[offset],
            self.data[offset + 1],
            self.data[offset + 2],
            self.data[offset + 3],
        ]))
    }

    #[inline]
    pub fn slice(&self, offset: usize, len: usize) -> Result<&'a [u8], ImportError> {
        if offset + len > self.data.len() {
            return Err(ImportError::UnexpectedEof);
        }
        Ok(&self.data[offset..offset + len])
    }
}

impl TfmxModule {
    /// Ingests a paired TFMX module from `mdat` and `smpl` byte slices,
    /// or automatically unpacks single-file containers (TFHD, TFMX-MOD).
    pub fn parse(mdat: &[u8], smpl: &[u8]) -> Result<Self, ImportError> {
        // Transparent single-file container unpacking: TFHD
        if mdat.starts_with(MAGIC_TFHD) {
            if mdat.len() < 18 {
                return Err(ImportError::UnexpectedEof);
            }
            let mdat_off = u32::from_be_bytes([mdat[4], mdat[5], mdat[6], mdat[7]]) as usize;
            let mdat_len = u32::from_be_bytes([mdat[10], mdat[11], mdat[12], mdat[13]]) as usize;
            let smpl_off = mdat_off + mdat_len;
            if mdat_off < 14 || smpl_off > mdat.len() {
                return Err(ImportError::OutOfRange("TFHD header offsets"));
            }
            return Self::parse(&mdat[mdat_off..smpl_off], &mdat[smpl_off..]);
        }

        // Transparent single-file container unpacking: TFMX-MOD
        if mdat.starts_with(MAGIC_TFMX_MOD) {
            if mdat.len() < 20 {
                return Err(ImportError::UnexpectedEof);
            }
            let smpl_off = u32::from_le_bytes([mdat[8], mdat[9], mdat[10], mdat[11]]) as usize;
            let mdat_off = 20;
            if smpl_off <= mdat_off || smpl_off > mdat.len() {
                return Err(ImportError::OutOfRange("TFMX-MOD header offsets"));
            }
            return Self::parse(&mdat[mdat_off..smpl_off], &mdat[smpl_off..]);
        }

        if mdat.len() < 0x200 {
            return Err(ImportError::UnexpectedEof);
        }

        // Validate magic identifier
        let magic = &mdat[0..10.min(mdat.len())];
        if magic != MAGIC_TFMX_SONG
            && magic != MAGIC_TFMX_SONG_CR
            && magic != MAGIC_TFMXSONG
        {
            return Err(ImportError::InvalidMagic("TFMX magic header"));
        }

        let reader = TfmxReader::new(mdat);

        // Read module text lines from header (0x10..0x100: 6 lines of 40 bytes)
        let mut text_lines = Vec::new();
        for l in 0..6 {
            let start = 0x10 + l * 40;
            if start + 40 <= mdat.len() {
                let slice = &mdat[start..start + 40];
                let line_str = String::from_utf8_lossy(slice).trim().to_string();
                if !line_str.is_empty() {
                    text_lines.push(line_str);
                }
            }
        }
        let title = text_lines.first().cloned().unwrap_or_default();

        // Subsong tables at 0x100..0x1C0 (32 starts, 32 ends, 32 tempos)
        let mut subsongs = Vec::with_capacity(32);
        for i in 0..32 {
            let start = reader.u16_be(0x100 + i * 2)?;
            let end = reader.u16_be(0x140 + i * 2)?;
            let tempo = reader.u16_be(0x180 + i * 2)?;

            // A subsong is valid if start <= end and either end > 0 or tempo > 0
            if start <= end && (end > 0 || tempo > 0) {
                subsongs.push(TfmxSubSong {
                    index: i,
                    start_pos: start,
                    end_pos: end,
                    tempo: if tempo >= 0x10 { 4 } else { tempo },
                });
            }
        }

        // Pointers at 0x1D0..0x1DC
        let mut ts_ptr = reader.u32_be(0x1D0)? as usize;
        let mut pat_ptr = reader.u32_be(0x1D4)? as usize;
        let mut mac_ptr = reader.u32_be(0x1D8)? as usize;

        if ts_ptr == 0 {
            ts_ptr = 0x200;
        }
        if pat_ptr == 0 {
            pat_ptr = 0x400;
        }
        if mac_ptr == 0 {
            mac_ptr = 0x600;
        }

        if ts_ptr > mdat.len() || pat_ptr > mdat.len() || mac_ptr > mdat.len() {
            return Err(ImportError::UnexpectedEof);
        }

        // Parse tracksteps (from ts_ptr up to pat_ptr, 16 bytes per step)
        let ts_bytes = if pat_ptr > ts_ptr { pat_ptr - ts_ptr } else { 0 };
        let num_steps = ts_bytes / 16;
        let mut tracksteps = Vec::with_capacity(num_steps);
        let mut is_7voice = false;

        for step_idx in 0..num_steps {
            let step_off = ts_ptr + step_idx * 16;
            let mut words = [0u16; 8];
            for w in 0..8 {
                words[w] = reader.u16_be(step_off + w * 2)?;
            }
            let step = TfmxTrackStep { words };

            // Check if 7-voice mode command is present (0xEFFE 0x0003) or active voices 4..7 used
            if step.is_command() && step.command_type() == 0x0003 {
                is_7voice = true;
            }

            tracksteps.push(step);
        }

        // Parse pattern offsets (from pat_ptr up to mac_ptr, 4 bytes per pointer)
        let pat_table_len = if mac_ptr > pat_ptr { mac_ptr - pat_ptr } else { 0 };
        let num_pats = pat_table_len / 4;
        let mut pat_offsets = Vec::with_capacity(num_pats);
        for p in 0..num_pats {
            pat_offsets.push(reader.u32_be(pat_ptr + p * 4)?);
        }

        // Parse patterns
        let mut patterns = Vec::with_capacity(num_pats);
        for (idx, &off) in pat_offsets.iter().enumerate() {
            let start = off as usize;
            if start >= mdat.len() {
                patterns.push(TfmxPattern {
                    index: idx,
                    offset: off,
                    bytes: Vec::new(),
                });
                continue;
            }

            let mut cursor = start;
            while cursor + 4 <= mdat.len() {
                let b0 = mdat[cursor];
                cursor += 4;
                if b0 == 0xF0 {
                    break;
                }
            }
            let pat_data = mdat[start..cursor].to_vec();
            patterns.push(TfmxPattern {
                index: idx,
                offset: off,
                bytes: pat_data,
            });
        }

        // Parse sound macro offsets (from mac_ptr to end of mdat, 4 bytes per pointer)
        let mac_table_len = mdat.len().saturating_sub(mac_ptr);
        let num_macs = mac_table_len / 4;
        let mut mac_offsets = Vec::with_capacity(num_macs);
        for m in 0..num_macs {
            let off = reader.u32_be(mac_ptr + m * 4)?;
            mac_offsets.push(off);
        }

        // Parse macros (4 bytes per macro step, ends with op 0x07 Stop)
        let mut macros = Vec::with_capacity(mac_offsets.len());
        for (idx, &off) in mac_offsets.iter().enumerate() {
            let mut steps = Vec::new();
            let mut cursor = off as usize;

            while cursor + 4 <= mdat.len() {
                let step = TfmxMacroStep::from_bytes([
                    mdat[cursor],
                    mdat[cursor + 1],
                    mdat[cursor + 2],
                    mdat[cursor + 3],
                ]);
                steps.push(step);
                cursor += 4;
                if (step.op & 0x3F) == 0x07 {
                    break;
                }
            }

            macros.push(TfmxMacro {
                index: idx,
                offset: off,
                steps,
            });
        }

        Ok(TfmxModule {
            title,
            text_lines,
            is_7voice,
            subsongs,
            tracksteps,
            pat_offsets,
            mac_offsets,
            patterns,
            macros,
            mdat_bytes: mdat.to_vec(),
            sample_data: smpl.to_vec(),
        })
    }
}
