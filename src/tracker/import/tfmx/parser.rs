#![forbid(unsafe_code)]

//! TFMX binary parser for `mdat.*` (or `.tfx`) and `smpl.*` (or `.sam`).

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::header::{
    TfmxMacro, TfmxMacroStep, TfmxModule, TfmxPattern, TfmxSubSong, TfmxTrackStep, TfmxVariant,
    MAGIC_TFHD, MAGIC_TFMXPAK, MAGIC_TFMXSONG, MAGIC_TFMX_MOD, MAGIC_TFMX_SONG, MAGIC_TFMX_SONG_CR,
    MAGIC_TFMX_V1,
};
use super::tables::crc32_ieee;
use crate::tracker::import::bin_reader::ImportError;

/// Binary reader helper for TFMX data streams.
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
    pub fn slice(&self, offset: usize, len: usize) -> Result<&'a [u8], ImportError> {
        let end = offset.checked_add(len).ok_or(ImportError::UnexpectedEof)?;
        self.data.get(offset..end).ok_or(ImportError::UnexpectedEof)
    }

    #[inline]
    pub fn u16_be(&self, offset: usize) -> Result<u16, ImportError> {
        let bytes: [u8; 2] = self.slice(offset, 2)?.try_into().unwrap();
        Ok(u16::from_be_bytes(bytes))
    }

    #[inline]
    pub fn u32_be(&self, offset: usize) -> Result<u32, ImportError> {
        let bytes: [u8; 4] = self.slice(offset, 4)?.try_into().unwrap();
        Ok(u32::from_be_bytes(bytes))
    }

    #[inline]
    pub fn u32_le(&self, offset: usize) -> Result<u32, ImportError> {
        let bytes: [u8; 4] = self.slice(offset, 4)?.try_into().unwrap();
        Ok(u32::from_le_bytes(bytes))
    }
}

impl TfmxModule {
    /// Ingests a paired TFMX module from `mdat` and `smpl` byte slices,
    /// or automatically unpacks single-file containers (TFHD, TFMX-MOD, TFMXPAK).
    pub fn parse(mdat: &[u8], smpl: &[u8]) -> Result<Self, ImportError> {
        Self::parse_with_hint(mdat, smpl, 0)
    }

