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

use super::wave_program::{self, ProgramState};
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
    /// Per-note arpeggio phase — a counter of its own, because the replayer
    /// steps it inside the effect chain and a fetch frame skips that.
    pub arp_phase: u32,
    pub drum_hi: u8,
    pub gate_off_ctrl: u8,
    pub skydive_semis: u8,
    pub interp_ctr: u8,
    pub interp_up: bool,
    /// The vibrato's start-delay countdown (`lh_vib_delay`, `$1a5e,X`). It ticks
    /// only on frames the effect chain runs, so it is state, not a deadline —
    /// see the guard in [`modulate`].
    pub vib_delay: u8,
    /// Voice-program cursor + running pitch bend. Reset at note-on.
    pub prog: ProgramState,
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
/// One step of the period-interpolation vibrato's ping-pong position.
///
/// `hold_top` is the one difference between the two replayer generations that
/// share this counter: the last player's `$14b2` does `INC pos / LDA range /
/// CMP pos / BCS done`, which leaves the position ON `range` for a second frame
/// before turning (period 2·depth + 1), while the v15 routine steps straight
/// back down (period 2·depth).
pub(crate) fn advance_pingpong(ctr: &mut u8, up: &mut bool, depth: u8, hold_top: bool) {
    if *up {
        if *ctr >= depth {
            *up = false;
            if !hold_top {
                *ctr = depth.saturating_sub(1);
            }
        } else {
            *ctr += 1;
        }
    } else if *ctr == 0 {
        *up = true;
        *ctr = 1;
    } else {
        *ctr -= 1;
    }
}

/// The replayer's `lh_sounding_note` (`$1a52,X`): the struck note plus the
/// arpeggio's current offset, computed at `$1449` BEFORE anything reads a
/// frequency. Both the note reload and the vibrato are built on it.
///
/// Returns the struck note unchanged when no arpeggio is armed, so it is safe
/// to call on every path.
fn sounding_note(fx: &SidFx, s: &FxState, frame: u32, base: u32, skydive_semis: u8) -> u32 {
    if fx.arp_len == 0 {
        return base;
    }
    let phase = if fx.arp_per_note { s.arp_phase } else { frame };
    let off = fx.arp_steps[(phase % fx.arp_len.min(3) as u32) as usize];
    let from = if fx.arp_follow_climb && fx.skydive_climb && skydive_semis != 0 {
        shift_semitones_up(base, skydive_semis)
    } else {
        base
    };
    shift_semitones_signed(from, off)
}

/// What one modulated frame asks the driver to write.
pub(crate) struct Modulated {
    /// Frequency register value.
    pub reg: u16,
    /// Control register, when an effect owns it this frame.
    pub ctrl: Option<u8>,
    /// Write ONLY the frequency register's HIGH byte (`$d401`) and leave the
    /// low byte as it was. The voice program's `Fixed` step is the one thing
    /// that does this — see [`wave_program`] and the consumer below.
    pub freq_hi_only: bool,
    /// Does the end-of-note gate mask apply to `ctrl`? The replayer's ordinary
    /// write ANDs the control shadow with `$1a4c,X` (`$17bd`), which drops to
    /// `0xFE` once the note's last row has passed; a `Fixed` program step
    /// bypasses that write and is stored raw.
    pub ctrl_masked: bool,
}

