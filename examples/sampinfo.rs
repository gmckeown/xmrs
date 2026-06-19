//! Dump instrument global-volume + sample volume/length for an instrument.
//! Usage: sampinfo <module> <instr>
use xmrs::core::instrument::InstrumentType;
use xmrs::core::module::Module;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let m = Module::load(&std::fs::read(&a[1]).unwrap()).unwrap();
    let i: usize = a[2].parse().unwrap();
    if let InstrumentType::Default(d) = &m.instrument[i].instr_type {
        println!(
            "instr {} '{}' instr_global_vol={:?}",
            i,
            m.instrument[i].name.trim(),
            d.voice.volume
        );
        for (si, s) in d.sample.iter().enumerate() {
            if let Some(s) = s {
                println!(
                    "  sample {}: vol={:?} global_vol={:?} len={} loop={:?} finetune/relpitch={:?}",
                    si,
                    s.volume,
                    s.default_note_volume,
                    s.len(),
                    s.loop_flag,
                    s.relative_pitch
                );
            }
        }
    }
}
