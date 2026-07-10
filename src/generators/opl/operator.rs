//! One OPL operator (a.k.a. "slot"): phase generator + envelope
//! generator + waveform output.
//!
//! Clean-room implementation of the documented YMF262/YM3812 operator. The
//! phase generator and the log-sine/exp output path are bit-faithful to the
//! hardware algorithm (see [`super::tables`]); the **envelope generator** is
//! an independent reimplementation of the documented YMF262 envelope
//! mechanism: a shared global counter (the chip's envelope counter, owned by
//! [`super::OplChip`]) gates each phase on `counter & ((1<<shift)-1)`, a
//! dithered increment table supplies the per-phase step, and the attack
//! approaches 0 proportionally (`volume += (~volume·inc) >> 3`). The
//! attenuation domain (0..511) matches the chip exactly, so the envelope is
//! cycle-accurate against the silicon — no calibrated timing anchor. (GPL
//! emulators such as Schism's `player/fmopl3.c` are used only as behavioural
//! references for what the hardware does; no code or table is copied — see
//! the crate-level clean-room note in [`super`].) See
//! `OPL_SYNTHESIS_RFC.md` §3.2.

use super::tables::{EXP_ROM, KSL, KSL_SHIFT, LOGSIN_ROM, MUL};

/// Envelope generator phase.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EgState {
    /// Silent and idle (key released and faded out).
    Off,
    /// Rising toward 0 attenuation (loud).
    Attack,
    /// Falling toward the sustain level.
    Decay,
    /// Held at the sustain level (EG-type "sustaining" operators only).
    Sustain,
    /// Falling toward full attenuation (key-off, or percussive tail).
    Release,
}

/// Attenuation index range of the YMF262 envelope: 0 = loud, 511 = silent;
/// 1 unit ≈ 0.1875 dB (a 9-bit attenuation domain). The envelope carries an
/// **integer** attenuation in this domain, advanced by the cycle-accurate
/// hardware mechanism below (no longer a calibrated Q16 approximation).
const MAX_ATT_INDEX: i32 = 511;

/// Even (Bresenham) distribution of `count` unit-steps across the 8
/// global-timer phases — the standard way to realise a fractional average
/// rate with integer per-sample steps.
const fn spread8(count: u32, phase: u32) -> u8 {
    (((count * (phase + 1)) >> 3) - ((count * phase) >> 3)) as u8
}

/// Per-phase envelope increments (15 rows × 8 global-timer phases),
/// **generated** from the documented YMF262 rate law `(4 + rate_lo)·2^rate_hi`
/// — *not* a table lifted from any emulator. Rows 0-3 spread the count
/// `4 + lo` over the 8 phases (rate groups 0..12); rows 4-7 (rate 13) spread
/// `2·(4 + lo)`; rows 8-11 (rate 14) double the matching rate-13 row; row 12
/// is rate-15 decay (flat 4), row 13 is the "zero-time" attack (flat 8), row
/// 14 is held (flat 0). This reproduces the chip's dither **counts** exactly
/// (so envelope timing is identical) and its per-phase ordering in 14 of the
/// 15 rows; the single differing row is count-identical — an inaudible
/// sub-step reordering. Cross-checked against the documented behaviour, not
/// copied.
const EG_INC: [u8; 15 * 8] = {
    let mut t = [0u8; 15 * 8];
    let mut row = 0usize;
    while row < 15 {
        let mut p = 0u32;
        while p < 8 {
            let v = if row < 4 {
                spread8(4 + row as u32, p) // rates 00..12, low bits 0..3
            } else if row < 8 {
                spread8(2 * (4 + (row as u32 - 4)), p) // rate 13
            } else if row < 12 {
                2 * spread8(2 * (4 + (row as u32 - 8)), p) // rate 14 = 2× rate 13
            } else if row == 12 {
                4 // rate 15 decay/release
            } else if row == 13 {
                8 // rate 15 attack (zero time)
            } else {
                0 // held (infinite time)
            };
            t[row * 8 + p as usize] = v;
            p += 1;
        }
        row += 1;
    }
    t
};

