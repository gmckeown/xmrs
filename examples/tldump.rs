//! Dump the built TimelineMap entries (song 0): order/pattern/row, tick,
//! speed_at_row, loop_iter — for diagnosing pattern-delay / loop timing.
//! Repeated consecutive (pattern,row) pairs reveal SEx row-repeats (or the
//! S6x mis-expansion bug); a row whose speed_at_row != the running speed
//! reveals an S6x tick-delay. Usage: tldump <module> [from_order] [to_order]
use xmrs::core::module::Module;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let b = std::fs::read(&args[1]).unwrap();
    let m = Module::load(&b).unwrap();
    let from = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0u32);
    let to = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(u32::MAX);
    let mut prev_speed = 0u8;
    for e in m.timeline_map.entries.iter().filter(|e| e.song == 0) {
        if e.order_idx < from || e.order_idx > to {
            continue;
        }
        let mark = if e.speed_at_row != prev_speed {
            " <-SPD"
        } else {
            ""
        };
        println!(
            "ord={:3} pat={:3} row={:3} tick={:6} spd={:3} bpm={:3} iter={}{}",
            e.order_idx,
            e.pattern_idx,
            e.row_idx,
            e.tick,
            e.speed_at_row,
            e.bpm_at_row,
            e.loop_iter,
            mark
        );
        prev_speed = e.speed_at_row;
    }
}
