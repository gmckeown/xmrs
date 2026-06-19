//! High-level OPL driver: translates tracker-channel gestures into chip
//! calls. A Rust re-implementation of the documented AdLib programming
//! procedure in Schism's `player/snd_fm.c` (note→F-number/block,
//! `OPL_Touch` volume law, `OPL_Pan`, voice allocation) — the procedure
//! is documented hardware programming, not copied code.

use alloc::vec;
use alloc::vec::Vec;

use crate::tracker::instr_opl::{InstrOpl, MdiOpl};

use super::{ChannelPatch, OpPatch, OplChip, OPL_NATIVE_RATE};

/// Per-hardware-slot ownership + the patch's resting carrier/modulator
/// total levels (needed to recompute volume via the `OPL_Touch` law).
#[derive(Clone, Copy, Default)]
struct SlotState {
    owner: Option<usize>, // tracker channel currently holding this slot
    keyed: bool,
    carrier_tl: u8,
    modulator_tl: u8,
    additive: bool,
}

pub struct OplDriver {
    chip: OplChip,
    slots: Vec<SlotState>,
}

impl OplDriver {
    pub fn new(output_rate: u32) -> Self {
        let chip = OplChip::new(output_rate);
        let n = chip.channel_count();
        Self {
            chip,
            slots: vec![SlotState::default(); n],
        }
    }

    pub fn any_active(&self) -> bool {
        self.chip.any_active()
    }

    pub fn render_frame(&mut self) -> (i32, i32) {
        self.chip.render_frame()
    }

    /// Find (or reuse) a hardware slot for a tracker channel. Prefers the
    /// channel's existing slot, then a silent/free slot, then steals the
    /// first slot (mirrors `snd_fm.c`'s GetVoice/SetVoice intent — when
    /// the chip is oversubscribed something has to give).
    fn assign_slot(&mut self, track_ch: usize) -> usize {
        if let Some(i) = self.slots.iter().position(|s| s.owner == Some(track_ch)) {
            return i;
        }
        if let Some(i) = self
            .slots
            .iter()
            .position(|s| s.owner.is_none() || !s.keyed)
        {
            return i;
        }
        if let Some(i) = (0..self.slots.len()).find(|&i| self.chip.channel_is_silent(i)) {
            return i;
        }
        0
    }

    fn slot_of(&self, track_ch: usize) -> Option<usize> {
        self.slots.iter().position(|s| s.owner == Some(track_ch))
    }

    /// Trigger an OPL instrument on a tracker channel: allocate a slot,
    /// program the patch + pitch + volume + pan, and key it on.
    pub fn note_on(
        &mut self,
        track_ch: usize,
        instr: &InstrOpl,
        milli_hz: u32,
        volume_0_63: u8,
        pan_l: bool,
        pan_r: bool,
    ) {
        let slot = self.assign_slot(track_ch);
        let patch = channel_patch_from_instr(instr);

        self.slots[slot] = SlotState {
            owner: Some(track_ch),
            keyed: true,
            carrier_tl: patch.carrier.tl,
            modulator_tl: patch.modulator.tl,
            additive: patch.additive,
        };

        self.chip.set_patch(slot, &patch);
        let (fnum, block) = hz_to_fnum_block(milli_hz);
        self.chip.set_frequency(slot, fnum, block);
        self.chip.set_pan(slot, pan_l, pan_r);
        self.apply_volume(slot, volume_0_63);
        self.chip.key_on(slot);
    }

    pub fn set_frequency(&mut self, track_ch: usize, milli_hz: u32) {
        if let Some(slot) = self.slot_of(track_ch) {
            let (fnum, block) = hz_to_fnum_block(milli_hz);
            self.chip.set_frequency(slot, fnum, block);
        }
    }

    pub fn set_volume(&mut self, track_ch: usize, volume_0_63: u8) {
        if let Some(slot) = self.slot_of(track_ch) {
            self.apply_volume(slot, volume_0_63);
        }
    }

    pub fn set_pan(&mut self, track_ch: usize, pan_l: bool, pan_r: bool) {
        if let Some(slot) = self.slot_of(track_ch) {
            self.chip.set_pan(slot, pan_l, pan_r);
        }
    }