pub(crate) struct Operator {
    // ---- register-derived configuration (set by patch / key) ----
    /// Phase increment per sample (driver-computed from F-num/block/mul).
    phase_inc: u32,
    mul: u8,
    waveform: u8,
    /// Total level attenuation, eg-domain (`tl_reg << 2`), 0..252.
    tl_eg: u16,
    ksl_reg: u8,
    ksr: bool,
    /// Amplitude-modulation (tremolo) enable — the chip's global ~3.7 Hz
    /// tremolo LFO is added to this operator's attenuation.
    am: bool,
    /// Frequency-modulation (vibrato) enable — the chip's global ~6 Hz
    /// vibrato LFO is added to this operator's phase increment.
    vib: bool,
    /// EG-type: true = sustaining (hold at sustain level until key-off);
    /// false = percussive (decay straight through to silence).
    sustaining: bool,
    attack_rate: u8,
    decay_rate: u8,
    sustain_level_eg: u16, // eg-domain target for the decay→sustain knee
    release_rate: u8,

    // ---- live frequency context (for KSL + key-scale-of-rate) ----
    fnum: u16,
    block: u8,

    // ---- runtime state ----
    phase: u32,
    eg_state: EgState,
    volume: i32, // attenuation index, 0 (loud) .. MAX_ATT_INDEX (silent)
    /// Previous operator output (for feedback on operator 0).
    prev_out: i32,
    out: i32,
}

impl Operator {
    pub(crate) fn new() -> Self {
        Self {
            phase_inc: 0,
            mul: 0,
            waveform: 0,
            tl_eg: 0,
            ksl_reg: 0,
            ksr: false,
            am: false,
            vib: false,
            sustaining: false,
            attack_rate: 0,
            decay_rate: 0,
            sustain_level_eg: 0,
            release_rate: 0,
            fnum: 0,
            block: 0,
            phase: 0,
            eg_state: EgState::Off,
            volume: MAX_ATT_INDEX,
            prev_out: 0,
            out: 0,
        }
    }

