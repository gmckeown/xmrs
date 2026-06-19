#![forbid(unsafe_code)]

//! `DwModule::load` — entry point of the `import_dw` feature.
//!
//! ## Pipeline
//!
//! 1. [`super::detect::detect`] scans the payload for the canonical
//!    entry-point stubs and the init-time PC-relative opcodes, and
//!    returns a [`DwLayout`] holding the file offsets of every
//!    data table.
//! 2. `parse_samples` walks the sample-info/sample-data tables to
//!    recover per-sample metadata (loop, length, volume) and copy
//!    the PCM; `parse_tracks` / `parse_sub_songs` / the envelope and
//!    arpeggio parsers lift the rest.
//! 3. [`DwModule::to_module`] runs the [`super::runtime`] simulator
//!    over the parsed data and materialises the full DAW layer:
//!    instrument bank, per-`(channel, sample)` tracks, clips and a
//!    timeline — one xmrs song per sub-song.

use alloc::format;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use alloc::collections::BTreeMap;

use super::detect::{detect, DwLayout, DwVariant};
use super::header::{
    DwArpeggio, DwPositionList, DwSample, DwSubSong, DwTrack, DwVolumeEnvelope, DW_NUM_CHANNELS,
};
use super::tables::PeriodTable;
use crate::core::fixed::units::{ChannelVolume, EnvValue, Finetune, Panning, Volume};
use crate::prelude::*;
use crate::tracker::import::bin_reader::{BinReader, ImportError};

/// One placed note collected from the simulator trace during DAW
/// projection, as a row-quantised tuple:
/// `(row, effective_note, sample_index, volume, envelope_index,
/// arpeggio_index)`. The segmentation pass groups consecutive
/// `NoteRow`s by `(sample[, envelope, arpeggio])` into
/// [`ChannelSegment`]s.
type NoteRow = (usize, i16, usize, Option<u8>, Option<u16>, Option<u16>);

/// A simulated sub-song ready for clip emission:
/// `(song_index, row-grid speed in frames, the frame-stamped event
/// trace)`. The speed is carried so the post-dedup lane walk
/// quantises on the same grid the clips were built on.
type SongTrace = (u16, u32, Vec<(u32, super::runtime::TickEvent)>);

/// Parsed contents of a `.dw` file. Self-sufficient — once a
/// [`DwModule`] is built the original payload bytes can be dropped.
#[derive(Debug, Clone)]
pub struct DwModule {
    /// Replayer family the file targets.
    pub variant: DwVariant,
    /// Which period table the replayer uses.
    pub period_table: PeriodTable,
    /// `true` when `AUDxPER` is computed the new-player way — full-range
    /// note index into [`Self::period_table`] scaled by the per-sample
    /// fine-tune (`table[note] × mult >> 10`) — rather than the qball-era
    /// one-octave composite `PERIODS1[note % 12]`. Always `true` on the
    /// new player and on the old-stream players that carry the fine-tune
    /// idiom (leviathan, empire); `false` on genuine qball composite
    /// players. See [`super::detect::DwLayout::period_via_finetune`].
    pub period_via_finetune: bool,
    /// The full sample bank. Indices match the on-disk `SetSample`
    /// command argument minus the per-module `newSampleCmd`
    /// threshold (which the runtime layer applies).
    pub samples: Vec<DwSample>,
    /// Currently selected sub-song descriptor. `None` when no
    /// usable sub-song could be located.
    ///
    /// Multi-song `.dw` files are common: most game soundtracks
    /// ship every track in a single binary, indexed by a small
    /// integer the host passes via D0. The replayer multiplies
    /// that index by the row width and uses the result as offset
    /// into the sub-song table. The importer parses every entry
    /// it can validate into [`Self::sub_songs`], then picks the
    /// one most likely to be the "main" song (heuristic — see
    /// `pick_main_sub_song_index`). Callers that want a
    /// different sub-song can read [`Self::sub_songs`] and
    /// re-derive position lists via [`crate::tracker::import::dw::dw_module::position_lists_for`].
    pub sub_song: Option<DwSubSong>,

    /// Every sub-song the importer could decode from the
    /// `sub_song_list_offset` row table. Order matches the
    /// on-disk index (sub-song 0 = first row); the chosen
    /// "primary" sub-song is exposed separately as
    /// [`Self::sub_song`] / [`Self::selected_sub_song`]. Empty
    /// when no sub-song header was located.
    pub sub_songs: Vec<DwSubSong>,

    /// Index into [`Self::sub_songs`] of the sub-song mirrored
    /// into [`Self::sub_song`] / [`Self::position_lists`].
    /// `None` when no sub-song is available.
    pub selected_sub_song: Option<usize>,

    /// One position list per Paula channel for the **selected**
    /// sub-song, in channel order. Empty when the sub-song header
    /// could not be located or when the channel is intentionally
    /// silent for this song.
    pub position_lists: [DwPositionList; DW_NUM_CHANNELS],

    /// Position lists for **every** sub-song, indexed to match
    /// [`Self::sub_songs`]. [`Self::position_lists`] is a clone of
    /// `all_position_lists[selected_sub_song]`. The DAW projection
    /// walks this to render each sub-song as its own xmrs song.
    pub all_position_lists: Vec<[DwPositionList; DW_NUM_CHANNELS]>,

    /// Every distinct track byte stream referenced by at least one
    /// position list, parsed up to its `EndOfTrack` terminator.
    /// De-duplicated by file offset so a single track played by
    /// many channels / many times contributes a single entry.
    pub tracks: Vec<DwTrack>,

    /// Per-channel volume envelopes — indexed by `(track_byte -
    /// volume_envelope_threshold)` at runtime. Empty when the module
    /// either lacks the `0xA0..` dispatcher bracket or when the
    /// table-base probe couldn't locate the envelope-pointer
    /// table. See [`DwVolumeEnvelope`] for the per-entry shape.
    pub volume_envelopes: Vec<DwVolumeEnvelope>,

    /// Per-channel pitch arpeggios — indexed by `(track_byte -
    /// pitch_arpeggio_threshold)` at runtime (the `0x90..` dispatcher
    /// bracket). Empty when the module has no `0x90..` bracket or
    /// the table-base probe failed. See [`DwArpeggio`].
    pub arpeggios: Vec<DwArpeggio>,

    /// Per-module command dispatcher thresholds — kept on the
    /// `DwModule` so the simulator can resolve raw track bytes
    /// (e.g. `0xA5`) back to envelope-table indices without
    /// re-reading the binary.
    pub dispatcher: super::detect::DwDispatcher,

    /// Detect-time feature flags lifted directly from
    /// [`super::detect::DwLayout::features`]. The simulator gates
    /// module-dependent semantics (e.g. `Effect8` → channel
    /// transpose vs. global volume fade) on these.
    pub features: super::detect::DwFeatures,

    /// When `true`, the `SetVolumeEnvelope` dispatcher bracket arms a
    /// **pitch arpeggio** (indexing [`Self::arpeggios`]) instead of
    /// a volume envelope — see
    /// [`super::detect::DwLayout::volume_bracket_is_pitch`]. tetris.dw
    /// is the canonical case.
    pub volume_bracket_is_pitch: bool,

    /// Old-player per-channel static volume (0..=64), indexed by
    /// Paula channel. The qball-era replayer loads this into
    /// `AUDxVOL` on every note trigger and never scales it by a
    /// sample or envelope value — it *is* the channel's loudness.
    /// Read from [`super::detect::DwLayout::channel_volume_offset`];
    /// defaults to full scale on new-player modules (whose loudness
    /// comes from the volume envelope) and when the table couldn't be
    /// located.
    pub channel_volumes: [u16; DW_NUM_CHANNELS],

    /// Static global master volume (`1..=64`), or `None` when the
    /// module has no master-volume scaling. The empire-era replayer
    /// multiplies every Paula volume by this before a `>> 6`
    /// (`AUDxVOL = volByte × master >> 6`), at both note-trigger and
    /// per-tick envelope animation; the importer reproduces it by
    /// scaling all emitted Paula volumes by `master / 64`. Read from
    /// [`super::detect::DwLayout::master_volume_offset`]. `None` leaves
    /// volumes unscaled (the common case — new players use the
    /// identity `master = 64`, qball writes channel volumes raw).
    pub master_volume: Option<u16>,

    /// When `true`, the **volume** envelope is baked per-tick on a
    /// synthetic per-`(sample, envelope)` instrument (frame-resolution
    /// loudness), as opposed to the row-resolution cell `Volume`
    /// animation. Set for the jump-table (`command_map`) family —
    /// bubble bobble & kin. (Historically this flag also drove the
    /// *pitch* arpeggio via a looping pitch envelope; that role has
    /// moved to the per-frame `use_arpeggio_pitch_lane` Points lane,
    /// so this now governs only the volume-envelope split.)
    pub use_pitch_arpeggio: bool,

    /// When `true`, per-frame **pitch arpeggios** are baked as a
    /// `TrackPitch` `LaneKind::Points` curve of
    /// [`crate::core::daw::automation::AutomationValue::Pitch`] points
    /// — one per arpeggio-offset change, evaluated every player tick.
    /// This is the faithful projection of the replayer's per-channel,
    /// per-frame arpeggio pointer (Ghidra `play_tick` held-note
    /// branch: offset added to the period-table *index* = semitone
    /// space, pointer continuous across notes), as opposed to the
    /// classic three-step `TrackEffect::Arpeggio` (mod-3, trigger-row
    /// only) or the old per-note pitch-envelope path. Set for **every**
    /// DW module — the lane is the single arpeggio path for DW, so the
    /// classic effect is suppressed and the arpeggio segment-split /
    /// pitch-envelope are skipped. No-op for modules that never arm an
    /// arpeggio (no points emitted). (`use_pitch_arpeggio` now governs
    /// only the *volume*-envelope synthetic-instrument path, not the
    /// arpeggio.)
    pub use_arpeggio_pitch_lane: bool,
}

impl DwModule {
    /// Parse a `.dw` payload. Returns `InvalidMagic("dw_detect")`
    /// when the payload does not look like a David Whittaker
    /// module.
    pub fn load(source: &[u8]) -> Result<Self, ImportError> {
        let layout = detect(source).ok_or(ImportError::InvalidMagic("dw_detect"))?;
        let samples = Self::parse_samples(source, &layout)?;
        let sub_songs = Self::parse_sub_songs(source, &layout);
        let selected = Self::pick_main_sub_song_index(
            source,
            &sub_songs,
            layout.uses_32bit_pointers,
            layout.start_offset,
            // The jump-table (command_map) family — bubble bobble and
            // kin — multiplexes every game tune into one binary and the
            // replayer plays sub-song 0 on the default `D0 = 0` entry.
            // Their sub-song 0 is the real in-game music, so the
            // "longest position list wins" heuristic (which suits the
            // canonical player, where sub-song 0 is sometimes a short
            // jingle) picks the wrong tune. Prefer index 0 for this
            // cluster to match the replayer's own default.
            layout.command_map.is_some(),
        );
        // Decode the position lists of every sub-song up front, so
        // the DAW projection can render each tune as its own xmrs
        // song. Tracks are parsed from the union of all of them.
        let mut all_position_lists: Vec<[DwPositionList; DW_NUM_CHANNELS]> = sub_songs
            .iter()
            .map(|s| position_lists_for(source, s, layout.uses_32bit_pointers, layout.start_offset))
            .collect();
        // Resolve `SeqPtr` (cmd 0x89) loops: a channel whose intro track
        // redirects the sequencer to a sub-sequence loops THAT, not back
        // to the intro. Done before `parse_tracks` so the sub-sequence's
        // tracks are collected. (See `follow_seq_ptr_loops`.)
        for lists in all_position_lists.iter_mut() {
            follow_seq_ptr_loops(
                source,
                lists,
                &layout.dispatcher,
                &layout.features,
                layout.command_map.as_ref(),
                layout.uses_32bit_pointers,
                layout.start_offset,
            );
        }
        let position_lists = selected
            .map(|i| all_position_lists[i].clone())
            .unwrap_or_default();
        let sub_song = selected.map(|i| sub_songs[i].clone());
        let tracks = Self::parse_tracks(
            source,
            &all_position_lists,
            &layout.dispatcher,
            &layout.features,
            layout.command_map.as_ref(),
        );
        let volume_envelopes = Self::parse_volume_envelopes(source, &layout);
        let arpeggios = Self::parse_arpeggios(source, &layout);
        let channel_volumes = Self::parse_channel_volumes(source, &layout);
        let master_volume = layout.master_volume_offset.and_then(|off| {
            source
                .get(off..off + 2)
                .map(|b| u16::from_be_bytes([b[0], b[1]]))
        });

        // Reject payloads that are DW-shaped (the LEA A3 anchor
        // matched) but carry no playable song: no samples, no
        // sub-song header, and no tracks. These are sound-effect
        // banks and placeholder/"fake" files (`*-sfx.dw`,
        // `* fake.dw`) — they trigger individual sounds from host
        // code rather than running a song timeline, so there's
        // nothing for the importer to build a `Module` from.
        // Returning an error here keeps `Module::load` autodetect
        // from producing a hollow Module.
        if samples.is_empty() && sub_songs.is_empty() && tracks.is_empty() {
            return Err(ImportError::InvalidMagic("dw_no_song"));
        }

        Ok(Self {
            variant: layout.variant,
            period_table: layout.period_table,
            period_via_finetune: layout.period_via_finetune,
            samples,
            sub_song,
            sub_songs,
            selected_sub_song: selected,
            position_lists,
            all_position_lists,
            tracks,
            volume_envelopes,
            arpeggios,
            dispatcher: layout.dispatcher.clone(),
            features: layout.features,
            volume_bracket_is_pitch: layout.volume_bracket_is_pitch,
            channel_volumes,
            master_volume,
            // Governs the volume-envelope synth path; on the C4
            // oscillator family (grimblood/cosmic pirate) it ALSO keeps
            // the old per-note pitch-envelope arpeggio (see lane gate).
            use_pitch_arpeggio: layout.command_map.is_some(),
            // Per-frame `TrackPitch` Points lane = the arpeggio path for
            // DW, EXCEPT the C4 "oscillator" replayer family (Ghidra
            // `grimblood FUN_00000d6e`): its per-frame pitch is a
            // period-space 2-word oscillator, NOT a period-table-index
            // arpeggio, so the semitone Points lane mis-renders it
            // (grimblood ch0 octave spikes, MAE 6.8→21.9). That family
            // is the only `command_map` + `P2` shape in the corpus
            // (grimblood; cosmic pirate is `P3` but arms no arpeggio, so
            // it is unaffected either way) — keep it on its prior
            // pitch-envelope path. Every other DW module (canonical
            // jump-table, tetris, bubble bobble) uses the faithful lane.
            // No-op when no arpeggio is ever armed.
            use_arpeggio_pitch_lane: !(layout.command_map.is_some()
                && matches!(layout.period_table, PeriodTable::P2)),
        })
    }

    /// Scale a Paula volume byte (`0..=64`) by the module's global
    /// master volume, reproducing the empire-era replayer's
    /// `AUDxVOL = volByte × master >> 6`. A no-op when no master is
    /// set or it is the identity `64`; the result is clamped to
    /// Paula's `0..=64` range.
    #[inline]
    fn scale_master_volume(&self, v: u8) -> u8 {
        match self.master_volume {
            Some(master) if master != 64 => (((v as u32) * (master as u32)) >> 6).min(64) as u8,
            _ => v,
        }
    }

