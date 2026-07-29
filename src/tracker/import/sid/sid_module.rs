//! SID importer — Commodore 64 / MOS6581.
//!
//! SID has no pattern grid in its source data — just three voice
//! streams playing in parallel. The produced [`Module`] carries
//! `tracks` / `clips` / `timeline_map` and leaves `pattern` /
//! `pattern_order` empty.
//!
//! Pipeline per sub-song:
//!
//! 1. `Voices::voice_passes` decodes each voice's own single-pass row
//!    sequence (its ordering's phrases concatenated) — **no** common grid.
//!    Each voice loops independently via a [`crate::core::daw::loop_region::ChannelLoop`]
//!    `[0, pass_len·speed)`, exactly as the three SID voices drift on the
//!    real hardware. (This replaced an LCM-grid hack from before the DAW
//!    layer gained per-lane loops.)
//! 2. Per voice: feed the 1-column slot stream to the internal
//!    `ImportMemory::unpack_patterns` as a 1-channel pattern. Each
//!    voice gets its own `ImportMemory` instance because SID voices
//!    have independent effect-memory state.
//! 3. Split the resolved row sequence with
//!    [`crate::tracker::import::build::split_rows_by_instrument`] →
//!    one [`Track`] + one [`Clip`] per instrument-coherent segment.
//! 4. [`crate::tracker::import::build::dedupe_tracks_by_content`] fuses
//!    bit-identical segments.
//! 5. `timeline_map`: filled directly (`tick = row * speed`) over the longest
//!    voice's pass; the per-lane free-run folds the shorter voices. SID is
//!    linear — no jumps or breaks — so `walk_order` is unnecessary.

use alloc::format;
use alloc::{vec, vec::Vec};

use crate::prelude::*;
use crate::tracker::import::memory::{ImportMemory, MemoryType};
use crate::tracker::import::patternslot::PatternSlot;

use super::instr_helper::InstrHelper;
use super::one_sid::OneSid;
use super::sound_fx::SoundFx;
use super::voices::Voices;

#[derive(Debug)]
pub struct SidModule {
    pub sid: OneSid,
    pub voices: Voices,
    pub instruments: Vec<InstrRobSid>,
    pub soundfx: Vec<SoundFx>,
}