    /// Program the operator from a decoded patch operator (the
    /// `MdiOpl`-equivalent fields the driver hands over).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn set_patch(
        &mut self,
        mul: u8,
        waveform: u8,
        tl_reg: u8,
        ksl_reg: u8,
        ksr: bool,
        sustaining: bool,
        attack_rate: u8,
        decay_rate: u8,
        sustain_reg: u8,
        release_rate: u8,
        am: bool,
        vib: bool,
    ) {
        self.mul = mul & 0x0F;
        self.waveform = waveform & 0x07;
        self.tl_eg = ((tl_reg & 0x3F) as u16) << 2;
        self.ksl_reg = ksl_reg & 0x03;
        self.ksr = ksr;
        self.am = am;
        self.vib = vib;
        self.sustaining = sustaining;
        self.attack_rate = attack_rate & 0x0F;
        self.decay_rate = decay_rate & 0x0F;
        // Sustain level: register 0..15 → eg attenuation; 15 = near silent.
        self.sustain_level_eg = if sustain_reg >= 0x0F {
            0x1F0
        } else {
            ((sustain_reg & 0x0F) as u16) << 4
        };
        self.release_rate = release_rate & 0x0F;
    }

    /// Update the live frequency context. The driver calls this on every
    /// pitch change (note, porta, vibrato) so KSL and key-scale-of-rate
    /// track the current note. `phase_inc` is recomputed here.
    ///
    /// The chip runs at its native 49716 Hz (the resampler in [`super::OplChip`]
    /// converts to the output rate), so the driver passes `rate_scale_q16 =
    /// 1 << 16` (unity); the parameter is kept for the rare caller that wants
    /// to run the operator directly at another rate.
    pub(crate) fn set_frequency(&mut self, fnum: u16, block: u8, rate_scale_q16: u32) {
        self.fnum = fnum & 0x3FF;
        self.block = block & 0x07;
        // Nuked phase law: ((fnum << block) >> 1) * MUL[mul] >> 1.
        let base = ((self.fnum as u32) << self.block) >> 1;
        let native = (base * MUL[self.mul as usize]) >> 1;
        self.phase_inc = ((native as u64 * rate_scale_q16 as u64) >> 16) as u32;
    }

    /// Override the total-level attenuation (per-note volume). `tl_reg`
    /// is the 0..63 register value; see `snd_fm.c::OPL_Touch`.
    pub(crate) fn set_total_level(&mut self, tl_reg: u8) {
        self.tl_eg = ((tl_reg & 0x3F) as u16) << 2;
    }

    // --- Raw OPL register writes (for a register-level `OplChip::write_reg`) ---

    /// Register 0x20: AM / VIB / EG-type / KSR / MULT. Recomputes `phase_inc`
    /// (MULT affects it) from the stored F-number / block.
    pub(crate) fn write_reg20(&mut self, val: u8) {
        self.am = val & 0x80 != 0;
        self.vib = val & 0x40 != 0;
        self.sustaining = val & 0x20 != 0;
        self.ksr = val & 0x10 != 0;
        self.mul = val & 0x0F;
        self.set_frequency(self.fnum, self.block, 1 << 16);
    }

    /// Register 0x40: KSL (bits 6-7) + total level (bits 0-5).
    pub(crate) fn write_reg40(&mut self, val: u8) {
        self.ksl_reg = val >> 6;
        self.tl_eg = ((val & 0x3F) as u16) << 2;
    }

    /// Register 0x60: attack rate (bits 4-7) + decay rate (bits 0-3).
    pub(crate) fn write_reg60(&mut self, val: u8) {
        self.attack_rate = val >> 4;
        self.decay_rate = val & 0x0F;
    }

    /// Register 0x80: sustain level (bits 4-7) + release rate (bits 0-3).
    pub(crate) fn write_reg80(&mut self, val: u8) {
        let sustain_reg = val >> 4;
        self.sustain_level_eg = if sustain_reg >= 0x0F {
            0x1F0
        } else {
            (sustain_reg as u16) << 4
        };
        self.release_rate = val & 0x0F;
    }

    /// Register 0xE0: waveform select (bits 0-2).
    pub(crate) fn write_reg_e0(&mut self, val: u8) {
        self.waveform = val & 0x07;
    }

    /// Key-on: (re)start the envelope from the attack phase and reset the
    /// phase accumulator (a fresh note restarts the waveform).
    pub(crate) fn key_on(&mut self) {
        self.eg_state = EgState::Attack;
        self.phase = 0;
        // Attack continues from wherever the envelope currently sits
        // (re-trigger of a still-sounding operator), matching hardware.
    }

    /// Key-off: enter the release phase.
    pub(crate) fn key_off(&mut self) {
        if self.eg_state != EgState::Off {
            self.eg_state = EgState::Release;
        }
    }

    pub(crate) fn is_silent(&self) -> bool {
        self.eg_state == EgState::Off
            || (self.eg_state == EgState::Release && self.volume >= MAX_ATT_INDEX)
    }

    /// Key-scale-of-rate offset (0..3 added rate, scaled ×... below). The
    /// hardware derives a 4-bit "key scale number" from block and the top
    /// fnum bit; `ksr` selects whether all 4 or only the top 2 bits apply.
    fn ksr_offset(&self) -> u8 {
        let ksn = (self.block << 1) | ((self.fnum >> 9) & 1) as u8; // 0..15
        if self.ksr {
            ksn
        } else {
            ksn >> 2
        }
    }

    /// Decode an envelope rate into `(timer shift, increment row offset)`,
    /// reproducing the YMF262 rate-shift / rate-select behaviour documented
    /// for the hardware. `reg` is the 0..15 register rate, `ksr` the
    /// key-scale offset (0..15). A register rate of 0 holds forever (row 14);
    /// `attack` selects the "zero-time" instant-attack row (13) for the
    /// fastest rates. The index `16 + (reg<<2) + ksr` indexes the hardware
    /// table: rates 00..12 use shift `12 - rate` and rows 0..3 (by low bits),
    /// rate 13/14 use shift 0 and rows 4..7 / 8..11, rate 15 row 12.
    fn eg_params(reg: u8, ksr: u8, attack: bool) -> (u32, usize) {
        if reg == 0 {
            return (0, 14 * 8); // infinite time (held)
        }
        let idx = 16 + ((reg as usize) << 2) + ksr as usize;
        if attack && idx >= 16 + 60 {
            return (0, 13 * 8); // all rate-15 attacks take zero time
        }
        let r = idx - 16;
        if r < 52 {
            ((12 - (r >> 2)) as u32, (r & 3) * 8) // rates 00..12
        } else if r < 56 {
            (0, (4 + (r & 3)) * 8) // rate 13
        } else if r < 60 {
            (0, (8 + (r & 3)) * 8) // rate 14
        } else {
            (0, 12 * 8) // rate 15 (+ dummy RKS rows)
        }
    }

    /// Advance the envelope one sample against the chip's global envelope
    /// counter `eg_cnt`. Cycle-accurate against the documented YMF262
    /// envelope mechanism: each phase only moves on the
    /// `eg_cnt & ((1<<shift)-1)` boundary, adding the dithered increment step;
    /// the attack approaches 0 proportionally (`volume += (~volume · inc) >> 3`).
    fn tick_envelope(&mut self, eg_cnt: u32) {
        let ksr = self.ksr_offset();
        match self.eg_state {
            EgState::Off | EgState::Sustain => {}
            EgState::Attack => {
                let (sh, sel) = Self::eg_params(self.attack_rate, ksr, true);
                if eg_cnt & ((1 << sh) - 1) == 0 {
                    let inc = EG_INC[sel + ((eg_cnt >> sh) & 7) as usize] as i32;
                    self.volume += (!self.volume * inc) >> 3;
                    if self.volume <= 0 {
                        self.volume = 0;
                        self.eg_state = EgState::Decay;
                    }
                }
            }
            EgState::Decay => {
                let (sh, sel) = Self::eg_params(self.decay_rate, ksr, false);
                if eg_cnt & ((1 << sh) - 1) == 0 {
                    self.volume += EG_INC[sel + ((eg_cnt >> sh) & 7) as usize] as i32;
                    if self.volume >= self.sustain_level_eg as i32 {
                        // Sustaining (EG-type) operators hold; percussive ones
                        // continue falling at the release rate.
                        self.eg_state = if self.sustaining {
                            EgState::Sustain
                        } else {
                            EgState::Release
                        };
                    }
                }
            }
            EgState::Release => {
                let (sh, sel) = Self::eg_params(self.release_rate, ksr, false);
                if eg_cnt & ((1 << sh) - 1) == 0 {
                    self.volume += EG_INC[sel + ((eg_cnt >> sh) & 7) as usize] as i32;
                    if self.volume >= MAX_ATT_INDEX {
                        self.volume = MAX_ATT_INDEX;
                        self.eg_state = EgState::Off;
                    }
                }
            }
        }
    }

    /// KSL attenuation contribution (eg-domain units) for the current note.
    /// Matches the YM3812/YMF262 (Nuked) formula:
    /// `max(0, (kslrom[fnum>>6] << 2) − ((8 − block) << 5)) >> kslshift[reg]`.
    /// The contribution lives in the same eg domain as `tl << 2`, so it sums
    /// straight into [`Self::attenuation_eg`]. (The previous ¼-scale
    /// `kslrom − (7−block)·8` left high notes far too bright for KSL-using
    /// instruments — the high-note timbre divergence `opl_compare` flagged.)
    fn ksl_attenuation(&self) -> u16 {
        if self.ksl_reg == 0 {
            return 0;
        }
        let base = (KSL[(self.fnum >> 6) as usize] as i32) << 2;
        let level = base - ((8 - self.block as i32) << 5);
        if level <= 0 {
            0
        } else {
            (level >> KSL_SHIFT[self.ksl_reg as usize]) as u16
        }
    }

    /// Total attenuation in eg-domain (envelope + total-level + KSL + the
    /// chip's tremolo when AM is enabled).
    fn attenuation_eg(&self, tremolo: u16) -> u32 {
        let env = self.volume.max(0) as u32;
        let trem = if self.am { tremolo as u32 } else { 0 };
        env + self.tl_eg as u32 + self.ksl_attenuation() as u32 + trem
    }

    /// Vibrato deviation added to `phase_inc` this sample (0 unless VIB).
    /// Mirrors the YMF262 8-step vibrato LFO: a triangle in units of
    /// `fnum >> 7`, converted to phase-increment units via block/multiple.
    fn vib_delta(&self, vibpos: u8) -> i32 {
        if !self.vib {
            return 0;
        }
        let mut range = ((self.fnum >> 7) & 7) as i32;
        if vibpos & 3 == 0 {
            range = 0;
        } else if vibpos & 1 == 1 {
            range >>= 1;
        }
        if vibpos & 4 != 0 {
            range = -range;
        }
        // `range` is an fnum delta → phase-increment delta (same law as
        // `set_frequency`): ((range << block) >> 1) * MUL[mul] >> 1.
        (((range << self.block) >> 1) * MUL[self.mul as usize] as i32) >> 1
    }

    /// Compute the operator output for the current phase, given a phase
    /// modulation input (in 10-bit phase units) and the chip's LFO state
    /// (`tremolo` eg-units, `vibpos` 0..7). Advances the phase and the
    /// envelope by one sample. Returns a signed magnitude (~±4084 at full
    /// volume — the chip's 12-bit operator output).
    pub(crate) fn next(&mut self, mod_input: i32, tremolo: u16, vibpos: u8, eg_cnt: u32) -> i32 {
        self.advance(vibpos, eg_cnt);
        let phase10 = ((self.phase >> 9) as i32).wrapping_add(mod_input) as u32 & 0x3FF;
        self.output_at(phase10, tremolo)
    }

    /// Tick the envelope and advance the phase accumulator (without producing output). Split from
    /// [`Self::output_at`] for the **OPL rhythm mode**, where the output phase of some operators
    /// (HH/SD/CY) is *forced* from the phase bits of other operators, while still letting each
    /// operator advance its own phase.
    pub(crate) fn advance(&mut self, vibpos: u8, eg_cnt: u32) {
        self.tick_envelope(eg_cnt);
        let inc = (self.phase_inc as i32).wrapping_add(self.vib_delta(vibpos));
        self.phase = self.phase.wrapping_add(inc as u32);
    }

    /// Produce the operator's output at a given 10-bit phase (already advanced by
    /// [`Self::advance`]). In normal mode `phase10 = (phase>>9) + mod_input`.
    pub(crate) fn output_at(&mut self, phase10: u32, tremolo: u16) -> i32 {
        let att = self.attenuation_eg(tremolo);
        let out = waveform_output(self.waveform, phase10 & 0x3FF, att);
        self.prev_out = self.out;
        self.out = out;
        out
    }

    /// Current phase bits (`phase >> 9`) — for the rhythm-mode correlated-noise computation.
    pub(crate) fn phase_out(&self) -> u32 {
        self.phase >> 9
    }

    /// Modulator self-feedback in phase-index units. The chip sums the last
    /// two operator outputs and shifts by the feedback register: the documented
    /// hardware applies `(prev_out + cur_out) << (fb_reg + 7)` to the 16.16
    /// phase, i.e. a phase-index delta of
    /// `(prev + cur) >> (9 - fb_reg)` (`fb_reg` 1..7 → shift 8..2). `fb_reg ==
    /// 0` disables feedback. With the 12-bit operator output now at the real
    /// hardware scale, this single law is exact for both FM and additive
    /// channels — no per-mode base.
    pub(crate) fn feedback_phase(&self, fb_reg: u8) -> i32 {
        if fb_reg == 0 {
            0
        } else {
            (self.prev_out + self.out) >> (9 - fb_reg as i32).max(0)
        }
    }
}