    /// Read the old-player per-channel volume table: `DW_NUM_CHANNELS`
    /// big-endian `u16`s at
    /// [`super::detect::DwLayout::channel_volume_offset`], each clamped
    /// to Paula's 0..=64 range. Returns full scale on every channel
    /// when the offset is absent (new-player modules) or unreadable.
    fn parse_channel_volumes(
        source: &[u8],
        layout: &super::detect::DwLayout,
    ) -> [u16; DW_NUM_CHANNELS] {
        let mut vols = [64u16; DW_NUM_CHANNELS];
        if let Some(base) = layout.channel_volume_offset {
            for (ch, slot) in vols.iter_mut().enumerate() {
                let at = base + ch * 2;
                if let Some(bytes) = source.get(at..at + 2) {
                    *slot = u16::from_be_bytes([bytes[0], bytes[1]]).min(64);
                }
            }
        }
        vols
    }

    /// Pick the "main" sub-song from a list. Heuristic:
    /// largest cumulative position-list length across the four
    /// channels — degenerate sub-songs (the leading test/jingle
    /// entries seen on beast1.* with 1 track per channel) lose
    /// to the real music. Ties break by lowest index. Returns
    /// `None` when the list is empty.
    fn pick_main_sub_song_index(
        source: &[u8],
        sub_songs: &[DwSubSong],
        uses_32bit: bool,
        start_offset: isize,
        prefer_first: bool,
    ) -> Option<usize> {
        if sub_songs.is_empty() {
            return None;
        }
        // Investigation/override hook: `DW_SUBSONG=<n>` forces a
        // specific sub-song so a `.dw` with several tunes (title /
        // in-game / variations) can be auditioned one at a time.
        // Out-of-range values fall through to the heuristic.
        #[cfg(feature = "std")]
        if let Ok(v) = std::env::var("DW_SUBSONG") {
            if let Ok(i) = v.trim().parse::<usize>() {
                if i < sub_songs.len() {
                    return Some(i);
                }
            }
        }
        // Jump-table family: the replayer's default entry is sub-song 0
        // (verified against the Paula oracle on bubble bobble — song 0
        // matches the golden at 96 % gate-agree, the heuristic's pick
        // at 50 %). Don't run the "longest list" heuristic for them.
        if prefer_first {
            return Some(0);
        }
        let score = |s: &DwSubSong| -> usize {
            position_lists_for(source, s, uses_32bit, start_offset)
                .iter()
                .map(|l| l.len())
                .sum()
        };
        // The replayer's *true* default is sub-song 0: `init` is entered
        // with the game's `d0` selecting the tune, and the canonical
        // "play the main music" call passes `d0 = 0` (Ghidra: kickstart
        // `init` reads `d0` straight into the song index; `FUN_ac`
        // doesn't choose one). So prefer sub-song 0 unless it is a
        // *degenerate* leading stub — the beast1.* test/jingle case this
        // heuristic was built for carries ~1 track per channel. Only
        // then fall back to the longest-list pick.
        //
        // The old unconditional "longest list" rule mis-picked exactly
        // two corpus modules (kickstart ii → sub-song 1, xenon →
        // sub-song 2) whose sub-song 0 is the real, longer-playing tune;
        // forcing sub-song 0 drops both from period-MAE ~150-240 to
        // ~9-11 (Paula-oracle verified), with no change to the other 111
        // modules (which already resolve to sub-song 0).
        const DEGENERATE_MAX: usize = 8; // ≈2 entries/channel
        if score(&sub_songs[0]) > DEGENERATE_MAX {
            return Some(0);
        }
        let mut best = 0usize;
        let mut best_score = 0usize;
        for (i, s) in sub_songs.iter().enumerate() {
            let sc = score(s);
            if sc > best_score {
                best_score = sc;
                best = i;
            }
        }
        Some(best)
    }

