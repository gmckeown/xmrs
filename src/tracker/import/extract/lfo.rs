//! LFO event extraction for Vibrato / Tremolo / Panbrello classes.

use alloc::vec::Vec;

use crate::core::compatibility::PlaybackQuirks;
use crate::core::daw::automation::LfoEvent;
use crate::core::fixed::fixed::{Q15, Q8_8};
use crate::core::fixed::units::PitchDelta;
use crate::core::waveform::Waveform;
use crate::tracker::import::effect::TrackImportEffect;

use super::RowVisit;

/// Rolling LFO state across the row sequence.
#[derive(Debug, Clone, Copy)]
struct LfoState {
    active: bool,
    waveform: Waveform,
    retrig: bool,
}

impl Default for LfoState {
    fn default() -> Self {
        Self {
            active: false,
            waveform: Waveform::BipolarSine,
            retrig: true,
        }
    }
}

/// Per-row aggregated view of one LFO class. Each field is `None`
/// when the row doesn't carry the corresponding sub-effect.
#[derive(Debug, Clone, Copy, Default)]
struct LfoRowView {
    main: Option<(Q8_8, i16)>,
    speed_only: Option<Q8_8>,
    depth_only: Option<i16>,
    waveform: Option<(Waveform, bool)>,
}

fn vibrato_row_view(effects: &[TrackImportEffect]) -> LfoRowView {
    let mut main_speed = Q8_8::ZERO;
    let mut main_depth = PitchDelta::ZERO;
    let mut has_main = false;
    let mut speed_only: Option<Q8_8> = None;
    let mut depth_sum = PitchDelta::ZERO;
    let mut has_depth = false;
    let mut waveform: Option<(Waveform, bool)> = None;
    for fx in effects {
        match fx {
            TrackImportEffect::Vibrato(s, d) | TrackImportEffect::VibratoFine(s, d) => {
                main_speed = main_speed + *s;
                main_depth = main_depth + *d;
                has_main = true;
            }
            TrackImportEffect::VibratoSpeed(s) => {
                speed_only = Some(*s);
            }
            TrackImportEffect::VibratoDepth(d) | TrackImportEffect::VibratoDepthFxVol(d) => {
                depth_sum = depth_sum + *d;
                has_depth = true;
            }
            TrackImportEffect::VibratoWaveform(wf, retrig) => {
                waveform = Some((*wf, *retrig));
            }
            _ => {}
        }
    }
    LfoRowView {
        main: has_main.then_some((main_speed, main_depth.as_q8_8_i16())),
        speed_only,
        depth_only: has_depth.then_some(depth_sum.as_q8_8_i16()),
        waveform,
    }
}

fn tremolo_row_view(effects: &[TrackImportEffect]) -> LfoRowView {
    let mut speed = Q8_8::ZERO;
    let mut depth = Q15::ZERO;
    let mut has_main = false;
    let mut waveform: Option<(Waveform, bool)> = None;
    for fx in effects {
        match fx {
            TrackImportEffect::Tremolo(s, d) => {
                speed = speed + *s;
                depth = depth + *d;
                has_main = true;
            }
            TrackImportEffect::TremoloWaveform(wf, retrig) => {
                waveform = Some((*wf, *retrig));
            }
            _ => {}
        }
    }
    LfoRowView {
        main: has_main.then_some((speed, depth.raw())),
        speed_only: None,
        depth_only: None,
        waveform,
    }
}

fn panbrello_row_view(effects: &[TrackImportEffect]) -> LfoRowView {
    let mut speed = Q8_8::ZERO;
    let mut depth = Q15::ZERO;
    let mut has_main = false;
    let mut waveform: Option<(Waveform, bool)> = None;
    for fx in effects {
        match fx {
            TrackImportEffect::Panbrello(s, d) => {
                speed = speed + *s;
                depth = depth + *d;
                has_main = true;
            }
            TrackImportEffect::PanbrelloWaveform(wf, retrig) => {
                waveform = Some((*wf, *retrig));
            }
            _ => {}
        }
    }
    LfoRowView {
        main: has_main.then_some((speed, depth.raw())),
        speed_only: None,
        depth_only: None,
        waveform,
    }
}

