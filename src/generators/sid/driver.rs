//! Per-instrument SID **bank** — the multi-chip driver the player owns, the
//! SID counterpart of OPL's single `OplDriver`. The "one chip per instrument"
//! model means the bank holds a lazy [`SidSynth`] per module instrument
//! (created on first trigger), each chip's three voices serving as that
//! instrument's polyphony pool.
//!
//! The bank also tracks, per tracker channel, which instrument it currently
//! sounds, so switching a channel to a different SID instrument releases the
//! voice it held on the previous chip.

use alloc::vec;
use alloc::vec::Vec;

use crate::tracker::instr_robsid::{ArpMode, RobEffects, SkydiveMode};
use crate::tracker::instr_sid::SidVoice;

use super::{SidFx, SidModel, SidRegion, SidSynth, SidVoicePatch};

/// A bank of per-instrument SID chips driven by tracker-channel gestures.
pub struct SidBank {
    /// One lazily-created chip per module instrument index.
    synths: Vec<Option<SidSynth>>,
    /// Per tracker channel: the instrument it is currently sounding, if any.
    channel_instr: Vec<Option<usize>>,
    /// Global VBlank frame counter (the replayer's `counter`) — drives the
    /// shared vibrato / arpeggio phase across all voices. Advanced once per
    /// frame via [`Self::begin_frame`].
    frame_counter: u32,
    /// Song ticks-per-row (the replayer's `resetspd + 1`). The drum effect needs
    /// it to place its initial noise burst over the note's first row.
    speed: u8,
    model: SidModel,
    region: SidRegion,
    output_rate: u32,
}

impl SidBank {
    /// Build an empty bank sized for `num_instruments` and `num_channels`.
    /// No chip is allocated until a note actually triggers one.
    pub fn new(
        output_rate: u32,
        num_instruments: usize,
        num_channels: usize,
        speed: u8,
        model: SidModel,
        region: SidRegion,
    ) -> Self {
        Self {
            synths: (0..num_instruments).map(|_| None).collect(),
            channel_instr: vec![None; num_channels],
            speed: speed.max(1),
            // The replayer resets its `counter` to 0 on the music-start frame
            // and increments it at the *top* of `play()`, so the first played
            // frame runs with `counter == 0`. `begin_frame` increments before
            // the channels are driven, so start one below 0 (wrapping) to land
            // the first frame on 0 — this aligns the shared vibrato (`&7`
            // triangle phase) and octave-arpeggio (`&1`) with the real engine.
            frame_counter: u32::MAX,
            model,
            region,
            output_rate: output_rate.max(1),
        }
    }

    /// Advance the global VBlank frame counter once per frame (call at the
    /// start of each tick, before driving the channels) and step every active
    /// instrument's per-instrument pulse-width sweep.
    pub fn begin_frame(&mut self) {
        self.frame_counter = self.frame_counter.wrapping_add(1);
        for s in self.synths.iter_mut().flatten() {
            s.advance_pw();
        }
    }

    /// Get (creating on first use) the chip for instrument `instr`.
    fn synth_mut(&mut self, instr: usize) -> Option<&mut SidSynth> {
        let slot = self.synths.get_mut(instr)?;
        if slot.is_none() {
            *slot = Some(SidSynth::new(self.output_rate, self.model, self.region));
        }
        slot.as_mut()
    }

    fn synth_at(&mut self, instr: usize) -> Option<&mut SidSynth> {
        self.synths.get_mut(instr).and_then(|o| o.as_mut())
    }

    /// Trigger a note: release any different instrument this channel held,
    /// then key-on `instr`'s chip with the patch + per-frame effects built
    /// from `voice` / `fx`.
    pub fn note_on(
        &mut self,
        channel: usize,
        instr: usize,
        voice: &SidVoice,
        fx: &RobEffects,
        milli_hz: u32,
    ) {
        if let Some(prev) = self.channel_instr.get(channel).copied().flatten() {
            if prev != instr {
                if let Some(s) = self.synth_at(prev) {
                    s.note_cut(channel);
                }
            }
        }
        let patch = patch_from_voice(voice);
        let sfx = fx_from_robeffects(fx);
        // The driver honours `re.drum.enable` directly: the importer only sets it where
        // the drum has been verified register-exact (so it doubles as the gate).
        if let Some(s) = self.synth_mut(instr) {
            s.note_on(channel, &patch, &sfx, milli_hz);
        }
        if let Some(slot) = self.channel_instr.get_mut(channel) {
            *slot = Some(instr);
        }
    }

