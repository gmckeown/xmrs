#![forbid(unsafe_code)]

//! David Whittaker replayer simulator.
//!
//! Walks the replayer state machine (the per-frame tick loop and
//! the effect runtime semantics) without
//! producing audio: instead the simulator emits a [`TickEvent`]
//! stream that downstream layers turn into xmrs [`Cell`]s on a
//! DAW timeline.
//!
//! This first slice covers the structural backbone: the
//! per-channel speed-counter ratchet, track-byte consumption,
//! position-list advancement (including loop), and the
//! note / set-sample / wait events. The per-tick effect family
//! (slide / vibrato / arpeggio / envelope) is acknowledged in
//! [`ChannelState`] but the actual `do_frame_stuff` work is left
//! as a follow-up — the events still fire so the higher layers
//! can wire timing correctly today.
//!
//! [`Cell`]: crate::core::cell::Cell

use alloc::vec::Vec;

use super::dw_module::DwModule;
use super::event::DwTrackEvent;
use super::header::{DwPositionList, DwTrack, DW_NUM_CHANNELS};

/// Hard cap on simulator runtime — bumped to 20 minutes so the
/// per-row frame budget can grow (e.g. tracker-style `tempo = 8`
/// makes one row span 8 frames instead of 3, so a 7-minute song
/// needs ~21 000 frames). Still a safety net against runaway
/// state on malformed modules.
pub const MAX_SIMULATION_FRAMES: u32 = 50 * 60 * 20;

/// Per-frame event emitted by [`Simulator::tick`]. The variants
/// carry just enough information for a DAW-side converter to
/// produce the right xmrs `Cell`; runtime audio details (Paula
/// period, mixer level) stay inside the simulator until they're
/// needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickEvent {
    /// A note triggered on `channel`. `note` is the raw track
    /// byte (`0..=0x7F`); `sample_index` is whatever the channel
    /// had last set via [`DwTrackEvent::SetSample`]. New-player
    /// notes also store the resolved index here (the runtime
    /// will refine when sample-transpose lands).
    ///
    /// `volume` carries the initial Paula-scale volume (0..=127,
    /// 7-bit) read from the channel's currently selected volume
    /// envelope at the moment of the note trigger. `None` when
    /// no envelope is armed — the channel keeps whatever Paula
    /// volume was last loaded (xmrs defaults to full scale, which
    /// matches the audible behaviour of an un-shaped Whittaker
    /// channel).
    ///
    /// `effective_note` is the raw track byte plus the song's
    /// global transpose and the channel's local transpose (set
    /// by `Effect8` in modules with
    /// [`super::detect::DwFeatures::enable_channel_transpose`]).
    /// It can fall outside the 0..=0x7F range when either
    /// transpose pushes the note past the period table boundary;
    /// downstream `Pitch` conversion clamps to the playable
    /// keyboard range.
    NoteOn {
        channel: u8,
        sample_index: Option<u16>,
        note: u8,
        effective_note: i16,
        volume: Option<u8>,
        /// Index into [`super::dw_module::DwModule::volume_envelopes`]
        /// of the envelope armed at trigger time. The DAW layer
        /// uses this to walk the full step sequence row-by-row
        /// and project the envelope's attack/decay/sustain shape
        /// onto subsequent `TrackEffect::Volume` cells, rather
        /// than the single-`peak` approximation carried in
        /// `volume`. `None` when no envelope is armed.
        envelope_index: Option<u16>,
        /// Index into [`super::dw_module::DwModule::arpeggios`] of
        /// the pitch arpeggio armed at trigger time, or `None`.
        /// The DAW layer maps it to a `TrackEffect::Arpeggio`.
        arpeggio_index: Option<u16>,
    },
    /// `SetSample` arrived without a following note in this
    /// tick — the channel will use this sample on the next note.
    SampleSet { channel: u8, sample_index: u16 },
    /// `Mute` command — silence the channel immediately.
    Mute { channel: u8 },
    /// `SetSpeed` updated the channel's tick budget.
    SpeedChange { channel: u8, speed: u8 },
    /// `StopSong` command — the simulator halts after this tick.
    SongEnd { channel: u8 },
    /// Channel exhausted its position list with no `loop_to`,
    /// so it falls silent for the remainder of the song.
    ChannelFinished { channel: u8 },
    /// `StartVibrato` command — arm pitch LFO on the channel.
    /// `speed` and `depth` are the Whittaker bytes from the
    /// command. The DAW converter turns these into a
    /// [`crate::core::daw::automation::LfoEvent::Set`] on a
    /// `TrackPitch` lane.
    VibratoStart { channel: u8, speed: u8, depth: u8 },
    /// `StopVibrato` command — clear the channel's pitch LFO.
    VibratoStop { channel: u8 },
    /// `Slide` command — arm a linear pitch slide on the
    /// channel. `speed` is the per-tick period delta (signed,
    /// negative = pitch up), `counter` is the delay in ticks
    /// before the slide engages. Mapped to a `SlideEvent::Set`
    /// on the channel's `TrackPitch` lane.
    SlideStart { channel: u8, speed: i8, counter: u8 },
    /// Internal: the simulator emits this implicitly when a
    /// note arrives that overwrites the slide state (a new note
    /// resets `slide_enabled = false`). Lets
    /// the DAW converter close the slide cleanly.
    SlideStop { channel: u8 },
    /// Per-frame pitch arpeggio offset (in semitones) on the
    /// channel — emitted whenever the held-note arpeggio cycle's
    /// current offset *changes* (including back to `0` when the
    /// note ends or no arpeggio is armed). The replayer advances a
    /// per-channel pointer through the arpeggio's offset list every
    /// frame and adds the offset to the note's period-table *index*
    /// (semitone space, Ghidra `play_tick` held-note branch); the
    /// DAW converter turns the stream into a `TrackPitch`
    /// `LaneKind::Points` curve of
    /// [`AutomationValue::Pitch`](crate::core::daw::automation::AutomationValue::Pitch) points
    /// — the faithful, per-frame-continuous form the classic
    /// three-step `TrackEffect::Arpeggio` can't express.
    Arpeggio { channel: u8, semitones: i8 },
}

impl TickEvent {
    /// The Paula channel (0..4) this event acts on. Every variant is
    /// per-channel, so this is always defined.
    pub fn channel(&self) -> u8 {
        match self {
            TickEvent::NoteOn { channel, .. }
            | TickEvent::SampleSet { channel, .. }
            | TickEvent::Mute { channel }
            | TickEvent::SpeedChange { channel, .. }
            | TickEvent::SongEnd { channel }
            | TickEvent::ChannelFinished { channel }
            | TickEvent::VibratoStart { channel, .. }
            | TickEvent::VibratoStop { channel }
            | TickEvent::SlideStart { channel, .. }
            | TickEvent::SlideStop { channel }
            | TickEvent::Arpeggio { channel, .. } => *channel,
        }
    }
}

