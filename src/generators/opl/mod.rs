//! Clean-room OPL2 (Yamaha YM3812) FM synthesis core — RFC Phase A.
//!
//! `OPL_SYNTHESIS_RFC.md` (`xmrs/src/tracker/`) is the design. The player
//! drives one `OplChip` (owned by `Voices`, behind an `Option`, so a
//! module with no FM instrument never instantiates it and stays
//! bit-identical). [`OplDriver`](crate::generators::opl::driver::OplDriver) translates tracker-channel
//! gestures (trigger / pitch / volume / pan) into chip calls, mirroring
//! Schism's `player/snd_fm.c`.
//!
//! Scope is 2-operator OPL2 voices (exactly the S3M/IT AdLib patch shape)
//! plus OPL3 stereo panning — see RFC §3.2. 4-operator linking and the
//! rhythm/percussion mode are out of scope (no corpus module uses them).
//!
//! ## Clean-room & attribution
//!
//! This crate is MIT-licensed. The OPL emulators it is *validated against*
//! are not: Schism Tracker's `player/fmopl3.c` (the ymf262 core it runs) is
//! **GPL-2.0+, © Jarek Burczynski**, and Nuked-OPL3 is LGPL. **No code or
//! data table is copied from either.** Every constant here is independently
//! generated from the documented hardware behaviour and integer math: the
//! log-sin/exp ROMs from their defining formulas (the `tables` module), the envelope
//! increments from the documented `(4+lo)·2^rate_hi` rate law, the tremolo
//! and vibrato LFOs from their documented triangle/8-step shapes, and the
//! `MUL`/`KSL`/key-scale values from the Yamaha YM3812/YMF262 datasheet.
//! `fmopl3.c` and the datasheet are consulted only as *behavioural
//! references* (to know what the silicon does), with thanks to their
//! authors; the implementation — structure, code and tables — is original.

pub mod driver;
mod operator;
mod tables;

use alloc::vec::Vec;
use operator::Operator;

/// Native OPL sample rate (OPLRATEBASE in `snd_fm.c`). F-numbers are
/// computed against this; the chip then runs one step per output frame
/// with the phase increment scaled by `49716 / output_rate`.
pub(crate) const OPL_NATIVE_RATE: u32 = 49716;

/// Number of 2-operator channels on an OPL2 (YM3812).
pub(crate) const OPL2_CHANNELS: usize = 9;

/// One 2-operator FM channel: modulator (op 0) → carrier (op 1), or both
/// summed in additive ("AM") mode.
struct OplChannel {
    modulator: Operator,
    carrier: Operator,
    /// Feedback register (0..7) applied to the modulator's self-FM.
    feedback: u8,
    /// Connection: `false` = FM (mod → carrier), `true` = additive.
    additive: bool,
    /// Stereo enable bits (OPL3 panning): `(left, right)`.
    pan_l: bool,
    pan_r: bool,
    /// Latched F-number low byte (reg 0xA0) and high 2 bits + block (reg 0xB0), so an 0xA0 write
    /// can recompute pitch with the current block.
    fnum_low: u8,
    fnum_high: u8,
    block: u8,
}

impl OplChannel {
    fn new() -> Self {
        Self {
            modulator: Operator::new(),
            carrier: Operator::new(),
            feedback: 0,
            additive: false,
            pan_l: true,
            pan_r: true,
            fnum_low: 0,
            fnum_high: 0,
            block: 0,
        }
    }

    /// Recompute pitch on both operators from the latched F-number / block.
    fn refresh_freq(&mut self) {
        let fnum = (self.fnum_low as u16) | (((self.fnum_high & 0x03) as u16) << 8);
        self.modulator.set_frequency(fnum, self.block, 1 << 16);
        self.carrier.set_frequency(fnum, self.block, 1 << 16);
    }

    fn is_silent(&self) -> bool {
        self.carrier.is_silent() && (!self.additive || self.modulator.is_silent())
    }