impl SidModule {
    pub fn to_modules(&self) -> Vec<Module> {
        let mut modules: Vec<Module> = vec![];

        for song_number in 0..self.voices.songs.len() {
            let mut module = Module::default();
            module.name = format!("{} {}", self.sid.name, song_number);
            module.comment = format!(
                "{} - {} (song #{})",
                self.sid.copyright, self.sid.author, song_number
            );
            // SID conversion targets MOD-style memory / Amiga frequencies.
            module.quirks = crate::tracker::profiles::pt();
            module.origin = Some(crate::tracker::format::ModuleFormat::Sid);
            // Per-sub-song ticks-per-row: some tunes (commando = [2,3,2]) set a
            // different speed per song in their init routine; fall back to the
            // scalar `resetspd` when no per-song table is given.
            module.default_tempo = 1 + self.sid.resetspd_for(song_number);
            // Some later Rob-Hubbard replayers (International Karate, Thrust)
            // open `play` with a periodic frame-skip — `DEC cnt; BPL ok;
            // LDA #N; STA cnt; RTS` — that returns early (does NOTHING) once
            // every `N+1` frames, so the whole tune (rows AND per-frame
            // effects) advances at only `N/(N+1)` of the 50 Hz rate. This is a
            // deliberate FRACTIONAL-TEMPO device (a pure 50 Hz player can only
            // hit tempos of 50/integer). Our player has no per-frame skip, so we
            // reproduce the *average* slowdown by scaling the baked bpm by
            // `N/(N+1)` (non-core: just the existing `default_bpm` field). 0 =
            // no skip (every other tune; bpm stays the 125 default → unchanged).
            if self.sid.frame_skip_reset > 0 {
                let n = self.sid.frame_skip_reset;
                module.default_bpm = (module.default_bpm * n) / (n + 1);
            }

            // Each voice is its OWN single-pass sequence (its ordering's
            // phrases concatenated). Voices are **not** cycled into a common
            // grid; each loops independently via a `ChannelLoop` below, exactly
            // as the three SID voices run on the hardware with no global
            // realignment. This replaces the old `get_voice_grid` LCM hack
            // (which materialised `lcm(voice lengths)` rows — thousands per
            // song — to fit a single pattern-style grid, needed before the DAW
            // layer had per-lane loops).
            // Per-instrument short-gate flag (v30 fxmask bit0): drum-flagged
            // instruments use a 1-row gate (see `Voices::decode_phrase`). Read
            // from the raw instrument bytes — the v30 importer deliberately does
            // not model the drum effect into `RobEffects`, so `fx[0].drum` is
            // always false here. All-false for non-v30 tunes (no behaviour
            // change). Recomputed per song but cheap (≤ instr_qty bytes).
            let short_gate_instrs = self.sid.short_gate_instrs();
            let passes = self.voices.voice_passes(song_number, &short_gate_instrs);
            let num_voices = passes.len();
            // Timeline span = the longest voice's pass; shorter voices fold
            // continuously across it (the per-lane free-run never snaps them).
            let song_rows = passes.iter().map(|p| p.len()).max().unwrap_or(0);

            // Per voice: resolve effect memory through a 1-channel
            // `ImportMemory` call, then split the resulting row
            // sequence into instrument-coherent Tracks that
            // play back-to-back on the same `target_channel`.
            let speed = module.default_tempo as u8;
            // Tempo cadence: uniform `speed` frames per row by default. When the
            // tune carries a *fractional* tempo (`tempo_frac`, a dual-counter
            // replayer like ace_2), bake a non-uniform per-row tick cadence
            // `tick(r) = ⌊r·num/den⌋` so the average frames/row is exactly
            // `num/den` while the SID chip still advances one frame per tick
            // (50 Hz) — the per-frame filter sweep / vibrato stay at hardware
            // rate, only the note-stream advance slows to the true rate. With
            // `tempo_frac = None`, `tick_at(r) == r·speed` and `speed_at(r) ==
            // speed`, i.e. the path is byte-identical to the uniform code.
            // `tick(r) = ⌈r·num/den⌉` (round UP): this reproduces the real
            // dual-counter's exact note-step frame phase. ace_2's counter fires
            // on cumulative frames 0,3,6,8,11,14,16,… (gaps 3,3,2) — which is
            // `⌈r·8/3⌉`, not `⌊r·8/3⌋` (= 0,2,5,8,… gaps 2,3,3, a 1-frame phase
            // error that smears the per-note loudness envelope). `tick_at(L)`
            // stays exact when `den | L` (ace_2: 5880 → 15680).
            let cadence = self.sid.tempo_frac_for(song_number);
            let tick_at = move |r: u32| -> u32 {
                match cadence {
                    Some((num, den)) => (r * num).div_ceil(den),
                    None => r * speed as u32,
                }
            };
            // Independent of `tick_at` (so both stay usable): the per-row gap.
            let speed_at = move |r: u32| -> u8 {
                match cadence {
                    Some((num, den)) => {
                        (((r + 1) * num).div_ceil(den) - (r * num).div_ceil(den)) as u8
                    }
                    None => speed,
                }
            };
            // First pass: build per-voice TIU streams (one ImportMemory
            // per voice — SID voices have independent effect-memory
            // state).
            let mut voices_tius: Vec<Vec<crate::tracker::import::unit::TrackImportUnit>> =
                Vec::with_capacity(num_voices);
            for pass in passes.iter().take(num_voices) {
                let voice_pattern: Vec<Vec<PatternSlot>> = pass.iter().map(|s| vec![*s]).collect();
                let mut im = ImportMemory::default();
                let unpacked = im.unpack_patterns(
                    // The SID is a LINEAR-frequency chip (`freq_reg ∝ Hz`), and
                    // `Module::default` plays in `LinearFrequencies`. Resolving
                    // the import in Amiga (period ∝ 1/Hz) space here was a
                    // mismatch that shifted low notes by an octave (the freq
                    // path round-tripped through the wrong space). Keep it
                    // linear end-to-end.
                    FrequencyType::LinearFrequencies,
                    MemoryType::Mod,
                    &[vec![0]],
                    &[voice_pattern],
                );
                let voice_rows: Vec<crate::tracker::import::unit::TrackImportUnit> = unpacked
                    .first()
                    .map(|p| p.iter().map(|r| r[0].clone()).collect())
                    .unwrap_or_default();
                voices_tius.push(voice_rows);
            }

            // Per-voice independent loop: each voice repeats its own pass
            // `[0, pass_len·speed)` forever, drifting against the others (the
            // player free-runs the timeline and folds each lane by its region).
            for (voice_idx, vrows) in voices_tius.iter().enumerate() {
                let len = vrows.len() as u32;
                if len > 0 {
                    module
                        .channel_loops
                        .push(crate::core::daw::loop_region::ChannelLoop {
                            song: 0,
                            channel: voice_idx as u8,
                            start_tick: 0,
                            end_tick: tick_at(len),
                        });
                }
            }

            // Second pass: split each voice into instrument-coherent
            // segments → one Track + one Clip per segment.
            let mut tracks: Vec<Track> = Vec::new();
            let mut clips: Vec<Clip> = Vec::new();
            for (voice_idx, voice_rows) in voices_tius.iter().enumerate() {
                let segments = crate::tracker::import::build::split_rows_by_instrument(voice_rows);
                for seg in segments {
                    let track_idx = tracks.len() as u32;
                    let seg_start_row = seg.start_row;
                    let materialised: Vec<Cell> =
                        seg.rows.into_iter().map(|tiu| tiu.prepare_cell()).collect();
                    let seg_len = materialised.len() as u32;
                    tracks.push(Track::Notes {
                        name: format!("voice {} seg {}", voice_idx, track_idx),
                        instrument: seg.instrument,
                        rows: materialised,
                        muted: false,
                    });
                    let position_tick = tick_at(seg_start_row);
                    clips.push(Clip {
                        track: track_idx,
                        song: 0,
                        target_channel: voice_idx as u8,
                        position_tick,
                        speed_at_start: speed_at(seg_start_row),
                        track_row_offset: 0,
                        source_start_row: seg_start_row,
                        // SID is linear; tick positions follow the (possibly
                        // fractional) tempo cadence via `tick_at`.
                        end_tick: tick_at(seg_start_row + seg_len),
                    });
                }
            }

            // Build `timeline_map` directly: SID songs are linear,
            // no jumps / loops / pattern breaks, so each row is at
            // `tick = tick_at(row)` (uniform `row·speed` unless the tune
            // has a fractional cadence). No `walk_order` needed.
            let bpm = module.default_bpm as u16;
            let mut entries: Vec<crate::core::daw::timeline::TimelineEntry> =
                Vec::with_capacity(song_rows);
            for r in 0..song_rows {
                entries.push(crate::core::daw::timeline::TimelineEntry {
                    song: 0,
                    order_idx: 0,
                    pattern_idx: 0, // synthetic single-pattern coordinate
                    row_idx: r as u32,
                    loop_iter: 0,
                    tick: tick_at(r as u32),
                    speed_at_row: speed_at(r as u32),
                    bpm_at_row: bpm,
                });
            }
            module.timeline_map = crate::core::daw::timeline::TimelineMap { entries };

            module.tracks = tracks;
            module.clips = crate::core::daw::sorted_clips::SortedClips::from_unsorted(clips);

            let idst = InstrHelper::irss_to_instruments(&self.instruments);
            module.instrument = idst;

            // Apply the standard content-dedup pass: two SID voices
            // that share an identical instrument-coherent segment
            // (e.g. a melody played in unison) fuse into a single
            // shared Track.
            crate::tracker::import::build::dedupe_tracks_by_content(&mut module);

            // DAW migration Phase 3c.3 — feed the per-Track lane
            // extractor through a synthetic pattern transposed from
            // the per-voice TIU streams. `extract_per_track_lanes_from_patterns`
            // walks `timeline_map` and resolves each visit through
            // `clips.active_at(song, channel, tick)`, which after
            // dedup points at the surviving representative Track.
            // SID carries no song-level globals.
            let max_rows = voices_tius.iter().map(|v| v.len()).max().unwrap_or(0);
            let synthetic_pattern: crate::tracker::import::build::Pattern = (0..max_rows)
                .map(|r| {
                    voices_tius
                        .iter()
                        .map(|voice| voice.get(r).cloned().unwrap_or_default())
                        .collect()
                })
                .collect();
            let lanes = crate::tracker::import::extract::extract_per_track_lanes_from_patterns(
                core::slice::from_ref(&synthetic_pattern),
                &module.timeline_map,
                &module.clips,
                &module.quirks,
            );
            module.automation.extend(lanes);

            modules.push(module);
        }

        modules
    }