    /// Convert into the editor-friendly [`Module`] representation.
    ///
    /// Populates the full DAW layer: the instrument bank (one
    /// [`InstrDefault`] per parsed sample), one [`Track::Notes`]
    /// per `(Paula channel, current sample)` run discovered by
    /// the simulator, matching [`Clip`]s placed on each channel
    /// lane, and a [`TimelineMap`] covering the song length
    /// yielded by [`Self::simulate`]. After segment creation
    /// the standard xmrs [`crate::tracker::import::build::dedupe_tracks_by_content`]
    /// pass fuses bit-identical segments so a single phrase
    /// shared by several channels lives in one `Track`.
    ///
    /// Quantisation: one row = one channel speed-tick (`speed`
    /// frames at 50 Hz PAL). Notes whose Whittaker byte exceeds
    /// the 120-position xmrs `Pitch` range are clamped to the
    /// top — DW's period tables cover at most 6 octaves while
    /// xmrs has 10, so the clamp only fires on out-of-band
    /// values.
    pub fn to_module(&self) -> Module {
        use crate::core::cell::{Cell, CellEvent};
        use crate::core::daw::clip::Clip;
        use crate::core::daw::sorted_clips::SortedClips;
        use crate::core::daw::timeline::{TimelineEntry, TimelineMap};
        use crate::core::daw::track::Track;
        use crate::core::fixed::units::Volume;
        use crate::core::pitch::Pitch;
        use alloc::string::String;
        use core::convert::TryFrom;

        let mut module = Module::default();
        module.name = String::from("David Whittaker (.dw)");
        module.origin = Some(crate::tracker::format::ModuleFormat::Dw);
        // Whittaker ticks at 50 Hz PAL. Mapping that onto the
        // (tempo, bpm) tracker idiom: one tracker row = `speed`
        // Paula frames. Without these overrides the runtime
        // assumes the global default (tempo 6, BPM 125) and plays
        // every row twice as slow as the simulator intended.
        // BPM stays at the canonical 125 — same default `Module`
        // already uses, restated here for clarity.
        // Set below once `speed` is known.

        // Whittaker drives Paula directly, so the runtime
        // period-to-frequency math must use the Amiga model.
        // The pitch helper below also depends on this choice.
        module.frequency_type = crate::tracker::period::FrequencyType::AmigaFrequencies;

        // The Whittaker pitch slide runs in `DoFrameStuff` on every
        // frame of the held note (1 frame = 1 player tick here), so
        // its `TrackPitch` Slide lane must advance on tick 0 too —
        // unlike the tracker formats that reserve tick 0 for the row
        // trigger. See `attach_slide_lanes`.
        module.quirks.pitch_slide_ticks_at_row_zero = true;

        // Whittaker vibrato (cmd 0x81, and cmd 0x86 on the jump-table
        // family) likewise runs in `DoFrameStuff` every frame until an
        // explicit stop — so its `TrackPitch` LFO lane must keep
        // advancing each tick once armed, not only on rows that
        // re-issue the vibrato. Without this the wobble collapses to
        // near-flat (verified vs the Paula oracle: ch0 stays at ~356
        // instead of oscillating 347..359). See `advance_lfos_from_lanes`.
        module.quirks.pitch_vibrato_ticks_continuously = true;

        // Paula's hardware panning: channels 0+3 land on the
        // left output, channels 1+2 on the right (the classic
        // "LRRL" layout). Whittaker mixes for this image — every
        // melodic lead and bass placement on the original
        // assumes channel 0 ≠ channel 1 acoustically. Without
        // this `channel_defaults` block all four voices play
        // dead-centre and the stereo separation Whittaker
        // arranged for collapses.
        use crate::core::module::ChannelDefault;
        module.channel_defaults = alloc::vec![
            ChannelDefault {
                panning: Some(Panning::LEFT),
                ..Default::default()
            },
            ChannelDefault {
                panning: Some(Panning::RIGHT),
                ..Default::default()
            },
            ChannelDefault {
                panning: Some(Panning::RIGHT),
                ..Default::default()
            },
            ChannelDefault {
                panning: Some(Panning::LEFT),
                ..Default::default()
            },
        ];
        let helper = crate::tracker::period::PeriodHelper::new(module.frequency_type, false);

        // ---- instruments ----
        module.instrument.reserve(self.samples.len());
        for s in &self.samples {
            module.instrument.push(sample_to_instrument(s));
        }

        // ---- timing / quantisation (per-sub-song grid) ----
        //
        // One tracker row = `dw_speed` Paula frames (50 Hz PAL) — the
        // sub-song's OWN Whittaker speed byte, NOT a fixed grid. Every
        // note lasts `(LongWait − 0xDF) × dw_speed` frames, so a row
        // grid of exactly `dw_speed` frames lands every event on a row
        // boundary with zero quantisation jitter, whatever the speed.
        // A fixed 3-frame grid (the previous choice) only stayed exact
        // when `dw_speed` was a multiple of 3; on sub-songs at speed
        // 4/5 (e.g. bubble bobble songs 2/3) `floor((frame−1)/3)` spread
        // notes over an irregular 1/2-row spacing — the tempo mean was
        // right but each note jittered ±1 row (±60 ms), audible as a
        // "rushed"/unsteady rhythm.
        //
        // The grid is therefore chosen per sub-song inside the render
        // loop below (`speed`/`speed_u32` = that song's `dw_speed`).
        // This is the exact tracker idiom: tempo = ticks/row = the
        // Whittaker speed, BPM = tick rate (which keeps carrying the
        // delay-counter slowdown, see `bpm_for`). The runtime honours a
        // per-song tempo via `TimelineEntry::speed_at_row` /
        // `bpm_at_row` (sequencer reads them at each row start), so no
        // core change is needed. `default_tempo` is set to the primary
        // song's speed once `order` is known (below).

        // BPM models the global delay counter. The replayer skips
        // ~`delay/256` of every frame (Ghidra `play_tick` head — an
        // 8-bit accumulator that drops a frame on overflow), a UNIFORM
        // slowdown to `(256 − delay)/256 × 50 Hz`. We render notes on
        // the clean row grid (the simulator no longer skips frames, so
        // spacing stays integer rows) and fold that slowdown into BPM
        // instead: PAL 125 BPM = 50 Hz, so `125 × (256 − delay)/256`.
        // Doing it as frame-skips + row quantisation jittered the row
        // spacing ±1 (each row arriving "hesitantly"); a flat BPM is
        // both steadier and the perceptually-faithful result. Computed
        // per sub-song from its `delay_speed` (×16 when
        // `enable_delay_multiply`); clamped so a large delay can't zero
        // the tempo.
        let bpm_for = |ss_idx: usize| -> usize {
            let eff_delay = if self.features.enable_delay_counter {
                let d = self.sub_songs[ss_idx].delay_speed as u32;
                if self.features.enable_delay_multiply {
                    d.saturating_mul(16)
                } else {
                    d
                }
            } else {
                0
            }
            .min(224); // keep ≥ ~14% of full rate
            ((125 * (256 - eff_delay)) / 256).max(32) as usize
        };

        let instr_count = module.instrument.len();
        let clamp_sample = |s: u16| -> usize {
            let s = s as usize;
            if instr_count == 0 {
                0
            } else {
                s.min(instr_count - 1)
            }
        };

        // Period-exact note → xmrs `Pitch`, for both replayer
        // families (see the long rationale below — hoisted above
        // the per-song loop since it depends only on the module,
        // not the sub-song).
        let note_to_pitch = |note_byte: i16, sample_freq: u16, sample_transpose: i8| -> Pitch {
            // qball-era composite players map the note byte's low part
            // into a single octave (`PERIODS1[note % 12]`, no fine-tune).
            // The new player *and* the old-stream fine-tune players
            // (leviathan, empire) index a full-range word table scaled by
            // the per-sample fine-tune — `period_via_finetune` selects it.
            let period: u32 = if matches!(self.variant, DwVariant::Old) && !self.period_via_finetune
            {
                let idx = note_byte.clamp(0, 11) as usize;
                super::tables::PERIODS1[idx] as u32
            } else {
                let max_idx = match self.period_table {
                    PeriodTable::P1 => 11,
                    PeriodTable::P2 => super::tables::PERIODS2.len() - 1,
                    PeriodTable::P3 => super::tables::PERIODS3.len() - 1,
                };
                let idx = (note_byte + sample_transpose as i16).clamp(0, max_idx as i16) as usize;
                let base = self.period_table.period(idx).unwrap_or(0) as u32;
                let finetune = 0x0036_9E99u32 / (sample_freq.max(1) as u32);
                (base.saturating_mul(finetune)) >> 10
            };
            if period == 0 {
                return Pitch::C5;
            }
            let p = crate::core::fixed::units::Period::from_raw(period.clamp(1, 0xFFFF) as u16);
            let pitch_q8_8 = helper.period_to_pitch(p).as_q8_8_i32();
            let semitone = ((pitch_q8_8 + 0x80) >> 8).clamp(0, 119) as u8;
            Pitch::try_from(semitone).unwrap_or(Pitch::C5)
        };

        // ---- render every sub-song as its own xmrs song ----
        //
        // Song 0 is the auto-selected "main" tune (so the player's
        // default `-s 0` plays it); the remaining sub-songs follow
        // in on-disk index order. The Track bank is shared across
        // songs; each Clip / TimelineEntry carries its song index.
        // Cap the number of rendered songs. Some modules are game
        // music banks carrying a dozen-plus sub-songs — one main tune
        // plus short cues, jingles and entry-point variations
        // (grimblood ships 18, loopz / gold-of-the-aztecs 21). The cap
        // is purely a runaway guard against a pathological file: the
        // measured cost of exposing every sub-song is modest (the
        // heaviest 21-song modules render in ~20 ms / ~5400 clips —
        // looping-song simulation is already frame-bounded), so it is
        // set well above the known corpus maximum. The order is
        // [selected, 0, 1, 2, …], so the first entries (notably song 0,
        // the auto-selected main tune used by the fidelity oracles) are
        // identical regardless of this value — raising it only appends
        // the remaining sub-songs. Modules with ≤ this many sub-songs
        // are unaffected.
        const MAX_SONGS: usize = 32;
        let mut order: alloc::vec::Vec<usize> = alloc::vec::Vec::new();
        if let Some(sel) = self.selected_sub_song {
            order.push(sel);
        }
        for i in 0..self.all_position_lists.len() {
            if order.len() >= MAX_SONGS {
                break;
            }
            if Some(i) != self.selected_sub_song {
                order.push(i);
            }
        }

        // Module default tempo / BPM = the primary (auto-selected)
        // song's. Tempo is that song's Whittaker speed (ticks per row =
        // frames per row); BPM carries the delay-counter slowdown. The
        // per-song values below override these at each row.
        module.default_tempo = order
            .first()
            .map(|&i| self.sub_songs[i].speed.max(1) as usize)
            .unwrap_or(6);
        module.default_bpm = order.first().map(|&i| bpm_for(i)).unwrap_or(125);

        let mut clips_vec: Vec<Clip> = Vec::new();
        let mut timeline_entries: Vec<TimelineEntry> = Vec::new();
        // (song index, that song's grid `speed_u32`, trace) — the
        // per-song speed is carried so the post-dedup lane walk below
        // quantises with the same grid the clips were built on.
        let mut song_traces: Vec<SongTrace> = Vec::new();
        // Synthetic per-`(sample, arpeggio)` instruments (pitch-envelope
        // path), shared across every song so a `(sample, arp)` pair
        // reused by several tunes contributes a single instrument.
        let mut arp_instr_cache: BTreeMap<(usize, Option<u16>, Option<u16>), usize> =
            BTreeMap::new();

        for (song_u, &ss_idx) in order.iter().enumerate() {
            let song = song_u as u16;
            let lists = &self.all_position_lists[ss_idx];
            let dw_speed = self.sub_songs[ss_idx].speed.max(1);
            // Per-sub-song row grid (Option A): one row = `dw_speed`
            // frames, so every event lands exactly on a row boundary.
            let speed: u8 = dw_speed;
            let speed_u32 = dw_speed as u32;
            // This song's delay-counter tempo (see `bpm_for`). The
            // simulator runs at the clean 50 Hz grid; the delay's
            // uniform slowdown is carried entirely by this BPM.
            let song_bpm = bpm_for(ss_idx) as u16;
            let mut sim = super::runtime::Simulator::new_for_subsong(self, lists, dw_speed);
            // The primary song (rendered first) renders exactly ONE
            // seamless loop period — each Whittaker channel loops its
            // own position list, and they realign at the LCM of the
            // per-channel pass lengths (10725 frames for xenon2, whose
            // four voices share one period). `Module::song_loop_to` is
            // then set so the player repeats the song like the
            // original instead of stopping after one pass. Falls back
            // to the full 20-minute cap with no loop marker when no
            // clean period is found inside the cap. Audition songs
            // (> 0) stay bounded short (~2 min at speed 3 = 6000
            // frames) so a module with many looping variations doesn't
            // blow up the Module, and don't carry their own loop mark.
            // Render to the song's **seamless loop period** — the LCM
            // of the per-channel position-list pass lengths, where every
            // voice is simultaneously back at list entry 0. Truncating
            // the trace to that exact frame count is what keeps the
            // rendered loop length a whole multiple of the real period;
            // the previous `run_capped(6000)` for songs > 0 cut at an
            // arbitrary frame and the `last_frame/speed` row-rounding
            // then shaved a few frames off, so those songs looped a few
            // rows short and drifted against the original (e.g. song 1
            // looped at 2295 frames instead of the true 2304, audible as
            // a seam at the order wrap). `run_with_loop` finds the true
            // period for songs 1-3 (2304 / 1344 / 1920); song 0 has no
            // clean LCM inside the cap (independent channel lengths) and
            // returns `None`, falling back to the full cap.
            //
            // `song_loop_to` is module-global, so only the auto-selected
            // primary song (song_u 0) sets it — the secondary audition
            // songs still render one faithful loop but don't claim the
            // single module-level wrap marker.
            let cap = if song_u == 0 {
                super::runtime::MAX_SIMULATION_FRAMES
            } else {
                // Bound secondary songs so a module with many looping
                // variations can't blow up: one period, or ~2 min.
                6000
            };
            let (mut trace, period) = sim.run_with_loop(cap);
            // Drifting-voices fallback (Whittaker title tunes). When
            // `run_with_loop` hits the cap (`period == cap`), the four
            // voices share NO common loop within ~20 min: each loops its
            // own position list at its own length and they drift forever
            // (xenon2: 9888/9888/9912/10014 frames — confirmed against
            // the real 68k replayer: cmd 0x80 wraps each channel to its
            // OWN list start, no global restart). A single global wrap
            // can't be seamless for all of them. Wrap at the LONGEST
            // voice's pass length so NO voice's material is ever cut — a
            // shorter voice is merely mid-loop at the wrap (a small
            // stutter). This replaces the old blunt 20-min cap wrap,
            // which reset all four voices mid-phrase at once. (The
            // per-lane loop increment — `Module::channel_loops` — will
            // later remove even this residual stutter by looping each
            // voice independently; see `daw/LOOP_REGION_RFC.md`.)
            let drift_len: Option<u32> = if song_u == 0 && period == Some(cap) {
                sim.first_wrap_frame
                    .iter()
                    .filter_map(|w| w.map(|f| f.saturating_sub(1)))
                    .max()
                    .filter(|&m| m > 0 && m < cap)
            } else {
                None
            };
            if let Some(dl) = drift_len {
                // Per-lane truncation: keep each voice's events only
                // within ITS OWN pass `[0, first_wrap[ch])`. The player's
                // per-lane fold replays each region, so tiling a voice
                // past its own wrap is dead weight the fold never reaches.
                // Voices that never wrapped (finished) keep up to the
                // global span `dl`.
                let pass_end = |ch: usize| -> u32 {
                    sim.first_wrap_frame
                        .get(ch)
                        .and_then(|w| *w)
                        .map(|f| f.saturating_sub(1))
                        .unwrap_or(dl)
                };
                trace.retain(|(f, ev)| *f <= pass_end(ev.channel() as usize));
                // Per-lane loops: each voice loops its own pass
                // independently (Ghidra-confirmed: cmd 0x80 wraps each
                // channel to its OWN list start). The player free-runs
                // the timeline and folds each lane by its region, so the
                // voices drift forever with NO global snap — exactly the
                // hardware. The longest voice's region end equals the
                // timeline span, so it wraps seamlessly; shorter voices
                // fold continuously across the replay. Voices that never
                // wrapped (finished) get no region and stay silent after
                // their content. See `daw/LOOP_REGION_RFC.md`.
                for (ch, w) in sim.first_wrap_frame.iter().enumerate() {
                    if let Some(end) = w.map(|f| f.saturating_sub(1)).filter(|&e| e > 0) {
                        // Loop body starts where the channel first reached
                        // its `loop_to` target — tick 0 for whole-pass
                        // loops, but past the intro for `SeqPtr`-redirected
                        // channels (`follow_seq_ptr_loops`), so the intro
                        // plays once and only the body loops (bad company
                        // ch0/1/3). Clamp below `end` so the body is never
                        // empty/inverted.
                        let start = sim.loop_start_frame[ch]
                            .map(|f| f.saturating_sub(1))
                            .unwrap_or(0)
                            .min(end);
                        module
                            .channel_loops
                            .push(crate::core::daw::loop_region::ChannelLoop {
                                song,
                                channel: ch as u8,
                                start_tick: start,
                                end_tick: end,
                            });
                    }
                }
            }
            // Song-start loop point. Two cases set it:
            //  - a clean per-channel loop period was found (`period`), or
            //  - the song HALTED via StopSong (cmd 0x84 = `DMACON = 0x000F`).
            //    On the hardware that kills the replayer (plays once); a
            //    tracker has no silent-stop, so the faithful idiom is the
            //    order-end loop-to-start (like a `Bxx`/restart) — the song
            //    replays from the top. The Paula oracle, which never
            //    re-inits, just shows silence after the stop, so its golden
            //    must be captured truncated at the stop (the `f0`
            //    playing-flag option), exactly as `bubble_bobble.csv` is.
            let halted_via_stop = trace
                .iter()
                .any(|(_, ev)| matches!(ev, super::runtime::TickEvent::SongEnd { .. }));
            if song_u == 0 && (period.is_some() || halted_via_stop) {
                // List loops wholly from entry 0, so the realignment
                // point is the song start (tick 0).
                module.song_loop_to = Some(0);
            }
            if trace.is_empty() {
                continue;
            }
            // Song length in rows. When a seamless loop period was found,
            // derive the row count from the **period** itself, not from
            // the last event's frame: the last NoteOn often lands a few
            // frames before the period boundary (a held note fills the
            // gap), so `last_frame` undershoots the true loop length and
            // the row-rounding then shaved the loop short (song 1 looped
            // at 765 rows / 2295 frames instead of 768 / 2304, drifting
            // against the original at the wrap). The period is a whole
            // multiple of the channel pass lengths, so `period / speed`
            // is exact. Without a period (song 0), fall back to the last
            // rendered frame.
            let last_frame = trace.iter().map(|(f, _)| *f).max().unwrap_or(0);
            // Longest-voice length in the drift case, else the clean
            // loop period, else the last rendered frame.
            let length_frames = drift_len.or(period).unwrap_or(last_frame);
            let total_rows = (length_frames.saturating_sub(1) / speed_u32 + 1) as usize;
            // Per-lane row count for segment building. In the drift case
            // a looping voice's clips end at ITS OWN pass, not the global
            // span — otherwise `build_segments` would extend the last
            // segment's held note to `total_rows`, leaving a trailing
            // clip in `[end_ch, span)` that the fold never reaches. The
            // longest voice (and finished/non-drift voices) use the full
            // `total_rows`.
            let lane_rows: [usize; 4] = {
                let mut lr = [total_rows; 4];
                if drift_len.is_some() {
                    for (slot, &w) in lr.iter_mut().zip(sim.first_wrap_frame.iter()) {
                        if let Some(end) = w.map(|f| f.saturating_sub(1)).filter(|&e| e > 0) {
                            *slot = (end.saturating_sub(1) / speed_u32 + 1) as usize;
                        }
                    }
                }
                lr
            };

            // ---- collect per-channel notes with their sample
            //      context. The trace already carries
            //      `NoteOn.sample_index` (the channel's current
            //      sample at the moment of the note), so we don't
            //      need a separate SetSample pass.
            // Per-note tuple: (row, effective_note, sample_index,
            // paula_volume_peak, envelope_index, arpeggio_index).
            // `volume = None` means "no envelope armed" → keep
            // `Volume::FULL`. The envelope index, when present, lets
            // the segment build walk the full step sequence row-by-row.
            // `arpeggio_index` is only populated on the pitch-envelope
            // path (`use_pitch_arpeggio`); it becomes a segment key so
            // each `(sample, arpeggio)` run gets its own synthetic
            // pitch-envelope instrument. `None` on every other module
            // leaves segmentation (and the classic `TrackEffect::Arpeggio`
            // path below) byte-for-byte unchanged.
            let mut per_ch_notes: [Vec<NoteRow>; 4] = Default::default();
            // Pitch arpeggio armed per (channel, row), looked up at
            // cell-emission time for the classic-effect path. The
            // arpeggio is a per-note effect, not a segmentation key,
            // there.
            let mut arp_by_row: [BTreeMap<usize, u16>; 4] = Default::default();
            for (frame, ev) in &trace {
                if let super::runtime::TickEvent::NoteOn {
                    channel,
                    effective_note,
                    sample_index,
                    volume,
                    envelope_index,
                    arpeggio_index,
                    ..
                } = ev
                {
                    let row = ((frame.saturating_sub(1)) / speed_u32) as usize;
                    if row >= total_rows {
                        continue;
                    }
                    let ch = *channel as usize;
                    if ch >= 4 {
                        continue;
                    }
                    let sample = sample_index.map(clamp_sample).unwrap_or(0);
                    // The arpeggio is never a segment key now: it rides
                    // the per-frame `TrackPitch` Points lane
                    // (`use_arpeggio_pitch_lane`, always on for DW), not a
                    // per-(sample,arpeggio) synthetic pitch envelope. Kept
                    // as an explicit `None` (the old pitch-envelope split
                    // only fired when the lane was off, which no DW module
                    // does anymore).
                    let arp_for_seg = if self.use_pitch_arpeggio && !self.use_arpeggio_pitch_lane {
                        *arpeggio_index
                    } else {
                        None
                    };
                    per_ch_notes[ch].push((
                        row,
                        *effective_note,
                        sample,
                        *volume,
                        *envelope_index,
                        arp_for_seg,
                    ));
                    if let Some(arp) = arpeggio_index {
                        // Last write wins per row, matching the note
                        // dedup below.
                        arp_by_row[ch].insert(row, *arp);
                    }
                }
            }

            // Quantising frames to rows can land two notes on the same
            // row (a channel that re-triggers within one `speed`-frame
            // window). The DAW model is one cell per row, so collapse
            // each run of same-row notes to its **last** entry — the
            // tracker "later write wins" convention, which also matches
            // the runtime (the last sample armed on the channel is what
            // keeps sounding through the row). Without this,
            // `build_segments` can open a sample-change segment at a
            // row equal to the previous segment's start, producing two
            // clips that share a `position_tick` and trip
            // `verify_layers_consistent`'s ClipsOverlap check (seen on
            // alfred chicken.dw / snow strike.dw). The trace is emitted
            // in frame order, so `row` is non-decreasing: keep the last
            // tuple of each equal-row run.
            for notes in per_ch_notes.iter_mut() {
                let mut deduped: Vec<NoteRow> = Vec::with_capacity(notes.len());
                for &tuple in notes.iter() {
                    if let Some(last) = deduped.last_mut() {
                        if last.0 == tuple.0 {
                            *last = tuple; // same row → later note wins
                            continue;
                        }
                    }
                    deduped.push(tuple);
                }
                *notes = deduped;
            }

            // ---- build segments + tracks + clips (this song) ----
            for (ch, notes) in per_ch_notes.iter().enumerate() {
                // Build sample-change segments first, then split each
                // multi-pattern segment so every clip fits in one
                // pattern (xmrs's `Module::row_at` indexes track rows
                // from 0 relative to the clip's pattern — a clip that
                // spans patterns is silently skipped past its first
                // pattern boundary).
                let segments = split_at_pattern_boundaries(build_segments(
                    notes,
                    lane_rows[ch],
                    self.use_pitch_arpeggio,
                ));
                for (seg_idx, seg) in segments.iter().enumerate() {
                    let seg_len = seg.end_row + 1 - seg.start_row;
                    let mut rows = vec![Cell::default(); seg_len];
                    // The period-exact pitch map needs the active
                    // sample's header frequency for its finetune (new
                    // player). All notes in a segment share one sample.
                    let (seg_sample_freq, seg_sample_transpose) = self
                        .samples
                        .get(seg.sample_index)
                        .map(|s| (s.frequency, s.transpose))
                        .unwrap_or((8372, 0));
                    for &(abs_row, note_byte, paula_vol, _env_idx) in &seg.notes {
                        if abs_row < seg.start_row || abs_row > seg.end_row {
                            continue;
                        }
                        let rel_row = abs_row - seg.start_row;
                        // Whittaker note bytes index `Periods3`
                        // directly: each step is one semitone, byte 0
                        // is the lowest playable period. The
                        // `Sample.relative_pitch` from
                        // `sample_to_instrument` already aligns each
                        // `note_byte` is the simulator's
                        // `effective_note` (raw + global + channel
                        // transposes already combined). Saturate to
                        // the xmrs playable range so a transpose that
                        // pushes a note below 0 or above 119 doesn't
                        // wrap into the wrong octave.
                        let pitch = note_to_pitch(note_byte, seg_sample_freq, seg_sample_transpose);
                        // Whittaker envelope bytes are already in
                        // Paula's 0..=64 range (the on-disk values
                        // observed on `dw.xenon 2` peak at 0x40, never
                        // breaching Paula's 6-bit hardware limit). The
                        // replayer's `MULU global_vol ; LSR #6` step
                        // just scales by the song's master volume; at
                        // master = 64 (the default) the multiplication
                        // is the identity, so the envelope byte is the
                        // final Paula level. We saturate at 64 in case
                        // a less canonical module reaches 65..=127.
                        // `None` (no envelope armed) keeps the
                        // historical full-scale default so samples
                        // without paired `SetVolumeEnvelope` stay audible.
                        // Per-note loudness source, in priority order:
                        //   1. the runtime's envelope value (`paula_vol`,
                        //      new-player volume envelopes);
                        //   2. the old-player per-channel static volume
                        //      (`channel_volumes[ch]`) — the qball-era
                        //      replayer's `SetAmigaVolume(channelVolumes[ch])`
                        //      with no sample/envelope scaling;
                        //   3. full scale (un-shaped channel).
                        let effective_vol = paula_vol
                            .map(|v| v.min(64))
                            .or_else(|| {
                                if self.period_via_finetune {
                                    // leviathan-family: the per-note Paula
                                    // volume is the sample's static
                                    // per-instrument volume (record +0xE),
                                    // written raw on every note — NOT the
                                    // qball per-channel table. (Empire is
                                    // also `period_via_finetune` but its
                                    // sample volumes are full scale, so this
                                    // matches its old behaviour; its shaped
                                    // notes carry `paula_vol` above.)
                                    Some(
                                        self.samples
                                            .get(seg.sample_index)
                                            .map(|s| s.volume.min(64) as u8)
                                            .unwrap_or(64),
                                    )
                                } else if matches!(self.variant, DwVariant::Old) {
                                    Some(self.channel_volumes[ch].min(64) as u8)
                                } else {
                                    None
                                }
                            })
                            // Empire-family global master scale
                            // (`× master / 64`); identity elsewhere.
                            .map(|v| self.scale_master_volume(v));
                        let velocity = effective_vol
                            .map(Volume::from_byte_64)
                            .unwrap_or(Volume::FULL);
                        // The `velocity` slot on `CellEvent::NoteOn`
                        // is metadata only — xmrsplayer's hot path
                        // (see `xmrsplayer/src/channel/triggers.rs`)
                        // seeds `self.volume` from the sample's
                        // static `Sample.volume`, not the cell. To
                        // actually shape per-note loudness we also
                        // emit a [`TrackEffect::Volume`] at tick 0
                        // of the row; the channel-tick handler at
                        // `tick.rs:264` assigns `self.volume = v`
                        // when `current_tick == 0`, which lands the
                        // envelope's peak just as the note triggers.
                        let mut effects: Vec<TrackEffect> = Vec::new();
                        // When the segment's volume envelope is baked
                        // per-tick on the synthetic instrument
                        // (`env_index`), the instrument's full-scale
                        // sample + volume envelope already produce the
                        // loudness — emitting a cell `Volume` here would
                        // double-shape it. Otherwise (the common path)
                        // set the channel volume from the envelope's
                        // first step at the trigger tick.
                        if effective_vol.is_some() && seg.env_index.is_none() {
                            effects.push(TrackEffect::Volume {
                                value: velocity,
                                tick: 0,
                            });
                        }
                        // Pitch arpeggio (classic-effect path only).
                        // On the pitch-envelope path the arpeggio rides
                        // the synthetic instrument's looping pitch
                        // envelope instead (see `resolve_arp_instrument`
                        // / `arpeggio_to_pitch_envelope`); on the
                        // pitch-lane path it rides a `TrackPitch` Points
                        // curve (`attach_arpeggio_pitch_lanes`). Both are
                        // frame-faithful, so skip the lossy classic
                        // effect for them. The classic three-step
                        // `Arpeggio { half1, half2 }` (xmrs cycles base /
                        // base+half1 / base+half2 once per tick) now only
                        // ever fires for non-DW formats — for DW one of
                        // the two faithful paths is always active.
                        if !self.use_pitch_arpeggio && !self.use_arpeggio_pitch_lane {
                            if let Some(&arp_idx) = arp_by_row[ch].get(&abs_row) {
                                if let Some(arp) = self.arpeggios.get(arp_idx as usize) {
                                    let (half1, half2) = arp.classic_pair();
                                    if half1 != 0 || half2 != 0 {
                                        effects.push(TrackEffect::Arpeggio { half1, half2 });
                                    }
                                }
                            }
                        }
                        rows[rel_row] = Cell {
                            event: CellEvent::NoteOn { pitch, velocity },
                            effects,
                            expression: crate::core::cell::NoteExpression::NEUTRAL,
                        };
                    }

                    // Per-row envelope animation: for every note that
                    // carries an envelope index, walk the rows between
                    // this trigger and the next trigger and emit
                    // ghost `TrackEffect::Volume` cells reflecting the
                    // envelope's current step. Implementation note:
                    // xmrsplayer's hot path only acts on
                    // `TrackEffect::Volume { tick: 0 }` (it sets
                    // `self.volume = v` at that tick), so even an
                    // otherwise-empty `Cell` with the effect attached
                    // will land the new volume without retriggering
                    // the sample.
                    // Row-cell envelope animation — only when the volume
                    // envelope is NOT baked per-tick on the instrument
                    // (`env_index` is `None`). When it IS baked, the
                    // instrument's volume envelope replaces this coarse
                    // row-grid animation (which aliased the decay).
                    if seg.env_index.is_none() {
                        attach_envelope_animation(
                            &mut rows,
                            &seg.notes,
                            seg.start_row,
                            speed_u32,
                            &self.volume_envelopes,
                            self.master_volume,
                        );
                    }

                    // Resolve the instrument: the plain per-sample one,
                    // or a synthetic `(sample, arpeggio, volume-env)`
                    // instrument carrying the arpeggio pitch envelope
                    // and/or the per-tick volume envelope. Computed
                    // before the `tracks.push` so the `&mut
                    // module.instrument` borrow doesn't overlap.
                    let instr_idx = resolve_synth_instrument(
                        &mut module.instrument,
                        &mut arp_instr_cache,
                        &self.samples,
                        &self.arpeggios,
                        &self.volume_envelopes,
                        self.master_volume,
                        seg.sample_index,
                        seg.arp_index,
                        seg.env_index,
                    );
                    let track_idx = module.tracks.len() as u32;
                    module.tracks.push(Track::Notes {
                        name: format!("ch{} seg{:02} smp{:02}", ch, seg_idx, seg.sample_index),
                        instrument: instr_idx,
                        rows,
                        muted: false,
                    });
                    // `source_start_row` is pattern-relative per
                    // `Module::row_at` semantics (`row_in_track =
                    // row_u32 - source_start_row` where `row_u32`
                    // is the pattern-relative row 0..63). After the
                    // pattern-boundary split above, the segment lives
                    // inside one pattern, so this modulo is its
                    // first row within that pattern.
                    let pat_rel_start = (seg.start_row % ROWS_PER_PATTERN as usize) as u32;
                    clips_vec.push(Clip {
                        track: track_idx,
                        song,
                        target_channel: ch as u8,
                        position_tick: (seg.start_row as u32) * speed_u32,
                        speed_at_start: speed,
                        track_row_offset: 0,
                        source_start_row: pat_rel_start,
                        end_tick: ((seg.end_row + 1) as u32) * speed_u32,
                    });
                }
            }

            // ---- timeline for this song ----
            //
            // Whittaker songs have no native concept of "pattern"
            // the way XM / MOD do — the runtime walks one big flat
            // list of notes per channel. We synthesise 64-row
            // patterns (`ROWS_PER_PATTERN`) so `-d` debug dumps and
            // editor views read like a classic tracker. The segment
            // builder above splits Tracks at the same boundary so
            // each clip fits in one pattern. Each song's `tick` /
            // `pattern` numbering restarts at 0.
            for r in 0..total_rows {
                let r_u32 = r as u32;
                let pattern = r_u32 / ROWS_PER_PATTERN;
                timeline_entries.push(TimelineEntry {
                    song,
                    order_idx: pattern,
                    pattern_idx: pattern,
                    row_idx: r_u32 % ROWS_PER_PATTERN,
                    loop_iter: 0,
                    tick: r_u32 * speed_u32,
                    speed_at_row: speed,
                    bpm_at_row: song_bpm,
                });
            }

            song_traces.push((song, speed_u32, trace));
        }

        module.clips = SortedClips::from_unsorted(clips_vec);
        module.timeline_map = TimelineMap {
            entries: timeline_entries,
        };

        // ---- dedup ----
        // After the pattern-boundary split many adjacent segments
        // contain identical short cell sequences (especially the
        // long silent runs between musical phrases). Dedup fuses
        // them so the on-disk Track count stays manageable; the
        // Clips that referenced the dropped Tracks are re-pointed
        // and continue to play. Run once over all songs' tracks.
        crate::tracker::import::build::dedupe_tracks_by_content(&mut module);

        // ---- per-song automation lanes (vibrato / slide) ----
        // `StartVibrato`/`StopVibrato` → `LfoEvent` and `Slide` →
        // `SlideEvent` on the active Track's `TrackPitch` lane.
        // Walked *after* dedup so the resolved clip.track indices
        // are final, and per song so the clip lookup uses the
        // right timeline.
        for (song, song_speed, trace) in &song_traces {
            attach_vibrato_lanes(&mut module, *song, trace, *song_speed);
            attach_slide_lanes(&mut module, *song, trace, *song_speed);
            if self.use_arpeggio_pitch_lane {
                attach_arpeggio_pitch_lanes(&mut module, *song, trace);
            }
        }
        collapse_dead_duplicate_modulation_lanes(&mut module);

        module
    }

