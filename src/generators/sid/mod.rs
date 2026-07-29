//! Cycle-accurate SID (MOS 6581/8580) synthesis core — the runtime
//! counterpart of [`crate::core::instr_sid::InstrSid`], mirroring the OPL
//! generator's role.
//!
//! ## Model: one chip per instrument
//!
//! Each SID *instrument* is an autonomous single-voice patch (verified at
//! import: the historical `[SidVoice; 3]` / `[RobEffects; 3]` arrays only ever
//! filled slot 0). So the natural mapping is **one [`sidera::Sid`] per
//! instrument**, with the chip's three hardware voices serving as a
//! *polyphony pool* for that instrument (allocated per sounding note, exactly
//! like the OPL driver's slots). The player (`Voices`) owns a lazy
//! per-instrument map of [`SidSynth`](crate::generators::sid::SidSynth), so a module with no SID instrument
//! instantiates none and stays bit-identical.
//!
//! ## Output: mix vs. per-voice
//!
//! [`SidSynth::clock`](crate::generators::sid::SidSynth::clock) returns `(mix, [v0, v1, v2])` from
//! [`sidera::Sid::clock_with_voices`]:
//! - `mix` (i16) is the faithful chip output — filter, mixer DC and master
//!   volume applied, DC removed by the on-chip external RC. **Phase A sums
//!   each instrument-chip's `mix` at the master** (in `i32`, clamped once);
//!   because the filter is off and the RC/volume stages are linear,
//!   sum-of-chips is equivalent to the original single-chip mix (plan §4).
//! - `[v0, v1, v2]` are the raw *pre-filter* voice outputs, the routing hook
//!   for sending individual voices to separate DAW tracks (Phase B). They
//!   carry the 6581 voice DC and must be high-passed host-side before use —
//!   not summed to reconstruct the mix.
//!
//! ## Volume
//!
//! The SID has no per-voice volume register, so the chip runs wide open
//! (master volume 15) and per-note / per-track gain is applied host-side when
//! the chip's output is folded into the mix.

pub mod coupled;
pub mod driver;
pub(crate) mod fx_engine;
pub mod wave_program;

use sidera::registers::{
    CTRL_GATE, CTRL_RING, CTRL_SYNC, CTRL_TEST, MODE_VOL, V1_ATTACK_DECAY, V1_CONTROL, V1_FREQ_HI,
    V1_FREQ_LO, V1_PULSE_HI, V1_PULSE_LO, V1_SUSTAIN_RELEASE,
};
use sidera::{ChipModel, Sid};

/// SID chip revision. The 6581 has analogue DC offsets and a strongly
/// non-linear filter; the 8580 is cleaner and symmetric.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidModel {
    /// MOS 6581 (original C64). Default for PSID/RSID without a model hint.
    Mos6581,
    /// MOS 8580 (C64C / later).
    Mos8580,
}

/// SID master-clock region: PAL (most European C64s) or NTSC (North America).
/// Sets both the decimation ratio and the note→frequency-register scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidRegion {
    /// PAL: 985 248 Hz.
    Pal,
    /// NTSC: 1 022 727 Hz.
    Ntsc,
}

impl SidRegion {
    #[inline]
    fn clock_hz(self) -> u32 {
        match self {
            SidRegion::Pal => sidera::PAL_CLOCK,
            SidRegion::Ntsc => sidera::NTSC_CLOCK,
        }
    }
}

/// One SID voice's register-level patch — the generator's neutral input,
/// decoupled from the tracker instrument type (the SID counterpart of OPL's
/// `OpPatch`). The importer bridges `InstrSid` → `SidVoicePatch`.
#[derive(Clone, Copy, Default)]
pub struct SidVoicePatch {
    /// Waveform-select nibble: bit0 = triangle, bit1 = saw, bit2 = pulse,
    /// bit3 = noise (combined waveforms allowed, exactly the CONTROL bits 4-7).
    pub waveform: u8,
    /// 12-bit pulse width (`0..=4095`).
    pub pulse_width: u16,
    /// ATTACK_DECAY register byte (`attack << 4 | decay`).
    pub attack_decay: u8,
    /// SUSTAIN_RELEASE register byte (`sustain << 4 | release`).
    pub sustain_release: u8,
    /// Ring-modulate this voice with its hardware neighbour.
    pub ring: bool,
    /// Hard-sync this voice to its hardware neighbour.
    pub sync: bool,
    /// TEST bit (freezes the oscillator at 0; used by some drum/SFX patches).
    pub test: bool,
    /// The instrument's stored GATE bit (CONTROL bit 0). Most instruments leave
    /// it clear and rely on the note-on to gate; but the two-phase-attack
    /// instruments (v20/v25/v30) read their stored control byte AS-IS for the
    /// sustain phase, so an instrument defined with the gate clear is a PLUCK
    /// (gate drops after the attack), not a sustained tone — see the two-phase
    /// handling in `CoupledSid::advance_fx`.
    pub gate: bool,
}

/// Documented SID decay/release times per nibble (ms) — the same hardware
/// timings sidera's high-level API quantises to. Used only to bound the
/// release tail for voice activity/allocation; the audio itself comes from
/// the cycle-accurate chip, so a slightly long bound is merely a few wasted
/// cycles, never an audible artefact. (Release-from-sustain is shorter than a
/// full-scale decay, so this over-estimates → it never cuts a tail early.)
// Release-tail lengths in ms per SR-nibble. Shared with `coupled.rs`.
pub(super) const RELEASE_MS: [u16; 16] = [
    6, 24, 48, 72, 114, 168, 204, 240, 300, 750, 1500, 2400, 3000, 9000, 15000, 24000,
];

/// One equal-tempered semitone as a fraction (`2^(1/12) − 1 ≈ 0.059463`),
/// used to make the Rob-Hubbard vibrato/arpeggio proportional to pitch exactly
/// like the replayer's `RH_FREQ_HEX[note+1] − RH_FREQ_HEX[note]` semitone step.
pub(crate) const SEMI_NUM: i64 = 119;
pub(crate) const SEMI_DEN: i64 = 2000;

/// One semitone up as a Q16 multiplier (`round(2^(1/12) · 65536)`), used by
/// [`shift_semitones`] to climb a note-increment skydive (Thrust: the replayer
/// does `notenum++` every 4th frame and re-reads the chromatic freq table).
/// Integer / reproducible.
const SEMI_UP_Q16: u64 = 69433; // 2^(1/12) · 65536