    /// The same tune as ONE [`Module`] whose sub-songs are the tune's, indexed
    /// by the `song` field the DAW layer already carries everywhere.
    ///
    /// [`Self::to_modules`] hands back one `Module` per sub-song, which is the
    /// shape the oracle gates were written against and stays untouched. But it
    /// does not fit `Module::load`'s contract — one file, one module — which is
    /// why `.sid` files of this family could not be auto-detected and needed a
    /// dedicated CLI switch. This is the same content folded the way the DW
    /// importer folds its own multi-sub-song files: sub-song `N` keeps its own
    /// tick space, its own lanes and its own per-row cadence, and a player
    /// selects it with the usual sub-song index.
    ///
    /// Everything that names a track index — clips, automation lanes — is
    /// rebased as each sub-song's tracks are appended. `default_tempo` /
    /// `default_bpm` remain sub-song 0's, as they are for every other format;
    /// the per-sub-song cadence lives in `timeline_map`'s `speed_at_row`, which
    /// is where a player reads it.
    ///
    /// `None` when the tune decoded to no sub-song at all.
    pub fn to_module(&self) -> Option<Module> {
        let mut parts = self.to_modules();
        if parts.is_empty() {
            return None;
        }

        let mut merged = parts.remove(0);
        // `to_modules` numbers each part in its name ("Delta 0", "Delta 1")
        // because each was a whole module. One module, one title.
        merged.name = self.sid.name.into();
        merged.comment = format!("{} - {}", self.sid.copyright, self.sid.author);

        for (n, part) in parts.into_iter().enumerate() {
            // Every sub-song of one tune shares one instrument table — it is
            // rebuilt identically per part — so nothing is appended and nothing
            // is rebased.
            append_sub_songs(&mut merged, part, (n + 1) as u16, 0);
        }

        Some(merged)
    }
}

