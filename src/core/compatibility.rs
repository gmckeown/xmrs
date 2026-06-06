//! Per-module playback behaviour switches.
//!
//! [`PlaybackQuirks`] is the **runtime** knob the replayer consults
//! to decide how to render a module. Each switch is orthogonal,
//! named for what it *does* rather than which tracker it came from,
//! so an editor can compose any subset without lying about the
//! output format.
//!
//! The `Default` impl is fully conservative: every quirk off. The
//! preset constructors that reproduce a specific historical tracker
//! live in `crate::tracker::profiles` (gated behind any `import_*`
//! feature) — `ft2()`, `it214()`, `it215()`, `st3()`, `pt()`. A
//! no-tracker `--no-default-features` build does not compile them.

use serde::{Deserialize, Serialize};

use crate::core::fixed::units::Period;

/// Per-module playback behaviour switches. See module docs.
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

    /// A `TrackPitch` *Slide* lane advances on every tick of a row,
    /// including tick 0, instead of only the `speed-1` non-row ticks.
    /// Tracker formats (XM/MOD/S3M/IT) trigger the row on tick 0 and
    /// run continuous slides on ticks `1..speed`, so the lane must
    /// skip tick 0 there. The David Whittaker replayer instead runs
    /// its slide in `DoFrameStuff` on *every* frame of the held note
    /// (1 frame = 1 player tick), so skipping tick 0 would undercount
    /// the ramp by `(speed-1)/speed` per row. DW on; all trackers off.
    pub pitch_slide_ticks_at_row_zero: bool,

    /// A `TrackPitch` *Lfo* (vibrato) lane keeps advancing its phase
    /// on every tick once armed, instead of only on rows that
    /// re-issue an `LfoEvent::Set`. Tracker formats (XM/MOD/S3M/IT)
    /// re-arm vibrato per row — the effect column drives `tick_q`, so
    /// the LFO only ticks while the row carries the effect. The David
    /// Whittaker replayer instead runs its vibrato in `DoFrameStuff`
    /// on *every* frame of the held note (1 frame = 1 player tick)
    /// until an explicit stop, so without this the wobble collapses
    /// to near-zero: the `Set` fires once at the note, then the phase
    /// never moves and the period stays flat. Symmetric to
    /// [`Self::pitch_slide_ticks_at_row_zero`]. DW on; all trackers off.
    pub pitch_vibrato_ticks_continuously: bool,

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
    /// origin. S3M on, IT on. digcmd.c:s_patloop — `song.patloopstart
    /// = song.np_row + 1` at exit; schism `effects.c:fx_pattern_loop`
    /// — `chan->patloop_row = csf->row + 1` when the counter expires.
    pub pattern_loop_resumes: bool,

    /// While a pattern loop is still iterating on **any** channel,
    /// a `PatternBreak` (`Cxx` / `Dxx`) on the same row is **ignored**
    /// — the loop keeps the playhead inside the pattern and the break
    /// only takes effect once every loop has finished. Impulse Tracker
    /// behaviour: schism `effects.c:fx_special` sets `patloop = 1` when
    /// any channel has an active `cd_patloop`, then the `FX_PATTERNBREAK`
    /// arm is guarded by `if (patloop) break;` (`effects.c:2421`).
    /// FT2/PT instead let the break win (their pattern-loop and
    /// pattern-break flags are independent). IT on; XM/MOD/S3M off.
    pub active_loop_suppresses_break: bool,

    /// When a pattern loop **jumps** and a `PatternBreak` lands on the
    /// same row, the break still wins the order advance but the next
    /// pattern starts at **row 0** instead of the break's target row.
    /// ProTracker: its row-end handler consumes the loop's `pBreakFlag`
    /// first — `row = pBreakPosition; pBreakPosition = 0`
    /// (`pt2_replayer.c:1429`) — *clearing* the shared break position, so
    /// the subsequent `posJumpAssert` advance reads 0. FT2 does **not**
    /// clear between the two steps (`ft2_replayer.c:2311`/`:2317`), so
    /// there the break's target row survives. MOD/PT on; XM/S3M/IT off.
    pub pattern_loop_break_resets_row: bool,

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

    /// `Sample.volume` (IT's "GvL", Global Volume per sample) stays
    /// a static per-sample gain applied on every mixer tick inside
    /// the voice's `get_volume` chain. For MOD/XM/S3M, `Sample.
    /// volume` is consumed ONCE at note-trigger time as the
    /// channel's initial volume — subsequent V-column / tremolo
    /// updates overwrite it and it must NOT re-scale the voice
    /// downstream. Without this flag the player would apply
    /// `Sample.volume` twice for non-IT formats (once at trigger,
    /// once per tick), squaring it and pulling every reduced-
    /// volume sample down by an extra few dB.
    /// IT on; MOD/XM/S3M off. schism `sndmix.c` keeps GvL in the
    /// running gain chain; ft2_replayer.c / Protracker consume
    /// `vol` at `triggerNote` only.
    pub sample_volume_is_static_gain: bool,

    /// `Fxx` with `xx == 0` is the historical ProTracker / MOD
    /// "stop song" marker — the engine halts playback when it
    /// hits this row. XM reuses the same `Fxx` encoding and
    /// although strict FT2 actually freezes on `F00` (it sets
    /// `song.speed = 0` and the main loop stops advancing),
    /// every modern XM player (OpenMPT, MilkyTracker, XMPlay,
    /// Schism) treats it as end-of-song — matching the
    /// composer's intent when the marker is placed at the end
    /// of the order table. S3M and IT route speed and BPM
    /// through *separate* effects (`Axx` / `Txx`), and
    /// `Axx` with `xx == 0` is documented as "ignored" in both
    /// ScreamTracker 3 and Impulse Tracker — there's no
    /// end-of-song convention to honour there.
    ///
    /// When this quirk is ON, a `SongLevelEffect::Speed(0)` on a
    /// row signals end-of-song to the player (the row plays
    /// through its natural tick run, then `SongEnd` fires
    /// before the next row would start) and to the
    /// [`crate::core::duration`] estimator (the simulator breaks
    /// out of its order-walk loop at the same point).
    ///
    /// MOD on (`pt`), XM on (`ft2`), S3M off (`st3`), IT off
    /// (`it214` / `it215`), modern off (`modern`).
    pub speed_zero_ends_song: bool,
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
///   `Xxx`/`Pxx` effects ever touch it.
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