/// Transpose `base` (milli-Hz) up by `semis` equal-tempered semitones (Q16
/// repeated multiply). `semis == 0` returns `base`.
pub(crate) fn shift_semitones_up(base: u32, semis: u8) -> u32 {
    let mut r = base as u64;
    for _ in 0..semis {
        r = (r * SEMI_UP_Q16) >> 16;
        if r > u32::MAX as u64 {
            r = u32::MAX as u64;
        }
    }
    r as u32
}

/// One semitone down as a Q16 multiplier (`round(2^(-1/12) · 65536)`).
const SEMI_DOWN_Q16: u64 = 61858;

/// Transpose `base` (milli-Hz) down by `semis` equal-tempered semitones.
pub(crate) fn shift_semitones_down(base: u32, semis: u8) -> u32 {
    let mut r = base as u64;
    for _ in 0..semis {
        r = (r * SEMI_DOWN_Q16) >> 16;
    }
    r as u32
}

/// Middle-C (note index 48, "C-4") in milli-Hertz — the player's musical pitch
/// reference (`sid_current_milli_hz` rescales every played note to this scale).
pub(crate) const C4_MILLI_HZ: u32 = 261_625;
/// Note index of [`C4_MILLI_HZ`] on the SID importer's chromatic scale
/// (`Pitch::try_from(note)`, so the index IS the semitone number).
pub(crate) const C4_NOTE: u8 = 48;

/// Absolute SID note index (0..=95) → milli-Hertz on the same equal-tempered
/// scale the played notes use, so a baked attack note ([`SidFx::wave_attack_note`])
/// lands on exactly the played-note grid without needing the replayer's per-tune
/// `freq_table` at runtime.
pub(crate) fn note_milli_hz(note: u8) -> u32 {
    if note >= C4_NOTE {
        shift_semitones_up(C4_MILLI_HZ, note - C4_NOTE)
    } else {
        shift_semitones_down(C4_MILLI_HZ, C4_NOTE - note)
    }
}

/// Frames a v30 note must be held (gated) before length-gated vibrato engages —
/// the no-core approximation of the replayer's `(note length) >= 3 rows` gate
/// (Ghidra `dovib` $c04c). Sits between a 1-row staccato (released first, never
/// vibrates) and a 3-row sustained note. See [`SidFx::vib_len_gate`] /
/// `advance_fx`.
pub(crate) const VIB_GATE_FRAMES: u32 = 4;