    /// One output frame for this channel → signed mono magnitude
    /// (~±4084 per operator at full level — the chip's 12-bit output).
    fn next_mono(&mut self, tremolo: u16, vibpos: u8, eg_cnt: u32) -> i32 {
        // Modulator self-feedback (same law for both modes now that the
        // operator output is at the real 12-bit hardware scale — see
        // `Operator::feedback_phase`).
        let fb = self.modulator.feedback_phase(self.feedback);
        let mod_out = self.modulator.next(fb, tremolo, vibpos, eg_cnt);
        if self.additive {
            // Both operators sound; carrier runs unmodulated.
            let car = self.carrier.next(0, tremolo, vibpos, eg_cnt);
            mod_out + car
        } else {
            // FM: the modulator output bends the carrier phase directly. A
            // ±4084 operator output → a ±4084 phase-index shift ≈ ±4 sine
            // cycles, exactly the chip's modulation index (the hardware adds
            // the modulator output into the carrier's phase index with no
            // scaling).
            self.carrier.next(mod_out, tremolo, vibpos, eg_cnt)
        }
    }
}

/// A single OPL2 chip: 9 two-operator channels, summed to stereo.
///
/// The operators run at the chip's **native 49716 Hz** (so their internal
/// aliasing matches real hardware — the YM3812 itself folds harmonics above
/// 24858 Hz); [`Self::render_frame`] resamples that native stream down to
/// the player's output rate. Rendering directly at 44100/48000 would fold
/// harmonics at the *wrong* Nyquist and produce inauthentic timbre on
/// bright (high-multiple) operators.
pub(crate) struct OplChip {
    channels: Vec<OplChannel>,
    /// Native→output resampler: how many native (49716 Hz) frames to
    /// advance per output frame, Q32. `> 1<<32` since native > output.
    resamp_step_q32: u64,
    /// Fractional native-stream position for the next output frame, Q32.
    resamp_cursor_q32: u64,
    /// The four most recent native frames `[n-3, n-2, n-1, n]`; the output
    /// position lies between `[1]` and `[2]` (4-point cubic interpolation).
    native_hist: [(i32, i32); 4],
    primed: bool,
    /// Global LFO counter (one tick per native frame); drives the tremolo
    /// and vibrato sub-LFOs.
    lfo_timer: u32,
    /// Tremolo triangle position (0..209), advanced every 64 native frames.
    tremolopos: u32,
    /// Global envelope counter (one tick per native frame). Every operator's
    /// envelope reads the *same* counter, so their fractional-rate dither
    /// phases stay aligned exactly as on the chip.
    eg_cnt: u32,
    /// **OPL rhythm mode** (reg 0xBD bit 5): channels 6/7/8 become BD/HH/SD/TT/CY.
    rhythm_mode: bool,
    /// Active percussion bits (reg 0xBD bits 0-4: BD 0x10, SD 0x08, TT 0x04, CY 0x02, HH 0x01).
    rhythm_keys: u8,
    /// 23-bit noise LFSR for HH/SD/CY (rhythm mode).
    noise: u32,
}

// Rhythm-mode channels and reg 0xBD bit masks.
const CH_BD: usize = 6; // bass drum (2-operator FM)
const CH_HH_SD: usize = 7; // modulator = hi-hat (HH), carrier = snare (SD)
const CH_TT_CY: usize = 8; // modulator = tom-tom (TT), carrier = cymbal (CY)
const RM_BD: u8 = 0x10;
const RM_SD: u8 = 0x08;
const RM_TT: u8 = 0x04;
const RM_CY: u8 = 0x02;
const RM_HH: u8 = 0x01;

impl OplChip {
    pub(crate) fn new(output_rate: u32) -> Self {
        let r = output_rate.max(1) as u64;
        Self {
            channels: (0..OPL2_CHANNELS).map(|_| OplChannel::new()).collect(),
            // native frames consumed per output frame = 49716 / output_rate.
            resamp_step_q32: ((OPL_NATIVE_RATE as u64) << 32) / r,
            resamp_cursor_q32: 0,
            native_hist: [(0, 0); 4],
            primed: false,
            lfo_timer: 0,
            tremolopos: 0,
            eg_cnt: 0,
            rhythm_mode: false,
            rhythm_keys: 0,
            noise: 1,
        }
    }