/// Number of sub-songs a module exposes: the highest `song` its timeline
/// mentions, plus one.
pub(crate) fn song_count(module: &Module) -> u16 {
    module
        .timeline_map
        .entries
        .iter()
        .map(|e| e.song)
        .max()
        .map_or(1, |m| m + 1)
}

/// Append `part`'s content to `merged` as further sub-songs.
///
/// Every index `part` carries internally is rebased as it moves: sub-song
/// numbers by `song_base`, track references by however many tracks `merged`
/// already holds, instrument references by `instr_base`. The caller owns the
/// instrument table itself — pass `instr_base = 0` when both sides already
/// share one (the sub-songs of a single tune do), or append `part.instrument`
/// and pass the old length when they do not (two replayers in one file).
///
/// Track indices appear in three places, and missing any one of them produces
/// a module that still loads and plays the wrong instrument: `Clip::track`,
/// the per-track [`AutomationTarget`] variants, and — for instruments —
/// `Track::Notes::instrument`.
pub(crate) fn append_sub_songs(
    merged: &mut Module,
    part: Module,
    song_base: u16,
    instr_base: usize,
) {
    use crate::core::daw::automation::AutomationTarget as T;
    use crate::tracker::import::build::EFFECT_ONLY_INSTRUMENT;

    let track_base = merged.tracks.len() as u32;

    merged.tracks.extend(part.tracks.into_iter().map(|mut t| {
        if let Track::Notes { instrument, .. } = &mut t {
            // The effect-only marker is `usize::MAX`, a sentinel and not an
            // index — shifting it would both overflow and lose its meaning.
            if *instrument != EFFECT_ONLY_INSTRUMENT {
                *instrument += instr_base;
            }
        }
        t
    }));

    let mut clips = merged.clips.snapshot();
    clips.extend(part.clips.snapshot().into_iter().map(|mut c| {
        c.track += track_base;
        c.song += song_base;
        c
    }));
    merged.clips = crate::core::daw::sorted_clips::SortedClips::from_unsorted(clips);

    merged
        .timeline_map
        .entries
        .extend(part.timeline_map.entries.into_iter().map(|mut e| {
            e.song += song_base;
            e
        }));

    merged
        .channel_loops
        .extend(part.channel_loops.into_iter().map(|mut l| {
            l.song += song_base;
            l
        }));

    merged
        .automation
        .extend(part.automation.into_iter().map(|mut lane| {
            lane.target = match lane.target {
                T::TrackVolume(i) => T::TrackVolume(i + track_base),
                T::TrackPanning(i) => T::TrackPanning(i + track_base),
                T::TrackPitch(i) => T::TrackPitch(i + track_base),
                T::TrackChannelVolume(i) => T::TrackChannelVolume(i + track_base),
                other => other,
            };
            lane.song += song_base;
            lane
        }));
}