/// Per-frame Rob-Hubbard modulations applied to a held voice — the neutral
/// generator input (the importer bridges `RobEffects` → `SidFx`).
#[derive(Clone, Copy, Default)]
pub struct SidFx {
    /// Pulse-width sweep: add `pw_speed` to the 12-bit pulse width each frame
    /// (after `pw_delay` frames), bouncing at the 0/4095 bounds — Hubbard's
    /// signature moving-pulse timbre.
    pub pw_sweep: bool,
    pub pw_speed: i8,
    pub pw_delay: u16,
    /// PW low-byte-only increment mode (v10 `instrfx & 8`): add `pw_speed` (the
    /// full byte) to the pulse-width LOW byte each frame, 8-bit wrap, high byte
    /// untouched, no delay/bounce. See [`crate::core::instr_robsid::PulseSweep::low_byte_mode`].
    pub pw_low_byte_inc: bool,
    /// Pulse-width bounce bounds on the PW **high byte**: the sweep reverses
    /// direction when `pw_cur >> 8` reaches `pw_hi_bound` (climbing) or
    /// `pw_lo_bound` (falling). v30 (Delta) carries them per instrument in
    /// `fx_v2[i]+5` (hi nibble = upper, lo nibble = lower); older players used a
    /// fixed `0x0E`/`0x08`, which stays the bridge fallback when these are 0.
    pub pw_hi_bound: u8,
    pub pw_lo_bound: u8,
    /// The bounce bounds are explicit (`pw_hi_bound`/`pw_lo_bound` are the real
    /// values, including 0) rather than the legacy fixed 0x0E/0x08. The flat
    /// engine carries presence here; the musical model expresses it as
    /// [`crate::core::instr_robsid::PulseSweep::bounce`]
    /// (`Some` ⇒ `true`, `None` ⇒ `false`).
    pub pw_bounds_set: bool,
    /// Restart the sweep from the patch base PW on every note-on (v20/v25/v30)
    /// instead of free-running across notes (v10). See
    /// [`crate::core::instr_robsid::PulseSweep::reseed_on_note`]. Only the
    /// coupled driver honours this; the per-instrument [`driver::SidBank`] always
    /// reseeds.
    pub pw_reseed_on_note: bool,
    /// v30 only: the replayer gates vibrato on the note's length field
    /// (`(flags & 0x1f) >= 3`, Ghidra `dovib` $c04c) — staccato notes get NO
    /// vibrato. We can't see the length field in the driver, so we approximate:
    /// suppress vibrato until the note has been held a few frames (short notes
    /// release first). See `advance_fx`.
    pub vib_len_gate: bool,
    /// Vibrato: an upward proportional triangle pitch LFO (replayer-exact —
    /// `oscilatval` 0,1,2,3,3,2,1,0 over 8 frames, never below the base note).
    /// `vib_depth` is the per-step right-shift of the semitone excursion.
    pub vibrato: bool,
    pub vib_depth: u8,
    pub vib_div: u8,
    /// Raw per-osc-step freq-register excursion for an overflow note whose
    /// vibrato `tempvdif` the replayer reads from work-RAM past `RH_FREQ_HEX`
    /// (e.g. Monty note 104 → 0x540 = 1344). 0 = use the semitone excursion.
    pub vib_tempvdif_reg: u16,
    /// Octave arpeggio: alternate note / note+12 every frame (`instrfx & 4`).
    pub arp: bool,
    /// Period-interpolation vibrato (`play $09f1`, instr byte +5): a triangle
    /// pitch swing of `±interp_half_depth` semitone steps (each `>>interp_shift`),
    /// the counter ping-ponging one step per frame. Separate from / wider than
    /// the `vib_depth` LFO. `interp_half_depth == 0` ⇒ off.
    pub interp_vib: bool,
    pub interp_half_depth: u8,
    pub interp_shift: u8,
    /// Frames after note-on before the swing runs at all (the ping-pong counter
    /// stays put meanwhile), and frames during which only the centring applies.
    /// Both 0 on the replayers that lack them.
    pub interp_delay: u8,
    pub interp_flat: u8,
    /// Two-phase waveform (v20 player effect 0x4): the raw attack control byte
    /// (waveform + sync/ring/test bits, e.g. pulse+sync 0x43) played for
    /// `wave_attack_frames` frames after note-on, then the patch's sustain
    /// waveform takes over. 0 ⇒ disabled.
    pub wave_attack_ctrl: u8,
    pub wave_attack_frames: u8,
    /// Two-phase PITCH attack (fxmask bit6, `0x40`): the absolute attack note
    /// index (0..=95) the voice plays for the attack phase before the struck
    /// note. The PITCH twin of `wave_attack_ctrl`, sharing `wave_attack_frames`.
    /// Only the coupled driver acts on it (the bit6 tunes — lightforce / sanxion
    /// / delta / ace_2 — all run coupled). `wave_attack_note_set == false` ⇒ off.
    pub wave_attack_note: u8,
    pub wave_attack_note_set: bool,
    /// Per-frame waveform alternation (v30 effect 0x2): toggle the control
    /// register between the instrument waveform and this alt waveform each
    /// frame (`$c348 & 1`). 0 ⇒ disabled.
    pub wave_alt_ctrl: u8,
    /// Two-note arpeggio (v30 effect 0x8): toggle pitch between the struck note
    /// and this fixed SID frequency register each frame. 0 ⇒ disabled.
    pub arp2_reg: u16,
    /// Cyclic semitone-offset arpeggio — the flattened
    /// [`crate::core::instr_robsid::ArpMode::Cycle`]. `arp_len == 0` ⇒ disabled;
    /// otherwise the note is shifted by `arp_steps[phase % arp_len]` SIGNED
    /// semitones each frame. `arp_per_note` selects the phase source (frames
    /// since note-on vs the global frame counter), and `arp_follow_climb` makes
    /// the offsets ride the note-increment skydive's climbed pitch (v15).
    pub arp_steps: [i8; 3],
    pub arp_len: u8,
    pub arp_per_note: bool,
    pub arp_follow_climb: bool,
    /// Note-increment skydive (`instrfx & 2` with a note-step replayer, e.g.
    /// Thrust): climb the pitch by one semitone every 4th frame, sweeping up
    /// from the note's base (the replayer's `notenum++; freq = table[notenum]`).
    pub skydive_climb: bool,
    /// Freq-add skydive (`instrfx & 2`, commando/monty/crazy/spellbound): every
    /// OTHER frame slide the freq-register HIGH byte by `skydive_add >> 8`
    /// (commando +512→+2, monty −256→−1), writing the swept high byte; the base
    /// note plays on the alternate frames. 0 = no freq-add skydive.
    pub skydive_add: i16,
    /// Skydive only fires while the note length exceeds this (the replayer's
    /// `skydive_v1_when`); 0 = always (monty), 2 = commando.
    pub skydive_when: u8,
    /// Drum (`instrfx & 1`): a percussive hit — the replayer plays one noise
    /// frame then clears the gate while sliding the frequency down, so the
    /// note is short instead of a sustained tone.
    pub drum: bool,
    /// SID filter: this instrument drives the global resonant low-pass. Only the
    /// coupled driver acts on it (the per-instrument bank has no global filter).
    pub filter: bool,
    /// `$D417` resonance | routing written while this voice drives the filter.
    pub filter_resfilt: u8,
    /// WRAP law (v10–v30): `$D416` per-frame signed cutoff-hi increment
    /// (0 = static). Ignored when `filter_bounce` is set.
    pub filter_step: i8,
    /// Cutoff seed loaded on note-on, 16-bit. v10–v30 only use the high byte.
    pub filter_seed: u16,
    /// BOUNCE law (last player): the cutoff is a 16-bit accumulator that
    /// reverses at `filter_bounce_min` / `filter_bounce_max` (compared on the
    /// high byte) instead of wrapping. Selects the law; `filter_step` is unused.
    pub filter_bounce: bool,
    pub filter_bounce_step: u16,
    pub filter_bounce_min: u8,
    pub filter_bounce_max: u8,
    /// Direction the bounce starts in (`true` = climbing).
    pub filter_bounce_up: bool,
    /// `$D418` high-nibble mode bits: 1 = LP, 2 = BP, 4 = HP (combinable).
    /// v10–v30 hardwire 1; 0 means "no filter mode" (the master-volume default).
    pub filter_mode: u8,
    /// The instrument's per-frame waveform + pitch program, flattened for the
    /// engine (see [`crate::core::instr_robsid::VoiceProgram`]). Empty ⇒ the
    /// voice runs the ordinary effect chain; non-empty ⇒ the program owns the
    /// waveform and pitch.
    pub program: super::sid::wave_program::ProgramSteps,
    /// Re-seed the cutoff sweep accumulator to `filter_seed` on **every**
    /// note-on, rather than continuing it across same-instrument notes. The v30
    /// replayer (Delta) zeroes the accumulator (`$c34e`) unconditionally in its
    /// note-setup (`$beaf`), so its cutoff pulses per note; older replayers let
    /// it run legato. Set by the importer per replayer version.
    pub filter_reseed_each_note: bool,
    /// Suppress the drum's freq-register-HIGH slide (the `drum_hi--` per-frame
    /// decrement). The v10 drum DOES slide the freq-hi down (register-exact vs
    /// commando — see the `drum` field), but the v15 routine (`$e2c6`) keeps the
    /// freq-hi constant per note; the v10 slide invents downward glissandi the
    /// v15 oracle never produces. Set for v15 drum instruments; the drum then
    /// owns only the control register (noise + gate-off), not the pitch.
    pub drum_no_freq_slide: bool,
    /// Zero AD+SR at note-off (v15 `$e198`) so the note cuts sharp rather than
    /// ringing out on its release nibble. See [`crate::core::instr_robsid::ReleaseMode::HardCut`].
    pub zero_adsr_on_note_off: bool,
    /// `ReleaseMode::Ramp` — the fixed envelope written a few frames before the
    /// note ends, while it is still gated. `release_after` is the per-NOTE
    /// countdown in frames (0 = this note has no ramp).
    pub release_ramp: bool,
    pub release_ad: u8,
    pub release_sr: u8,
    pub release_after: u16,
}