/// Module-wide playback state — speed, delay, global transpose,
/// fade. Mirrors the replayer's `GlobalPlayingInfo` with the
/// fields actually consulted by this simulator's first slice.
#[derive(Debug, Clone)]
pub struct GlobalState {
    /// Initial speed of every channel at song start.
    pub initial_speed: u8,
    /// Delay-counter ratchet — number of *full* frames between
    /// two effective ticks when [`super::detect::DwFeatures::enable_delay_counter`]
    /// is set. Zero means the feature is off and every frame
    /// ticks.
    pub delay_counter_speed: u8,
    /// Frames since the last effective tick (cycles `0..=delay_counter_speed-1`).
    pub delay_counter: u8,
    /// Global transposition added to every note before period
    /// lookup. Modified by `GlobalTranspose` (cmd 5).
    pub transpose: i8,
    /// Global volume scaler (`0..=64`). Modified by
    /// `SetGlobalVolume` (cmd 12) and decayed by
    /// `GlobalVolumeFade` (cmd 11).
    pub global_volume: u8,
    /// Sub-song speed-fade increment used by `GlobalVolumeFade`.
    /// `0` means the fade is inactive.
    pub global_volume_fade_speed: u8,
    /// Down-counter that triggers a one-step volume decrement
    /// every time it reaches zero.
    pub global_volume_fade_counter: u8,
}

/// State carried per channel between ticks.
#[derive(Debug, Clone)]
pub struct ChannelState {
    /// 0-based channel index (0..4). Used in the event log.
    pub index: u8,
    /// Index into this channel's `DwPositionList.entries` — the
    /// next position to advance to after `EndOfTrack`.
    pub position_index: usize,
    /// Position the channel returns to on
    /// [`DwPositionList::loop_to`].
    pub restart_position: u16,
    /// Index into [`DwModule::tracks`] of the currently playing
    /// track, or `None` when the channel is finished or
    /// uninitialised.
    pub track_index: Option<usize>,
    /// Cursor inside the current track's `events` array.
    pub event_cursor: usize,
    /// Frames remaining before this channel reads its next
    /// event. Decremented each `Play()` tick.
    pub speed_counter: u16,
    /// Default frame budget between reads (set by
    /// [`DwTrackEvent::SetSpeed`]).
    pub speed: u8,
    /// Last `SetSample` command issued on this channel.
    pub current_sample: Option<u16>,
    /// Last note played on this channel.
    pub current_note: Option<u8>,
    /// Index into [`super::dw_module::DwModule::volume_envelopes`]
    /// of the envelope currently armed on this channel — set by
    /// the dispatcher's `0xA0..` bracket
    /// ([`DwTrackEvent::SetVolumeEnvelope`]). `None` until the first
    /// such command fires, at which point the next note pulls its
    /// initial volume from the envelope's first step.
    pub current_envelope: Option<u16>,
    /// Index into [`super::dw_module::DwModule::arpeggios`] of the
    /// pitch arpeggio currently armed on this channel — set by the
    /// dispatcher's `0x90..` bracket ([`DwTrackEvent::SetPitchArpeggio`]).
    /// `None` until the first such command fires. Carried onto each
    /// `NoteOn` so the DAW layer can attach the matching
    /// `TrackEffect::Arpeggio`.
    pub current_arpeggio: Option<u16>,
    /// Per-channel transpose (set by Effect8 when the module
    /// has [`super::detect::DwFeatures::enable_channel_transpose`]).
    pub transpose: i8,
    /// Inter-note wait, in frames. Holds the last `(byte - 0xDF) ×
    /// sub_song.speed` value emitted by a `LongWait` on this
    /// channel; the runtime arms the post-Note speed counter from
    /// it so two consecutive Notes (with or without an inline
    /// `LongWait` between them) pace exactly like Whittaker's
    /// `chan[+0x1E] = chan[+0x1C]` reset at the bottom of the
    /// replayer's per-channel block (`Play+0x358`).
    pub next_wait_frames: u16,
    /// `true` once the channel has finished its position list
    /// (no further events will fire).
    pub finished: bool,
    /// `true` while a `Slide` armed on this channel is still live.
    /// Whittaker clears the slide flag (`*pbVar15 = 0`) at the head
    /// of every row read (`play_tick`, `speed_counter == 0` branch),
    /// so a slide only lasts the wait of its own row. We mirror that
    /// here to emit a bounding [`TickEvent::SlideStop`] on the next
    /// row read — without it the converted `SlideEvent::Set` would
    /// never get a `Clear` and the lane would stay armed forever.
    pub slide_active: bool,
    /// Position of the per-frame arpeggio pointer within the active
    /// `current_arpeggio`'s offset list. The replayer advances it
    /// one step every held frame and wraps at the terminator
    /// (Ghidra `play_tick` held-note branch, ptr `chan+0x648`); it
    /// is **not** reset by a new note, only when a fresh arpeggio is
    /// armed. Drives the `TrackPitch` Points lane.
    pub arp_phase: usize,
    /// Last arpeggio offset emitted as a [`TickEvent::Arpeggio`], so
    /// the simulator only logs *changes* (the Points lane latches a
    /// value forward). `None` until the first emission.
    pub arp_last_emitted: Option<i8>,
}

impl ChannelState {
    fn new(index: u8, position_list: &DwPositionList, speed: u8) -> Self {
        Self {
            index,
            position_index: 0,
            restart_position: 0,
            track_index: None,
            event_cursor: 0,
            // Whittaker's per-channel `chan[+0x1E]` is initialised
            // to 1 in `Init` (`MOVE.W #1, (0x1E, A1)` at
            // `init+0x9C`). After the first 50 Hz tick decrement
            // hits zero, the channel reads its very first event —
            // typically a `SetSample` / `LongWait` pair that
            // initialises `chan[+0x1C]` before any Note fires.
            speed_counter: 1,
            speed,
            current_sample: None,
            current_note: None,
            current_envelope: None,
            current_arpeggio: None,
            transpose: 0,
            // Whittaker leaves `chan[+0x1C]` zero at init; the
            // first `LongWait` event on the channel sets it. We
            // mirror that here, so a track that opens with
            // `LongWait_5 ; Note` paces by 15 frames (= `5 × 3`)
            // exactly as the original does.
            next_wait_frames: 0,
            finished: position_list.is_empty(),
            slide_active: false,
            arp_phase: 0,
            arp_last_emitted: None,
        }
    }
}

