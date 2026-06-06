#![forbid(unsafe_code)]

//! Period tables used by the David Whittaker replayer.
//!
//! Each table is indexed by note number (0 = lowest playable note,
//! growing upward by semitone). Three flavours exist depending on
//! the replayer variant; the right one is selected at detection by
//! looking at the first word of the LEA target inside `Play()`.
//!
//! The runtime Paula period is derived from the table value and
//! a per-sample fine-tune multiplier computed from the sample's
//! native rate (`0x369E99 / frequency`):
//!
//! ```text
//! paula_period = (PERIODS[note] * (0x369E99 / frequency)) >> 10
//! ```
//!
//! The three or so trailing entries in `PERIODS2` / `PERIODS3` are
//! safety guards (covering the worst case of an arpeggio /
//! transpose pushing the index past the end of the original
//! tracker-side range). Values read straight from the period tables
//! embedded in the replayers (Ghidra) — e.g. `PERIODS2` @0x320 in
//! leviathan, `PERIODS3` @0x64a in the empire.

/// "QBall" / old-player periods — one octave, 12 entries.
pub const PERIODS1: &[u16; 12] = &[256, 242, 228, 215, 203, 192, 181, 171, 161, 152, 144, 136];

/// First new-player periods — four octaves plus three guard
/// entries.
pub const PERIODS2: &[u16; 51] = &[
    4096, 3864, 3648, 3444, 3252, 3068, 2896, 2732, 2580, 2436, 2300, 2168, 2048, 1932, 1824, 1722,
    1626, 1534, 1448, 1366, 1290, 1218, 1150, 1084, 1024, 966, 912, 861, 813, 767, 724, 683, 645,
    609, 575, 542, 512, 483, 456, 430, 406, 383, 362, 341, 322, 304, 287, 271, 256, 241, 228,
];

/// Extended new-player periods — six octaves of usable range.
pub const PERIODS3: &[u16; 72] = &[
    8192, 7728, 7296, 6888, 6504, 6136, 5792, 5464, 5160, 4872, 4600, 4336, 4096, 3864, 3648, 3444,
    3252, 3068, 2896, 2732, 2580, 2436, 2300, 2168, 2048, 1932, 1824, 1722, 1626, 1534, 1448, 1366,
    1290, 1218, 1150, 1084, 1024, 966, 912, 861, 813, 767, 724, 683, 645, 609, 575, 542, 512, 483,
    456, 430, 406, 383, 362, 341, 322, 304, 287, 271, 256, 241, 228, 215, 203, 191, 181, 170, 161,
    152, 143, 135,
];

/// Default "empty" track payload (one byte = `EndOfTrack` command).
/// Used when a position list references a track outside the loaded
/// payload, so the channel safely advances instead of running off
/// into garbage.
pub const EMPTY_TRACK: &[u8; 1] = &[0x80];

/// Which period table a parsed module uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodTable {
    /// Old / QBall player — [`PERIODS1`].
    P1,
    /// First new-player variant — [`PERIODS2`].
    P2,
    /// Extended new-player variant — [`PERIODS3`].
    P3,
}

impl PeriodTable {
    /// Look up a note index in the selected table. Returns the
    /// untransformed period — the caller still has to multiply by
    /// the sample's fine-tune and shift right by 10 to get the
    /// final Paula period.
    pub fn period(&self, note: usize) -> Option<u16> {
        match self {
            PeriodTable::P1 => PERIODS1.get(note).copied(),
            PeriodTable::P2 => PERIODS2.get(note).copied(),
            PeriodTable::P3 => PERIODS3.get(note).copied(),
        }
    }
}
