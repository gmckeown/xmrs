//! Dump the Speed and Bpm automation-lane Points of any module (the
//! per-tick tempo map), for diagnosing timing / tempo-command fidelity.
//! Usage: speedlane <module>
use xmrs::core::daw::automation::{AutomationTarget, LaneEvent};
use xmrs::core::module::Module;

fn main() {
    let b = std::fs::read(std::env::args().nth(1).unwrap()).unwrap();
    let m = Module::load(&b).unwrap();
    for tgt in [AutomationTarget::Speed, AutomationTarget::Bpm] {
        for lane in m.lanes_for(tgt, 0) {
            println!("=== lane {:?} ===", tgt);
            for ev in lane.events_in(0, u32::MAX) {
                if let LaneEvent::Point(p) = ev {
                    println!("  tick={} val={:?}", p.tick, p.value);
                }
            }
        }
    }
}
