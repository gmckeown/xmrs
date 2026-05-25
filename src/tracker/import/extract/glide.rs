//! TonePortamento glide-event extractor (target pitch carried by the
//! row's note column).

use alloc::vec::Vec;

use crate::core::cell_note::CellNote;
use crate::core::daw::automation::GlideEvent;
use crate::core::pitch::Pitch;
use crate::tracker::import::effect::TrackImportEffect;

use super::RowVisit;

/// Aggregate `TonePortamento + TonePortamentoFxVol` per row.
fn tone_portamento_rate(effects: &[TrackImportEffect]) -> Option<i16> {
    let mut sum: i16 = 0;
    let mut any = false;
    for fx in effects {
        match fx {
            TrackImportEffect::TonePortamento(s) | TrackImportEffect::TonePortamentoFxVol(s) => {
                sum = sum.saturating_add(*s);
                any = true;
            }
            _ => {}
        }
    }
    any.then_some(sum)
}

/// Extract TonePortamento glide events. Each row that carries
/// `TonePortamento` rebinds the target pitch to the row's NoteOn
/// pitch (if any); subsequent rows without TonePortamento emit a
/// `Clear`.
///
/// If the row has TonePortamento *but* no NoteOn pitch, the target
/// keeps the latest target seen on a prior row — `Set` is still
/// emitted (with the rolling target) so the runtime knows the rate.
/// If no prior target exists either, the row is skipped.
pub fn extract_tone_portamento_events(rows: &[RowVisit<'_>]) -> Vec<GlideEvent> {
    let mut out = Vec::new();
    let mut active = false;
    let mut last_target: Option<Pitch> = None;
    for row in rows {
        match tone_portamento_rate(row.effects) {
            Some(rate) => {
                if let CellNote::Play(p) = row.note {
                    last_target = Some(p);
                }
                if let Some(target) = last_target {
                    out.push(GlideEvent::Set {
                        tick: row.tick,
                        target,
                        rate,
                    });
                    active = true;
                }
            }
            None => {
                if active {
                    out.push(GlideEvent::Clear { tick: row.tick });
                    active = false;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::rows_at_speed_with_notes;
    use super::*;
    use alloc::vec;

    #[test]
    fn extract_tone_portamento_basic() {
        let per_row = vec![
            (
                CellNote::Play(Pitch::C5),
                vec![TrackImportEffect::TonePortamento(5)],
            ),
            (CellNote::Empty, vec![TrackImportEffect::TonePortamento(5)]),
        ];
        let rows = rows_at_speed_with_notes(&per_row, 6);
        let events = extract_tone_portamento_events(&rows);
        assert_eq!(events.len(), 2);
        for e in &events {
            assert!(matches!(e, GlideEvent::Set { .. }));
        }
        match events[0] {
            GlideEvent::Set { target, rate, .. } => {
                assert_eq!(target, Pitch::C5);
                assert_eq!(rate, 5);
            }
            _ => panic!("expected Set"),
        }
    }

    #[test]
    fn extract_tone_portamento_retarget() {
        let per_row = vec![
            (
                CellNote::Play(Pitch::C5),
                vec![TrackImportEffect::TonePortamento(5)],
            ),
            (
                CellNote::Play(Pitch::E5),
                vec![TrackImportEffect::TonePortamento(5)],
            ),
        ];
        let rows = rows_at_speed_with_notes(&per_row, 6);
        let events = extract_tone_portamento_events(&rows);
        assert_eq!(events.len(), 2);
        match events[0] {
            GlideEvent::Set { target, .. } => assert_eq!(target, Pitch::C5),
            _ => panic!("expected Set"),
        }
        match events[1] {
            GlideEvent::Set { target, .. } => assert_eq!(target, Pitch::E5),
            _ => panic!("expected Set"),
        }
    }
}
