//! Shared per-frame Rob-Hubbard modulation engine for BOTH SID drivers
//! ([`super::driver::SidBank`] per-instrument and [`super::coupled::CoupledSid`]
//! shared-chip). The per-voice per-frame effect chain (vibrato → drum →
//! skydive → interp-vibrato → arp → arp2 → alt-arp → wave-alt) is identical
//! between the two drivers; only the chip topology (3 chips vs 1) and the
//! global filter differ. Centralising it here removes the copy-paste that let
//! the two engines drift (every effect previously had to be edited twice).
//!
//! Effects a given tune does not use are gated by their `SidFx` flag (0/false),
//! so the chain reduces to exactly the subset that driver previously ran — the
//! two per-instrument tunes (monty, human_race) set NO coupled-only flag, so
//! routing them through this superset chain is bit-identical (verified against
//! the regdump + the commando golden).

use super::{
    shift_semitones_down, shift_semitones_up, SidFx, CTRL_GATE, SEMI_DEN, SEMI_NUM, VIB_GATE_FRAMES,
};

/// Shift `base` (milli-Hz) by a SIGNED number of equal-tempered semitones
/// (positive = up, negative = down) — the kernel of the three-note arpeggio's
/// arbitrary offsets.
#[inline]
fn shift_semitones_signed(base: u32, semis: i8) -> u32 {
    if semis >= 0 {
        shift_semitones_up(base, semis as u8)
    } else {
        shift_semitones_down(base, (-(semis as i16)) as u8)
    }
}

/// Per-voice modulation state the engine reads and updates each frame. Both
/// drivers copy their voice's fields in, call [`modulate`], then copy back.
pub(crate) struct FxState {
    pub base_milli_hz: u32,
    pub gated: bool,
    pub note_frame: u32,
    pub drum_hi: u8,
    pub gate_off_ctrl: u8,
    pub skydive_semis: u8,
    pub interp_ctr: u8,
    pub interp_up: bool,
}

/// `freq_reg = (mHz << 24) / (chip_clock_hz · 1000)`, clamped to 16 bits. The
/// single source of truth for both drivers' milli-Hz→SID-register conversion.
#[inline]
pub(crate) fn freq_reg(milli_hz: u32, clock_hz: u32) -> u16 {
    (((milli_hz as u64) << 24) / (clock_hz as u64 * 1000)).min(65535) as u16
}

