/// Original XM Pattern Slot
use crate::import::patternslot::PatternSlot;
use crate::pitch::Pitch;
use bincode::error::DecodeError;

use alloc::vec::Vec;

impl PatternSlot {
    pub fn load_xm(src: &[u8]) -> Result<(&[u8], PatternSlot), DecodeError> {
        let mut dst: [u8; 5] = [0; 5];
        let mut i = 0;
        let mut j = 0;

        let note = src[i];
        i += 1;
        if note & 0b1000_0000 != 0 {
            dst[j] = if note & 0b0000_0001 != 0 {
                i += 1;
                src[i - 1]
            } else {
                0
            };
            j += 1;
            dst[j] = if note & 0b0000_0010 != 0 {
                i += 1;
                src[i - 1]
            } else {
                0
            };
            j += 1;
            dst[j] = if note & 0b0000_0100 != 0 {
                i += 1;
                src[i - 1]
            } else {
                0
            };
            j += 1;
            dst[j] = if note & 0b0000_1000 != 0 {
                i += 1;
                src[i - 1]
            } else {
                0
            };
            j += 1;
            dst[j] = if note & 0b0001_0000 != 0 {
                i += 1;
                src[i - 1]
            } else {
                0
            };
        } else {
            dst[j] = note;
            j += 1;
            dst[j] = src[i];
            i += 1;
            j += 1;
            dst[j] = src[i];
            i += 1;
            j += 1;
            dst[j] = src[i];
            i += 1;
            j += 1;
            dst[j] = src[i];
            i += 1;
        }

        Ok((
            &src[i..],
            PatternSlot {
                note: {
                    if dst[0] == 97 {
                        // Special case: we don't want to use 97, because we want more octaves...
                        Pitch::Off
                    } else if dst[0] == 0 {
                        // Special case: we don't want to use 0, because we want full MIDI compatibility...
                        Pitch::None
                    } else {
                        match Pitch::try_from(dst[0] - 1) {
                            Ok(n) => n,
                            Err(_e) => Pitch::None,
                        }
                    }
                },
                instrument: {
                    if dst[1] != 0 {
                        Some(dst[1] as usize - 1)
                    } else {
                        None
                    }
                },
                volume: dst[2],
                effect_type: dst[3],
                effect_parameter: dst[4],
            },
        ))
    }

    pub fn save_xm_unpack(&self) -> Vec<u8> {
        let mut bytes: [u8; 5] = [0; 5];
        bytes[0] = {
            if self.note.is_keyoff() {
                97
            } else {
                self.note.value() + 1
            }
        };
        bytes[1] = {
            if let Some(instr) = self.instrument {
                (instr + 1) as u8
            } else {
                0
            }
        };
        bytes[2] = self.volume;
        bytes[3] = self.effect_type;
        bytes[4] = self.effect_parameter;
        bytes.to_vec()
    }

    pub fn save_xm(&self) -> Vec<u8> {
        let mut bytes: [u8; 5] = [0; 5];
        bytes[0] = {
            if self.note.is_keyoff() {
                97
            } else {
                self.note.value() + 1
            }
        };
        bytes[1] = {
            if let Some(instr) = self.instrument {
                (instr + 1) as u8
            } else {
                0
            }
        };
        bytes[2] = self.volume;
        bytes[3] = self.effect_type;
        bytes[4] = self.effect_parameter;

        let mut dst: [u8; 5] = [0; 5];
        let mut pack_bits = 0;
        let mut i = 1;
        if bytes[0] > 0 {
            pack_bits |= 0b0001;
            dst[i] = bytes[0];
            i += 1;
        } // note
        if bytes[1] > 0 {
            pack_bits |= 0b0010;
            dst[i] = bytes[1];
            i += 1;
        } // instrument
        if bytes[2] > 0 {
            pack_bits |= 0b0100;
            dst[i] = bytes[2];
            i += 1;
        } // volume
        if bytes[3] > 0 {
            pack_bits |= 0b1000;
            dst[i] = bytes[3];
            i += 1;
        } // effect type

        if pack_bits == 15 {
            // first four bits set? no packing needed.
            return bytes.to_vec();
        }

        if bytes[4] > 0 {
            pack_bits |= 16;
            dst[i] = bytes[4];
            i += 1;
        } // effect parameter
        dst[0] = pack_bits | 0b1000_0000;
        dst[0..i].to_vec()
    }
}
