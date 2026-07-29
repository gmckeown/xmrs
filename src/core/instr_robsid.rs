//! [`InstrRobSid`](crate::core::instr_robsid::InstrRobSid) —
//! Rob-Hubbard-style C64 instrument: an
//! [`crate::core::instr_sid::SidVoice`] plus the per-tick effect tables (waveform
//! cycling, pulse-width sweep, frequency arpeggio, vibrato) that
//! drove Hubbard's signature sound.
//!
//! Opaque to the xmrs sample engine: the player skips voices whose
//! `instr_type` is `InstrumentType::RobSid`.
//!
//! **Always compiled**, in every feature configuration. It is a data type —
//! integer-only, no emulator dependency — and only its render engine
//! (`generators::sid`) gates on `synth_sid`. That is deliberate: the
//! `.xmr` `INST` chunk carries `InstrumentType` through the model's own
//! `Serialize`, so a variant that came and went with a Cargo feature would make
//! the *file format* depend on how the writer was built (`FORMAT_RFC.md` §5.1).
//! A build with no importer and no synthesiser still round-trips one of these.

use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::core::instr_sid::SidVoice;

/// The waveform (and its modifiers) one [`WaveStep`] selects — the same seven
/// control bits [`SidVoice`] exposes, so an instrument editor can reuse the very
/// same widget for the base patch and for a program step.
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub struct WaveShape {
    pub noise: bool,
    pub pulse: bool,
    pub sawtooth: bool,
    pub triangle: bool,
    /// Freezes the oscillator at zero — silence, or the reset a percussive
    /// attack needs before its next step.
    pub test: bool,
    /// Ring-modulate with the neighbouring hardware voice.
    pub ring: bool,
    /// Hard-sync to the neighbouring hardware voice.
    pub sync: bool,
    /// Envelope gate. A step that clears it releases the note **mid-program** —
    /// how a drum decays while the program keeps running.
    pub gate: bool,
}

impl WaveShape {
    /// Unpack a raw `$D404` control byte.
    pub fn from_ctrl(b: u8) -> Self {
        Self {
            noise: b & 0x80 != 0,
            pulse: b & 0x40 != 0,
            sawtooth: b & 0x20 != 0,
            triangle: b & 0x10 != 0,
            test: b & 0x08 != 0,
            ring: b & 0x04 != 0,
            sync: b & 0x02 != 0,
            gate: b & 0x01 != 0,
        }
    }

    /// Whether this shape makes no sound at all — every oscillator off.
    ///
    /// The replayers spell "this feature is unused" as an all-zero control byte,
    /// so the model spells it as a shape that would be silent. An editor can
    /// show the same thing as an unticked row rather than a magic zero.
    pub fn is_silent(self) -> bool {
        !(self.noise || self.pulse || self.sawtooth || self.triangle)
    }

    /// Pack back into a raw `$D404` control byte.
    pub fn to_ctrl(self) -> u8 {
        (self.noise as u8) << 7
            | (self.pulse as u8) << 6
            | (self.sawtooth as u8) << 5
            | (self.triangle as u8) << 4
            | (self.test as u8) << 3
            | (self.ring as u8) << 2
            | (self.sync as u8) << 1
            | self.gate as u8
    }
}

/// What a [`WaveStep`] does to the pitch.
#[derive(Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub enum PitchAction {
    /// Bend the played note by adding this signed amount to the program's
    /// running pitch offset. The offset **accumulates** across steps: a run of
    /// `Bend` steps is a glide, a single one is a fixed detune, and `Bend(0)`
    /// simply holds the current pitch while the waveform changes.
    ///
    /// The unit is the raw SID frequency register, so the same value bends a
    /// high note less (in cents) than a low one — that is the replayer's own
    /// behaviour, and part of the character of these programs.
    Bend(i16),
    /// Ignore the played note and set the frequency register's HIGH byte
    /// outright. How a percussive step pins a noise burst to a fixed pitch
    /// regardless of which note was struck.
    Fixed(u8),
}

impl Default for PitchAction {
    fn default() -> Self {
        Self::Bend(0)
    }
}