    fn parse_with_hint(mdat: &[u8], smpl: &[u8], version_hint: u8) -> Result<Self, ImportError> {
        let reader = TfmxReader::new(mdat);

        // Transparent single-file container unpacking: TFMXPAK
        if mdat.starts_with(MAGIC_TFMXPAK) {
            let header_end = mdat.len().min(32);
            let header_str = core::str::from_utf8(&mdat[..header_end])
                .map_err(|_| ImportError::InvalidMagic("TFMXPAK ASCII header"))?;
            let arrow_pos = header_str
                .find(">>>")
                .ok_or(ImportError::InvalidMagic("TFMXPAK delimiter"))?;
            let nums_part = header_str[8..arrow_pos].trim();
            let mut parts = nums_part.split_whitespace();
            let mdat_len: usize = parts
                .next()
                .and_then(|s| s.parse().ok())
                .ok_or(ImportError::OutOfRange("TFMXPAK mdat size"))?;
            let smpl_len: usize = parts
                .next()
                .and_then(|s| s.parse().ok())
                .ok_or(ImportError::OutOfRange("TFMXPAK smpl size"))?;
            let mdat_off = arrow_pos + 3;
            let smpl_off = mdat_off
                .checked_add(mdat_len)
                .ok_or(ImportError::OutOfRange("TFMXPAK offsets"))?;
            let end_off = smpl_off
                .checked_add(smpl_len)
                .ok_or(ImportError::OutOfRange("TFMXPAK offsets"))?;
            if mdat_len == 0 || smpl_off > mdat.len() {
                return Err(ImportError::OutOfRange("TFMXPAK offsets"));
            }
            let end_clamped = end_off.min(mdat.len());
            return Self::parse_with_hint(&mdat[mdat_off..smpl_off], &mdat[smpl_off..end_clamped], 0);
        }

        // Transparent single-file container unpacking: TFHD
        if mdat.starts_with(MAGIC_TFHD) {
            if mdat.len() < 18 {
                return Err(ImportError::UnexpectedEof);
            }
            let mdat_off = reader.u32_be(4)? as usize;
            let tfhd_version = mdat[8] & 0x7F;
            let mdat_len = reader.u32_be(10)? as usize;
            let smpl_off = mdat_off.checked_add(mdat_len).ok_or(ImportError::OutOfRange("TFHD header offsets"))?;
            if mdat_off < 14 || smpl_off > mdat.len() {
                return Err(ImportError::OutOfRange("TFHD header offsets"));
            }
            return Self::parse_with_hint(&mdat[mdat_off..smpl_off], &mdat[smpl_off..], tfhd_version);
        }

        // Transparent single-file container unpacking: TFMX-MOD
        if mdat.starts_with(MAGIC_TFMX_MOD) {
            if mdat.len() < 20 {
                return Err(ImportError::UnexpectedEof);
            }
            let smpl_off = reader.u32_le(8)? as usize;
            let mdat_off = 20;
            if smpl_off <= mdat_off || smpl_off > mdat.len() {
                return Err(ImportError::OutOfRange("TFMX-MOD header offsets"));
            }
            return Self::parse_with_hint(&mdat[mdat_off..smpl_off], &mdat[smpl_off..], 0);
        }

        if mdat.len() < 0x200 {
            return Err(ImportError::UnexpectedEof);
        }

        // Validate magic identifier
        let magic: &[u8; 10] = mdat[..10].try_into().unwrap();
        let is_v1_magic = mdat.starts_with(MAGIC_TFMX_V1);
        if !is_v1_magic && !matches!(magic, MAGIC_TFMX_SONG | MAGIC_TFMX_SONG_CR | MAGIC_TFMXSONG) {
            return Err(ImportError::InvalidMagic("TFMX magic header"));
        }

        let mut variant = if is_v1_magic || version_hint == 1 {
            TfmxVariant::tfmx_v1()
        } else {
            TfmxVariant::default()
        };

        let mut mdat_vec = mdat.to_vec();
        let mut smpl_vec = smpl.to_vec();
        if smpl_vec.len() >= 4 {
            smpl_vec[0] = 0;
            smpl_vec[1] = 0;
            smpl_vec[2] = 0;
            smpl_vec[3] = 0;
        }

        // Read module text lines from header (0x10..0x100: 6 lines of 40 bytes)
        let mut text_lines = Vec::new();
        for l in 0..6 {
            let start = 0x10 + l * 40;
            if let Some(slice) = mdat.get(start..start + 40) {
                let line_str = String::from_utf8_lossy(slice).trim().to_string();
                if !line_str.is_empty() {
                    text_lines.push(line_str);
                }
            }
        }
        let title = text_lines.first().cloned().unwrap_or_default();

        // Pointers at 0x1D0..0x1DC (with little-endian PC conversion fallback)
        let mut o1 = reader.u32_be(0x1D0)? as usize;
        let mut o2 = reader.u32_be(0x1D4)? as usize;
        let mut o3 = reader.u32_be(0x1D8)? as usize;
        if (o1 | o2 | o3) != 0 && (o1 >= mdat.len() || o2 >= mdat.len() || o3 >= mdat.len()) {
            o1 = (o1 as u32).swap_bytes() as usize;
            o2 = (o2 as u32).swap_bytes() as usize;
            o3 = (o3 as u32).swap_bytes() as usize;
        }

        let (ts_ptr, pat_ptr, mac_ptr, is_uncompressed) = if (o1 | o2 | o3) == 0 {
            (0x800, 0x400, 0x600, true)
        } else {
            (o1, o2, o3, false)
        };

        if ts_ptr > mdat.len() || pat_ptr > mdat.len() || mac_ptr > mdat.len() {
            return Err(ImportError::UnexpectedEof);
        }

        let (num_pats, num_macs, ts_end) = if is_uncompressed {
            let num_pats = 128.min(mdat.len().saturating_sub(pat_ptr) / 4);
            let num_macs = 128.min(mdat.len().saturating_sub(mac_ptr) / 4);
            let p0 = if mdat.len() >= pat_ptr + 4 {
                reader.u32_be(pat_ptr)? as usize
            } else {
                ts_ptr
            };
            let compress_hint = reader.u32_be(0x0C)? as usize;
            let ts_end = if compress_hint != 0 && reader.u16_be(0x0A)? == 1 {
                (ts_ptr + compress_hint)
                    .saturating_sub(0x800 - 0x10)
                    .min(p0)
                    .min(mdat.len())
            } else {
                p0.min(mdat.len())
            };
            (num_pats, num_macs, ts_end)
        } else {
            let num_pats = mac_ptr.saturating_sub(pat_ptr) / 4;
            let num_macs = mdat.len().saturating_sub(mac_ptr) / 4;
            (num_pats, num_macs, pat_ptr)
        };

        // Checksum-based player trait adjustments (`ByChecksum.cpp`)
        if num_pats > 0 && pat_ptr + 4 <= mdat.len() {
            let p0 = reader.u32_be(pat_ptr)? as usize;
            if let Some(crc_slice) = mdat.get(p0..p0 + 0x100) {
                let crc1 = crc32_ieee(crc_slice);
                match crc1 {
                    // Turrican 1
                    0x73868fda | 0x93730029 | 0x6e799869 | 0x3d00fc52 | 0xd76d33ed
                    | 0xb989d570 | 0x8fa80b4e | 0x88f7c34b | 0xb762f2dc => {
                        variant = TfmxVariant::tfmx_v1();
                        variant.vibrato_time_mask = false;
                    }
                    // Danger Freak
                    0x48960d8c | 0x5dcd624f | 0x3f0b151f => {
                        variant = TfmxVariant::tfmx_v1();
                        variant.no_note_detune = true;
                    }
                    // Hard'n'Heavy, R-Type, Rock'n'Roll intro, Grand Monster Slam,
                    // Circus Attractions, Gordian Tomb, Oxxonian, X-Out, Musication Vol. 1
                    0x27f8998c | 0x26447707 | 0xd404651b | 0xb5348633 | 0x8ac70fc8
                    | 0xda279570 | 0xb54457fc | 0x97707404 | 0x5f04d9af | 0x72ef7307
                    | 0xf2292eae | 0x0629665d | 0x9264f036 | 0x22a86efb | 0x711d8520
                    | 0x2f525ee0 | 0x8412d8d5 | 0xc0376a19 | 0xade39424 | 0x2adbf5f9
                    | 0x236d305d | 0x14adce8c | 0x0eed9c91 | 0x1a5d2b53 | 0x22a92c26
                    | 0xe60babf2 => {
                        variant = TfmxVariant::tfmx_v1();
                        if crc1 == 0x0eed9c91 {
                            let end = 0x303e4.min(smpl_vec.len());
                            for b in smpl_vec.iter_mut().take(end).skip(4) {
                                *b = b.wrapping_sub(0x38);
                            }
                        }
                    }
                    // Puzzy
                    0x5fb2f54e => {
                        variant = TfmxVariant::tfmx_v1();
                        if mdat_vec.len() > 0x143 {
                            mdat_vec[0x143] = 0x6F;
                        }
                    }
                    // Sledge Hammer One
                    0xcb3c7113 => {
                        variant = TfmxVariant::tfmx_v1();
                        if mdat_vec.len() > 0x141 && ts_ptr < mdat_vec.len() {
                            mdat_vec[0x101] = 0x04;
                            mdat_vec[0x141] = 0x6B;
                            mdat_vec[ts_ptr] = 0xFF;
                        }
                    }
                    // Ooops Up
                    0x76f8aa6e => {
                        variant.bpm_speed_5 = true;
                    }
                    // Z-Out (fix off-by-one sample start in macro 0x10)
                    0x8648904b | 0xb7f700b2 | 0x0eabbc61 | 0x999c8c6b | 0xdb5e4afc
                    | 0x9d652521 | 0xa1dc3139 => {
                        if num_macs > 0x10 && mac_ptr + 0x44 <= mdat_vec.len() {
                            let mo = u32::from_be_bytes(
                                mdat_vec[mac_ptr + 0x40..mac_ptr + 0x44].try_into().unwrap(),
                            ) as usize;
                            if mo + 8 <= mdat_vec.len()
                                && mdat_vec[mo + 4..mo + 8] == [0x02, 0x00, 0x88, 0x4F]
                            {
                                mdat_vec[mo + 7] = 0x4E;
                            }
                        }
                    }
                    // Bundesliga Manager HATTRICK
                    0x3c2eb450 => {
                        if mdat_vec.len() > 0x103 {
                            mdat_vec[0x102] = 0x01;
                            mdat_vec[0x103] = 0xFF;
                        }
                    }
                    _ => {}
                }
            }
        }

        let reader = TfmxReader::new(&mdat_vec);

        // Parse tracksteps (from ts_ptr up to ts_end, 16 bytes per step)
        let ts_bytes = ts_end.saturating_sub(ts_ptr);
        let num_steps = ts_bytes / 16;
        let mut tracksteps = Vec::with_capacity(num_steps);
        let mut is_7voice = version_hint == 3;

        for step_idx in 0..num_steps {
            let step_off = ts_ptr + step_idx * 16;
            let mut words = [0u16; 8];
            for (w, word) in words.iter_mut().enumerate() {
                *word = reader.u16_be(step_off + w * 2)?;
            }
            let step = TfmxTrackStep { words };

            // Check if 7-voice mode command is present (0xEFFE 0x0003)
            if step.is_command() && step.command_type() == 0x0003 {
                is_7voice = true;
            }

            tracksteps.push(step);
        }

        // Subsong tables at 0x100..0x1C0 (32 starts, 32 ends, 32 tempos)
        let step_max = num_steps as u16;
        let mut subsongs = Vec::with_capacity(32);
        for i in 0..32 {
            let start = reader.u16_be(0x100 + i * 2)?;
            let mut end = reader.u16_be(0x140 + i * 2)?;
            let tempo = reader.u16_be(0x180 + i * 2)?;

            if i == 0 && step_max > 0 && end > step_max {
                end = step_max;
            }
            if start > end
                || start > 0x1FF
                || end > 0x1FF
                || (step_max > 0 && start >= step_max)
                || (i > 0 && (start == 0x1FF || end == 0x1FF))
            {
                continue;
            }
            if end > 0 || tempo > 0 {
                subsongs.push(TfmxSubSong {
                    index: i,
                    start_pos: start,
                    end_pos: end,
                    tempo,
                });
            }
        }

        // Parse pattern offsets
        let mut pat_offsets = Vec::with_capacity(num_pats);
        for p in 0..num_pats {
            pat_offsets.push(reader.u32_be(pat_ptr + p * 4)?);
        }

        // Parse patterns
        let mut patterns = Vec::with_capacity(num_pats);
        for (idx, &off) in pat_offsets.iter().enumerate() {
            let start = off as usize;
            if start >= mdat_vec.len() {
                patterns.push(TfmxPattern {
                    index: idx,
                    offset: off,
                    bytes: Vec::new(),
                });
                continue;
            }

            let mut cursor = start;
            while cursor + 4 <= mdat_vec.len() {
                let b0 = mdat_vec[cursor];
                cursor += 4;
                if b0 == 0xF0 {
                    break;
                }
            }
            let pat_data = mdat_vec[start..cursor].to_vec();
            patterns.push(TfmxPattern {
                index: idx,
                offset: off,
                bytes: pat_data,
            });
        }

        // Parse sound macro offsets
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

            while let Some(chunk) = mdat_vec.get(cursor..cursor + 4) {
                let step = TfmxMacroStep::from_bytes(chunk.try_into().unwrap());
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
            variant,
            subsongs,
            tracksteps,
            pat_offsets,
            mac_offsets,
            patterns,
            macros,
            mdat_bytes: mdat_vec,
            sample_data: smpl_vec,
        })
    }
}
