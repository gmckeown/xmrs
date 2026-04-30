use serde::{Deserialize, Serialize};

use crate::fixed::units::{ChannelVolume, Panning, Period, Volume};
use crate::instrument::Instrument;
use crate::mix_plugin::MixPlugins;
use crate::period_helper::FrequencyType;
use crate::prelude::TrackUnit;

use alloc::string::String;
use alloc::string::ToString;
use alloc::{vec, vec::Vec};

#[cfg(target_pointer_width = "16")]
pub const MAX_NUM_ROWS: usize = 255;

#[cfg(target_pointer_width = "32")]
pub const MAX_NUM_ROWS: usize = 4095;

#[cfg(target_pointer_width = "64")]
pub const MAX_NUM_ROWS: usize = 4095;

/// A row contains its column elements
pub type Row = Vec<TrackUnit>;

/// Patterns are sequences of lines
pub type Pattern = Vec<Row>;

/// Which tracker convention this module was imported from.
///
/// This is **metadata only** — it identifies the source format for
/// display, export, and editor UI purposes. Runtime playback
/// decisions are driven entirely by [`PlaybackQuirks`] on [`Module`],
/// which an editor can set independently of the format tag. This
/// lets a new module be authored with any combination of historical
/// quirks (or none at all) without having to lie about the format
/// it "came from".
///
/// Importers set two things: the `format` tag here (as metadata) and
/// the `quirks` struct (as behaviour). An editor creating a fresh
/// module from scratch typically leaves `format = Unknown` and
/// `quirks = PlaybackQuirks::default()` (all-off) for a clean,
/// quirk-free playback.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModuleFormat {
    /// Unknown / hand-constructed / non-tracker source.
    #[default]
    Unknown,
    /// ProTracker-style MOD (Amiga Soundtracker family).
    Mod,
    /// ScreamTracker III (S3M).
    S3m,
    /// Fasttracker II (XM).
    Xm,
    /// Impulse Tracker (IT), including OpenMPT's IT extensions.
    It,
}

impl ModuleFormat {
    /// `true` when this tag is `Xm`.
    #[inline]
    pub fn is_xm(&self) -> bool {
        matches!(self, ModuleFormat::Xm)
    }

    /// `true` when this tag is `S3m`.
    #[inline]
    pub fn is_s3m(&self) -> bool {
        matches!(self, ModuleFormat::S3m)
    }

    /// `true` when this tag is `It`.
    #[inline]
    pub fn is_it(&self) -> bool {
        matches!(self, ModuleFormat::It)
    }

    /// `true` when this tag is `Mod`.
    #[inline]
    pub fn is_mod(&self) -> bool {
        matches!(self, ModuleFormat::Mod)
    }
}

/// Per-module playback behaviour switches.
///
/// These are the **runtime** knobs the replayer consults to decide
/// how to render a module. Each knob is an orthogonal switch for a
/// single historical tracker behaviour, named for what it *does*
/// rather than which tracker it came from — so an editor can compose
/// any subset of them without pretending the output is a specific
/// historical format.
///
/// The `Default` impl is fully conservative: every quirk is off
/// (`false` / `None`). A module built from scratch by an editor plays
/// cleanly, without inheriting any historical bug or edge case. The
/// importers (XM, S3M, IT, MOD) enable the quirks that are canonical
/// for their source format.
///
/// Every comment below is structured as: *what the knob does* ·
/// *which format(s) enable it by default* · *the upstream reference
/// point*.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Default)]
pub struct PlaybackQuirks {
    // --- Period / pitch ---
    /// Reproduce FT2's signed-overflow quirk in portamento-down:
    /// periods in `[32000, 32767]` snap to `31999`, but periods that
    /// grow past `32768` pass through unchanged (the int16 cast in
    /// FT2's source flips negative, so its own clamp check fails).
    /// Some XM modules rely on this to produce very deep pitch
    /// slides past the audible range. XM on. ft2_replayer.c:1914
    /// (explicit "FT2 bug" marker).
    pub ft2_pitch_slide_overflow: bool,