/// One frame of a [`VoiceProgram`].
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub struct WaveStep {
    /// The waveform this frame.
    pub shape: WaveShape,
    /// What the pitch does this frame.
    pub pitch: PitchAction,
}

/// What happens once the last step has played.
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub enum ProgramEnd {
    /// Freeze on the last step for as long as the note is held. The usual
    /// ending: the timbre settles into a sustain.
    #[default]
    Hold,
    /// Jump back to `step` and keep cycling — a repeating texture (a trill, a
    /// rattle, an arpeggiated buzz).
    Loop { step: u8 },
}

/// A per-instrument program that drives the voice's **waveform and pitch, one
/// step per frame** (50 Hz) — Rob Hubbard's "wavetable".
///
/// Instead of "this instrument is a sawtooth with an envelope", the instrument
/// says: *frame 1 → noise pinned at this pitch; frame 2 → pulse bending down;
/// frame 3 → hold*. That is how the C64's three voices produce drums, metallic
/// attacks and composite timbres: the timbre is **programmed frame by frame**,
/// not chosen from a list.
///
/// It is the general form of three effects the older replayers hard-wired:
/// [`Drum`] (noise, then gate off), [`WaveAlt`] (two waveforms alternating) and
/// [`TwoPhase`] (attack waveform, then sustain waveform) are each a two- or
/// three-step program. Those stay in the model because the older tunes must
/// import register-exact, but an instrument that carries a `VoiceProgram` needs
/// none of them — an editor should present the program as the primary timbre
/// control and fold the three legacy knobs away.
///
/// While a program runs it OWNS the voice's waveform and pitch: the per-frame
/// effects that would otherwise set them do not apply.
#[derive(Default, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct VoiceProgram {
    /// The steps, one consumed per frame, starting at 0 on every note-on.
    pub steps: Vec<WaveStep>,
    /// Where the program goes after the last step.
    pub end: ProgramEnd,
}

/// Which vibrato LAW a voice runs. Rob Hubbard's replayers carry three
/// genuinely different pitch LFOs — not one LFO with different settings — so
/// this is an enum rather than a bag of flags: an instrument runs exactly one.
///
/// The distinction that matters musically is the **unit of the swing**: the two
/// register laws move the frequency register by a fixed amount, so their depth
/// in cents shrinks as the note rises; [`Self::Semitone`] moves by a fraction of
/// a semitone at the current note, so it is constant in cents. That is why
/// [`Self::Upward`] / [`Self::Bipolar`] cannot be re-expressed as
/// [`Self::Semitone`] (nor the reverse) — see `ROBSID2_INSTRUMENT_MODEL.md` §11.2.
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub enum VibratoMode {
    /// No vibrato.
    #[default]
    None,
    /// v10 / v20 / v25 upward LFO: the replayer's `oscilatval` triangle
    /// `0,1,2,3,3,2,1,0` over 8 frames of the GLOBAL counter, added to the note
    /// — the pitch never goes below the struck note, it only bulges upward.
    /// `depth` sets the per-step right-shift of the excursion.
    Upward {
        depth: u8,
        /// Fixed freq-register excursion per triangle step, instead of the
        /// note-relative semitone one. Non-zero only for an OVERFLOW note (a
        /// note byte > 95, whose vibrato `tempvdif` the replayer reads past
        /// `RH_FREQ_HEX` into work RAM — Monty instrument 12 → 1344). It is a
        /// second *unit* for the same law, which is why it lives here rather
        /// than in a compatibility side-car.
        reg_step: u16,
    },
    /// v30 (Delta) centred LFO: a triangle of period `2·depth` frames swinging
    /// symmetrically around the note, each step `semitone >> div`. Always gated
    /// on note length — the replayer skips it on short/staccato notes
    /// (Ghidra `dovib $c04c`: skip when `(flags & 0x1f) < 3`).
    Bipolar { depth: u8, div: u8 },
    /// Period-interpolation law (v15 Spellbound `sb_play $e012`, Thrust
    /// `play $09f1`, and the last player's `+5` / marker `0x86`): a triangle
    /// swinging symmetrically around the note by `±half_depth` steps, each step
    /// one semitone at the current note `>> shift`. Constant in cents.
    ///
    /// Two timing details of the same law, both `0` on the replayers that lack
    /// them (so an instrument that never used them is unchanged):
    /// - `delay`: frames to wait after note-on before the vibrato runs AT ALL —
    ///   the note is held dead straight first, and the ping-pong counter does
    ///   not advance either.
    /// - `flat`: once running, frames during which only the CENTRING applies
    ///   and not the swing. The pitch therefore sits half a swing below the note
    ///   for that moment — audible as a slight scoop into the vibrato, and part
    ///   of the sound. The last player uses 4.
    Semitone {
        half_depth: u8,
        shift: u8,
        delay: u8,
        flat: u8,
    },
}