    /// Write the OPL rhythm register (0xBD): bit 5 = enable, bits 0-4 = drum key-ons.
    /// A rising bit → key-on the drum's operator(s); a falling bit → key-off.
    pub(crate) fn write_rhythm(&mut self, bd: u8) {
        self.rhythm_mode = bd & 0x20 != 0;
        let old = self.rhythm_keys;
        let new = bd & 0x1F;
        self.rhythm_keys = new;
        if !self.rhythm_mode {
            return;
        }
        let edge = |mask: u8| (new & mask != 0, (new & mask) != (old & mask));
        // (channel, modulateur?, masque)
        for &(ch, modu, mask) in &[
            (CH_BD, true, RM_BD),
            (CH_BD, false, RM_BD),
            (CH_HH_SD, true, RM_HH),
            (CH_HH_SD, false, RM_SD),
            (CH_TT_CY, true, RM_TT),
            (CH_TT_CY, false, RM_CY),
        ] {
            let (on, changed) = edge(mask);
            if !changed {
                continue;
            }
            if let Some(c) = self.channels.get_mut(ch) {
                let op = if modu {
                    &mut c.modulator
                } else {
                    &mut c.carrier
                };
                if on {
                    op.key_on();
                } else {
                    op.key_off();
                }
            }
        }
    }

    pub(crate) fn rhythm_enabled(&self) -> bool {
        self.rhythm_mode
    }

    /// Raw OPL2 register write — decodes the register address and updates the affected operator
    /// or channel, exactly like writing to the chip's register port. This is the primitive a
    /// register-level player (DRO / VGM / a ported sound driver) needs. Registers: 0x20-0x35
    /// (AM/VIB/EG/KSR/MULT), 0x40-0x55 (KSL/TL), 0x60-0x75 (AR/DR), 0x80-0x95 (SL/RR), 0xA0-0xA8
    /// (F-num low), 0xB0-0xB8 (F-num high / block / key-on), 0xC0-0xC8 (feedback/connection),
    /// 0xE0-0xF5 (waveform), 0xBD (rhythm mode + drum key-on).
    pub(crate) fn write_reg(&mut self, reg: u8, val: u8) {
        // Operator register block → (channel, is_carrier). Offsets 0x06/0x07/0x0E/0x0F unused.
        fn op_slot(off: u8) -> Option<(usize, bool)> {
            let (grp, idx) = (off / 8, off % 8);
            if idx >= 6 || grp >= 3 {
                return None;
            }
            let base = (grp * 3) as usize;
            match idx {
                0..=2 => Some((base + idx as usize, false)), // modulators
                3..=5 => Some((base + (idx - 3) as usize, true)), // carriers
                _ => None,
            }
        }
        fn op_mut(c: &mut OplChannel, carrier: bool) -> &mut Operator {
            if carrier {
                &mut c.carrier
            } else {
                &mut c.modulator
            }
        }
        match reg {
            0xBD => self.write_rhythm(val),
            0x20..=0x35 => {
                if let Some((ch, car)) = op_slot(reg - 0x20) {
                    if let Some(c) = self.channels.get_mut(ch) {
                        op_mut(c, car).write_reg20(val);
                    }
                }
            }
            0x40..=0x55 => {
                if let Some((ch, car)) = op_slot(reg - 0x40) {
                    if let Some(c) = self.channels.get_mut(ch) {
                        op_mut(c, car).write_reg40(val);
                    }
                }
            }
            0x60..=0x75 => {
                if let Some((ch, car)) = op_slot(reg - 0x60) {
                    if let Some(c) = self.channels.get_mut(ch) {
                        op_mut(c, car).write_reg60(val);
                    }
                }
            }
            0x80..=0x95 => {
                if let Some((ch, car)) = op_slot(reg - 0x80) {
                    if let Some(c) = self.channels.get_mut(ch) {
                        op_mut(c, car).write_reg80(val);
                    }
                }
            }
            0xE0..=0xF5 => {
                if let Some((ch, car)) = op_slot(reg - 0xE0) {
                    if let Some(c) = self.channels.get_mut(ch) {
                        op_mut(c, car).write_reg_e0(val);
                    }
                }
            }
            0xA0..=0xA8 => {
                let ch = (reg - 0xA0) as usize;
                if let Some(c) = self.channels.get_mut(ch) {
                    c.fnum_low = val;
                    c.refresh_freq();
                }
            }
            0xB0..=0xB8 => {
                let ch = (reg - 0xB0) as usize;
                let rhythm_ch =
                    self.rhythm_mode && (ch == CH_BD || ch == CH_HH_SD || ch == CH_TT_CY);
                if let Some(c) = self.channels.get_mut(ch) {
                    c.fnum_high = val & 0x03;
                    c.block = (val >> 2) & 0x07;
                    c.refresh_freq();
                    // Key-on/off (bit 5) — ignored for rhythm channels in rhythm mode (0xBD keys them).
                    if !rhythm_ch {
                        if val & 0x20 != 0 {
                            c.modulator.key_on();
                            c.carrier.key_on();
                        } else {
                            c.modulator.key_off();
                            c.carrier.key_off();
                        }
                    }
                }
            }
            0xC0..=0xC8 => {
                let ch = (reg - 0xC0) as usize;
                if let Some(c) = self.channels.get_mut(ch) {
                    c.feedback = (val >> 1) & 0x07;
                    c.additive = val & 0x01 != 0;
                }
            }
            _ => {}
        }
    }