/// One hardware voice's allocation + per-frame-effect state within a chip.
#[derive(Clone, Copy, Default)]
struct VoiceSlot {
    /// Tracker channel currently holding this voice, if any.
    owner: Option<usize>,
    /// GATE is open (note held).
    gated: bool,
    /// Release nibble of the held patch (to seed the tail on note-off).
    release_nibble: u8,
    /// Output frames remaining of the release tail after gate-off.
    release_tail: u32,
    /// Unmodulated note frequency (milli-Hz) — the base the per-voice pitch
    /// effects (vibrato / arpeggio) modulate each frame.
    base_milli_hz: u32,
    /// Frames elapsed since the note triggered (drives the drum percussion).
    note_frame: u32,
    /// Note-increment skydive: semitones climbed so far this note (the
    /// replayer's `notenum - base_notenum`). Reset to 0 at each trigger.
    skydive_semis: u8,
    /// Drum slide state: the high byte of the 16-bit freq register, slid down
    /// by 1 each frame while the drum effect (`instrfx & 1`) is active — the
    /// replayer's `save_freq_high`. Seeded from the note's freq register at
    /// trigger; the drum stops once it reaches 0.
    drum_hi: u8,
    /// The voice's control byte with the gate cleared (instrument waveform +
    /// sync / ring / test, the replayer's `voice_ctrl & 0xfe`) — what the drum
    /// re-asserts on the chip each slide frame.
    gate_off_ctrl: u8,
    /// Voice-program cursor + running pitch bend. Reset at every trigger, so a
    /// program always restarts at step 0 on a new note.
    prog: super::sid::wave_program::ProgramState,
}

/// One SID chip (one instrument) with its three voices used as a polyphony
/// pool. Drives a [`sidera::Sid`]; translates note gestures into register
/// writes; tracks per-voice activity so the player can gate clocking and
/// route voices.
pub struct SidSynth {
    chip: Sid,
    chip_clock_hz: u32,
    output_rate: u32,
    slots: [VoiceSlot; 3],
    /// Per-frame effect parameters — per *instrument* (one [`SidSynth`](crate::generators::sid::SidSynth) = one
    /// instrument), so they're the same for every note this chip plays.
    fx: SidFx,
    /// Pulse-width sweep state, stored **per instrument** (the replayer keeps
    /// it in the instrument data, so the sweep is continuous across notes — a
    /// per-note reset would break Hubbard's slow PW decrescendos).
    pw_cur: i32,
    pulse_dir: u8,
    pulse_delay_ctr: i32,
    /// `pw_cur` has been seeded from the first note's patch.
    pw_init: bool,
    /// Diagnostic shadow of every register written to the chip (the chip is
    /// write-only). Lets a host build an oracle-comparable register snapshot.
    reg: [u8; 0x20],
}

impl SidSynth {
    /// Create a chip for `output_rate` Hz with the given model and region.
    pub fn new(output_rate: u32, model: SidModel, region: SidRegion) -> Self {
        let output_rate = output_rate.max(1);
        let mut chip = Sid::new(output_rate);
        chip.set_model(match model {
            SidModel::Mos6581 => ChipModel::Mos6581,
            SidModel::Mos8580 => ChipModel::Mos8580,
        });
        let chip_clock_hz = region.clock_hz();
        chip.set_chip_clock(chip_clock_hz);
        // Master volume to max; filter mode off; VOICE-3-OFF clear. Per-note
        // and per-track gain are applied host-side (the SID has no per-voice
        // volume register), so the chip runs wide open.
        chip.write_register(MODE_VOL, 0x0F);
        Self {
            chip,
            chip_clock_hz,
            output_rate,
            slots: [VoiceSlot::default(); 3],
            fx: SidFx::default(),
            pw_cur: 0,
            pulse_dir: 0,
            pulse_delay_ctr: 0,
            pw_init: false,
            reg: [0u8; 0x20],
        }
    }

    /// Write a chip register, shadowing the value for the diagnostic snapshot.
    #[inline]
    fn wr(&mut self, reg: u8, val: u8) {
        if (reg as usize) < self.reg.len() {
            self.reg[reg as usize] = val;
        }
        self.chip.write_register(reg, val);
    }

    /// The 7 registers ($Dx00..$Dx06) of the voice currently held by tracker
    /// channel `owner`, if any — for an oracle-comparable register dump.
    pub fn owner_voice_regs(&self, owner: usize) -> Option<[u8; 7]> {
        let slot = self.slot_of(owner)?;
        let b = slot * 7;
        let mut out = [0u8; 7];
        out.copy_from_slice(&self.reg[b..b + 7]);
        Some(out)
    }

    /// Diagnostic: the unmodulated base frequency (milli-Hz) and gate state of
    /// the voice held by tracker channel `owner`, if any. For per-voice traces
    /// that compare the synth's effect base against the `sidplay` oracle's three
    /// real SID voices (the `$D400-$D418` snapshot can't disambiguate which
    /// instrument-chip a channel lives on). Diagnostic only.
    #[cfg(feature = "std")]
    pub fn owner_state(&self, owner: usize) -> Option<(u32, bool)> {
        let slot = self.slot_of(owner)?;
        let s = &self.slots[slot];
        Some((s.base_milli_hz, s.gated))
    }

    /// Register offset for hardware voice `slot` (voices are 7 registers apart).
    #[inline]
    fn base(slot: usize) -> u8 {
        slot as u8 * 7
    }

    /// Pick a hardware voice for `owner`: its existing voice, then any
    /// free/silent voice, else steal voice 0 (mirrors the OPL slot logic —
    /// when oversubscribed something has to give).
    fn assign_slot(&mut self, owner: usize) -> usize {
        if let Some(i) = self.slots.iter().position(|s| s.owner == Some(owner)) {
            return i;
        }
        if let Some(i) = self
            .slots
            .iter()
            .position(|s| !s.gated && s.release_tail == 0)
        {
            return i;
        }
        0
    }

    fn slot_of(&self, owner: usize) -> Option<usize> {
        self.slots.iter().position(|s| s.owner == Some(owner))
    }

    /// Convert a frequency in milli-Hertz to the 16-bit FREQ register:
    /// milli-Hz → SID frequency register (delegates to the shared helper).
    #[inline]
    fn freq_reg(&self, milli_hz: u32) -> u16 {
        fx_engine::freq_reg(milli_hz, self.chip_clock_hz)
    }

