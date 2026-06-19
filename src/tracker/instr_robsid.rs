//! [`InstrRobSid`] — Rob-Hubbard-style C64 instrument: an
//! [`crate::tracker::instr_sid::SidVoice`] plus the per-tick effect tables (waveform
//! cycling, pulse-width sweep, frequency arpeggio, vibrato) that
//! drove Hubbard's signature sound.
//!
//! Opaque to the xmrs sample engine: the player skips voices whose
//! `instr_type` is `InstrumentType::RobSid`. Populated by the SID
//! importer; only compiled when `import_sid` is enabled.

use serde::{Deserialize, Serialize};

use crate::tracker::instr_sid::SidVoice;

/// LFO vibrato.
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug)]
pub struct Vibrato {
    /// Master enable.
    pub enable: bool,
    /// Pitch-swing amplitude in raw freq-register steps per half-cycle.
    pub depth: u8,
    /// Speed divider: larger = slower LFO (frames between steps).
    pub speed_div: u8,
    /// Gate the vibrato on note length (v30: suppress on short/staccato notes).
    pub length_gate: bool,
}

/// Period-interpolation vibrato — a second, wider triangle pitch swing
/// (`play $09f1`, instr byte +5), separate from the LFO [`Vibrato`].
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug)]
pub struct InterpVibrato {
    /// Master enable (off when `half_depth == 0`).
    pub enable: bool,
    /// Half-depth of the swing, in semitone-period steps (each `>> shift`).
    pub half_depth: u8,
    /// Right-shift applied to each step (smaller swing as it grows).
    pub shift: u8,
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

/// Arpeggio flavour (mutually exclusive — a given instrument uses one).
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub enum ArpMode {
    /// No arpeggio.
    #[default]
    None,
    /// Octave arpeggio (`instrfx & 4`): alternate note / note+12 every frame.
    Octave,
    /// v30 two-note arp: toggle every other frame between the struck note and a
    /// fixed second note (raw PAL SID frequency register; `play $c17f`/`$c226`).
    TwoNoteFixed(u16),
    /// v15/v20 two-note arp: toggle the struck note down by N semitones on
    /// alternate frames.
    TwoNoteInterval(u8),
    /// v20/v25 three-note arpeggio (fxmask bit4, `0x10`; Ghidra lightforce
    /// `play $f0bf`, `DAT_f5f2 & 0x10`): a global, phase-locked 3-frame loop
    /// shifting the struck note by `0, off_a, off_b` SIGNED semitones (the
    /// played note plus two arbitrary semitone offsets). The offset pair comes
    /// from a per-tune master table selected by `fx_v2[i][0]`; the importer
    /// resolves and bakes the two offsets here (lightforce `$f60c` holds
    /// downward pairs, e.g. `(-4,-7)`). It is the direct generalisation of the
    /// octave / two-note arps — NOT a separate vibrato-table type.
    ThreeNote { off_a: i8, off_b: i8 },
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
    /// Attack control byte (waveform + sync/ring/test, e.g. pulse+sync `0x43`).
    #[serde(default)]
    pub attack_ctrl: u8,
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

/// Per-frame waveform alternation: toggle this control register with the
/// instrument waveform every frame (v30 `play $c14a`). 0 ⇒ off.
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug)]
pub struct WaveAlt {
    #[serde(default)]
    pub alt_ctrl: u8,
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
    /// Voice-routing bits, 0..=15 (`$D417` low nibble: voice 1/2/3 + external).
    #[serde(default)]
    pub routing: u8,
    /// `$D416` per-frame signed cutoff-hi increment (0 = static cutoff).
    #[serde(default)]
    pub cutoff_step: i8,
    /// Cutoff-hi loaded on note-on (accumulator seed). ace_2=48, lightforce=0.
    #[serde(default)]
    pub cutoff_seed: u8,
    /// Re-seed the cutoff each note-on (v30) instead of continuing the sweep.
    #[serde(default)]
    pub reseed_each_note: bool,
}

/// Replayer-compatibility values that are NOT user-facing musical effects: the
/// importer sets them to faithfully reproduce a specific `.sid` replayer's
/// behaviour, but a musician would never dial them in. An instrument editor
/// should hide this (an "advanced / compatibility" fold), not expose it as a
/// knob. Ideally empty; reality keeps the one irreducible case below.
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug)]
pub struct ReplayerCompat {
    /// Absolute per-osc-step frequency-register excursion the replayer applies
    /// to an OVERFLOW note (a note byte > 95 whose vibrato `tempvdif` is read
    /// past `RH_FREQ_HEX` into work-RAM = garbage). Set by the importer from
    /// `one_sid.rs::overflow_vib` (only monty instr 12 = 1344). 0 = use the
    /// normal note-relative semitone vibrato. It cannot live in [`Vibrato`] (it
    /// is a fixed register step, note-independent, not a musical depth) nor be
    /// baked into the period/lane system (the SID effect is per-instrument in
    /// register space) — so it stays here as a runtime carrier, import-owned.
    #[serde(default)]
    pub vib_overflow_reg: u16,
}

/// Rob Hubbard per-frame effect set for one voice, grouped by musical function.
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug)]
pub struct RobEffects {
    pub vibrato: Vibrato,
    pub interp: InterpVibrato,
    pub pulse_sweep: PulseSweep,
    pub arpeggio: ArpMode,
    pub drum: Drum,
    pub skydive: Skydive,
    pub two_phase: TwoPhase,
    pub wave_alt: WaveAlt,
    pub filter: Filter,
    /// Cut AD+SR at note-off so the note ends sharp instead of ringing out on the
    /// release nibble (v15 `$e198`). (Reframe → `ReleaseMode` enum is step 5.)
    #[serde(default)]
    pub hard_cut_release: bool,
    /// Non-musical replayer-compat carriers (importer-set; hidden from the UI).
    #[serde(default)]
    pub compat: ReplayerCompat,
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
    /// The per-frame effect set applied on top of `voice`.
    pub fx: RobEffects,
}
