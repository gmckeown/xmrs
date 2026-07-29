//! How much of a Rob Hubbard corpus the two detectors can actually open.
//!
//! Fifteen tunes have a hand-written record in `one_sid.rs`; everything else was
//! unreachable. This walks a directory of `.sid` files and reports, per file,
//! which detector claims it — the last generation (`g2::G2Config::detect`), the
//! v10..v30 family (`SidLayout::detect`), or neither — plus the layout the
//! second one recovered, so an implausible reading is visible rather than
//! silent.
//!
//! ```sh
//! cargo run --release --example sid_corpus_scan --features import_sid -- ~/path/to/sids
//! ```

use xmrs::tracker::import::sid::detect::SidLayout;
use xmrs::tracker::import::sid::g2::G2Config;

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: sid_corpus_scan <directory of .sid files>");
        std::process::exit(2);
    });

    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{dir}: {e}"))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("sid")))
        .collect();
    files.sort();

    let (mut g2, mut v1030, mut none) = (0, 0, 0);
    let mut unclaimed: Vec<String> = Vec::new();
    let mut suspect: Vec<(String, usize, usize)> = Vec::new();
    let mut broken: Vec<(String, String)> = Vec::new();
    std::panic::set_hook(Box::new(|_| {})); // the report below is the output
    println!(
        "{:<38} {:>6}  {:<14} {:>5} {:>5} {:>5} {:>5} {:>4}",
        "file", "player", "stream", "songs", "patts", "instr", "load", "skip"
    );
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let Ok(data) = std::fs::read(path) else {
            continue;
        };
        if let Some(c) = G2Config::detect(&data) {
            g2 += 1;
            println!(
                "{name:<38} {:>6}  instr {:04x} pptr {:04x} order {:04x} tempo {:04x}",
                match c.generation {
                    xmrs::tracker::import::sid::g2::Generation::Last => "G2",
                    _ => "3mark",
                },
                c.instruments,
                c.pattern_ptrs,
                c.order_ptrs,
                c.tempo_table
            );
        } else if let Some(d) = SidLayout::detect(&data) {
            v1030 += 1;
            println!(
                "{name:<38} {:>6}  {:<14} {:>5} {:>5} {:>5} {:>5x} {:>4}",
                format!("v{}", d.version()),
                format!("{:?}", d.stream),
                d.song_list_qty,
                d.patt_qty,
                d.instr_qty,
                d.load,
                d.frame_skip_reset
            );
            if d.patt_qty <= 4 || d.instr_qty <= 2 {
                suspect.push((name.clone(), d.patt_qty, d.instr_qty));
            }
            // Recognising a tune is not the same as surviving it. The importer
            // indexes the image directly in places, so a layout that passes
            // every consistency rule can still walk off the end while the
            // patterns are being decoded — and the player now offers these files
            // to anyone with a path.
            let leaked: &'static [u8] = Box::leak(data.clone().into_boxed_slice());
            match std::panic::catch_unwind(|| {
                xmrs::tracker::import::sid::sid_module::SidModule::from_detected(leaked)
                    .map(|m| m.to_modules().len())
            }) {
                Ok(Some(n)) if n > 0 => {}
                Ok(other) => broken.push((name, format!("built {other:?} sub-songs"))),
                Err(_) => broken.push((name, "PANICKED".into())),
            }
        } else {
            none += 1;
            // A file can be rejected because it is broken, or because it is a
            // player nobody has written a reader for. The marker dispatcher
            // tells them apart: `CMP #$82`/`#$83` without `#$87` is the
            // three-marker generation that sits between the fx-mask players and
            // the last one — supported by neither reader, and not a bug.
            let img = &data[126.min(data.len())..];
            let marker = |m: u8| {
                img.windows(3)
                    .any(|w| w[0] == 0xC9 && w[1] == m && w[2] == 0xF0)
            };
            let family = if marker(0x80) && marker(0x82) && !marker(0x87) {
                " [three-marker generation — see THREE_MARKER_GENERATION.md]"
            } else {
                ""
            };
            let raw = SidLayout::detect_unchecked(&data).map_or_else(
                || String::from("(nothing recovered)"),
                |d| {
                    format!(
                        "list {:04x} ptl {:04x} gap {:>3} trk {}",
                        d.song_list,
                        d.patt_ptl,
                        d.patt_ptl.wrapping_sub(d.song_list),
                        d.song_track_qty
                    )
                },
            );
            unclaimed.push(format!(
                "{name:<38} {:<52} {raw}{family}",
                SidLayout::why_not(&data).unwrap_or("?")
            ));
        }
    }

    println!(
        "\n{} files: {g2} last-generation, {v1030} v10..v30, {none} unclaimed",
        files.len()
    );
    if !suspect.is_empty() {
        // A layout can satisfy every consistency rule and still be wrong: one
        // pattern of one instrument passes each check individually. Call these
        // out rather than counting them as wins.
        println!(
            "\n{} of the {v1030} look degenerate (a handful of patterns or instruments — \
             plausible for a jingle, suspicious for a game score):",
            suspect.len()
        );
        for (n, p, i) in &suspect {
            println!("  {n:<38} {p} pattern(s), {i} instrument(s)");
        }
    }
    if broken.is_empty() {
        println!("\nevery recognised tune imported and built at least one sub-song.");
    } else {
        println!("\n{} recognised but did NOT import cleanly:", broken.len());
        for (n, why) in &broken {
            println!("  {n:<38} {why}");
        }
    }
    if !unclaimed.is_empty() {
        println!("\nunclaimed:");
        for n in &unclaimed {
            println!("  {n}");
        }
    }
}