    fn write_freq(&mut self, slot: usize, milli_hz: u32) {
        let freq_reg = self.freq_reg(milli_hz);
        let b = Self::base(slot);
        self.wr(b + V1_FREQ_LO, (freq_reg & 0xFF) as u8);
        self.wr(b + V1_FREQ_HI, (freq_reg >> 8) as u8);
    }

    /// Write the 12-bit pulse width to a voice (the per-instrument swept value).
    fn write_pw(&mut self, slot: usize, pw: u16) {
        let b = Self::base(slot);
        self.wr(b + V1_PULSE_LO, (pw & 0xFF) as u8);
        self.wr(b + V1_PULSE_HI, ((pw >> 8) & 0x0F) as u8);
    }

    /// Program a voice's waveform / ADSR / sync-ring-test (GATE stays closed;
    /// [`Self::note_on`] opens it). The pulse width is written separately from
    /// the per-instrument sweep state. Returns the gate-off control byte (the
    /// replayer's `voice_ctrl & 0xfe`), which the drum effect re-asserts.
    fn program_voice(&mut self, slot: usize, p: &SidVoicePatch) -> u8 {
        let b = Self::base(slot);
        self.wr(b + V1_ATTACK_DECAY, p.attack_decay);
        self.wr(b + V1_SUSTAIN_RELEASE, p.sustain_release);
        let ctrl = ((p.waveform & 0x0F) << 4)
            | (CTRL_SYNC * p.sync as u8)
            | (CTRL_RING * p.ring as u8)
            | (CTRL_TEST * p.test as u8);
        self.wr(b + V1_CONTROL, ctrl);
        ctrl
    }

    /// Trigger a note for tracker channel `owner`: allocate a voice, program
    /// the patch + pitch, open the gate. Returns the hardware voice index used
    /// (its position in the [`Self::clock`] triplet).
    pub fn note_on(
        &mut self,
        owner: usize,
        patch: &SidVoicePatch,
        fx: &SidFx,
        milli_hz: u32,
    ) -> usize {
        let slot = self.assign_slot(owner);
        // Effect params are per-instrument (same every note). Seed the PW sweep
        // ONCE from the first note's patch; thereafter it persists and keeps
        // sweeping across notes (replayer stores it in the instrument data).
        self.fx = *fx;
        // PW seed, mirroring the two replayer behaviours (see
        // `SidFx::pw_reseed_on_note`): the v20/v25 routine reloads the
        // instrument base PW on every note-on (sweep restarts each note),
        // whereas the v10/v15/v30 routine mutates the instrument PW table in
        // place and reloads from there, so the sweep is *continuous* across
        // notes (Monty/commando bounce the high nibble 0x8..0xE — they never run
        // away, the bounce caps them, so continuing is both correct and safe).
        // Either way the direction / delay counters are seeded once and free-run.
        if fx.pw_reseed_on_note || !self.pw_init {
            self.pw_cur = patch.pulse_width as i32;
        }
        if !self.pw_init {
            self.pulse_dir = 0;
            self.pulse_delay_ctr = fx.pw_delay as i32;
            self.pw_init = true;
        }
        let gate_off_ctrl = self.program_voice(slot, patch);
        let pw = self.pw_cur as u16;
        self.write_pw(slot, pw);
        self.write_freq(slot, milli_hz);
        // Open the gate, preserving the waveform / sync / ring / test bits —
        // BUT only when the instrument actually selects a waveform. A patch
        // with no waveform bits is a **silent instrument** (the replayer stores
        // its control byte with the gate bit clear too — e.g. Delta's instr 5,
        // `$00`, used as a rest/timing voice). Gating it would force the
        // envelope open on a null oscillator instead of staying silent like the
        // real chip, so respect the waveform: no waveform ⇒ no gate.
        let b = Self::base(slot);
        let has_waveform = (patch.waveform & 0x0F) != 0;
        let ctrl = if has_waveform {
            self.chip.control(slot as u8) | CTRL_GATE
        } else {
            self.chip.control(slot as u8) & !CTRL_GATE
        };
        self.wr(b + V1_CONTROL, ctrl);
        self.slots[slot] = VoiceSlot {
            // A program always restarts at step 0 on a new note.
            prog: Default::default(),
            owner: Some(owner),
            gated: has_waveform,
            release_nibble: patch.sustain_release & 0x0F,
            release_tail: 0,
            base_milli_hz: milli_hz,
            note_frame: 0,
            skydive_semis: 0,
            // Seed the drum slide from the note's freq-register high byte
            // (the replayer's `save_freq_high`); the gate-off waveform is what
            // the slide re-asserts on the chip each frame.
            drum_hi: (self.freq_reg(milli_hz) >> 8) as u8,
            gate_off_ctrl,
        };
        slot
    }

    /// Advance the per-instrument pulse-width sweep one frame (replayer-exact):
    /// step only every `pw_delay` frames, by the top-3-bit speed value, with
    /// the high nibble bouncing between 0x8 and 0xE (pulse width 0x800..0xE00).
    /// The swept value is written to every active voice — the sweep is shared
    /// across the instrument's notes (and so is *continuous*, the key to
    /// Hubbard's slow PW decrescendos). Call once per frame per instrument.
    pub fn advance_pw(&mut self) {
        if !self.fx.pw_sweep || (self.fx.pw_delay == 0 && self.fx.pw_speed == 0) {
            return;
        }
        if !self.any_active() {
            return; // the sweep only runs while the instrument is sounding
        }
        // Low-byte-only increment mode (v10 fx bit 3): full pw_speed added to the
        // PW low byte each frame, 8-bit wrap, high byte untouched, no delay/bounce.
        if self.fx.pw_low_byte_inc {
            let step = (self.fx.pw_speed as u8) as i32;
            let pw_lo = ((self.pw_cur & 0xFF) + step) & 0xFF;
            self.pw_cur = (self.pw_cur & 0xF00) | pw_lo;
            let pw = self.pw_cur as u16;
            for slot in 0..self.slots.len() {
                if self.slots[slot].owner.is_some() {
                    self.write_pw(slot, pw);
                }
            }
            return;
        }
        self.pulse_delay_ctr -= 1;
        if self.pulse_delay_ctr >= 0 {
            return;
        }
        self.pulse_delay_ctr = self.fx.pw_delay as i32;
        let step = (self.fx.pw_speed as u8) as i32; // unsigned 0x20..0xE0
                                                    // Per-instrument bounce bounds (fx_v2[i]+5 nibbles) when explicitly set,
                                                    // else the legacy fixed 0x0E/0x08 (a bound of 0 is legitimate, so the
                                                    // flag is required to tell it from "unset").
        let (hi, lo) = if self.fx.pw_bounds_set {
            (self.fx.pw_hi_bound as i32, self.fx.pw_lo_bound as i32)
        } else {
            (0x0E, 0x08)
        };
        if self.pulse_dir == 0 {
            self.pw_cur += step;
            if (self.pw_cur >> 8) >= hi {
                self.pulse_dir = 1;
            }
        } else {
            self.pw_cur -= step;
            if (self.pw_cur >> 8) <= lo {
                self.pulse_dir = 0;
            }
        }
        self.pw_cur = self.pw_cur.clamp(0, 0xFFF);
        let pw = self.pw_cur as u16;
        for slot in 0..self.slots.len() {
            if self.slots[slot].owner.is_some() {
                self.write_pw(slot, pw);
            }
        }
    }