    /// Diagnostic helper: returns the number of `(channel, sample)`
    /// segments the conversion would emit before dedup. Used by
    /// the integration test to verify the split actually fires.
    #[doc(hidden)]
    pub fn segment_count_for_diagnostics(&self) -> usize {
        let speed = self.sub_song.as_ref().map(|s| s.speed.max(1)).unwrap_or(6);
        let trace = self.simulate();
        if trace.is_empty() {
            return 0;
        }
        let last_frame = trace.iter().map(|(f, _)| *f).max().unwrap_or(0);
        let total_rows = (last_frame.saturating_sub(1) / speed as u32 + 1) as usize;
        let speed_u32 = speed as u32;
        let instr_count = self.samples.len();
        let clamp = |s: u16| -> usize {
            let s = s as usize;
            if instr_count == 0 {
                0
            } else {
                s.min(instr_count - 1)
            }
        };
        let mut per_ch_notes: [Vec<NoteRow>; 4] = Default::default();
        for (frame, ev) in &trace {
            if let super::runtime::TickEvent::NoteOn {
                channel,
                effective_note,
                sample_index,
                volume,
                envelope_index,
                ..
            } = ev
            {
                let row = ((frame.saturating_sub(1)) / speed_u32) as usize;
                if row >= total_rows {
                    continue;
                }
                let ch = *channel as usize;
                if ch >= 4 {
                    continue;
                }
                per_ch_notes[ch].push((
                    row,
                    *effective_note,
                    sample_index.map(clamp).unwrap_or(0),
                    *volume,
                    *envelope_index,
                    None,
                ));
            }
        }
        let mut total = 0;
        for notes in &per_ch_notes {
            total += build_segments(notes, total_rows, false).len();
        }
        total
    }

    // ---------- internals ----------

    fn parse_samples(source: &[u8], layout: &DwLayout) -> Result<Vec<DwSample>, ImportError> {
        // Detection is best-effort: when the probes can't lock onto
        // the loader pattern we degrade to "no samples" rather than
        // erroring out, so the caller can still keep the
        // `DwModule` for diagnostics or layer in its own probes.
        //
        // The on-disk layout (verified in Ghidra on dw.xenon 2)
        // splits sample metadata across two contiguous tables:
        //
        // - `sample_data_offset` (0x1B6C in this module): per-
        //   sample concatenated blocks of the form
        //     u32  length_bytes
        //     u16  frequency
        //     i8[length_bytes] pcm
        //
        // - `sample_info_offset` (0x79A): `N × 12` bytes laid out
        //   as a runtime channel-info struct. Most of the struct
        //   is rewritten by the loader at boot from `sample_data`
        //   (PCM pointer, length, period). The only field that is
        //   **read** rather than overwritten is the `s32 loop_start`
        //   at byte offset +4 — that field is what we lift here.
        //
        // Per-sample volume does not live in either table for this
        // variant of the new player. Paula gets its level from the
        // playback layer (per-note in the track stream and/or
        // hardcoded defaults inside the replayer). The importer
        // therefore reports `volume = 64` (Paula full scale) for
        // every sample; a future pass that tracks the runtime
        // mixer state can refine this.
        let (Some(n_raw), Some(data_off)) = (layout.number_of_samples, layout.sample_data_offset)
        else {
            return Ok(Vec::new());
        };
        let n = n_raw as usize;
        if n == 0 {
            return Ok(Vec::new());
        }
        let info_off = layout.sample_info_offset;

        // ---- sample-data table ----
        //
        // Concatenated per-sample blocks at `data_off`:
        //   u32 length_bytes  ; big-endian
        //   u16 frequency     ; big-endian, Paula native rate hint
        //   i8[length_bytes] pcm
        //
        // Iterated `n` times. We stop early at the first short
        // read rather than erroring, so a slightly mis-tuned `n`
        // still yields the samples up to that point.
        let mut data_reader = open_reader(source, data_off)?;
        let mut samples: Vec<DwSample> = Vec::with_capacity(n);
        // RFC §1A shared audio pool: identical PCM payloads collapse to
        // a single `Arc<[i8]>`. Bucketed by an FNV-1a hash with a
        // byte-exact tie-break, so a hash collision can never alias two
        // different buffers.
        let mut pcm_pool: BTreeMap<u64, Vec<Arc<[i8]>>> = BTreeMap::new();
        for idx in 0..n {
            let Ok(length_bytes) = data_reader.read_u32_be() else {
                break;
            };
            let Ok(frequency) = data_reader.read_u16_be() else {
                break;
            };
            // Reject implausible lengths (a stray non-loader
            // signature would otherwise consume gigabytes here).
            if length_bytes as usize > data_reader.remaining() {
                break;
            }
            let Ok(pcm_bytes) = data_reader.read_slice(length_bytes as usize) else {
                break;
            };
            let pcm_vec: Vec<i8> = pcm_bytes.iter().map(|&b| b as i8).collect();
            let pcm: Arc<[i8]> = dedup_pcm(&mut pcm_pool, pcm_vec);

            // Per-sample loop-start.
            //
            // **New player**: the `s32` at +4 inside the 12-byte
            // sample-info row is genuine on-disk data — `0xFFFFFFFF`
            // (= -1) means one-shot, any non-negative value loops
            // forward from that byte offset.
            //
            // **Old player (qball-era)**: the sample-info table is
            // *runtime-built* — the loader writes the PCM pointer,
            // word length and base period into it at boot
            // (Ghidra: qball `Init` stores `length>>1` at info+4,
            // never a loop point). On disk those bytes are blank
            // padding, so reading "+4" yields a meaningless `0`
            // that would loop every sample from its start. There
            // is no loop metadata anywhere in the old format, so
            // the samples are treated as one-shot.
            // Sample-info row stride: the tetris-family loader
            // (the `enable_sample_transpose` modules) uses 16-byte
            // entries — `[ptr, loop_start(s32), len, finetune,
            // volume, transpose, pad]` — whereas xenon2/beast1 pack
            // 12-byte rows. `read_loop_start` indexes by this
            // stride, so getting it wrong reads later samples'
            // loop fields from the wrong offset (it surfaced as
            // spurious `loop=0` on tetris's odd samples).
            let info_stride = if layout.features.enable_sample_transpose {
                16
            } else {
                12
            };
            let loop_start = if matches!(layout.variant, DwVariant::Old) {
                None
            } else {
                info_off.and_then(|base| read_loop_start(source, base, idx, info_stride))
            };

            // Per-sample note transpose (tetris-family loader): a
            // signed semitone offset at sample-info byte +14 in a
            // 16-byte entry, added to the period-table index of
            // every note that plays this sample. Only read when the
            // module sets `enable_sample_transpose` — xenon2 / beast1
            // leave it 0 (their note path has no `+= transpose`
            // step). Mis-reading it as non-zero on those modules
            // would detune every note, so the feature gate matters.
            let transpose = if layout.features.enable_sample_transpose {
                info_off
                    .and_then(|base| source.get(base + idx * 16 + 14).copied())
                    .map(|b| b as i8)
                    .unwrap_or(0)
            } else {
                0
            };

            // Per-sample base volume (tetris-family 16-byte loader): a
            // big-endian word at sample-info byte +12, loaded straight
            // into `AUDxVOL` on every note trigger. Ghidra tetris
            // `PlayTick`: `AUDxVOL = *(short *)(sampleInfo + 0xC) -
            // DAT_0000041a` (the `Effect8` global offset, normally 0).
            // The on-disk words vary per sample (e.g. 64 / 56 / 54 / 52
            // / 30) — this is what makes tetris's lead/bass voices sit
            // at 56 while its percussion stays at 64; the previous
            // hard-coded `64` flattened them all to full scale. Only
            // read it on `enable_sample_transpose` modules: the 12-byte
            // xenon2/beast1 rows have no such field and their loudness
            // comes from the volume envelope instead. Clamp to Paula's
            // 0..=64 range.
            let volume = if layout.features.enable_sample_transpose {
                info_off
                    .and_then(|base| source.get(base + idx * 16 + 12..base + idx * 16 + 14))
                    .map(|b| u16::from_be_bytes([b[0], b[1]]).min(64))
                    .unwrap_or(64)
            } else if layout.period_via_finetune {
                // leviathan-era old-stream fine-tune player (C7): a
                // *static* per-instrument volume word lives at record
                // byte +0xE in the 16-byte instrument table (the
                // `(0xE,A5)` AUDxVOL write at every note trigger; no
                // envelope). Records are `instr_base + 0x10*idx`. When
                // the table isn't located (e.g. empire, whose 12-byte
                // records use a volume envelope instead), fall back to
                // full scale. Clamp to Paula's 0..=64.
                layout
                    .instrument_volume_offset
                    .and_then(|base| source.get(base + idx * 0x10 + 0xE..base + idx * 0x10 + 0x10))
                    .map(|b| u16::from_be_bytes([b[0], b[1]]).min(64))
                    .unwrap_or(64)
            } else {
                64
            };

            samples.push(DwSample {
                index: idx as u16,
                loop_start: loop_start.filter(|&s| s < length_bytes),
                length: length_bytes,
                frequency,
                volume,
                transpose,
                pcm,
            });
        }

        Ok(samples)
    }
}