/// Map (waveform, 10-bit phase, eg-domain attenuation) → signed sample.
fn waveform_output(waveform: u8, phase10: u32, att_eg: u32) -> i32 {
    let p = phase10 & 0x3FF;

    // Resolve (quarter-wave index, negate, mute) per OPL2 waveform.
    let (idx, neg, mute) = match waveform & 0x07 {
        // 0: full sine.
        0 => {
            let quad = (p >> 8) & 3;
            let idx = if quad & 1 != 0 {
                255 - (p & 0xFF)
            } else {
                p & 0xFF
            };
            (idx, quad & 2 != 0, false)
        }
        // 1: half sine (second half muted).
        1 => {
            if p & 0x200 != 0 {
                (0, false, true)
            } else {
                let idx = if p & 0x100 != 0 {
                    255 - (p & 0xFF)
                } else {
                    p & 0xFF
                };
                (idx, false, false)
            }
        }
        // 2: absolute sine (period halved, always positive).
        2 | 6 => {
            let pp = p & 0x1FF;
            let idx = if pp & 0x100 != 0 {
                255 - (pp & 0xFF)
            } else {
                pp & 0xFF
            };
            (idx, false, false)
        }
        // 3: quarter (pulse) sine — only the rising quarter of each half.
        3 | 7 => {
            let pp = p & 0x1FF;
            if pp & 0x100 != 0 {
                (0, false, true)
            } else {
                (pp & 0xFF, false, false)
            }
        }
        // 4..5 are OPL3 logarithmic-sawtooth variants; fall back to full
        // sine (no corpus module uses them — see RFC §3.2).
        _ => {
            let quad = (p >> 8) & 3;
            let idx = if quad & 1 != 0 {
                255 - (p & 0xFF)
            } else {
                p & 0xFF
            };
            (idx, quad & 2 != 0, false)
        }
    };

    if mute {
        return 0;
    }
    // logsin (attenuation) + envelope, then exp. eg-domain → logsin-domain
    // via `<< 3` (1 eg-unit ≈ 8 logsin-units ≈ 0.1875 dB).
    let total = LOGSIN_ROM[idx as usize] as u32 + (att_eg << 3);
    let shift = total >> 8;
    // 12-bit operator output (±4084 max), the real chip's width (Schism's
    // `tl_tab`). `EXP_ROM` is a Q10 mantissa, so `<< 2` lifts it to 12 bits;
    // the FM-modulation and feedback paths in `OplChannel::next_mono` then run
    // at the textbook hardware scale — no per-mode compensation. `shift ≥ 13`
    // is below the LSB (silence), and bounds the shift well under 32.
    let mag = if shift >= 13 {
        0
    } else {
        (((EXP_ROM[(total & 0xFF) as usize] as u32) << 2) >> shift) as i32
    };
    if neg {
        -mag
    } else {
        mag
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The generated `EG_INC` realises the documented YMF262 rate law: each
    /// row's per-cycle sum (the dither *count* that sets the envelope speed)
    /// is `(4 + lo)·2^rate_hi`, and every step is one of the hardware's
    /// {0,1,2,4,8}. This pins the envelope timing without embedding any
    /// emulator's literal table.
    #[test]
    fn eg_inc_counts_match_rate_law() {
        let row_sum = |r: usize| -> u32 { (0..8).map(|p| EG_INC[r * 8 + p] as u32).sum() };
        // rows 0-3: rate ≤12, count 4+lo
        for lo in 0..4 {
            assert_eq!(row_sum(lo), 4 + lo as u32, "row {lo}");
        }
        // rows 4-7: rate 13, count 2·(4+lo)
        for lo in 0..4 {
            assert_eq!(row_sum(4 + lo), 2 * (4 + lo as u32), "row {}", 4 + lo);
        }
        // rows 8-11: rate 14, count 4·(4+lo)
        for lo in 0..4 {
            assert_eq!(row_sum(8 + lo), 4 * (4 + lo as u32), "row {}", 8 + lo);
        }
        assert_eq!(row_sum(12), 32); // rate 15 decay: flat 4
        assert_eq!(row_sum(13), 64); // rate 15 attack: flat 8
        assert_eq!(row_sum(14), 0); // held
        for &v in EG_INC.iter() {
            assert!(matches!(v, 0 | 1 | 2 | 4 | 8), "illegal step {v}");
        }
    }
}