/// Drives a [`DwModule`] one frame at a time. The simulator
/// borrows the module immutably — all mutation lives on
/// [`Self::global`] and [`Self::channels`].
pub struct Simulator<'a> {
    module: &'a DwModule,
    /// Position lists driving this run — one per Paula channel.
    /// Defaults to the module's selected sub-song
    /// ([`DwModule::position_lists`]) but
    /// [`Simulator::new_for_subsong`] points it at any other
    /// sub-song's lists so every tune in a multi-song `.dw` can be
    /// simulated. Tracks are still resolved against
    /// [`DwModule::tracks`], which holds the union of every
    /// sub-song's tracks.
    position_lists: &'a [DwPositionList; DW_NUM_CHANNELS],
    /// Active sub-song state. Initialised from
    /// [`DwModule::sub_song`] at construction.
    pub global: GlobalState,
    /// Per-Paula-channel state, indexed 0..4.
    pub channels: [ChannelState; DW_NUM_CHANNELS],
    /// Frames elapsed since `new()`. Wraps at the cap.
    pub frame: u32,
    /// `true` after a [`TickEvent::SongEnd`] or after all four
    /// channels finished their position lists.
    pub halted: bool,
    /// Frame at which each channel first wrapped its position list
    /// back to the loop target (`DwPositionList::loop_to`). `None`
    /// while the channel is still on its first pass (or never loops).
    /// One full traversal of a channel's list takes `first_wrap_frame
    /// - 1` frames, so the song's seamless loop period is the LCM of
    /// the four — used by [`DwModule::to_module`] to bound the
    /// rendered primary song and place `Module::song_loop_to`.
    pub first_wrap_frame: [Option<u32>; DW_NUM_CHANNELS],
    /// Frame at which each channel first reached its `loop_to` target
    /// entry — i.e. the start of the loop body. `1` (→ tick 0) for the
    /// common `loop_to = 0` channels (the whole pass loops); later for a
    /// channel whose intro track redirects via `SeqPtr` to a
    /// sub-sequence (`follow_seq_ptr_loops`), where the intro plays once
    /// and only the body loops. [`DwModule::to_module`] uses it as the
    /// `ChannelLoop::start_tick` so the player's per-lane fold replays
    /// the body, not the intro.
    pub loop_start_frame: [Option<u32>; DW_NUM_CHANNELS],
}

impl<'a> Simulator<'a> {
    /// Build a simulator from a parsed module. `module.sub_song`
    /// supplies speed / delay defaults; when missing the
    /// simulator initialises with a sensible Whittaker default
    /// (speed=6, no delay).
    pub fn new(module: &'a DwModule) -> Self {
        // Per-channel tick rate = the sub-song's speed byte.
        //
        // This is the LongWait multiplier: a note's inter-read
        // gap is `(longwait_byte - 0xDF) × speed` frames (50 Hz
        // PAL). On the **old player** a Ghidra trace of qball.dw
        // confirms it directly — `PlayTick` reloads the global
        // tick counter from the sub-song speed byte
        // (`cRam000002E1`, = 7 on qball) and the per-channel
        // wait counter counts in those ticks, so the note spacing
        // is `chan_wait × speed` frames. On the **new player**
        // the same byte feeds the LongWait `(byte-0xDF) × speed`
        // step; xenon2's value is 3, which is what this used to
        // be hardcoded to — reading it from the sub-song instead
        // keeps xenon2 identical while fixing every module whose
        // speed isn't 3 (qball=7 was playing ~2× too fast at the
        // old hardcoded 3, beast1=6, speedball=4, …).
        //
        // The sub-song delay-counter throttle is NOT applied as a
        // per-frame skip (see `tick`): it's a uniform tempo cut, so
        // the DAW projection folds it into BPM instead. The simulator
        // runs every frame, keeping note timing on the clean row grid.
        let speed = module
            .sub_song
            .as_ref()
            .map(|s| s.speed.max(1))
            .unwrap_or(6);
        Self::build(module, &module.position_lists, speed)
    }

    /// Build a simulator for an explicit sub-song: its own
    /// `position_lists` and `speed`, while still resolving track
    /// offsets against the shared [`DwModule::tracks`]. Used by
    /// [`DwModule::to_module`] to render every sub-song of a
    /// multi-song `.dw` as a separate xmrs song.
    pub fn new_for_subsong(
        module: &'a DwModule,
        position_lists: &'a [DwPositionList; DW_NUM_CHANNELS],
        speed: u8,
    ) -> Self {
        Self::build(module, position_lists, speed.max(1))
    }

    fn build(
        module: &'a DwModule,
        position_lists: &'a [DwPositionList; DW_NUM_CHANNELS],
        speed: u8,
    ) -> Self {
        let channels =
            core::array::from_fn(|i| ChannelState::new(i as u8, &position_lists[i], speed));
        Self {
            module,
            position_lists,
            global: GlobalState {
                initial_speed: speed,
                delay_counter_speed: 0,
                delay_counter: 0,
                transpose: 0,
                global_volume: 64,
                global_volume_fade_speed: 0,
                global_volume_fade_counter: 0,
            },
            channels,
            frame: 0,
            halted: false,
            first_wrap_frame: [None; DW_NUM_CHANNELS],
            loop_start_frame: [None; DW_NUM_CHANNELS],
        }
    }