    pub(crate) fn channel_count(&self) -> usize {
        self.channels.len()
    }

    /// Program a channel's two operators + routing from a decoded patch.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn set_patch(&mut self, ch: usize, patch: &ChannelPatch) {
        let Some(c) = self.channels.get_mut(ch) else {
            return;
        };
        c.feedback = patch.feedback & 0x07;
        c.additive = patch.additive;
        let m = &patch.modulator;
        c.modulator.set_patch(
            m.mul,
            m.waveform,
            m.tl,
            m.ksl,
            m.ksr,
            m.sustaining,
            m.attack,
            m.decay,
            m.sustain,
            m.release,
            m.am,
            m.vib,
        );
        let cr = &patch.carrier;
        c.carrier.set_patch(
            cr.mul,
            cr.waveform,
            cr.tl,
            cr.ksl,
            cr.ksr,
            cr.sustaining,
            cr.attack,
            cr.decay,
            cr.sustain,
            cr.release,
            cr.am,
            cr.vib,
        );
    }

    /// Program a **single** operator (modulator or carrier) of a channel from an [`OpPatch`] —
    /// used to load the rhythm-mode percussion voices (HH/SD on ch7, TT/CY on ch8, which are
    /// single operators; BD on ch6 uses the full channel via [`Self::set_patch`]).
    pub(crate) fn set_rhythm_operator(&mut self, ch: usize, carrier: bool, p: &OpPatch) {
        let Some(c) = self.channels.get_mut(ch) else {
            return;
        };
        let op = if carrier {
            &mut c.carrier
        } else {
            &mut c.modulator
        };
        op.set_patch(
            p.mul,
            p.waveform,
            p.tl,
            p.ksl,
            p.ksr,
            p.sustaining,
            p.attack,
            p.decay,
            p.sustain,
            p.release,
            p.am,
            p.vib,
        );
    }

    /// Set the pitch of a single rhythm operator (ch7/ch8 carry two drums sharing the channel
    /// F-number on hardware, but each drum's operator can be phase-driven independently here).
    pub(crate) fn set_operator_frequency(
        &mut self,
        ch: usize,
        carrier: bool,
        fnum: u16,
        block: u8,
    ) {
        if let Some(c) = self.channels.get_mut(ch) {
            let op = if carrier {
                &mut c.carrier
            } else {
                &mut c.modulator
            };
            op.set_frequency(fnum, block, 1 << 16);
        }
    }

    /// Set a channel's F-number / block (pitch). Recomputes both
    /// operators' phase increments.
    pub(crate) fn set_frequency(&mut self, ch: usize, fnum: u16, block: u8) {
        if let Some(c) = self.channels.get_mut(ch) {
            // Native rate: no phase rescaling (the resampler handles the
            // output-rate conversion), so pass unity (`1 << 16`).
            c.modulator.set_frequency(fnum, block, 1 << 16);
            c.carrier.set_frequency(fnum, block, 1 << 16);
        }
    }

    /// Override the carrier total level (per-note volume; see
    /// `snd_fm.c::OPL_Touch`). `tl` is the 0..63 register value.
    pub(crate) fn set_carrier_tl(&mut self, ch: usize, tl: u8) {
        if let Some(c) = self.channels.get_mut(ch) {
            c.carrier.set_total_level(tl);
            if c.additive {
                c.modulator.set_total_level(tl);
            }
        }
    }

    pub(crate) fn set_pan(&mut self, ch: usize, left: bool, right: bool) {
        if let Some(c) = self.channels.get_mut(ch) {
            c.pan_l = left;
            c.pan_r = right;
        }
    }

    pub(crate) fn key_on(&mut self, ch: usize) {
        if let Some(c) = self.channels.get_mut(ch) {
            c.modulator.key_on();
            c.carrier.key_on();
        }
    }

    pub(crate) fn key_off(&mut self, ch: usize) {
        if let Some(c) = self.channels.get_mut(ch) {
            c.modulator.key_off();
            c.carrier.key_off();
        }
    }

    pub(crate) fn channel_is_silent(&self, ch: usize) -> bool {
        self.channels.get(ch).map(|c| c.is_silent()).unwrap_or(true)
    }

    /// Any channel still sounding? (Cheap activity gate for the mixer.)
    pub(crate) fn any_active(&self) -> bool {
        self.channels.iter().any(|c| !c.is_silent())
    }

    /// One **native-rate** (49716 Hz) frame: advance the global LFO, then
    /// sum all channels to stereo with the current tremolo / vibrato.
    fn render_native(&mut self) -> (i32, i32) {
        // Global LFO (YMF262): the timer ticks once per native frame.
        self.lfo_timer = self.lfo_timer.wrapping_add(1);
        // Global envelope counter: one tick per native frame (`eg_timer_add ==
        // eg_timer_overflow` at the native rate, so `eg_cnt` increments by 1).
        self.eg_cnt = self.eg_cnt.wrapping_add(1);
        let eg_cnt = self.eg_cnt;
        // Tremolo (AM): the documented YMF262 amplitude LFO is a 0→26→0
        // triangle stepped once every 64 native frames over 210 steps — a
        // 49716 / (210·64) ≈ 3.7 Hz oscillation. The 0..26 eg-unit swing is
        // the deep (DAM = 1, ≈4.8 dB) depth; S3M/IT never set DAM, so the
        // default `>> 2` gives the shallow ≈1 dB depth (peak 6 eg-units ≈
        // 1.1 dB). Derived from the documented shape — no table copied.
        if self.lfo_timer & 0x3f == 0x3f {
            self.tremolopos = (self.tremolopos + 1) % 210;
        }
        let ramp = if self.tremolopos < 105 {
            self.tremolopos
        } else {
            209 - self.tremolopos
        }; // 0..104..0
        let am26 = (ramp * 26) / 104; // documented 0..26 eg-unit triangle
        let tremolo = (am26 >> 2) as u16; // DAM = 0 (shallow, ≈1 dB)
                                          // Vibrato (PM): an 8-step LFO advanced every 1024 frames (≈6 Hz); the
                                          // per-operator deviation is applied in `Operator::vib_delta`.
        let vibpos = ((self.lfo_timer >> 10) & 7) as u8;

        let mut l: i32 = 0;
        let mut r: i32 = 0;
        let rhythm = self.rhythm_mode && self.channels.len() >= 9;
        for ch in 0..self.channels.len() {
            // In rhythm mode, channels 7/8 (HH/SD/TT/CY) are rendered separately; channel 6 (BD)
            // stays a normal FM channel (keyed by `write_rhythm`), so it is handled in this loop.
            if rhythm && (ch == CH_HH_SD || ch == CH_TT_CY) {
                continue;
            }
            let c = &mut self.channels[ch];
            if c.is_silent() {
                continue;
            }
            let s = c.next_mono(tremolo, vibpos, eg_cnt);
            if c.pan_l {
                l += s;
            }
            if c.pan_r {
                r += s;
            }
        }

        if rhythm {
            let (dl, dr) = self.render_rhythm_percussion(tremolo, vibpos, eg_cnt);
            l += dl;
            r += dr;
        }
        // 23-bit noise LFSR (advances every native frame, whether or not rhythm mode is on).
        self.noise = if self.noise & 1 != 0 {
            (self.noise >> 1) ^ 0x0080_0302
        } else {
            self.noise >> 1
        };
        (l, r)
    }

    /// Render the 4 percussion voices on channels 7/8 (HH/SD on ch7, TT/CY on ch8) using the OPL
    /// correlated-noise algorithm (ref. Nuked-OPL3): each operator advances its phase, then HH/SD/CY
    /// have their **output phase forced** from the phase bits of HH (ch7 mod) and CY (ch8 carrier)
    /// combined with the noise bit. TT is a normal operator.
    fn render_rhythm_percussion(&mut self, tremolo: u16, vibpos: u8, eg_cnt: u32) -> (i32, i32) {
        let noise_bit = self.noise & 1;
        // Advance the 4 operators (envelope + phase) before reading their phase bits.
        self.channels[CH_HH_SD].modulator.advance(vibpos, eg_cnt); // HH
        self.channels[CH_HH_SD].carrier.advance(vibpos, eg_cnt); // SD
        self.channels[CH_TT_CY].modulator.advance(vibpos, eg_cnt); // TT
        self.channels[CH_TT_CY].carrier.advance(vibpos, eg_cnt); // CY

        let hh_p = self.channels[CH_HH_SD].modulator.phase_out();
        let cy_p = self.channels[CH_TT_CY].carrier.phase_out();
        let bit = |v: u32, n: u32| (v >> n) & 1;
        let rm = (bit(hh_p, 2) ^ bit(hh_p, 7))
            | (bit(hh_p, 3) ^ bit(cy_p, 5))
            | (bit(cy_p, 3) ^ bit(cy_p, 5));

        // Hi-hat (HH)
        let hh_phase = (rm << 9) | if (rm ^ noise_bit) != 0 { 0xD0 } else { 0x34 };
        let hh_out = self.channels[CH_HH_SD]
            .modulator
            .output_at(hh_phase, tremolo);
        // Snare drum (SD)
        let hh_bit8 = bit(hh_p, 8);
        let sd_phase = (hh_bit8 << 9) | ((hh_bit8 ^ noise_bit) << 8);
        let sd_out = self.channels[CH_HH_SD].carrier.output_at(sd_phase, tremolo);
        // Tom-tom (TT): normal operator (own phase, no modulation)
        let tt_phase = self.channels[CH_TT_CY].modulator.phase_out() & 0x3FF;
        let tt_out = self.channels[CH_TT_CY]
            .modulator
            .output_at(tt_phase, tremolo);
        // Cymbal (CY)
        let cy_phase = (rm << 9) | 0x80;
        let cy_out = self.channels[CH_TT_CY].carrier.output_at(cy_phase, tremolo);

        // Percussion is summed at double gain (hardware), with each voice's panning.
        let d7 = (hh_out + sd_out) * 2;
        let d8 = (tt_out + cy_out) * 2;
        let mut l = 0;
        let mut r = 0;
        if self.channels[CH_HH_SD].pan_l {
            l += d7;
        }
        if self.channels[CH_HH_SD].pan_r {
            r += d7;
        }
        if self.channels[CH_TT_CY].pan_l {
            l += d8;
        }
        if self.channels[CH_TT_CY].pan_r {
            r += d8;
        }
        (l, r)
    }

    /// Render one **output-rate** frame by resampling the native 49716 Hz
    /// stream with **4-point cubic (Catmull-Rom)** interpolation — a flatter
    /// passband than linear, so the top audio octave isn't rolled off (which
    /// dulled high notes). The chip advances `resamp_step_q32` native frames
    /// per call, so its operators always run at the authentic chip rate.
    pub(crate) fn render_frame(&mut self) -> (i32, i32) {
        if !self.primed {
            for i in 0..4 {
                self.native_hist[i] = self.render_native();
            }
            self.primed = true;
        }
        self.resamp_cursor_q32 += self.resamp_step_q32;
        while self.resamp_cursor_q32 >= (1u64 << 32) {
            self.resamp_cursor_q32 -= 1u64 << 32;
            self.native_hist[0] = self.native_hist[1];
            self.native_hist[1] = self.native_hist[2];
            self.native_hist[2] = self.native_hist[3];
            self.native_hist[3] = self.render_native();
        }
        // Position between native_hist[1] and [2], Q16 fraction.
        let t = ((self.resamp_cursor_q32 & 0xFFFF_FFFF) >> 16) as i64;
        let h = self.native_hist;
        (
            cubic(h[0].0, h[1].0, h[2].0, h[3].0, t),
            cubic(h[0].1, h[1].1, h[2].1, h[3].1, t),
        )
    }
}

