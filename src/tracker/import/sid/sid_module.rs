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
            let resetspd = self
                .sid
                .resetspd_songs
                .get(song_number)
                .copied()
                .unwrap_or(self.sid.resetspd);
            module.default_tempo = 1 + resetspd;
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
}

/// Bundled Rob Hubbard tunes, each returned as a ready-to-play [`SidModule`].
/// Multi-part tunes expose their sub-songs through [`SidModule::to_modules`].
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
