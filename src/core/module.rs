//! [`Module`] — top-level container for a SoundTracker song.
//!
//! Holds metadata, the instrument bank, the DAW layer (tracks,
//! clips, automation lanes, timeline_map), the native signal graph
//! (per-channel insert chains, aux/return buses + sends, master
//! chain) and shared audio asset pool, playback quirks, and
//! IT-specific extras (`midi_macros`, `mix_plugins`, `mix_volume`,
//! `pitch_wheel_depth`). The signal-graph and asset fields are empty
//! for every tracker import (⇒ inert ⇒ bit-identical render).
//!
//! A `Module` is constructed three ways:
//! - via importers (`Module::load`, `load_xm`, `load_it`, …) which
//!   parse a tracker file (XM / IT / S3M / MOD / SID) and call
//!   [`crate::tracker::import::build::build_timeline_layer`];
//! - via `Module::default()` for editor-authored content;
//! - via deserialisation from any serde format (postcard / json /
//!   bincode / …).
//!
//! After construction, the DAW layer should only be mutated through
//! the atomic [`crate::edit`] API so the verify-layers invariants
//! stay coherent.

use serde::{Deserialize, Serialize};

use crate::core::cell::Cell;
use crate::core::compatibility::PlaybackQuirks;
use crate::core::daw::automation::AutomationLane;
use crate::core::daw::clip::Clip;
use crate::core::daw::sorted_clips::SortedClips;
use crate::core::daw::timeline::TimelineMap;
use crate::core::daw::track::Track;
use crate::core::fixed::units::{ChannelVolume, Panning, Volume};
use crate::core::instrument::Instrument;
#[cfg(feature = "import_it")]
use crate::tracker::mix_plugin::MixPlugins;
use crate::tracker::period::FrequencyType;