/// A **register-level OPL2 (YM3812) emulator** — the public, raw interface over the internal
/// [`OplChip`]. Write chip registers with [`Opl2::write_reg`] exactly as a real driver would, then
/// pull output-rate stereo frames with [`Opl2::render_frame`]. This is what a ported sound driver
/// or a DRO / VGM player needs (the structured note/patch API lives in [`super::driver::OplDriver`]).
///
/// The chip runs internally at 49716 Hz and resamples to `output_rate` (cubic).
pub struct Opl2 {
    chip: OplChip,
}

impl Opl2 {
    /// New OPL2 rendering at `output_rate` Hz (stereo).
    pub fn new(output_rate: u32) -> Self {
        Self {
            chip: OplChip::new(output_rate),
        }
    }

    /// Write a raw OPL2 register (`reg`, `val`). See [`OplChip::write_reg`] for the register map.
    pub fn write_reg(&mut self, reg: u8, val: u8) {
        self.chip.write_reg(reg, val);
    }

    /// Render one output-rate stereo frame (`left`, `right`), each ~±full-scale `i32`.
    pub fn render_frame(&mut self) -> (i32, i32) {
        self.chip.render_frame()
    }

    /// Any channel still sounding?
    pub fn any_active(&self) -> bool {
        self.chip.any_active()
    }
}