/// Bundled Rob Hubbard tunes, each returned as a ready-to-play [`SidModule`].
/// Multi-part tunes expose their sub-songs through [`SidModule::to_modules`].
///
/// Behind the off-by-default `sid_songs` feature: these need the `.sid` images,
/// which are third-party binaries (see [`super::songs`]). The records they wrap
/// are always compiled — [`SidModule::for_image`] applies them to bytes the
/// caller supplies.
#[cfg(feature = "sid_songs")]
impl SidModule {
    /// Bundled "Commando" by Rob Hubbard.
    pub fn get_sid_commando() -> Self {
        let sid = OneSid::get_sid_commando();
        sid.to_sidmodule()
    }

    /// Bundled "Crazy Comets" by Rob Hubbard.
    pub fn get_sid_crazy_comets() -> Self {
        let sid = OneSid::get_sid_crazy_comets();
        sid.to_sidmodule()
    }

    /// Bundled "The Last V8" by Rob Hubbard.
    pub fn get_sid_last_v8() -> Self {
        let sid = OneSid::get_sid_last_v8();
        sid.to_sidmodule()
    }

    /// Bundled "Monty on the Run" by Rob Hubbard.
    pub fn get_sid_monty_on_the_run() -> Self {
        let sid = OneSid::get_sid_monty_on_the_run();
        sid.to_sidmodule()
    }

    /// Bundled "Thing on a Spring" by Rob Hubbard.
    pub fn get_sid_thing_on_a_spring() -> Self {
        let sid = OneSid::get_sid_thing_on_a_spring();
        sid.to_sidmodule()
    }

    /// Bundled "Zoid" by Rob Hubbard.
    pub fn get_sid_zoid() -> Self {
        let sid = OneSid::get_sid_zoid();
        sid.to_sidmodule()
    }

    /// Bundled "ACE II" by Rob Hubbard.
    pub fn get_sid_ace_2() -> Self {
        let sid = OneSid::get_sid_ace_2();
        sid.to_sidmodule()
    }

    /// Bundled "Delta" by Rob Hubbard (multi-part; see [`SidModule::to_modules`]).
    pub fn get_sid_delta() -> Self {
        let sid = OneSid::get_sid_delta();
        sid.to_sidmodule()
    }

    /// Bundled "The Human Race" by Rob Hubbard.
    pub fn get_sid_human_race() -> Self {
        let sid = OneSid::get_sid_human_race();
        sid.to_sidmodule()
    }

    /// Bundled "International Karate" by Rob Hubbard.
    pub fn get_sid_international_karate() -> Self {
        let sid = OneSid::get_sid_international_karate();
        sid.to_sidmodule()
    }

    /// Bundled "Lightforce" by Rob Hubbard.
    pub fn get_sid_lightforce() -> Self {
        let sid = OneSid::get_sid_lightforce();
        sid.to_sidmodule()
    }

    /// Bundled "Sanxion" (song 1) by Rob Hubbard.
    pub fn get_sid_sanxion_song_1() -> Self {
        let sid = OneSid::get_sid_sanxion_song_1();
        sid.to_sidmodule()
    }

    /// Bundled "Sanxion" (song 2) by Rob Hubbard.
    pub fn get_sid_sanxion_song_2() -> Self {
        let sid = OneSid::get_sid_sanxion_song_2();
        sid.to_sidmodule()
    }

    /// Bundled "Spellbound" by Rob Hubbard.
    pub fn get_sid_spellbound() -> Self {
        let sid = OneSid::get_sid_spellbound();
        sid.to_sidmodule()
    }

