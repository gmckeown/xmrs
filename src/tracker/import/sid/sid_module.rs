//! SID importer — Commodore 64 / MOS6581.
//!
//! SID has no pattern grid in its source data — just three voice
//! streams playing in parallel. The produced [`Module`] carries
//! `tracks` / `clips` / `timeline_map` and leaves `pattern` /
//! `pattern_order` empty.
//!
//! Pipeline per sub-song:
//!
//! 1. The voice grid (internal `get_voice_grid`) cycles each voice
//!    through its phrase ordering until every voice has completed
//!    at least one pass; returns a `(rows × 3 voices)` grid of
//!    [`PatternSlot`].
//! 2. Per voice: feed the 1-column slot stream to the internal
//!    `ImportMemory::unpack_patterns` as a 1-channel pattern. Each
//!    voice gets its own `ImportMemory` instance because SID voices
//!    have independent effect-memory state.
//! 3. Split the resolved row sequence with
//!    [`crate::tracker::import::build::split_rows_by_instrument`] →
//!    one [`Track`] + one [`Clip`] per instrument-coherent segment.
//! 4. [`crate::tracker::import::build::dedupe_tracks_by_content`] fuses
//!    bit-identical segments.
//! 5. `timeline_map`: filled directly (`tick = row * speed`). SID is
//!    linear — no jumps, loops, or breaks — so `walk_order` is
//!    unnecessary.

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
    pub fn to_modules(&self, original_instruments: bool) -> Vec<Module> {
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
            module.default_tempo = 1 + self.sid.resetspd;

            // `get_voice_grid` cycles each voice through its phrase
            // ordering until every voice has completed at least one
            // full pass, padding shorter voices by looping. After
            // that every voice has the same row count.
            let grid = self.voices.get_voice_grid(song_number);
            let num_voices = grid.first().map(|r| r.len()).unwrap_or(0);
            let song_rows = grid.len();

            // Per voice: resolve effect memory through a 1-channel
            // `ImportMemory` call, then split the resulting row
            // sequence into instrument-coherent Tracks that
            // play back-to-back on the same `target_channel`.
            let speed = module.default_tempo as u8;
            // First pass: build per-voice TIU streams (one ImportMemory
            // per voice — SID voices have independent effect-memory
            // state).
            let mut voices_tius: Vec<Vec<crate::tracker::import::unit::TrackImportUnit>> =
                Vec::with_capacity(num_voices);
            for voice_idx in 0..num_voices {
                let voice_pattern: Vec<Vec<PatternSlot>> =
                    grid.iter().map(|row| vec![row[voice_idx]]).collect();
                let mut im = ImportMemory::default();
                let unpacked = im.unpack_patterns(
                    FrequencyType::AmigaFrequencies,
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
                    let position_tick = seg_start_row * speed as u32;
                    clips.push(Clip {
                        track: track_idx,
                        song: 0,
                        target_channel: voice_idx as u8,
                        position_tick,
                        speed_at_start: speed,
                        track_row_offset: 0,
                        source_start_row: seg_start_row,
                        // SID is linear with constant speed.
                        end_tick: position_tick + seg_len * speed as u32,
                    });
                }
            }

            // Build `timeline_map` directly: SID songs are linear,
            // no jumps / loops / pattern breaks, so each row is at
            // `tick = row * speed`. No `walk_order` needed.
            let speed = module.default_tempo as u8;
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
                    tick: r as u32 * speed as u32,
                    speed_at_row: speed,
                    bpm_at_row: bpm,
                });
            }
            module.timeline_map = crate::core::daw::timeline::TimelineMap { entries };

            module.tracks = tracks;
            module.clips = crate::core::daw::sorted_clips::SortedClips::from_unsorted(clips);

            let idst = InstrHelper::irss_to_instruments(&self.instruments, original_instruments);
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
}

impl SidModule {
    pub fn get_sid_commando() -> Self {
        let sid = OneSid::get_sid_commando();
        sid.to_sidmodule()
    }

    pub fn get_sid_crazy_comets() -> Self {
        let sid = OneSid::get_sid_crazy_comets();
        sid.to_sidmodule()
    }

    pub fn get_sid_last_v8() -> Self {
        let sid = OneSid::get_sid_last_v8();
        sid.to_sidmodule()
    }

    pub fn get_sid_monty_on_the_run() -> Self {
        let sid = OneSid::get_sid_monty_on_the_run();
        sid.to_sidmodule()
    }

    pub fn get_sid_thing_on_a_spring() -> Self {
        let sid = OneSid::get_sid_thing_on_a_spring();
        sid.to_sidmodule()
    }

    pub fn get_sid_zoid() -> Self {
        let sid = OneSid::get_sid_zoid();
        sid.to_sidmodule()
    }

    //--------------------------
    // WIP

    pub fn get_sid_ace_2() -> Self {
        let sid = OneSid::get_sid_ace_2();
        sid.to_sidmodule()
    }

    pub fn get_sid_delta() -> Self {
        let sid = OneSid::get_sid_delta();
        sid.to_sidmodule()
    }

    pub fn get_sid_human_race() -> Self {
        let sid = OneSid::get_sid_human_race();
        sid.to_sidmodule()
    }

    pub fn get_sid_international_karate() -> Self {
        let sid = OneSid::get_sid_international_karate();
        sid.to_sidmodule()
    }

    pub fn get_sid_lightforce() -> Self {
        let sid = OneSid::get_sid_lightforce();
        sid.to_sidmodule()
    }

    pub fn get_sid_sanxion_song_1() -> Self {
        let sid = OneSid::get_sid_sanxion_song_1();
        sid.to_sidmodule()
    }

    pub fn get_sid_sanxion_song_2() -> Self {
        let sid = OneSid::get_sid_sanxion_song_2();
        sid.to_sidmodule()
    }

    pub fn get_sid_spellbound() -> Self {
        let sid = OneSid::get_sid_spellbound();
        sid.to_sidmodule()
    }

    pub fn get_sid_thrust() -> Self {
        let sid = OneSid::get_sid_thrust();
        sid.to_sidmodule()
    }
}