    /// Advance the simulator by exactly one Paula frame (50 Hz
    /// PAL). Returns every event emitted during this tick — a
    /// frame may yield multiple events (one per channel that
    /// consumed something) or none (all channels waiting).
    pub fn tick(&mut self) -> Vec<TickEvent> {
        let mut out: Vec<TickEvent> = Vec::new();
        if self.halted {
            return out;
        }
        self.frame = self.frame.saturating_add(1);

        // Phase 1 — global delay ratchet. Ghidra
        // `play_tick` head is an 8-bit accumulator (`DAT_5ea +=
        // DAT_5e8`) that SKIPS the whole frame on overflow — a uniform
        // `delay/256` thinning of frames (xenon2 delay 20 → ~8.5%
        // slower, ~46 Hz). We deliberately do NOT skip frames: note
        // spacing is an integer multiple of the row grid (`LongWait ×
        // speed` frames), so running every frame keeps notes exactly
        // on row boundaries. Skipping would push them off-grid by a
        // non-integer amount and the row quantisation in `to_module`
        // would jitter the spacing ±1 row (audible as rows arriving
        // "hesitantly", an accumulating drift). The original's skip is
        // sub-perceptual 50 Hz jitter; the faithful projection of its
        // net effect is a uniform tempo cut, which `to_module` folds
        // into BPM (`125 × (256 − delay) / 256`).

        // Phase 2 — global volume fade. `GlobalVolumeFade`
        // (cmd 11) arms `global_volume_fade_speed`; here the master
        // volume steps down by 1 every `fade_speed` frames until it
        // reaches 0. Now that `global_volume` actually scales each
        // note (see the `Note` handler), this produces the fade-out
        // those commands intend. Inactive (speed 0) on most modules.
        if self.global.global_volume_fade_speed > 0 && self.global.global_volume > 0 {
            self.global.global_volume_fade_counter =
                self.global.global_volume_fade_counter.saturating_sub(1);
            if self.global.global_volume_fade_counter == 0 {
                self.global.global_volume = self.global.global_volume.saturating_sub(1);
                self.global.global_volume_fade_counter = self.global.global_volume_fade_speed;
            }
        }

        // Phase 4 — per-channel work. Phase 3 (square
        // waveform morph) is acknowledged in the state but its
        // processing is deferred.
        for i in 0..DW_NUM_CHANNELS {
            if self.channels[i].finished {
                continue;
            }
            self.tick_channel(i, &mut out);
            if self.channels[i].finished && !self.halted {
                out.push(TickEvent::ChannelFinished { channel: i as u8 });
            }
        }

        // Re-resolve each NoteOn's `effective_note` with the global /
        // channel transpose as of the END of this tick. The replayer
        // recomputes a (held) note's base period EVERY frame from the
        // *current* global transpose (`Play+0x2C0`), so a transpose set
        // by a later-numbered channel on the SAME frame governs the
        // note's whole sustain — e.g. bad company: ch1 sets +12 at f1,
        // *after* ch0's note triggers, and the oracle's ch0 base jumps an
        // octave from f2. Baking the trigger-instant transpose left such
        // a note (held across the change by a slide) an octave off for
        // its entire slide; baking the end-of-tick value puts the right
        // transpose into the base *before* the period-space slide
        // subtracts (a semitone `Pitch` lane applied after the slide
        // would instead scale the slide — see the note-trigger comment).
        // Correct for the sustain; only the trigger frame itself can
        // differ by one frame. No-op when no transpose changes this tick.
        for ev in out.iter_mut() {
            if let TickEvent::NoteOn {
                channel,
                note,
                effective_note,
                ..
            } = ev
            {
                let ct = self
                    .channels
                    .get(*channel as usize)
                    .map(|c| c.transpose)
                    .unwrap_or(0);
                *effective_note = (*note as i16)
                    .saturating_add(self.global.transpose as i16)
                    .saturating_add(ct as i16);
            }
        }

        if self.channels.iter().all(|c| c.finished) {
            self.halted = true;
        }
        out
    }

    /// Run until [`Self::halted`] is set or `MAX_SIMULATION_FRAMES`
    /// is reached. Returns the full event trace as
    /// `(frame_index, event)` pairs — useful for diffing against
    /// the Paula-emulation oracle (`oracle/`).
    pub fn run(&mut self) -> Vec<(u32, TickEvent)> {
        self.run_capped(MAX_SIMULATION_FRAMES)
    }

    /// Like [`Self::run`] but stops at `max_frames`. Used by the
    /// DAW projection to keep *audition* sub-songs (the non-primary
    /// tunes of a multi-song `.dw`) short — a looping variation
    /// would otherwise simulate the full 20-minute cap, and a
    /// module with a dozen of them (grimblood ships 18) balloons
    /// the rendered `Module` enough to exhaust memory. The primary
    /// (auto-selected) song still runs the full cap.
    pub fn run_capped(&mut self, max_frames: u32) -> Vec<(u32, TickEvent)> {
        let mut trace = Vec::new();
        while !self.halted && self.frame < max_frames {
            let events = self.tick();
            let frame = self.frame;
            for ev in events {
                trace.push((frame, ev));
            }
        }
        trace
    }

    /// Run the primary song for exactly one **seamless loop period**
    /// and report it (in frames). Each Whittaker channel loops its
    /// own position list independently, so the whole arrangement only
    /// realigns — every channel simultaneously back at list position
    /// 0 — after the LCM of the per-channel pass lengths. xenon2's
    /// four voices are authored to share one period (10725 frames),
    /// so the LCM is just that; the general case caps at `max_frames`.
    ///
    /// Returns `(trace, Some(period))` truncated to the period when a
    /// loop was found, or `(trace, None)` (rendered to `max_frames`,
    /// no clean loop) when at least one looping channel never wrapped
    /// inside the cap. The DAW projection sets
    /// [`crate::core::module::Module::song_loop_to`] to tick 0 when a
    /// period is returned — the list loops wholly from entry 0, so the
    /// realignment point is the song start.
    pub fn run_with_loop(&mut self, max_frames: u32) -> (Vec<(u32, TickEvent)>, Option<u32>) {
        let mut trace = Vec::new();
        let mut period: Option<u32> = None;
        while !self.halted && self.frame < max_frames {
            if let Some(p) = period {
                if self.frame >= p {
                    break;
                }
            }
            let events = self.tick();
            let frame = self.frame;
            for ev in events {
                trace.push((frame, ev));
            }
            if period.is_none() {
                // Every channel that can loop must have wrapped once
                // before the period is well-defined; channels that
                // finished or never loop don't gate it.
                let all_settled = (0..DW_NUM_CHANNELS).all(|c| {
                    self.position_lists[c].loop_to.is_none()
                        || self.first_wrap_frame[c].is_some()
                        || self.channels[c].finished
                });
                if all_settled && self.first_wrap_frame.iter().any(|w| w.is_some()) {
                    period = Some(self.loop_period(max_frames));
                }
            }
        }
        if let Some(p) = period {
            trace.retain(|(f, _)| *f <= p);
        }
        (trace, period)
    }

    /// LCM of the per-channel pass lengths (`first_wrap_frame - 1`),
    /// capped at `max_frames`. Zero-length / never-wrapped channels
    /// are skipped. Returns 0 when no channel wrapped.
    fn loop_period(&self, max_frames: u32) -> u32 {
        fn gcd(a: u64, b: u64) -> u64 {
            if b == 0 {
                a
            } else {
                gcd(b, a % b)
            }
        }
        let mut acc: u64 = 1;
        let mut any = false;
        for f in self.first_wrap_frame.iter().flatten() {
            let p = f.saturating_sub(1) as u64;
            if p > 0 {
                acc = acc / gcd(acc, p) * p;
                any = true;
                if acc >= max_frames as u64 {
                    return max_frames;
                }
            }
        }
        if any {
            (acc as u32).min(max_frames)
        } else {
            0
        }
    }

    // ---------- internals ----------