use alloc::string::String;
use alloc::string::ToString;
use alloc::{vec, vec::Vec};

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
    /// Playback behaviour switches that match the authoring tracker.
    /// Importers populate this via the preset constructors in
    /// `crate::tracker::profiles` (gated behind any `import_*`
    /// feature). Editor-authored modules typically leave this at
    /// [`PlaybackQuirks::default()`] for clean, quirk-free playback.
    pub quirks: PlaybackQuirks,

    /// Tag identifying the tracker that authored this module.
    /// Metadata only — playback decisions read [`Self::quirks`],
    /// not this field. Set by importers; left `None` for
    /// editor-authored modules. Only present when at least one
    /// `import_*` feature is enabled (the tag has no meaning
    /// without any tracker importer compiled in).
    #[cfg(any(
        feature = "import_mod",
        feature = "import_xm",
        feature = "import_s3m",
        feature = "import_it",
        feature = "import_sid",
        feature = "import_dw",
    ))]
    pub origin: Option<crate::tracker::format::ModuleFormat>,
    pub frequency_type: FrequencyType,
    pub default_tempo: usize,
    pub default_bpm: usize,
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
    /// embedded plugin section. Gated on `import_it`: a build
    /// without the IT importer drops the field entirely.
    #[cfg(feature = "import_it")]
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

    // ---- DAW layer ----
    //
    // Filled by importers via
    // [`crate::tracker::import::build::build_timeline_layer`] and consumed
    // by the player.
    /// Library of reusable tracks (DAW layer).
    #[serde(default)]
    pub tracks: Vec<Track>,

    /// Placement of tracks on the timeline. The
    /// [`crate::core::daw::sorted_clips::SortedClips`] wrapper keeps the
    /// `(song, target_channel, position_tick)` ascending invariant
    /// — every `(song, target_channel)` lane is a contiguous slice
    /// suitable for binary search.
    #[serde(default)]
    pub clips: SortedClips,

    /// Automation lanes. Empty at import; mutated only through the
    /// [`crate::edit`] API.
    #[serde(default)]
    pub automation: Vec<AutomationLane>,

    /// Linearised playback order: maps each visited `(song,
    /// pattern_idx, row_idx)` to its absolute tick + per-row speed
    /// and BPM. The DAW layer's authoritative bridge between the
    /// navigation coordinates effects refer to (`PositionJump`,
    /// `PatternBreak`) and the tick-aligned [`Clip`] timeline.
    #[serde(default)]
    pub timeline_map: TimelineMap,

    /// Absolute tick the song loops back to once playback runs off
    /// the end of [`Self::timeline_map`]. Encodes the historical
    /// XM / MOD "restart byte" — and also any in-pattern `Bxx` /
    /// `Dxx` jump that loops back into already-walked rows (detected
    /// by [`crate::tracker::import::build::build_timeline_map`] and
    /// forwarded here at import time; the in-row jump takes
    /// precedence over the header byte).
    ///
    /// `None` means the sequencer falls back to "wrap to the first
    /// entry of the current sub-song" at end-of-timeline.
    ///
    /// **Scope**: this is a single tick, conceptually keyed against
    /// sub-song 0. Multi-sub-song layouts (XM / IT splits on `0xFE`
    /// / `0xFF` order markers) are uncommon — every historical
    /// tracker (FT2 / OpenMPT / Schism) treats those markers as
    /// skip / end inside a single playable song — and a per-sub-song
    /// wrap table is not modelled here. Sub-songs > 0 fall back to
    /// wrap-to-first-entry.
    #[serde(default)]
    pub song_loop_to: Option<u32>,

    /// Per-lane independent loops, for replayers whose voices each
    /// loop their own sequence at their own length (David Whittaker
    /// and most custom C64 / Amiga drivers — see
    /// [`crate::core::daw::loop_region::ChannelLoop`]).
    ///
    /// Keyed implicitly by each entry's `(song, channel)`. **Empty for
    /// every tracker format** (MOD / XM / IT / S3M): those wrap as a
    /// whole via [`Self::song_loop_to`], and an empty `channel_loops`
    /// leaves [`Self::row_at`] on the byte-for-byte global-wrap path.
    /// An importer only populates this when a sub-song's voices have
    /// *unequal* loop lengths; voices that share one period stay on the
    /// global-wrap path.
    #[serde(default)]
    pub channel_loops: Vec<crate::core::daw::loop_region::ChannelLoop>,

    /// RFC §3B — per-mixer-channel insert device chains (the native
    /// signal-graph plumbing). Indexed by mixer-channel number; a
    /// missing or empty entry is a pass-through. **Empty for every
    /// tracker import** — the importers never author inserts, so the
    /// mixer skips the chain and the render stays bit-identical.
    /// Distinct from `mix_plugins` (a field that only exists under
    /// `import_it`, hence no link), which holds opaque, host-only VST
    /// chunks the player cannot execute.
    #[serde(default)]
    pub channel_inserts: Vec<crate::core::daw::device::DeviceChain>,

    /// RFC §3B — return/aux buses (each a named insert chain). Channels
    /// feed them via [`Self::channel_sends`]; each bus's processed
    /// output is summed back into the master mix. **Empty for every
    /// tracker import.**
    #[serde(default)]
    pub buses: Vec<crate::core::daw::device::Bus>,

    /// RFC §3B — per-mixer-channel sends into [`Self::buses`]. Indexed
    /// by mixer-channel number; each entry is that channel's list of
    /// `(bus, level)` sends. **Empty for every tracker import.**
    #[serde(default)]
    pub channel_sends: Vec<Vec<crate::core::daw::device::Send>>,

    /// RFC §3B — the **master bus** insert chain, applied to the final
    /// summed mix (channels + bus returns) before the output gain.
    /// Empty ⇒ pass-through ⇒ bit-identical to the pre-3B mixer.
    #[serde(default)]
    pub master_chain: crate::core::daw::device::DeviceChain,

    /// RFC §4 — the shared **audio asset pool**. A
    /// [`Track::Audio`] whose
    /// `source` is
    /// [`AudioSource::Pooled(i)`](crate::core::daw::track::AudioSource::Pooled)
    /// reads `assets[i]`, so one recording placed many times on the
    /// timeline is stored once. This generalises §1A's per-instrument
    /// `Arc` PCM sharing into a module-level, index-referenced registry.
    /// **Empty for every tracker import** — importers only ever produce
    /// inline samples, so nothing references the pool and the render is
    /// bit-identical. Resolve a track against it with
    /// [`Self::track_audio`].
    #[serde(default)]
    pub assets: Vec<crate::core::sample::Sample>,
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
            quirks: PlaybackQuirks::default(),
            #[cfg(any(
                feature = "import_mod",
                feature = "import_xm",
                feature = "import_s3m",
                feature = "import_it",
                feature = "import_sid",
                feature = "import_dw",
            ))]
            origin: None,
            frequency_type: FrequencyType::LinearFrequencies,
            default_tempo: 6,
            default_bpm: 125,
            channel_names: vec![],
            channel_defaults: vec![],
            instrument: vec![],
            midi_macros: None,
            pattern_highlight: PatternHighlight::default(),
            #[cfg(feature = "import_it")]
            mix_plugins: None,
            pitch_wheel_depth: 2,
            mix_volume: Volume::FULL,
            tracks: vec![],
            clips: SortedClips::new(),
            automation: vec![],
            timeline_map: TimelineMap::default(),
            song_loop_to: None,
            channel_loops: vec![],
            channel_inserts: vec![],
            buses: vec![],
            channel_sends: vec![],
            master_chain: crate::core::daw::device::DeviceChain::default(),
            assets: vec![],
        }
    }
}

