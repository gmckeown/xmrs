//! Emit candidate Ghidra-oracle entry points for every `.dw` in a
//! directory, as TSV for the headless capture batch
//! (`oracle/capture_all.sh`).
//!
//! ```text
//! cargo run --release --features=demo --example dw_oracle_args -- [dir]
//! ```
//! (dir defaults to `$HOME/Music/dw`)
//!
//! Per module it prints:
//!   name <TAB> a3hex32 <TAB> lea_play <TAB> detect_play <TAB> detect_init
//! where
//! * `a3hex32` = `DwLayout::start_offset` as a 32-bit value (the A3
//!   base; `-` when 0). Required by the bubble-bobble family; harmless
//!   otherwise (the oracle sets A3 before each call; self-basing
//!   players overwrite it).
//! * `lea_play` = the per-frame driver entry found from the Paula
//!   `lea (0xdff000).L,A6` (`4D F9 00 DF F0 00`): scan back to the
//!   previous function terminator. This is the directly-callable body
//!   on the no-wrapper family.
//! * `detect_play` / `detect_init` = `DwLayout`'s own offsets (right for
//!   a few modules where the lea/wrapper heuristics miss).
//!
//! The batch tries candidate `(init,play)` pairs built from these plus
//! the universal `(0x0,0xe)` wrapper convention, and keeps the first
//! that yields a sane Paula trace.

use std::path::Path;
use xmrs::tracker::import::dw::detect;

/// `lea (0xdff000).L, A6` — the per-frame Paula loop's register setup.
fn find_paula_lea(p: &[u8]) -> Option<usize> {
    p.windows(6)
        .position(|w| w == [0x4D, 0xF9, 0x00, 0xDF, 0xF0, 0x00])
}

/// Word-aligned backward scan from `from` for the previous function
/// terminator (rts/rte/bra.w/jmp.L); the entry is just past it.
fn entry_before(p: &[u8], from: usize) -> Option<usize> {
    let mut i = from & !1;
    while i >= 2 {
        i -= 2;
        match [p[i], p[i + 1]] {
            [0x4E, 0x75] | [0x4E, 0x73] => return Some(i + 2), // rts / rte
            [0x60, 0x00] => return Some(i + 4),                // bra.w (+disp word)
            [0x4E, 0xF9] => return Some(i + 6),                // jmp (xxx).L
            _ => {}
        }
    }
    None
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| format!("{}/Music/dw", std::env::var("HOME").unwrap()));
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("read dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("dw"))
        .collect();
    entries.sort();

    for p in entries {
        let b = match std::fs::read(&p) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let name = Path::new(&p).file_stem().unwrap().to_string_lossy();
        let layout = detect::detect(&b);
        let a3 = layout
            .as_ref()
            .map(|l| l.start_offset)
            .filter(|&s| s != 0)
            .map(|s| format!("0x{:x}", (s as i64) & 0xffff_ffff))
            .unwrap_or_else(|| "-".into());
        let lea = find_paula_lea(&b)
            .and_then(|l| entry_before(&b, l))
            .map(|x| format!("0x{:x}", x))
            .unwrap_or_else(|| "-".into());
        let dp = layout
            .as_ref()
            .and_then(|l| l.play_offset)
            .map(|x| format!("0x{:x}", x))
            .unwrap_or_else(|| "-".into());
        let di = layout
            .as_ref()
            .map(|l| format!("0x{:x}", l.init_offset))
            .unwrap_or_else(|| "-".into());
        println!("{}\t{}\t{}\t{}\t{}", name, a3, lea, dp, di);
    }
}