impl DwModule {
    /// Read the sub-song header and follow each channel's position
    /// list. Both pieces degrade gracefully — when the probe
    /// hasn't located `sub_song_list_offset` (or any of the
    /// referenced position-list starts is out of bounds) the
    /// affected fields fall back to defaults.
    /// Parse every sub-song the importer can validate from the
    /// `sub_song_list_offset` row table. Stops when:
    ///
    /// - the next row would overlap the first position list (the
    ///   smallest channel offset observed so far — position
    ///   lists are always placed right after the sub-song
    ///   table), or
    /// - any channel offset in the row is `0` or points outside
    ///   the file, or
    /// - the speed byte is `0` (the spec's "speed > 255" sentinel
    ///   maps to a 0 byte at the next row's start when the table
    ///   is padded out).
    ///
    /// Returns an empty `Vec` when the sub-song table couldn't be
    /// located at all.
    fn parse_sub_songs(source: &[u8], layout: &DwLayout) -> Vec<DwSubSong> {
        // Row width is set by the detect probe. Three layouts are
        // currently recognised:
        //
        // - **8 bytes** — two shapes (disambiguated by the detected
        //   `sub_song_header`): a 4-voice `[ch0..ch3]` (header 0,
        //   e.g. sentinel) and a 3-voice `[param, ch0, ch1, ch2]`
        //   (header 2, e.g. archipelagos / fright night). Speed
        //   defaults to the historical Whittaker tempo of 6.
        // - **10 bytes** — `[speed, delay_speed, 4 × u16]`, the
        //   canonical new player (xenon2, beast1.*, speedball).
        // - **18 bytes** — `[reserved, speed, 4 × u32]`, the
        //   qball-era 32-bit build.
        //
        // Other row widths fall back to 10 to stay safe.
        let row = match layout.sub_song_row_width {
            8 | 10 | 18 => layout.sub_song_row_width,
            _ => 10,
        };
        let header = layout.sub_song_header;
        let ptr_width = if layout.uses_32bit_pointers { 4 } else { 2 };
        // Voice count = the channel pointers that fit after the
        // header, exactly what `init_main`'s setup loop iterates
        // (`(row - header) / ptr_width`; its `CMP.W #voices` bound).
        // Channels beyond it are unused and left at offset 0 → an
        // empty (silent) position list, NOT a phantom voice.
        let voices = row
            .saturating_sub(header)
            .checked_div(ptr_width)
            .unwrap_or(DW_NUM_CHANNELS)
            .clamp(1, DW_NUM_CHANNELS);
        let mut out = Vec::new();
        let Some(off) = layout.sub_song_list_offset else {
            return out;
        };

        let mut min_list_off: usize = source.len();
        let mut cursor = off;
        // Hard cap as a defensive bound — every Whittaker module
        // observed ships ≤ a dozen sub-songs.
        for _ in 0..64 {
            if cursor + row > source.len() {
                break;
            }
            if cursor + row > min_list_off {
                break;
            }
            // Header layout differs by row width and by player:
            // - 8-byte rows have no header (use a default speed
            //   of 6, the Whittaker historical tempo);
            // - 18-byte qball-era rows pad byte 0 to zero for
            //   longword alignment, so speed sits at `row + 1`;
            // - 10-byte rows come in two new-player flavours:
            //     * `[u8 speed, u8 delay, 4×u16]` — xenon2,
            //       beast1 (speed in byte 0, delay in byte 1);
            //     * `[u16 speed, 4×u16]` — tetris and kin, which
            //       read the speed as a full word (Ghidra:
            //       `MOVE.W (A0,D0), $41C(A3)` at tetris+0x5A).
            //   Disambiguate by byte 0: a real per-row speed byte
            //   is never 0, so byte0==0 means the word form, where
            //   the speed is the low byte (byte 1) and there is no
            //   delay.
            // The header carries the speed/delay (when present). A
            // header-0 row (8-byte 4-voice) has none → Whittaker
            // default 6. A header-2 row reads them like the canonical
            // 10-byte layout — including the 3-voice 8-byte shape,
            // whose param byte IS the speed (Ghidra `init_main`:
            // `DAT_624 = byte[table + ss*row]`, the LongWait
            // note-duration multiplier — archipelagos = 7, not 6).
            let (speed, delay_speed) = if header == 0 {
                (6u8, 0u8)
            } else if layout.uses_32bit_pointers {
                (source[cursor + 1], source[cursor])
            } else if source[cursor] != 0 {
                (source[cursor], source[cursor + 1])
            } else {
                (source[cursor + 1], 0u8)
            };
            let mut offs = [0u32; DW_NUM_CHANNELS];
            let mut valid = true;
            // Read only the real `voices` channel pointers; the rest
            // stay 0 (→ empty position list = silent). On 3-voice
            // modules this stops the param word being read as ch0's
            // base and stops a phantom ch3 from playing.
            // `i` drives byte-pointer arithmetic (`ptr_width * i`), not
            // just the `offs[i]` write — an iterator rewrite would
            // obscure the pointer walk.
            #[allow(clippy::needless_range_loop)]
            for i in 0..voices {
                let p = cursor + header + ptr_width * i;
                let o = if ptr_width == 4 {
                    u32::from_be_bytes([source[p], source[p + 1], source[p + 2], source[p + 3]])
                } else {
                    u16::from_be_bytes([source[p], source[p + 1]]) as u32
                };
                if o == 0 || (o as usize) >= source.len() {
                    valid = false;
                    break;
                }
                offs[i] = o;
            }
            // First row is treated as authoritative even when
            // `speed == 0`, because some modules use that slot
            // as a runtime-only "current sub-song" cache (the
            // host overwrites it before calling `Play`); ignoring
            // it would drop legitimate music. From row 2 onward
            // we use `speed == 0` as the loop sentinel.
            if !valid || (!out.is_empty() && speed == 0) {
                break;
            }
            let row_min = offs.iter().copied().min().unwrap_or(0) as usize;
            min_list_off = min_list_off.min(row_min);
            out.push(DwSubSong {
                speed,
                delay_speed,
                channel_position_offsets: offs,
            });
            cursor += row;
        }
        out
    }
}

/// Decode the four per-channel position lists for a given
/// sub-song. Re-runs `read_position_list` against each of the
/// sub-song's channel offsets, so callers that want to switch
/// the active sub-song can rebuild the lists without re-parsing
/// the rest of the module.
pub fn position_lists_for(
    source: &[u8],
    sub_song: &DwSubSong,
    uses_32bit_pointers: bool,
    start_offset: isize,
) -> [DwPositionList; DW_NUM_CHANNELS] {
    let mut lists: [DwPositionList; DW_NUM_CHANNELS] = Default::default();
    for (ch, &off) in sub_song.channel_position_offsets.iter().enumerate() {
        lists[ch] = read_position_list(source, off as usize, uses_32bit_pointers, start_offset);
    }
    lists
}

impl DwModule {
    /// Collect every unique track offset referenced by any
    /// position list, then scan each one until its `EndOfTrack`
    /// (`0x80`) command. De-duplication keeps the on-disk-order
    /// sort that a `BTreeMap` provides, which makes the resulting
    /// `Vec` stable across re-imports.
    /// Walk the per-channel volume-envelope pointer table and
    /// decode each entry into a [`DwVolumeEnvelope`].
    ///
    /// On-disk layout (verified on dw.xenon 2 at file offset
    /// 0x1A48):
    ///
    /// ```text
    /// [step_interval byte][envelope bytes... terminator]
    /// [step_interval byte][envelope bytes... terminator]
    /// ...
    /// ```
    ///
    /// where each pointer-table entry is a big-endian `u16` whose
    /// value is the file offset of the first **envelope byte**
    /// (i.e. the step-interval byte sits at `entry - 1`). An
    /// envelope ends at the first byte with the MSB set; the
    /// terminator's low 7 bits are the sustain volume and become
    /// the last entry of [`DwVolumeEnvelope::steps`]. Reading is
    /// bounded at `MAX_STEPS` to defend against pointers into
    /// non-envelope regions.
    fn parse_volume_envelopes(
        source: &[u8],
        layout: &super::detect::DwLayout,
    ) -> Vec<DwVolumeEnvelope> {
        const MAX_STEPS: usize = 128;

        let (Some(table_off), Some(n)) = (
            layout.volume_envelope_table_offset,
            layout.volume_envelope_table_len,
        ) else {
            return Vec::new();
        };
        let n = n as usize;
        let mut out = Vec::with_capacity(n);
        for idx in 0..n {
            let entry_off = table_off + idx * 2;
            if entry_off + 2 > source.len() {
                break;
            }
            // The pointer-table entries are A3-relative (the player
            // does `MOVEA.W (table,Dn),A2 / ADDA.L A3,A2`), so rebase
            // through `start_offset` to a real file offset. No-op on
            // modules whose A3 sits at file 0 (almost all of them).
            let raw = u16::from_be_bytes([source[entry_off], source[entry_off + 1]]) as usize;
            let env_start = if raw == 0 {
                0
            } else {
                match (raw as isize).checked_add(layout.start_offset) {
                    Some(f) if f >= 0 && (f as usize) < source.len() => f as usize,
                    _ => 0,
                }
            };
            // The step-interval byte sits one byte before the
            // envelope. A zero `env_start` would indicate the
            // entry is unused — skip it but keep the slot so
            // indices stay aligned with the dispatcher's
            // `(byte - threshold)` calculation.
            let step_interval = if env_start > 0 && env_start - 1 < source.len() {
                source[env_start - 1]
            } else {
                0
            };
            let mut steps = Vec::new();
            let mut cursor = env_start;
            while cursor < source.len() && steps.len() < MAX_STEPS {
                let b = source[cursor];
                steps.push(b & 0x7F);
                cursor += 1;
                if b & 0x80 != 0 {
                    break;
                }
            }
            out.push(DwVolumeEnvelope {
                index: idx as u16,
                step_interval,
                steps,
            });
        }
        out
    }

    /// Walk the pitch-arpeggio pointer table (the `0x90..` bracket
    /// LEA target) and decode each entry into a [`DwArpeggio`].
    ///
    /// On-disk layout mirrors the volume-envelope table: a flat
    /// array of big-endian `u16` PC-relative offsets, each
    /// pointing at a semitone-offset byte stream terminated by a
    /// byte with its `0x80` bit set (the terminator's low 7 bits
    /// are the final offset). Offsets are read as signed `i8`.
    ///
    /// Validation: an entry whose decoded offsets all exceed
    /// ±0x30 semitones is almost certainly a mis-resolved pointer
    /// into non-arpeggio data, so the whole table is discarded
    /// (returns empty) rather than emitting nonsense pitch
    /// effects. This keeps a wrong probe from corrupting playback.
    fn parse_arpeggios(source: &[u8], layout: &DwLayout) -> Vec<DwArpeggio> {
        const MAX_STEPS: usize = 64;

        let (Some(table_off), Some(n)) = (layout.arpeggio_table_offset, layout.arpeggio_table_len)
        else {
            return Vec::new();
        };
        let n = n as usize;
        let mut out = Vec::with_capacity(n);
        let mut sane_entries = 0usize;
        for idx in 0..n {
            let entry_off = table_off + idx * 2;
            if entry_off + 2 > source.len() {
                break;
            }
            // A3-relative entry (same as the envelope table) — rebase
            // through `start_offset` to a file offset.
            let raw = u16::from_be_bytes([source[entry_off], source[entry_off + 1]]) as usize;
            let arp_start = if raw == 0 {
                0
            } else {
                match (raw as isize).checked_add(layout.start_offset) {
                    Some(f) if f >= 0 && (f as usize) < source.len() => f as usize,
                    _ => 0,
                }
            };
            let mut offsets = Vec::new();
            let mut cursor = arp_start;
            while cursor < source.len() && offsets.len() < MAX_STEPS {
                let b = source[cursor];
                // Low 7 bits are the semitone offset, sign-extended
                // from the 7-bit field (bit 6 is the sign). The
                // 0x80 bit marks the terminator (still a valid
                // offset).
                let raw7 = (b & 0x7F) as i16;
                let signed = if raw7 >= 0x40 {
                    (raw7 - 0x80) as i8
                } else {
                    raw7 as i8
                };
                offsets.push(signed);
                cursor += 1;
                if b & 0x80 != 0 {
                    break;
                }
            }
            if !offsets.is_empty() && offsets.iter().all(|&o| (-0x30..=0x30).contains(&o)) {
                sane_entries += 1;
            }
            out.push(DwArpeggio {
                index: idx as u16,
                offsets,
            });
        }
        // If fewer than half the entries decoded to plausible
        // semitone ranges, the table base is probably wrong —
        // drop the lot so the projection emits no arpeggio rather
        // than garbage pitch jumps.
        if out.is_empty() || sane_entries * 2 < out.len() {
            return Vec::new();
        }
        out
    }

    /// Collect every unique track referenced by **any** sub-song's
    /// position lists (de-duplicated by file offset), so the shared
    /// [`Self::tracks`] bank covers every tune the module can play.
    fn parse_tracks(
        source: &[u8],
        all_lists: &[[DwPositionList; DW_NUM_CHANNELS]],
        dispatcher: &super::detect::DwDispatcher,
        features: &super::detect::DwFeatures,
        command_map: Option<&super::command_map::DwCommandMap>,
    ) -> Vec<DwTrack> {
        let mut seen: BTreeMap<u32, DwTrack> = BTreeMap::new();
        for lists in all_lists {
            for list in lists {
                for &offset in &list.entries {
                    seen.entry(offset).or_insert_with(|| {
                        // Read a generous raw window, then let the event
                        // decoder (the single authority on parameter
                        // widths) report exactly how far the track ran and
                        // trim the stored bytes to that span.
                        let mut bytes = read_track_bytes(source, offset as usize);
                        let (events, used) = super::event::decode_track_counted(
                            &bytes,
                            dispatcher,
                            features,
                            command_map,
                        );
                        bytes.truncate(used);
                        DwTrack {
                            offset,
                            bytes,
                            events,
                        }
                    });
                }
            }
        }
        seen.into_values().collect()
    }
}

