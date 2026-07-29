//! Coupled (shared-chip) SID driver — the **opt-in** counterpart to the
//! default one-chip-per-instrument [`super::driver::SidBank`], for tunes whose
//! voices **hard-sync or ring-modulate each other** (commando v2 syncs to v1,
//! zoids/commando v1 ring-modulate). Those links only work when the three SID
//! voices live on the **same** chip in their real voice slots, because the
//! sync/ring routing is fixed (SID voice 2←1, 1←3, 3←2) and sidera already
//! emulates it across `voices[0/1/2]`. The per-instrument model puts each
//! note on a *separate* chip, so the links have no source — hence this model.
//!
//! Here tracker channel `i` maps to chip voice slot `i` (1:1, the SID is
//! monophonic per voice), and each voice carries its OWN per-frame Rob-Hubbard
//! effect state + `SidFx` (each voice may play a different instrument). It is a
//! completion of the per-instrument model, not a replacement — the player
//! selects it only when the song uses sync/ring (see the importer's detection).

use sidera::registers::{
    CTRL_GATE, CTRL_RING, CTRL_SYNC, CTRL_TEST, FC_HI, FC_LO, MODE_VOL, RES_FILT, V1_ATTACK_DECAY,
    V1_CONTROL, V1_FREQ_HI, V1_FREQ_LO, V1_PULSE_HI, V1_PULSE_LO, V1_SUSTAIN_RELEASE,
};
use sidera::{ChipModel, Sid};

use super::{SidFx, SidModel, SidRegion, SidVoicePatch};

/// Per-frame Rob-Hubbard modulation state for one coupled voice (the same axes
/// as the per-instrument [`super::VoiceSlot`], but each voice owns its effects
/// because voices play different instruments here).
#[derive(Clone, Copy, Default)]
struct CoupledVoice {
    /// Is this voice currently sounding (gate held or releasing)?
    active: bool,
    gated: bool,
    release_nibble: u8,
    release_tail: u32,
    base_milli_hz: u32,
    note_frame: u32,
    /// `$d400` as it stood at the START of this frame — the value the replayer
    /// would still be holding when a voice-program `Fixed` step writes `$d401`
    /// alone. The tracker refreshes the played note before the effect pass, so
    /// the low byte has to be restored from here rather than simply not written.
    freq_lo_prev: u8,
    /// Diagnostic: did this voice's per-frame effect chain run this frame? The
    /// replayer's pattern-entry fetch ends in `goto lh_write_voice_regs`, so a
    /// frame where the chain did NOT run is exactly a fetch — the one signal
    /// that can be compared, frame for frame, against the replayer's own
    /// pattern cursor (`$1a34,X`).
    fx_ran: bool,
    drum_hi: u8,
    gate_off_ctrl: u8,
    skydive_semis: u8,
    fx: SidFx,
    // Per-voice pulse-width sweep (in the coupled model the instrument's PW
    // state travels with the voice it is programmed on).
    pw_cur: i32,
    pulse_dir: u8,
    pulse_delay_ctr: i32,
    pw_init: bool,
    /// SID filter cutoff-hi accumulator (the replayer's per-voice `$D416`
    /// state): seeded on note-on, `+= fx.filter_step` each frame, u8 wrap.
    /// The WRAP law (v10–v30) only.
    filter_cutoff_hi: u8,
    /// BOUNCE law (last player): the 16-bit cutoff accumulator and the direction
    /// it is currently travelling in. Untouched while `fx.filter_bounce` is
    /// clear, so the wrap law above is bit-for-bit what it always was.
    filter_cutoff16: u16,
    filter_up: bool,
    /// Voice-program cursor + running pitch bend. Reset at every note-on, so a
    /// program always restarts at step 0 on a new note.
    prog: super::wave_program::ProgramState,
    /// Frames left before this note's release envelope takes over
    /// (`ReleaseMode::Ramp`). Seeded per NOTE, counted down each frame, fires
    /// once at zero. `0` = this note has no ramp.
    release_ctr: u16,
    /// Instrument index this voice last played. The replayer resets the filter
    /// accumulator only when a voice (re)loads its phrase — approximated here by
    /// an instrument change: a same-instrument note-on continues the sweep
    /// (legato within a phrase) instead of re-seeding (see SID_FILTER_PLAN A2).
    cur_instr: usize,
    /// Per-note arpeggio phase (`lh_arp_phase` at `$1a58,X`). NOT derived from
    /// `note_frame`: the replayer bumps it at `$1427`, inside the effect chain,
    /// which a pattern-entry fetch frame jumps over — so it freezes on those
    /// frames while `frames_since_note_on` keeps counting. Reset at note-on.
    arp_phase: u32,
    /// Period-interpolation vibrato (`play $09f1`) ping-pong counter (0..=depth)
    /// and its direction. One step per frame; reset at note-on.
    interp_ctr: u8,
    interp_up: bool,
    /// The replayer's `lh_vib_delay` ($1a5e,X), reloaded at every note-on that
    /// does not carry a `0x86` marker and ticked only inside the effect chain.
    vib_delay: u8,
    /// Has this voice already taken its generation's ping-pong seed? Only the
    /// FIRST last-player note replaces the constructor's Thrust phase; after
    /// that the counter free-runs like the replayer's.
    interp_seeded: bool,
    /// Two-phase waveform (v20 effect 0x4): the raw attack control byte and the
    /// number of frames it still plays before the sustain waveform takes over.
    /// `wave_attack_ctrl == 0` ⇒ feature off for this voice.
    wave_attack_ctrl: u8,
    wave_attack_ctr: u8,
    /// Set once the attack window has expired and the sustain waveform has been
    /// restored, so the restore is not re-emitted on every later frame.
    wave_attack_done: bool,
    /// Two-phase PITCH attack (fxmask bit6): the SID freq register of the
    /// absolute attack note, written for the attack phase (sharing
    /// `wave_attack_ctr` with the waveform attack). 0 ⇒ no pitch attack.
    wave_attack_reg: u16,
    /// The instrument's stored control byte for the SUSTAIN phase after a
    /// two-phase attack: `gate_off_ctrl` plus the instrument's own gate bit. An
    /// instrument defined with the gate clear plucks (gate drops after attack);
    /// one defined with the gate set sustains. (Ghidra lightforce `play` $f4ca:
    /// the sustain ctrl is read from the instrument table AS-IS, not re-gated.)
    sustain_ctrl: u8,
}

