//! Does the last generation's *entry* decoder read the three-marker
//! generation's patterns?
//!
//! Their first pattern opens with the same three bytes as Lion Heart's — `df 60
//! 81`, a single 32-row rest — which suggests the note-entry encoding carried
//! over unchanged and only the marker set shrank. That is worth testing before
//! writing a second decoder: if it holds, the remaining work is the markers and
//! the tables, not the walk.
//!
//! ```sh
//! cargo run --release --example sid_three_marker_decode --features import_sid
//! ```

use xmrs::tracker::import::sid::g2::{G2Config, Generation};

const OFF_THE_CUFF: &[u8] = include_bytes!("../src/tracker/import/sid/songs/off_the_cuff.sid");
const LION_HEART: &[u8] = include_bytes!("../src/tracker/import/sid/songs/lion_heart.sid");

fn report(name: &str, data: &[u8]) {
    let Some(c) = G2Config::detect(data) else {
        println!("{name}: not recognised");
        return;
    };
    println!("\n=== {name} ({:?}) ===", c.generation);
    println!(
        "  order pointers (subtune 0): {:04x?}",
        c.order_pointers(data, 0)
    );

    for (t, &ptr) in c.order_pointers(data, 0).iter().enumerate() {
        let steps = c.order_list(data, ptr);
        let shown: Vec<String> = steps.iter().take(10).map(|s| format!("{s:?}")).collect();
        println!(
            "  track {t} @{ptr:04x}: {} steps — {}",
            steps.len(),
            shown.join(" ")
        );
    }

    // Walk every pattern the order lists reach and report how each decodes.
    // A pattern that decodes to nothing, or to one entry of absurd length, is
    // the decoder failing rather than the tune being sparse.
    let mut reached: Vec<u8> = Vec::new();
    for &ptr in c.order_pointers(data, 0).iter() {
        for s in c.order_list(data, ptr) {
            if let xmrs::tracker::import::sid::g2::OrderStep::Pattern(p) = s {
                if !reached.contains(&p) {
                    reached.push(p);
                }
            }
        }
    }
    reached.sort_unstable();
    println!("  {} distinct patterns reached", reached.len());

    let (mut empty, mut notes, mut rests, mut markers) = (0usize, 0usize, 0usize, 0usize);
    for &p in &reached {
        let entries = c.pattern(data, p);
        if entries.is_empty() {
            empty += 1;
        }
        for e in &entries {
            if e.note.is_some() {
                notes += 1;
            } else {
                rests += 1;
            }
            markers += e.markers.len();
        }
    }
    println!("  {notes} notes, {rests} rests, {markers} markers, {empty} patterns decoded empty");

    // The first pattern, in full — the direct comparison with Lion Heart's.
    if let Some(&first) = reached.first() {
        let e = c.pattern(data, first);
        println!("  pattern {first}: {:?}", &e[..e.len().min(6)]);
    }
}

fn dump_programs(name: &str, data: &[u8]) {
    let Some(c) = G2Config::detect(data) else {
        return;
    };
    println!("\n  voice programs of {name}:");
    for i in 0..16 {
        let ins = c.instrument(data, i);
        if let Some(p) = &ins.program {
            println!(
                "    instrument {i}: {} steps, end {:?}",
                p.steps.len(),
                p.end
            );
            for s in p.steps.iter().take(6) {
                println!("      {s:?}");
            }
        }
    }
}

fn main() {
    report("lion_heart", LION_HEART);
    report("off_the_cuff", OFF_THE_CUFF);
    dump_programs("off_the_cuff", OFF_THE_CUFF);
    let _ = Generation::Last;
}
