//! FastTracker II Extended Track (`.xt`) loader / serializer —
//! the single-channel sidecar format that FT2 produced for
//! cross-pattern channel copy-paste.

use crate::tracker::import::bin_reader::write_u16_le;
use crate::tracker::import::patternslot::PatternSlot;
use alloc::{vec, vec::Vec};

pub struct XtTrack;

impl XtTrack {
    /// Here we use `Vec<PatternSlot>` like a track _not_ like a Pattern row!
    pub fn save(track: &[PatternSlot]) -> Vec<u8> {
        let mut data: Vec<u8> = vec![];
        let version: u16 = 1;
        let nrow: u16 = track.len() as u16;
        write_u16_le(&mut data, version);
        write_u16_le(&mut data, nrow);
        for xmps in track {
            let mut d = xmps.save_xm_unpack();
            data.append(&mut d);
        }
        data
    }
}
