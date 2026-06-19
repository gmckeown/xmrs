//! Per-module feature census → TSV line. Joined with the fidelity
//! sweep (env_corr) to find which feature cluster drives the
//! structural-divergence tail.
use xmrs::core::daw::automation::AutomationTarget;
use xmrs::core::instrument::InstrumentType;
use xmrs::core::instrument_behavior::{DuplicateCheckAction, DuplicateCheckType, NewNoteAction};
use xmrs::core::module::Module;

fn nna_of(d: &DuplicateCheckType) -> NewNoteAction {
    let act = |a: &DuplicateCheckAction| match a {
        DuplicateCheckAction::NoteCut(n)
        | DuplicateCheckAction::NoteOff(n)
        | DuplicateCheckAction::NoteFadeOut(n) => *n,
    };
    match d {
        DuplicateCheckType::Off(n) => *n,
        DuplicateCheckType::Note(a)
        | DuplicateCheckType::Sample(a)
        | DuplicateCheckType::Instrument(a) => act(a),
    }
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: feature_census <file>");
    let bytes = std::fs::read(&path).expect("read");
    let module = match Module::load(&bytes) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("LOAD ERROR {}: {:?}", path, e);
            return;
        }
    };
    let fname = std::path::Path::new(&path)
        .file_name()
        .unwrap()
        .to_string_lossy();

    let mut n_instr = 0;
    let mut nna_noncut = 0; // NNA = Continue/Off/Fade
    let mut dct_on = 0; // duplicate check active
    let mut filter = 0; // cutoff/resonance/pitch-as-lowpass
    let mut filt_env = 0; // pitch envelope used as low-pass
    let mut autovib = 0; // instrument auto-vibrato
    let mut volenv = 0;

    for instr in &module.instrument {
        if let InstrumentType::Default(d) = &instr.instr_type {
            n_instr += 1;
            if !matches!(nna_of(&d.behavior.duplicate_check), NewNoteAction::NoteCut) {
                nna_noncut += 1;
            }
            if !matches!(d.behavior.duplicate_check, DuplicateCheckType::Off(_)) {
                dct_on += 1;
            }
            let v = &d.voice;
            if v.initial_filter_cutoff != 0 || v.initial_filter_resonance != 0 {
                filter += 1;
            }
            if v.pitch_envelope_as_low_pass_filter {
                filt_env += 1;
            }
            if v.vibrato.depth != xmrs::core::fixed::units::PitchDelta::ZERO {
                autovib += 1;
            }
            if v.volume_envelope.enabled {
                volenv += 1;
            }
        }
    }

    // lane targets of interest
    let mut gvol = 0;
    let mut devparam = 0; // Zxx → filter macro → DeviceParam
    let mut chvol = 0;
    for lane in &module.automation {
        match &lane.target {
            AutomationTarget::GlobalVolume => gvol += 1,
            AutomationTarget::DeviceParam { .. } => devparam += 1,
            AutomationTarget::ChannelVolume(_) | AutomationTarget::TrackChannelVolume(_) => {
                chvol += 1
            }
            _ => {}
        }
    }

    // TSV: file, chans, instr, nna_noncut, dct, filter, filt_env, autovib, volenv, gvol, devparam, chvol
    println!(
        "{fname}\t{}\t{n_instr}\t{nna_noncut}\t{dct_on}\t{filter}\t{filt_env}\t{autovib}\t{volenv}\t{gvol}\t{devparam}\t{chvol}",
        module.get_num_channels()
    );
}
