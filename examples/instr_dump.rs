//! Dump per-instrument volume settings: volume envelope
//! (enabled/sustain/points), fadeout, and each sample's default volume.
//! Used to chase per-instrument level divergences.
//! Usage: instr_dump <module> [instr_index]
use xmrs::core::instrument::InstrumentType;
use xmrs::core::module::Module;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let bytes = std::fs::read(&a[1]).unwrap();
    let module = Module::load(&bytes).unwrap();
    let only: Option<usize> = a.get(2).and_then(|s| s.parse().ok());
    for (i, instr) in module.instrument.iter().enumerate() {
        if let Some(o) = only {
            if o != i {
                continue;
            }
        }
        let name = instr.name.trim();
        if let InstrumentType::Default(d) = &instr.instr_type {
            let ve = &d.voice.volume_envelope;
            println!(
                "instr#{i:3} \"{name}\"  fadeout={:?}  volenv enabled={} sustain={}",
                d.voice.volume_fadeout, ve.enabled, ve.sustain_enabled,
            );
            if ve.enabled {
                let pts: Vec<String> = ve
                    .point
                    .iter()
                    .map(|p| format!("({},{:?})", p.frame, p.value))
                    .collect();
                println!(
                    "        points: {}  sustain=[{},{}]",
                    pts.join(" "),
                    ve.sustain_start_point,
                    ve.sustain_end_point
                );
            }
            for (si, s) in d.sample.iter().enumerate() {
                if let Some(s) = s {
                    println!(
                        "        sample#{si} vol={:?} finetune={:?} rel_pitch={} loop={:?} len={}",
                        s.volume,
                        s.finetune,
                        s.relative_pitch,
                        s.loop_flag,
                        s.len()
                    );
                }
            }
        }
    }
}