    /// Advance one frame of per-note Rob-Hubbard modulation for `channel`'s
    /// held note (called once per player tick = once per VBlank frame).
    pub fn advance_fx(&mut self, channel: usize) {
        let frame = self.frame_counter;
        let speed = self.speed;
        if let Some(instr) = self.channel_instr.get(channel).copied().flatten() {
            if let Some(s) = self.synth_at(instr) {
                s.advance_fx(channel, frame, speed);
            }
        }
    }

    /// Retune the held note on `channel` (porta / slide / vibrato).
    pub fn set_frequency(&mut self, channel: usize, milli_hz: u32) {
        if let Some(instr) = self.channel_instr.get(channel).copied().flatten() {
            if let Some(s) = self.synth_at(instr) {
                s.set_frequency(channel, milli_hz);
            }
        }
    }

    /// Release the held note on `channel` (note-off / fade). `is_fetch` marks an
    /// appended-note fetch (which re-fetches AD/SR rather than zeroing them) — see
    /// [`SidSynth::note_off`](super::SidSynth::note_off).
    pub fn note_off(&mut self, channel: usize, is_fetch: bool) {
        if let Some(instr) = self.channel_instr.get(channel).copied().flatten() {
            if let Some(s) = self.synth_at(instr) {
                s.note_off(channel, is_fetch);
            }
        }
    }

    /// Hard-cut the held note on `channel` and free its chip voice.
    pub fn note_cut(&mut self, channel: usize) {
        if let Some(instr) = self.channel_instr.get(channel).copied().flatten() {
            if let Some(s) = self.synth_at(instr) {
                s.note_cut(channel);
            }
        }
        if let Some(slot) = self.channel_instr.get_mut(channel) {
            *slot = None;
        }
    }

    /// Any chip still sounding? Cheap gate for the mixer's fold pass.
    pub fn any_active(&self) -> bool {
        self.synths.iter().flatten().any(|s| s.any_active())
    }

    /// Advance every active chip by one output frame and sum their faithful
    /// `mix` outputs (mono). Phase A folds this single mono value into the
    /// master; per-voice routing to separate tracks is Phase B. Because the
    /// filter is off and the RC / volume stages are linear, summing each
    /// instrument-chip's mix is equivalent to the original single-chip mix.
    pub fn mix_frame(&mut self) -> i32 {
        let mut acc = 0i32;
        for s in self.synths.iter_mut().flatten() {
            if s.any_active() {
                let (mix, _voices) = s.clock();
                acc += mix as i32;
            }
        }
        acc
    }
}

impl SidBank {
    /// Build an oracle-comparable 25-register snapshot ($D400-$D418) of the
    /// "virtual chip": tracker channel `i` maps to SID voice `i`
    /// ($D400 + i·7), reading that channel's held-voice register shadow. Master
    /// volume is the constant 0x0F the replayer writes. Diagnostic only.
    #[cfg(feature = "std")]
    #[doc(hidden)]
    pub fn snapshot(&self) -> [u8; 0x19] {
        let mut regs = [0u8; 0x19];
        regs[0x18] = 0x0f;
        for ch in 0..3 {
            if let Some(instr) = self.channel_instr.get(ch).copied().flatten() {
                if let Some(s) = self.synths.get(instr).and_then(|o| o.as_ref()) {
                    if let Some(vr) = s.owner_voice_regs(ch) {
                        regs[ch * 7..ch * 7 + 7].copy_from_slice(&vr);
                    }
                }
            }
        }
        regs
    }

    /// Push a register snapshot to the diagnostic capture buffer. A cheap
    /// thread-local flag check unless a capture is active via [`capture_begin`] —
    /// the snapshot is not even computed when no capture is running.
    #[cfg(feature = "std")]
    #[doc(hidden)]
    pub fn capture_frame(&self) {
        if CAPTURE_ON.with(|f| f.get()) {
            push_capture(self.snapshot());
        }
    }
}

/// Push one `$D400-$D418` snapshot to the diagnostic capture buffer. Shared by
/// both drivers; callers guard on [`CAPTURE_ON`] so the snapshot is only built
/// while a capture is active.
#[cfg(feature = "std")]
fn push_capture(snap: [u8; 0x19]) {
    CAPTURE.with(|c| c.borrow_mut().push(snap));
}