/// Catmull-Rom cubic interpolation at fraction `t` (Q16, between `p1` and
/// `p2`): `p1 + 0.5·t·((p2−p0) + t·((2p0−5p1+4p2−p3) + t·(−p0+3p1−3p2+p3)))`.
#[inline]
fn cubic(p0: i32, p1: i32, p2: i32, p3: i32, t: i64) -> i32 {
    let (p0, p1, p2, p3) = (p0 as i64, p1 as i64, p2 as i64, p3 as i64);
    let a = -p0 + 3 * p1 - 3 * p2 + p3;
    let b = 2 * p0 - 5 * p1 + 4 * p2 - p3;
    let c = p2 - p0;
    let inner = b + ((a * t) >> 16);
    let inner = c + ((inner * t) >> 16);
    let half = (inner * t) >> 16;
    (p1 + (half >> 1)) as i32
}

/// One operator's decoded patch fields (the `MdiOpl`-equivalent the
/// driver extracts from `InstrOpl`).
#[derive(Clone, Copy, Default)]
pub(crate) struct OpPatch {
    pub mul: u8,
    pub waveform: u8,
    pub tl: u8,
    pub ksl: u8,
    pub ksr: bool,
    pub sustaining: bool,
    pub attack: u8,
    pub decay: u8,
    pub sustain: u8,
    pub release: u8,
    /// Tremolo (AM) / vibrato enable bits.
    pub am: bool,
    pub vib: bool,
}