/// One shared sidera chip driving three coupled voices (channel `i` → voice
/// `i`), so sidera's native sync / ring modulation links them.
pub struct CoupledSid {
    chip: Sid,
    chip_clock_hz: u32,
    output_rate: u32,
    voices: [CoupledVoice; 3],
    /// The replayer's global VBlank `counter` (shared vibrato / arp / skydive
    /// phase). Advanced once per frame in [`Self::begin_frame`].
    frame_counter: u32,
    speed: u8,
    reg: [u8; 0x20],
    /// Per-INSTRUMENT pulse-width accumulator (the replayer's instrument table
    /// `DAT_5591[instr*8]`, mutated in place by the sweep). Indexed by tracker
    /// instrument number; `None` until the instrument first plays (then seeded
    /// from its patch base). Lets a re-triggered note resume the instrument's
    /// own swept PW even if another instrument played on the voice in between.
    /// Fixed inline table — SID tunes have a handful of instruments (≤ ~20);
    /// indices beyond the table just don't persist (fall back to the patch base).
    instr_pw: [Option<i32>; Self::MAX_INSTR],
}

use super::RELEASE_MS;
// The per-semitone pitch helpers and the SEMI_* constants used to live here AND
// in `mod.rs` (duplicated). They now live once in `mod.rs` (`pub(crate)`) and are
// used by the shared `fx_engine`; this driver no longer references them directly.

impl CoupledSid {
    /// Size of the per-instrument PW table. Rob-Hubbard SID tunes carry well
    /// under this many instruments; higher indices simply don't persist their PW.
    const MAX_INSTR: usize = 32;

    /// Build a coupled (shared-chip) SID driver at `output_rate` Hz for the
    /// given chip `model` / `region`; `speed` is the replayer tick divider.
    pub fn new(output_rate: u32, model: SidModel, region: SidRegion, speed: u8) -> Self {
        let output_rate = output_rate.max(1);
        let mut chip = Sid::new(output_rate);
        chip.set_model(match model {
            SidModel::Mos6581 => ChipModel::Mos6581,
            SidModel::Mos8580 => ChipModel::Mos8580,
        });
        let chip_clock_hz = region.clock_hz();
        chip.set_chip_clock(chip_clock_hz);
        let mut s = Self {
            chip,
            chip_clock_hz,
            output_rate,
            voices: [CoupledVoice::default(); 3],
            frame_counter: u32::MAX,
            speed: speed.max(1),
            reg: [0u8; 0x20],
            instr_pw: [None; Self::MAX_INSTR],
        };
        // Master volume 0x0F (no filter mode) — written through `wr` so the
        // shadow `reg` array (and thus `snapshot`) reflects it. Tunes that use
        // the filter raise this to 0x1F (LP) in `note_on`.
        s.wr(MODE_VOL, 0x0F);
        // De-phase the per-voice interp-vibrato counters. GHIDRA-DERIVED, not a
        // guess. The counter `$cce` is never written by code: init `$1567` clears
        // only $cae/$cb1/$cb4, the play loop at `$09ce` only ping-pongs it, and
        // note-fetch never touches it. Its start value is BAKED INTO THE TUNE
        // IMAGE — Thrust.sid ships $0cce/$0ccf/$0cd0 = [2,6,2], $0cee.. = [0,0,0]
        // (rising). (Verified identical under zero-fill and the C64 power-on RAM
        // pattern: $0cce lies inside the loaded image, so this is deterministic
        // composer-authored state, NOT uninitialised RAM.) The intro's three
        // voices only converge to a unison note at frame ~4 (they start on
        // different notes/instruments — voice2's first instrument even has
        // half-depth 2, not 6); by then the sidera regdump of the real replayer
        // shows the counters spread to [3,3,1] with directions rising/falling/
        // rising. We seed THAT converged state, because that is the phase the
        // audible unison begins on; our uniformly-advancing synth then preserves
        // the relative 3-phase spread (a chorus, not a flat unison). Reproducing
        // the literal [2,6,2] start instead would need replaying the frame-0..3
        // transitional notes, which the importer's static patch does not model.
        // Inert for tunes without interp vibrato (the counter is never read).
        //
        // ⚠️ All of that is THRUST's, and it does NOT carry to the last player,
        // whose counter lives at `$1003,X` and whose image ships it as 0 rising
        // on all three voices (`--stateaddrs 1003,1006,1009` reads 0 on every
        // frame before the first armed note). Seeding Thrust's converged phase
        // there put every voice on a phase that tune never plays — which is why
        // the position agreed with the replayer's RAM on only 5 % of frames
        // while the ADVANCE decision agreed on 96 %. See `note_on`.
        s.voices[0].interp_ctr = 3;
        s.voices[0].interp_up = true;
        s.voices[1].interp_ctr = 3;
        s.voices[1].interp_up = false;
        s.voices[2].interp_ctr = 1;
        s.voices[2].interp_up = true;
        s
    }

    #[inline]
    fn wr(&mut self, reg: u8, val: u8) {
        if (reg as usize) < self.reg.len() {
            self.reg[reg as usize] = val;
        }
        self.chip.write_register(reg, val);
    }

    #[inline]
    fn base(slot: usize) -> u8 {
        (slot * 7) as u8
    }

    #[inline]
    fn freq_reg(&self, milli_hz: u32) -> u16 {
        super::fx_engine::freq_reg(milli_hz, self.chip_clock_hz)
    }

    fn release_frames(&self, nibble: u8) -> u32 {
        (RELEASE_MS[(nibble & 0x0F) as usize] as u32 * self.output_rate) / 1000
    }

    fn write_freq(&mut self, slot: usize, milli_hz: u32) {
        let r = self.freq_reg(milli_hz);
        let b = Self::base(slot);
        self.wr(b + V1_FREQ_LO, (r & 0xFF) as u8);
        self.wr(b + V1_FREQ_HI, (r >> 8) as u8);
    }

    fn write_pw(&mut self, slot: usize, pw: u16) {
        let b = Self::base(slot);
        self.wr(b + V1_PULSE_LO, (pw & 0xFF) as u8);
        self.wr(b + V1_PULSE_HI, ((pw >> 8) & 0x0F) as u8);
    }