    /// If `Some((min, max))`, pitch slides clamp the period to this
    /// inclusive range. Models ST3's `amigalimits` masterflag
    /// (`[113.25, 856]` in xmrs Amiga-period units, = ST3's
    /// `[453, 3424]` / 4). Leave `None` for the default
    /// `[1 or 0 .. 31999]` fallback. S3M sets it when masterflag
    /// bit 0x10 is on. dig.c:setmasterflags.
    /// Period clamp `(min, max)` in xmrs Amiga period units,
    /// stored as [`Period`] (Q-typed `u16` raw).
    pub period_clamp: Option<(Period, Period)>,

    /// When a pitch slide tries to make the period go below 1,
    /// allow it to reach 0 ("infinitely high note") instead of
    /// clamping at 1. S3M convention (the minimum period is
    /// effectively silent anyway). FT2/IT clamp at 1.
    pub allow_zero_period: bool,

    // --- Arpeggio ---
    /// Use FT2's historical `arpeggioTab` for arpeggio index
    /// selection — a reverse-tick LUT with hand-crafted holes
    /// that produces FT2's signature odd rhythm at high speeds.
    /// Off = plain `tick % 3` rotation. XM on. ft2_replayer.c
    /// `arpeggioTab` + the reverse `song.tick` semantics.
    pub ft2_arpeggio_lut: bool,

    /// When arpeggio is active, clamp the base note to `≤ 95` in
    /// `adjust_period` to keep the octave-perturbed lookup inside
    /// FT2's 0–95 note range. Paired with `ft2_arpeggio_lut`.
    /// XM on. ft2_replayer.c:arpNote → `period2NotePeriod`.
    pub ft2_arpeggio_note_clamp: bool,

    // --- Envelopes / note triggering ---
    /// A note-off on a channel whose instrument has no volume
    /// envelope CUTS the note (quick anti-click ramp to zero)
    /// instead of FADING via `volume_fadeout`. XM on; IT off (IT
    /// always fades). ft2_replayer.c:`keyOff`. Canonical XM
    /// playback — not a "bug" per se.
    pub keyoff_cuts_without_vol_env: bool,

    /// `K00` (note-off effect with param 0) at tick 0 swallows
    /// the note trigger on the same row — the note column is
    /// ignored and the channel just receives a keyoff. Only
    /// applies to `K00` (param 0); `K0y` with `y > 0` plays the
    /// note normally and the keyoff fires at tick `y`. XM on.
    /// ft2_replayer.c:`getNewNote`, early-return before
    /// `triggerNote`.
    pub k00_eats_note: bool,

    // --- Vibrato ---
    /// A vol-column vibrato-depth slot (FT2 `Bx`) without a
    /// main-effect 4xy on the same row still ticks the vibrato
    /// LFO on every non-zero tick. FT2's `v_Vibrato` calls
    /// `doVibrato` unconditionally after updating the depth.
    /// Without this, modules that drive vibrato primarily from
    /// the vol column leave the LFO frozen at its last position.
    /// XM on.
    pub volcol_b_advances_vibrato: bool,

    // --- Pattern loop ---
    /// An `E60` / `SB0` (set loop start) also leaks its row
    /// number into the song-level break position, so when the
    /// pattern ends naturally the NEXT pattern starts at that
    /// row instead of row 0. FT2 bug that some XM modules rely
    /// on. XM on. ft2_replayer.c:`patternLoop` → `pBreakPos`
    /// side-effect.
    pub e60_leaks_to_next_pattern: bool,

    /// A pattern loop that completes (`SBx` / `E6x` after `x`
    /// iterations) updates its origin to `current_row + 1`, so
    /// a subsequent loop in the same pattern resumes *after*
    /// the previous loop's end instead of re-using the original
    /// origin. S3M on. digcmd.c:s_patloop — `song.patloopstart
    /// = song.np_row + 1` at exit.
    pub pattern_loop_resumes: bool,

    // --- Tremor ---
    /// The tremor effect (I / T) uses a persistent state machine
    /// whose on/off counter carries across rows — consecutive
    /// rows of tremor form a single continuous cycle. Off = FT2
    /// per-row modular formula, which restarts the phase at
    /// every row. S3M on. digcmd.c:`s_tremor` (`atremor` +
    /// `atreon` both live on the channel struct, never reset
    /// at row load).
    pub tremor_state_persists: bool,