/// Translate every `VibratoStart` / `VibratoStop` event in the
/// simulator trace into `LfoEvent::Set` / `LfoEvent::Clear`
/// entries on an `AutomationLane { target: TrackPitch(tidx),
/// kind: Lfo, .. }` for the Track active on the channel at the
/// event's tick.
///
/// Multi-Track vibrato (one that starts on Track A and continues
/// past a clip boundary into Track B) is **not yet** propagated
/// to B — only the Track containing the original `StartVibrato`
/// row gets armed. A follow-up that re-arms the LFO at every
/// clip transition will be needed to fully match the original's
/// modulation continuity.
fn attach_vibrato_lanes(
    module: &mut Module,
    song: u16,
    trace: &[(u32, super::runtime::TickEvent)],
    speed: u32,
) {
    use crate::core::daw::automation::{AutomationLane, AutomationTarget, LaneKind, LfoEvent};
    use crate::core::fixed::fixed::{Q15, Q8_8};
    use crate::core::waveform::Waveform;

    // A vibrato is a per-CHANNEL effect, but the xmrs LFO lane is
    // per-Track and a held note crosses several clips/tracks (the
    // segment builder splits at every 64-row pattern boundary). The
    // old code armed only the track active at the `VibratoStart`
    // tick, so a sustained, looping lead note (xenon2 ch1, sample 35
    // loops) lost its wobble the moment playback crossed into the
    // next pattern's clip. Instead, project each vibrato **span**
    // (`Start` until the next `Start`/`Stop` on that channel) onto
    // *every* clip it overlaps, emitting a self-contained
    // `Set..Clear` pair bounded to each clip's tick window. Bounding
    // every span keeps it safe against `dedupe_tracks_by_content`
    // sharing one Track across clips/channels — a shared Track's lane
    // is only "armed" inside the exact windows we wrote.
    #[derive(Clone, Copy)]
    struct VibParams {
        speed: Q8_8,
        depth: Q15,
    }
    // Per-channel sorted (tick, Some(params)=Start | None=Stop).
    let mut vib_by_ch: [alloc::vec::Vec<(u32, Option<VibParams>)>; 4] = Default::default();
    for (frame, ev) in trace {
        let tick = (frame.saturating_sub(1) / speed) * speed;
        match ev {
            super::runtime::TickEvent::VibratoStart {
                channel,
                speed: vspd,
                depth,
            } if (*channel as usize) < 4 => {
                // Whittaker vibrato is a PERIOD triangle (Ghidra
                // `play_tick` / ref `DoFrameStuff` 2169): `VibratoValue`
                // ramps `0→max→0` by `vspd` each frame and the Paula
                // period is nudged ±VibratoValue, sign flipping every
                // half-cycle. Full triangle = `4·max/vspd` frames; one
                // LFO tick = one Whittaker frame, so cycles/tick =
                // `vspd/(4·max)` and the Q8.8 phase speed (raw 256 = 1
                // cycle/tick, `lanes.rs`) is `64·vspd/max`. (xenon2's
                // leads use `vspd = max/2` → raw 32, a constant ~8-
                // frame wobble whose depth varies.)
                //
                // Depth: peak deviation is `max` **Paula-period units**.
                // The player now applies the effect vibrato in period
                // space for Amiga modules (`update_frequency`:
                // `outPeriod = realPeriod ± lfo`, matching FT2/Protracker),
                // so stash `max` straight in the depth raw — the LFO peaks
                // at `depth.raw() = max` period units. (Was `max << 3`, a
                // semitone hack for the old pitch-space path that scaled
                // the wobble by the note's register — wrong by design.)
                // Waveform = `BipolarTriangle`: the DW vibrato modulates
                // the period with a symmetric triangle
                // (`0 → +max → 0 → −max → 0`), exactly
                // `Waveform::BipolarTriangle` — bipolar, swinging the
                // period both ways like the original.
                let denom = (*depth as u32).max(1);
                let params = VibParams {
                    speed: Q8_8::from_raw(
                        ((64 * *vspd as u32) / denom).clamp(1, i16::MAX as u32) as i16
                    ),
                    depth: Q15::from_raw((*depth as i32).clamp(0, i16::MAX as i32) as i16),
                };
                vib_by_ch[*channel as usize].push((tick, Some(params)));
            }
            super::runtime::TickEvent::VibratoStop { channel } if (*channel as usize) < 4 => {
                vib_by_ch[*channel as usize].push((tick, None));
            }
            _ => {}
        }
    }

    let mut events_per_track: BTreeMap<u32, Vec<LfoEvent>> = BTreeMap::new();
    for ch in 0..4u8 {
        let evs = &mut vib_by_ch[ch as usize];
        if evs.is_empty() {
            continue;
        }
        evs.sort_by_key(|(t, _)| *t);
        let clips = module.clips.lane(song, ch);
        for (i, &(start_tick, params)) in evs.iter().enumerate() {
            // Only `Start`s open a span; `Stop`s just end the
            // previous one (handled via `span_end`).
            let Some(params) = params else { continue };
            let span_end = evs.get(i + 1).map(|(t, _)| *t).unwrap_or(u32::MAX);
            if span_end <= start_tick {
                continue; // zero-width (two events same tick)
            }
            for clip in clips {
                let cs = clip.position_tick.max(start_tick);
                let ce = clip.end_tick.min(span_end);
                if cs >= ce {
                    continue;
                }
                let lane = events_per_track.entry(clip.track).or_default();
                lane.push(LfoEvent::Set {
                    tick: cs,
                    speed: params.speed,
                    depth: params.depth,
                    waveform: Waveform::BipolarTriangle,
                    retrig: false,
                });
                lane.push(LfoEvent::Clear { tick: ce });
            }
        }
    }

    for (track_idx, mut events) in events_per_track {
        if events.is_empty() {
            continue;
        }
        // Lane invariant: events sorted by tick ascending. At an
        // equal tick a `Clear` must precede a `Set` so a span that
        // ends exactly where the next begins (adjacent clips sharing
        // a Track after dedup) stays armed — the later `Set` wins in
        // `lfo_state_at`'s forward walk.
        events.sort_by_key(|e| {
            let (tick, is_set) = match e {
                LfoEvent::Set { tick, .. } => (*tick, 1u8),
                LfoEvent::DepthOnly { tick, .. } => (*tick, 1),
                LfoEvent::SpeedOnly { tick, .. } => (*tick, 1),
                LfoEvent::WaveformOnly { tick, .. } => (*tick, 1),
                LfoEvent::Clear { tick } => (*tick, 0),
            };
            (tick, is_set)
        });
        module.automation.push(
            AutomationLane::new_with_kind(
                AutomationTarget::TrackPitch(track_idx),
                LaneKind::Lfo { events },
            )
            .with_song(song),
        );
    }
}

/// Translate `SlideStart` / `SlideStop` events in the simulator
/// trace into `SlideEvent::Set` / `SlideEvent::Clear` entries on
/// a per-Track `AutomationLane { kind: Slide }` targeting
/// `TrackPitch`. The active Track at the slide's tick is the one
/// that hosts the slide.
///
/// `dw.xenon 2` only has one `Slide` event but the conversion
/// infrastructure mirrors the vibrato path so future modules
/// with denser slide usage benefit too.
fn attach_slide_lanes(
    module: &mut Module,
    song: u16,
    trace: &[(u32, super::runtime::TickEvent)],
    speed: u32,
) {
    use crate::core::daw::automation::{AutomationLane, AutomationTarget, LaneKind, SlideEvent};
    use crate::core::fixed::fixed::Q15;

    let mut events_per_track: BTreeMap<u32, Vec<SlideEvent>> = BTreeMap::new();
    // Track the lane each open slide lives on, per channel, so the
    // bounding `Clear` lands on the SAME `TrackPitch(n)` lane as its
    // `Set` — at the `SlideStop` tick a new note's clip is already
    // active, so resolving the track from that tick would attach the
    // `Clear` to the wrong lane and leave the slide lane armed.
    let mut open_slide_track: BTreeMap<u8, u32> = BTreeMap::new();
    for (frame, ev) in trace {
        let row = frame.saturating_sub(1) / speed;
        let tick = row * speed;
        let (ch, slide_ev) = match ev {
            super::runtime::TickEvent::SlideStart {
                channel,
                speed: spd,
                counter,
            } => {
                // Whittaker slide, read from Ghidra `play_tick`
                // (effect `case 0x81` arm + the per-frame apply):
                //
                //   arm:   SlideValue = 0; SlideSpeed = speed;
                //          SlideCounter = counter; SlideEnabled = true
                //   tick:  if SlideCounter == 0 {
                //              SlideValue += SlideSpeed;
                //              period = base - SlideValue   // base re-
                //          } else { SlideCounter-- }        // -read each
                //                                           // frame
                //
                // `base` (the note period) is recomputed fresh every
                // frame, so `period - SlideValue` with `SlideValue =
                // k·speed` is a *LINEAR* period ramp of `-speed` units
                // per frame (NOT accelerating — the old `N(N+1)/2`
                // average was wrong), preceded by a `counter`-frame
                // pre-engage delay.
                //
                // The player's TrackPitch Slide lane already ACCUMULATES
                // (`lanes.rs`: `period = period.saturating_add_signed(
                // rate.raw())` every tick → `period + k·rate`), and a
                // player tick maps 1:1 to a DW frame here (`speed` ticks
                // per row = `speed` frames per row). So the faithful
                // mapping is `rate = -speed` (×1, period units) with the
                // Set delayed by `counter` ticks. The lane is told to run
                // on tick 0 too (`pitch_slide_ticks_at_row_zero`), so it
                // advances on every frame of the held note just like the
                // replayer — no `(speed-1)/speed` undercount. `SlideStop`
                // (emitted by the runtime at the next row read) bounds it
                // with a `Clear`. Residual: `tick` is row-quantised, so
                // the engage point can be off by < 1 row.
                let rate_raw = -(*spd as i32);
                let rate = Q15::from_raw(rate_raw.clamp(i16::MIN as i32, i16::MAX as i32) as i16);
                (
                    *channel,
                    SlideEvent::Set {
                        tick: tick + *counter as u32,
                        rate,
                        fine: false,
                    },
                )
            }
            super::runtime::TickEvent::SlideStop { channel } => {
                (*channel, SlideEvent::Clear { tick })
            }
            _ => continue,
        };

        let target_track = match &slide_ev {
            SlideEvent::Set { .. } => {
                let t = module.clips.active_at(song, ch, tick).map(|(_, c)| c.track);
                if let Some(t) = t {
                    open_slide_track.insert(ch, t);
                }
                t
            }
            SlideEvent::Clear { .. } => open_slide_track
                .remove(&ch)
                .or_else(|| module.clips.active_at(song, ch, tick).map(|(_, c)| c.track)),
        };
        if let Some(track) = target_track {
            events_per_track.entry(track).or_default().push(slide_ev);
        }
    }

    for (track_idx, mut events) in events_per_track {
        if events.is_empty() {
            continue;
        }
        events.sort_by_key(|e| match e {
            SlideEvent::Set { tick, .. } => *tick,
            SlideEvent::Clear { tick } => *tick,
        });
        module.automation.push(
            AutomationLane::new_with_kind(
                AutomationTarget::TrackPitch(track_idx),
                LaneKind::Slide { events },
            )
            .with_song(song),
        );
    }
}

/// Translate the simulator's per-frame [`TickEvent::Arpeggio`] stream
/// into a per-Track `AutomationLane { kind: Points }` of
/// [`AutomationValue::Pitch`] points on `TrackPitch` — the faithful
/// projection of the replayer's per-channel arpeggio pointer.
///
/// The replayer advances the pointer one offset per **frame** (not per
/// row) and adds the offset to the note's period-table *index*, so the
/// points must land at frame resolution. A DW frame maps 1:1 to a
/// player tick (`speed` ticks per row = `speed` frames per row), and
/// the player's absolute tick at frame `f` is `f - 1`; place each point
/// there. The runtime already emits only on *change* (and a `0` when a
/// note ends or no arpeggio is armed), and the lane latches a value
/// forward, so the point set is the minimal change-stream. The active
/// clip at the point's tick supplies the host Track (its content is
/// what carries the arpeggio commands).
///
/// [`TickEvent::Arpeggio`]: super::runtime::TickEvent::Arpeggio
fn attach_arpeggio_pitch_lanes(
    module: &mut Module,
    song: u16,
    trace: &[(u32, super::runtime::TickEvent)],
) {
    use crate::core::daw::automation::{
        AutomationLane, AutomationPoint, AutomationTarget, AutomationValue, LaneKind,
    };
    use crate::core::fixed::units::PitchDelta;

    // Per-Track tick→offset. A BTreeMap keyed by tick deduplicates the
    // rare case of two channels sharing one deduped Track at the same
    // tick (identical content → identical offset, so the survivor is
    // correct); it also keeps the points sorted for free.
    let mut by_track: BTreeMap<u32, BTreeMap<u32, i8>> = BTreeMap::new();
    for (frame, ev) in trace {
        let super::runtime::TickEvent::Arpeggio { channel, semitones } = ev else {
            continue;
        };
        let tick = frame.saturating_sub(1);
        let Some((_, clip)) = module.clips.active_at(song, *channel, tick) else {
            continue;
        };
        by_track
            .entry(clip.track)
            .or_default()
            .insert(tick, *semitones);
    }

    for (track_idx, offsets) in by_track {
        if offsets.is_empty() {
            continue;
        }
        let points: Vec<AutomationPoint> = offsets
            .into_iter()
            .map(|(tick, semis)| AutomationPoint {
                tick,
                value: AutomationValue::Pitch(PitchDelta::from_semitones(semis as i16)),
            })
            .collect();
        module.automation.push(
            AutomationLane::new_with_kind(
                AutomationTarget::TrackPitch(track_idx),
                LaneKind::Points(points),
            )
            .with_song(song),
        );
    }
}

/// Read a generous **raw window** of one track byte stream starting
/// at `start`, capped at [`TRACK_BYTE_CAP`] bytes (or end-of-file).
///
/// This deliberately does NOT try to find the track's `EndOfTrack`
/// terminator or account for per-command parameter widths: that
/// knowledge lives in exactly one place,
/// [`super::event::decode_track_counted`], which the caller
/// ([`DwModule::parse_tracks`]) runs over this window and then uses
/// to trim the buffer to the real consumed span. Keeping the
/// parameter-width table out of here means the byte delimiter and
/// the event decoder can never disagree (they used to: the old
/// hand-rolled table here assumed `Effect9` took 1 byte and was
/// blind to the jump-table command remap, so a parameter byte equal
/// to `0x80` could truncate a track early).
fn read_track_bytes(source: &[u8], start: usize) -> Vec<u8> {
    if start == 0 || start >= source.len() {
        return Vec::new();
    }
    let end = (start + TRACK_BYTE_CAP).min(source.len());
    source[start..end].to_vec()
}

/// Defensive cap on a single track's byte span — well past any real
/// Whittaker track, but bounds a misaligned `start` from scanning
/// off into the rest of the image.
const TRACK_BYTE_CAP: usize = 0x800;

