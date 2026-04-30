use super::serde_helper::deserialize_string_22;
use bincode::error::DecodeError;
use serde::Deserialize;

use alloc::string::String;
use core::fmt;

use crate::fixed::units::{ChannelVolume, Finetune, Panning, Volume};
use crate::prelude::*;

#[derive(Default, Deserialize)]
pub struct AmigaSample {
    #[serde(deserialize_with = "deserialize_string_22")]
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
    pub fn load(ser_sample: &[u8]) -> Result<(&[u8], Self), DecodeError> {
        match bincode::serde::decode_from_slice::<AmigaSample, _>(
            ser_sample,
            bincode::config::legacy(),
        ) {
            Ok((mut aspl, _)) => {
                aspl.length_div2 = aspl.length_div2.rotate_right(8);
                aspl.repeat_offset_div2 = aspl.repeat_offset_div2.rotate_right(8);
                aspl.repeat_length_div2 = aspl.repeat_length_div2.rotate_right(8);
                Ok((&ser_sample[30..], aspl))
            }
            Err(e) => Err(e),
        }
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