    fn tick_channel(&mut self, ch: usize, out: &mut Vec<TickEvent>) {
        // Speed counter ratchet: tick down, only consume events
        // when it hits zero.
        if self.channels[ch].speed_counter > 1 {
            self.channels[ch].speed_counter -= 1;
            // Per-frame pitch arpeggio (Ghidra `play_tick` held-note
            // branch): while a note sustains, the channel's arpeggio
            // pointer advances one offset per frame and the offset is
            // added to the note's period-table index (semitone space).
            // We project that as a `TrackPitch` Points curve, so emit
            // the current offset whenever it changes (the lane latches
            // a value forward). Slide / vibrato remain their own lanes.
            //
            // The replayer does NOT advance the arpeggio on the *gate*
            // frame — the one where the counter reaches 1, just before
            // the next note is read (Ghidra: `counter == 1` takes the
            // DMA-gate branch, not the held branch). Skipping it keeps
            // the arpeggio phase aligned across note boundaries; without
            // it a re-armed note's cycle slips a frame (tetris `3,4`
            // re-arm gaps came out as `6`).
            if self.channels[ch].speed_counter > 1 {
                self.tick_arpeggio(ch, out);
            }
            return;
        }
        self.channels[ch].speed_counter = self.channels[ch].speed.max(1) as u16;

        // Whittaker zeroes the per-channel flag byte (`*pbVar15 = 0`,
        // slide bit included) at the head of every row read, so a
        // `Slide` only lives for the wait of its own row. Mirror that
        // by closing any live slide here — this is the `Clear` that
        // bounds the `SlideEvent::Set` the converter emitted.
        if self.channels[ch].slide_active {
            self.channels[ch].slide_active = false;
            out.push(TickEvent::SlideStop { channel: ch as u8 });
        }

        // Consume events until we hit one that ends the tick
        // (a Note, a LongWait, a WaitUntilNextRow, an EndOfTrack
        // that fails to advance, or the song end).
        //
        // Guard against a degenerate position list that loops over
        // tracks containing no tick-ending event (no Note /
        // LongWait / WaitUntilNextRow before the loop returns):
        // `advance_position` would then cycle forever *within a
        // single frame* — `self.frame` never advances, so the
        // outer `MAX_SIMULATION_FRAMES` cap can't stop it — pushing
        // events until memory is exhausted (observed on bmx
        // simulator.dw's sub-song). When a single tick consumes
        // more events than the module could legitimately hold,
        // treat the channel as finished and bail.
        let consume_cap = self
            .module
            .tracks
            .iter()
            .map(|t| t.events.len())
            .sum::<usize>()
            .saturating_add(1024);
        let mut consumed = 0usize;
        loop {
            consumed += 1;
            if consumed > consume_cap {
                self.channels[ch].finished = true;
                return;
            }
            // Make sure we have a track loaded.
            if self.channels[ch].track_index.is_none() && !self.advance_position(ch, out) {
                return;
            }
            let Some(track_idx) = self.channels[ch].track_index else {
                return;
            };
            let track: &DwTrack = &self.module.tracks[track_idx];

            // EOF of the event vector — should not happen on
            // well-formed input, but defend against it.
            if self.channels[ch].event_cursor >= track.events.len() {
                if !self.advance_position(ch, out) {
                    return;
                }
                continue;
            }

            let ev = track.events[self.channels[ch].event_cursor];
            self.channels[ch].event_cursor += 1;

            if let Some(action) = self.apply_event(ch, ev, out) {
                match action {
                    EventAction::EndTick => return,
                    EventAction::AdvancePosition => {
                        if !self.advance_position(ch, out) {
                            return;
                        }
                        // Whittaker semantics: `EndOfTrack`
                        // does not consume the tick — keep reading
                        // from the freshly loaded track.
                    }
                    EventAction::Halt => {
                        self.halted = true;
                        return;
                    }
                }
            }
            // No action returned ⇒ inline event (SetSample,
            // SetSpeed, GlobalTranspose, …): keep consuming in
            // the same tick.
        }
    }

    /// Advance the per-frame arpeggio on a sustaining channel and log
    /// the offset whenever it changes. Mirrors the replayer's held-note
    /// branch: an arpeggio armed by the `0x90`/`0xA0` bracket cycles its
    /// offset list one entry per frame, wrapping at the terminator. The
    /// pointer persists across notes (only a fresh arpeggio command
    /// resets it, see [`apply_event`]); a rest (`current_note == None`)
    /// or the trivial arpeggio yields offset `0`, which we emit so the
    /// latched Points lane releases back to the note's base pitch.
    fn tick_arpeggio(&mut self, ch: usize, out: &mut Vec<TickEvent>) {
        let off = match (
            self.channels[ch].current_arpeggio,
            self.channels[ch].current_note,
        ) {
            (Some(idx), Some(_)) => match self.module.arpeggios.get(idx as usize) {
                Some(arp) if !arp.offsets.is_empty() => {
                    let pos = self.channels[ch].arp_phase % arp.offsets.len();
                    self.channels[ch].arp_phase = self.channels[ch].arp_phase.wrapping_add(1);
                    arp.offsets[pos]
                }
                _ => 0,
            },
            _ => 0,
        };
        if self.channels[ch].arp_last_emitted != Some(off) {
            self.channels[ch].arp_last_emitted = Some(off);
            out.push(TickEvent::Arpeggio {
                channel: ch as u8,
                semitones: off,
            });
        }
    }

