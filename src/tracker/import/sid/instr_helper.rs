//! SID → `Instrument` bridge: wraps a parsed SID voice + its
//! Rob-Hubbard-style effect table into the `InstrumentType::RobSid`
//! variant, which the player renders through the cycle-accurate
//! `sidera` chip (one per instrument).

use crate::prelude::*;
use alloc::format;
use alloc::vec::Vec;

pub struct InstrHelper;

impl InstrHelper {
    fn irs_to_instrument(irs: &InstrRobSid) -> Instrument {
        let mut idst = Instrument::default();
        let v = &irs.voice;
        if v.ctrl_noise {
            idst.name = format!("Noise {:02X}{:02X}", v.ad, v.sr);
        } else if v.ctrl_pulse {
            idst.name = format!("Pulse {:02X}{:02X}", v.ad, v.sr);
        } else if v.ctrl_sawtooth {
            idst.name = format!("Sawtooth {:02X}{:02X}", v.ad, v.sr);
        } else if v.ctrl_triangle {
            idst.name = format!("Triangle {:02X}{:02X}", v.ad, v.sr);
        }
        idst.instr_type = InstrumentType::RobSid(irs.clone());
        idst
    }

    pub fn irss_to_instruments(irss: &[InstrRobSid]) -> Vec<Instrument> {
        irss.iter().map(Self::irs_to_instrument).collect()
    }
}