    // --- IT "old effects" (flags bit 4) ---
    /// IT modules carry a file-level "Old Effects" bit (ITTECH,
    /// Flags bit 4). When ON, several effects revert to their
    /// ST3-ish behaviour:
    ///   * Hxy/Uxy vibrato depth is doubled.
    ///   * Hxy/Uxy vibrato updates only on non-row ticks (like
    ///     FT2) instead of every tick.
    ///   * Ixy/Tremor `on_time` / `off_time` are effectively `+1`.
    ///   * Oxx past sample end plays from the END of the sample
    ///     instead of being ignored.
    ///
    /// IT importer sets this from the header flag; non-IT formats
    /// leave it `false`. The replayer consults it on every
    /// tick-zero effect-resolution step.
    pub it_old_effects: bool,

    /// IT flag bit 5: link Gxx (tone portamento) memory with Exx/Fxx
    /// (pitch slides) memory. When ON, the three effects share a
    /// single "last slide value" register per channel; also, `Gxx`
    /// when a new note is present retriggers the envelopes.
    /// IT importer sets this from the header.
    pub it_link_gxx_memory: bool,

    /// What to do with the channel's pan when a note retriggers.
    /// Each tracker generation made a different choice; see
    /// [`PanResetPolicy`] for the three options. Set by the
    /// importer to match the source tracker.
    pub pan_reset_policy: PanResetPolicy,

    /// IT 2.14+ in non-old-effects mode advances the vibrato LFO
    /// every tick, including tick 0 of a row. XM/MOD/S3M (and IT
    /// with old-effects on) only tick on non-row ticks. Read in
    /// conjunction with [`Self::it_old_effects`]: when the latter
    /// is on, this flag is meaningless because the IT old-effects
    /// path takes precedence. IT-only — set by `it214()` /
    /// `it215()`; off elsewhere.
    pub it_vibrato_ticks_at_row_zero: bool,
}

/// Per-format policy for the channel's pan when a note retriggers.
///
/// The three trackers we model made three different choices:
///
/// - **XM/MOD** (`Always`): the channel pan is reset to the sample's
///   `default_pan` on every retrigger. Pxx slides and pan envelopes
///   are wiped clean each note.
/// - **IT** (`OnInstrumentChange`): the pan resets only when the
///   cell carries an instrument-column change. Pure note retriggers
///   (ghost notes) inherit the channel's current pan, so in-flight
///   Pxx slides and pan envelopes survive.
/// - **S3M** (`Never`): the pan is pure channel state. Note
///   retriggers leave it alone — the channel's pan was set by the
///   header's `channel_settings` byte at song start, and only
///   `Xxx`/`Pxx` effects ever touch it. Schism's
///   `effects.c:249-253` only writes the instrument's pan when the
///   sample/instrument carries an explicit `CHN_PANNING` flag, which
///   in our model corresponds to never resetting from a plain note
///   retrigger.
#[derive(Default, Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum PanResetPolicy {
    /// Reset on every retrigger. XM/MOD/PT default.
    #[default]
    Always,
    /// Reset only when the cell has an instrument column. IT default.
    OnInstrumentChange,
    /// Never reset on retrigger. S3M default.
    Never,
}

/// Initial state of one channel, applied at song start before any
/// pattern row plays. Used by formats whose header carries
/// per-channel hints — IT (`initial_channel_pan` / `initial_channel
/// _volume`) and S3M (`channel_settings`). All fields default to
/// "no override": `None` for the optional ones, `false` for the
/// flags. The player applies each property only when the
/// corresponding field is set.
#[derive(Default, Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct ChannelDefault {
    /// Initial pan in `[0, 1]` (left..right) as Q1.15. `None`
    /// keeps the channel at its centre default.
    pub panning: Option<Panning>,

    /// Initial channel volume in `[0, 1]` as Q1.15. `None` keeps
    /// the channel at full volume. Mirrors IT's `Mxx` register
    /// state at song start (header byte
    /// `initial_channel_volume[ch]`, 0..64 normalised here).
    pub volume: Option<ChannelVolume>,

    /// Channel starts muted. S3M's `channel_settings & 0x80` and
    /// IT's `initial_channel_pan & 0x80` both set this. The player
    /// suppresses output for the channel until something un-mutes
    /// it (typically nothing in practice — disabled channels stay
    /// silent for the whole song).
    pub muted: bool,

    /// Channel starts in IT's "surround" mode (S91): the right
    /// output is phase-inverted at mix time, producing a
    /// pseudo-stereo effect on regular speaker pairs. Set when
    /// the IT header's `initial_channel_pan[ch]` byte equals 100
    /// (IT's surround sentinel). Toggled at runtime by S90/S91
    /// effects.
    pub surround: bool,
}

