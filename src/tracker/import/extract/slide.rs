//! Slide event extraction for Portamento / VolumeSlide /
//! ChannelVolumeSlide / PanningSlide classes.

use alloc::vec::Vec;

use crate::core::daw::automation::SlideEvent;
use crate::core::fixed::fixed::Q15;
use crate::tracker::import::effect::TrackImportEffect;

use super::RowVisit;

/// Per-row aggregated view of one Slide class.
#[derive(Debug, Clone, Copy, Default)]
struct SlideRowView {
    rate: Option<(Q15, bool)>,
}

/// Portamento aggregates regular (Up/Down) and fine
/// (FineUp/FineDown/ExtraFineUp/ExtraFineDown) speeds into separate
/// sums. When both are present, regular wins for the lane Set —
/// matches the legacy `to_track_effects` ordering where the
/// regular slide was emitted first and `find_map` picked it.
fn portamento_row_view(effects: &[TrackImportEffect]) -> SlideRowView {
    let mut regular: i16 = 0;
    let mut has_regular = false;
    let mut fine: i16 = 0;
    let mut has_fine = false;
    for fx in effects {
        match fx {
            TrackImportEffect::PortamentoUp(s) | TrackImportEffect::PortamentoDown(s) => {
                regular = regular.saturating_add(*s);
                has_regular = true;
            }
            TrackImportEffect::PortamentoFineUp(s)
            | TrackImportEffect::PortamentoFineDown(s)
            | TrackImportEffect::PortamentoExtraFineUp(s)
            | TrackImportEffect::PortamentoExtraFineDown(s) => {
                fine = fine.saturating_add(*s);
                has_fine = true;
            }
            _ => {}
        }
    }
    let rate = if has_regular {
        Some((Q15::from_raw(regular), false))
    } else if has_fine {
        Some((Q15::from_raw(fine), true))
    } else {
        None
    };
    SlideRowView { rate }
}

fn volume_slide_row_view(effects: &[TrackImportEffect]) -> SlideRowView {
    for fx in effects {
        match fx {
            TrackImportEffect::VolumeSlideN(v) => {
                return SlideRowView {
                    rate: Some((*v, false)),
                };
            }
            TrackImportEffect::VolumeSlide0(v) => {
                return SlideRowView {
                    rate: Some((*v, true)),
                };
            }
            _ => {}
        }
    }
    SlideRowView::default()
}

fn channel_volume_slide_row_view(effects: &[TrackImportEffect]) -> SlideRowView {
    for fx in effects {
        match fx {
            TrackImportEffect::ChannelVolumeSlideN(v) => {
                return SlideRowView {
                    rate: Some((*v, false)),
                };
            }
            TrackImportEffect::ChannelVolumeSlide0(v) => {
                return SlideRowView {
                    rate: Some((*v, true)),
                };
            }
            _ => {}
        }
    }
    SlideRowView::default()
}

fn panning_slide_row_view(effects: &[TrackImportEffect]) -> SlideRowView {
    for fx in effects {
        match fx {
            TrackImportEffect::PanningSlideN(v) => {
                return SlideRowView {
                    rate: Some((*v, false)),
                };
            }
            TrackImportEffect::PanningSlide0(v) => {
                return SlideRowView {
                    rate: Some((*v, true)),
                };
            }
            _ => {}
        }
    }
    SlideRowView::default()
}

/// Walk the row sequence and emit Slide events from per-row
/// aggregated views.
fn slide_events_from_views<F>(rows: &[RowVisit<'_>], view_fn: F) -> Vec<SlideEvent>
where
    F: Fn(&[TrackImportEffect]) -> SlideRowView,
{
    let mut out = Vec::new();
    let mut active = false;
    for row in rows {
        let view = view_fn(row.effects);
        match view.rate {
            Some((rate, fine)) => {
                out.push(SlideEvent::Set {
                    tick: row.tick,
                    rate,
                    fine,
                });
                active = true;
            }
            None => {
                if active {
                    out.push(SlideEvent::Clear { tick: row.tick });
                    active = false;
                }
            }
        }
    }
    out
}

/// Extract Portamento events on the per-Track-pitch lane.
pub fn extract_portamento_events(rows: &[RowVisit<'_>]) -> Vec<SlideEvent> {
    slide_events_from_views(rows, portamento_row_view)
}

/// Extract VolumeSlide events on the per-Track-volume lane.
pub fn extract_volume_slide_events(rows: &[RowVisit<'_>]) -> Vec<SlideEvent> {
    slide_events_from_views(rows, volume_slide_row_view)
}

/// Extract ChannelVolumeSlide events on the per-Track-channel-volume
/// lane.
pub fn extract_channel_volume_slide_events(rows: &[RowVisit<'_>]) -> Vec<SlideEvent> {
    slide_events_from_views(rows, channel_volume_slide_row_view)
}

/// Extract PanningSlide events on the per-Track-panning lane.
pub fn extract_panning_slide_events(rows: &[RowVisit<'_>]) -> Vec<SlideEvent> {
    slide_events_from_views(rows, panning_slide_row_view)
}

#[cfg(test)]
mod tests {
    use super::super::rows_at_speed;
    use super::*;
    use alloc::vec;

    fn empty() -> Vec<TrackImportEffect> {
        Vec::new()
    }

    #[test]
    fn extract_volume_slide_continuous() {
        let v = || TrackImportEffect::VolumeSlideN(Q15::from_raw(1));
        let per_row = vec![vec![v()], vec![v()], vec![v()]];
        let rows = rows_at_speed(&per_row, 6);
        let events = extract_volume_slide_events(&rows);
        assert_eq!(events.len(), 3);
        for e in &events {
            assert!(matches!(e, SlideEvent::Set { .. }));
        }
    }

    #[test]
    fn extract_volume_slide_then_stop() {
        let v = || TrackImportEffect::VolumeSlideN(Q15::from_raw(2));
        let per_row = vec![vec![v()], vec![v()], empty()];
        let rows = rows_at_speed(&per_row, 6);
        let events = extract_volume_slide_events(&rows);
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], SlideEvent::Set { .. }));
        assert!(matches!(events[1], SlideEvent::Set { .. }));
        assert_eq!(events[2], SlideEvent::Clear { tick: 12 });
    }

    #[test]
    fn extract_volume_slide_fine() {
        let per_row = vec![vec![TrackImportEffect::VolumeSlide0(Q15::from_raw(5))]];
        let rows = rows_at_speed(&per_row, 6);
        let events = extract_volume_slide_events(&rows);
        assert_eq!(events.len(), 1);
        match events[0] {
            SlideEvent::Set { fine, .. } => assert!(fine),
            _ => panic!("expected Set"),
        }
    }

    #[test]
    fn extract_channel_volume_slide_set_clear() {
        let per_row = vec![
            vec![TrackImportEffect::ChannelVolumeSlideN(Q15::from_raw(1))],
            empty(),
        ];
        let rows = rows_at_speed(&per_row, 6);
        let evs = extract_channel_volume_slide_events(&rows);
        assert_eq!(evs.len(), 2);
        assert!(matches!(evs[0], SlideEvent::Set { .. }));
        assert_eq!(evs[1], SlideEvent::Clear { tick: 6 });
    }
}