/// Walk a position list starting at `start`. Reads
/// `u16` BE entries until either:
///
/// - a `0x0000` terminator (plain end-of-list);
/// - a value with bit 15 set (loop back to `value & 0x7FFF` —
///   stored separately in [`DwPositionList::loop_to`]).
///
/// Returns an empty list when `start` is out of bounds or no
/// terminator is reached within a defensive cap (the replayer
/// itself doesn't bound list length, but we do — otherwise a
/// misaligned `start` could chase u16s for the rest of the file).
fn read_position_list(
    source: &[u8],
    start: usize,
    uses_32bit: bool,
    start_offset: isize,
) -> DwPositionList {
    const MAX_ENTRIES: usize = 1024;

    let mut list = DwPositionList::default();
    let width = if uses_32bit { 4 } else { 2 };
    // `start` and every stored track offset are A3-relative; rebase
    // through `start_offset` to obtain real file positions. Entries
    // are stored already-rebased (file offsets) so downstream track
    // parsing can use them directly. `rebase` returns `None` when a
    // value falls outside the file (corrupt / sentinel).
    let rebase = |v: u32| -> Option<usize> {
        let f = v as isize + start_offset;
        (f >= 0 && (f as usize) < source.len()).then_some(f as usize)
    };
    let Some(mut cursor) = rebase(start as u32) else {
        return list;
    };
    if start == 0 || cursor + width > source.len() {
        return list;
    }
    let loop_mask: u32 = if uses_32bit { 0x8000_0000 } else { 0x0000_8000 };
    let value_mask: u32 = !loop_mask;
    for _ in 0..MAX_ENTRIES {
        if cursor + width > source.len() {
            break;
        }
        let raw: u32 = if uses_32bit {
            u32::from_be_bytes([
                source[cursor],
                source[cursor + 1],
                source[cursor + 2],
                source[cursor + 3],
            ])
        } else {
            u16::from_be_bytes([source[cursor], source[cursor + 1]]) as u32
        };
        cursor += width;
        if raw == 0 {
            // Ghidra `play_tick` case 0x80 (EndOfTrack): a `0` entry
            // is the list terminator and resets the channel to entry
            // 0 (`RestartPosition + 1` with the default restart 0 in
            // `HandleEndOfTrackEffect`). So a plain-`0`-terminated
            // list LOOPS to its start, it does not fall silent — the
            // earlier `loop_to = None` here made every such channel
            // finish after one pass (xenon2 played once then stopped).
            // The Effect9-set `RestartPosition` refinement (a mid-list
            // jump target) isn't modelled yet; the corpus modules seen
            // so far restart at 0.
            list.loop_to = Some(0);
            break;
        }
        if raw & loop_mask != 0 {
            // The on-disk value (with the loop-flag bit cleared)
            // is the **track offset** to resume from — Whittaker
            // encodes the loop target the same way as a regular
            // entry, just with the MSB set as a sentinel. The
            // runtime expects a 0-based **index** into
            // [`Self::entries`], so resolve the target by scanning
            // for the first matching entry. When no entry matches
            // (rare — usually means the loop points one past the
            // last entry, restarting from the top) we fall back
            // to position 0.
            // Rebase the loop target into the same (file-offset)
            // space as `entries` before resolving it to an index.
            let target_offset = rebase(raw & value_mask).unwrap_or(0) as u32;
            let target_index = list
                .entries
                .iter()
                .position(|&e| e == target_offset)
                .unwrap_or(0) as u32;
            list.loop_to = Some(target_index);
            break;
        }
        // Store the track offset already rebased to a file offset so
        // downstream track parsing can dereference it directly. A
        // value that rebases out of range terminates the list.
        let Some(file_off) = rebase(raw) else {
            break;
        };
        list.entries.push(file_off as u32);
    }
    list
}

/// Resolve each channel's real loop target by following a `SeqPtr`
/// (`cmd 0x89`) in its **intro** (first) track.
///
/// The new player starts a channel on its `channel_position_offset`
/// array, but a track ending `SeqPtr(X) ; SeqAdvance` (cmd `0x89`,
/// Ghidra `Play` case 0x89: `chan+0x600 = X ; index = 0`) redirects the
/// sequencer to the sub-sequence at `X`, which then loops on *itself* —
/// so the intro plays ONCE and the channel loops the sub-sequence, never
/// back to the intro. [`read_position_list`] can't see this: it stops at
/// the initial array's own terminator and loops to entry 0, making the
/// channel re-play its intro every cycle. On bad company all of
/// ch0/1/3 are intro-`SeqPtr` channels (e.g. ch0 intro `…89 08 94 80` →
/// sub-sequence `0x894 = [body, loop]`), so the importer's wrap-to-intro
/// diverged from the oracle the moment its loop fired (~t560: it replays
/// the slid intro note while the oracle is mid-body). Here we decode the
/// intro, and if it sets a `SeqPtr(X)`, rebuild the list as
/// `[intro] ++ sub-sequence(X)` with the loop pointing inside the
/// sub-sequence (`sub.loop_to + 1`, past the intro). A no-op for
/// channels whose intro sets no `SeqPtr`.
fn follow_seq_ptr_loops(
    source: &[u8],
    lists: &mut [DwPositionList; DW_NUM_CHANNELS],
    dispatcher: &super::detect::DwDispatcher,
    features: &super::detect::DwFeatures,
    command_map: Option<&super::command_map::DwCommandMap>,
    uses_32bit: bool,
    start_offset: isize,
) {
    // `lists[ch]` is both read at the top and reassigned at the bottom
    // of the body; an `iter_mut()` rewrite would fight the borrow on the
    // intervening `read_position_list` reads.
    #[allow(clippy::needless_range_loop)]
    for ch in 0..DW_NUM_CHANNELS {
        let Some(&intro_off) = lists[ch].entries.first() else {
            continue;
        };
        let bytes = read_track_bytes(source, intro_off as usize);
        let (events, _) =
            super::event::decode_track_counted(&bytes, dispatcher, features, command_map);
        let Some(x_raw) = events.iter().find_map(|e| match e {
            super::event::DwTrackEvent::SeqPtr(x) => Some(*x),
            _ => None,
        }) else {
            continue;
        };
        let sub = read_position_list(source, x_raw as usize, uses_32bit, start_offset);
        if sub.entries.is_empty() {
            continue;
        }
        let mut entries = alloc::vec![intro_off];
        entries.extend_from_slice(&sub.entries);
        lists[ch] = DwPositionList {
            entries,
            loop_to: Some(sub.loop_to.unwrap_or(0) + 1),
        };
    }
}

/// Walk every (already-placed) NoteOn cell in `rows` and emit
/// ghost `TrackEffect::Volume` cells between triggers, reflecting
/// the envelope's current value at each row's frame.
///
/// Whittaker's envelope ticks at `Play+0x412`:
///
/// ```text
/// SUBQ.B #1, chan[+0x2B]           ; decrement counter
/// BCC.B  skip                      ; if no underflow, hold
/// MOVE.B chan[+0x2A], chan[+0x2B]  ; reset counter
/// MOVEA.L chan[+0x26], A2          ; fetch advance pointer
/// MOVE.B (A2)+, D1                 ; consume next byte
/// BMI.B  sustain                   ; if byte >= 0x80, freeze
/// MOVE.L A2, chan[+0x26]           ;   else advance
/// ANDI.W #0x7F, D1                 ; mask sustain bit
/// ; Paula AUD0VOL ← (D1 * global_vol) >> 6
/// ```
///
/// so step `k` covers frames `k*(step_interval+1) ..
/// (k+1)*(step_interval+1) - 1` from the note trigger, with the
/// last step's value held forever (sustain bit). Mapping that to
/// row resolution: `step = ((row - trigger_row) * speed) /
/// (step_interval + 1)`. The function compares each row's value
/// against the previous emitted volume and only writes a ghost
/// cell when the value actually changes — keeps the cell count
/// down and matches the xmrs runtime semantic (last
/// `TrackEffect::Volume` persists on the channel until
/// overwritten).
fn attach_envelope_animation(
    rows: &mut [Cell],
    notes: &[(usize, i16, Option<u8>, Option<u16>)],
    seg_start: usize,
    speed: u32,
    envelopes: &[super::header::DwVolumeEnvelope],
    master_volume: Option<u16>,
) {
    // Empire-family global master scale (`× master / 64`); identity
    // elsewhere. Mirrors `DwModule::scale_master_volume` so the
    // envelope-animation ghost cells track the same Paula level the
    // replayer's per-tick `MULU master ; LSR #6` produces.
    let scale = |v: u8| -> u8 {
        match master_volume {
            Some(master) if master != 64 => (((v as u32) * (master as u32)) >> 6).min(64) as u8,
            _ => v,
        }
    };
    if notes.is_empty() || rows.is_empty() {
        return;
    }
    let seg_end = seg_start + rows.len() - 1;
    let speed = speed.max(1);
    for (i, &(note_abs_row, _, _peak, env_idx)) in notes.iter().enumerate() {
        let Some(env_idx) = env_idx else { continue };
        let Some(env) = envelopes.get(env_idx as usize) else {
            continue;
        };
        if env.steps.is_empty() {
            continue;
        }
        // Compute the note's lifetime within this segment:
        // [note_abs_row, next_note_abs_row) intersected with the
        // segment's row span. The last note in a segment runs to
        // `seg_end + 1` so the sustain volume reaches the
        // segment's final row.
        let next_abs_row = notes
            .get(i + 1)
            .map(|&(r, _, _, _)| r)
            .unwrap_or(seg_end + 1);
        if note_abs_row > seg_end {
            continue;
        }
        let lifetime_end = next_abs_row.min(seg_end + 1);
        // The trigger row's volume is already set by the NoteOn
        // cell to the envelope's first step (`initial()`, matching
        // the runtime); the animation writes ghost cells starting
        // at row `note_abs_row + 1`. Track `prev_paula` against
        // that same first step (`step_for_row(note_abs_row)` =
        // `steps[0]`) so the first ghost only fires once the
        // envelope actually advances past step 0 — the on-disk
        // semantic at trigger time is "Paula gets `env[0]`".
        let step_for_row = |r: usize| -> u8 {
            let frames = (r - note_abs_row) as u32 * speed;
            let step_idx = (frames / (env.step_interval as u32 + 1)) as usize;
            env.steps[step_idx.min(env.steps.len() - 1)]
        };
        let mut prev_paula: u8 = step_for_row(note_abs_row);
        for r in (note_abs_row + 1)..lifetime_end {
            let paula = step_for_row(r);
            if paula == prev_paula {
                continue;
            }
            prev_paula = paula;
            let rel = r - seg_start;
            if rel >= rows.len() {
                break;
            }
            // Only overwrite empty cells — if a later note
            // already placed a NoteOn at this row, that trigger
            // wins (the envelope for the new note will rearm
            // from scratch).
            if !matches!(rows[rel].event, CellEvent::None) {
                continue;
            }
            let velocity = Volume::from_byte_64(scale(paula.min(64)));
            rows[rel] = Cell {
                event: CellEvent::None,
                effects: vec![TrackEffect::Volume {
                    value: velocity,
                    tick: 0,
                }],
                expression: crate::core::cell::NoteExpression::NEUTRAL,
            };
        }
    }
}

/// A `(channel, sample)` segment produced by [`build_segments`].
/// Each segment becomes one xmrs `Track::Notes` + one `Clip`.
#[derive(Debug)]
struct ChannelSegment {
    /// First absolute song row this segment covers (inclusive).
    start_row: usize,
    /// Last absolute song row this segment covers (inclusive).
    end_row: usize,
    /// Sample index every `NoteOn` in this segment uses — the base
    /// for the `Track::Notes::instrument` value (a per-`(sample,
    /// arpeggio)` synthetic instrument when [`Self::arp_index`] is
    /// set, else the plain per-sample instrument).
    sample_index: usize,
    /// Pitch arpeggio armed across this segment, when on the
    /// pitch-envelope path. `None` everywhere else (and on segments
    /// with no arpeggio). A change in this value opens a new segment
    /// so each run gets the right synthetic instrument.
    arp_index: Option<u16>,
    /// Volume envelope armed across this segment, when baking the
    /// volume envelope per-tick on the synthetic instrument
    /// (`split_by_env`). `None` everywhere else — there the volume is
    /// animated via row cells (`attach_envelope_animation`). A change
    /// opens a new segment (different instrument).
    env_index: Option<u16>,
    /// Absolute-row note events captured during the run. The
    /// builder converts them into relative `Cell` positions at
    /// the segment's `start_row`. Each tuple is
    /// `(absolute_row, effective_note, paula_volume_peak,
    /// envelope_index)` where `effective_note` already has
    /// global + per-channel transposes applied (so it can be
    /// `< 0` or `> 127` near the period-table edges; the pitch
    /// builder clamps). `paula_volume = None` means "no envelope
    /// armed at trigger time" — the Cell falls back to
    /// `Volume::FULL`. `envelope_index` (when `Some`) lets the
    /// cell builder walk the full per-tick step sequence and
    /// emit ghost `TrackEffect::Volume` cells between triggers.
    notes: Vec<(usize, i16, Option<u8>, Option<u16>)>,
}

/// Rows per synthesized pattern. xmrs's `Module::row_at` resolves
/// a cell via `track.rows[pattern_row - source_start_row]`, so each
/// clip must stay strictly inside one pattern — clips that span a
/// pattern boundary are silently skipped past the boundary. Setting
/// the value to a tracker-conventional 64 keeps the debug dump
/// readable while letting the splitter below fit every segment into
/// one pattern.
const ROWS_PER_PATTERN: u32 = 64;

/// Split each segment whose row range crosses a pattern boundary
/// into one segment per pattern it touches. Each output segment
/// fits entirely within one pattern, with its notes filtered to
/// that pattern's row range. The sub-segments inherit the parent's
/// `sample_index` so the dedup pass downstream can still fuse
/// identical contiguous chunks.
fn split_at_pattern_boundaries(segs: Vec<ChannelSegment>) -> Vec<ChannelSegment> {
    let rpp = ROWS_PER_PATTERN as usize;
    let mut out: Vec<ChannelSegment> = Vec::with_capacity(segs.len());
    for seg in segs {
        let mut start = seg.start_row;
        while start <= seg.end_row {
            let pat = start / rpp;
            let pat_last = ((pat + 1) * rpp).saturating_sub(1);
            let chunk_end = pat_last.min(seg.end_row);
            let notes_in_chunk: Vec<(usize, i16, Option<u8>, Option<u16>)> = seg
                .notes
                .iter()
                .filter(|(r, _, _, _)| *r >= start && *r <= chunk_end)
                .copied()
                .collect();
            out.push(ChannelSegment {
                start_row: start,
                end_row: chunk_end,
                sample_index: seg.sample_index,
                arp_index: seg.arp_index,
                env_index: seg.env_index,
                notes: notes_in_chunk,
            });
            start = chunk_end + 1;
        }
    }
    out
}