/// Inclusive bounce range for the pulse-width sweep, on the PW **high nibble**
/// (each bound 0..=15). The sweep climbs until the high nibble reaches `hi`,
/// then falls until it reaches `lo`, then reverses again — Hubbard's "breathing"
/// pulse. Taken from the instrument's `fx_v2[i]+5` nibbles (hi nibble = `hi`, lo
/// nibble = `lo`). A bound of 0 is legitimate (e.g. Sanxion voice 1 sweeps 0..2),
/// which is why this is an [`Option`] on [`PulseSweep`] rather than a sentinel.
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub struct BounceRange {
    /// Lower bound on the PW high nibble (0..=15).
    pub lo: u8,
    /// Upper bound on the PW high nibble (0..=15).
    pub hi: u8,
}

/// Pulse-width sweep — Hubbard's signature moving "fat" pulse. Pulse waveform only.
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug)]
pub struct PulseSweep {
    /// Master enable.
    pub enable: bool,
    /// Signed step added to the 12-bit PW each tick (sign = direction; bounces at
    /// the high-byte bounds). In `low_byte_mode`, the full per-frame step.
    pub speed: i8,
    /// Frames between two steps (speed divider; larger = slower).
    pub delay: u16,
    /// Explicit bounce range on the PW high nibble (the `fx_v2[i]+5` nibbles).
    /// `None` ⇒ the legacy fixed 0x08..=0x0E range (v10/v15 and the v20 tunes
    /// without an `fx_v2` table). `Some` carries the real bounds, including a 0
    /// lower bound — which is why this is an [`Option`] and not a value-0 sentinel.
    #[serde(default)]
    pub bounce: Option<BounceRange>,
    /// Low-byte-only increment mode (v10 `instrfx & 8`): add `speed` to the PW
    /// LOW byte each frame, 8-bit wrap, no bounce/delay (commando/zoids voice 2).
    #[serde(default)]
    pub low_byte_mode: bool,
    /// Restart the sweep from the patch base PW on every note-on, instead of
    /// free-running continuously across notes. The v20/v25/v30 replayers keep the
    /// swept value in per-voice RAM and reload the instrument's base PW on each
    /// note-setup (`$f618,X` for Lightforce); the v10 routine instead mutates the
    /// instrument table in place, so its sweep persists (commando/zoids). Set by
    /// the importer from the replayer version.
    #[serde(default)]
    pub reseed_on_note: bool,
}

/// Where an [`ArpMode::Cycle`] reads its phase.
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub enum ArpPhase {
    /// The replayer's single global VBlank counter, shared by every voice — so
    /// two voices arpeggiating together stay phase-locked to each other, and a
    /// note struck mid-cycle joins the cycle where it already is. This is what
    /// the v15/v20/v25 replayers do (one counter in the player, not per voice).
    #[default]
    Global,
    /// Frames since *this* note was struck: the cycle restarts at step 0 on
    /// every note-on, independently per voice. This is what the last player's
    /// per-note arpeggio marker (`0x83`) does.
    PerNote,
}

