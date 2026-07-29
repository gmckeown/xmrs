//! Reverse-reference probe for the v10..v30 Rob Hubbard replayers.
//!
//! The fifteen `OneSid` records in `one_sid.rs` are hand-written address tables:
//! every one of them was read out of Ghidra by hand, which is why no tune
//! outside that list can be imported. Before writing a `detect()` for them (the
//! way `G2Config::detect` recovers the last generation's tables from the image),
//! this probe answers the prerequisite question: **which 6502 instruction
//! references each table?**
//!
//! It works backwards. For every known table address it scans the image for any
//! absolute-addressed instruction whose operand lands on (or just inside) that
//! table, and prints the instruction with its context. A byte pattern that shows
//! up at the same offset-into-the-table in all fifteen tunes is a signature
//! `detect()` can key on; one that appears in a single tune is not.
//!
//! Nothing here is wired into the importer — it only reads the bundled `.sid`
//! files and the truth table transcribed below.
//!
//! ```sh
//! cargo run --release --example sid_xref_probe --features import_sid
//! cargo run --release --example sid_xref_probe --features import_sid -- instr
//! ```

use std::collections::BTreeMap;

/// A tune's known-good table addresses, transcribed from `one_sid.rs`.
struct Truth {
    name: &'static str,
    file: &'static str,
    version: usize,
    load: u16,
    song_list: u16,
    song_list_qty: usize,
    patt_ptl: u16,
    patt_pth: u16,
    patt_qty: usize,
    instr: u16,
    instr_qty: usize,
    fx_v1: u16,
    fx_v2: u16,
    resetspd: usize,
    resetspd_songs: &'static [usize],
    tempo_frac: Option<(u32, u32)>,
    filter_seed: u8,
    filter_table: u16,
    arp3_table: u16,
}

