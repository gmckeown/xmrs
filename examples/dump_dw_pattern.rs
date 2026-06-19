//! Dump the rendered tracker-grid view of a `.dw` after `to_module`.
//!
//! ```text
//! cargo run --release --features=demo --example dump_dw_pattern -- <file.dw> <song> <pattern> [channel]
//! ```
//!
//! Prints, for the given (song, pattern), each row's cell on every
//! channel (or only `channel` if given): note, instrument, row-pinned
//! `Cell.effects`, and the continuous automation lanes (vibrato /
//! slide / volume-envelope etc.) overlapping that row's tick window
//! for the track active on that channel.

use std::process::ExitCode;

use xmrs::core::cell::CellEvent;
use xmrs::core::daw::automation::AutomationTarget;
use xmrs::tracker::import::dw::dw_module::DwModule;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("usage: dump_dw_pattern <file.dw> <song> <pattern> [channel]");
        return ExitCode::from(2);
    }
    let path = &args[0];
    let song: usize = args[1].parse().unwrap();
    let pattern: usize = args[2].parse().unwrap();
    let only_ch: Option<usize> = args.get(3).map(|s| s.parse().unwrap());

    let data = std::fs::read(path).expect("read");
    let dw = DwModule::load(&data).expect("DwModule::load");
    let module = dw.to_module();
    let num_ch = module.get_num_channels();

    let rows = module.timeline_map.row_count_in_pattern(pattern);
    println!(
        "{} — song {} pattern {} ({} rows, {} channels)",
        path, song, pattern, rows, num_ch
    );

    for row in 0..rows {
        let entry = match module.timeline_map.find_entry(song, pattern, row) {
            Some(e) => e,
            None => continue,
        };
        let tick = entry.tick;
        let row_end = tick + entry.speed_at_row.max(1) as u32;
        let cells = module.row_at(song, pattern, row);

        for (ch, (cell, instr)) in cells.iter().enumerate() {
            if let Some(only) = only_ch {
                if ch != only {
                    continue;
                }
            }
            // What track is active on this channel/tick? (for automation lookup)
            let track_idx = module
                .clips
                .active_at(song as u16, ch as u8, tick)
                .filter(|(_, c)| tick < c.end_tick)
                .map(|(_, c)| c.track);

            let note = match &cell.event {
                CellEvent::NoteOn { pitch, velocity } => format!("{:?} v{:?}", pitch, velocity),
                CellEvent::NoteOnGhost { pitch, velocity } => {
                    format!("{:?}~ v{:?}", pitch, velocity)
                }
                CellEvent::NoteOff { .. } => "===".into(),
                CellEvent::NoteCut => "^^^".into(),
                CellEvent::NoteFade => "~~~".into(),
                CellEvent::InstrReset => "ins".into(),
                CellEvent::None => "---".into(),
            };

            // Gather automation lanes overlapping this row for the active track.
            let mut lanes_desc: Vec<String> = Vec::new();
            if let Some(t) = track_idx {
                for tgt in [
                    AutomationTarget::TrackPitch(t),
                    AutomationTarget::TrackVolume(t),
                    AutomationTarget::TrackPanning(t),
                    AutomationTarget::TrackChannelVolume(t),
                ] {
                    for lane in module.lanes_for(tgt, 0) {
                        let evs = lane.events_in(tick, row_end);
                        if !evs.is_empty() {
                            lanes_desc.push(format!(
                                "{:?}{}: {:?}",
                                lane.target,
                                if lane.enabled { "" } else { "(off)" },
                                evs
                            ));
                        }
                    }
                }
            }

            let is_empty = matches!(cell.event, CellEvent::None)
                && cell.effects.is_empty()
                && lanes_desc.is_empty();
            if is_empty {
                continue;
            }

            print!(
                "  row {:02X} ch{} t{:<5} trk{:?}  {:<12} ins={:?}",
                row, ch, tick, track_idx, note, instr
            );
            if !cell.effects.is_empty() {
                print!("  fx={:?}", cell.effects);
            }
            for l in &lanes_desc {
                print!("  | {}", l);
            }
            println!();
        }
    }

    ExitCode::SUCCESS
}
