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
        }
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
}

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
        for c in &mut self.channels {
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
}