const TRUTH: &[Truth] = &[
    Truth {
        name: "commando",
        file: "commando.sid",
        version: 10,
        load: 0x5000,
        song_list: 0x56FF,
        song_list_qty: 3,
        patt_ptl: 0x5711,
        patt_pth: 0x573E,
        patt_qty: 45,
        instr: 0x5591,
        instr_qty: 13,
        fx_v1: 0x55F9,
        fx_v2: 0,
        resetspd: 2,
        resetspd_songs: &[2, 3, 2],
        tempo_frac: None,
        filter_seed: 0,
        filter_table: 0,
        arp3_table: 0,
    },
    Truth {
        name: "crazy_comets",
        file: "crazy_comets.sid",
        version: 10,
        load: 0x5000,
        song_list: 0x5732,
        song_list_qty: 2,
        patt_ptl: 0x573E,
        patt_pth: 0x5773,
        patt_qty: 53,
        instr: 0x5574,
        instr_qty: 23,
        fx_v1: 0x562C,
        fx_v2: 0,
        resetspd: 2,
        resetspd_songs: &[],
        tempo_frac: None,
        filter_seed: 0,
        filter_table: 0,
        arp3_table: 0,
    },
    Truth {
        name: "last_v8",
        file: "last_v8.sid",
        version: 10,
        load: 0x8010,
        song_list: 0x8797,
        song_list_qty: 3,
        patt_ptl: 0x87A9,
        patt_pth: 0x87C6,
        patt_qty: 28,
        instr: 0x85A1,
        instr_qty: 19,
        fx_v1: 0x8699,
        fx_v2: 0,
        resetspd: 1,
        resetspd_songs: &[],
        tempo_frac: None,
        filter_seed: 0,
        filter_table: 0,
        arp3_table: 0,
    },
    Truth {
        name: "monty",
        file: "monty_on_the_run.sid",
        version: 10,
        load: 0x8000,
        song_list: 0x856C,
        song_list_qty: 3,
        patt_ptl: 0x857E,
        patt_pth: 0x85CB,
        patt_qty: 77,
        instr: 0x93B4,
        instr_qty: 20,
        fx_v1: 0x9454,
        fx_v2: 0,
        resetspd: 1,
        resetspd_songs: &[],
        tempo_frac: None,
        filter_seed: 0,
        filter_table: 0,
        arp3_table: 0,
    },
    Truth {
        name: "thing",
        file: "thing_on_a_spring.sid",
        version: 10,
        load: 0xC000,
        song_list: 0xC509,
        song_list_qty: 1,
        patt_ptl: 0xC50F,
        patt_pth: 0xC533,
        patt_qty: 36,
        instr: 0xCD2A,
        instr_qty: 45,
        fx_v1: 0xCE92,
        fx_v2: 0,
        resetspd: 1,
        resetspd_songs: &[],
        tempo_frac: None,
        filter_seed: 0,
        filter_table: 0,
        arp3_table: 0,
    },
    Truth {
        name: "zoids",
        file: "zoids.sid",
        version: 10,
        load: 0x1000,
        song_list: 0x14FC,
        song_list_qty: 3,
        patt_ptl: 0x150E,
        patt_pth: 0x152D,
        patt_qty: 31,
        instr: 0x147E,
        instr_qty: 15,
        fx_v1: 0,
        fx_v2: 0,
        resetspd: 2,
        resetspd_songs: &[],
        tempo_frac: None,
        filter_seed: 0,
        filter_table: 0,
        arp3_table: 0,
    },
    Truth {
        name: "thrust",
        file: "thrust.sid",
        version: 10,
        load: 0x0800,
        song_list: 0x0DD7,
        song_list_qty: 1,
        patt_ptl: 0x0DDD,
        patt_pth: 0x0E04,
        patt_qty: 39,
        instr: 0x0CF7,
        instr_qty: 27,
        fx_v1: 0,
        fx_v2: 0,
        resetspd: 2,
        resetspd_songs: &[],
        tempo_frac: None,
        filter_seed: 0,
        filter_table: 0,
        arp3_table: 0,
    },
    Truth {
        name: "spellbound",
        file: "spellbound.sid",
        version: 15,
        load: 0xE000,
        song_list: 0xE6B6,
        song_list_qty: 3,
        patt_ptl: 0xE6C8,
        patt_pth: 0xE6F2,
        patt_qty: 42,
        instr: 0xE548,
        instr_qty: 25,
        fx_v1: 0xE610,
        fx_v2: 0,
        resetspd: 1,
        resetspd_songs: &[],
        tempo_frac: Some((11, 5)),
        filter_seed: 0,
        filter_table: 0,
        arp3_table: 0,
    },
    Truth {
        name: "ace_2",
        file: "ace_2.sid",
        version: 20,
        load: 0xE000,
        song_list: 0xE67C,
        song_list_qty: 1,
        patt_ptl: 0xE682,
        patt_pth: 0xE6A8,
        patt_qty: 38,
        instr: 0xE5CB,
        instr_qty: 10,
        fx_v1: 0,
        fx_v2: 0,
        resetspd: 1,
        resetspd_songs: &[],
        tempo_frac: Some((8, 3)),
        filter_seed: 48,
        filter_table: 0xE624,
        arp3_table: 0,
    },
    Truth {
        name: "human_race",
        file: "human_race.sid",
        version: 20,
        load: 0x0980,
        song_list: 0x0E9F,
        song_list_qty: 3,
        patt_ptl: 0x0EB3,
        patt_pth: 0x0F02,
        patt_qty: 58,
        instr: 0x0DE3,
        instr_qty: 23,
        fx_v1: 0,
        fx_v2: 0,
        resetspd: 3,
        resetspd_songs: &[],
        tempo_frac: None,
        filter_seed: 0,
        filter_table: 0,
        arp3_table: 0,
    },
    Truth {
        name: "int_karate",
        file: "international_karate.sid",
        version: 20,
        load: 0xAE00,
        song_list: 0xB3B0,
        song_list_qty: 1,
        patt_ptl: 0xB3B6,
        patt_pth: 0xB3EB,
        patt_qty: 53,
        instr: 0xB308,
        instr_qty: 20,
        fx_v1: 0,
        fx_v2: 0,
        resetspd: 2,
        resetspd_songs: &[],
        tempo_frac: Some((33, 10)),
        filter_seed: 0,
        filter_table: 0,
        arp3_table: 0,
    },
    Truth {
        name: "sanxion1",
        file: "sanxion.sid",
        version: 20,
        load: 0xB000,
        song_list: 0xB73C,
        song_list_qty: 1,
        patt_ptl: 0xB742,
        patt_pth: 0xB75D,
        patt_qty: 27,
        instr: 0xB56C,
        instr_qty: 29,
        fx_v1: 0,
        fx_v2: 0xB654,
        resetspd: 2,
        resetspd_songs: &[],
        tempo_frac: None,
        filter_seed: 0,
        filter_table: 0,
        arp3_table: 0,
    },
    Truth {
        name: "sanxion2",
        file: "sanxion.sid",
        version: 20,
        load: 0xB000,
        song_list: 0xC5F5,
        song_list_qty: 1,
        patt_ptl: 0xC5FB,
        patt_pth: 0xC633,
        patt_qty: 56,
        instr: 0xC4B5,
        instr_qty: 20,
        fx_v1: 0,
        fx_v2: 0xC5F5,
        resetspd: 2,
        resetspd_songs: &[],
        tempo_frac: None,
        filter_seed: 0,
        filter_table: 0,
        arp3_table: 0,
    },
    Truth {
        name: "lightforce",
        file: "lightforce.sid",
        version: 25,
        load: 0xF000,
        song_list: 0xF778,
        song_list_qty: 1,
        patt_ptl: 0xF77E,
        patt_pth: 0xF79D,
        patt_qty: 31,
        instr: 0xF618,
        instr_qty: 22,
        fx_v1: 0,
        fx_v2: 0xF6C8,
        resetspd: 2,
        resetspd_songs: &[],
        tempo_frac: Some((7, 2)),
        filter_seed: 0,
        filter_table: 0xF6C8,
        arp3_table: 0xF60C,
    },
    Truth {
        name: "delta",
        file: "delta.sid",
        version: 30,
        load: 0xBC00,
        song_list: 0xC4F4,
        song_list_qty: 13,
        patt_ptl: 0xC542,
        patt_pth: 0xC5AF,
        patt_qty: 109,
        instr: 0xC38E,
        instr_qty: 22,
        fx_v1: 0,
        fx_v2: 0xC43E,
        resetspd: 1,
        resetspd_songs: &[],
        tempo_frac: Some((5, 2)),
        filter_seed: 0,
        filter_table: 0xC43E,
        arp3_table: 0,
    },
];