/// Arpeggio flavour (mutually exclusive — a given instrument uses one).
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub enum ArpMode {
    /// No arpeggio.
    #[default]
    None,
    /// Octave arpeggio (`instrfx & 4`): alternate the struck note and its exact
    /// frequency DOUBLE every frame.
    ///
    /// Deliberately NOT a [`Self::Cycle`] with a `+12` step: the replayer reads
    /// an octave-doubled frequency table, so the ratio is exactly 2, whereas
    /// `Cycle` shifts by equal-tempered semitones (a Q16 repeated multiply that
    /// lands ~0.02 % short of 2× after twelve steps). Keeping the two apart is
    /// the register-exact choice, not a modelling accident.
    Octave,
    /// v30 two-note arp: toggle every other frame between the struck note and a
    /// fixed second note (raw PAL SID frequency register; `play $c17f`/`$c226`).
    ///
    /// The one arpeggio that is NOT expressible as a semitone cycle: it names an
    /// absolute frequency register, off the note grid.
    TwoNoteFixed(u16),
    /// Cyclic semitone-offset arpeggio — the general form.
    ///
    /// Unifies the v15/v20 two-note drop (`steps = [-n, 0]`, `len = 2`), the
    /// v20/v25 three-note arp (fxmask bit4, `steps = [0, off_a, off_b]`,
    /// `len = 3`, offsets SIGNED, from a per-tune master table selected by
    /// `fx_v2[i][0]` — lightforce `$f60c` holds downward pairs such as
    /// `(-4, -7)`), and the last player's per-note nibble arp (`steps =
    /// [0, N>>4, N&0x0F]`, unsigned, [`ArpPhase::PerNote`]).
    Cycle {
        /// Semitone offsets applied in order; `steps[0]` is the phase-0 offset.
        /// Only the first `len` entries are used.
        steps: [i8; 3],
        /// Active cycle length in frames (2 or 3).
        len: u8,
        /// Phase source — see [`ArpPhase`]. Same arithmetic on both sides, but
        /// this is where the two families genuinely differ.
        phase: ArpPhase,
        /// Apply the offsets on top of the note-increment skydive's climbed
        /// pitch instead of the struck note (v15: the lead's arp rides the
        /// climb). No effect unless [`Skydive`] is enabled in `Climb` mode.
        follow_climb: bool,
    },
}

/// Percussive-hit mode (`instrfx & 1`): a noise frame, then gate-off and (v10) a
/// freq-high slide — a short drum hit instead of a sustained tone.
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug)]
pub struct Drum {
    /// Master enable.
    pub enable: bool,
    /// The drum owns only the control register (noise + gate-off), NO freq-high
    /// slide (v15 `$e2c6`; v10 slides). Set for v15.
    #[serde(default)]
    pub no_freq_slide: bool,
}

/// How the skydive moves the pitch.
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub enum SkydiveMode {
    /// Note-increment climb (the pitch climbs by note number each frame).
    #[default]
    Climb,
    /// Freq-add: every other frame slide the freq-register HIGH byte by
    /// `amount >> 8` (Commando +512, Monty −256); base note on alternate frames.
    Add(i16),
}

/// Pitch dive/climb on note-on.
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug)]
pub struct Skydive {
    /// Master enable.
    pub enable: bool,
    /// Length gate: only fires while the note length exceeds this
    /// (`skydive_v1_when`); 0 = always (Monty), 2 = Commando.
    pub length_gate: u8,
    /// Climb vs freq-add (see [`SkydiveMode`]).
    pub mode: SkydiveMode,
}

/// Two-phase waveform attack: play `attack_ctrl` for `attack_frames` after
/// note-on, then the patch sustain waveform (Lightforce's bright pulse+sync over
/// a triangle sustain). `attack_ctrl == 0` ⇒ off. (v20 `$f6c9`/`$f6cb`.)
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug)]
pub struct TwoPhase {
    /// The waveform the attack phase plays, before the instrument's own takes
    /// over. A silent shape ([`WaveShape::is_silent`]) means no waveform attack.
    ///
    /// Named oscillators rather than the replayer's raw control byte: an editor
    /// shows "pulse + sync", not `0x43`. The byte is rebuilt at the driver
    /// boundary with [`WaveShape::to_ctrl`], which is where register-shaped
    /// values belong.
    #[serde(default)]
    pub attack_shape: WaveShape,
    /// Frames the attack byte is held before the sustain waveform. Shared by the
    /// waveform attack (bit2) and the pitch attack (bit6, [`Self::attack_note`]):
    /// when an instrument sets BOTH the replayer's shared attack counter is
    /// decremented by each active bit, so the unified attack lasts `frames / 2`
    /// (Ghidra sanxion `$be20`, the `0x44` case).
    #[serde(default)]
    pub attack_frames: u8,
    /// Two-phase **pitch** attack (fxmask bit6, `0x40`; Ghidra sanxion `$be20`,
    /// `DAT_b548 & 0x40`): an ABSOLUTE note index (0..=95, the replayer's
    /// `freq_table[note*2]`) the voice plays for the attack phase before dropping
    /// to the struck note — a fixed-pitch transient (e.g. a percussive click)
    /// regardless of the melodic note. The PITCH twin of the bit2 waveform
    /// attack, sharing [`Self::attack_frames`]. `None` ⇒ no pitch attack.
    #[serde(default)]
    pub attack_note: Option<u8>,
}