/// A full 2-operator channel patch.
#[derive(Clone, Copy, Default)]
pub(crate) struct ChannelPatch {
    pub modulator: OpPatch,
    pub carrier: OpPatch,
    pub feedback: u8,
    pub additive: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opl2_raw_registers_render_a_tone() {
        // Program a simple FM voice on channel 0 via raw register writes (as a driver would),
        // key it on, and check it produces sound — validates the `write_reg` decoder + `Opl2`.
        let mut opl = Opl2::new(44100);
        // Modulator (op slot 0x00) quiet, carrier (op slot 0x03) audible.
        opl.write_reg(0x20, 0x01); // mod: mult=1
        opl.write_reg(0x23, 0x01); // car: mult=1
        opl.write_reg(0x40, 0x3F); // mod: TL max attenuation (silent modulator)
        opl.write_reg(0x43, 0x00); // car: TL loud
        opl.write_reg(0x60, 0xF0); // mod: fast attack
        opl.write_reg(0x63, 0xF0); // car: fast attack
        opl.write_reg(0x80, 0x00); // mod: sustain/release
        opl.write_reg(0x83, 0x00); // car
        opl.write_reg(0xC0, 0x00); // FM, no feedback
        opl.write_reg(0xA0, 0x98); // fnum low
        opl.write_reg(0xB0, 0x20 | (4 << 2) | 0x01); // key-on + block 4 + fnum high
        let mut peak = 0i32;
        for _ in 0..8820 {
            let (l, _r) = opl.render_frame();
            peak = peak.max(l.abs());
        }
        assert!(peak > 100, "raw-register tone silent, peak={peak}");
    }