/// Pattern-grid display hint. Editors use these two periods to
/// draw row-highlight separators in the pattern view, so a user
/// can read beats and measures at a glance. Has no effect on
/// playback — purely a UI cue.
///
/// IT carries this in its file header; XM, S3M and MOD do not.
/// Defaulting to `(beat: 4, measure: 16)` means "4/4 time at 16th-
/// note resolution", which matches IT's own startup defaults and
/// what virtually every authored module assumes when the editor
/// presents an empty grid.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatternHighlight {
    /// Number of rows that make up one beat. The editor draws a
    /// subtle highlight at every multiple of this row index. Also
    /// known as "rows per beat" in IT/OpenMPT.
    pub beat: u8,
    /// Number of rows that make up one measure. The editor draws
    /// a stronger highlight at every multiple of this row index,
    /// typically `beat * time_signature_top`. Also known as "rows
    /// per measure" in IT/OpenMPT.
    pub measure: u8,
}

impl Default for PatternHighlight {
    fn default() -> Self {
        Self {
            beat: 4,
            measure: 16,
        }
    }
}

/// SoundTracker Module with Steroid
#[derive(Serialize, Deserialize, Debug)]
pub struct Module {
    pub name: String,
    pub comment: String,
    /// The format this module was authored in, plus the playback
    /// quirks that match its authoring tracker. See
    /// [`crate::compatibility_profile::CompatibilityProfile`] for
    /// the named constructors importers use; editor-authored
    /// modules typically leave this at
    /// [`CompatibilityProfile::modern`](crate::compatibility_profile::CompatibilityProfile::modern) for clean, quirk-free
    /// playback.
    pub profile: crate::compatibility_profile::CompatibilityProfile,
    pub frequency_type: FrequencyType,
    /// Restart index in `pattern_order`
    pub restart_position: usize,
    pub default_tempo: usize,
    pub default_bpm: usize,
    /// Defines the exact order for the patterns playback
    /// It is possible to have several music in the same Module
    pub pattern_order: Vec<Vec<usize>>,
    pub pattern: Vec<Pattern>,
    pub pattern_names: Vec<String>,
    pub channel_names: Vec<String>,
    /// Per-channel initial state, applied at song start before any
    /// pattern row plays. Each entry bundles every property a
    /// format's header can pre-set on a channel: panning, channel
    /// volume, mute, surround. Empty means "use defaults for every
    /// channel" (centre pan, full volume, unmuted, no surround).
    ///
    /// Populated by formats whose header carries per-channel hints
    /// — currently S3M (`channel_settings`) and IT (initial channel
    /// pan/volume + surround sentinel). XM/MOD leave it empty:
    /// those formats centre every channel at song start, and any
    /// per-channel state that follows is encoded in the patterns.
    pub channel_defaults: Vec<ChannelDefault>,
    pub instrument: Vec<Instrument>,
    /// Optional MIDI-macro table. Populated by the IT importer when
    /// the source file carries an embedded-macros flag; `None` for
    /// non-IT formats and for IT files without macros. Consumed by
    /// the replayer's MIDI-macro interpreter (per-channel filter
    /// automation, MIDI-out, etc.).
    pub midi_macros: Option<MidiMacros>,
    /// Pattern-grid display hint (rows per beat / rows per measure).
    /// Populated from the IT header; XM/S3M/MOD inherit the
    /// `PatternHighlight::default()` 4/16 cadence.
    pub pattern_highlight: PatternHighlight,
    /// Optional IT-style mix-plugin table (OpenMPT extension).
    /// `None` for non-IT formats and for IT files without an
    /// embedded plugin section.
    pub mix_plugins: Option<MixPlugins>,
    /// MIDI pitch-wheel depth declared by the module author, in
    /// semitones. The IT header carries this byte (ITTECH:94, "PWD:
    /// Pitch wheel depth for MIDI controllers") so downstream MIDI-
    /// out code knows how to scale tracker pitch effects (Gxx/Fxx/
    /// Exx) to MIDI 14-bit pitch-bend values when forwarding notes
    /// played through `InstrMidi` / `InstrOpl` / `InstrSid`
    /// instruments.
    ///
    /// **Not consumed by the core replayer.** xmrsplayer renders
    /// only `InstrumentType::Default` (sampled) instruments
    /// natively; the synth / MIDI-out paths are the responsibility
    /// of code that wraps the player and observes its events. This
    /// field exists so such wrappers can read a single source of
    /// truth instead of carrying their own per-module config.
    ///
    /// Macro-driven pitch bend (`Zxx` / `SFx` macros encoding
    /// `En ll mm` bytes) is unaffected — the macro author writes
    /// the absolute 14-bit value directly, and xmrsplayer's
    /// [`MidiObserver`](https://docs.rs/xmrsplayer) emits it
    /// verbatim. PWD only matters when downstream code is
    /// *generating* pitch bend from a tracker semitone delta.
    ///
    /// IT importer copies the header byte. Other formats default
    /// to **2** semitones — the General MIDI default a receiver
    /// would assume in the absence of an explicit RPN setup.
    /// Range per ITTECH is 0..=15.
    pub pitch_wheel_depth: u8,
    /// Song-level mix volume scalar (0..1). IT-specific: each IT
    /// file carries a "mix volume" register (0..128, default 48)
    /// which is a constant mixer-level headroom applied on top of
    /// the Vxx-animated global volume. It is NOT part of the Vxx
    /// effect chain — authors set it once to reserve dynamic range
    /// for resonant-filter peaks and multi-voice summing.
    ///
    /// Mix master amplifier, Q1.15 in `[0, 1]`. Multiplies the
    /// final mixed audio. Was `f32` — see [`Volume`] for the
    /// conversion convention.
    ///
    /// IT-specific (the IT importer reads it from the file
    /// header). Non-IT formats leave this at [`Volume::FULL`]
    /// (identity). The player reads it once at construction
    /// and applies it as a final constant multiplier separate
    /// from both `global_volume` and user-facing
    /// `amplification`.
    pub mix_volume: Volume,
}