/// Per-frame waveform alternation: every frame the voice flips between the
/// instrument's own waveform and this one (v30 `play $c14a`) — a buzz or a
/// rasp rather than a steady tone.
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug)]
pub struct WaveAlt {
    /// The alternate waveform. A silent shape ([`WaveShape::is_silent`]) means
    /// no alternation, and the voice holds its own waveform.
    #[serde(default)]
    pub alt_shape: WaveShape,
}

/// SID global resonant low-pass (coupled driver only; see SID_FILTER_PLAN.md and
/// the Ghidra reference). All-zero ⇒ no filter.
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug)]
pub struct Filter {
    /// This instrument drives / routes through the SID filter.
    pub enable: bool,
    /// Resonance amount, 0..=15 (`$D417` high nibble).
    #[serde(default)]
    pub resonance: u8,
    /// Which voices this filter is placed across.
    ///
    /// Explicit in v10–v30, where the instrument names the routing outright. The
    /// last player instead DERIVES it — the mask is the OR of the voice bits of
    /// every voice whose instrument enables the filter — so a G2 import leaves
    /// this at the voice's own bit.
    #[serde(default)]
    pub routing: FilterRouting,
    /// Cutoff loaded on note-on: high byte in the top 8 bits. v10–v30 seed only
    /// the high byte (ace_2 = 48<<8, lightforce = 0); the last player seeds both.
    #[serde(default)]
    pub cutoff_seed: u16,
    /// How the cutoff moves each frame — see [`CutoffSweep`].
    #[serde(default)]
    pub sweep: CutoffSweep,
    /// Which bands the filter passes. v10–v30 hardwire low-pass; the last
    /// player reads it from the filter slot's byte +6, bits 0-2.
    #[serde(default)]
    pub mode: FilterMode,
    /// Re-seed the cutoff each note-on (v30) instead of continuing the sweep.
    #[serde(default)]
    pub reseed_each_note: bool,
}

/// Which voices run through the SID's one filter. The chip has a single filter
/// shared by all three voices plus its external input, and each is switched in
/// or out independently — so this is four switches, not a choice.
///
/// The `$D417` low-nibble packing happens at the driver boundary
/// ([`Self::to_bits`]).
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub struct FilterRouting {
    pub voice1: bool,
    pub voice2: bool,
    pub voice3: bool,
    /// The chip's external audio input (`$D417` bit 3). Unused by these tunes.
    pub external: bool,
}

impl FilterRouting {
    /// Unpack the `$D417` low nibble.
    pub fn from_bits(b: u8) -> Self {
        Self {
            voice1: b & 0x01 != 0,
            voice2: b & 0x02 != 0,
            voice3: b & 0x04 != 0,
            external: b & 0x08 != 0,
        }
    }

    /// Pack back into the `$D417` low nibble.
    pub fn to_bits(self) -> u8 {
        self.voice1 as u8
            | (self.voice2 as u8) << 1
            | (self.voice3 as u8) << 2
            | (self.external as u8) << 3
    }
}

/// Which bands the SID's one filter lets through. They combine — low + band is
/// a common Hubbard setting — so this is three switches rather than a choice of
/// one, and all three off means the filter passes nothing.
///
/// The chip spells it as three bits of the `$D418` high nibble; that packing
/// happens at the driver boundary ([`Self::to_bits`]), not here.
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub struct FilterMode {
    pub low_pass: bool,
    pub band_pass: bool,
    pub high_pass: bool,
}