/// Walk the row sequence and emit Lfo events based on per-row
/// aggregated views. `advance_on_depth_only_with_quirk` controls
/// the FT2 vol-col-B quirk (only set for Vibrato).
fn lfo_events_from_views<F>(
    rows: &[RowVisit<'_>],
    view_fn: F,
    advance_on_depth_only_with_quirk: bool,
    quirks: &PlaybackQuirks,
) -> Vec<LfoEvent>
where
    F: Fn(&[TrackImportEffect]) -> LfoRowView,
{
    let mut out = Vec::new();
    let mut state = LfoState::default();
    for row in rows {
        let view = view_fn(row.effects);
        let wave_only = view.waveform.is_some()
            && view.main.is_none()
            && view.speed_only.is_none()
            && view.depth_only.is_none();
        if let Some((w, r)) = view.waveform {
            state.waveform = w;
            state.retrig = r;
        }

        let depth_q15 = |raw: i16| Q15::from_raw(raw);

        if let Some((speed, depth)) = view.main {
            let speed = view.speed_only.unwrap_or(speed);
            let depth = view.depth_only.unwrap_or(depth);
            out.push(LfoEvent::Set {
                tick: row.tick,
                speed,
                depth: depth_q15(depth),
                waveform: state.waveform,
                retrig: state.retrig,
            });
            state.active = true;
            continue;
        }

        match (view.speed_only, view.depth_only) {
            (Some(s), Some(d)) => {
                out.push(LfoEvent::Set {
                    tick: row.tick,
                    speed: s,
                    depth: depth_q15(d),
                    waveform: state.waveform,
                    retrig: state.retrig,
                });
                state.active = true;
                continue;
            }
            (Some(s), None) => {
                out.push(LfoEvent::SpeedOnly {
                    tick: row.tick,
                    speed: s,
                });
                state.active = true;
                continue;
            }
            (None, Some(d)) => {
                if advance_on_depth_only_with_quirk && quirks.volcol_b_advances_vibrato {
                    out.push(LfoEvent::DepthOnly {
                        tick: row.tick,
                        depth: depth_q15(d),
                    });
                    state.active = true;
                    continue;
                }
                out.push(LfoEvent::DepthOnly {
                    tick: row.tick,
                    depth: depth_q15(d),
                });
                continue;
            }
            (None, None) => { /* fall through */ }
        }

        if wave_only {
            out.push(LfoEvent::WaveformOnly {
                tick: row.tick,
                waveform: state.waveform,
                retrig: state.retrig,
            });
            continue;
        }

        if state.active {
            out.push(LfoEvent::Clear { tick: row.tick });
            state.active = false;
        }
    }
    out
}

/// Extract Vibrato LFO events on the per-Track-pitch lane.
pub fn extract_vibrato_events(rows: &[RowVisit<'_>], quirks: &PlaybackQuirks) -> Vec<LfoEvent> {
    lfo_events_from_views(rows, vibrato_row_view, true, quirks)
}

/// Extract Tremolo LFO events on the per-Track-volume lane.
pub fn extract_tremolo_events(rows: &[RowVisit<'_>], quirks: &PlaybackQuirks) -> Vec<LfoEvent> {
    lfo_events_from_views(rows, tremolo_row_view, false, quirks)
}

/// Extract Panbrello LFO events on the per-Track-panning lane.
pub fn extract_panbrello_events(rows: &[RowVisit<'_>], quirks: &PlaybackQuirks) -> Vec<LfoEvent> {
    lfo_events_from_views(rows, panbrello_row_view, false, quirks)
}

#[cfg(test)]
mod tests {
    use super::super::rows_at_speed;
    use super::*;
    use alloc::vec;

    fn empty() -> Vec<TrackImportEffect> {
        Vec::new()
    }

    fn xm_quirks() -> PlaybackQuirks {
        let mut q = PlaybackQuirks::default();
        q.volcol_b_advances_vibrato = true;
        q
    }

    fn modern_quirks() -> PlaybackQuirks {
        PlaybackQuirks::default()
    }

    fn vib(speed: i16, depth: i16) -> TrackImportEffect {
        TrackImportEffect::Vibrato(Q8_8::from_int(speed), PitchDelta::from_q8_8_i16(depth))
    }

    #[test]
    fn extract_vibrato_basic() {
        let per_row = vec![vec![vib(3, 6)], vec![vib(3, 6)], vec![vib(3, 6)]];
        let rows = rows_at_speed(&per_row, 6);
        let events = extract_vibrato_events(&rows, &modern_quirks());
        assert_eq!(events.len(), 3);
        for e in &events {
            assert!(matches!(e, LfoEvent::Set { .. }));
        }
        assert_eq!(events[0].tick(), 0);
        assert_eq!(events[1].tick(), 6);
        assert_eq!(events[2].tick(), 12);
    }

    #[test]
    fn extract_vibrato_then_clear() {
        let per_row = vec![vec![vib(3, 6)], vec![vib(3, 6)], empty()];
        let rows = rows_at_speed(&per_row, 6);
        let events = extract_vibrato_events(&rows, &modern_quirks());
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], LfoEvent::Set { .. }));
        assert!(matches!(events[1], LfoEvent::Set { .. }));
        assert_eq!(events[2], LfoEvent::Clear { tick: 12 });
    }

    #[test]
    fn extract_vibrato_volcol_b_xm() {
        let per_row = vec![
            vec![vib(4, 8)],
            vec![TrackImportEffect::VibratoDepth(PitchDelta::from_q8_8_i16(
                16,
            ))],
        ];
        let rows = rows_at_speed(&per_row, 6);
        let events = extract_vibrato_events(&rows, &xm_quirks());
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], LfoEvent::Set { .. }));
        assert!(matches!(events[1], LfoEvent::DepthOnly { .. }));
    }

    #[test]
    fn extract_vibrato_volcol_b_non_xm() {
        let per_row = vec![
            vec![vib(4, 8)],
            vec![TrackImportEffect::VibratoDepth(PitchDelta::from_q8_8_i16(
                16,
            ))],
        ];
        let rows = rows_at_speed(&per_row, 6);
        let events = extract_vibrato_events(&rows, &modern_quirks());
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], LfoEvent::Set { .. }));
        assert!(matches!(events[1], LfoEvent::DepthOnly { .. }));
    }

    #[test]
    fn extract_vibrato_speed_only() {
        let per_row = vec![vec![TrackImportEffect::VibratoSpeed(Q8_8::from_int(5))]];
        let rows = rows_at_speed(&per_row, 6);
        let events = extract_vibrato_events(&rows, &modern_quirks());
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], LfoEvent::SpeedOnly { .. }));
    }

    #[test]
    fn extract_vibrato_waveform_change() {
        let per_row = vec![
            vec![TrackImportEffect::VibratoWaveform(
                Waveform::BipolarSquare,
                false,
            )],
            vec![vib(3, 6)],
        ];
        let rows = rows_at_speed(&per_row, 6);
        let events = extract_vibrato_events(&rows, &modern_quirks());
        assert_eq!(events.len(), 2);
        match events[0] {
            LfoEvent::WaveformOnly {
                waveform, retrig, ..
            } => {
                assert_eq!(waveform, Waveform::BipolarSquare);
                assert!(!retrig);
            }
            _ => panic!("expected WaveformOnly at row 0, got {:?}", events[0]),
        }
        match events[1] {
            LfoEvent::Set {
                waveform, retrig, ..
            } => {
                assert_eq!(waveform, Waveform::BipolarSquare);
                assert!(!retrig);
            }
            _ => panic!("expected Set at row 1, got {:?}", events[1]),
        }
    }

    #[test]
    fn extract_vibrato_waveform_after_set() {
        let per_row = vec![
            vec![vib(3, 6)],
            vec![TrackImportEffect::VibratoWaveform(
                Waveform::BipolarRampDown,
                true,
            )],
        ];
        let rows = rows_at_speed(&per_row, 6);
        let events = extract_vibrato_events(&rows, &modern_quirks());
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], LfoEvent::Set { .. }));
        match events[1] {
            LfoEvent::WaveformOnly {
                waveform, retrig, ..
            } => {
                assert_eq!(waveform, Waveform::BipolarRampDown);
                assert!(retrig);
            }
            _ => panic!("expected WaveformOnly at row 1"),
        }
    }
}