    /// Advance one frame of per-*note* modulation (drum / vibrato / arpeggio)
    /// for `owner`'s voice, on the global frame phase. `speed` is the song's
    /// ticks-per-row (the replayer's `resetspd + 1`), needed to place the drum's
    /// initial noise burst (the first row's frames). The pulse-width sweep is
    /// per-instrument — see [`Self::advance_pw`].
    pub fn advance_fx(&mut self, owner: usize, frame: u32, speed: u8) {
        let Some(slot) = self.slot_of(owner) else {
            return;
        };
        let fx = self.fx;
        let (base, gated, note_frame, drum_hi0, gate_off_ctrl, skydive_semis) = {
            let s = &mut self.slots[slot];
            s.note_frame = s.note_frame.saturating_add(1);
            // Note-increment skydive climbs one semitone every 4th frame
            // (replayer `counter & 3 == 0`), only while the note is held.
            // Note-increment climb keeps running through the note's release (no
            // gate check in the replayer) — see `CoupledSid::advance_fx`.
            if fx.skydive_climb && (s.gated || s.release_tail > 0) && frame & 3 == 0 {
                s.skydive_semis = s.skydive_semis.saturating_add(1);
            }
            (
                s.base_milli_hz,
                s.gated,
                s.note_frame,
                s.drum_hi,
                s.gate_off_ctrl,
                s.skydive_semis,
            )
        };
        if base == 0 {
            return;
        }

        // Replayer effect order each frame: vibrato → drum → arpeggio, each
        // writing the freq register so the LAST one wins for the audible pitch.
        // The drum additionally owns the CONTROL register (noise burst, gate
        // off), which arpeggio/vibrato never touch — so e.g. a `drum + arp`
        // instrument arpeggiates (arp owns the freq) *while* the drum makes it
        // percussive (drum owns the ctrl). This coexistence is why a drum must
        // not be modelled as exclusive with the pitch effects.
        // Shared per-frame modulation (vibrato → drum → skydive → arp; the
        // coupled-only interp/arp2/alt-arp/wave-alt blocks are inert here because
        // no per-instrument tune sets their flags — verified by `sid_flag_audit`).
        // `note_frame` and the skydive counter were advanced in the extract block;
        // `interp_*` is coupled-only state, so pass 0/false (never read here).
        let mut st = fx_engine::FxState {
            base_milli_hz: base,
            gated,
            note_frame,
            // Per-instrument tunes drive their arpeggio off the GLOBAL frame
            // counter, never the per-note phase (`arp_per_note` is a last-player
            // trait), so this is never read on this path.
            arp_phase: 0,
            drum_hi: drum_hi0,
            gate_off_ctrl,
            skydive_semis,
            interp_ctr: 0,
            interp_up: false,
            // The interp vibrato is a last-player / v15 trait; this driver's
            // generations never arm it, so its start delay never runs.
            vib_delay: 0,
            prog: self.slots[slot].prog,
        };
        let m = fx_engine::modulate(&mut st, &fx, frame, speed, self.chip_clock_hz);
        let (reg, ctrl_override) = (m.reg, m.ctrl);
        self.slots[slot].drum_hi = st.drum_hi;
        self.slots[slot].prog = st.prog;

        let b = Self::base(slot);
        // A `Fixed` program step writes `$d401` alone and leaves `$d400` as the
        // last full write left it (replayer `$16cb`).
        if !m.freq_hi_only {
            self.wr(b + V1_FREQ_LO, (reg & 0xFF) as u8);
        }
        self.wr(b + V1_FREQ_HI, (reg >> 8) as u8);
        if let Some(c) = ctrl_override {
            self.wr(b + V1_CONTROL, c);
        }
    }

    /// Retune the held note on `owner` (portamento / pitch slide). Updates the
    /// effect base; [`Self::advance_fx`] re-modulates on top each frame.
    pub fn set_frequency(&mut self, owner: usize, milli_hz: u32) {
        if let Some(slot) = self.slot_of(owner) {
            self.slots[slot].base_milli_hz = milli_hz;
            self.write_freq(slot, milli_hz);
        }
    }

    /// Release the note on `owner` (note-off / fade): close the gate and seed
    /// the release tail so the voice stays active until it decays.
    ///
    /// `is_fetch` distinguishes the two replayer note-end paths (Hubbard `play`
    /// @ `$8012`): the ordinary **release** path (`is_fetch == false`, our
    /// `KeyOff`) runs the replayer's "kill adsr" — gate-off AND `AD=SR=0` — when
    /// the version does it (`zero_adsr_on_note_off`). The **appended-note** path
    /// (`is_fetch == true`, our `KeyOffFetch`) clears the gate but RE-FETCHES the
    /// instrument's AD/SR (`ctrl = instr.ctrl & 0xfe`, AD/SR rewritten from the
    /// instrument, unchanged) — it must NEVER zero them, so a long note (e.g.
    /// Monty's voice-1 lead, `SR=0xff`) rings out on its release nibble instead
    /// of being cut dead.
    pub fn note_off(&mut self, owner: usize, is_fetch: bool) {
        if let Some(slot) = self.slot_of(owner) {
            let b = Self::base(slot);
            // Note-end: clear the gate bit. The v10/v15 replayers ALSO zero AD+SR
            // at note-end (verified vs the sidplay regdump — `$d405/$d406 = 0`),
            // cutting the note sharp; v30 (Delta) does NOT (its notes ring out at
            // the instrument's release nibble — zeroing SR there cut every note
            // dead). Gated by `zero_adsr_on_note_off` (set per version by the
            // importer) rather than a blanket choice — and NEVER on a note-fetch.
            // Gate-off restores the instrument's STORED waveform without the gate
            // (replayer `(&DAT_54f8)[v] & 0xfe`), NOT the current control register:
            // the drum leaves 0x80 (pure noise) in the register, and `& ~GATE` on
            // that would freeze the noise on instead of releasing the note's own
            // waveform. For a note with no effect overriding ctrl the two are
            // identical (reg == base_ctrl|GATE ⇒ reg & ~GATE == gate_off_ctrl).
            let ctrl = self.slots[slot].gate_off_ctrl;
            self.wr(b + V1_CONTROL, ctrl);
            self.slots[slot].gated = false;
            if !is_fetch && self.fx.zero_adsr_on_note_off {
                self.wr(b + V1_ATTACK_DECAY, 0);
                self.wr(b + V1_SUSTAIN_RELEASE, 0);
            }
            let nibble = self.slots[slot].release_nibble;
            self.slots[slot].release_tail = self.release_frames(nibble);
        }
    }