impl FilterMode {
    /// Unpack the `$D418` mode bits (1 = low, 2 = band, 4 = high).
    pub fn from_bits(b: u8) -> Self {
        Self {
            low_pass: b & 0x01 != 0,
            band_pass: b & 0x02 != 0,
            high_pass: b & 0x04 != 0,
        }
    }

    /// Pack back into the `$D418` mode bits.
    pub fn to_bits(self) -> u8 {
        self.low_pass as u8 | (self.band_pass as u8) << 1 | (self.high_pass as u8) << 2
    }

    /// The plain low-pass every v10..v30 tune uses.
    pub fn low() -> Self {
        Self {
            low_pass: true,
            ..Default::default()
        }
    }
}

/// How a filter's cutoff moves. Two genuinely different laws, like
/// [`VibratoMode`] — an instrument runs one, and neither is a parameterisation
/// of the other: v10–v30 accumulate a SIGNED byte that wraps and never turns
/// round, the last player accumulates an UNSIGNED 16-bit magnitude that bounces
/// between two bounds.
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub enum CutoffSweep {
    /// The cutoff stays at [`Filter::cutoff_seed`].
    #[default]
    Static,
    /// v10–v30 (`$D416` per-frame increment): add a SIGNED step to the cutoff
    /// HIGH byte each frame, wrapping at 8 bits. No bounds, no turn-around —
    /// the sweep climbs (or falls) and wraps past the end of the range.
    Wrap { step: i8 },
    /// Last player: a 16-bit accumulator that BOUNCES between `min` and `max`
    /// (compared on the high byte), reversing direction at each bound. `up` is
    /// the direction it starts in, from the filter slot's byte +6 bit 7.
    Bounce {
        step: u16,
        min: u8,
        max: u8,
        up: bool,
    },
}

/// How a note ends. The third axis (with [`VibratoMode`] and [`ArpMode`]) where
/// the replayer generations genuinely disagree rather than merely differing in
/// settings — so an instrument picks one, and none of them can be a silent
/// default layered on top of another.
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub enum ReleaseMode {
    /// Clear the gate at note-off and let the patch's release nibble ring out
    /// (v20/v25/v30 — Delta's notes decay on their own release).
    #[default]
    Gate,
    /// v10 / v15 (`sb_play $e198`): zero AD+SR at note-off as well, so the note
    /// cuts sharp instead of ringing out on its release nibble. Verified per
    /// version against the sidplay regdump (the oracle writes 0x00 to
    /// `$d405`/`$d406` at note-end).
    HardCut,
    /// The last player: as a note approaches its end the replayer OVERWRITES the
    /// instrument's envelope with a fixed pair, so every note finishes the same
    /// way whatever patch played it.
    ///
    /// The pair is a per-tune constant compiled into the player, not song data
    /// and not derived from the instrument: Lion_Heart ends on `AD=$0F, SR=$01`,
    /// Sun_Never_Shines on `AD=$FF, SR=$F0`. WHEN it fires is a property of the
    /// NOTE, not of the instrument — a couple of frames before the note ends,
    /// while it is still gated — and travels on
    /// [`crate::core::effect::TrackEffect::NoteArming`]. Writing it at gate-off
    /// instead is not an approximation but a different effect: measured, it
    /// roughly triples the divergence.
    Ramp { ad: u8, sr: u8 },
}

/// Rob Hubbard per-frame effect set for one voice, grouped by musical function.
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug)]
pub struct RobEffects {
    /// The voice's pitch LFO — one law, see [`VibratoMode`].
    pub vibrato: VibratoMode,
    pub pulse_sweep: PulseSweep,
    pub arpeggio: ArpMode,
    pub drum: Drum,
    pub skydive: Skydive,
    pub two_phase: TwoPhase,
    pub wave_alt: WaveAlt,
    pub filter: Filter,
    /// How the note ends — see [`ReleaseMode`].
    #[serde(default)]
    pub release: ReleaseMode,
}