    #[test]
    fn chip_renders_a_tone() {
        let mut chip = OplChip::new(44100);
        // A simple sine-ish patch: carrier with a fast attack, modest
        // decay, sustaining; modulator quiet (near-pure carrier tone).
        let patch = ChannelPatch {
            modulator: OpPatch {
                mul: 1,
                tl: 63, // modulator silent → carrier ≈ pure sine
                attack: 15,
                decay: 0,
                sustain: 0,
                release: 7,
                sustaining: true,
                ..Default::default()
            },
            carrier: OpPatch {
                mul: 1,
                tl: 0,
                attack: 15,
                decay: 0,
                sustain: 0,
                release: 7,
                sustaining: true,
                ..Default::default()
            },
            feedback: 0,
            additive: false,
        };
        chip.set_patch(0, &patch);
        // ~440 Hz: pick a block/fnum near concert A.
        chip.set_frequency(0, 0x2AE, 4);
        chip.key_on(0);

        let mut peak = 0i32;
        let mut nonzero = 0;
        for _ in 0..4410 {
            let (l, _r) = chip.render_frame();
            peak = peak.max(l.abs());
            if l != 0 {
                nonzero += 1;
            }
        }
        assert!(peak > 100, "expected an audible tone, peak={peak}");
        assert!(
            nonzero > 4000,
            "expected a sustained tone, nonzero={nonzero}"
        );

        // Key-off then run the release; it should fall silent.
        chip.key_off(0);
        let mut tail_peak = 0i32;
        for _ in 0..44100 {
            let (l, _r) = chip.render_frame();
            tail_peak = tail_peak.max(l.abs());
            if chip.channel_is_silent(0) {
                break;
            }
        }
        assert!(
            chip.channel_is_silent(0),
            "channel should release to silence"
        );
        let _ = tail_peak;
    }

    #[test]
    fn rhythm_mode_percussion_sounds() {
        let mut chip = OplChip::new(44100);
        // Percussive patches for the rhythm voices (fast attack, short release).
        let perc = ChannelPatch {
            modulator: OpPatch {
                mul: 1,
                tl: 0,
                attack: 15,
                decay: 4,
                sustain: 0,
                release: 7,
                sustaining: false,
                ..Default::default()
            },
            carrier: OpPatch {
                mul: 1,
                tl: 0,
                attack: 15,
                decay: 4,
                sustain: 0,
                release: 7,
                sustaining: false,
                ..Default::default()
            },
            feedback: 0,
            additive: false,
        };
        for ch in [CH_BD, CH_HH_SD, CH_TT_CY] {
            chip.set_patch(ch, &perc);
            chip.set_frequency(ch, 0x200, 4);
        }
        // Enable rhythm mode + trigger BD, SD and HH.
        chip.write_rhythm(0x20 | RM_BD | RM_SD | RM_HH);
        assert!(chip.rhythm_enabled());

        let mut peak = 0i32;
        for _ in 0..4410 {
            let (l, _r) = chip.render_frame();
            peak = peak.max(l.abs());
        }
        assert!(peak > 100, "rhythm percussion is silent, peak={peak}");
    }
}