    /// Advance the shared VBlank counter. The PW sweep is NOT advanced here: it
    /// belongs to the per-frame effect pass (`advance_fx`), so it runs on held /
    /// release frames but is skipped on a voice's note-setup (trigger) frame —
    /// the replayer JMPs past the per-frame effects when fetching a new note, so
    /// the PW holds its swept value on the trigger frame instead of stepping
    /// (verified vs the commando regdump: stepping here put the sweep one frame
    /// ahead of the oracle on every re-trigger).
    pub fn begin_frame(&mut self) {
        self.frame_counter = self.frame_counter.wrapping_add(1);
        // Frames-since-note-on advances on EVERY frame, fetch frames included:
        // the replayer's increment lives in `lh_voice_loop_next` ($17cf,
        // `INC $1ace,X`), which every path reaches — including the note-fetch
        // `goto` that skips the whole effect chain. Incrementing it inside
        // `advance_fx` instead made it lag by one per fetch, and it is what
        // gates the vibrato's flat window and start delay, the per-note
        // arpeggio phase and the two-phase attack.
        for v in &mut self.voices {
            if v.active {
                v.note_frame = v.note_frame.saturating_add(1);
            }
        }
        // A SILENT voice keeps ping-ponging. The replayer's voice loop has no
        // "is this voice sounding" test anywhere: `$13e7` runs the whole effect
        // chain for all three voices every frame, so once a vibrato is armed its
        // position at `$1003,X` free-runs through the note's release and through
        // the silence after it — and the NEXT note therefore starts on whatever
        // phase that left. Our `advance_fx` stops at `!active`, which froze
        // Lion_Heart voice 2 on 535 frames the chip kept moving.
        //
        // Gated on `interp_flat` (last-player imports only) because the v15
        // corpus is frozen against a driver that never did this, and its own
        // ping-pong seed is a different, tune-baked value.
        // ⚠️ Freezing this on a pattern-entry fetch that lands on an
        // already-silent voice has now been TRIED AND REJECTED TWICE, on two
        // materially different engines (position agreement 5342 → 4996, then
        // 6245 → 6128). It reads as obviously right — the replayer's fetch path
        // does skip the chain whether or not the voice sounds — and it is not
        // the answer, because it cannot be: our timeline has no entry to fetch
        // on those frames. Voice 2 of Lion_Heart first slips at frame 87, where
        // our voice is silent AND we have no cell at all, so no guard on our
        // side can freeze anything there. See the note on `active` below.
        //
        // The real gap is that `active` is OURS, not the replayer's: it ends
        // when a release tail expires, whereas the replayer processes all three
        // voices forever and its rest entries keep arriving. This free-run
        // papers over that, and over-advances by the handful of fetches those
        // silences contain (voice 2: the chip's counter moves on 555 of the 612
        // frames we call silent, so we are wrong on 57 — and since nothing ever
        // resynchronises the ping-pong, those 57 cost the rest of the tune).
        // What IS established, by reading the replayer's RAM across our silent
        // frames: on voice 2 the chip is armed on 612 of 612 and its counter
        // moves on 555, so the free-run is right there. On voices 0 and 1 it is
        // armed on only 38 of 83 and 129 of 646 — which says nothing about us,
        // because the condition below is driven by the SAME byte the replayer
        // reads (`$1a5b,X` is record+5, or the `0x86` marker's operand, set at
        // note-on and left alone by ties and rests — exactly the lifetime of
        // `v.fx` here). On those frames neither side is armed.
        //
        // ⚠️ One difference in the arming RULE, on paper: the replayer arms on
        // the whole byte (`$147b BNE`), the import on the RANGE bits alone
        // (`(v5 & 0x78) != 0`). An instrument carrying only the shift bits would
        // ping-pong on the chip and not here. Checked across all six G2 tunes in
        // the corpus (`cargo run --example g2_probe`): every armed instrument
        // carries range bits, so nothing exercises it. Left as a note, not a
        // defect — and do not "confirm" it from a hand-rolled byte reader, the
        // PSID image starts at 126, and reading from 124 invents this exact
        // divergence on all six tunes at once.
        // Runs at the END of this method, AFTER the diagnostic snapshot, so that
        // a free-running voice is sampled at the same point in the frame as a
        // sounding one — before its advance, not after. Sampling it after put
        // the free-runners one step ahead of every other voice in the state
        // dump, i.e. the comparison itself was wrong for exactly the voices the
        // free-run was meant to fix.
        // Snapshot `$d400` per voice before anything in this frame touches it.
        for slot in 0..3 {
            self.voices[slot].freq_lo_prev = self.reg[(Self::base(slot) + V1_FREQ_LO) as usize];
        }
        // ⚠️ The replayer's voice loop DOES run all three voices every frame, so
        // it is tempting to free-run a silent voice's ping-pong here. It is
        // WRONG, and the clean proof is the exactness test rather than any
        // aggregate: with the counter as it stands, voice 2 of Lion_Heart has
        // **0 of 680** frames where the two counters agree and the frequency
        // does not. Free-running turns that into 35 of 706 — it makes counters
        // coincide without the sound following, i.e. by accident, and the
        // voice's wrong frames go 974→1280.
        #[cfg(feature = "std")]
        {
            let f = |v: &CoupledVoice| {
                (v.active as u8)
                    | ((v.gated as u8) << 1)
                    | ((v.fx.interp_vib as u8) << 2)
                    // bit 3: is an arpeggio armed at all? The replayer's whole
                    // arpeggio block (phase counter included) is skipped when
                    // `lh_arp_param` ($1a55,X) is zero, so this is the flag to
                    // compare against that byte — a phase that disagrees while
                    // the arming agrees is a counter bug, the other way round
                    // is an import bug.
                    | (((v.fx.arp_len != 0) as u8) << 3)
                    // bit 4: the effect chain ran on the frame just gone — so a
                    // sounding voice with this CLEAR was on a pattern-entry
                    // fetch, the replayer's one reason to skip the chain.
                    | ((v.fx_ran as u8) << 4)
            };
            super::driver::state_push([
                self.voices[0].interp_ctr,
                self.voices[1].interp_ctr,
                self.voices[2].interp_ctr,
                f(&self.voices[0]),
                f(&self.voices[1]),
                f(&self.voices[2]),
                self.voices[0].note_frame.min(255) as u8,
                self.voices[1].note_frame.min(255) as u8,
                self.voices[2].note_frame.min(255) as u8,
                self.voices[0].arp_phase.min(255) as u8,
                self.voices[1].arp_phase.min(255) as u8,
                self.voices[2].arp_phase.min(255) as u8,
                self.voices[0].prog.cursor,
                self.voices[1].prog.cursor,
                self.voices[2].prog.cursor,
                // Wave-program bend accumulator, 16-bit little-endian per voice
                // — the replayer's `$1a93,X` / `$1a96,X`.
                self.voices[0].prog.bend_acc as u16 as u8,
                (self.voices[0].prog.bend_acc as u16 >> 8) as u8,
                self.voices[1].prog.bend_acc as u16 as u8,
                (self.voices[1].prog.bend_acc as u16 >> 8) as u8,
                self.voices[2].prog.bend_acc as u16 as u8,
                (self.voices[2].prog.bend_acc as u16 >> 8) as u8,
            ]);
        }
        // Cleared AFTER the snapshot: the push reports the frame just gone.
        for v in &mut self.voices {
            v.fx_ran = false;
            if !v.active && v.fx.interp_flat != 0 && v.fx.interp_vib && v.fx.interp_half_depth > 0 {
                super::fx_engine::advance_pingpong(
                    &mut v.interp_ctr,
                    &mut v.interp_up,
                    v.fx.interp_half_depth,
                    true,
                );
            }
        }
    }

