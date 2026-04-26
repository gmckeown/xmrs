//! `CellNote` — what a pattern cell can carry in its note column.
//!
//! Distinct from [`Pitch`](crate::pitch::Pitch), which represents a
//! musical pitch only (one of the 120 chromatic positions across 10
//! octaves). The note column of a tracker cell can also encode
//! channel-level events that aren't pitches: key-off, note-cut,
//! note-fade. Mixing those with the pitch values in a single enum
//! made every consumer of the value test "is this actually a pitch?"
//! before doing anything useful.
//!
//! `CellNote` makes the distinction explicit. Most code paths only
//! care about the [`Play`](Self::Play) arm — that's where a real
//! pitch lands. The other arms describe note-column events, and the
//! channel logic dispatches them through `match` rather than through
//! ad-hoc `is_keyoff` / `is_fade` checks.

use crate::pitch::Pitch;
use serde::{Deserialize, Serialize};

/// Contents of a pattern cell's note column.
///
/// Variants correspond to the four kinds of value the IT/XM/MOD/S3M
/// formats can encode in the note byte:
///
/// - [`Empty`](Self::Empty): no note this cell.
/// - [`Play(p)`](Self::Play): trigger pitch `p`. The chromatic
///   position 0..119 the cell carries.
/// - [`KeyOff`](Self::KeyOff): release the envelope and start fadeout.
///   IT byte `255`, XM byte `97`.
/// - [`NoteCut`](Self::NoteCut): silence the channel immediately.
///   IT byte `254`. (S3M and earlier formats lack this; their
///   importers never produce it.)
/// - [`NoteFade`](Self::NoteFade): start the volume fadeout *without*
///   releasing the envelope — the envelope keeps ticking from where
///   it was. Specific to IT (and Schism's reading of any
///   out-of-spec byte 120..=252 as fade, see `fmt/it.c:284`).
///
/// `Default` is [`Empty`] so that pattern slots constructed by
/// `Default::default()` start as silent.
#[derive(Default, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CellNote {
    /// No note in this cell.
    #[default]
    Empty,
    /// Trigger this pitch.
    Play(Pitch),
    /// Release the envelope and begin fadeout.
    KeyOff,
    /// Silence the channel immediately.
    NoteCut,
    /// Start fadeout *without* releasing the envelope. IT-specific.
    NoteFade,
}

impl CellNote {
    /// Decode a raw note byte from a tracker pattern stream.
    ///
    /// The mapping follows IT's convention, which is the broadest:
    ///
    /// | byte         | result          |
    /// |--------------|-----------------|
    /// | 0..=119      | `Play(Pitch)`   |
    /// | 120..=252    | `NoteFade`      |
    /// | 253          | `Empty`         |
    /// | 254          | `NoteCut`       |
    /// | 255          | `KeyOff`        |
    ///
    /// The 120..=252 → `NoteFade` mapping mirrors Schism's
    /// `fmt/it.c:284` ("Impulse Tracker handles all unknown notes
    /// as fade internally"). XM and MOD importers don't produce
    /// values in that range; for them, only the four canonical
    /// bytes (0..=95 in MOD/XM scales remapped to 0..=95 here, plus
    /// 97/254/255) ever appear.
    #[inline]
    pub fn from_byte(b: u8) -> Self {
        match b {
            0..=119 => match Pitch::try_from(b) {
                Ok(p) => Self::Play(p),
                // Unreachable: 0..=119 is a valid Pitch variant by
                // construction. Treated defensively as Empty.
                Err(_) => Self::Empty,
            },
            120..=252 => Self::NoteFade,
            253 => Self::Empty,
            254 => Self::NoteCut,
            255 => Self::KeyOff,
        }
    }

    /// Returns the pitch if this cell triggers a note, else `None`.
    /// Convenience for the common path where only the pitch is
    /// relevant — `match` is still the cleaner choice when several
    /// arms are involved.
    #[inline]
    pub fn pitch(&self) -> Option<Pitch> {
        match self {
            Self::Play(p) => Some(*p),
            _ => None,
        }
    }
}

impl core::fmt::Debug for CellNote {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Empty => write!(f, "---"),
            Self::Play(p) => write!(f, "{:?}", p),
            Self::KeyOff => write!(f, "==="),
            Self::NoteCut => write!(f, "^^^"),
            Self::NoteFade => write!(f, "~~~"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_byte_round_trip_on_pitch_values() {
        for b in 0u8..=119 {
            match CellNote::from_byte(b) {
                CellNote::Play(p) => assert_eq!(p.value(), b),
                _ => panic!("byte {} should decode as Play", b),
            }
        }
    }

    #[test]
    fn from_byte_handles_event_bytes() {
        assert_eq!(CellNote::from_byte(120), CellNote::NoteFade);
        assert_eq!(CellNote::from_byte(200), CellNote::NoteFade);
        assert_eq!(CellNote::from_byte(252), CellNote::NoteFade);
        assert_eq!(CellNote::from_byte(253), CellNote::Empty);
        assert_eq!(CellNote::from_byte(254), CellNote::NoteCut);
        assert_eq!(CellNote::from_byte(255), CellNote::KeyOff);
    }

    #[test]
    fn pitch_extraction() {
        assert_eq!(CellNote::Empty.pitch(), None);
        assert_eq!(CellNote::Play(Pitch::C5).pitch(), Some(Pitch::C5));
        assert_eq!(CellNote::KeyOff.pitch(), None);
        assert_eq!(CellNote::NoteCut.pitch(), None);
        assert_eq!(CellNote::NoteFade.pitch(), None);
    }

    #[test]
    fn default_is_empty() {
        let n: CellNote = Default::default();
        assert_eq!(n, CellNote::Empty);
    }
}
