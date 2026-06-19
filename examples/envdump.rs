//! Dump volume-envelope nodes + sample loop type for instruments.
//! Usage: envdump <module> [instr...]
use xmrs::core::instrument::InstrumentType;
use xmrs::core::module::Module;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let m = Module::load(&std::fs::read(&a[1]).unwrap()).unwrap();
    let which: Vec<usize> = a[2..].iter().map(|s| s.parse().unwrap()).collect();
    for (i, ins) in m.instrument.iter().enumerate() {
        if !which.is_empty() && !which.contains(&i) {
            continue;
        }
        if let InstrumentType::Default(d) = &ins.instr_type {
            let e = &d.voice.volume_envelope;
            let pts: Vec<String> = e
                .point
                .iter()
                .map(|p| format!("({},{:?})", p.frame, p.value))
                .collect();
            let loops: Vec<String> = d
                .sample
                .iter()
                .flatten()
                .map(|s| format!("{:?}", s.loop_flag))
                .collect();
            let lastv = e.point.last().map(|p| p.value).unwrap_or_default();
            println!(
                "LASTVAL instr {:3} last={:?} npts={} loop_on={} sus_on={}",
                i,
                lastv,
                e.point.len(),
                e.loop_enabled,
                e.sustain_enabled
            );
            println!("instr {:3} venv_on={} sus_on={} loop_on={} npts={}\n   pts={:?}\n   sample_loops={:?}",
                i, e.enabled, e.sustain_enabled, e.loop_enabled, e.point.len(), pts, loops);
        }
    }
}