    /// Bundled "Thrust" by Rob Hubbard.
    pub fn get_sid_thrust() -> Self {
        let sid = OneSid::get_sid_thrust();
        sid.to_sidmodule()
    }
}

impl SidModule {
    /// The best reading of `image` this crate can give: its transcribed
    /// record(s) if it is one of the fifteen, detection otherwise.
    ///
    /// This is the entry point production code wants. Recognition is by
    /// fingerprint, not by file name, so a user's own copy of Commando gets
    /// Commando's hand-tuned tempo and effect settings — the thing detection
    /// cannot recover — without the crate having to ship the tune.
    ///
    /// The list has one entry per replayer in the file. Almost always one;
    /// Sanxion holds two, one per song, and returning both is what lets a
    /// caller reach the second, which a single detection pass over the image
    /// never could.
    ///
    /// Empty when the image is not of this player family at all — try
    /// [`super::g2::to_module`] for the last generation.
    pub fn for_image(image: impl Into<alloc::borrow::Cow<'static, [u8]>>) -> Vec<Self> {
        let image = image.into();
        let records = OneSid::records_for_image(image.clone());
        if !records.is_empty() {
            return records.iter().map(OneSid::to_sidmodule).collect();
        }
        OneSid::from_detected(image)
            .map(|r| vec![r.to_sidmodule()])
            .unwrap_or_default()
    }

    /// Load a v10..v30 tune that has no bundled record, straight from its image.
    ///
    /// `None` if the image is not of that player family — try
    /// [`super::g2::to_module`] for the last generation. Read
    /// [`OneSid::from_detected`] before trusting the result musically: the
    /// tables are recovered, the tempo and the effect semantics are not.
    pub fn from_detected(song: impl Into<alloc::borrow::Cow<'static, [u8]>>) -> Option<Self> {
        Some(OneSid::from_detected(song)?.to_sidmodule())
    }

    /// The same tune, decoded with the table layout recovered from its image
    /// rather than the hand-transcribed one. `None` if the image is not of the
    /// v10..v30 player family.
    ///
    /// Diagnostic — see [`OneSid::with_detected_layout`].
    pub fn with_detected_layout(&self) -> Option<Self> {
        Some(self.sid.with_detected_layout()?.to_sidmodule())
    }