    fn advance_pw(&mut self, slot: usize) {
        let v = &mut self.voices[slot];
        if !v.active || !v.fx.pw_sweep || (v.fx.pw_delay == 0 && v.fx.pw_speed == 0) {
            return;
        }
        // Low-byte-only increment mode (v10 fx bit 3): add the full pw_speed to
        // the PW low byte each frame, 8-bit wrap, high byte untouched, no
        // delay/bounce. (Ghidra cmd_play `DAT_5523 & 8`.)
        if v.fx.pw_low_byte_inc {
            let step = (v.fx.pw_speed as u8) as i32;
            let lo = ((v.pw_cur & 0xFF) + step) & 0xFF;
            v.pw_cur = (v.pw_cur & 0xF00) | lo;
            let (pw, instr) = ((v.pw_cur & 0x0FFF) as u16, v.cur_instr);
            self.save_instr_pw(instr, self.voices[slot].pw_cur);
            self.write_pw(slot, pw);
            return;
        }
        v.pulse_delay_ctr -= 1;
        if v.pulse_delay_ctr >= 0 {
            return;
        }
        v.pulse_delay_ctr = v.fx.pw_delay as i32;
        let step = (v.fx.pw_speed as u8) as i32;
        // Per-instrument bounce bounds (fx_v2[i]+5 nibbles); when not explicitly
        // set, the legacy fixed 0x0E/0x08 (a bound of 0 is legitimate, so the
        // explicit flag is required to tell it from "unset").
        let (hi, lo) = if v.fx.pw_bounds_set {
            (v.fx.pw_hi_bound as i32, v.fx.pw_lo_bound as i32)
        } else {
            (0x0E, 0x08)
        };
        if v.pulse_dir == 0 {
            v.pw_cur += step;
            if ((v.pw_cur >> 8) & 0x0F) >= hi {
                v.pulse_dir = 1;
            }
        } else {
            v.pw_cur -= step;
            if ((v.pw_cur >> 8) & 0x0F) <= lo {
                v.pulse_dir = 0;
            }
        }
        let (pw, instr, pw_cur) = ((v.pw_cur & 0x0FFF) as u16, v.cur_instr, v.pw_cur);
        // Persist the swept value PER INSTRUMENT (the replayer mutates the
        // instrument table `DAT_5591[instr*8]` in place, so a re-trigger of this
        // instrument — even after a different instrument played on the voice —
        // resumes the sweep from here, not the base). Only the value is
        // per-instrument; the direction / delay counters stay per-voice.
        self.save_instr_pw(instr, pw_cur);
        self.write_pw(slot, pw);
    }

    /// Store the swept PW value for an instrument (no-op for out-of-range idx).
    fn save_instr_pw(&mut self, instr: usize, pw_cur: i32) {
        if let Some(slot) = self.instr_pw.get_mut(instr) {
            *slot = Some(pw_cur);
        }
    }

    /// Trigger a note for channel `ch` (→ voice `ch`). `instr` is the tracker
    /// instrument index, used to decide whether a filter note-on re-seeds the
    /// cutoff (instrument change) or continues the sweep (same instrument).
    /// A TIE that also changes the instrument.
    ///
    /// The replayer's tie branch is not "keep everything". It skips the note-on
    /// block, but two reads happen before the branch and one after, and from the
    /// next frame the per-frame path re-reads the record afresh — so a tie DOES
    /// change the instrument, for everything except what the note-on block owns.
    /// Ghidra `lh_play`:
    ///
    /// | what | where | on a tie |
    /// |---|---|---|
    /// | control shadow ← record +2 | `$12b5` then `$13b5` | **re-read, at once** |
    /// | flags (wavetable / filter / attack bits) ← record +7 | `$13f2`, every frame | **follows** |
    /// | PW sweep speed ← record +6, bounds ← record +13 | `$13f8`, `$1552` | **follows** |
    /// | wave-program pointer ← record +8/+9 | `$168b`, every frame | **follows** |
    /// | ADSR, PW seed, vibrato param + delay, filter slot, attack count | `$130a`-`$1389` | kept |
    /// | frames-since-note-on, wave cursor, arpeggio phase + param | `$138c`-`$13a1` | kept |
    ///
    /// So this swaps the record-derived half of `SidFx` and leaves the half that
    /// lives in per-voice RAM alone — swapping it wholesale would hand the tied
    /// note a vibrato and an arpeggio the replayer never reloads.
    pub fn retie(&mut self, ch: usize, instr: usize, patch: &SidVoicePatch, fx: &SidFx) {
        if ch >= 3 || !self.voices[ch].active {
            return;
        }
        let (slot, b) = (ch, Self::base(ch));
        let base_ctrl = ((patch.waveform & 0x0F) << 4)
            | (CTRL_SYNC * patch.sync as u8)
            | (CTRL_RING * patch.ring as u8)
            | (CTRL_TEST * patch.test as u8);
        {
            let v = &mut self.voices[slot];
            v.cur_instr = instr;
            v.gate_off_ctrl = base_ctrl;
            v.sustain_ctrl = base_ctrl | (if patch.gate { CTRL_GATE } else { 0 });
            // Record-derived, re-read every frame by the replayer's per-frame path.
            v.fx.program = fx.program;
            v.fx.drum = fx.drum;
            v.fx.skydive_climb = fx.skydive_climb;
            v.fx.skydive_add = fx.skydive_add;
            v.fx.skydive_when = fx.skydive_when;
            v.fx.filter = fx.filter;
            v.fx.pw_sweep = fx.pw_sweep;
            v.fx.pw_speed = fx.pw_speed;
            v.fx.pw_hi_bound = fx.pw_hi_bound;
            v.fx.pw_lo_bound = fx.pw_lo_bound;
            v.fx.pw_bounds_set = fx.pw_bounds_set;
        }
        // The control shadow reaches `$d404` on this very frame (`$13b5` →
        // `$13ca` → the write), through the gate mask like every other tie.
        let gated = self.voices[slot].gated;
        let ctrl = if gated {
            base_ctrl | CTRL_GATE
        } else {
            base_ctrl
        };
        self.wr(b + V1_CONTROL, ctrl);
    }