    /// Hard cut (note-cut, or the channel taken by another instrument): close
    /// the gate and free the voice for immediate reuse. The SID has no instant
    /// mute, so a faint release tail may still sound — matching the dry,
    /// no-ramp cut behaviour of the rest of the engine.
    pub fn note_cut(&mut self, owner: usize) {
        if let Some(slot) = self.slot_of(owner) {
            let b = Self::base(slot);
            let ctrl = self.chip.control(slot as u8) & !CTRL_GATE;
            self.wr(b + V1_CONTROL, ctrl);
            self.slots[slot] = VoiceSlot::default();
        }
    }

    fn release_frames(&self, nibble: u8) -> u32 {
        (RELEASE_MS[(nibble & 0x0F) as usize] as u32 * self.output_rate) / 1000
    }

    /// Advance the chip by one output frame, returning `(mix, [v0, v1, v2])`.
    /// `mix` is the faithful chip output; the triplet is the per-voice routing
    /// hook (see the module docs). Decrements the release-tail counters.
    pub fn clock(&mut self) -> (i16, [i32; 3]) {
        let out = self.chip.clock_with_voices();
        for s in &mut self.slots {
            if !s.gated && s.release_tail > 0 {
                s.release_tail -= 1;
            }
        }
        out
    }

    /// The tracker channel routed to hardware voice `i` (`0..3`), if any —
    /// for sending `clock()`'s triplet entry to that channel's strip.
    pub fn voice_owner(&self, i: usize) -> Option<usize> {
        self.slots.get(i).and_then(|s| s.owner)
    }

    /// Is hardware voice `i` still sounding (gated or in its release tail)?
    pub fn voice_active(&self, i: usize) -> bool {
        self.slots
            .get(i)
            .map(|s| s.gated || s.release_tail > 0)
            .unwrap_or(false)
    }

    /// Any voice still sounding? Cheap activity gate — the player skips
    /// clocking (and folding) a synth that is fully idle.
    pub fn any_active(&self) -> bool {
        self.slots.iter().any(|s| s.gated || s.release_tail > 0)
    }

