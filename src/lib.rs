#![forbid(unsafe_code)]
#![allow(dead_code)]

//! # xmrs — A Safe SoundTracker Library
//!
//! ## Architecture
//!
//! ```text
//! xmrs
//! ├── core/                        ← Modern DAW data model (format-agnostic)
//! │   ├── module::Module
//! │   │    ├── instrument: Vec<Instrument>
//! │   │    ├── tracks: Vec<Track> ──► Cell { event, effects: Vec<TrackEffect> }
//! │   │    ├── clips: SortedClips
//! │   │    ├── automation: Vec<AutomationLane>
//! │   │    ├── timeline_map: TimelineMap
//! │   │    └── quirks: PlaybackQuirks (default all-off)
//! │   ├── compatibility::{PlaybackQuirks, PanResetPolicy}
//! │   ├── daw/                     ← AutomationLane, Clip, Track, TimelineMap, Euclidean
//! │   └── fixed/                   ← Q-format primitives
//! │
//! └── tracker/                     ← Historical-format support
//!     ├── format::ModuleFormat     ← (gated on any `import_*`)
//!     ├── profiles::{ft2, it214, st3, pt}    ← preset quirks
//!     ├── period::FrequencyType
//!     ├── codepage                 ← CP437 / Latin-1
//!     ├── mix_plugin               ← IT OpenMPT extension (cfg import_it)
//!     ├── instr_{opl, sid, robsid} ← cfg-gated per importer
//!     └── import/                  ← XM / IT / S3M / MOD / SID readers
//!         ├── build/               ← Pattern → Tracks + Clips + TimelineMap
//!         └── extract/             ← TrackImportUnit → AutomationLane
//! ```
//!
//! ## Features
//!
//! Default `std + import` matches the desktop / server case. For a
//! clean no-tracker build:
//!
//! ```toml
//! [dependencies]
//! xmrs = { version = "0.13", default-features = false, features = ["std"] }
//! ```
//!
//! In this mode `Module` carries only the universal fields; no
//! `origin`, no `mix_plugins`, no `Opl/Sid/RobSid` instrument
//! variants. The historical presets in `tracker::profiles` are
//! absent — quirks are all-off by default.
//!
//! You can serialize your work using serde.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

/// Core data model — format-agnostic.
pub mod core;

/// Format-tied items: presets, importers, format-specific instruments.
/// Each sub-module is gated behind one or more `import_*` features.
pub mod tracker;

/// Edit command API (atomic, undoable mutations of `Module`)
pub mod edit;