/// Rob Hubbard Generalized Instrument — a SINGLE SID voice patch plus its
/// per-frame effect set. (A Rob-Hubbard "instrument" is one voice; the chip's
/// three hardware voices come from three instruments on three channels, not
/// from one `InstrRobSid`.)
#[derive(Default, Serialize, Deserialize, Debug, Clone)]
pub struct InstrRobSid {
    /// The voice's starting register state (waveform / pulse / ADSR / sync /
    /// ring / test / gate).
    pub voice: SidVoice,
    /// An optional per-frame waveform + pitch program. When present it OWNS the
    /// timbre: it overrides `voice`'s waveform every frame and drives the pitch
    /// itself, so the legacy [`Drum`] / [`WaveAlt`] / [`TwoPhase`] knobs in `fx`
    /// are redundant and an editor should hide them. See [`VoiceProgram`].
    #[serde(default)]
    pub program: Option<VoiceProgram>,
    /// The per-frame effect set applied on top of `voice`.
    pub fx: RobEffects,
}

#[cfg(test)]
mod frozen_shape {
    use super::*;

    /// `InstrRobSid` is **frozen**, and this is the guard that holds in every
    /// build.
    ///
    /// Its serialised shape is the `.xmr` `INST` payload, because that chunk
    /// rides the model's own derive rather than a wire mirror
    /// (`FORMAT_RFC.md` §5.1). A renamed or retyped field therefore changes what
    /// a saved file means — and CBOR maps by NAME, with `#[serde(default)]`
    /// filling a field whose name moved, so an old file **loads quietly missing
    /// that effect** rather than being refused. Measured, not assumed: renaming
    /// `attack_ctrl` to `attack_shape` produced exactly that.
    ///
    /// Exhaustive destructuring is what makes this hold: adding, removing or
    /// renaming a field fails to COMPILE, and the type ascriptions catch a
    /// changed type. No feature, no codec, no test-runner flag can skip it —
    /// which the CBOR check below cannot say, since it needs `format`.
    ///
    /// If a field must change after all, that is a `schema_version` bump plus a
    /// migration (§6.2), not an edit to these patterns.
    ///
    /// **One known tension.** The three-marker SID replayer is still being
    /// decoded (`THREE_MARKER_GENERATION.md`), and its remaining error is a
    /// pitch one whose named suspect is the vibrato law. If that law turns out
    /// not to fit [`VibratoMode`]'s existing variants, it needs a NEW variant —
    /// which is additive for CBOR (old files still read; new files simply
    /// require a newer reader, i.e. a `min_reader_version` matter) and so does
    /// not break the freeze. Changing an existing variant's fields would.
    #[test]
    fn field_names_and_types_are_frozen() {
        let InstrRobSid { voice, program, fx } = InstrRobSid::default();
        let _: SidVoice = voice;
        let _: Option<VoiceProgram> = program;

        let RobEffects {
            vibrato,
            pulse_sweep,
            arpeggio,
            drum,
            skydive,
            two_phase,
            wave_alt,
            filter,
            release,
        } = fx;
        let _: VibratoMode = vibrato;
        let _: ArpMode = arpeggio;
        let _: ReleaseMode = release;

        let TwoPhase {
            attack_shape,
            attack_frames,
            attack_note,
        } = two_phase;
        let _: WaveShape = attack_shape;
        let _: u8 = attack_frames;
        let _: Option<u8> = attack_note;

        let WaveAlt { alt_shape } = wave_alt;
        let _: WaveShape = alt_shape;

        let Filter {
            enable,
            resonance,
            routing,
            cutoff_seed,
            sweep,
            mode,
            reseed_each_note,
        } = filter;
        let _: bool = enable;
        let _: u8 = resonance;
        let _: FilterRouting = routing;
        // Register units, not Hz — frozen that way deliberately; a UI converts.
        let _: u16 = cutoff_seed;
        let _: CutoffSweep = sweep;
        let _: FilterMode = mode;
        let _: bool = reseed_each_note;

        let Skydive {
            enable,
            length_gate,
            mode,
        } = skydive;
        let _: bool = enable;
        // In ROWS, not frames — likewise deliberate.
        let _: u8 = length_gate;
        let _: SkydiveMode = mode;

        let PulseSweep {
            enable,
            speed,
            delay,
            bounce,
            low_byte_mode,
            reseed_on_note,
        } = pulse_sweep;
        let _: bool = enable;
        // Per FRAME.
        let _: i8 = speed;
        let _: u16 = delay;
        let _: Option<BounceRange> = bounce;
        let _: bool = low_byte_mode;
        let _: bool = reseed_on_note;

        let Drum {
            enable,
            no_freq_slide,
        } = drum;
        let _: bool = enable;
        let _: bool = no_freq_slide;

        let WaveShape {
            noise,
            pulse,
            sawtooth,
            triangle,
            test,
            ring,
            sync,
            gate,
        } = WaveShape::default();
        let _: [bool; 8] = [noise, pulse, sawtooth, triangle, test, ring, sync, gate];

        let FilterMode {
            low_pass,
            band_pass,
            high_pass,
        } = FilterMode::default();
        let _: [bool; 3] = [low_pass, band_pass, high_pass];

        let FilterRouting {
            voice1,
            voice2,
            voice3,
            external,
        } = FilterRouting::default();
        let _: [bool; 4] = [voice1, voice2, voice3, external];
    }
}

