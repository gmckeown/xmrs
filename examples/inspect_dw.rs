//! Triage tool for the David Whittaker (`.dw`) importer.
//!
//! Loads a single `.dw` file through `DwModule::load`, then prints a
//! one-screen diagnostic covering the bits that vary between modules:
//! detection layout, sample bank, dispatcher cascade, feature flags,
//! position lists, track event mix, and the simulator outcome.
//!
//! ```text
//! cargo run --release --features=demo --example inspect_dw -- path/to/file.dw
//! cargo run --release --features=demo --example inspect_dw -- ~/Music/dw/*.dw
//! ```
//!
//! Pass `--quiet` (or `-q`) to drop every block but the one-line
//! summary — useful for batch triage across a folder.
use std::process::ExitCode;

use xmrs::tracker::import::dw::detect::detect;
use xmrs::tracker::import::dw::dw_module::DwModule;
use xmrs::tracker::import::dw::event::DwTrackEvent;
use xmrs::tracker::import::dw::runtime::{Simulator, TickEvent};

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let quiet = args.iter().any(|a| a == "-q" || a == "--quiet");
    args.retain(|a| a != "-q" && a != "--quiet");
    if args.is_empty() {
        eprintln!("usage: inspect_dw [-q|--quiet] <file.dw> [more.dw ...]");
        return ExitCode::from(2);
    }

    let mut failed = 0u32;
    for path in &args {
        match inspect_one(path, quiet) {
            Ok(()) => {}
            Err(msg) => {
                eprintln!("{}: ERROR — {}", path, msg);
                failed += 1;
            }
        }
    }
    if failed > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn inspect_one(path: &str, quiet: bool) -> Result<(), String> {
    let data = std::fs::read(path).map_err(|e| format!("read failed: {}", e))?;

    let layout = match detect(&data) {
        Some(l) => l,
        None => {
            println!("{}: detect=FAIL (not recognised as .dw)", path);
            return Err("detect returned None".into());
        }
    };
    let dw = DwModule::load(&data).map_err(|e| format!("DwModule::load: {:?}", e))?;

    // --- one-line summary, always shown ---
    let pcm: usize = dw.samples.iter().map(|s| s.pcm.len()).sum();
    let looping = dw.samples.iter().filter(|s| s.is_looping()).count();
    let total_notes: usize = dw.tracks.iter().map(|t| t.note_count()).sum();
    let total_cmds: usize = dw.tracks.iter().map(|t| t.command_count()).sum();
    let ch_lens = [
        dw.position_lists[0].len(),
        dw.position_lists[1].len(),
        dw.position_lists[2].len(),
        dw.position_lists[3].len(),
    ];
    println!(
        "{}: variant={:?} period={:?} samples={}({}loop,{}B) tracks={}({}n/{}c) pos=[{},{},{},{}] disp={:02X?}",
        path,
        dw.variant,
        dw.period_table,
        dw.samples.len(),
        looping,
        pcm,
        dw.tracks.len(),
        total_notes,
        total_cmds,
        ch_lens[0],
        ch_lens[1],
        ch_lens[2],
        ch_lens[3],
        dw.dispatcher.thresholds,
    );

    if quiet {
        return Ok(());
    }

    // --- detection layout ---
    println!("  layout:");
    println!("    start_offset={:#X}", layout.start_offset);
    println!(
        "    init=0x{:X} play={} sample_info={} sample_data={} sub_song={} N_samples={}",
        layout.init_offset,
        opt_hex(layout.play_offset),
        opt_hex(layout.sample_info_offset),
        opt_hex(layout.sample_data_offset),
        opt_hex(layout.sub_song_list_offset),
        layout
            .number_of_samples
            .map(|n| n.to_string())
            .unwrap_or_else(|| "?".into()),
    );
    println!(
        "    vol_env_table={} len={}",
        opt_hex(layout.volume_envelope_table_offset),
        layout
            .volume_envelope_table_len
            .map(|n| n.to_string())
            .unwrap_or_else(|| "?".into()),
    );
    println!(
        "    arp_table={} len={} volume_bracket_is_pitch={}",
        opt_hex(layout.arpeggio_table_offset),
        layout
            .arpeggio_table_len
            .map(|n| n.to_string())
            .unwrap_or_else(|| "?".into()),
        layout.volume_bracket_is_pitch,
    );
    let f = layout.features;
    println!(
        "  features: dly={} dly_mul={} sq={} sample_tr={} arp={} env={} vib={} half_vol={} vol_fade={} ch_tr={}",
        f.enable_delay_counter,
        f.enable_delay_multiply,
        f.enable_square_waveform,
        f.enable_sample_transpose,
        f.enable_arpeggio,
        f.enable_envelopes,
        f.enable_vibrato,
        f.enable_half_volume,
        f.enable_volume_fade,
        f.enable_channel_transpose,
    );

    // --- sub-song / position lists ---
    if dw.sub_songs.is_empty() {
        println!("  sub_song: <not located>");
    } else {
        let sel = dw.selected_sub_song.unwrap_or(0);
        println!(
            "  sub_songs: {} total, selected=#{}",
            dw.sub_songs.len(),
            sel
        );
        for (i, ss) in dw.sub_songs.iter().enumerate() {
            let lists = xmrs::tracker::import::dw::dw_module::position_lists_for(
                &data,
                ss,
                layout.uses_32bit_pointers,
                layout.start_offset,
            );
            let cum: usize = lists.iter().map(|l| l.len()).sum();
            let marker = if Some(i) == dw.selected_sub_song {
                " *"
            } else {
                "  "
            };
            println!(
                "   {}#{}: speed={} delay={} ch_offsets={:04X?} cum_entries={}",
                marker, i, ss.speed, ss.delay_speed, ss.channel_position_offsets, cum,
            );
        }
    }
    for (ch, list) in dw.position_lists.iter().enumerate() {
        let loop_str = match list.loop_to {
            Some(i) => format!(", loop_to={}", i),
            None => String::new(),
        };
        println!("    ch{}: {} entries{}", ch, list.len(), loop_str);
    }

    // --- samples (compact) ---
    if !dw.samples.is_empty() {
        println!("  samples:");
        for s in &dw.samples {
            let loop_s = match s.loop_start {
                Some(ls) => format!("loop={}", ls),
                None => "one-shot".into(),
            };
            println!(
                "    s{:02}: len={} freq={}Hz vol={} tr={} {}",
                s.index, s.length, s.frequency, s.volume, s.transpose, loop_s,
            );
        }
    }

    // --- arpeggio table ---
    if !dw.arpeggios.is_empty() {
        let nonzero = dw
            .arpeggios
            .iter()
            .filter(|a| a.offsets.iter().any(|&o| o != 0))
            .count();
        println!(
            "  arpeggios: {} entries ({} non-trivial)",
            dw.arpeggios.len(),
            nonzero
        );
        for a in dw.arpeggios.iter().take(6) {
            if a.offsets.iter().any(|&o| o != 0) {
                println!("    arp{:02}: {:?}", a.index, a.offsets);
            }
        }
    }

    // --- track event histogram ---
    let mut notes = 0usize;
    let mut long_waits = 0usize;
    let mut set_samples = 0usize;
    let mut slides = 0usize;
    let mut vibratos = 0usize;
    let mut speeds = 0usize;
    let mut transposes = 0usize;
    let mut fx8 = 0usize;
    let mut fx9 = 0usize;
    let mut waits_until_next = 0usize;
    let mut set_arpeggios = 0usize;
    let mut set_envelopes = 0usize;
    let mut other = 0usize;
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
                DwTrackEvent::Effect8(_) => fx8 += 1,
                DwTrackEvent::Effect9(_) => fx9 += 1,
                DwTrackEvent::WaitUntilNextRow => waits_until_next += 1,
                DwTrackEvent::EndOfTrack => {}
                DwTrackEvent::SetVolumeEnvelope(_) => set_envelopes += 1,
                DwTrackEvent::SetPitchArpeggio(_) => set_arpeggios += 1,
                _ => other += 1,
            }
        }
    }
    println!(
        "  events: notes={} long_wait={} sample={} arp={} env={} slide={} vib={} speed={} trans={} fx8={} fx9={} wait_row={} other={}",
        notes,
        long_waits,
        set_samples,
        set_arpeggios,
        set_envelopes,
        slides,
        vibratos,
        speeds,
        transposes,
        fx8,
        fx9,
        waits_until_next,
        other,
    );

    // --- per-track event dump (debug): DW_TRACK=0x8f4 ---
    if let Ok(spec) = std::env::var("DW_TRACK") {
        let want = u32::from_str_radix(spec.trim_start_matches("0x"), 16).unwrap_or(u32::MAX);
        for t in &dw.tracks {
            if want == u32::MAX || t.offset == want {
                println!("  TRACK @{:04x} bytes={:02x?}", t.offset, t.bytes);
                for ev in &t.events {
                    println!("      {:?}", ev);
                }
            }
        }
    }

    // --- simulator ---
    // Render exactly one seamless loop period (channels loop their
    // own lists; they realign at the LCM of the per-channel pass
    // lengths) so the diagnostic doesn't run to the 20-min cap now
    // that position lists loop.
    let mut sim = Simulator::new(&dw);
    let (trace, loop_period) =
        sim.run_with_loop(xmrs::tracker::import::dw::runtime::MAX_SIMULATION_FRAMES);
    let halted = sim.halted;
    let final_frame = trace.iter().map(|(f, _)| *f).max().unwrap_or(0);
    let mut note_per_ch = [0usize; 4];
    let mut first_note = [u32::MAX; 4];
    let mut finished = [false; 4];
    let mut song_end = false;
    for (frame, ev) in &trace {
        match ev {
            TickEvent::NoteOn { channel, .. } => {
                let c = *channel as usize;
                note_per_ch[c] += 1;
                if first_note[c] == u32::MAX {
                    first_note[c] = *frame;
                }
            }
            TickEvent::ChannelFinished { channel } => finished[*channel as usize] = true,
            TickEvent::SongEnd { .. } => song_end = true,
            _ => {}
        }
    }
    if std::env::var("DW_TRACE").is_ok() {
        for (frame, ev) in &trace {
            println!("    [f{}] {:?}", frame, ev);
        }
    }
    println!("  first NoteOn frame per ch: {:?}", first_note);
    println!(
        "  loop: period={} per_ch_wrap={:?}",
        loop_period.map(|p| p as i64).unwrap_or(-1),
        sim.first_wrap_frame,
    );
    println!(
        "  sim: {} frames (~{}s) halted={} song_end={} finished={:?} notes_per_ch=[{},{},{},{}] trace={}",
        final_frame,
        final_frame / 50,
        halted,
        song_end,
        finished,
        note_per_ch[0],
        note_per_ch[1],
        note_per_ch[2],
        note_per_ch[3],
        trace.len(),
    );

    // End-to-end sanity: the conversion to xmrs `Module` is what
    // downstream callers see. Failing here would mean the importer
    // builds a `DwModule` that the DAW projection can't handle.
    let mut module = dw.to_module();
    // `.dw` has no embedded title; title the module after the file
    // name (sans extension), mirroring `Module::load_dw_path`.
    if let Some(stem) = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
    {
        if !stem.is_empty() {
            module.name = stem.to_string();
        }
    }
    let cell_count: usize = module
        .tracks
        .iter()
        .map(|t| t.rows().map(|r| r.len()).unwrap_or(0))
        .sum();
    println!(
        "  module: name={:?} instruments={} tracks={} clips={} cells={} timeline={}",
        module.name,
        module.instrument.len(),
        module.tracks.len(),
        module.clips.len(),
        cell_count,
        module.timeline_map.entries.len(),
    );
    Ok(())
}

fn opt_hex(v: Option<usize>) -> String {
    match v {
        Some(x) => format!("0x{:X}", x),
        None => "<none>".into(),
    }
}