impl Module {
    /// Extract a rectangular block of cells from the requested
    /// `tracks` and `[row_start, row_end)` row window. Output
    /// layout: `out\[i\]\[j\]` is the cell at
    /// `tracks\[i\].rows\[row_start + j\]`. Rows past the end of a
    /// Track default to
    /// [`crate::core::cell::Cell::default()`] so the rectangle is always
    /// fully filled — paste operations stay aligned to the
    /// requested window.
    ///
    /// Read-only — see [`crate::edit::EditCommand::PasteBlock`] for
    /// the mutating counterpart.
    pub fn copy_block(
        &self,
        tracks: &[u32],
        row_start: u32,
        row_end: u32,
    ) -> alloc::vec::Vec<alloc::vec::Vec<Cell>> {
        use alloc::vec::Vec;
        if row_end <= row_start {
            return Vec::new();
        }
        let len = (row_end - row_start) as usize;
        tracks
            .iter()
            .map(|&t| {
                let mut col: Vec<Cell> = Vec::with_capacity(len);
                let track = self.tracks.get(t as usize);
                for r in row_start..row_end {
                    let cell = track.map(|tr| tr.cell_at(r)).unwrap_or_default();
                    col.push(cell);
                }
                col
            })
            .collect()
    }

    /// First [`AutomationLane`] whose `target == target`. The runtime
    /// fetches the relevant LFO / Slide / Glide lane for a given
    /// Track-keyed modulation class (`TrackPitch(n)`,
    /// `TrackVolume(n)`, …) through this accessor. Returns `None`
    /// if no lane was extracted for that target — common for
    /// editor-authored modules that don't carry the matching
    /// effect class.
    pub fn lane_for(
        &self,
        target: crate::core::daw::automation::AutomationTarget,
        song: u16,
    ) -> Option<&AutomationLane> {
        self.automation
            .iter()
            .find(|l| l.target == target && l.song == song)
    }

