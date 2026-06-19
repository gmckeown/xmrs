//! Dump per-instrument DCT/DCA/NNA + volume-fadeout + envelope, to
//! diagnose NNA voice accumulation. Usage: nnadump <module>
use xmrs::core::instrument::InstrumentType;
use xmrs::core::module::Module;
fn main() {
    let b = std::fs::read(std::env::args().nth(1).unwrap()).unwrap();
    let m = Module::load(&b).unwrap();
    for (i, ins) in m.instrument.iter().enumerate() {
        if let InstrumentType::Default(d) = &ins.instr_type {
            let nsamp = d.sample.iter().filter(|s| s.is_some()).count();
            let looping = d
                .sample
                .iter()
                .flatten()
                .any(|s| s.loop_flag != xmrs::core::sample::LoopType::No);
            println!(
                "instr {:3} '{}' dct={:?} fadeout={:?} venv_on={} sus={} samples={} loop={}",
                i,
                ins.name.trim(),
                d.behavior.duplicate_check,
                d.voice.volume_fadeout,
                d.voice.volume_envelope.enabled,
                d.voice.volume_envelope.sustain_enabled,
                nsamp,
                looping
            );
        }
    }
}