/// Walk the per-channel `(row, note, sample)` stream and split
/// it at every sample change. Returns a contiguous, non-overlapping
/// sequence of segments covering `0..total_rows`. An empty input
/// yields an empty result — channels with no `NoteOn` events
/// produce no segments (the caller can choose whether to emit an
/// empty stub or skip them entirely; we skip).
fn build_segments(notes: &[NoteRow], total_rows: usize, split_by_env: bool) -> Vec<ChannelSegment> {
    let mut out: Vec<ChannelSegment> = Vec::new();
    if notes.is_empty() || total_rows == 0 {
        return out;
    }
    // When `split_by_env`, the volume envelope is baked per-tick on a
    // synthetic instrument, so a change in the armed envelope must open
    // a new segment (different instrument). When false (every non-
    // jump-table module), the envelope is animated via row cells and is
    // NOT a segmentation key — `seg_env` stays `None` so segmentation is
    // byte-for-byte unchanged.
    let key_env = |e: Option<u16>| if split_by_env { e } else { None };
    let mut seg_start = 0usize;
    let mut seg_sample = notes[0].2;
    let mut seg_arp = notes[0].5;
    let mut seg_env = key_env(notes[0].4);
    let mut buf: Vec<(usize, i16, Option<u8>, Option<u16>)> = Vec::new();

    for (i, &(row, note_byte, sample, volume, env_idx, arp)) in notes.iter().enumerate() {
        if i > 0 && (sample != seg_sample || arp != seg_arp || key_env(env_idx) != seg_env) {
            // Close the running segment one row before the sample /
            // arpeggio / envelope change, then start a fresh one.
            let end_row = row.saturating_sub(1).max(seg_start);
            out.push(ChannelSegment {
                start_row: seg_start,
                end_row,
                sample_index: seg_sample,
                arp_index: seg_arp,
                env_index: seg_env,
                notes: core::mem::take(&mut buf),
            });
            seg_start = row;
            seg_sample = sample;
            seg_arp = arp;
            seg_env = key_env(env_idx);
        }
        buf.push((row, note_byte, volume, env_idx));
    }

    // Trailing segment runs to the end of the song.
    out.push(ChannelSegment {
        start_row: seg_start,
        end_row: total_rows.saturating_sub(1).max(seg_start),
        sample_index: seg_sample,
        arp_index: seg_arp,
        env_index: seg_env,
        notes: buf,
    });
    out
}

fn open_reader(source: &[u8], offset: usize) -> Result<BinReader<'_>, ImportError> {
    if offset >= source.len() {
        return Err(ImportError::OutOfRange("dw_table_offset"));
    }
    Ok(BinReader::new(&source[offset..]))
}

/// Read the `loop_start` field for sample `idx` from the on-disk
/// sample-info table. Each row is 12 bytes; `loop_start` is a
/// big-endian signed i32 at byte offset +4. Returns `None` for
/// negative sentinels or out-of-bounds rows.
/// Drop dead duplicate Lfo/Slide/Glide automation lanes — the RFC §1B
/// invariant that an importer emits at most one lane per
/// `(target, kind)`. The DW importer attaches vibrato/slide lanes
/// *per sub-song*, so a Track that plays in more than one sub-song
/// accumulates several lanes on the same `TrackPitch(track)` target.
/// The player only ever read the **first**: its `find_map` over
/// `lanes_for(target)` returns lane[0], and an Lfo/Slide/Glide lane
/// always yields `Some` state, so later same-kind lanes were
/// unreachable. Removing them is therefore bit-identical, and it
/// restores the single-lane invariant that lets the §1B summed fold
/// treat any *remaining* multiplicity as deliberate (authored)
/// modulator stacking rather than an import artifact.
///
/// `Points` lanes are deliberately left intact: the player **sums**
/// them (e.g. the DW per-frame arpeggio transpose in
/// `track_pitch_points_delta`), so they are not "dead duplicates".
fn collapse_dead_duplicate_modulation_lanes(module: &mut Module) {
    use crate::core::daw::automation::{AutomationTarget, LaneKind};
    let mut seen: Vec<(AutomationTarget, u8)> = Vec::new();
    module.automation.retain(|l| {
        let disc: u8 = match &l.kind {
            LaneKind::Points(_) => return true,
            LaneKind::Lfo { .. } => 1,
            LaneKind::Slide { .. } => 2,
            LaneKind::Glide { .. } => 3,
        };
        let key = (l.target, disc);
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
}

/// Intern a freshly-decoded PCM buffer into the shared pool (RFC §1A):
/// return an existing `Arc<[i8]>` when the exact same bytes were seen
/// before, otherwise store and return a new one. Bucketed by FNV-1a
/// with a byte-exact comparison inside the bucket, so the dedup is
/// content-correct and collision-proof — never merely hash-equal.
fn dedup_pcm(pool: &mut BTreeMap<u64, Vec<Arc<[i8]>>>, pcm: Vec<i8>) -> Arc<[i8]> {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in &pcm {
        h ^= b as u8 as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let bucket = pool.entry(h).or_default();
    if let Some(existing) = bucket.iter().find(|a| a.as_ref() == pcm.as_slice()) {
        return Arc::clone(existing);
    }
    let arc: Arc<[i8]> = Arc::from(pcm);
    bucket.push(Arc::clone(&arc));
    arc
}

fn read_loop_start(source: &[u8], base: usize, idx: usize, stride: usize) -> Option<u32> {
    let row = base.checked_add(idx.checked_mul(stride)?)?;
    let field = row.checked_add(4)?;
    let bytes: [u8; 4] = source.get(field..field + 4)?.try_into().ok()?;
    let v = i32::from_be_bytes(bytes);
    if v >= 0 {
        Some(v as u32)
    } else {
        None
    }
}

/// Promote a single Whittaker sample into an [`Instrument`] with
/// one [`Sample`] slot. The instrument is named after its source
/// index so editors can tell them apart at a glance.
///
/// `helper` is the [`PeriodHelper`] of the parent module — used
/// to convert the sample's native Hz into the
/// `(relative_pitch, finetune)` pair that xmrs's runtime
/// expects. Reusing the canonical helper instead of an ad-hoc
/// `f32::log2` keeps the conversion bit-identical to every other
/// importer in the crate.
fn sample_to_instrument(s: &DwSample) -> Instrument {
    let loop_flag = if s.is_looping() {
        LoopType::Forward
    } else {
        LoopType::No
    };
    // `relative_pitch = 0` for every variant: the cell pitch is
    // mapped *period-exactly* in `to_module::note_to_pitch` (the
    // Paula period — including the new player's
    // `× 0x369E99/freq >> 10` finetune — is reproduced and
    // converted straight to a pitch). The sample must therefore
    // play at the literal Amiga rate `3_546_894 / period` with no
    // extra shift; any non-zero `relative_pitch` would double-count
    // the header frequency and detune the result.
    let rp = 0;

    let sample = Sample {
        name: format!("dw#{:02}", s.index),
        relative_pitch: rp,
        finetune: Finetune::ZERO,
        volume: ChannelVolume::from_byte_64(s.volume.min(64) as u8),
        default_note_volume: Volume::FULL,
        panning: Panning::CENTER,
        loop_flag,
        loop_start: s.loop_start.unwrap_or(0),
        loop_length: s.loop_length().unwrap_or(0),
        sustain_loop_flag: LoopType::No,
        sustain_loop_start: 0,
        sustain_loop_length: 0,
        data: Some(SampleDataType::Mono8(s.pcm.clone())),
    };

    let mut instr = InstrDefault::default();
    instr.sample = vec![Some(sample)];
    // Without this the playback runtime gets `None` from
    // `keyboard.sample_for_pitch[pitch]` for every NoteOn and
    // silently drops the trigger — same behaviour the Amiga MOD
    // importer needs (see `amiga_module.rs`, the `map_all_to(0)`
    // call right after the sample is wired into the instrument).
    instr.keyboard.map_all_to(0);

    Instrument {
        name: format!("Whittaker sample {} ({} Hz)", s.index, s.frequency),
        instr_type: InstrumentType::Default(instr),
        ..Default::default()
    }
}

/// Build a looping **pitch envelope** that reproduces a David
/// Whittaker per-tick arpeggio (an offset stream read one entry
/// per frame, wrapping at its terminator). Here 1 DW frame = 1
/// player tick, and the player walks one envelope point per tick,
/// so the envelope *is* the arpeggio:
///
/// - `point[0]` = the note's plain pitch (offset 0): the replayer's
///   trigger frame applies no arpeggio, only the sustain frames do.
/// - `point[1..=N]` = the cycle, **looped forever** (`loop` over
///   `[1, N]`, skipping the one-shot trigger frame). The arpeggio
///   therefore runs for the note's whole duration — unlike
///   `TrackEffect::Arpeggio`, which only lives on the trigger row
///   and is capped at 3 steps / `0..15` semitones.
///
/// Offset `o` semitones is encoded exactly as
/// `EnvValue::from_signed_byte_64(2·o)` — the player's
/// `get_pitch_envelope_offset` maps an envelope byte back to
/// `byte/2` semitones — so the representable range is ±16
/// semitones; a wider step (e.g. the `+36` in one bubble-bobble
/// arpeggio) saturates there. Returns `None` for a trivial
/// (empty / all-zero) arpeggio, which keeps the plain per-sample
/// instrument.
fn arpeggio_to_pitch_envelope(arp: &DwArpeggio) -> Option<Envelope> {
    if arp.offsets.is_empty() || arp.offsets.iter().all(|&o| o == 0) {
        return None;
    }
    let enc = |o: i8| -> EnvValue {
        EnvValue::from_signed_byte_64(((o as i16) * 2).clamp(-32, 32) as i8)
    };
    let mut point = Vec::with_capacity(arp.offsets.len() + 1);
    point.push(EnvelopePoint {
        frame: 0,
        value: enc(0),
    });
    for (k, &o) in arp.offsets.iter().enumerate() {
        point.push(EnvelopePoint {
            frame: k + 1,
            value: enc(o),
        });
    }
    let last = point.len() - 1;
    // The cycle must run while the note is HELD. The player's
    // envelope tick uses the **sustain** loop while `sustained`
    // (and only falls back to the plain loop once released), so a
    // held DW note that never keys off would otherwise walk to the
    // last point and freeze there. Arm the sustain loop over the
    // cycle `[1, last]` (frame 0 = the one-shot trigger base);
    // mirror it on the plain loop so a released note keeps cycling
    // too.
    Some(Envelope {
        enabled: true,
        point,
        sustain_enabled: true,
        sustain_start_point: 1,
        sustain_end_point: last,
        loop_enabled: true,
        loop_start_point: 1,
        loop_end_point: last,
    })
}

/// Build a per-tick **volume envelope** reproducing a David Whittaker
/// volume envelope (`step_interval` frames between advances, then the
/// `steps` values, the last held as the sustain level). The replayer
/// advances the channel volume every `step_interval + 1` frames; the
/// row-grid `attach_envelope_animation` could only sample that on the
/// 3-frame row boundaries, aliasing the decay (skipping steps,
/// landing too loud — the audible "violent" volume). Here 1 frame =
/// 1 player tick, so a point per frame reproduces the staircase
/// exactly. A **sustain loop on the last point** holds the sustain
/// level while the note is held (without it, walking past the last
/// point arms the fadeout and the voice decays to silence).
///
/// `master` applies the empire-family `× master / 64` scale (identity
/// elsewhere). Returns `None` for an empty envelope (no shaping).
fn volume_to_volume_envelope(env: &DwVolumeEnvelope, master: Option<u16>) -> Option<Envelope> {
    if env.steps.is_empty() {
        return None;
    }
    let scale = |v: u8| -> u8 {
        match master {
            Some(m) if m != 64 => (((v as u32) * (m as u32)) >> 6).min(64) as u8,
            _ => v.min(64),
        }
    };
    let si = env.step_interval as usize + 1; // frames per step
                                             // One point per frame over the decay (cap defends against a
                                             // pathological step count); the tail holds via the sustain loop.
    let total = (env.steps.len() * si).clamp(1, 256);
    let mut point = Vec::with_capacity(total);
    for f in 0..total {
        let k = (f / si).min(env.steps.len() - 1);
        point.push(EnvelopePoint {
            frame: f,
            value: EnvValue::from_byte_64(scale(env.steps[k])),
        });
    }
    let last = point.len() - 1;
    Some(Envelope {
        enabled: true,
        point,
        // Hold the last (sustain) step while the note is held.
        sustain_enabled: true,
        sustain_start_point: last,
        sustain_end_point: last,
        loop_enabled: false,
        loop_start_point: 0,
        loop_end_point: 0,
    })
}

/// Resolve the `Track::Notes::instrument` index for a `(sample,
/// arpeggio, volume-envelope)` triple. Absent/trivial arpeggio AND
/// envelope reuse the plain per-sample instrument at `sample_index`.
/// Otherwise lazily build a synthetic instrument carrying the
/// arpeggio's **pitch** envelope and/or the DW **volume** envelope
/// (per-tick, frame-exact), caching one per distinct triple actually
/// used. When a volume envelope is attached the sample's static
/// volume is forced to full scale so the envelope alone shapes the
/// loudness (matching the replayer, whose channel volume *is* the
/// envelope byte).
#[allow(clippy::too_many_arguments)] // resolves one note's full (sample, arp, env, master) context
fn resolve_synth_instrument(
    instruments: &mut Vec<Instrument>,
    cache: &mut BTreeMap<(usize, Option<u16>, Option<u16>), usize>,
    samples: &[DwSample],
    arpeggios: &[DwArpeggio],
    volume_envelopes: &[DwVolumeEnvelope],
    master_volume: Option<u16>,
    sample_index: usize,
    arp_index: Option<u16>,
    env_index: Option<u16>,
) -> usize {
    let base = sample_index.min(samples.len().saturating_sub(1));
    let pitch_env = arp_index
        .and_then(|a| arpeggios.get(a as usize))
        .and_then(arpeggio_to_pitch_envelope);
    let vol_env = env_index
        .and_then(|e| volume_envelopes.get(e as usize))
        .and_then(|e| volume_to_volume_envelope(e, master_volume));
    if pitch_env.is_none() && vol_env.is_none() {
        return base;
    }
    let key = (
        base,
        arp_index.filter(|_| pitch_env.is_some()),
        env_index.filter(|_| vol_env.is_some()),
    );
    if let Some(&idx) = cache.get(&key) {
        return idx;
    }
    // `Instrument` isn't `Clone`, so rebuild the base from its sample.
    let Some(s) = samples.get(base) else {
        return base;
    };
    let mut instr = sample_to_instrument(s);
    if let InstrumentType::Default(ref mut d) = instr.instr_type {
        if let Some(pe) = pitch_env {
            d.voice.pitch_envelope = pe;
            d.voice.pitch_envelope_as_low_pass_filter = false;
        }
        if let Some(ve) = vol_env {
            d.voice.volume_envelope = ve;
            // The envelope carries the full Paula loudness, so the
            // sample's own static volume must not also scale it.
            if let Some(Some(smp)) = d.sample.get_mut(0) {
                smp.volume = ChannelVolume::from_byte_64(64);
            }
        }
    }
    // Append a readable tag for whichever modulation(s) this synthetic
    // instrument carries, e.g. "… +arp12", "… +env7", "… +arp12+env7".
    let mut tag = alloc::string::String::new();
    if let Some(a) = key.1 {
        tag.push_str(&format!(" +arp{}", a));
    }
    if let Some(e) = key.2 {
        tag.push_str(&format!(" +env{}", e));
    }
    instr.name = format!("{}{}", instr.name, tag);
    let idx = instruments.len();
    instruments.push(instr);
    cache.insert(key, idx);
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_garbage() {
        let buf = [0xFFu8; 256];
        assert!(matches!(
            DwModule::load(&buf),
            Err(ImportError::InvalidMagic("dw_detect"))
        ));
    }

    #[test]
    fn rejects_truncated() {
        assert!(DwModule::load(&[0u8; 16]).is_err());
    }
}