    /// All [`AutomationLane`]s whose `target == target`. There can
    /// be more than one when a Track is hit by multiple modulation
    /// classes (e.g. Vibrato → LFO lane *and* Portamento → Slide
    /// lane, both targeting `TrackPitch(n)`).
    pub fn lanes_for<'a>(
        &'a self,
        target: crate::core::daw::automation::AutomationTarget,
        song: u16,
    ) -> impl Iterator<Item = &'a AutomationLane> + 'a {
        self.automation
            .iter()
            .filter(move |l| l.target == target && l.song == song)
    }

    /// Number of channels visible to the player: the smallest
    /// envelope that holds every clip's `target_channel`. Channels
    /// past the rightmost clip are silent by construction and
    /// don't need to be materialised.
    pub fn get_num_channels(&self) -> usize {
        // Max over every song — the player allocates one set of
        // voices that covers all sub-songs the module carries. The
        // sort groups by song first so we can't read this off the
        // last entry; a full pass is the simplest correct answer.
        self.clips
            .iter()
            .map(|c| c.target_channel as usize + 1)
            .max()
            .unwrap_or(0)
    }

    /// RFC §4 — register a recorded waveform in the shared
    /// [`Self::assets`] pool and return its index, so a
    /// [`Track::Audio`] can
    /// reference it via
    /// [`AudioSource::Pooled`](crate::core::daw::track::AudioSource::Pooled).
    /// Appends unconditionally; the same index handed to several clips
    /// is how one recording is stored once and played many times.
    pub fn push_asset(&mut self, sample: crate::core::sample::Sample) -> usize {
        let idx = self.assets.len();
        self.assets.push(sample);
        idx
    }

    /// RFC §4 — resolve a track's recorded audio against the asset pool.
    /// Returns `(sample, source_rate)` for a
    /// [`Track::Audio`] whose
    /// `source` is inline or a *live* pool reference; `None` if the
    /// track isn't Audio or its
    /// [`Pooled`](crate::core::daw::track::AudioSource::Pooled) index is
    /// dangling. This is the pool-aware counterpart to the inline-only
    /// [`Track::audio_sample`](crate::core::daw::track::Track::audio_sample);
    /// the player resolves every clip through here.
    pub fn track_audio(&self, track_idx: usize) -> Option<(&crate::core::sample::Sample, u32)> {
        use crate::core::daw::track::{AudioSource, Track};
        let (source, rate) = match self.tracks.get(track_idx)? {
            Track::Audio {
                source,
                source_rate,
                ..
            } => (source, *source_rate),
            _ => return None,
        };
        let sample = match source {
            AudioSource::Inline(s) => s,
            AudioSource::Pooled(i) => self.assets.get(*i)?,
        };
        Some((sample, rate))
    }

    // ---- Derived accessors over the DAW layer ----
    //
    // Resolve queries about song shape (length, pattern index at
    // an order position, pattern length) from `timeline_map.entries`.
    // O(n) but called at most once per row by the sequencer — well
    // under the cost of the audio mixing loop.

    /// Number of order entries for `song`. Derived from the
    /// timeline_map.
    pub fn song_length(&self, song: usize) -> usize {
        self.timeline_map.order_count(song)
    }

    /// Pattern index referenced at `(song, order_idx)`. Returns `None`
    /// if out of range.
    pub fn order_pattern_idx(&self, song: usize, order_idx: usize) -> Option<usize> {
        self.timeline_map
            .find_order_entry(song, order_idx)
            .map(|e| e.pattern_idx as usize)
    }

    /// Maximum row index + 1 ever played for `pat_idx`, derived from
    /// the timeline_map entries that target this pattern.
    pub fn pattern_row_count(&self, pat_idx: usize) -> usize {
        self.timeline_map.row_count_in_pattern(pat_idx)
    }

    /// First clip whose `[position_tick, end_tick)` collides with
    /// `candidate`'s on its `(song, target_channel)` lane. Indices
    /// listed in `ignore` are skipped (typically the index of a clip
    /// being moved or resized in place).
    ///
    /// Operates on the contiguous lane slice rather than the full
    /// clip vector — `O(log N + lane_size)` instead of `O(N)`.
    pub fn find_overlapping_clip(&self, candidate: &Clip, ignore: &[u32]) -> Option<u32> {
        let range = self
            .clips
            .lane_range(candidate.song, candidate.target_channel);
        let lane_start = range.start;
        let cand_end = candidate.end_tick(self);
        for (offset, existing) in self.clips.as_slice()[range].iter().enumerate() {
            let global_idx = (lane_start + offset) as u32;
            if ignore.contains(&global_idx) {
                continue;
            }
            let existing_end = existing.end_tick(self);
            let disjoint =
                cand_end <= existing.position_tick || existing_end <= candidate.position_tick;
            if !disjoint {
                return Some(global_idx);
            }
        }
        None
    }

    /// The [`crate::core::daw::loop_region::ChannelLoop`] for lane
    /// `(song, channel)`, if any. `None` for every lane of a tracker
    /// module (their `channel_loops` is empty) — the common case, so
    /// the linear scan is over an empty `Vec`. Returns a copy so call
    /// sites can use it while `&self.clips` is also borrowed.
    pub fn channel_loop(
        &self,
        song: u16,
        channel: u8,
    ) -> Option<crate::core::daw::loop_region::ChannelLoop> {
        self.channel_loops
            .iter()
            .find(|l| l.song == song && l.channel == channel)
            .copied()
    }

    /// Materialise one row of cells for playback, resolved through
    /// the DAW timeline. Returns a fresh
    /// `Vec<(Cell, Option<usize>)>` of length `num_channels`. The
    /// second value is the instrument the playing Track binds to
    /// (when one is active); the replayer carries this alongside
    /// the [`Cell`] because the cell event itself does not carry a
    /// per-cell instrument index — the instrument is invariant for
    /// a track. Channels with no active clip get
    /// `(Cell::default(), None)`.
    ///
    /// # Per-channel resolution
    ///
    /// For each channel, find the active clip via binary search on
    /// `module.clips` (sorted by `(song, target_channel,
    /// position_tick)`), then map the source row to a track row
    /// via `row_in_track = row_idx - clip.source_start_row`.
    ///
    /// The active clip is the one with the greatest
    /// `position_tick ≤ abs_tick`. Its `[position_tick, end_tick)`
    /// containment is then checked — a clip that just ended at
    /// exactly `abs_tick` is *not* active and must not leak into
    /// the next pattern's first row.
    ///
    /// # Two dispatch paths
    ///
    /// 1. [`Track::Notes`]: the cell at `rows[row_in_track]` is
    ///    cloned into the output.
    ///
    /// 2. [`Track::Euclidean`]: the per-row content is *generated*
    ///    from the variant's `(events, steps, rotation)`. The
    ///    prototype is emitted on Bjorklund pulse positions,
    ///    `Empty` elsewhere. The rhythm loops every `steps` rows;
    ///    its duration is bounded by the clip's
    ///    `[position_tick, end_tick)`.
    ///
    /// # Complexity
    ///
    /// Per-channel cost is `O(log N_clips)` thanks to the
    /// canonical clip sort; the Bresenham Bjorklund recompute on
    /// the Euclidean path is `O(steps)` (≤ 255 in practice).
    pub fn row_at(
        &self,
        song: usize,
        pat_idx: usize,
        row_idx: usize,
    ) -> alloc::vec::Vec<(Cell, Option<usize>)> {
        // The global-wrap playhead is just this row's timeline tick.
        let playhead = match self.timeline_map.find_entry(song, pat_idx, row_idx) {
            Some(e) => e.tick,
            None => {
                return (0..self.get_num_channels())
                    .map(|_| (Cell::default(), None))
                    .collect()
            }
        };
        self.row_at_with_playhead(song, pat_idx, row_idx, playhead)
    }

    /// [`Self::row_at`] with an explicit `playhead_tick` as the
    /// per-lane fold base, instead of deriving it from this row's
    /// timeline tick. The player passes a **monotonic** playhead (one
    /// that keeps increasing across timeline replays) so that under
    /// per-lane free-run each voice keeps drifting instead of snapping
    /// back when the timeline loops. When `playhead_tick` equals the
    /// row's timeline tick — every tracker song, and the primary pass
    /// of a looping song — this is identical to [`Self::row_at`].
    pub fn row_at_with_playhead(
        &self,
        song: usize,
        _pat_idx: usize,
        row_idx: usize,
        playhead_tick: u32,
    ) -> alloc::vec::Vec<(Cell, Option<usize>)> {
        let num_channels = self.get_num_channels();
        let mut out: alloc::vec::Vec<(Cell, Option<usize>)> =
            (0..num_channels).map(|_| (Cell::default(), None)).collect();

        // Fold base for per-lane loops; monotonic across timeline
        // replays under free-run, else this row's tick.
        let abs_tick = playhead_tick;
        let song_u16 = song as u16;
        let global_row = row_idx as u32;
        for (ch_idx, slot) in out.iter_mut().enumerate() {
            // Per-lane loop fold. A lane with a `ChannelLoop` repeats
            // its `[start_tick, end_tick)` body independently of the
            // others (Whittaker-style drifting voices): fold the
            // playhead tick into that body, then map the folded tick
            // back to the row it lands on. With no `ChannelLoop` for
            // this lane — every tracker format — `local_tick ==
            // abs_tick` and `local_row == global_row`, so this is the
            // byte-for-byte global-wrap path. The reverse lookup only
            // fires once the lane has wrapped (`abs_tick >= end_tick`);
            // the first pass keeps `global_row` directly.
            let (local_tick, local_row) = match self.channel_loop(song_u16, ch_idx as u8) {
                Some(l) => {
                    let folded = l.fold_tick(abs_tick);
                    if folded == abs_tick {
                        (abs_tick, global_row)
                    } else {
                        match self.timeline_map.entry_at_tick(song, folded) {
                            Some(e) => (folded, e.row_idx),
                            // Folded tick has no row (shouldn't happen
                            // for a row-aligned region) → lane silent.
                            None => continue,
                        }
                    }
                }
                None => (abs_tick, global_row),
            };
            let Some((_, clip)) = self.clips.active_at(song_u16, ch_idx as u8, local_tick) else {
                continue;
            };
            // `active_at` returns the clip with the greatest
            // `position_tick ≤ local_tick` on this lane; it can
            // legitimately have ended already, so the half-open
            // `[position_tick, end_tick)` containment must be
            // checked here. Without it, the cells of the clip that
            // *just ended* leak into the next pattern's first row.
            if local_tick >= clip.end_tick {
                continue;
            }
            // Source-row-based index: independent of per-row speed,
            // so a `SongLevelEffect::Speed` mid-clip can't alias two
            // source rows onto the same track index.
            if local_row < clip.source_start_row {
                continue;
            }
            let row_in_track = local_row - clip.source_start_row;
            if row_in_track < clip.track_row_offset {
                continue;
            }
            let track = match clip.track_in(self) {
                Some(t) => t,
                None => continue,
            };
            // The track-level instrument that the replayer should
            // route to for *this cell* on *this channel*. The
            // sentinel `EFFECT_ONLY_INSTRUMENT` carries no real
            // instrument — it surfaces as `None` so the player
            // dispatches the cell's effects without trying to load
            // a sample.
            let track_instr_idx = track.instrument();
            let track_instr =
                if track_instr_idx == crate::tracker::import::build::EFFECT_ONLY_INSTRUMENT {
                    None
                } else {
                    Some(track_instr_idx)
                };
            match track {
                Track::Notes { rows, .. } => {
                    if (row_in_track as usize) >= rows.len() {
                        continue;
                    }
                    slot.0 = rows[row_in_track as usize].clone();
                    slot.1 = track_instr;
                }
                Track::Euclidean {
                    events,
                    steps,
                    rotation,
                    prototype,
                    ..
                } => {
                    let steps_u32 = (*steps).max(1) as u32;
                    let pos = row_in_track % steps_u32;
                    let rot = (*rotation as u32) % steps_u32;
                    let bjork_idx = ((pos + steps_u32 - rot) % steps_u32) as usize;
                    let canonical = crate::core::daw::euclidean::euclidean_pattern(*events, *steps);
                    if canonical.get(bjork_idx).copied().unwrap_or(false) {
                        slot.0 = prototype.clone();
                        slot.1 = track_instr;
                    }
                }
                // Audio tracks carry no cells — leave the default slot.
                Track::Audio { .. } => {}
            }
        }
        out
    }

    /// Verify that the DAW layer (`tracks` / `clips` / `automation`
    /// / `timeline_map`) is internally consistent: clips sorted and
    /// non-overlapping per `(song, target_channel)`, automation
    /// values matched against their lane's target, timeline_map
    /// entries monotonic per song, tracks pointing at valid
    /// instruments. Used by importers and tests.
    pub fn verify_layers_consistent(&self) -> Result<(), LayerInconsistency> {
        // 1. clips sorted by (song, target_channel, position_tick).
        // `SortedClips` enforces this invariant internally, so this
        // pass is a sanity belt-and-braces against deserialisation
        // sinks that bypass the type's API.
        for w in self.clips.as_slice().windows(2) {
            let a = (w[0].song, w[0].target_channel, w[0].position_tick);
            let b = (w[1].song, w[1].target_channel, w[1].position_tick);
            if a > b {
                return Err(LayerInconsistency::ClipsUnsorted);
            }
        }

        // 2. clip.track must point inside `tracks`
        for (i, clip) in self.clips.iter().enumerate() {
            if clip.track as usize >= self.tracks.len() {
                return Err(LayerInconsistency::ClipTrackOutOfRange {
                    clip_idx: i,
                    track: clip.track,
                });
            }
        }

        // 3. No overlap on (song, target_channel)
        for i in 0..self.clips.len() {
            let ci = &self.clips[i];
            let end_i = ci.end_tick(self);
            for j in (i + 1)..self.clips.len() {
                let cj = &self.clips[j];
                if cj.song != ci.song || cj.target_channel != ci.target_channel {
                    continue;
                }
                if cj.position_tick >= end_i {
                    // No further overlap possible with i on this
                    // (song, channel) — clips are sorted by
                    // position_tick. break out of inner loop.
                    break;
                }
                return Err(LayerInconsistency::ClipsOverlap {
                    clip_a: i,
                    clip_b: j,
                });
            }
        }

        // 4. tracks: instrument index in range. The
        // `EFFECT_ONLY_INSTRUMENT` sentinel marks a track whose
        // cells modify a voice inherited from a prior pattern; it
        // intentionally points at no real instrument.
        // A `Track`'s instrument is the single source of truth —
        // cells do not carry one.
        for (i, t) in self.tracks.iter().enumerate() {
            let instr = t.instrument();
            if instr == crate::tracker::import::build::EFFECT_ONLY_INSTRUMENT {
                continue;
            }
            if instr >= self.instrument.len() {
                return Err(LayerInconsistency::TrackInstrumentMismatch {
                    track_idx: i,
                    row: 0,
                });
            }
        }

        // 5. Automation lanes: typed values + sorted points (Points
        // kind only — Lfo/Slide/Glide kinds carry their own
        // tick-sorted event lists with no per-event value typing,
        // so the matrix below doesn't apply to them).
        for (i, lane) in self.automation.iter().enumerate() {
            if let Some(points) = lane.points() {
                let mut last_tick: Option<u32> = None;
                for (j, p) in points.iter().enumerate() {
                    if !p.value.is_compatible_with(lane.target) {
                        return Err(LayerInconsistency::AutomationTypeMismatch {
                            lane_idx: i,
                            point_idx: j,
                        });
                    }
                    if let Some(lt) = last_tick {
                        if p.tick < lt {
                            return Err(LayerInconsistency::AutomationPointsUnsorted {
                                lane_idx: i,
                            });
                        }
                    }
                    last_tick = Some(p.tick);
                }
            }
        }

        // 6. timeline_map: entries are sorted by (song, tick) within
        // each song. They are produced in this order by the builder.
        let mut last_song: u16 = 0;
        let mut last_tick: u32 = 0;
        let mut first = true;
        for (i, e) in self.timeline_map.entries.iter().enumerate() {
            if first || e.song != last_song {
                first = false;
                last_song = e.song;
                last_tick = e.tick;
                continue;
            }
            if e.tick < last_tick {
                return Err(LayerInconsistency::TimelineMapMismatch { entry_idx: i });
            }
            last_tick = e.tick;
        }

        // 7. Per-lane loops well-formed: a `ChannelLoop` must have a
        //    positive body (`start_tick < end_tick`), otherwise
        //    `fold_tick` can't make progress.
        for (i, l) in self.channel_loops.iter().enumerate() {
            if l.start_tick >= l.end_tick {
                return Err(LayerInconsistency::ChannelLoopEmpty { loop_idx: i });
            }
        }

        Ok(())
    }
}