/// The Xmrs Prelude
pub mod prelude;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(42, 42);
    }

    #[cfg(feature = "import_xm")]
    #[test]
    fn load_empty_xm() {
        let data = include_bytes!("../examples/empty.xm");
        let module = crate::core::module::Module::load_xm(data);
        assert!(
            module.is_ok(),
            "Failed to load empty.xm: {:?}",
            module.err()
        );
    }

    #[cfg(feature = "import_xm")]
    #[test]
    fn load_note_xm() {
        let data = include_bytes!("../examples/note.xm");
        let module = crate::core::module::Module::load_xm(data).expect("Failed to load note.xm");
        assert!(
            !module.instrument.is_empty(),
            "Module should have instruments"
        );
        assert!(
            !module.timeline_map.entries.is_empty(),
            "timeline_map should be populated"
        );
        assert!(!module.tracks.is_empty(), "tracks should be populated");
        module
            .verify_layers_consistent()
            .expect("DAW layer should be consistent after import");
    }

    #[cfg(feature = "import_sid")]
    #[test]
    fn load_sid_commando_has_consistent_layer() {
        let sid = crate::tracker::import::sid::sid_module::SidModule::get_sid_commando();
        let modules = sid.to_modules(false);
        assert!(
            !modules.is_empty(),
            "SID should produce at least one Module"
        );
        for (i, m) in modules.iter().enumerate() {
            assert!(
                !m.tracks.is_empty(),
                "SID module {} should have tracks after build_timeline_layer",
                i
            );
            m.verify_layers_consistent()
                .unwrap_or_else(|e| panic!("SID module {} inconsistent: {:?}", i, e));
        }
    }

    #[cfg(feature = "import_xm")]
    #[test]
    fn load_empty_xm_has_consistent_layer() {
        let data = include_bytes!("../examples/empty.xm");
        let module = crate::core::module::Module::load_xm(data).expect("load");
        module
            .verify_layers_consistent()
            .expect("DAW layer should be consistent on empty.xm");
    }

    #[cfg(feature = "import_xm")]
    #[test]
    fn load_xi_instrument() {
        let data = include_bytes!("../examples/instr.xi");
        let xmi = crate::tracker::import::xm::xi_instrument::XiInstrument::load(data);
        assert!(xmi.is_ok(), "Failed to load instr.xi: {:?}", xmi.err());
    }

    #[cfg(feature = "import_xm")]
    #[test]
    fn autodetect_xm() {
        let data = include_bytes!("../examples/note.xm");
        let module = crate::core::module::Module::load(data);
        assert!(module.is_ok(), "Autodetect should find XM format");
    }

    /// Verify that `Module::load` autodetects a `.dw` payload — the
    /// integration path xmrsplayer hits when a user passes a file
    /// path to the player binary. Skipped silently when the
    /// developer's reference file isn't present.
    #[cfg(all(feature = "import_dw", feature = "std"))]
    #[test]
    fn autodetect_dw_xenon2() {
        use crate::tracker::format::ModuleFormat;
        let path = "/home/user/Music/dw/xenon2 (title).dw";
        let Ok(data) = std::fs::read(path) else {
            eprintln!("skipped: {} not found", path);
            return;
        };
        let module = crate::core::module::Module::load(&data)
            .expect("autodetect should accept the dw payload");
        assert_eq!(
            module.origin,
            Some(ModuleFormat::Dw),
            "autodetect must tag as Dw"
        );
        assert!(
            !module.tracks.is_empty(),
            "autodetected DW module should carry tracks"
        );
        module
            .verify_layers_consistent()
            .expect("autodetected DW DAW layer must be consistent");
    }

    /// Cross-module regression: loop over every `.dw` in the
    /// developer's local rip directory and assert end-to-end
    /// loading for the **targeted** files (the ones the importer
    /// currently claims support for — xenon2, beast1.*, qball,
    /// speedball). The remaining corpus is exercised in
    /// best-effort mode: failures are printed as a summary but do
    /// not panic, so the test stays a useful regression gate
    /// without blocking on the work that's still ahead.
    ///
    /// Skipped silently when the directory isn't present (CI,
    /// fresh checkout).
    #[cfg(all(feature = "import_dw", feature = "std"))]
    #[test]
    fn load_all_dw_modules_from_music_dir() {
        use crate::tracker::format::ModuleFormat;
        use crate::tracker::import::dw::dw_module::DwModule;
        let dir = "/home/user/Music/dw";
        let Ok(entries) = std::fs::read_dir(dir) else {
            eprintln!("skipped: {} not found", dir);
            return;
        };

        let mut paths: alloc::vec::Vec<std::path::PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("dw"))
            .collect();
        paths.sort();

        // Every `.dw` that decodes to a song must load fully. The
        // one acceptable non-load is the explicit `dw_no_song`
        // rejection — sound-effect / placeholder banks
        // (`*-sfx.dw`, `* fake.dw`) carry no song timeline.
        let mut failures: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
        let mut total = 0usize;
        let mut ok_count = 0usize;
        let mut skipped = 0usize;
        for path in &paths {
            total += 1;
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("<?>");

            let data = match std::fs::read(path) {
                Ok(d) => d,
                Err(e) => {
                    failures.push(alloc::format!("{}: read failed: {}", name, e));
                    continue;
                }
            };
            // 1. DwModule must build. A clean `dw_no_song`
            //    rejection is acceptable (sfx / placeholder bank).
            let dw = match DwModule::load(&data) {
                Ok(m) => m,
                Err(crate::tracker::import::bin_reader::ImportError::InvalidMagic(
                    "dw_no_song",
                )) => {
                    skipped += 1;
                    eprintln!("  -- {} : no song content (sfx/placeholder bank)", name);
                    continue;
                }
                Err(e) => {
                    failures.push(alloc::format!("{}: DwModule::load: {:?}", name, e));
                    continue;
                }
            };
            // 2. A loaded module must carry samples. A song is
            //    *not* required: SFX sample banks (xenon-sfx,
            //    leviathan-sfx) legitimately ship a sample bank
            //    with no sub-song / tracks, and we still want their
            //    PCM. Files with neither samples nor a song are
            //    rejected upstream as `dw_no_song`.
            if dw.samples.is_empty() {
                failures.push(alloc::format!("{}: zero samples parsed", name));
                continue;
            }
            // 3. `to_module()` builds a consistent DAW layer.
            let module = dw.to_module();
            if module.instrument.is_empty() {
                failures.push(alloc::format!("{}: empty instrument bank", name));
                continue;
            }
            if let Err(e) = module.verify_layers_consistent() {
                failures.push(alloc::format!("{}: DAW layer inconsistent: {:?}", name, e));
                continue;
            }
            // 4. `Module::load` autodetect must tag the file.
            match crate::core::module::Module::load(&data) {
                Ok(auto) => {
                    if auto.origin != Some(ModuleFormat::Dw) {
                        failures.push(alloc::format!(
                            "{}: autodetect produced origin {:?}, expected Dw",
                            name,
                            auto.origin
                        ));
                    }
                }
                Err(e) => failures.push(alloc::format!("{}: autodetect failed: {:?}", name, e)),
            }
            ok_count += 1;
        }
        eprintln!(
            "load_all_dw_modules_from_music_dir: {} files, {} ok, {} sfx/placeholder, {} failures",
            total,
            ok_count,
            skipped,
            failures.len()
        );
        if !failures.is_empty() {
            for f in &failures {
                eprintln!("  FAIL {}", f);
            }
            panic!("{} module(s) failed to load cleanly", failures.len());
        }
    }

    /// Integration test for the David Whittaker `.dw` importer.
    ///
    /// Looks for a real-world module on the developer's machine
    /// (the Xenon 2 title rip, ~360 KB). Skipped silently when the
    /// file is not present — `cargo test` on a fresh checkout / in
    /// CI just prints a `skipped:` note and passes.
    ///
    /// Replace the path with any `.dw` from Modland (`David
    /// Whittaker/dw.*`) to test against a different module.
    #[cfg(all(feature = "import_dw", feature = "std"))]
    #[test]
    fn load_dw_xenon2_from_home() {
        use crate::tracker::import::dw::dw_module::DwModule;
        let path = "/home/user/Music/dw/xenon2 (title).dw";
        let Ok(data) = std::fs::read(path) else {
            eprintln!("skipped: {} not found", path);
            return;
        };

        let dw = match DwModule::load(&data) {
            Ok(m) => m,
            Err(e) => panic!("DwModule::load({}) failed: {:?}", path, e),
        };

        // First slice: detection + structural fields must be sound,
        // sample-table extraction is best-effort and may yield zero
        // until the full probe catalogue lands. The test asserts the
        // pipeline doesn't crash and the Module wrapper is
        // well-formed; it accepts an empty sample bank.
        let pcm_total: usize = dw.samples.iter().map(|s| s.pcm.len()).sum();
        let looping = dw.samples.iter().filter(|s| s.is_looping()).count();
        eprintln!(
            "dw.xenon 2: variant {:?}, period table {:?}, {} samples ({} looping), {} PCM bytes",
            dw.variant,
            dw.period_table,
            dw.samples.len(),
            looping,
            pcm_total,
        );
        for s in &dw.samples {
            if let Some(start) = s.loop_start {
                eprintln!(
                    "  sample {:02}: loop_start={} len={} freq={}Hz",
                    s.index, start, s.length, s.frequency
                );
            }
        }
        if let Some(ss) = &dw.sub_song {
            eprintln!(
                "  sub-song: speed={}, delay_speed={}, channel offsets={:04x?}",
                ss.speed, ss.delay_speed, ss.channel_position_offsets,
            );
        }
        for (ch, list) in dw.position_lists.iter().enumerate() {
            eprintln!(
                "  position list ch{}: {} entries{}",
                ch,
                list.len(),
                match list.loop_to {
                    Some(i) => alloc::format!(", loop_to={}", i),
                    None => alloc::string::String::new(),
                }
            );
        }
        let total_track_bytes: usize = dw.tracks.iter().map(|t| t.bytes.len()).sum();
        let total_notes: usize = dw.tracks.iter().map(|t| t.note_count()).sum();
        let total_cmds: usize = dw.tracks.iter().map(|t| t.command_count()).sum();
        eprintln!(
            "  tracks: {} unique, {} bytes total, {} notes / {} commands",
            dw.tracks.len(),
            total_track_bytes,
            total_notes,
            total_cmds,
        );
        if let (Some(shortest), Some(longest)) = (
            dw.tracks.iter().map(|t| t.bytes.len()).min(),
            dw.tracks.iter().map(|t| t.bytes.len()).max(),
        ) {
            eprintln!("  track length range: {}..={} bytes", shortest, longest);
        }

        // Event-level breakdown — by family so we can see which
        // command codes Whittaker actually used in the title music.
        use crate::tracker::import::dw::event::DwTrackEvent;
        let mut notes = 0usize;
        let mut long_waits = 0usize;
        let mut set_samples = 0usize;
        let mut slides = 0usize;
        let mut vibratos = 0usize;
        let mut speeds = 0usize;
        let mut transposes = 0usize;
        let mut effect8 = 0usize;
        let mut effect9 = 0usize;
        let mut waits_until_next = 0usize;
        let mut other_cmds = 0usize;
        let mut set_arpeggios = 0usize;
        let mut set_envelopes = 0usize;
        for t in &dw.tracks {
            for ev in &t.events {
                match ev {
                    DwTrackEvent::Note(_) => notes += 1,
                    DwTrackEvent::LongWait(_) => long_waits += 1,
                    DwTrackEvent::SetSample(_) => set_samples += 1,
                    DwTrackEvent::Slide { .. } => slides += 1,
                    DwTrackEvent::StartVibrato { .. } | DwTrackEvent::StopVibrato => vibratos += 1,
                    DwTrackEvent::SetSpeed(_) => speeds += 1,
                    DwTrackEvent::GlobalTranspose(_) => transposes += 1,
                    DwTrackEvent::Effect8(_) => effect8 += 1,
                    DwTrackEvent::Effect9(_) => effect9 += 1,
                    DwTrackEvent::WaitUntilNextRow => waits_until_next += 1,
                    DwTrackEvent::EndOfTrack => {}
                    DwTrackEvent::SetVolumeEnvelope(_) => set_envelopes += 1,
                    DwTrackEvent::SetPitchArpeggio(_) => set_arpeggios += 1,
                    _ => other_cmds += 1,
                }
            }
        }
        eprintln!(
            "  events: notes={} long_wait={} set_sample={} set_arp={} set_env={} slide={} vibrato={} speed={} transpose={} fx8={} fx9={} wait_row={} other={}",
            notes,
            long_waits,
            set_samples,
            set_arpeggios,
            set_envelopes,
            slides,
            vibratos,
            speeds,
            transposes,
            effect8,
            effect9,
            waits_until_next,
            other_cmds,
        );

        // Feature flags can't be read off DwModule directly (they
        // live on DwLayout), so re-run detection just for the
        // diagnostic. Cheap — pure byte scans.
        let layout = crate::tracker::import::dw::detect::detect(&data).expect("re-detect");
        let feat = layout.features;
        eprintln!(
            "  features: delay_counter={} delay_multiply={} square_waveform={} sample_transpose={} arpeggio={} envelopes={}",
            feat.enable_delay_counter,
            feat.enable_delay_multiply,
            feat.enable_square_waveform,
            feat.enable_sample_transpose,
            feat.enable_arpeggio,
            feat.enable_envelopes,
        );
        eprintln!(
            "            vibrato={} half_volume={} volume_fade={} channel_transpose={}",
            feat.enable_vibrato,
            feat.enable_half_volume,
            feat.enable_volume_fade,
            feat.enable_channel_transpose,
        );

        // Run the full tick simulation and report shape stats.
        use crate::tracker::import::dw::runtime::{Simulator, TickEvent};
        let mut sim = Simulator::new(&dw);
        let trace = sim.run();
        let halted = sim.halted;
        let final_frame = sim.frame;
        let mut note_count = [0usize; 4];
        let mut sample_set = [0usize; 4];
        let mut mute_count = [0usize; 4];
        let mut speed_changes = 0usize;
        let mut channel_finished = [false; 4];
        let mut song_end = false;
        let mut notes_with_envelope = 0usize;
        let mut notes_with_full_volume = 0usize;
        for (_, ev) in &trace {
            match ev {
                TickEvent::NoteOn {
                    channel, volume, ..
                } => {
                    note_count[*channel as usize] += 1;
                    if volume.is_some() {
                        notes_with_envelope += 1;
                    } else {
                        notes_with_full_volume += 1;
                    }
                }
                TickEvent::SampleSet { channel, .. } => sample_set[*channel as usize] += 1,
                TickEvent::Mute { channel } => mute_count[*channel as usize] += 1,
                TickEvent::SpeedChange { .. } => speed_changes += 1,
                TickEvent::ChannelFinished { channel } => {
                    channel_finished[*channel as usize] = true
                }
                TickEvent::SongEnd { .. } => song_end = true,
                TickEvent::VibratoStart { .. }
                | TickEvent::VibratoStop { .. }
                | TickEvent::SlideStart { .. }
                | TickEvent::SlideStop { .. }
                | TickEvent::Arpeggio { .. } => {}
            }
        }
        eprintln!(
            "  simulation: {} frames (~{}s @ 50Hz), halted={}, trace_events={}",
            final_frame,
            final_frame / 50,
            halted,
            trace.len(),
        );
        eprintln!(
            "    notes per channel: ch0={} ch1={} ch2={} ch3={}",
            note_count[0], note_count[1], note_count[2], note_count[3]
        );
        eprintln!(
            "    volume envelopes: {} parsed, {} notes shaped, {} unshaped",
            dw.volume_envelopes.len(),
            notes_with_envelope,
            notes_with_full_volume,
        );
        eprintln!(
            "    sample_sets per ch: ch0={} ch1={} ch2={} ch3={}",
            sample_set[0], sample_set[1], sample_set[2], sample_set[3]
        );
        eprintln!(
            "    finished per ch: {:?}, song_end={}, speed_changes={}, total_mutes={}",
            channel_finished,
            song_end,
            speed_changes,
            mute_count.iter().sum::<usize>(),
        );
        // Histogram of note frames per channel, bucketed by
        // tenth of the song duration. Tells us whether notes
        // are evenly distributed (simulator OK) or clumped at
        // one end (simulator dropping events somewhere).
        let buckets = 10usize;
        let last = final_frame.max(1);
        let mut hist: [[usize; 10]; 4] = [[0; 10]; 4];
        for (frame, ev) in &trace {
            if let TickEvent::NoteOn { channel, .. } = ev {
                let ch = (*channel as usize).min(3);
                let bucket = ((*frame as u64 * buckets as u64 - 1) / last as u64) as usize;
                let bucket = bucket.min(buckets - 1);
                hist[ch][bucket] += 1;
            }
        }
        for ch in 0..4 {
            eprintln!(
                "    ch{} note distribution (10% buckets): {:?}",
                ch, hist[ch]
            );
        }
        eprintln!(
            "    final channel state: ch0 pos={}/{} cursor={} sp_cnt={} | ch1 pos={}/{} | ch2 pos={}/{} | ch3 pos={}/{}",
            sim.channels[0].position_index,
            dw.position_lists[0].len(),
            sim.channels[0].event_cursor,
            sim.channels[0].speed_counter,
            sim.channels[1].position_index,
            dw.position_lists[1].len(),
            sim.channels[2].position_index,
            dw.position_lists[2].len(),
            sim.channels[3].position_index,
            dw.position_lists[3].len(),
        );

        let module = dw.to_module();
        assert_eq!(
            module.instrument.len(),
            dw.samples.len(),
            "every DwSample should yield exactly one Instrument"
        );
        assert_eq!(
            module.origin,
            Some(crate::tracker::format::ModuleFormat::Dw),
            "origin should be tagged as Dw"
        );

        // DAW layer must be coherent — same invariant every other
        // importer in xmrs maintains.
        module
            .verify_layers_consistent()
            .expect("DW DAW layer should be consistent");

        let cell_count: usize = module
            .tracks
            .iter()
            .map(|t| match t {
                crate::core::daw::track::Track::Notes { rows, .. } => rows.len(),
                _ => 0,
            })
            .sum();
        let triggered: usize = module
            .tracks
            .iter()
            .map(|t| match t {
                crate::core::daw::track::Track::Notes { rows, .. } => rows
                    .iter()
                    .filter(|c| !matches!(c.event, crate::core::cell::CellEvent::None))
                    .count(),
                _ => 0,
            })
            .sum();
        let pre_dedup_segments = dw.segment_count_for_diagnostics();
        eprintln!(
            "  daw layer: {} segments → {} tracks (dedup), {} clips, {} timeline entries, {} cells ({} triggered)",
            pre_dedup_segments,
            module.tracks.len(),
            module.clips.as_slice().len(),
            module.timeline_map.entries.len(),
            cell_count,
            triggered,
        );
    }
}
