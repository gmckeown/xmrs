//! Throwaway diagnostic for PLAN.md Tier 1 silent-channel triage.
//! For each `.dw` arg: per channel of song 0, report note count, the
//! set of instruments triggered, and each instrument's PCM length.
//!
//! ```text
//! cargo run --release --features=demo --example dw_channel_energy -- <file.dw>...
//! ```

use std::collections::{BTreeMap, BTreeSet};

use xmrs::core::cell::CellEvent;
use xmrs::core::instrument::InstrumentType;
use xmrs::tracker::import::dw::dw_module::DwModule;

fn instr_pcm_len(module: &xmrs::core::module::Module, idx: usize) -> usize {
    let Some(instr) = module.instrument.get(idx) else {
        return 0;
    };
    match &instr.instr_type {
        InstrumentType::Default(d) => d
            .sample
            .iter()
            .flatten()
            .map(|s| s.data.as_ref().map(|x| x.len()).unwrap_or(0))
            .sum(),
        _ => 0,
    }
}

/// (max |pcm|, number of mapped pitches in keyboard) for an instrument.
fn instr_probe(module: &xmrs::core::module::Module, idx: usize) -> (i32, usize) {
    let Some(instr) = module.instrument.get(idx) else {
        return (0, 0);
    };
    match &instr.instr_type {
        InstrumentType::Default(d) => {
            let max_abs = d
                .sample
                .iter()
                .flatten()
                .map(|s| match &s.data {
                    Some(xmrs::core::sample::SampleDataType::Mono8(v)) => {
                        v.iter().map(|x| (*x as i32).abs()).max().unwrap_or(0)
                    }
                    Some(xmrs::core::sample::SampleDataType::Mono16(v)) => {
                        v.iter().map(|x| (*x as i32).abs()).max().unwrap_or(0)
                    }
                    _ => 0,
                })
                .max()
                .unwrap_or(0);
            let mapped = d
                .keyboard
                .sample_for_pitch
                .iter()
                .filter(|s| s.is_some())
                .count();
            (max_abs, mapped)
        }
        _ => (0, 0),
    }
}

fn main() {
    for path in std::env::args().skip(1) {
        let data = std::fs::read(&path).expect("read");
        let dw = match DwModule::load(&data) {
            Ok(m) => m,
            Err(e) => {
                println!("{}: load error {:?}", path, e);
                continue;
            }
        };
        let module = dw.to_module();
        let num_ch = module.get_num_channels();
        println!("\n=== {} ({} channels) ===", path, num_ch);

        // notes per channel + instruments used (song 0 only)
        let mut notes = vec![0usize; num_ch];
        let mut audible = vec![0usize; num_ch]; // velocity > 0
        let mut instrs: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); num_ch];
        let mut songs: BTreeSet<u16> = BTreeSet::new();
        for entry in &module.timeline_map.entries {
            songs.insert(entry.song);
            if entry.song != 0 {
                continue;
            }
            let cells = module.row_at(
                entry.song as usize,
                entry.pattern_idx as usize,
                entry.row_idx as usize,
            );
            for (ch, (cell, instr)) in cells.iter().enumerate() {
                if ch >= num_ch {
                    break;
                }
                if let CellEvent::NoteOn { velocity, .. }
                | CellEvent::NoteOnGhost { velocity, .. } = &cell.event
                {
                    notes[ch] += 1;
                    if velocity.raw() != xmrs::core::fixed::fixed::Q15::ZERO {
                        audible[ch] += 1;
                    }
                    if let Some(i) = instr {
                        instrs[ch].insert(*i);
                    }
                }
            }
        }
        println!("songs present: {:?}", songs);

        println!("channel_defaults: {:?}", module.channel_defaults);

        // first few note-cells per channel (song 0): tick, pitch, vel, instr, fx
        let mut shown = vec![0usize; num_ch];
        for entry in &module.timeline_map.entries {
            if entry.song != 0 {
                continue;
            }
            let cells = module.row_at(
                entry.song as usize,
                entry.pattern_idx as usize,
                entry.row_idx as usize,
            );
            for (ch, (cell, instr)) in cells.iter().enumerate() {
                if ch >= num_ch || shown[ch] >= 5 {
                    continue;
                }
                if let CellEvent::NoteOn { pitch, velocity }
                | CellEvent::NoteOnGhost { pitch, velocity } = &cell.event
                {
                    println!(
                        "  ch{} t{} {:?} vel={:?} instr={:?} fx={:?}",
                        ch, entry.tick, pitch, velocity, instr, cell.effects
                    );
                    shown[ch] += 1;
                }
            }
        }

        let mut pcm_cache: BTreeMap<usize, usize> = BTreeMap::new();
        for ch in 0..num_ch {
            let mut instr_desc = Vec::new();
            for &i in &instrs[ch] {
                let len = *pcm_cache
                    .entry(i)
                    .or_insert_with(|| instr_pcm_len(&module, i));
                let (max_abs, mapped) = instr_probe(&module, i);
                instr_desc.push(format!("#{}={}B(max|{}|,{}keys)", i, len, max_abs, mapped));
            }
            println!(
                "  ch{}: {} notes ({} vel>0), instr [{}]",
                ch,
                notes[ch],
                audible[ch],
                instr_desc.join(", ")
            );
        }
    }
}