/// Specific reason a [`Module::verify_layers_consistent`] check
/// failed. Returned in the `Err` variant so tests and editor code
/// can pin-point the broken invariant.
///
/// [`Module::verify_layers_consistent`]: crate::core::module::Module::verify_layers_consistent
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerInconsistency {
    /// `Module.clips` is not in canonical
    /// `(song, target_channel, position_tick)` ascending order.
    ClipsUnsorted,
    /// A `Clip.track` index points past the end of `Module.tracks`.
    ClipTrackOutOfRange {
        /// Position of the offending clip in `Module.clips`.
        clip_idx: usize,
        /// The out-of-range track index.
        track: u32,
    },
    /// Two clips on the same `(song, target_channel)` overlap on
    /// the timeline.
    ClipsOverlap {
        /// Index of the first overlapping clip.
        clip_a: usize,
        /// Index of the second overlapping clip.
        clip_b: usize,
    },
    /// A `TimelineEntry` references a `(pattern_idx, row_idx)`
    /// that doesn't resolve cleanly under the current Track/Clip
    /// layout.
    TimelineMapMismatch {
        /// Position of the offending entry in
        /// `Module.timeline_map.entries`.
        entry_idx: usize,
    },
    /// An [`AutomationPoint`]'s value type doesn't match its
    /// lane's [`AutomationTarget`] (e.g. `Bpm(u16)` on a
    /// `TrackVolume` lane).
    ///
    /// [`AutomationPoint`]: crate::core::daw::automation::AutomationPoint
    /// [`AutomationTarget`]: crate::core::daw::automation::AutomationTarget
    AutomationTypeMismatch {
        /// Position of the offending lane in `Module.automation`.
        lane_idx: usize,
        /// Position of the offending point inside the lane's
        /// `points` vector.
        point_idx: usize,
    },
    /// An automation lane's `points` aren't in tick-ascending
    /// order.
    AutomationPointsUnsorted {
        /// Position of the offending lane in `Module.automation`.
        lane_idx: usize,
    },
    /// A [`crate::core::daw::loop_region::ChannelLoop`] has an empty
    /// or inverted body (`start_tick >= end_tick`).
    ChannelLoopEmpty {
        /// Position of the offending loop in `Module.channel_loops`.
        loop_idx: usize,
    },
    /// A `Cell.event` carries a `NoteOn` whose instrument doesn't
    /// match its owning Track's instrument index.
    TrackInstrumentMismatch {
        /// Position of the Track in `Module.tracks`.
        track_idx: usize,
        /// Position of the offending row inside that Track.
        row: usize,
    },
}