    pub fn note_on(
        &mut self,
        ch: usize,
        instr: usize,
        patch: &SidVoicePatch,
        fx: &SidFx,
        milli_hz: u32,
    ) {
        if ch >= 3 {
            return;
        }
        let slot = ch;
        let b = Self::base(slot);
        // ADSR + control (waveform/sync/ring/test), gate per-waveform.
        self.wr(b + V1_ATTACK_DECAY, patch.attack_decay);
        self.wr(b + V1_SUSTAIN_RELEASE, patch.sustain_release);
        let has_waveform = (patch.waveform & 0x0F) != 0;
        let base_ctrl = ((patch.waveform & 0x0F) << 4)
            | (CTRL_SYNC * patch.sync as u8)
            | (CTRL_RING * patch.ring as u8)
            | (CTRL_TEST * patch.test as u8);
        let gate_off_ctrl = base_ctrl & !CTRL_GATE;
        // The two-phase sustain restores the instrument's stored control byte AS
        // IS — including its own gate bit. Gate clear ⇒ pluck (gate drops after
        // the attack, e.g. lightforce v0 / delta v0); gate set ⇒ sustained tone
        // (lightforce v1). Most instruments store gate clear and rely on note-on.
        let sustain_ctrl = gate_off_ctrl | (if patch.gate { CTRL_GATE } else { 0 });
        // Two-phase attack (v20 effect 0x4 = waveform, fxmask bit6 = pitch):
        // start on the ATTACK control byte (its own waveform + sync/ring/test
        // bits, e.g. pulse+sync) and/or the absolute ATTACK NOTE for
        // `wave_attack_frames` frames, then `advance_fx` falls back to the
        // sustain waveform (`gate_off_ctrl | GATE`) and the struck note. The
        // trigger frame counts as the first attack frame, matching the replayer's
        // `f5fc` countdown. An instrument may set the waveform attack, the pitch
        // attack, or both (the `0x44` lead) — they share the `wave_attack_ctr`.
        let wave_attack = fx.wave_attack_ctrl != 0;
        let note_attack = fx.wave_attack_note_set;
        let two_phase = has_waveform && fx.wave_attack_frames != 0 && (wave_attack || note_attack);
        let wave_attack_reg = if two_phase && note_attack {
            self.freq_reg(super::note_milli_hz(fx.wave_attack_note))
        } else {
            0
        };
        let ctrl = if !has_waveform {
            gate_off_ctrl
        } else {
            // The TRIGGER frame carries the instrument's own waveform, never the
            // attack byte: the last player's note-setup writes `lh_ctrl_frame =
            // record+2` (the sustain ctrl) and then `goto lh_write_voice_regs`,
            // jumping over the whole effect chain — `lh_instrflag_fx`, which is
            // what substitutes the attack, only runs from the NEXT frame
            // (Ghidra `lh_play_voice_loop $10b7` / `lh_instrflag_fx $170e`).
            base_ctrl | CTRL_GATE
        };
        self.wr(b + V1_CONTROL, ctrl);
        // Seed PW (persist sweep across notes on this voice).
        let prev = self.voices[slot];
        // Filter: re-seed the cutoff only when the instrument changes (≈ a new
        // phrase in the replayer); a same-instrument note-on continues the sweep
        // (legato). Either way one `step` applies this frame (the play routine
        // runs on the trigger frame; `advance_fx` is skipped then).
        let filter_continues =
            fx.filter && prev.pw_init && prev.cur_instr == instr && !fx.filter_reseed_each_note;
        let seed_hi = (fx.filter_seed >> 8) as u8;
        let filter_cutoff_hi = if fx.filter_reseed_each_note {
            // v30: note-setup zeroes the accumulator ($beaf) AND the note-fetch
            // frame JMPs past the play effects ($bf9a → $c204), so the cutoff
            // sweep adds NO step on the trigger frame — it starts at the seed
            // and `advance_fx` ramps it from the next frame (Ghidra $c1a8).
            seed_hi
        } else if filter_continues {
            prev.filter_cutoff_hi.wrapping_add(fx.filter_step as u8)
        } else {
            seed_hi.wrapping_add(fx.filter_step as u8)
        };
        // BOUNCE law: the last player reloads the whole slot on note-on unless
        // the note carries the filter-hold marker (which the importer models by
        // leaving the instrument's filter alone), so the accumulator restarts at
        // the slot's seed and the direction at the slot's bit 7.
        let (filter_cutoff16, filter_up) = if fx.filter_bounce && !filter_continues {
            (fx.filter_seed, fx.filter_bounce_up)
        } else {
            (prev.filter_cutoff16, prev.filter_up)
        };
        let v = CoupledVoice {
            active: true,
            gated: has_waveform,
            release_nibble: patch.sustain_release & 0x0F,
            release_tail: 0,
            base_milli_hz: milli_hz,
            note_frame: 0,
            freq_lo_prev: prev.freq_lo_prev,
            fx_ran: false,
            drum_hi: (self.freq_reg(milli_hz) >> 8) as u8,
            gate_off_ctrl,
            skydive_semis: 0,
            fx: *fx,
            // PW seed. Two replayer behaviours (see `SidFx::pw_reseed_on_note`):
            //
            // v20/v25/v30 (`pw_reseed_on_note`): the sweep lives in per-voice RAM
            // and the note-setup reloads the instrument's base PW (`$f618,X`) on
            // every note-on, so a re-struck note RESTARTS the sweep from base with
            // the direction reset to climbing (Ghidra lightforce play $f4xx).
            //
            // v10 (commando/zoids): the replayer keeps the swept value in the
            // INSTRUMENT table (`DAT_5591[instr*8]`, mutated in place) and the
            // note-setup reloads d402/d403 from there, so a re-struck note RESUMES
            // this instrument's sweep — even if a different instrument played on
            // the voice in between. Seed from the patch base on first play.
            pw_cur: if fx.pw_reseed_on_note {
                patch.pulse_width as i32
            } else {
                self.instr_pw
                    .get(instr)
                    .copied()
                    .flatten()
                    .unwrap_or(patch.pulse_width as i32)
            },
            pulse_dir: if fx.pw_reseed_on_note || !prev.pw_init {
                0
            } else {
                prev.pulse_dir
            },
            pulse_delay_ctr: if fx.pw_reseed_on_note || !prev.pw_init {
                fx.pw_delay as i32
            } else {
                prev.pulse_delay_ctr
            },
            pw_init: true,
            // See `filter_cutoff_hi` computation above (re-seed vs continue).
            filter_cutoff_hi,
            filter_cutoff16,
            filter_up,
            prog: Default::default(),
            release_ctr: if fx.release_ramp { fx.release_after } else { 0 },
            cur_instr: instr,
            // The interp-vibrato counter FREE-RUNS per voice — the replayer
            // never resets `$cce` on note-on (init only clears $cae/$cb1/$cb4,
            // note-fetch never touches it), so carry it across notes. Its
            // de-phased seed (set in `new`) is what spreads the 3 unison-note
            // intro voices into the oracle's chorus instead of a flat unison.
            // ⚠️ The last player's work-RAM starts all three counters at 0,
            // climbing (`--statedump` frames 0..13), so its FIRST note should
            // arguably start from 0 rather than from the seed. Tried: the
            // advance-rule agreement improves but the register divergence gets
            // WORSE (freq 3560→3827), same as free-running silent voices
            // (3560→3863). The two metrics pull opposite ways here, which says
            // something else is compensating — do not ship either alone.
            arp_phase: 0,
            // The ping-pong position free-runs across notes: the replayer's
            // note-on block (`$1389`-`$13a3`) clears the frame counter, the wave
            // cursor and the arpeggio phase, but never `$1003,X`.
            //
            // Except on the very first last-player note of a voice. Until the
            // vibrato is armed the replayer never touches that byte, so it still
            // holds what the tune's image shipped — 0, rising, for the last
            // player — and the constructor's Thrust seed has to give way to it.
            //
            // ARBITRATED against Sun_Never_Shines' own work RAM, not just
            // Lion_Heart's: with this seed, the `>` delay guard and the silent
            // free-run all disabled, its position agreement over sounding frames
            // is 1208 of 8508; with all three, 2147 — and voice 2 goes from 21
            // wrong frequencies out of 506 agreeing frames to **0 out of 1684**.
            // Its `freq` counter moved the other way (1818 → 1967), and that
            // counter was wrong to believe.
            interp_ctr: if fx.interp_flat != 0 && !prev.interp_seeded {
                0
            } else {
                prev.interp_ctr
            },
            interp_up: fx.interp_flat != 0 && !prev.interp_seeded || prev.interp_up,
            interp_seeded: prev.interp_seeded || fx.interp_flat != 0,
            // Reloaded at note-on (`$1303`). A tie skips the note-on block
            // entirely, so it keeps counting — but a tie does not reach here.
            vib_delay: fx.interp_delay,
            wave_attack_ctrl: if two_phase { fx.wave_attack_ctrl } else { 0 },
            wave_attack_ctr: if two_phase { fx.wave_attack_frames } else { 0 },
            wave_attack_done: false,
            wave_attack_reg,
            sustain_ctrl,
        };
        // Program the (possibly swept) pulse width.
        let pw = (v.pw_cur & 0x0FFF) as u16;
        self.voices[slot] = v;
        self.write_pw(slot, pw);
        // Pitch attack (bit6) does NOT sound on the trigger frame either — same
        // `goto` as the waveform attack above. The struck note plays for one
        // frame, then `advance_fx` substitutes the absolute attack note for the
        // window's length.
        self.write_freq(slot, milli_hz);
        // SID filter: enable LP master mode once, write this voice's seeded
        // cutoff + resonance/routing. Only filter instruments touch these regs.
        if fx.filter {
            // Mode nibble: low-pass (1) for every v10–v30 tune, so this is the
            // historical hardwired 0x1F; the last player picks LP/BP/HP per slot.
            self.wr(MODE_VOL, ((fx.filter_mode & 0x0F) << 4) | 0x0F);
            if fx.filter_bounce {
                let c = v.filter_cutoff16;
                self.wr(FC_LO, ((c as u8) >> 5) & 0x0F);
                self.wr(FC_HI, (c >> 8) as u8);
            } else {
                self.wr(FC_LO, 0);
                // v30 skips the trigger-frame cutoff write (note-fetch JMPs past the
                // filter sweep), so the $D416 register holds the previous note's last
                // value for one frame; `advance_fx` writes the swept cutoff from the
                // next frame. Other replayers write the seed on the trigger frame.
                if !fx.filter_reseed_each_note {
                    self.wr(FC_HI, v.filter_cutoff_hi);
                }
            }
            self.wr(RES_FILT, fx.filter_resfilt);
        }
    }