/// Run the full per-frame effect chain for one voice and return
/// `(freq_register, optional control-register override)`. The caller is
/// responsible for the `note_frame` increment, the skydive-climb counter, and
/// the `base == 0` guard (both drivers already do these in their own extract
/// block); this function only computes the registers and updates `drum_hi` /
/// the interp ping-pong. Mirrors the replayer's order: the LAST pitch effect
/// wins the freq register, the drum / wave-alt own the control register.
pub(crate) fn modulate(
    s: &mut FxState,
    fx: &SidFx,
    frame: u32,
    speed: u8,
    clock_hz: u32,
) -> (u16, Option<u8>) {
    let base = s.base_milli_hz;
    let gated = s.gated;
    let note_frame = s.note_frame;
    let drum_hi0 = s.drum_hi;
    let gate_off_ctrl = s.gate_off_ctrl;
    let skydive_semis = s.skydive_semis;

    let mut reg = freq_reg(base, clock_hz);

    // Vibrato: v30 length-gated BIPOLAR branch, else the v10 upward branch
    // (+ overflow-note `tempvdif` register step).
    let vib_ok = !fx.vib_len_gate || (gated && note_frame >= VIB_GATE_FRAMES);
    if fx.vibrato && fx.vib_depth != 0 && vib_ok && fx.vib_len_gate {
        let depth = fx.vib_depth as i64;
        let period = (2 * depth) as u32;
        let p = (frame % period) as i64;
        let phase = if p <= depth { p } else { period as i64 - p };
        let semitone = (base as i64) * SEMI_NUM / SEMI_DEN;
        let step = semitone >> (fx.vib_div as u32);
        let delta = (phase - depth / 2) * step;
        reg = freq_reg((base as i64 + delta).max(0) as u32, clock_hz);
    } else if fx.vibrato && fx.vib_depth != 0 && vib_ok {
        let mut osc = (frame & 7) as i64;
        if osc > 3 {
            osc ^= 7;
        }
        if fx.vib_tempvdif_reg != 0 {
            let base_reg = freq_reg(base, clock_hz) as i64;
            reg = (base_reg + fx.vib_tempvdif_reg as i64 * osc).clamp(0, 0xFFFF) as u16;
        } else {
            let shift = ((fx.vib_depth as u32) >> 2).min(8) + 1;
            let semitone = (base as i64) * SEMI_NUM / SEMI_DEN;
            let delta = (semitone >> shift) * osc;
            reg = freq_reg((base as i64 + delta).max(0) as u32, clock_hz);
        }
    }

    // Drum (`instrfx & 1`): noise burst for the first row, then freq-hi slide +
    // gate-off. Owns the control register (a later arp overrides freq, not ctrl).
    let mut ctrl_override = None;
    let mut new_drum_hi = drum_hi0;
    if fx.drum && gated && drum_hi0 != 0 {
        // Drum bit-0: NOISE burst on the note's first row, then gate-off so the
        // note decays. The v10 routine ALSO slides the freq-hi byte down each
        // frame (`drum_hi--`, register-exact vs commando); the v15 routine
        // (`$e2c6`) does NOT — it keeps the freq-hi constant, so for v15
        // (`drum_no_freq_slide`) the drum owns ONLY the control register (the v10
        // slide invented downward glissandi the v15 oracle never produces —
        // verified vs the sidplay regdump).
        if note_frame < speed as u32 {
            if !fx.drum_no_freq_slide {
                reg = (reg & 0x00FF) | ((drum_hi0 as u16) << 8);
            }
            ctrl_override = Some(0x80);
        } else {
            if !fx.drum_no_freq_slide {
                new_drum_hi = drum_hi0 - 1;
                reg = (reg & 0x00FF) | ((drum_hi0 as u16) << 8);
            }
            ctrl_override = Some(if gate_off_ctrl == 0 {
                0x80
            } else {
                gate_off_ctrl
            });
        }
    }

    // Freq-add skydive: every other frame slide the freq-hi byte. Ghidra ground
    // truth — the replayer runs this with NO gate test, only:
    //   monty `play` @ $8012 : `instrfx&2 && (counter&1) && save_freq_high!=0`
    //   crazy `cc_play` @ $500c: `instrfx&2 && (0x10 < (savelnthcc&0x1f)) && …`
    // i.e. the discriminant is the per-tune NOTE-LENGTH threshold `skydive_when`
    // (monty 0, commando 2, crazy 16), never the gate. So when `skydive_when==0`
    // (no length gate) the slide is unconditional and CONTINUES past gate-off,
    // gliding a held note's pitch down on its release tail (Monty voice-1 lead at
    // row 0x21). For `skydive_when>0` the faithful test is `note_length>when`,
    // which this engine does not yet carry per-note; falling back to `gated`
    // there is conservative — it stops the slide at gate-off, matching the oracle
    // for Crazy Comets (short notes hold a constant freq-hi post-gate-off; the
    // earlier unconditional version crashed their pitch) and keeping the Commando
    // drum bit-exact. (Modelling the real per-note length gate for `when>0` is
    // the remaining follow-up.)
    if fx.skydive_add != 0
        && (gated || fx.skydive_when == 0)
        && (frame & 1 != 0)
        && new_drum_hi != 0
    {
        reg = (reg & 0x00FF) | ((new_drum_hi as u16) << 8);
        let delta = (fx.skydive_add as i32) >> 8;
        new_drum_hi = (new_drum_hi as i32 + delta).clamp(0, 255) as u8;
    }

    // Note-increment skydive (climb): pitch shifted up `skydive_semis` semitones.
    if fx.skydive_climb && skydive_semis != 0 {
        reg = freq_reg(shift_semitones_up(base, skydive_semis), clock_hz);
    }

    // Period-interpolation vibrato (centred triangle; Thrust/Spellbound v15).
    if fx.interp_vib && fx.interp_half_depth > 0 {
        let base_reg = freq_reg(base, clock_hz) as i32;
        let down1 = freq_reg(shift_semitones_down(base, 1), clock_hz) as i32;
        let step = (base_reg - down1) >> fx.interp_shift;
        let half = (fx.interp_half_depth >> 1) as i32;
        let ctr = s.interp_ctr as i32;
        reg = (base_reg - half * step + ctr * step).clamp(0, 0xFFFF) as u16;
        let depth = fx.interp_half_depth;
        if s.interp_up {
            if s.interp_ctr >= depth {
                s.interp_up = false;
                s.interp_ctr = depth.saturating_sub(1);
            } else {
                s.interp_ctr += 1;
            }
        } else if s.interp_ctr == 0 {
            s.interp_up = true;
            s.interp_ctr = 1;
        } else {
            s.interp_ctr -= 1;
        }
    }

    // Octave arpeggio (v10 bit 2): even frame note, odd frame note×2.
    if fx.arp {
        let n = if frame & 1 == 0 {
            base
        } else {
            base.saturating_mul(2)
        };
        reg = freq_reg(n, clock_hz);
    }

    // Three-note arpeggio (fxmask bit4): a global, phase-locked 3-frame loop
    // shifting the note by `0, off_a, off_b` SIGNED semitones (the played note
    // plus two arbitrary offsets). The replayer's shared counter wraps with
    // period 3 across all voices; `frame % 3` reproduces that global phase. It
    // fully owns the freq on its frames (replayer order: runs among the arps).
    if fx.arp3 {
        let off = [0i8, fx.arp3_off_a, fx.arp3_off_b][(frame % 3) as usize];
        reg = freq_reg(shift_semitones_signed(base, off), clock_hz);
    }

    // v30 two-note arpeggio: toggle between the struck note and `arp2_reg`.
    if fx.arp2_reg != 0 && note_frame & 1 == 0 {
        reg = fx.arp2_reg;
    }

    // v15 two-note arpeggio (Spellbound): drop the (climbed) note by
    // `alt_arp_semitones` on alternate frames. Runs last among the pitch effects.
    if fx.alt_arp_semitones != 0 {
        let climbed = if fx.skydive_climb && skydive_semis != 0 {
            shift_semitones_up(base, skydive_semis)
        } else {
            base
        };
        reg = if frame & 1 == 0 {
            freq_reg(
                shift_semitones_down(climbed, fx.alt_arp_semitones),
                clock_hz,
            )
        } else {
            freq_reg(climbed, clock_hz)
        };
    }

    // v30 waveform alternation: toggle the control register between the
    // instrument waveform and `wave_alt_ctrl` every frame (only while gated).
    if fx.wave_alt_ctrl != 0 && gated {
        ctrl_override = Some(if note_frame & 1 == 1 {
            gate_off_ctrl | CTRL_GATE
        } else {
            fx.wave_alt_ctrl
        });
    }

    if new_drum_hi != drum_hi0 {
        s.drum_hi = new_drum_hi;
    }
    (reg, ctrl_override)
}
