//! Dump non-empty cells (event + effects) for one channel across given
//! patterns, to diagnose per-channel state effects (channel-volume,
//! tremolo, slides) that persist in continuous playback.
//! Usage: celldump <module> <channel_0based> <pat> [pat...]
use xmrs::core::module::Module;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let b = std::fs::read(&args[1]).unwrap();
    let m = Module::load(&b).unwrap();
    let ch: usize = args[2].parse().unwrap();
    let pats: Vec<usize> = args[3..].iter().map(|s| s.parse().unwrap()).collect();
    for &pat in &pats {
        let rows = m.pattern_row_count(pat);
        println!("=== pattern {} ({} rows) channel {} ===", pat, rows, ch);
        for row in 0..rows {
            let cells = m.row_at(0, pat, row);
            if let Some((cell, instr)) = cells.get(ch) {
                let has_evt = !matches!(cell.event, xmrs::core::cell::CellEvent::None);
                if has_evt || !cell.effects.is_empty() {
                    println!(
                        "  r{:3}  evt={:?}  instr={:?}  fx={:?}",
                        row, cell.event, instr, cell.effects
                    );
                }
            }
        }
    }
}