    pub fn note_off(&mut self, track_ch: usize) {
        if let Some(slot) = self.slot_of(track_ch) {
            self.chip.key_off(slot);
            self.slots[slot].keyed = false;
        }
    }

    /// Hard cut (note-cut / channel reused by a sample instrument): key
    /// off and release ownership immediately.
    pub fn note_cut(&mut self, track_ch: usize) {
        if let Some(slot) = self.slot_of(track_ch) {
            self.chip.key_off(slot);
            self.slots[slot] = SlotState::default();
        }
    }

    /// `OPL_Touch` volume law (Bisqwit's ST3-measured curve, `snd_fm.c`):
    /// effective carrier TL = `63 + (patch_tl·vol/63) − vol`. Modulator TL
    /// is only touched in additive mode.
    fn apply_volume(&mut self, slot: usize, vol: u8) {
        let v = vol.min(63) as i32;
        let st = self.slots[slot];
        let new_carrier = (63 + (st.carrier_tl as i32 * v / 63) - v).clamp(0, 63) as u8;
        self.chip.set_carrier_tl(slot, new_carrier);
        // (Additive modulator TL is handled inside `set_carrier_tl`, which
        // mirrors the change to the modulator when the channel is additive.)
        let _ = st.modulator_tl;
    }
}

/// Decode an [`InstrOpl`] into a [`ChannelPatch`]. The OPL connection bit
/// (`con`) and feedback live in the modulator's register fields.
fn channel_patch_from_instr(instr: &InstrOpl) -> ChannelPatch {
    let m = op_patch(
        &instr.element.modulator,
        instr.element.modulator_wave_select,
    );
    let c = op_patch(&instr.element.carrier, instr.element.carrier_wave_select);
    ChannelPatch {
        modulator: m,
        carrier: c,
        feedback: instr.element.modulator.feedback & 0x07,
        // `con = true` → additive (AM); `false` → FM. The importer sets
        // `con` on both operators from the same register bit.
        additive: instr.element.modulator.con,
    }
}

fn op_patch(mdi: &MdiOpl, waveform: u8) -> OpPatch {
    OpPatch {
        mul: mdi.multiple,
        waveform,
        tl: mdi.total_level,
        ksl: mdi.ksl,
        ksr: mdi.ksr,
        sustaining: mdi.eg,
        attack: mdi.attack,
        decay: mdi.decay,
        sustain: mdi.sustain,
        release: mdi.release,
        am: mdi.am,
        vib: mdi.vib,
    }
}

/// Convert a frequency in milli-Hertz to an OPL (F-number, block) pair,
/// integer-only. Mirrors `snd_fm.c::milliHertzToFnum` with the documented
/// block thresholds and `fnum = (mHz << (20 − block)) / (49716·1000)`.
pub fn hz_to_fnum_block(milli_hz: u32) -> (u16, u8) {
    let mhz = milli_hz as u64;
    if mhz == 0 {
        return (0, 0);
    }
    let block: u32 = if mhz > 3_104_215 {
        7
    } else if mhz > 1_552_107 {
        6
    } else if mhz > 776_053 {
        5
    } else if mhz > 388_026 {
        4
    } else if mhz > 194_013 {
        3
    } else if mhz > 97_006 {
        2
    } else if mhz > 48_503 {
        1
    } else {
        0
    };
    let denom = OPL_NATIVE_RATE as u64 * 1000;
    let mut fnum = (mhz << (20 - block)) / denom;
    let mut block = block;
    // Carry into the next block if the F-number overflowed its 10 bits.
    if fnum > 1023 && block < 7 {
        block += 1;
        fnum = (mhz << (20 - block)) / denom;
    }
    ((fnum.min(1023)) as u16, block as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnum_block_round_trips_a440() {
        // 440 Hz → fnum/block → reconstruct freq, within a few cents.
        let (fnum, block) = hz_to_fnum_block(440_000);
        // freq = fnum * 49716 / 2^(20-block)
        let freq = (fnum as u64 * OPL_NATIVE_RATE as u64) as f64 / (1u64 << (20 - block)) as f64;
        assert!((freq - 440.0).abs() < 2.0, "freq={freq}");
    }
}
