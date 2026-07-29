//! Census of the per-note MARKERS used by Rob Hubbard's last player generation.
//!
//! The markers are the whole point of that generation — effects bound to the
//! note instead of the instrument — so before deciding how they should land in
//! the model, it is worth knowing which ones the music actually uses, how often,
//! and how many distinct (instrument, marker-set) combinations exist.
//!
//! ```text
//! cargo run --example g2_census --features "std,import_sid" -- <tune.sid>...
//! ```

use std::collections::{BTreeMap, BTreeSet};

use xmrs::tracker::import::sid::g2::{G2Config, NoteMarker, RowNote};

fn kind(m: &NoteMarker) -> &'static str {
    match m {
        NoteMarker::Instrument(_) => "instrument",
        NoteMarker::Portamento(_) => "portamento",
        NoteMarker::Arpeggio(_) => "arpeggio",
        NoteMarker::HoldFilter => "hold-filter",
        NoteMarker::Vibrato(_) => "vibrato",
        NoteMarker::Cutoff(_) => "cutoff",
        NoteMarker::HoldPulseWidth => "hold-pw",
        NoteMarker::FilterVoice(_) => "filter-voice",
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: g2_census <tune.sid>...");
        std::process::exit(2);
    }

    let mut grand: BTreeMap<&'static str, usize> = BTreeMap::new();
    println!(
        "{:<22} {:>6} {:>8} {:>7}  markers used",
        "tune", "notes", "w/marker", "combos"
    );

    for path in &args {
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("{path}: {e}");
                continue;
            }
        };
        let name = std::path::Path::new(path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());

        let Some(c) = G2Config::detect(&data) else {
            println!("{name:<22} — not a last-generation tune");
            continue;
        };
        let bank = c.instruments(&data);

        let mut notes = 0usize;
        let mut marked = 0usize;
        let mut combos: BTreeSet<String> = BTreeSet::new();
        let mut used: BTreeMap<&'static str, usize> = BTreeMap::new();

        for ptr in c.order_pointers(&data, 0) {
            for r in c.voice_rows(&data, ptr, &bank) {
                if matches!(r.note, RowNote::Hold) {
                    continue;
                }
                notes += 1;
                let mut set: Vec<&'static str> = r
                    .markers
                    .iter()
                    .filter(|m| !matches!(m, NoteMarker::Instrument(_)))
                    .map(kind)
                    .collect();
                set.sort_unstable();
                set.dedup();
                if !set.is_empty() {
                    marked += 1;
                    for k in &set {
                        *used.entry(k).or_default() += 1;
                        *grand.entry(k).or_default() += 1;
                    }
                }
                combos.insert(format!("{}|{}", r.instrument, set.join(",")));
            }
        }

        // How many INSTRUMENT variants a no-core import would need: one per
        // distinct (instrument, hold-filter, hold-pw, vibrato, cutoff) — the
        // markers that map onto instrument fields. `reseed_each_note` and
        // `reseed_on_note` already exist, so the two holds need no new field,
        // only a duplicated patch.
        let mut variants: BTreeSet<String> = BTreeSet::new();
        for ptr in c.order_pointers(&data, 0) {
            for r in c.voice_rows(&data, ptr, &bank) {
                if matches!(r.note, RowNote::Hold) {
                    continue;
                }
                let mut key = format!("{}", r.instrument);
                for m in &r.markers {
                    match m {
                        NoteMarker::HoldFilter => key.push_str("|hf"),
                        NoteMarker::HoldPulseWidth => key.push_str("|hp"),
                        NoteMarker::Vibrato(v) => key.push_str(&format!("|v{v}")),
                        NoteMarker::Cutoff(v) => key.push_str(&format!("|c{v}")),
                        _ => {}
                    }
                }
                variants.insert(key);
            }
        }
        // What the importer actually RESOLVES into per-note arming.
        let (mut lg, mut hf, mut hp, mut at) = (0, 0, 0, 0);
        for ptr in c.order_pointers(&data, 0) {
            for r in c.voice_rows(&data, ptr, &bank) {
                if r.arming.legato {
                    lg += 1;
                }
                match r.arming.filter {
                    xmrs::core::effect::ArmMode::Hold => hf += 1,
                    xmrs::core::effect::ArmMode::At(_) => at += 1,
                    _ => {}
                }
                if r.arming.pulse_width == xmrs::core::effect::ArmMode::Hold {
                    hp += 1;
                }
            }
        }
        println!(
            "{:<22} arming resolved: legato={lg} hold-filter={hf} hold-pw={hp} cutoff-at={at}",
            name
        );
        let list: Vec<String> = used.iter().map(|(k, n)| format!("{k}×{n}")).collect();
        // Distinct cutoff VALUES, and how they spread over instruments: if a
        // handful of values are reused, the variant count collapses.
        let mut cut_vals: BTreeSet<u16> = BTreeSet::new();
        let mut cut_pairs: BTreeSet<(u8, u16)> = BTreeSet::new();
        for ptr in c.order_pointers(&data, 0) {
            for r in c.voice_rows(&data, ptr, &bank) {
                for m in &r.markers {
                    if let NoteMarker::Cutoff(v) = m {
                        cut_vals.insert(*v);
                        cut_pairs.insert((r.instrument, *v));
                    }
                }
            }
        }
        println!(
            "{:<22} {:>6} instrument variants  |  distinct cutoff values={} (instr,cutoff) pairs={}",
            name,
            variants.len(),
            cut_vals.len(),
            cut_pairs.len()
        );
        println!(
            "{:<22} {:>6} {:>8} {:>7}  {}",
            name,
            notes,
            marked,
            combos.len(),
            list.join(" ")
        );
    }

    println!("\ncorpus total per marker:");
    for (k, n) in &grand {
        println!("  {k:<14} {n}");
    }
}