    /// The same tune with **only** the note-stream payload width taken from the
    /// image — the tables and every behavioural setting stay as transcribed.
    ///
    /// Isolating this one change is the point: `version` selects the payload
    /// width *and* the release mode, vibrato model and drum semantics all at
    /// once, so flipping the version to test a decode hypothesis would confound
    /// four things. `None` if the image is not of this player family.
    pub fn with_detected_stream(&self) -> Option<Self> {
        let layout = crate::tracker::import::sid::detect::SidLayout::detect(&self.sid.song)?;
        let mut out = self.sid.to_sidmodule();
        out.voices = out.voices.with_stream(Some(layout.stream));
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DELTA: &[u8] = include_bytes!("songs/delta.sid");
    const SANXION: &[u8] = include_bytes!("songs/sanxion.sid");
    const COMMANDO: &[u8] = include_bytes!("songs/commando.sid");
    const LION_HEART: &[u8] = include_bytes!("songs/lion_heart.sid");

    /// Folding the sub-songs must not lose any of them, nor any of their rows.
    /// A sub-song that silently ends up empty still loads and still plays —
    /// it just plays nothing — so count the rows, not the songs.
    #[test]
    fn merging_sub_songs_keeps_every_row() {
        let sid = OneSid::delta().with_image(DELTA).to_sidmodule();
        let parts = sid.to_modules();
        let merged = sid.to_module().expect("delta merges");

        assert_eq!(song_count(&merged) as usize, parts.len());
        for (n, part) in parts.iter().enumerate() {
            let rows = merged
                .timeline_map
                .entries
                .iter()
                .filter(|e| e.song as usize == n)
                .count();
            assert_eq!(
                rows,
                part.timeline_map.entries.len(),
                "sub-song {n} lost rows in the merge"
            );
        }
        merged
            .verify_layers_consistent()
            .expect("merged module is consistent");
    }

    /// The cadence is per sub-song — Delta patches its second counter from a
    /// table — and the merged module has only ONE `default_tempo`. So the
    /// per-song cadence has to survive in the timeline, which is where a player
    /// reads it; if it did not, every sub-song would silently play at song 0's
    /// speed.
    #[test]
    fn merged_sub_songs_keep_their_own_cadence() {
        let sid = OneSid::delta().with_image(DELTA).to_sidmodule();
        let parts = sid.to_modules();
        let merged = sid.to_module().expect("delta merges");

        for (n, part) in parts.iter().enumerate() {
            let mine: Vec<u8> = merged
                .timeline_map
                .entries
                .iter()
                .filter(|e| e.song as usize == n)
                .map(|e| e.speed_at_row)
                .collect();
            let theirs: Vec<u8> = part
                .timeline_map
                .entries
                .iter()
                .map(|e| e.speed_at_row)
                .collect();
            assert_eq!(mine, theirs, "sub-song {n} lost its own row cadence");
        }
    }

    /// A user's own copy of a bundled tune must get the transcribed record —
    /// the hand-tuned tempo and effect settings — not a detected reading.
    /// Recognition is by fingerprint, and the whole separation of record from
    /// image rests on it working.
    #[test]
    fn a_file_off_disk_gets_its_transcribed_record() {
        let by_record = OneSid::commando().with_image(COMMANDO).to_sidmodule();
        let found = OneSid::records_for_image(COMMANDO);
        assert_eq!(found.len(), 1, "commando should match exactly one record");
        assert_eq!(found[0].name, by_record.sid.name);
        // `resetspd_songs` is transcribed, not detectable — if the record were
        // missed, this is the first thing that would go.
        assert_eq!(found[0].resetspd_for(1), by_record.sid.resetspd_for(1));
    }

    /// Sanxion holds two complete replayers, one per song. A single detection
    /// pass over the image only ever sees the first, which is why song 2 used
    /// to need its own CLI name. Both records claim the file, so loading it
    /// reaches both.
    #[test]
    fn two_replayers_in_one_file_both_load() {
        assert_eq!(OneSid::records_for_image(SANXION).len(), 2);

        let module = Module::load(SANXION).expect("sanxion loads");
        assert!(
            song_count(&module) >= 2,
            "both sanxion replayers should contribute sub-songs, got {}",
            song_count(&module)
        );
        module
            .verify_layers_consistent()
            .expect("merged sanxion is consistent");
    }

    /// Every generation this crate reads now goes through plain
    /// `Module::load` — the point of the exercise. A `.sid` is settled off its
    /// magic and never falls through to MOD, whose detection would otherwise
    /// happily claim one.
    #[test]
    fn module_load_handles_every_readable_generation() {
        for (name, image) in [
            ("commando (fx-mask, record)", COMMANDO),
            ("delta (fx-mask, multi sub-song)", DELTA),
            ("lion_heart (last generation)", LION_HEART),
        ] {
            let module = Module::load(image)
                .unwrap_or_else(|e| panic!("{name} should load through Module::load: {e:?}"));
            assert!(!module.tracks.is_empty(), "{name} loaded with no track");
        }
    }

    /// The three-marker generation loads too — it is the third one this crate
    /// reads, and the one that used to need its own explanation in the CLI.
    #[test]
    fn the_three_marker_generation_loads_as_well() {
        const OFF_THE_CUFF: &[u8] = include_bytes!("songs/off_the_cuff.sid");
        let module = Module::load(OFF_THE_CUFF).expect("off the cuff loads");
        assert!(!module.tracks.is_empty());
    }

    /// A `.sid` this crate cannot decode must FAIL, not come back as something
    /// else. MOD's detector "accepts almost anything", so before the magic was
    /// settled first a tune we could not read could come back as a plausible
    /// 15-sample Soundtracker module — noise with a track list.
    #[test]
    fn an_undecodable_sid_is_refused_rather_than_mis_parsed() {
        // A well-formed PSID header over a payload no replayer recognises.
        let mut junk = vec![0u8; 512];
        junk[..4].copy_from_slice(b"PSID");
        junk[4..6].copy_from_slice(&2u16.to_be_bytes()); // version
        assert!(
            Module::load(&junk).is_err(),
            "a PSID we cannot decode must be refused, not handed to another \
             format's detector"
        );
    }
}