    /// Update voice `ch`'s base frequency (milli-Hz) for a held note.
    pub fn set_frequency(&mut self, ch: usize, milli_hz: u32) {
        if ch >= 3 || !self.voices[ch].active {
            return;
        }
        self.voices[ch].base_milli_hz = milli_hz;
        self.write_freq(ch, milli_hz);
    }

    /// `is_fetch`: see [`super::SidSynth::note_off`]. An appended-note fetch
    /// re-fetches the instrument's AD/SR (never zeroes them); only the ordinary
    /// release path runs the version's "kill adsr".
    pub fn note_off(&mut self, ch: usize, is_fetch: bool) {
        if ch >= 3 || !self.voices[ch].active {
            return;
        }
        let slot = ch;
        let b = Self::base(slot);
        // Gate-off restores the instrument's STORED waveform without the gate
        // (replayer `(&DAT_54f8)[v] & 0xfe`), NOT the current control register:
        // the drum effect leaves 0x80 (pure noise) in the register, and masking
        // that with `& ~GATE` would freeze the noise on instead of releasing the
        // note's own waveform. For a note with no effect overriding ctrl the two
        // are identical (reg == base_ctrl|GATE ⇒ reg & ~GATE == gate_off_ctrl).
        let cur = self.voices[slot].gate_off_ctrl;
        self.wr(b + V1_CONTROL, cur);
        self.voices[slot].gated = false;
        if !is_fetch && self.voices[slot].fx.zero_adsr_on_note_off {
            // v15 `$e198`: zero AD+SR at note-off → the note cuts sharp (SR=0 ⇒
            // instant release, no ring-out). The voice stays ACTIVE: the replayer
            // keeps processing it (the note-increment climb keeps writing the freq
            // register, silently), so we keep the normal release tail for clocking.
            self.wr(b + V1_ATTACK_DECAY, 0);
            self.wr(b + V1_SUSTAIN_RELEASE, 0);
        }
        let nibble = self.voices[slot].release_nibble;
        self.voices[slot].release_tail = self.release_frames(nibble);
    }

    /// Hard-cut voice `ch` (note-column `^^^`): silence it now, no release.
    pub fn note_cut(&mut self, ch: usize) {
        if ch >= 3 {
            return;
        }
        let slot = ch;
        let b = Self::base(slot);
        let cur = self.reg[(b + V1_CONTROL) as usize] & !CTRL_GATE;
        self.wr(b + V1_CONTROL, cur);
        self.voices[slot] = CoupledVoice::default();
    }