#[cfg(all(test, feature = "format"))]
mod freeze {
    use super::*;
    use alloc::vec::Vec;
    use ciborium::value::Value;

    /// `InstrRobSid` is **frozen**: its serialised shape is the `.xmr` `INST`
    /// payload, because that chunk rides the model's own derive rather than a
    /// wire mirror (`FORMAT_RFC.md` §5.1).
    ///
    /// The RFC defers byte-freezing checks until v1 is frozen, naming "a pending
    /// `RobSid` revision" as the very reason — so this is not that check. It is
    /// narrower and available now: it pins the FIELD NAMES of this one type, so
    /// that renaming or retyping a field goes red here instead of silently
    /// changing what a saved file means.
    ///
    /// Why names and not bytes: a rename is the dangerous edit. CBOR maps by
    /// name, and `#[serde(default)]` fills a field whose name has changed — so
    /// an old file LOADS, quietly missing that effect, rather than being
    /// refused. Measured, not assumed: renaming `attack_ctrl` to `attack_shape`
    /// produced exactly that, an attack silently lost.
    ///
    /// If a field must change after all, that is a `schema_version` bump plus a
    /// migration (§6.2) — not an edit to this list.
    #[test]
    fn serialised_field_names_are_frozen() {
        let mut buf = Vec::new();
        ciborium::into_writer(&InstrRobSid::default(), &mut buf).expect("encode");
        let v: Value = ciborium::from_reader(&buf[..]).expect("decode");

        let keys = |v: &Value| -> Vec<alloc::string::String> {
            match v {
                Value::Map(m) => m
                    .iter()
                    .filter_map(|(k, _)| match k {
                        Value::Text(t) => Some(t.clone()),
                        _ => None,
                    })
                    .collect(),
                _ => panic!("expected a map"),
            }
        };
        assert_eq!(keys(&v), ["voice", "program", "fx"]);

        let fx = match &v {
            Value::Map(m) => m
                .iter()
                .find(|(k, _)| k == &Value::Text("fx".into()))
                .unwrap()
                .1
                .clone(),
            _ => unreachable!(),
        };
        assert_eq!(
            keys(&fx),
            [
                "vibrato",
                "pulse_sweep",
                "arpeggio",
                "drum",
                "skydive",
                "two_phase",
                "wave_alt",
                "filter",
                "release",
            ]
        );

        // The four the curation pass renamed, and the units that stay as they
        // are because the type is now frozen: `cutoff_seed` in register units,
        // `speed` per frame, `length_gate` in rows.
        let group = |name: &str| -> Value {
            match &fx {
                Value::Map(m) => m
                    .iter()
                    .find(|(k, _)| k == &Value::Text(name.into()))
                    .unwrap()
                    .1
                    .clone(),
                _ => unreachable!(),
            }
        };
        assert_eq!(
            keys(&group("two_phase")),
            ["attack_shape", "attack_frames", "attack_note"]
        );
        assert_eq!(keys(&group("wave_alt")), ["alt_shape"]);
        assert_eq!(
            keys(&group("filter")),
            [
                "enable",
                "resonance",
                "routing",
                "cutoff_seed",
                "sweep",
                "mode",
                "reseed_each_note"
            ]
        );
        assert_eq!(keys(&group("skydive")), ["enable", "length_gate", "mode"]);
    }
}