const SONGS: &str = "xmrs/src/tracker/import/sid/songs";

/// Mnemonic and addressing mode of every absolute-operand opcode the replayers
/// use to reach a table. Anything else is a false positive by construction.
fn abs_opcode(op: u8) -> Option<(&'static str, &'static str)> {
    Some(match op {
        0x0D => ("ORA", "abs"),
        0x1D => ("ORA", "abs,X"),
        0x19 => ("ORA", "abs,Y"),
        0x2D => ("AND", "abs"),
        0x3D => ("AND", "abs,X"),
        0x39 => ("AND", "abs,Y"),
        0x4D => ("EOR", "abs"),
        0x5D => ("EOR", "abs,X"),
        0x59 => ("EOR", "abs,Y"),
        0x6D => ("ADC", "abs"),
        0x7D => ("ADC", "abs,X"),
        0x79 => ("ADC", "abs,Y"),
        0x8D => ("STA", "abs"),
        0x9D => ("STA", "abs,X"),
        0x99 => ("STA", "abs,Y"),
        0xAD => ("LDA", "abs"),
        0xBD => ("LDA", "abs,X"),
        0xB9 => ("LDA", "abs,Y"),
        0xCD => ("CMP", "abs"),
        0xDD => ("CMP", "abs,X"),
        0xD9 => ("CMP", "abs,Y"),
        0xED => ("SBC", "abs"),
        0xFD => ("SBC", "abs,X"),
        0xF9 => ("SBC", "abs,Y"),
        0xAE => ("LDX", "abs"),
        0xBE => ("LDX", "abs,Y"),
        0xAC => ("LDY", "abs"),
        0xBC => ("LDY", "abs,X"),
        0x8E => ("STX", "abs"),
        0x8C => ("STY", "abs"),
        0xEE => ("INC", "abs"),
        0xFE => ("INC", "abs,X"),
        0xCE => ("DEC", "abs"),
        0xDE => ("DEC", "abs,X"),
        0x2C => ("BIT", "abs"),
        0x0E => ("ASL", "abs"),
        0x1E => ("ASL", "abs,X"),
        0x4E => ("LSR", "abs"),
        0x5E => ("LSR", "abs,X"),
        0x2E => ("ROL", "abs"),
        0x3E => ("ROL", "abs,X"),
        0x6E => ("ROR", "abs"),
        0x7E => ("ROR", "abs,X"),
        0xEC => ("CPX", "abs"),
        0xCC => ("CPY", "abs"),
        _ => return None,
    })
}