    fn apply_event(
        &mut self,
        ch: usize,
        ev: DwTrackEvent,
        out: &mut Vec<TickEvent>,
    ) -> Option<EventAction> {
        match ev {
            // `Note(n)` plays note `n`; `Retrigger` (cmd 0x83) re-plays
            // the channel's CURRENT note — same sample/envelope/arpeggio,
            // a fresh Paula strike — without consuming a new note. They
            // share the trigger body below: `Retrigger` resolves `n` from
            // `current_note` (a Retrigger before any note is a no-op that
            // just ends the tick, matching the replayer striking silence).
            DwTrackEvent::Note(_) | DwTrackEvent::Retrigger => {
                let n = match ev {
                    DwTrackEvent::Note(n) => n,
                    _ => match self.channels[ch].current_note {
                        Some(n) => n,
                        None => return Some(EventAction::EndTick),
                    },
                };
                self.channels[ch].current_note = Some(n);
                // Per-note Paula volume = the envelope's FIRST step.
                // Ghidra/ref `ReadTrackCommands` (1836): on trigger
                // `newVolume = EnvelopeList[1] & 0x7f` — the first
                // value, not the peak. The per-row envelope animation
                // (`attach_envelope_animation`) then ramps/decays from
                // there, so attack envelopes (env00: 1→18 over ~45
                // rows; env07: 8→64) swell in as authored. Using
                // `peak()` here started those notes at full level and
                // killed the swell; for the common decay envelopes
                // (env10 lead: 64→1) `initial() == peak()` so nothing
                // changes. `None` propagates to the DAW layer as "no
                // shaping" → `Volume::FULL`.
                let envelope_index = self.channels[ch].current_envelope;
                let env_peak = envelope_index
                    .and_then(|i| self.module.volume_envelopes.get(i as usize))
                    .and_then(|env| env.initial());
                // Fold the song's master volume into the per-note
                // level. `global_volume` is 64 (full) unless a
                // `SetGlobalVolume` / `GlobalVolumeFade` command —
                // or, on modules whose `Effect8` is a global-volume
                // offset (tetris), an `Effect8` — has lowered it.
                // When it's full and there's no envelope, keep
                // `None` so the note plays at the sample's natural
                // level (unchanged for xenon2 et al.); only emit a
                // shaped volume when there's actually something to
                // shape, so we don't bury every note under a
                // redundant `TrackEffect::Volume`.
                let gv = self.global.global_volume.min(64) as u32;
                let volume = if env_peak.is_some() || gv < 64 {
                    let base = env_peak.unwrap_or(64) as u32;
                    Some(((base * gv) / 64).min(127) as u8)
                } else {
                    None
                };
                // Sample/pitch encoding depends on the replayer
                // family:
                //
                // - **New player**: the byte is a direct period-
                //   table index; the active sample is whatever the
                //   last `SetSample` command armed on this channel.
                // - **Old player (qball)**: the byte is a composite
                //   `sample × 12 + pitch_in_octave` — every note
                //   picks its own sample from the byte, and
                //   `SetSample` commands don't apply.
                // - **Old-stream fine-tune players (leviathan, empire)**:
                //   despite the old command layout, the note byte is a
                //   *direct* full-range period index and the sample is
                //   armed by a separate `SetSample` (0xC0 bracket) — the
                //   new-player decode. `period_via_finetune` flags them.
                let (sample_index, pitch_byte) =
                    if matches!(self.module.variant, super::detect::DwVariant::Old)
                        && !self.module.period_via_finetune
                    {
                        (Some((n / 12) as u16), n % 12)
                    } else {
                        (self.channels[ch].current_sample, n)
                    };
                // Apply both transposes at note time. Ghidra
                // trace at `Play+0x2C0..0x2C4`:
                //   ADD.B (global_transpose, PC), D0b
                //   ADD.B (chan_transpose, A0), D0b
                // so the order matches: global first, then per-
                // channel. Sign-extend each to i16 so combined
                // values that walk below zero (e.g. raw note 0
                // with a -3 transpose) survive the addition
                // without truncation. Baked (not a `Pitch` lane):
                // the transpose shifts the note's *base period*, which
                // the period-space slide then subtracts from — and a
                // semitone-space `Pitch` lane applied *after* the slide
                // would scale the slide (`transpose(base−slide) ≠
                // transpose(base)−slide`). The end-of-tick re-resolution
                // below covers a transpose set the same frame the note
                // triggers (bad company).
                let effective_note = (pitch_byte as i16)
                    .saturating_add(self.global.transpose as i16)
                    .saturating_add(self.channels[ch].transpose as i16);
                out.push(TickEvent::NoteOn {
                    channel: ch as u8,
                    sample_index,
                    note: pitch_byte,
                    effective_note,
                    volume,
                    envelope_index,
                    arpeggio_index: self.channels[ch].current_arpeggio,
                });
                // Inline cadence: the inter-note wait is the last
                // `LongWait` value on this channel
                // (`chan[+0x1E] = chan[+0x1C]` at `Play+0x358`),
                // which reproduces the original tempo.
                self.channels[ch].speed_counter = self.channels[ch].next_wait_frames.max(1);
                Some(EventAction::EndTick)
            }
            DwTrackEvent::LongWait(b) => {
                // Inline — sets the wait the next Note consumes.
                let multiplier = (b - 0xDF) as u16;
                self.channels[ch].next_wait_frames =
                    multiplier.saturating_mul(self.global.initial_speed.max(1) as u16);
                None
            }
            DwTrackEvent::SetSample(raw) => {
                // SetSample index = raw byte − the per-module
                // SetSample threshold (the top dispatcher bracket).
                // On `dw.xenon 2` that bracket is `CMPI.B #0xB0`
                // (Ghidra `Play + ~0x294`), but it is module-
                // dependent — `bubble bobble.dw` uses `#0xC0`
                // (its SetVolumeEnvelope/SetPitchArpeggio brackets sit at
                // 0xB0/0xA0), so a hardcoded 0xB0 would resolve
                // `0xC3` to sample 19 instead of 3. Use the detected
                // threshold and fall back to the xenon2 value.
                let threshold = self.module.dispatcher.sample_threshold().unwrap_or(0xB0);
                let idx = raw.saturating_sub(threshold) as u16;
                self.channels[ch].current_sample = Some(idx);
                out.push(TickEvent::SampleSet {
                    channel: ch as u8,
                    sample_index: idx,
                });
                None // inline
            }
            DwTrackEvent::SetVolumeEnvelope(raw) => {
                // The `0xA0..` dispatcher bracket arms a *per-channel
                // volume envelope* on every module the importer has
                // seen (Ghidra trace on `dw.xenon 2`,
                // `Play+0x412..0x436`: byte from `chan[+0x22]` →
                // `Paula AUD0VOL` via `(byte * global_volume) >> 6`).
                // Resolve the 0-based envelope index from the
                // per-module volume-envelope threshold so a module
                // that uses a non-canonical cascade (e.g. `0xB0/0xA0`
                // only) indexes correctly.
                let threshold = self
                    .module
                    .dispatcher
                    .volume_envelope_threshold()
                    .unwrap_or(0xA0);
                let idx = raw.saturating_sub(threshold) as u16;
                if self.module.volume_bracket_is_pitch {
                    // 2-bracket modules (tetris): this bracket is a
                    // pitch arpeggio, not a volume envelope. Arm the
                    // pitch cycle and leave channel volume at full
                    // (applying the offsets as volume scaled every
                    // note down to silence). Arming resets the per-frame
                    // pointer to the start of the offset list (Ghidra:
                    // `chan+0x648 = chan+0x644`).
                    self.channels[ch].current_arpeggio = Some(idx);
                    self.channels[ch].arp_phase = 0;
                } else {
                    self.channels[ch].current_envelope = Some(idx);
                }
                None
            }
            DwTrackEvent::SetPitchArpeggio(raw) => {
                // The `0x90..` dispatcher bracket arms a per-tick
                // pitch arpeggio. Resolve the 0-based
                // arpeggio index from the per-module pitch-arpeggio
                // threshold and stash it on the channel; the next
                // `NoteOn` carries it to the DAW layer, which maps
                // the offset cycle to `TrackEffect::Arpeggio`.
                let threshold = self
                    .module
                    .dispatcher
                    .pitch_arpeggio_threshold()
                    .unwrap_or(0x90);
                let idx = raw.saturating_sub(threshold) as u16;
                self.channels[ch].current_arpeggio = Some(idx);
                // Re-arming resets the per-frame pointer to the start of
                // the offset list (Ghidra: `chan+0x648 = chan+0x644`).
                self.channels[ch].arp_phase = 0;
                None
            }
            DwTrackEvent::EndOfTrack => Some(EventAction::AdvancePosition),
            DwTrackEvent::WaitUntilNextRow => {
                // Ghidra `play_tick` case 0x83 → `LAB_00000332`:
                // `chan[+0x616] = chan[+0x614]` — the row-hold reloads
                // the channel's speed counter from the last `LongWait`
                // value, exactly like Note (`Play+0x332`) and Mute
                // (case 0x82). Without this the hold collapses to
                // `self.speed` (the vestigial top-of-`tick_channel`
                // reset, = 3 on xenon2) instead of the intended 96
                // frames, so every following `EndOfTrack` fires ~190
                // rows too early and the whole arrangement piles up —
                // audible as a scrambled track order and a spurious
                // early note (the "clack" at pattern 0 row 0x26).
                self.channels[ch].speed_counter = self.channels[ch].next_wait_frames.max(1);
                Some(EventAction::EndTick)
            }
            DwTrackEvent::StopSong => {
                out.push(TickEvent::SongEnd { channel: ch as u8 });
                Some(EventAction::Halt)
            }
            DwTrackEvent::Mute => {
                // cmd 0x82 **ends the tick** (Ghidra `play_tick`
                // case 0x82: `chan[+0x616] = chan[+0x614]` then
                // `goto` the next channel), playing silence for
                // `next_wait_frames`. It is the rest primitive — a
                // Mute before a channel's first note delays that
                // note, which is exactly what staggers the voices'
                // entries: xenon2's channels 1–3 open with a Mute
                // while channel 0 does not, so channel 0 enters at
                // row 0 alone and the others come in later. (The old
                // inline handling made every rest instantaneous, so
                // all four voices fired on frame 1.)
                self.channels[ch].current_note = None;
                out.push(TickEvent::Mute { channel: ch as u8 });
                self.channels[ch].speed_counter = self.channels[ch].next_wait_frames.max(1);
                Some(EventAction::EndTick)
            }
            DwTrackEvent::SetSpeed(s) => {
                self.channels[ch].speed = s.max(1);
                out.push(TickEvent::SpeedChange {
                    channel: ch as u8,
                    speed: s,
                });
                None
            }
            DwTrackEvent::GlobalTranspose(t) => {
                self.global.transpose = t;
                None
            }
            DwTrackEvent::SetGlobalVolume(v) => {
                self.global.global_volume = v;
                None
            }
            DwTrackEvent::GlobalVolumeFade(speed) => {
                self.global.global_volume_fade_speed = speed;
                self.global.global_volume_fade_counter = speed;
                None
            }
            DwTrackEvent::StartVibrato { speed, max } => {
                out.push(TickEvent::VibratoStart {
                    channel: ch as u8,
                    speed,
                    depth: max,
                });
                None // inline — vibrato runs alongside the note
            }
            DwTrackEvent::StopVibrato => {
                out.push(TickEvent::VibratoStop { channel: ch as u8 });
                None
            }
            DwTrackEvent::Slide { speed, counter } => {
                out.push(TickEvent::SlideStart {
                    channel: ch as u8,
                    speed,
                    counter,
                });
                // Mark the slide live so the next row read emits the
                // bounding `SlideStop` (Whittaker clears the flag each
                // row — see `tick_channel`).
                self.channels[ch].slide_active = true;
                None // inline — slide accumulates alongside the note
            }
            DwTrackEvent::Effect8(arg) => {
                // The `Effect8` opcode is module-
                // dependent: it can mean half-volume toggle,
                // global volume fade, or per-channel transpose,
                // depending on which detect flags are set. The
                // Ghidra trace on `dw.xenon 2` (cmd 8 handler
                // at 0x598: `MOVE.B (A1)+, (0x3, A0)`) shows the
                // module-specific binding is per-channel
                // transpose — the byte is written to the
                // `chan[+3]` slot the note-trigger code reads
                // back at 0x2C4. Gate the assignment on the
                // detect-time flag so modules whose `Effect8`
                // dispatcher means something else aren't given
                // a spurious transpose.
                if self.module.features.enable_channel_transpose {
                    self.channels[ch].transpose = arg as i8;
                } else {
                    // Modules without channel-transpose bind
                    // `Effect8` to a **global volume offset**
                    // instead. Ghidra (tetris `PlayTick`): the
                    // arg is stored and the per-note Paula volume
                    // becomes `sample_vol(0x40) - arg`, i.e. the
                    // master volume drops to `64 - arg`. Clamp to
                    // the Paula 0..=64 range. This is what makes
                    // tetris's dynamics (20 `Effect8` events) audible
                    // instead of silently ignored.
                    self.global.global_volume = 64u8.saturating_sub(arg);
                }
                None
            }
            DwTrackEvent::Effect9(_arg) => {
                // Counterpart to `Effect8`. On the channel-transpose
                // family it's unused; on the global-volume family
                // (tetris) it restores full master volume. There
                // are other per-module bindings (half-volume
                // off, restart) we don't yet need on the corpus.
                if !self.module.features.enable_channel_transpose {
                    self.global.global_volume = 64;
                }
                None
            }
            // Sound-effect channel commands: ignored — they drive a
            // host-triggered background FX voice (Ghidra: a separate
            // per-channel engine, never armed by the music data) that
            // the data-driven decoder doesn't model.
            DwTrackEvent::StartOrStopSoundFx(_) | DwTrackEvent::StopSoundFx => None,
            // Position-sequencer pointer: resolved into the position
            // list's `loop_to` at import (`follow_seq_ptr_loops`), so the
            // simulator — which walks the rebuilt list — treats it as a
            // no-op.
            DwTrackEvent::SeqPtr(_) => None,
        }
    }