/// A module's MIDI macro table. IT-specific in practice but the
/// type lives on `Module` so the replayer doesn't need to know
/// which format the macros came from.
///
/// Each macro is a 32-byte sequence of hex values. The byte `b'z'`
/// (0x7A) is a placeholder that the replayer substitutes with the
/// current `Zxx` parameter at interpretation time — that's how IT's
/// typical "F0 F0 00 z" macro (set filter cutoff to `z`) works.
///
/// Tables follow ITTECH layout:
/// * `global`      — 9 entries (global MIDI setup: port init, note
///   on/off mapping, etc.)
/// * `parametric`  — 16 entries, selected per-channel via `SF0..SFF`
///   and invoked by `Zxx` with `xx < 0x80`
/// * `fixed`       — 128 entries, addressed absolutely via `Zxx`
///   with `xx >= 0x80` (index = `xx - 0x80`)
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct MidiMacros {
    pub global: Vec<Vec<u8>>,
    pub parametric: Vec<Vec<u8>>,
    pub fixed: Vec<Vec<u8>>,
}

impl Default for Module {
    fn default() -> Self {
        Module {
            name: "".to_string(),
            comment: "".to_string(),
            profile: crate::compatibility_profile::CompatibilityProfile::default(),
            frequency_type: FrequencyType::LinearFrequencies,
            restart_position: 0,
            default_tempo: 6,
            default_bpm: 125,
            pattern_order: vec![],
            pattern: vec![],
            pattern_names: vec![],
            channel_names: vec![],
            channel_defaults: vec![],
            instrument: vec![],
            midi_macros: None,
            pattern_highlight: PatternHighlight::default(),
            mix_plugins: None,
            pitch_wheel_depth: 2,
            mix_volume: Volume::FULL,
        }
    }
}

impl Module {
    /// get song length
    pub fn get_song_length(&self, song: usize) -> usize {
        self.pattern_order[song].len()
    }

    /// get number of channels
    pub fn get_num_channels(&self) -> usize {
        if let Some(first_row) = self.pattern.first().and_then(|p| p.first()) {
            first_row.len()
        } else {
            0
        }
    }

    /// get number of rows
    pub fn get_num_rows(&self, pat_idx: usize) -> usize {
        if pat_idx < self.pattern.len() {
            self.pattern[pat_idx].len()
        } else {
            0
        }
    }
}