// Diagnostic register capture (mirrors `sidplay --regdump` for a synth-vs-oracle
// frame-by-frame diff). THREAD-LOCAL on purpose: the capture is driven on the
// same thread that renders, and `cargo test` runs tests concurrently — a global
// buffer would let one test's SID frames pollute another's capture (the oracle
// golden faux-fails when it shares a buffer with the other SID renders). Per-
// thread state makes each render independent. No atomics/Mutex needed since each
// thread touches only its own cells.
#[cfg(feature = "std")]
thread_local! {
    static CAPTURE_ON: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
    static CAPTURE: core::cell::RefCell<Vec<[u8; 0x19]>> = const { core::cell::RefCell::new(Vec::new()) };
}

/// Enable/disable + reset the diagnostic register capture (current thread).
#[cfg(feature = "std")]
#[doc(hidden)]
pub fn capture_begin() {
    CAPTURE.with(|c| c.borrow_mut().clear());
    CAPTURE_ON.with(|f| f.set(true));
}

/// Stop and return the captured per-frame register snapshots (current thread).
#[cfg(feature = "std")]
#[doc(hidden)]
pub fn capture_take() -> Vec<[u8; 0x19]> {
    CAPTURE_ON.with(|f| f.set(false));
    CAPTURE.with(|c| core::mem::take(&mut *c.borrow_mut()))
}

/// Bridge a decoded tracker [`SidVoice`] into the generator's neutral
/// [`SidVoicePatch`] (the SID equivalent of OPL's `channel_patch_from_instr`).
pub(crate) fn patch_from_voice(v: &SidVoice) -> SidVoicePatch {
    let waveform = (v.ctrl_triangle as u8)
        | ((v.ctrl_sawtooth as u8) << 1)
        | ((v.ctrl_pulse as u8) << 2)
        | ((v.ctrl_noise as u8) << 3);
    SidVoicePatch {
        waveform,
        pulse_width: v.pw & 0x0FFF,
        attack_decay: v.ad,
        sustain_release: v.sr,
        ring: v.ctrl_rm,
        sync: v.ctrl_sync,
        test: v.ctrl_test,
        gate: v.ctrl_gate,
    }
}

/// Bridge the decoded [`RobEffects`] into the generator's neutral [`SidFx`].
/// The decoder never sets the `pw` flag (only `pw_speed` / `pw_delay`), so the
/// sweep is activated by a non-zero speed — mirroring how `vibrato` is keyed on
/// a non-zero depth.
pub(crate) fn fx_from_robeffects(re: &RobEffects) -> SidFx {
    SidFx {
        pw_sweep: re.pulse_sweep.enable || re.pulse_sweep.speed != 0,
        pw_speed: re.pulse_sweep.speed,
        pw_delay: re.pulse_sweep.delay,
        pw_low_byte_inc: re.pulse_sweep.low_byte_mode,
        // Flatten the musical `Option<BounceRange>` into the engine's neutral
        // triple (a 0 bound is real, so the `set` flag carries presence).
        pw_hi_bound: re.pulse_sweep.bounce.map_or(0, |b| b.hi),
        pw_lo_bound: re.pulse_sweep.bounce.map_or(0, |b| b.lo),
        pw_bounds_set: re.pulse_sweep.bounce.is_some(),
        pw_reseed_on_note: re.pulse_sweep.reseed_on_note,
        vib_len_gate: re.vibrato.length_gate,
        vibrato: re.vibrato.enable,
        vib_depth: re.vibrato.depth,
        vib_div: re.vibrato.speed_div,
        vib_tempvdif_reg: re.compat.vib_overflow_reg,
        arp: matches!(re.arpeggio, ArpMode::Octave),
        interp_vib: re.interp.enable,
        interp_half_depth: re.interp.half_depth,
        interp_shift: re.interp.shift,
        wave_attack_ctrl: re.two_phase.attack_ctrl,
        wave_attack_frames: re.two_phase.attack_frames,
        wave_attack_note: re.two_phase.attack_note.unwrap_or(0),
        wave_attack_note_set: re.two_phase.attack_note.is_some(),
        wave_alt_ctrl: re.wave_alt.alt_ctrl,
        arp2_reg: if let ArpMode::TwoNoteFixed(r) = re.arpeggio {
            r
        } else {
            0
        },
        arp3: matches!(re.arpeggio, ArpMode::ThreeNote { .. }),
        arp3_off_a: if let ArpMode::ThreeNote { off_a, .. } = re.arpeggio {
            off_a
        } else {
            0
        },
        arp3_off_b: if let ArpMode::ThreeNote { off_b, .. } = re.arpeggio {
            off_b
        } else {
            0
        },
        // Note-increment skydive: the skydive bit with a zero freq-add config
        // (the importer's signal that this engine sweeps by note number, e.g.
        // Thrust, rather than adding a constant to the freq register).
        skydive_climb: re.skydive.enable && matches!(re.skydive.mode, SkydiveMode::Climb),
        // Freq-add skydive: the skydive bit with a non-zero per-tune delta
        // (commando +512, monty/crazy −256, spellbound +256).
        skydive_add: if let (true, SkydiveMode::Add(a)) = (re.skydive.enable, re.skydive.mode) {
            a
        } else {
            0
        },
        skydive_when: re.skydive.length_gate,
        drum: re.drum.enable,
        filter: re.filter.enable,
        filter_resfilt: (re.filter.resonance << 4) | re.filter.routing,
        filter_step: re.filter.cutoff_step,
        filter_seed: re.filter.cutoff_seed,
        filter_reseed_each_note: re.filter.reseed_each_note,
        alt_arp_semitones: if let ArpMode::TwoNoteInterval(n) = re.arpeggio {
            n
        } else {
            0
        },
        drum_no_freq_slide: re.drum.no_freq_slide,
        zero_adsr_on_note_off: re.hard_cut_release,
    }
}

