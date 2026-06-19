//! Standalone repair tool for damaged Impulse Tracker (`.it`) modules.
//!
//! It works **purely on the file bytes** and only uses the public
//! `ItModule::load` to *verify* the result — it does not touch the rest of
//! the crate, so it can never mask a genuine import bug.
//!
//! What it repairs
//! ---------------
//! The offset tables of an `.it` (instrument / sample / pattern pointers) are
//! the spine of the format. When a file is mechanically damaged — a bad block,
//! a truncated copy — the pointers usually survive but the bytes they point at
//! get partially zeroed. The most common, and most fatal, casualty is the
//! 4-byte header magic (`IMPI` for instruments, `IMPS` for samples): lose it
//! and a strict loader rejects the whole module, even though every other field
//! (envelopes, note map, sample length / loop / data pointer) is still intact.
//!
//! This tool detects each such defect and applies the *minimal* fix:
//!   * instrument header with a valid in-range offset but wrong magic → restore
//!     `IMPI` (lossless: the rest of the header is reused as-is);
//!   * sample header likewise → restore `IMPS`; additionally, if the sample
//!     claims PCM data (`flag bit0`) but its data pointer/length runs past EOF,
//!     neutralise it (clear the flag, zero the length) so the player can't read
//!     out of bounds;
//!   * pattern pointer that is non-zero but points out of range / declares a
//!     length overrunning the file → reset to 0 (an empty pattern).
//!
//! Anything it cannot safely repair (e.g. an instrument whose *offset* itself
//! is out of range) is reported and left untouched.
//!
//! Usage
//! -----
//! ```text
//! cargo run --example it_repair --features "std import" -- <file.it>           # scan + report
//! cargo run --example it_repair --features "std import" -- <file.it> --write    # write <file>.repaired.it
//! cargo run --example it_repair --features "std import" -- <file.it> --write -o out.it
//! ```

use std::path::PathBuf;

use xmrs::tracker::import::it::it_module::ItModule;

const IT_HEADER_SIZE: usize = 192;
const INSTRUMENT_MAGIC: &[u8; 4] = b"IMPI";
const SAMPLE_MAGIC: &[u8; 4] = b"IMPS";

/// IT sample-header field offsets (relative to the header start).
const SMP_FLAG: usize = 0x12; // bit0 = sample present, bit1 = 16-bit
const SMP_LENGTH: usize = 0x30; // u32, in sample points
const SMP_DATA_PTR: usize = 0x48; // u32, absolute file offset of PCM

fn rd_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn rd_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

#[derive(Default)]
struct Report {
    fixes: Vec<String>,
    unrepairable: Vec<String>,
}

impl Report {
    fn fix(&mut self, msg: impl Into<String>) {
        self.fixes.push(msg.into());
    }
    fn fail(&mut self, msg: impl Into<String>) {
        self.unrepairable.push(msg.into());
    }
}

/// Read the four offset tables. Returns `(ins, smp, pat)` absolute offsets, or
/// `None` if the header / tables themselves are too short to trust.
fn offset_tables(data: &[u8]) -> Option<(Vec<u32>, Vec<u32>, Vec<u32>)> {
    if data.len() < IT_HEADER_SIZE || &data[0..4] != b"IMPM" {
        return None;
    }
    let ordn = rd_u16(data, 0x20) as usize;
    let insn = rd_u16(data, 0x22) as usize;
    let smpn = rd_u16(data, 0x24) as usize;
    let patn = rd_u16(data, 0x26) as usize;

    let mut cur = IT_HEADER_SIZE + ordn; // skip orders
    let need = cur + 4 * (insn + smpn + patn);
    if need > data.len() {
        return None;
    }
    let take = |start: usize, n: usize| -> Vec<u32> {
        (0..n).map(|i| rd_u32(data, start + 4 * i)).collect()
    };
    let ins = take(cur, insn);
    cur += 4 * insn;
    let smp = take(cur, smpn);
    cur += 4 * smpn;
    let pat = take(cur, patn);
    Some((ins, smp, pat))
}