    /// One frame of per-note modulation for voice `ch` (vibrato → drum →
    /// skydive → arp), mirroring [`super::SidSynth::advance_fx`] per voice.
    pub fn advance_fx(&mut self, ch: usize) {
        if ch >= 3 {
            return;
        }
        let slot = ch;
        // Being CALLED at all is the signal: the caller skips this pass on a
        // pattern-entry fetch, exactly as the replayer jumps over it.
        self.voices[slot].fx_ran = true;
        // PW sweep is part of the per-frame effect pass (see `begin_frame`): it
        // runs on held / release frames, never on the trigger frame.
        self.advance_pw(slot);
        let frame = self.frame_counter;
        let speed = self.speed;
        let fx = self.voices[slot].fx;
        let (base, gated, note_frame, drum_hi0, gate_off_ctrl, skydive_semis) = {
            let v = &mut self.voices[slot];
            if !v.active {
                return;
            }

            // The note-increment climb (Ghidra v15 bit1 / Thrust) runs in the
            // per-frame effects with NO gate check — it keeps climbing through the
            // note's RELEASE, so the lead rises *while fading* (the intro "whoosh").
            // Gating it on `gated` (gate-on) froze the climb at the release point
            // (Spellbound's intro lead capped at ~1 octave vs the oracle's ~5). Use
            // `active` (sounding: gated OR in release tail) instead.
            if fx.skydive_climb && v.active && frame & 3 == 0 {
                v.skydive_semis = v.skydive_semis.saturating_add(1);
            }
            (
                v.base_milli_hz,
                v.gated,
                v.note_frame,
                v.drum_hi,
                v.gate_off_ctrl,
                v.skydive_semis,
            )
        };
        // Two-phase attack (v20 effect 0x4 = waveform, fxmask bit6 = pitch):
        // count down the SHARED attack counter and, when it runs out, switch the
        // control register from the attack byte to the sustain waveform. The
        // replayer decrements the counter once per ACTIVE two-phase bit each
        // frame, so an instrument with BOTH (the `0x44` lead) burns it twice →
        // the unified attack lasts `frames / 2` (Ghidra sanxion `$be20`). The
        // waveform attack owns the control register here; the pitch attack
        // (`attack_active`) overrides the freq AFTER `modulate` below, so it wins
        // over the per-frame pitch effects during the attack window. (v20 sets no
        // drum/skydive, so this never races the `ctrl_override` path below.)
        let (attack_active, restore_sustain, attack_ctrl) = {
            let v = &mut self.voices[slot];
            let mut restore = None;
            // The attack WAVEFORM, which this driver decremented a counter for
            // and then never wrote. Ghidra `$170e`: while the attack counter is
            // non-zero the control shadow is loaded from record +9 — the attack
            // waveform — and only otherwise from record +2. So a two-phase
            // instrument opens on that waveform for the whole window.
            //
            // Missing it was audible, and on the tunes that use it, obvious: the
            // chip opens each note with two frames of NOISE (`$81`) where we
            // held the sustain pulse (`$41`) — 616 frames on Sun_Never_Shines
            // voice 0, 729 on Pacific_Coast voice 0, 463 on Lion_Heart voice 0,
            // 792 on Sun_Never_Shines voice 2. Every one of those is a
            // percussive attack transient that simply was not there.
            //
            // Not applied on the trigger frame: the replayer's note-setup jumps
            // straight to the register write, and this block belongs to the
            // per-frame path it skips. Nor when a voice program runs — that
            // path (`$1685`) branches around this block entirely and the
            // program owns the waveform, which is why `ctrl_override` below
            // takes precedence.
            let attack_ctrl =
                (v.wave_attack_ctr > 0 && v.wave_attack_ctrl != 0).then_some(if v.gated {
                    v.wave_attack_ctrl
                } else {
                    v.wave_attack_ctrl & !CTRL_GATE
                });
            // Ghidra tests the counter BEFORE decrementing (`if ctr == 0 {
            // sustain } else { ctr -= 1; attack }`), so a seed of N covers N
            // frames starting at the frame after the trigger. Reading it after
            // the decrement lost the window's last frame.
            let pre = v.wave_attack_ctr;
            if pre > 0 {
                let dec = (v.wave_attack_ctrl != 0) as u8 + (v.wave_attack_reg != 0) as u8;
                v.wave_attack_ctr = pre.saturating_sub(dec.max(1));
            } else if !v.wave_attack_done {
                v.wave_attack_done = true;
                if v.wave_attack_ctrl != 0 {
                    // Restore the instrument's stored sustain control byte (with
                    // its own gate bit) — a pluck (gate clear) or a sustained
                    // tone (gate set), per the instrument. The replayer ANDs the
                    // final control with a gate mask that drops the gate once the
                    // note's length has expired (`d404 = wave & gate_mask`), so a
                    // released voice keeps the waveform but loses the gate.
                    restore = Some(if v.gated {
                        v.sustain_ctrl
                    } else {
                        v.sustain_ctrl & !CTRL_GATE
                    });
                }
            }
            // Was this frame inside the window? (tested pre-decrement)
            (pre > 0 && v.wave_attack_reg != 0, restore, attack_ctrl)
        };
        if let Some(sustain) = restore_sustain {
            self.wr(Self::base(slot) + V1_CONTROL, sustain);
        }
        if let Some(c) = attack_ctrl {
            self.wr(Self::base(slot) + V1_CONTROL, c);
        }
        if base == 0 {
            return;
        }
        // Shared per-frame modulation (vibrato → drum → skydive → interp → arp →
        // arp2 → alt-arp → wave-alt), centralised in `fx_engine` and called
        // identically by the per-instrument driver. This driver adds only the
        // two-phase attack (above) and the global filter (below). `note_frame`
        // and the skydive counter were already advanced in the extract block.
        let mut st = super::fx_engine::FxState {
            base_milli_hz: base,
            gated,
            note_frame,
            arp_phase: self.voices[slot].arp_phase,
            drum_hi: drum_hi0,
            gate_off_ctrl,
            skydive_semis,
            interp_ctr: self.voices[slot].interp_ctr,
            interp_up: self.voices[slot].interp_up,
            vib_delay: self.voices[slot].vib_delay,
            prog: self.voices[slot].prog,
        };
        let m = super::fx_engine::modulate(&mut st, &fx, frame, speed, self.chip_clock_hz);
        let (reg, ctrl_override, freq_hi_only) = (m.reg, m.ctrl, m.freq_hi_only);
        self.voices[slot].drum_hi = st.drum_hi;
        self.voices[slot].prog = st.prog;
        // Step the arpeggio phase AFTER the frame has used it, and only while
        // the arpeggio is actually armed — the replayer's `$1403` reads
        // `lh_arp_param` first and skips the whole block (phase included) when
        // it is zero, which is why a non-arpeggiating voice sits at phase 0 for
        // the whole tune. It also WRAPS on the spot: at 2 when the parameter's
        // low nibble is zero (a two-step shimmer), at 3 otherwise.
        if fx.arp_len != 0 {
            let v = &mut self.voices[slot];
            v.arp_phase += 1;
            if v.arp_phase >= fx.arp_len.min(3) as u32 {
                v.arp_phase = 0;
            }
        } else {
            self.voices[slot].arp_phase = 0;
        }
        self.voices[slot].interp_ctr = st.interp_ctr;
        self.voices[slot].interp_up = st.interp_up;
        self.voices[slot].vib_delay = st.vib_delay;

        // Pitch attack (bit6) owns the freq during the attack window — it wins
        // over the per-frame pitch effects in `modulate`. After the window it
        // yields to the struck note (and its vibrato, per the replayer).
        let reg = if attack_active {
            self.voices[slot].wave_attack_reg
        } else {
            reg
        };
        let b = Self::base(slot);
        // A `Fixed` program step writes the high byte alone; every other frame
        // writes both (replayer `$16cb` against `$17c6`/`$17cc`). Restoring the
        // start-of-frame low byte, rather than merely skipping the write, is
        // what actually reproduces it here: the tracker has already refreshed
        // the played note into `$d400` before this pass runs, whereas the
        // replayer keeps the note in work RAM and writes the chip once, at the
        // end of the frame — a write this step jumps over.
        let lo = if freq_hi_only {
            self.voices[slot].freq_lo_prev
        } else {
            (reg & 0xFF) as u8
        };
        self.wr(b + V1_FREQ_LO, lo);
        self.wr(b + V1_FREQ_HI, (reg >> 8) as u8);
        if let Some(c) = ctrl_override {
            // Everything the replayer writes through `$17b7` passes the gate
            // mask (`$17bd`), so a program step — or the settled tail after the
            // program's terminator — must not re-open the gate of a note whose
            // last row has gone by. Only a `Fixed` step writes `$d404` raw.
            let c = if m.ctrl_masked && !gated {
                c & !CTRL_GATE
            } else {
                c
            };
            self.wr(b + V1_CONTROL, c);
        }

        // Release ramp: a few frames before the note ends — while it is still
        // GATED — the replayer swaps the instrument's envelope for a fixed pair,
        // so every note finishes alike. Writing it at gate-off instead is a
        // different effect entirely (measured: ~3x the divergence).
        if self.voices[slot].release_ctr > 0 {
            self.voices[slot].release_ctr -= 1;
            if self.voices[slot].release_ctr == 0 {
                let (ad, sr) = (fx.release_ad, fx.release_sr);
                self.wr(b + V1_ATTACK_DECAY, ad);
                self.wr(b + V1_SUSTAIN_RELEASE, sr);
            }
        }

        // SID filter sweep (replayer `f487`/`e41b` family): if this instrument
        // drives the filter, accumulate the per-frame cutoff step (u8 wrap) and
        // write the global cutoff-hi + resonance/routing. `$D415` stays 0.
        // Last filter-enabled voice in the frame wins the single global filter,
        // matching the replayer's per-voice loop.
        if fx.filter {
            if fx.filter_bounce {
                // BOUNCE law (last player): a 16-bit accumulator reversing at
                // the slot's bounds, compared on the HIGH byte — the same shape
                // as the pulse-width sweep. On reaching a bound the replayer
                // snaps the accumulator to `bound << 8` (low byte zeroed) and
                // flips the direction. A zero step freezes the cutoff.
                let v = &mut self.voices[slot];
                if fx.filter_bounce_step != 0 {
                    if v.filter_up {
                        let (n, carry) = v.filter_cutoff16.overflowing_add(fx.filter_bounce_step);
                        if carry || (n >> 8) as u8 >= fx.filter_bounce_max {
                            v.filter_cutoff16 = (fx.filter_bounce_max as u16) << 8;
                            v.filter_up = false;
                        } else {
                            v.filter_cutoff16 = n;
                        }
                    } else {
                        let (n, borrow) = v.filter_cutoff16.overflowing_sub(fx.filter_bounce_step);
                        if borrow || (n >> 8) as u8 <= fx.filter_bounce_min {
                            v.filter_cutoff16 = (fx.filter_bounce_min as u16) << 8;
                            v.filter_up = true;
                        } else {
                            v.filter_cutoff16 = n;
                        }
                    }
                }
                let c = self.voices[slot].filter_cutoff16;
                self.wr(FC_LO, ((c as u8) >> 5) & 0x0F);
                self.wr(FC_HI, (c >> 8) as u8);
            } else {
                let cut = self.voices[slot]
                    .filter_cutoff_hi
                    .wrapping_add(fx.filter_step as u8);
                self.voices[slot].filter_cutoff_hi = cut;
                self.wr(FC_LO, 0);
                self.wr(FC_HI, cut);
            }
            self.wr(RES_FILT, fx.filter_resfilt);
        }
    }