    /// Load the track referenced at the channel's current
    /// position, or — when called on a channel that has just
    /// finished a track — step to the next position first.
    /// Returns `false` when the channel has finished (no more
    /// entries, no loop target).
    ///
    /// The initial-load vs. mid-song-advance distinction is
    /// keyed off `track_index`: a `None` at position 0 is
    /// treated as "before any read" and loads position 0 in
    /// place; any other state means we're advancing past the
    /// just-finished track.
    fn advance_position(&mut self, ch: usize, out: &mut Vec<TickEvent>) -> bool {
        let _ = out; // reserved for future ChannelFinished emission
        let list = &self.position_lists[ch];
        if list.is_empty() {
            self.channels[ch].finished = true;
            return false;
        }

        let is_initial_load =
            self.channels[ch].track_index.is_none() && self.channels[ch].position_index == 0;

        if !is_initial_load {
            let cur = self.channels[ch].position_index;
            let next = cur + 1;
            if next >= list.entries.len() {
                match list.loop_to {
                    Some(target) => {
                        // First wrap = one full pass of this channel's
                        // list. Record it for the song-loop-period
                        // (LCM) computation in the DAW projection.
                        if self.first_wrap_frame[ch].is_none() {
                            self.first_wrap_frame[ch] = Some(self.frame);
                        }
                        self.channels[ch].position_index = target as usize;
                    }
                    None => {
                        self.channels[ch].finished = true;
                        return false;
                    }
                }
            } else {
                self.channels[ch].position_index = next;
            }
        }

        // Record the loop-body start: the first frame this channel
        // reaches its `loop_to` target entry. For `loop_to = 0` that is
        // the initial load (→ tick 0, whole pass loops); for a channel
        // whose intro `SeqPtr`-redirects to a sub-sequence the intro
        // plays once and the body starts here. Used by `to_module` for
        // `ChannelLoop::start_tick`.
        let pos = self.channels[ch].position_index;
        if self.position_lists[ch].loop_to == Some(pos as u32)
            && self.loop_start_frame[ch].is_none()
        {
            self.loop_start_frame[ch] = Some(self.frame);
        }

        let entry = self.position_lists[ch].entries[pos];
        let track_idx = self.module.tracks.iter().position(|t| t.offset == entry);
        self.channels[ch].track_index = track_idx;
        self.channels[ch].event_cursor = 0;
        track_idx.is_some()
    }
}