#[cfg(test)]
mod channel_loop_tests {
    use super::*;
    use crate::core::cell::{Cell, CellEvent};
    use crate::core::daw::clip::Clip;
    use crate::core::daw::loop_region::ChannelLoop;
    use crate::core::daw::sorted_clips::SortedClips;
    use crate::core::daw::timeline::{TimelineEntry, TimelineMap};
    use crate::core::daw::track::Track;
    use crate::core::fixed::units::Volume;
    use crate::core::pitch::Pitch;

    /// `row_at` folds a looping lane's playhead into its loop body:
    /// rows past the loop replay the body, leaving non-looping lanes
    /// (the tracker default, empty `channel_loops`) untouched.
    #[test]
    fn row_at_folds_looping_lane() {
        // One channel, one track of 4 distinct-pitch rows on a speed-3
        // grid (tick = row * 3). A 2-row loop body [0, 6).
        let pitch = |r: u32| Pitch::try_from((60 + r) as u8).unwrap();
        let mut rows = alloc::vec::Vec::new();
        for r in 0..4u32 {
            rows.push(Cell {
                event: CellEvent::NoteOn {
                    pitch: pitch(r),
                    velocity: Volume::FULL,
                },
                ..Cell::default()
            });
        }
        let mut m = Module::default();
        m.tracks.push(Track::Notes {
            name: "t".into(),
            instrument: 0,
            rows,
            muted: false,
        });
        m.clips = SortedClips::from_unsorted(alloc::vec![Clip {
            track: 0,
            song: 0,
            target_channel: 0,
            position_tick: 0,
            speed_at_start: 3,
            track_row_offset: 0,
            source_start_row: 0,
            end_tick: 12,
        }]);
        let mut entries = alloc::vec::Vec::new();
        for r in 0..4u32 {
            entries.push(TimelineEntry {
                song: 0,
                order_idx: 0,
                pattern_idx: 0,
                row_idx: r,
                loop_iter: 0,
                tick: r * 3,
                speed_at_row: 3,
                bpm_at_row: 125,
            });
        }
        m.timeline_map = TimelineMap { entries };

        let played = |m: &Module, row: u32| -> Option<Pitch> {
            match m.row_at(0, 0, row as usize)[0].0.event {
                CellEvent::NoteOn { pitch, .. } => Some(pitch),
                _ => None,
            }
        };

        // No loop yet → each row plays its own pitch.
        for r in 0..4u32 {
            assert_eq!(played(&m, r), Some(pitch(r)), "unlooped row {r}");
        }

        // Arm a 2-row loop body [0, 6) on lane (song 0, channel 0).
        m.channel_loops.push(ChannelLoop {
            song: 0,
            channel: 0,
            start_tick: 0,
            end_tick: 6,
        });

        // First pass unchanged; rows 2/3 fold back to the body.
        assert_eq!(played(&m, 0), Some(pitch(0)));
        assert_eq!(played(&m, 1), Some(pitch(1)));
        assert_eq!(
            played(&m, 2),
            Some(pitch(0)),
            "row 2 (tick 6) folds to row 0"
        );
        assert_eq!(
            played(&m, 3),
            Some(pitch(1)),
            "row 3 (tick 9) folds to row 1"
        );
    }

    /// An empty/inverted loop body is rejected by the layer check.
    #[test]
    fn empty_channel_loop_is_invalid() {
        let mut m = Module::default();
        m.channel_loops.push(ChannelLoop {
            song: 0,
            channel: 0,
            start_tick: 8,
            end_tick: 8,
        });
        assert!(matches!(
            m.verify_layers_consistent(),
            Err(LayerInconsistency::ChannelLoopEmpty { loop_idx: 0 })
        ));
    }
}