pub(crate) fn modulate(
    s: &mut FxState,
    fx: &SidFx,
    frame: u32,
    speed: u8,
    clock_hz: u32,
) -> Modulated {
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
        // `vib_div` is a right-shift divisor decoded from instrument data; real
        // tunes carry tiny values. Clamp to the i64 width so a hand-crafted /
        // deserialized `SidFx` with `vib_div >= 64` cannot overflow the shift
        // (debug panic). No-op for every real value — bit-identical.
        let step = semitone >> (fx.vib_div as u32).min(63);
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

    // Period-interpolation vibrato (centred triangle; Thrust/Spellbound v15,
    // and the last player).
    //
    // Two timing gates, both no-ops at 0 so the v10–v30 corpus is untouched:
    //  * `interp_delay` — the replayer holds the note dead straight for this
    //    many frames and skips the whole stage, so the ping-pong counter does
    //    NOT advance either (Ghidra `lh_fx_vibrato $147b`: `DEC lh_vib_delay`
    //    then jump past everything).
    //  * `interp_flat` — once running, the swing term only joins from this
    //    frame on, while the CENTRING applies from the first. The attack
    //    therefore sits half a swing below the note and scoops up into it.
    // A delay of D must skip frames 1..=D and resume at D+1: `note_frame` is
    // already 1 on the first MODULATED frame, and the replayer decrements its
    // per-voice counter on each of those D frames before falling through
    // (`$1486`). `>=` skipped one frame too few, giving the ping-pong an extra
    // advance on every delayed note. No-op at delay 0.
    //
    // And it swings around the SOUNDING note, not the struck one. The replayer
    // adds the arpeggio's offset first (`$1449` → `$1a52,X`), reloads the
    // frequency from that (`$1466`), and only then runs the vibrato, which
    // reads `$1a52,X` at `$14c3` to pick BOTH the swing's centre (`$14fa`,
    // `freqtab[note]`) and its step (`$14de`, `freqtab[note] - freqtab[note-1]`
    // shifted right `param & 7` times). So on an arpeggiating note the swing
    // rides the arpeggio note by note — it is not a swing around the struck
    // note that a later arpeggio stage overwrites, which is how this engine had
    // it. `interp_flat` keeps the v15 generation on its own arrangement.
    let mut interp_applied = false;
    // The start delay is a per-voice COUNTDOWN, not a deadline. `$1486` reads
    // `lh_vib_delay` ($1a5e,X), and while it is non-zero decrements it and jumps
    // past the whole stage — so it only ticks on frames where the effect chain
    // RUNS, and a pattern-entry fetch freezes it like everything else. Comparing
    // against `note_frame` instead conflated the two, because that counter is
    // bumped on every path including fetches: any fetch landing inside the delay
    // window let the vibrato start one frame early, and since nothing ever
    // resynchronises the ping-pong, that one frame stayed wrong for the rest of
    // the tune. Read straight off the replayer's RAM — Lion_Heart voice 0's
    // delay runs `5 4 3 2 1 1 0`, the repeat being a fetch.
    if s.vib_delay > 0 {
        s.vib_delay -= 1;
    } else if fx.interp_vib && fx.interp_half_depth > 0 {
        let centre = if fx.interp_flat != 0 {
            sounding_note(fx, s, frame, base, skydive_semis)
        } else {
            base
        };
        interp_applied = fx.interp_flat != 0;
        let base_reg = freq_reg(centre, clock_hz) as i32;
        let down1 = freq_reg(shift_semitones_down(centre, 1), clock_hz) as i32;
        // `interp_shift` is instrument-derived; real tunes use small values.
        // Clamp to the i32 width so a crafted / deserialized `SidFx` with
        // `interp_shift >= 32` cannot overflow the shift (debug panic). No-op
        // for every real value — bit-identical.
        let step = (base_reg - down1) >> fx.interp_shift.min(31);
        let half = (fx.interp_half_depth >> 1) as i32;
        let depth = fx.interp_half_depth;
        // ORDER. The last player advances its ping-pong position and only THEN
        // builds the swing from it (`$14a2` steps `$1003,X`; `$151c` reads it),
        // while the v15 routine emits first. `interp_flat` is non-zero only on
        // last-player imports, so it selects the generation without touching
        // thrust / spellbound.
        if fx.interp_flat != 0 {
            advance_pingpong(&mut s.interp_ctr, &mut s.interp_up, depth, true);
        }
        let ctr = if note_frame >= fx.interp_flat as u32 {
            s.interp_ctr as i32
        } else {
            0
        };
        reg = (base_reg - half * step + ctr * step).clamp(0, 0xFFFF) as u16;
        if fx.interp_flat != 0 {
            // Already advanced above.
        } else if s.interp_up {
            if s.interp_ctr >= depth {
                s.interp_up = false;
                s.interp_ctr = depth.saturating_sub(1);
                // The two replayer generations sharing this branch turn around
                // differently, so `interp_flat` (non-zero only on last-player
                // imports) selects between them:
                //   * v15 `play $09f1` (thrust / spellbound) steps straight back
                //     down — period 2·depth;
                //   * the last player HOLDS the top a second frame — `$14b2`,
                //     `INC pos / LDA range / CMP pos / BCS done`, leaves pos ON
                //     `range` without turning — period 2·depth + 1.
                // Read off the replayer's own work-RAM, not inferred:
                // `sidplay --statedump --stateaddrs 1003,1006,1009,1a5b,1a5e`
                // shows `… 4 5 5 4 …` against our `… 4 5 4 …`.
                //
                // ⚠️ Every earlier attempt to give the last player its own
                // turnaround measured WORSE, and that verdict is now known to
                // have been taken against two defects that have since been
                // fixed — the voice program writing pitch it never writes, and
                // the missing rest fetches. Do not re-copy those numbers; they
                // were measured in a different engine.
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

    // Cyclic semitone arpeggio — the one code path behind the v15 two-note drop
    // (`[-n, 0]`, riding the skydive climb), the v20/v25 three-note arp (fxmask
    // bit4, `[0, off_a, off_b]`), and the last player's per-note nibble arp.
    // The replayer's shared counter wraps across all voices, which `frame`
    // reproduces; `arp_per_note` switches to the per-note counter instead. It
    // fully owns the freq on its frames (replayer order: runs among the arps).
    // The replayer's own per-note phase is `lh_arp_phase` ($1a58,X), stepped at
    // `$1427` INSIDE the effect chain — so it freezes on every pattern-entry
    // fetch, unlike `frames_since_note_on`, which is bumped on every path.
    // `arp_per_note` selects it; the shared `frame` counter serves the earlier
    // generations, whose replayers really do wrap one counter across all voices.
    //
    // This stage is the NOTE RELOAD (`$1466`), not a pitch effect of its own:
    // it puts `freqtab[sounding note]` in the frequency register. When a vibrato
    // is running it has already built its swing on the same sounding note and
    // owns the register, so this must not overwrite it — the replayer's vibrato
    // writes `$1aaf/$1aac` last (`$1539`).
    if fx.arp_len != 0 && !interp_applied {
        let phase = if fx.arp_per_note { s.arp_phase } else { frame };
        // `arp_len` is instrument-derived (2 or 3 in every real tune). Clamp to
        // the array width so a crafted / deserialized `SidFx` cannot index out
        // of bounds. No-op for every real value — bit-identical.
        let off = fx.arp_steps[(phase % fx.arp_len.min(3) as u32) as usize];
        let from = if fx.arp_follow_climb && fx.skydive_climb && skydive_semis != 0 {
            shift_semitones_up(base, skydive_semis)
        } else {
            base
        };
        reg = freq_reg(shift_semitones_signed(from, off), clock_hz);
    }

    // v30 two-note arpeggio: toggle between the struck note and `arp2_reg`.
    // The only arpeggio that is not a semitone cycle (an absolute register).
    if fx.arp2_reg != 0 && note_frame & 1 == 0 {
        reg = fx.arp2_reg;
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

    // Voice program (the "wavetable"): runs LAST and OWNS the voice's waveform
    // and pitch. A `Bend` step accumulates into the program's own offset, which
    // is SUBTRACTED from whatever the effect chain above computed — so the
    // program bends *from* the played note (vibrato and portamento included),
    // exactly like the replayer re-seeds the frequency each frame and then
    // subtracts its accumulator.
    //
    // A `Fixed` step pins the frequency register's HIGH byte and writes NOTHING
    // ELSE: Ghidra `$16c1` stores `$d404` and `$d401` and then jumps straight to
    // `lh_voice_loop_next` (`$17cf`), so `$d400` keeps whatever the last full
    // write left there — the low byte of the note as of the previous Bend or
    // trigger frame, NOT the current note's. Recomposing it from the current
    // note instead put every pinned frame a few register units off (Lion_Heart
    // voice 1, `20c8` against our `200d`).
    //
    // And once the program ends it stops touching the pitch entirely — the bend
    // accumulator included. The replayer subtracts the accumulator inside the
    // Bend step (`$16f4`), not on every frame of the note, so a settled voice
    // returns to the plain played note rather than to a permanently bent one.
    let mut freq_hi_only = false;
    let mut ctrl_masked = false;
    if let Some(out) = wave_program::step(&fx.program, &mut s.prog) {
        ctrl_override = Some(out.ctrl);
        ctrl_masked = out.masked;
        match out.pitch {
            wave_program::ProgramPitch::FixedHi(hi) => {
                reg = (reg & 0x00FF) | ((hi as u16) << 8);
                freq_hi_only = true;
            }
            wave_program::ProgramPitch::Bend => {
                reg = (reg as i32 - s.prog.bend_acc as i32).clamp(0, 0xFFFF) as u16;
            }
            wave_program::ProgramPitch::Released => {}
        }
    }

    if new_drum_hi != drum_hi0 {
        s.drum_hi = new_drum_hi;
    }
    Modulated {
        reg,
        ctrl: ctrl_override,
        freq_hi_only,
        ctrl_masked,
    }
}