/// Outcome returned by [`Simulator::apply_event`] to drive the
/// inner consume-loop.
enum EventAction {
    /// Stop reading events for this tick; the channel's
    /// `speed_counter` will throttle the next read.
    EndTick,
    /// Move the channel to its next position-list entry, then
    /// keep reading inside the same tick (Whittaker semantics).
    AdvancePosition,
    /// The whole simulation halts after this tick.
    Halt,
}

impl DwModule {
    /// Run the full simulation and return the event trace.
    /// Convenience wrapper for [`Simulator::run`].
    pub fn simulate(&self) -> Vec<(u32, TickEvent)> {
        Simulator::new(self).run()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: a `WaitUntilNextRow` (`0x83`) row-hold must reuse
    /// the channel's last `LongWait` value, not collapse to the
    /// vestigial `self.speed`. Ghidra `play_tick` case 0x83:
    /// `chan[+0x616] = chan[+0x614]`. A track `LongWait(6f); Note;
    /// WaitUntilNextRow; Note` must space the two notes by the full
    /// 6-frame wait *twice* (note @1, hold consumed @7, note @13) —
    /// not @1 then @9, which is what the pre-fix code produced and
    /// which scrambled xenon2's whole track order.
    #[test]
    fn wait_until_next_row_holds_for_long_wait() {
        use super::super::event::DwTrackEvent as Ev;
        use super::super::header::{DwSubSong, DwTrack};

        let mut position_lists: [DwPositionList; DW_NUM_CHANNELS] = Default::default();
        position_lists[0] = DwPositionList {
            entries: alloc::vec![10],
            loop_to: None,
        };
        let track = DwTrack {
            offset: 10,
            bytes: alloc::vec![],
            // LongWait(0xE2) → (0xE2-0xDF)=3 × speed(2) = 6 frames.
            events: alloc::vec![
                Ev::LongWait(0xE2),
                Ev::Note(36),
                Ev::WaitUntilNextRow,
                Ev::Note(40),
                Ev::EndOfTrack,
            ],
        };
        let module = DwModule {
            variant: super::super::detect::DwVariant::New,
            period_table: super::super::tables::PeriodTable::P2,
            period_via_finetune: true,
            samples: alloc::vec::Vec::new(),
            sub_song: Some(DwSubSong {
                speed: 2,
                delay_speed: 0,
                channel_position_offsets: [0; DW_NUM_CHANNELS],
            }),
            sub_songs: alloc::vec::Vec::new(),
            selected_sub_song: None,
            position_lists,
            all_position_lists: alloc::vec::Vec::new(),
            tracks: alloc::vec![track],
            volume_envelopes: alloc::vec::Vec::new(),
            arpeggios: alloc::vec::Vec::new(),
            dispatcher: Default::default(),
            features: Default::default(),
            volume_bracket_is_pitch: false,
            channel_volumes: [64; DW_NUM_CHANNELS],
            master_volume: None,
            use_pitch_arpeggio: false,
            use_arpeggio_pitch_lane: false,
        };

        let trace = Simulator::new(&module).run();
        let note_frames: Vec<u32> = trace
            .iter()
            .filter_map(|(f, ev)| matches!(ev, TickEvent::NoteOn { .. }).then_some(*f))
            .collect();
        assert_eq!(
            note_frames,
            alloc::vec![1, 13],
            "WaitUntilNextRow must hold for the LongWait span (6 frames), \
             not the default speed"
        );
    }

    #[test]
    fn empty_module_halts_immediately() {
        // Hand-craft a module with no position lists. Should
        // halt on the first tick without panicking.
        let module = DwModule {
            variant: super::super::detect::DwVariant::New,
            period_table: super::super::tables::PeriodTable::P2,
            period_via_finetune: true,
            samples: alloc::vec::Vec::new(),
            sub_song: None,
            sub_songs: alloc::vec::Vec::new(),
            selected_sub_song: None,
            position_lists: Default::default(),
            all_position_lists: alloc::vec::Vec::new(),
            tracks: alloc::vec::Vec::new(),
            volume_envelopes: alloc::vec::Vec::new(),
            arpeggios: alloc::vec::Vec::new(),
            dispatcher: Default::default(),
            features: Default::default(),
            volume_bracket_is_pitch: false,
            channel_volumes: [64; DW_NUM_CHANNELS],
            master_volume: None,
            use_pitch_arpeggio: false,
            use_arpeggio_pitch_lane: false,
        };
        let mut sim = Simulator::new(&module);
        sim.tick();
        assert!(sim.halted);
    }
}