/// The SID driving model the player owns. The **per-instrument** [`SidBank`] is
/// the default (one chip per instrument); the **coupled** [`CoupledSid`](super::coupled::CoupledSid) is used
/// only for songs whose voices hard-sync / ring-modulate each other (so the
/// three SID voices must share one chip — see `coupled.rs`). The player selects
/// the variant once, at construction, from the song's instruments.
pub enum SidDriver {
    PerInstrument(SidBank),
    Coupled(Box<super::coupled::CoupledSid>),
}

impl SidDriver {
    pub fn begin_frame(&mut self) {
        match self {
            Self::PerInstrument(b) => b.begin_frame(),
            Self::Coupled(c) => c.begin_frame(),
        }
    }

    pub fn note_on(
        &mut self,
        channel: usize,
        instr: usize,
        voice: &SidVoice,
        fx: &RobEffects,
        milli_hz: u32,
    ) {
        match self {
            Self::PerInstrument(b) => b.note_on(channel, instr, voice, fx, milli_hz),
            Self::Coupled(c) => {
                let patch = patch_from_voice(voice);
                // The driver honours `re.drum.enable` directly (the importer only sets
                // it where the drum is register-exact, so it is its own gate).
                let sfx = fx_from_robeffects(fx);
                c.note_on(channel, instr, &patch, &sfx, milli_hz);
            }
        }
    }

    pub fn advance_fx(&mut self, channel: usize) {
        match self {
            Self::PerInstrument(b) => b.advance_fx(channel),
            Self::Coupled(c) => c.advance_fx(channel),
        }
    }

    pub fn set_frequency(&mut self, channel: usize, milli_hz: u32) {
        match self {
            Self::PerInstrument(b) => b.set_frequency(channel, milli_hz),
            Self::Coupled(c) => c.set_frequency(channel, milli_hz),
        }
    }

    pub fn note_off(&mut self, channel: usize, is_fetch: bool) {
        match self {
            Self::PerInstrument(b) => b.note_off(channel, is_fetch),
            Self::Coupled(c) => c.note_off(channel, is_fetch),
        }
    }

    pub fn note_cut(&mut self, channel: usize) {
        match self {
            Self::PerInstrument(b) => b.note_cut(channel),
            Self::Coupled(c) => c.note_cut(channel),
        }
    }

    pub fn any_active(&self) -> bool {
        match self {
            Self::PerInstrument(b) => b.any_active(),
            Self::Coupled(c) => c.any_active(),
        }
    }

    pub fn mix_frame(&mut self) -> i32 {
        match self {
            Self::PerInstrument(b) => b.mix_frame(),
            Self::Coupled(c) => c.mix_frame(),
        }
    }

    pub fn capture_frame(&self) {
        match self {
            Self::PerInstrument(b) => b.capture_frame(),
            #[cfg(feature = "std")]
            Self::Coupled(c) => {
                if CAPTURE_ON.with(|f| f.get()) {
                    push_capture(c.snapshot());
                }
            }
            #[cfg(not(feature = "std"))]
            Self::Coupled(_) => {}
        }
    }
}
