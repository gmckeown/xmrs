//! Dump GlobalVolume automation lanes (Points + Slide) for any module.
//! Used to diagnose "level grows toward the end" fidelity outliers:
//! is the song asking for a global-volume change we drop or mis-apply?
use xmrs::core::daw::automation::{AutomationTarget, LaneKind, LfoEvent, SlideEvent};
use xmrs::core::module::Module;

fn main() {
    let path = std::env::args().nth(1).expect("usage: gvol_census <file>");
    let bytes = std::fs::read(&path).expect("read");
    let module = match Module::load(&bytes) {
        Ok(m) => m,
        Err(e) => {
            println!("LOAD ERROR: {:?}", e);
            return;
        }
    };
    println!(
        "name={:?} channels={}",
        module.name,
        module.get_num_channels()
    );
    println!("total automation lanes: {}", module.automation.len());
    // histogram of lane targets (collapse per-track index)
    let mut hist: std::collections::BTreeMap<String, usize> = Default::default();
    for lane in &module.automation {
        let key = match &lane.target {
            AutomationTarget::GlobalVolume => "GlobalVolume".to_string(),
            t => format!("{:?}", t).split('(').next().unwrap().to_string(),
        };
        *hist.entry(key).or_default() += 1;
    }
    for (k, v) in &hist {
        println!("  target {k}: {v}");
    }
    // census of every target
    let mut gv = 0usize;
    for lane in &module.automation {
        if matches!(lane.target, AutomationTarget::GlobalVolume) {
            gv += 1;
        }
    }
    println!("GlobalVolume lanes: {}", gv);
    for (i, lane) in module.automation.iter().enumerate() {
        if !matches!(lane.target, AutomationTarget::GlobalVolume) {
            continue;
        }
        println!(
            "--- lane #{i} target=GlobalVolume song={} enabled={} ---",
            lane.song, lane.enabled
        );
        match &lane.kind {
            LaneKind::Points(pts) => {
                println!("  Points ({}):", pts.len());
                for p in pts {
                    println!("    tick={:>7} value={:?}", p.tick, p.value);
                }
            }
            LaneKind::Slide { events } => {
                println!("  Slide ({}):", events.len());
                for e in events {
                    match e {
                        SlideEvent::Set { tick, rate, fine } => {
                            println!("    tick={:>7} Set rate={:?} fine={}", tick, rate, fine)
                        }
                        SlideEvent::Clear { tick } => println!("    tick={:>7} Clear", tick),
                    }
                }
            }
            LaneKind::Lfo { events } => {
                println!(
                    "  Lfo ({}): first/last tick {:?}/{:?}",
                    events.len(),
                    events.first().map(|e| e.tick()),
                    events.last().map(LfoEvent::tick)
                );
            }
            LaneKind::Glide { events } => println!("  Glide ({})", events.len()),
        }
    }
}
