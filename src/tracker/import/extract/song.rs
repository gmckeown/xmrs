//! Song-level extractors: Bpm / Speed / GlobalVolume Points and Slides.

use alloc::vec::Vec;

use crate::core::daw::automation::{AutomationPoint, AutomationValue, SlideEvent};
use crate::core::effect::SongLevelEffect;
use crate::core::fixed::fixed::Q15;

use super::GlobalRowVisit;

/// Extract `SongLevelEffect::Bpm` rows as a Points curve.
pub fn extract_bpm_points(rows: &[GlobalRowVisit<'_>]) -> Vec<AutomationPoint> {
    let mut out = Vec::new();
    for row in rows {
        for ge in row.effects {
            if let SongLevelEffect::Bpm(b) = ge {
                out.push(AutomationPoint {
                    tick: row.tick,
                    value: AutomationValue::Bpm((*b).min(u16::MAX as usize) as u16),
                });
            }
        }
    }
    out
}

/// Extract `SongLevelEffect::Speed` rows as a Points curve.
pub fn extract_speed_points(rows: &[GlobalRowVisit<'_>]) -> Vec<AutomationPoint> {
    let mut out = Vec::new();
    for row in rows {
        for ge in row.effects {
            if let SongLevelEffect::Speed(s) = ge {
                out.push(AutomationPoint {
                    tick: row.tick,
                    value: AutomationValue::Speed((*s).min(u8::MAX as usize) as u8),
                });
            }
        }
    }
    out
}

/// Extract song-level `SongLevelEffect::Volume` rows as a Points curve.
pub fn extract_global_volume_points(rows: &[GlobalRowVisit<'_>]) -> Vec<AutomationPoint> {
    let mut out = Vec::new();
    for row in rows {
        for ge in row.effects {
            if let SongLevelEffect::Volume(v) = ge {
                out.push(AutomationPoint {
                    tick: row.tick,
                    value: AutomationValue::Normalized(v.raw()),
                });
            }
        }
    }
    out
}

/// Extract song-level `SongLevelEffect::VolumeSlide` events as Slide
/// events.
pub fn extract_global_volume_slide_events(rows: &[GlobalRowVisit<'_>]) -> Vec<SlideEvent> {
    let mut out = Vec::new();
    let mut active = false;
    for row in rows {
        let found = row.effects.iter().find_map(|ge| {
            if let SongLevelEffect::VolumeSlide { speed, fine } = ge {
                Some((*speed, *fine))
            } else {
                None
            }
        });
        match found {
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

/// Extract `SongLevelEffect::BpmSlide` rows as Slide events on the Bpm
/// lane.
pub fn extract_bpm_slide_events(rows: &[GlobalRowVisit<'_>]) -> Vec<SlideEvent> {
    let mut out = Vec::new();
    let mut active = false;
    for row in rows {
        let found = row.effects.iter().find_map(|ge| {
            if let SongLevelEffect::BpmSlide(d) = ge {
                Some(*d)
            } else {
                None
            }
        });
        match found {
            Some(d) => {
                let raw = d.clamp(i16::MIN as isize, i16::MAX as isize) as i16;
                out.push(SlideEvent::Set {
                    tick: row.tick,
                    rate: Q15::from_raw(raw),
                    fine: false,
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

#[cfg(test)]
mod tests {
    use super::super::global_rows_at_speed;
    use super::*;
    use alloc::vec;

    #[test]
    fn extract_global_bpm_change() {
        let rows_owned = vec![vec![SongLevelEffect::Bpm(140)]];
        let rows = global_rows_at_speed(&rows_owned, 6);
        let pts = extract_bpm_points(&rows);
        assert_eq!(pts.len(), 1);
        assert_eq!(pts[0].tick, 0);
        assert_eq!(pts[0].value, AutomationValue::Bpm(140));
    }

    #[test]
    fn extract_global_speed_change() {
        let rows_owned = vec![vec![SongLevelEffect::Speed(8)]];
        let rows = global_rows_at_speed(&rows_owned, 6);
        let pts = extract_speed_points(&rows);
        assert_eq!(pts.len(), 1);
        assert_eq!(pts[0].value, AutomationValue::Speed(8));
    }

    #[test]
    fn extract_global_volume_slide_emits_set_and_clear() {
        let rows_owned = vec![
            vec![SongLevelEffect::VolumeSlide {
                speed: Q15::from_raw(3),
                fine: false,
            }],
            vec![SongLevelEffect::VolumeSlide {
                speed: Q15::from_raw(3),
                fine: false,
            }],
            vec![],
        ];
        let rows = global_rows_at_speed(&rows_owned, 6);
        let events = extract_global_volume_slide_events(&rows);
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], SlideEvent::Set { .. }));
        assert!(matches!(events[1], SlideEvent::Set { .. }));
        assert_eq!(events[2], SlideEvent::Clear { tick: 12 });
    }
}