/// Every absolute-addressed instruction whose operand lands in
/// `[base, base + span)`, as `(pc, opcode, mode, offset_into_table)`.
fn xrefs(img: &[u8], load: u16, base: u16, span: u16) -> Vec<(u16, u8, &'static str, u16)> {
    let mut out = Vec::new();
    if base == 0 {
        return out;
    }
    for k in 0..img.len().saturating_sub(2) {
        let Some((_, mode)) = abs_opcode(img[k]) else {
            continue;
        };
        let addr = u16::from_le_bytes([img[k + 1], img[k + 2]]);
        if addr >= base && addr < base.saturating_add(span) {
            out.push((load.wrapping_add(k as u16), img[k], mode, addr - base));
        }
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn main() {
    let only = std::env::args().nth(1);
    let root = std::env::current_dir().unwrap();
    // The probe is run from the workspace root or from a member directory.
    let songs = root
        .ancestors()
        .flat_map(|a| [a.join(SONGS), a.join(SONGS.trim_start_matches("xmrs/"))])
        .find(|p| p.is_dir())
        .expect("cannot find the bundled songs directory");

    // Which table to cross-reference, and how far into it to look. The window
    // has to cover a whole record for the instrument table (16 bytes) but stay
    // narrow for the pointer tables, which sit back-to-back with their
    // neighbours.
    // (label, where the table starts in the truth record, bytes to scan).
    type Target = (&'static str, fn(&Truth) -> u16, u16);
    let targets: &[Target] = &[
        ("instr", |t| t.instr, 16),
        ("fx_v1", |t| t.fx_v1, 8),
        ("fx_v2", |t| t.fx_v2, 8),
        ("filter", |t| t.filter_table, 8),
        ("arp3", |t| t.arp3_table, 4),
        ("song_list", |t| t.song_list, 2),
        ("patt_ptl", |t| t.patt_ptl, 1),
        ("patt_pth", |t| t.patt_pth, 1),
    ];

    // Signature census: byte pattern → the tunes that show it.
    let mut census: BTreeMap<(String, String), Vec<&str>> = BTreeMap::new();

    for t in TRUTH {
        let data = std::fs::read(songs.join(t.file)).unwrap_or_else(|e| panic!("{}: {e}", t.file));
        let img = &data[126..];
        let header_load = u16::from_le_bytes([data[8], data[9]]);
        let embedded = u16::from_le_bytes([data[124], data[125]]);
        println!(
            "\n=== {} (v{}) load {:04x}  header_load {:04x}  embedded {:04x}  {} bytes ===",
            t.name,
            t.version,
            t.load,
            header_load,
            embedded,
            img.len()
        );
        if embedded != t.load {
            println!("  !! embedded load address disagrees with the truth table");
        }

        for (label, get, span) in targets {
            if only.as_deref().is_some_and(|o| o != *label) {
                continue;
            }
            let base = get(t);
            if base == 0 {
                continue;
            }
            let hits = xrefs(img, t.load, base, *span);
            println!("  {label} @ {base:04x}  ({} xrefs)", hits.len());
            for (pc, op, mode, off) in &hits {
                let k = (pc.wrapping_sub(t.load)) as usize;
                let before = &img[k.saturating_sub(4)..k];
                let after = &img[(k + 3).min(img.len())..(k + 14).min(img.len())];
                let (mn, _) = abs_opcode(*op).unwrap();
                println!(
                    "    {pc:04x}  {mn} {mode:<5} +{off:<2}   [{}] {:02x} .. .. [{}]",
                    hex(before),
                    op,
                    hex(after)
                );
                // Census key: what follows the load, which is what a detector
                // would key on.
                census
                    .entry((
                        format!("{label}+{off}"),
                        format!("{mn} {mode} -> {}", hex(&after[..3.min(after.len())])),
                    ))
                    .or_default()
                    .push(t.name);
            }
        }
    }

    println!("\n\n=== census: patterns shared across tunes ===");
    for ((what, pat), tunes) in &census {
        if tunes.len() >= 3 {
            println!(
                "  {:<14} {:<34} {:2} tunes: {}",
                what,
                pat,
                tunes.len(),
                tunes.join(",")
            );
        }
    }

    version_matrix(&songs);
    play_entry(&songs);
    note_fetch(&songs);
    detect_vs_truth(&songs);
}

/// Dump the head of each tune's `play` routine, where the row cadence lives.
///
/// `resetspd` is the value a per-frame counter reloads with, so it is reachable
/// only through the code: find the reload store, then read the byte it takes.
fn play_entry(songs: &std::path::Path) {
    println!("\n\n=== play entry (row cadence) ===");
    for t in TRUTH {
        if t.name == "sanxion2" {
            continue;
        }
        let data = std::fs::read(songs.join(t.file)).unwrap();
        let img = &data[126..];
        let play = u16::from_be_bytes([data[12], data[13]]);
        let Some(k) = (play as usize).checked_sub(t.load as usize) else {
            println!("{:<11} play {play:04x} outside the image", t.name);
            continue;
        };
        println!(
            "\n{:<11} v{:<2} resetspd {}  play {play:04x}",
            t.name, t.version, t.resetspd
        );
        for row in (k..(k + 48).min(img.len())).step_by(8) {
            println!(
                "   {:04x}  {}",
                t.load as usize + row,
                hex(&img[row..(row + 8).min(img.len())])
            );
        }
    }
}

/// Dump each tune's note-fetch routine.
///
/// The pattern-pointer signature hands us the zero-page pointer the stream is
/// read through, so `LDA (zp),Y` (`B1 zp`) pinpoints every read. How many `INY`s
/// the portamento branch spends is the difference between the v10 and the v20
/// stream layouts — which is what the payload width, and thus the instrument
/// count, hangs on.
fn note_fetch(songs: &std::path::Path) {
    println!("\n\n=== note-fetch routines ===");
    for t in TRUTH {
        if t.name == "sanxion2" {
            continue; // same image as sanxion1
        }
        let data = std::fs::read(songs.join(t.file)).unwrap();
        let img = &data[126..];
        let Some(p) = (0..img.len() - 11).find(|&k| {
            img[k] == 0xA8
                && img[k + 1] == 0xB9
                && img[k + 4] == 0x85
                && img[k + 6] == 0xB9
                && img[k + 9] == 0x85
                && img[k + 10] == img[k + 5].wrapping_add(1)
        }) else {
            println!("{:<11} no pattern-pointer signature", t.name);
            continue;
        };
        let zp = img[p + 5];
        let reads: Vec<usize> = (0..img.len() - 1)
            .filter(|&k| img[k] == 0xB1 && img[k + 1] == zp)
            .collect();
        println!(
            "\n{:<11} v{:<2} zp ${zp:02x}  {} indirect reads at {}",
            t.name,
            t.version,
            reads.len(),
            reads
                .iter()
                .map(|k| format!("{:04x}", t.load as usize + k))
                .collect::<Vec<_>>()
                .join(" ")
        );
        if let Some(&first) = reads.first() {
            let from = first.saturating_sub(8);
            for row in (from..(first + 72).min(img.len())).step_by(8) {
                println!(
                    "   {:04x}  {}",
                    t.load as usize + row,
                    hex(&img[row..(row + 8).min(img.len())])
                );
            }
        }
    }
}

/// Run the detector on the fifteen known tunes and diff it field by field
/// against the hand-written table. Anything but a clean sheet here means
/// `detect()` is not yet safe to rely on.
fn detect_vs_truth(songs: &std::path::Path) {
    use xmrs::tracker::import::sid::detect::SidLayout;

    println!("\n\n=== detect() vs the hand-written table ===");
    println!(
        "{:<11} {:>9} {:>9} {:>7} {:>9} {:>9} {:>7} {:>9} {:>7} {:>9} {:>9} {:>5}",
        "tune",
        "song_list",
        "slq",
        "trk",
        "patt_ptl",
        "patt_pth",
        "pq",
        "instr",
        "iq",
        "fx_v1",
        "fx_v2",
        "porta"
    );
    let mut bad = 0;
    for t in TRUTH {
        let data = std::fs::read(songs.join(t.file)).unwrap();
        let Some(d) = SidLayout::detect(&data) else {
            println!("{:<11} NOT DETECTED", t.name);
            bad += 1;
            continue;
        };
        // `fx_v2` and `filter_table` are one and the same table; the hand
        // records split them, so accept either spelling.
        let want_fx2 = if t.fx_v2 != 0 {
            t.fx_v2
        } else {
            t.filter_table
        };
        let f = |ok: bool, v: String| if ok { v } else { format!("{v}!") };
        let n_bad = [
            d.load == t.load,
            d.song_list == t.song_list,
            d.song_list_qty == t.song_list_qty,
            d.song_track_qty == if t.name == "human_race" { 2 } else { 3 },
            d.patt_ptl == t.patt_ptl,
            d.patt_pth == t.patt_pth,
            d.patt_qty == t.patt_qty,
            d.instr == t.instr,
            d.instr_qty == t.instr_qty,
            d.fx_v1 == t.fx_v1,
            d.fx_v2 == want_fx2,
        ]
        .iter()
        .filter(|ok| !**ok)
        .count();
        bad += n_bad;
        println!(
            "{:<11} {:>9} {:>9} {:>7} {:>9} {:>9} {:>7} {:>9} {:>7} {:>9} {:>9} {:>5}",
            t.name,
            f(d.song_list == t.song_list, format!("{:04x}", d.song_list)),
            f(
                d.song_list_qty == t.song_list_qty,
                format!("{}", d.song_list_qty)
            ),
            d.song_track_qty,
            f(d.patt_ptl == t.patt_ptl, format!("{:04x}", d.patt_ptl)),
            f(d.patt_pth == t.patt_pth, format!("{:04x}", d.patt_pth)),
            f(d.patt_qty == t.patt_qty, format!("{}", d.patt_qty)),
            f(d.instr == t.instr, format!("{:04x}", d.instr)),
            f(d.instr_qty == t.instr_qty, format!("{}", d.instr_qty)),
            f(d.fx_v1 == t.fx_v1, format!("{:04x}", d.fx_v1)),
            f(d.fx_v2 == want_fx2, format!("{:04x}", d.fx_v2)),
            {
                let ka = if d.tempo_ka != 0 {
                    d.tempo_ka
                } else {
                    d.frame_skip_reset
                };
                format!(
                    "spd {} ka {:>3}(dual {:>3} skip {:>2}) -> {:?} (hand {:?}) katbl {:04x}",
                    d.resetspd,
                    ka,
                    d.tempo_ka,
                    d.frame_skip_reset,
                    SidLayout::tempo_frac(d.resetspd as u32, ka as u32),
                    t.tempo_frac,
                    d.tempo_ka_table
                ) + &format!(
                    " seed {:>2}(want {}) arp3 {:04x}(want {:04x}) rle {}(v{}) sky {}/{} b1note {} arp {}",
                    d.filter_seed,
                    t.filter_seed,
                    d.arp3_table,
                    t.arp3_table,
                    d.order_compressed,
                    t.version,
                    d.skydive_when,
                    d.skydive_add,

                    d.interp_vibrato,


                    d.arp_semitones
                )
            },
        );
    }
    println!("\n  expected (hand-written table):");
    for t in TRUTH {
        println!(
            "{:<11} {:>9} {:>9} {:>7} {:>9} {:>9} {:>7} {:>9} {:>7} {:>9} {:>9}",
            t.name,
            format!("{:04x}", t.song_list),
            t.song_list_qty,
            if t.name == "human_race" { 2 } else { 3 },
            format!("{:04x}", t.patt_ptl),
            format!("{:04x}", t.patt_pth),
            t.patt_qty,
            format!("{:04x}", t.instr),
            t.instr_qty,
            format!("{:04x}", t.fx_v1),
            format!(
                "{:04x}",
                if t.fx_v2 != 0 {
                    t.fx_v2
                } else {
                    t.filter_table
                }
            ),
        );
    }
    println!("\n  {bad} field(s) wrong across {} tunes", TRUTH.len());

    println!("\n  per-sub-song row cadence, where a table was found:");
    for t in TRUTH {
        let data = std::fs::read(songs.join(t.file)).unwrap();
        let Some(d) = SidLayout::detect(&data) else {
            continue;
        };
        if d.resetspd_table == 0 {
            continue;
        }
        let base = 126 + d.resetspd_table as usize - d.load as usize;
        let vals: Vec<u8> = (0..d.song_list_qty)
            .filter_map(|i| data.get(base + i).copied())
            .collect();
        println!(
            "    {:<11} table {:04x} -> {:?}   (hand: {:?})",
            t.name, d.resetspd_table, vals, t.resetspd_songs
        );
    }

    println!("\n  instrument usage where the count disagrees (index×uses):");
    for t in TRUTH
        .iter()
        .filter(|t| ["thing", "zoids", "thrust"].contains(&t.name))
    {
        let data = std::fs::read(songs.join(t.file)).unwrap();
        let d = SidLayout::detect(&data).unwrap();
        let h = d.instrument_histogram(&data);
        let used: Vec<String> = h
            .iter()
            .enumerate()
            .filter(|(_, &n)| n > 0)
            .map(|(i, n)| format!("{i}×{n}"))
            .collect();
        println!(
            "    {:<11} hand {:>2}, detected {:>2}: {}",
            t.name,
            t.instr_qty,
            d.instr_qty,
            used.join(" ")
        );
    }

    // Where does The Human Race's apparent portamento come from? Walking its 79
    // detected patterns, the one- and two-byte widths reach different highest
    // instruments (22 vs 60) — which reads like "this tune has portamento
    // entries and the width matters". Restricting the walk to the 58 patterns
    // the hand record keeps shows the difference lives entirely in the 21
    // patterns beyond, which no sub-song order list reaches.
    {
        let data = std::fs::read(songs.join("human_race.sid")).unwrap();
        let mut d = SidLayout::detect(&data).unwrap();
        let all = d.walk_stats(&data);
        d.patt_qty = 58;
        let kept = d.walk_stats(&data);
        println!(
            "\n  human_race highest instrument by payload width (1|2 bytes):\n    \
             79 patterns: {}|{}\n    58 patterns: {}|{}",
            all[0].1, all[1].1, kept[0].1, kept[1].1
        );
    }

    // The Human Race is the only two-voice tune and the only one whose sub-song
    // count is disputed; dump the raw pointer table so the extra entries can be
    // judged rather than guessed at.
    let data = std::fs::read(songs.join("human_race.sid")).unwrap();
    let d = SidLayout::detect(&data).unwrap();
    let base = 126 + d.song_list as usize - d.load as usize;
    println!(
        "\n  human_race sub-song pointer table @ {:04x}:",
        d.song_list
    );
    for s in 0..(d.patt_ptl - d.song_list) as usize / 4 {
        let e = &data[base + s * 4..base + s * 4 + 4];
        println!(
            "    song {s}: voice0 {:04x}  voice1 {:04x}",
            u16::from_le_bytes([e[0], e[2]]),
            u16::from_le_bytes([e[1], e[3]])
        );
    }
}

/// Presence matrix of candidate version discriminators.
///
/// Every entry is a byte sequence with `..` wildcards for the operand bytes.
/// A row that splits the fifteen tunes exactly along the v10 / v15 / v20 / v25 /
/// v30 lines is a usable `version` signal; one that cuts across them is not.
fn version_matrix(songs: &std::path::Path) {
    // `None` = wildcard.
    type Sig = (&'static str, &'static [Option<u8>]);
    const W: Option<u8> = None;
    let sigs: &[Sig] = &[
        (
            "wave_tab   B9 .. .. 9D00D4",
            &[Some(0xB9), W, W, Some(0x9D), Some(0x00), Some(0xD4)],
        ),
        (
            "wave_tabY  B9 .. .. 99 00D4",
            &[Some(0xB9), W, W, Some(0x99), Some(0x00), Some(0xD4)],
        ),
        (
            "filter     B9 .. .. 8D17D4",
            &[Some(0xB9), W, W, Some(0x8D), Some(0x17), Some(0xD4)],
        ),
        (
            "filterY    B9 .. .. 99 17D4",
            &[Some(0xB9), W, W, Some(0x99), Some(0x17), Some(0xD4)],
        ),
        (
            "res_nib    B9 .. .. 29 0F",
            &[Some(0xB9), W, W, Some(0x29), Some(0x0F)],
        ),
        ("cutoff     8D 16 D4", &[Some(0x8D), Some(0x16), Some(0xD4)]),
        ("d418       8D 18 D4", &[Some(0x8D), Some(0x18), Some(0xD4)]),
        (
            "pw_seedX   BD .. .. 9902D4",
            &[Some(0xBD), W, W, Some(0x99), Some(0x02), Some(0xD4)],
        ),
        (
            "pw_seedY   B9 .. .. 9D02D4",
            &[Some(0xB9), W, W, Some(0x9D), Some(0x02), Some(0xD4)],
        ),
        ("skipframe  C6 .. 10", &[Some(0xC6), W, Some(0x10)]),
        ("skipframeA CE .. .. 10", &[Some(0xCE), W, W, Some(0x10)]),
        (
            "adsr_zeroX A9 00 9D05D4",
            &[Some(0xA9), Some(0x00), Some(0x9D), Some(0x05), Some(0xD4)],
        ),
        (
            "adsr_zeroY A9 00 9905D4",
            &[Some(0xA9), Some(0x00), Some(0x99), Some(0x05), Some(0xD4)],
        ),
        // The fx-mask bit-2 arpeggio: v10 jumps an octave up, the v15/Simple
        // family drops a fourth. `CLC : ADC #$0C` against `SEC : SBC #$05`.
        ("arp_up12   18 69 0C", &[Some(0x18), Some(0x69), Some(0x0C)]),
        ("arp_down5  38 E9 05", &[Some(0x38), Some(0xE9), Some(0x05)]),
    ];

    println!("\n\n=== version discriminator matrix ===");
    print!("{:<12}", "tune");
    for (name, _) in sigs {
        print!(" {:>3}", &name[..3]);
    }
    println!("   (legend below)");
    for t in TRUTH {
        let data = std::fs::read(songs.join(t.file)).unwrap();
        let img = &data[126..];
        print!("{:<9} v{:<2}", t.name, t.version);
        for (_, pat) in sigs {
            let n = (0..img.len().saturating_sub(pat.len()))
                .filter(|&k| {
                    pat.iter()
                        .enumerate()
                        .all(|(i, want)| want.is_none_or(|w| img[k + i] == w))
                })
                .count();
            print!(" {n:>3}");
        }
        println!();
    }
    println!();
    for (name, _) in sigs {
        println!("  {}", name);
    }
}