    /// Number of voices currently sounding (gated or in release) — diagnostic.
    pub fn active_voice_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|s| s.gated || s.release_tail > 0)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plain sawtooth patch with an instant attack and full sustain.
    fn saw() -> SidVoicePatch {
        SidVoicePatch {
            waveform: 0b0010,      // saw
            sustain_release: 0xF0, // sustain 15, release 0 (~6 ms tail)
            ..Default::default()
        }
    }

    #[test]
    fn synth_plays_then_releases_to_idle() {
        // 8580: no voice DC, so an idle voice reads exactly 0 and a gated one
        // carries energy — clean assertions on the routing triplet.
        let mut s = SidSynth::new(44100, SidModel::Mos8580, SidRegion::Pal);
        let slot = s.note_on(0, &saw(), &SidFx::default(), 440_000);
        assert_eq!(s.voice_owner(slot), Some(0));

        let mut peak = 0i32;
        for _ in 0..4410 {
            let (_mix, v) = s.clock();
            peak = peak.max(v[slot].abs());
        }
        assert!(peak > 0, "a gated voice should sound (peak={peak})");
        assert!(s.any_active());

        s.note_off(0, false);
        let mut frames = 0;
        while s.any_active() && frames < 44100 {
            s.clock();
            frames += 1;
        }
        assert!(
            !s.any_active(),
            "voice should fall idle after its release tail"
        );
    }

    #[test]
    fn two_notes_take_distinct_voices_and_route_independently() {
        let mut s = SidSynth::new(44100, SidModel::Mos8580, SidRegion::Pal);
        let a = s.note_on(0, &saw(), &SidFx::default(), 220_000);
        let b = s.note_on(1, &saw(), &SidFx::default(), 440_000);
        assert_ne!(a, b, "distinct owners get distinct hardware voices");
        assert_eq!(s.voice_owner(a), Some(0));
        assert_eq!(s.voice_owner(b), Some(1));

        let mut pk = [0i32; 3];
        for _ in 0..4410 {
            let (_mix, v) = s.clock();
            for i in 0..3 {
                pk[i] = pk[i].max(v[i].abs());
            }
        }
        assert!(
            pk[a] > 0 && pk[b] > 0,
            "both notes sound on their own voice"
        );
        let unused = (0..3).find(|i| *i != a && *i != b).unwrap();
        assert_eq!(pk[unused], 0, "an unallocated voice stays silent on 8580");
    }

    /// The drum (`instrfx & 1`) must reproduce the replayer's register
    /// sequence: NOISE (ctrl 0x80) held at the note's freq-high byte for the
    /// first row (`speed - 1` frames), then the instrument waveform with the
    /// gate cleared while the freq-high byte slides down one per frame.
    #[test]
    fn drum_plays_noise_burst_then_slides_freq_high() {
        const SPEED: u8 = 3;
        let mut s = SidSynth::new(44100, SidModel::Mos8580, SidRegion::Pal);
        // Triangle patch (waveform bit0) with a drum effect, no arp/vibrato.
        let patch = SidVoicePatch {
            waveform: 0b0001,
            attack_decay: 0x06,
            sustain_release: 0x40,
            ..Default::default()
        };
        let fx = SidFx {
            drum: true,
            ..Default::default()
        };
        // A high enough note that the slide has several steps before bottoming.
        let slot = s.note_on(0, &patch, &fx, 2_000_000);
        let hi0 = s.owner_voice_regs(0).unwrap()[V1_FREQ_HI as usize];
        assert!(
            hi0 >= 4,
            "test note should have a non-trivial freq-high byte"
        );
        // Gate-off control byte the slide re-asserts (triangle, gate clear).
        let gate_off = (0b0001u8) << 4;

        // Frames 1..SPEED-1 (= note_frame < SPEED): NOISE, freq-high held at hi0.
        for _ in 1..SPEED {
            s.advance_fx(0, 0, SPEED);
            let r = s.owner_voice_regs(0).unwrap();
            assert_eq!(r[V1_CONTROL as usize], 0x80, "first row plays noise");
            assert_eq!(
                r[V1_FREQ_HI as usize], hi0,
                "freq-high held during noise burst"
            );
        }
        // Subsequent frames: instrument waveform, gate off, freq-high slides down.
        let mut expect_hi = hi0;
        for _ in 0..3 {
            s.advance_fx(0, 0, SPEED);
            let r = s.owner_voice_regs(0).unwrap();
            assert_eq!(
                r[V1_CONTROL as usize], gate_off,
                "slide uses instr waveform, gate off"
            );
            assert_eq!(
                r[V1_FREQ_HI as usize], expect_hi,
                "freq-high emits then decrements"
            );
            expect_hi -= 1;
        }
        let _ = slot;
    }

    #[test]
    fn freed_voice_is_reused() {
        let mut s = SidSynth::new(44100, SidModel::Mos8580, SidRegion::Pal);
        let a = s.note_on(0, &saw(), &SidFx::default(), 220_000);
        s.note_cut(0); // frees the voice immediately
        assert!(!s.any_active());
        let b = s.note_on(1, &saw(), &SidFx::default(), 440_000);
        assert_eq!(a, b, "a hard-cut voice is reused by the next note");
        assert_eq!(s.voice_owner(b), Some(1));
    }

    /// The last player's BOUNCE cutoff law (`CutoffSweep::Bounce`): a 16-bit
    /// accumulator that reverses at the slot's bounds instead of wrapping like
    /// the v10–v30 law. No shipped tune uses it yet — the SID oracle gate cannot
    /// cover it, so it is pinned here directly.
    #[test]
    fn filter_bounce_law_reverses_at_both_bounds() {
        use super::coupled::CoupledSid;
        use sidera::registers::{FC_HI, FC_LO};

        let mut s = CoupledSid::new(44100, SidModel::Mos8580, SidRegion::Pal, 6);
        let fx = SidFx {
            filter: true,
            filter_mode: 2, // band-pass
            filter_seed: 0x2000,
            filter_bounce: true,
            filter_bounce_step: 0x0600,
            filter_bounce_min: 0x1D,
            filter_bounce_max: 0x30,
            filter_bounce_up: true,
            ..Default::default()
        };
        s.note_on(0, 0, &saw(), &fx, 220_000);
        let regs = s.snapshot();
        assert_eq!(
            regs[FC_HI as usize], 0x20,
            "note-on seeds the cutoff high byte"
        );
        assert_eq!(
            regs[MODE_VOL as usize], 0x2F,
            "the slot's mode nibble reaches $D418 (band-pass + volume 15)"
        );

        // Climb: 0x2000 → 0x2600 → clamped to max<<8 with the low byte zeroed.
        let mut seen_max = false;
        for _ in 0..4 {
            s.begin_frame();
            s.advance_fx(0);
            let r = s.snapshot();
            if r[FC_HI as usize] == 0x30 {
                seen_max = true;
                assert_eq!(
                    r[FC_LO as usize], 0,
                    "clamping to a bound zeroes the low byte"
                );
                break;
            }
        }
        assert!(seen_max, "the sweep should climb to its upper bound");

        // Having reversed, it must now fall to the lower bound, not wrap past it.
        let mut seen_min = false;
        for _ in 0..8 {
            s.begin_frame();
            s.advance_fx(0);
            let r = s.snapshot();
            assert!(
                r[FC_HI as usize] >= 0x1D && r[FC_HI as usize] <= 0x30,
                "the bounce must stay inside its bounds, got {:#04x}",
                r[FC_HI as usize]
            );
            if r[FC_HI as usize] == 0x1D {
                seen_min = true;
                break;
            }
        }
        assert!(
            seen_min,
            "the sweep should reverse and reach its lower bound"
        );
    }

    /// End-to-end: a `VoiceProgram` reaches the chip through the coupled driver
    /// and drives the control register frame by frame. No shipped tune carries a
    /// program yet, so the SID oracle gate cannot see this path — it is pinned
    /// here, like the filter bounce law.
    #[test]
    fn voice_program_drives_the_control_register_frame_by_frame() {
        use super::coupled::CoupledSid;
        use crate::core::instr_robsid::{
            PitchAction, ProgramEnd, VoiceProgram, WaveShape, WaveStep,
        };
        use alloc::vec;
        use sidera::registers::V1_CONTROL;

        let step_of = |ctrl: u8, pitch: PitchAction| WaveStep {
            shape: WaveShape::from_ctrl(ctrl),
            pitch,
        };
        // Noise burst → bending pulse → triangle, then freeze on the triangle.
        let prog = VoiceProgram {
            steps: vec![
                step_of(0x81, PitchAction::Fixed(0x30)),
                step_of(0x41, PitchAction::Bend(0x0400)),
                step_of(0x11, PitchAction::Bend(0x0100)),
            ],
            end: ProgramEnd::Hold,
        };
        let (flat, dropped) = wave_program::ProgramSteps::from_model(&prog);
        assert_eq!(dropped, 0);

        let mut s = CoupledSid::new(44100, SidModel::Mos8580, SidRegion::Pal, 6);
        let fx = SidFx {
            program: flat,
            ..Default::default()
        };
        s.note_on(0, 0, &saw(), &fx, 440_000);

        let ctrl_of = |s: &CoupledSid| s.snapshot()[V1_CONTROL as usize];
        let mut ctrls = alloc::vec::Vec::new();
        for _ in 0..5 {
            s.begin_frame();
            s.advance_fx(0);
            ctrls.push(ctrl_of(&s));
        }
        assert_eq!(
            ctrls,
            vec![0x81, 0x41, 0x11, 0x11, 0x11],
            "the program owns the waveform, then freezes on its last step"
        );
    }
}
