//! Amiga MOD sample header (30 bytes, big-endian):
//! `name:[u8;22] length_div2:u16BE finetune:u8 volume:u8
//!  repeat_offset_div2:u16BE repeat_length_div2:u16BE`.

use alloc::string::String;
use core::fmt;

use crate::fixed::units::{ChannelVolume, Finetune, Panning, Volume};
use crate::import::bin_reader::{bytes_to_trimmed_string, BinReader, ImportError};
use crate::prelude::*;

#[derive(Default)]
pub struct AmigaSample {
    pub name: String,
    pub length_div2: u16,
    pub finetune: u8,
    pub volume: u8,
    pub repeat_offset_div2: u16,
    pub repeat_length_div2: u16,
}

impl fmt::Debug for AmigaSample {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "Sample: {} (v:{}, f:{}, l:{}, ro:{}, rl:{})",
            self.name,
            self.volume,
            self.finetune,
            2 * self.length_div2 as usize,
            2 * self.repeat_offset_div2 as usize,
            2 * self.repeat_length_div2 as usize
        )
    }
}

impl AmigaSample {
    pub fn load(ser_sample: &[u8]) -> Result<(&[u8], Self), ImportError> {
        let mut r = BinReader::new(ser_sample);
        let name_bytes: [u8; 22] = r.read_array()?;
        let length_div2 = r.read_u16_be()?;
        let finetune = r.read_u8()?;
        let volume = r.read_u8()?;
        let repeat_offset_div2 = r.read_u16_be()?;
        let repeat_length_div2 = r.read_u16_be()?;
        let aspl = AmigaSample {
            name: bytes_to_trimmed_string(&name_bytes),
            length_div2,
            finetune,
            volume,
            repeat_offset_div2,
            repeat_length_div2,
        };
        Ok((r.tail(), aspl))
    }

    pub fn to_sample(&self) -> Sample {
        // Amiga finetune: 4-bit signed nibble (`finetune << 4` to
        // sign-extend through `i8`), normalised by `/127`. The
        // old `.clamp(-1.0, 1.0)` is folded into `Finetune`'s
        // saturating storage.
        let f = Finetune::from_ratio((self.finetune << 4) as i8 as i32, 127);
        let ro = if 2 * (self.repeat_offset_div2 as usize) < 2 * (self.length_div2 as usize) {
            2 * self.repeat_offset_div2 as usize
        } else {
            0_usize
        };
        let rl = if ro + 2 * (self.repeat_length_div2 as usize) <= 2 * (self.length_div2 as usize) {
            2 * self.repeat_length_div2 as usize
        } else {
            0
        };
        let flag = if rl > 2 {
            LoopType::Forward
        } else {
            LoopType::No
        };

        Sample {
            name: self.name.clone(),
            relative_pitch: 0,
            finetune: f,
            volume: ChannelVolume::from_byte_64(self.volume),
            // MOD has no concept of a per-sample default note
            // volume — the sample always triggers at full.
            default_note_volume: Volume::FULL,
            panning: Panning::CENTER,
            loop_flag: flag,
            loop_start: ro as u32,
            loop_length: rl as u32,
            sustain_loop_flag: LoopType::No,
            sustain_loop_start: 0,
            sustain_loop_length: 0,
            data: None,
        }
    }
}