/// Apply in-place repairs to `data`, recording every action in `rep`.
fn repair(data: &mut [u8], rep: &mut Report) {
    let Some((ins, smp, pat)) = offset_tables(data) else {
        rep.fail("IT header or offset tables too short / wrong magic — cannot parse spine");
        return;
    };
    let len = data.len();

    // --- Instruments: restore IMPI where the slot is reachable -------------
    for (i, &o) in ins.iter().enumerate() {
        let o = o as usize;
        if o == 0 {
            continue; // no instrument in this slot
        }
        if o + 4 > len {
            rep.fail(format!(
                "instrument #{i}: offset {o} is out of range (EOF={len}) — left untouched"
            ));
            continue;
        }
        if &data[o..o + 4] != INSTRUMENT_MAGIC {
            let was = [data[o], data[o + 1], data[o + 2], data[o + 3]];
            data[o..o + 4].copy_from_slice(INSTRUMENT_MAGIC);
            rep.fix(format!(
                "instrument #{i} @ {o}: restored 'IMPI' magic (was {was:02x?})"
            ));
        }
    }

    // --- Samples: restore IMPS, neutralise dangling PCM pointers -----------
    for (i, &o) in smp.iter().enumerate() {
        let o = o as usize;
        if o == 0 {
            continue;
        }
        if o + 0x50 > len {
            rep.fail(format!(
                "sample #{i}: header offset {o} is out of range (EOF={len}) — left untouched"
            ));
            continue;
        }
        if &data[o..o + 4] != SAMPLE_MAGIC {
            data[o..o + 4].copy_from_slice(SAMPLE_MAGIC);
            rep.fix(format!("sample #{i} @ {o}: restored 'IMPS' magic"));
        }
        // Guard the PCM pointer regardless of whether the magic was damaged:
        // a sample whose data can't be read would let the strict loader read
        // out of bounds. But we must be precise about *what* is unreadable:
        //
        //   * the data pointer landing past EOF is always fatal;
        //   * an over-long nominal length is fatal ONLY for *uncompressed*
        //     PCM, whose on-disk size is exactly `length × bytes/point`.
        //     IT214/215 *compressed* samples (flag bit3) are variable-length
        //     and self-terminating — their nominal length says nothing about
        //     the bytes on disk, so range-checking them produces false
        //     positives and would destroy perfectly good audio.
        let flag = data[o + SMP_FLAG];
        let associated = flag & 0x01 != 0;
        let bits16 = flag & 0x02 != 0;
        let stereo = flag & 0x04 != 0;
        let compressed = flag & 0x08 != 0;
        let length = rd_u32(data, o + SMP_LENGTH) as usize;
        let ptr = rd_u32(data, o + SMP_DATA_PTR) as usize;
        if associated && length > 0 {
            let ptr_bad = ptr == 0 || ptr >= len;
            let bytes = length * if bits16 { 2 } else { 1 } * if stereo { 2 } else { 1 };
            let overrun = !compressed && ptr.saturating_add(bytes) > len;
            if ptr_bad || overrun {
                data[o + SMP_FLAG] = flag & !0x01;
                data[o + SMP_LENGTH..o + SMP_LENGTH + 4].copy_from_slice(&0u32.to_le_bytes());
                let why = if ptr_bad {
                    format!("pointer {ptr} out of range")
                } else {
                    format!("uncompressed PCM overruns EOF (ptr={ptr}, +{bytes}B > {len})")
                };
                rep.fix(format!("sample #{i} @ {o}: {why} — emptied"));
            }
        }
    }

    // --- Patterns: reset unreachable / overrunning pointers to empty -------
    for (i, &o) in pat.iter().enumerate() {
        let o = o as usize;
        if o == 0 {
            continue; // empty pattern, legal
        }
        let bad = o + 8 > len || {
            let packed = rd_u16(data, o) as usize; // packed data length
            o + 8 + packed > len
        };
        if bad {
            // Find this pattern's slot in the offset table and zero it.
            if let Some((_, _, _)) = offset_tables(data) {
                // recompute the byte position of pat[i] in the file
                let ordn = rd_u16(data, 0x20) as usize;
                let insn = rd_u16(data, 0x22) as usize;
                let smpn = rd_u16(data, 0x24) as usize;
                let pos = IT_HEADER_SIZE + ordn + 4 * insn + 4 * smpn + 4 * i;
                data[pos..pos + 4].copy_from_slice(&0u32.to_le_bytes());
                rep.fix(format!(
                    "pattern #{i}: offset {o} unreachable/overruns file — reset to empty"
                ));
            }
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut input: Option<PathBuf> = None;
    let mut write = false;
    let mut out: Option<PathBuf> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--write" | "-w" => write = true,
            "-o" | "--out" => out = args.next().map(PathBuf::from),
            other => input = Some(PathBuf::from(other)),
        }
    }
    let Some(input) = input else {
        eprintln!(
            "usage: it_repair <file.it> [--write] [-o out.it]\n\
             scans an .it for damaged headers; with --write, saves a repaired copy."
        );
        std::process::exit(2);
    };

    let original = std::fs::read(&input).expect("read input");
    println!("file: {} ({} bytes)", input.display(), original.len());

    // Before/after load to prove the repair actually changes the verdict.
    let before = ItModule::load(&original).err();
    if let Some(e) = &before {
        println!("BEFORE: load rejected — {e:?}");
    } else {
        println!("BEFORE: loads cleanly (no repair needed for strict loader)");
    }

    let mut data = original.clone();
    let mut rep = Report::default();
    repair(&mut data, &mut rep);

    println!("\n--- repairs applied ({}) ---", rep.fixes.len());
    for f in &rep.fixes {
        println!("  ✓ {f}");
    }
    if !rep.unrepairable.is_empty() {
        println!("\n--- could NOT repair ({}) ---", rep.unrepairable.len());
        for f in &rep.unrepairable {
            println!("  ✗ {f}");
        }
    }

    // Verify the repaired bytes through the real importer.
    print!("\nAFTER:  ");
    match ItModule::load(&data) {
        Ok(it) => {
            let m = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| it.to_module()));
            match m {
                Ok(module) => println!(
                    "loads cleanly — {} channels, {} instruments",
                    module.get_num_channels(),
                    module.instrument.len()
                ),
                Err(_) => println!("header loads but to_module() still panics"),
            }
        }
        Err(e) => println!("still rejected — {e:?}"),
    }

    if write && !rep.fixes.is_empty() {
        let out = out.unwrap_or_else(|| {
            let mut p = input.clone();
            let stem = p.file_stem().unwrap().to_string_lossy().into_owned();
            p.set_file_name(format!("{stem}.repaired.it"));
            p
        });
        std::fs::write(&out, &data).expect("write output");
        println!("\nwrote repaired file: {}", out.display());
    } else if write {
        println!("\nnothing to write (no repairs applied).");
    } else if !rep.fixes.is_empty() {
        println!("\n(dry run — pass --write to save a repaired copy)");
    }
}