    /// Advance the chip one output frame; returns `(mix, [v0, v1, v2])`.
    /// Clock one frame; returns the mono mix and the three per-voice amplitudes.
    pub fn clock(&mut self) -> (i16, [i32; 3]) {
        let out = self.chip.clock_with_voices();
        for v in &mut self.voices {
            if v.active && !v.gated && v.release_tail > 0 {
                v.release_tail -= 1;
                if v.release_tail == 0 {
                    v.active = false;
                }
            }
        }
        out
    }

    /// True while any voice is still gated or ringing out a release tail.
    pub fn any_active(&self) -> bool {
        self.voices
            .iter()
            .any(|v| v.active && (v.gated || v.release_tail > 0))
    }

    /// Clock one frame and return the mono mix (i16 range as i32), matching
    /// [`super::driver::SidBank::mix_frame`]'s contract for the master fold.
    pub fn mix_frame(&mut self) -> i32 {
        let (mix, _) = self.clock();
        mix as i32
    }

    /// Diagnostic register snapshot ($D400-$D418): channel `i` → SID voice `i`.
    /// Copies the full writable register window ($D400-$D418) from the shadow
    /// array so the filter regs ($D415-$D418) appear in the `sidplay --regdump`
    /// frame diff.
    #[cfg(feature = "std")]
    #[doc(hidden)]
    pub fn snapshot(&self) -> [u8; 0x19] {
        let mut regs = [0u8; 0x19];
        regs.copy_from_slice(&self.reg[..0x19]);
        regs
    }
}
