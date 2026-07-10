//! High-level OPL driver: translates tracker-channel gestures into chip
//! calls. A Rust re-implementation of the documented AdLib programming
//! procedure in Schism's `player/snd_fm.c` (note→F-number/block,
//! `OPL_Touch` volume law, `OPL_Pan`, voice allocation) — the procedure
//! is documented hardware programming, not copied code.

use alloc::vec;
use alloc::vec::Vec;

use crate::tracker::instr_opl::{InstrOpl, MdiOpl, OplRhythm};

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
    /// Current rhythm register (0xBD) drum bits, and which tracker channel triggered each drum
    /// (so note-off clears the right bit). Empty until a percussion instrument plays.
    rhythm_owner: Vec<(usize, u8)>,
    rhythm_on: bool,
}

/// Fixed rhythm placement of a percussion role: `(chip channel, use carrier operator?, full
/// 2-op channel?, 0xBD drum bit)`. BD is a full FM channel (both operators); the others are
/// single operators sharing channels 7 and 8.
fn rhythm_placement(role: OplRhythm) -> Option<(usize, bool, bool, u8)> {
    match role {
        OplRhythm::Melodic => None,
        OplRhythm::BassDrum => Some((6, false, true, 0x10)),
        OplRhythm::HiHat => Some((7, false, false, 0x01)),
        OplRhythm::Snare => Some((7, true, false, 0x08)),
        OplRhythm::TomTom => Some((8, false, false, 0x04)),
        OplRhythm::Cymbal => Some((8, true, false, 0x02)),
    }
}

impl OplDriver {
    pub fn new(output_rate: u32) -> Self {
        let chip = OplChip::new(output_rate);
        let n = chip.channel_count();
        Self {
            chip,
            slots: vec![SlotState::default(); n],
            rhythm_owner: Vec::new(),
            rhythm_on: false,
        }
    }

    /// Import the imports needed for rhythm mapping (kept local to avoid a wide `use`).
    fn drum_bit_of(&self, track_ch: usize) -> Option<u8> {
        self.rhythm_owner
            .iter()
            .find(|(c, _)| *c == track_ch)
            .map(|(_, b)| *b)
    }

    /// Key on/off a rhythm drum bit in the 0xBD register and update ownership.
    fn set_drum(&mut self, track_ch: usize, bit: u8, on: bool) {
        // current 0xBD drum bits = OR of all owned bits
        self.rhythm_owner.retain(|(c, _)| *c != track_ch);
        if on {
            self.rhythm_owner.push((track_ch, bit));
        }
        let drums = self.rhythm_owner.iter().fold(0u8, |a, (_, b)| a | b);
        self.chip.write_rhythm(0x20 | drums); // bit5 = rhythm mode enabled
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
        // In rhythm mode, channels 6/7/8 are reserved for percussion — melodic notes avoid them.
        let melodic_max = if self.rhythm_on {
            self.slots.len().min(6)
        } else {
            self.slots.len()
        };
        if let Some(i) = self.slots[..melodic_max]
            .iter()
            .position(|s| s.owner == Some(track_ch))
        {
            return i;
        }
        if let Some(i) = self.slots[..melodic_max]
            .iter()
            .position(|s| s.owner.is_none() || !s.keyed)
        {
            return i;
        }
        if let Some(i) = (0..melodic_max).find(|&i| self.chip.channel_is_silent(i)) {
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
        // Percussion (rhythm mode): route to the fixed rhythm channel instead of a melodic slot.
        if let Some((ch, carrier, full, bit)) = rhythm_placement(instr.rhythm) {
            self.rhythm_on = true;
            let patch = channel_patch_from_instr(instr);
            let (fnum, block) = hz_to_fnum_block(milli_hz);
            if full {
                self.chip.set_patch(ch, &patch);
                self.chip.set_frequency(ch, fnum, block);
            } else {
                let op = if carrier {
                    &patch.carrier
                } else {
                    &patch.modulator
                };
                self.chip.set_rhythm_operator(ch, carrier, op);
                self.chip.set_operator_frequency(ch, carrier, fnum, block);
            }
            self.chip.set_pan(ch, pan_l, pan_r);
            // Retrigger: clear then set the drum bit so a repeated hit re-keys the envelope.
            self.set_drum(track_ch, bit, false);
            self.set_drum(track_ch, bit, true);
            return;
        }

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
        if let Some(bit) = self.drum_bit_of(track_ch) {
            self.set_drum(track_ch, bit, false);
            return;
        }
        if let Some(slot) = self.slot_of(track_ch) {
            self.chip.key_off(slot);
            self.slots[slot].keyed = false;
        }
    }

    /// Hard cut (note-cut / channel reused by a sample instrument): key
    /// off and release ownership immediately.
    pub fn note_cut(&mut self, track_ch: usize) {
        if let Some(bit) = self.drum_bit_of(track_ch) {
            self.set_drum(track_ch, bit, false);
            return;
        }
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

    #[test]
    fn rhythm_instrument_routes_to_percussion() {
        use crate::tracker::instr_opl::{InstrOpl, MdiInstr, MdiOpl, OplRhythm};
        let mut drv = OplDriver::new(44100);
        // Percussive patch (fast attack, quick decay) as a bass-drum instrument.
        let op = MdiOpl {
            multiple: 1,
            attack: 15,
            decay: 4,
            sustain: 0,
            release: 7,
            total_level: 0,
            ..Default::default()
        };
        let bd = InstrOpl {
            element: MdiInstr {
                modulator: op,
                carrier: op,
                ..Default::default()
            },
            volume: 63,
            rhythm: OplRhythm::BassDrum,
            ..Default::default()
        };
        // A bass-drum note on tracker channel 0 must produce sound via rhythm mode,
        // without consuming a melodic slot.
        drv.note_on(0, &bd, 100_000, 63, true, true);
        let mut peak = 0i32;
        for _ in 0..4410 {
            let (l, _r) = drv.render_frame();
            peak = peak.max(l.abs());
        }
        assert!(peak > 100, "bass drum silent, peak={peak}");
        assert!(
            drv.slots.iter().all(|s| s.owner.is_none()),
            "no melodic slot used"
        );
    }
}
