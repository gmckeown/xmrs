//! Parsed SID voice layout: the tables one `.sid` file unpacks
//! into, plus the cycled voice-grid producer consumed by
//! [`super::sid_module::SidModule::to_modules`].
//!
//! - `phrases: Vec<Vec<u8>>` — raw phrase byte streams. The SID
//!   format calls these "tracks", but the DAW layer has its own
//!   [`crate::core::daw::track::Track`] concept, so we say "phrase" here.
//! - `orderings: Vec<Vec<u8>>` — per-voice orderings (sequences of
//!   phrase indices).
//! - `songs: Vec<Vec<usize>>` — per sub-song, the three indices into
//!   `orderings` for the three SID voices.
//! - `version: usize` — SID variant (10, 15, 20+) that selects the
//!   phrase-byte decoder.

use crate::prelude::*;
use crate::tracker::import::patternslot::PatternSlot;
use alloc::{vec, vec::Vec};

#[derive(Debug)]
pub struct Voices {
    pub version: usize,
    pub songs: Vec<Vec<usize>>,
    pub orderings: Vec<Vec<u8>>,
    pub phrases: Vec<Vec<u8>>,
}

impl Voices {
    pub fn new(
        version: usize,
        songs: Vec<Vec<usize>>,
        orderings: Vec<Vec<u8>>,
        phrases: Vec<Vec<u8>>,
    ) -> Self {
        Self {
            version,
            songs,
            orderings,
            phrases,
        }
    }

    /// Cycle each voice through its phrase ordering until every
    /// voice has completed at least one full pass, and return the
    /// resulting `(rows × 3 voices)` grid of [`PatternSlot`]s.
    ///
    /// Bounded by [`Self::MAX_GRID_ROWS`]. SID v30 (Delta) does not
    /// emit a 254 end-of-song marker in every ordering and uses
    /// phrase-level compression / per-channel pattern loops that
    /// the v10/v20 decoder doesn't model — the natural
    /// "all voices wrapped once" terminator can fail to fire and
    /// the row generator runs away. The cap matches `MAX_ROWS` in
    /// [`crate::tracker::import::build`] for symmetry.
    pub fn get_voice_grid(&self, song_number: usize) -> Vec<Vec<PatternSlot>> {
        let phrases = self.decode_all_phrases();
        let voice_orderings = self.voice_orderings(song_number);
        let num_voices = voice_orderings.len();

        let mut all_done: Vec<bool> = vec![false; num_voices];
        let mut ordering_cursor: [usize; 3] = [0; 3];
        let mut grid: Vec<Vec<PatternSlot>> = Vec::new();

        loop {
            // Snapshot the current phrase per voice.
            let mut current_phrase: Vec<&Vec<PatternSlot>> = (0..num_voices)
                .map(|k| pick_phrase(&phrases, voice_orderings[k][ordering_cursor[k]]))
                .collect();
            let mut remaining = current_phrase.iter().map(|p| p.len()).max().unwrap_or(0);
            let mut row_cursor: [usize; 3] = [0; 3];

            while remaining != 0 {
                if grid.len() >= Self::MAX_GRID_ROWS {
                    return grid;
                }
                let mut row: Vec<PatternSlot> = Vec::with_capacity(num_voices);
                for k in 0..num_voices {
                    if row_cursor[k] >= current_phrase[k].len() {
                        // Advance to the next phrase in voice k's
                        // ordering, looping back to the start when
                        // the ordering ends.
                        ordering_cursor[k] += 1;
                        if ordering_cursor[k] >= voice_orderings[k].len() {
                            ordering_cursor[k] = 0;
                        }
                        row_cursor[k] = 0;
                        current_phrase[k] =
                            pick_phrase(&phrases, voice_orderings[k][ordering_cursor[k]]);
                        if current_phrase[k].len() > remaining {
                            remaining = current_phrase[k].len();
                        }
                    }
                    row.push(current_phrase[k][row_cursor[k]]);
                    row_cursor[k] += 1;
                }
                remaining -= 1;
                grid.push(row);
            }

            // After the inner loop, advance each voice's ordering
            // cursor and check termination.
            for k in 0..num_voices {
                ordering_cursor[k] += 1;
                if ordering_cursor[k] >= voice_orderings[k].len() {
                    ordering_cursor[k] = 0;
                    all_done[k] = true;
                    if all_done.iter().all(|&b| b) {
                        return grid;
                    }
                } else if voice_orderings[k][ordering_cursor[k]] == 254 {
                    // Original SID end-of-song marker.
                    return grid;
                }
            }
        }
    }

    /// Defensive cap on the row count `get_voice_grid` will emit
    /// before giving up. A typical SID song is `lcm(orderings)` ≈
    /// a few thousand rows; 65 536 rows is roughly 20 min at SID's
    /// 50 Hz row rate — well past any plausible song.
    pub const MAX_GRID_ROWS: usize = 1 << 16;

    fn voice_orderings(&self, song_number: usize) -> Vec<&Vec<u8>> {
        self.songs[song_number]
            .iter()
            .map(|s| &self.orderings[*s])
            .collect()
    }

    fn decode_all_phrases(&self) -> Vec<Vec<PatternSlot>> {
        self.phrases
            .iter()
            .map(|p| decode_phrase(self.version, p))
            .collect()
    }
}

/// Look up phrase index `idx`. The hardcoded fallbacks (`111 → 3`,
/// `112 → 2`) reproduce a quirk of `commando.sid`: the original
/// player reads memory locations that happen to map to those
/// phrases.
fn pick_phrase(phrases: &[Vec<PatternSlot>], idx: u8) -> &Vec<PatternSlot> {
    let i = idx as usize;
    if i < phrases.len() {
        return &phrases[i];
    }
    let fallback = match idx {
        111 => 3,
        112 => 2,
        _ => 0,
    };
    &phrases[fallback.min(phrases.len() - 1)]
}

fn decode_phrase(version: usize, source: &[u8]) -> Vec<PatternSlot> {
    // Treat OOB reads as the 0xFF sentinel: a truncated phrase
    // stream terminates instead of panicking.
    let byte = |idx: usize| -> u8 { source.get(idx).copied().unwrap_or(0xFF) };

    let mut phrase: Vec<PatternSlot> = vec![];
    let mut index: usize = 0;
    let mut last_instr: Option<usize> = None;

    while byte(index) != 255 {
        let mut current = PatternSlot::default();

        let length = byte(index) & 0b0001_1111; // 0-31
        let release = (byte(index) & 0b0010_0000) == 0;
        let append = (byte(index) & 0b0100_0000) == 0;
        let instr_or_portamento = (byte(index) & 0b1000_0000) != 0;

        if append {
            index += 1;
            if instr_or_portamento {
                match version {
                    10 => {
                        if byte(index) & 0b1000_0000 == 0 {
                            current.instrument = Some((byte(index) & 0b0111_1111) as usize);
                            last_instr = current.instrument;
                        } else {
                            // Portamento: bit 7 = portamento flag, bits 1-6 = speed,
                            // bit 0 = direction (0=up, 1=down).
                            // Speed is raw 16-bit freq delta per frame, >> 1 to scale for XM effects.
                            let p = byte(index) & 0b0111_1110;
                            if p != 0 {
                                current.effect_parameter = p >> 1;
                                if byte(index) & 1 == 0 {
                                    current.effect_type = 1; // portamento up
                                } else {
                                    current.effect_type = 2; // portamento down
                                }
                            }
                        }
                    }
                    15 => {
                        // Spellbound variant: byte 2 is always instrument, never portamento
                        if byte(index) & 0b1000_0000 == 0 {
                            current.instrument = Some((byte(index) & 0b0111_1111) as usize);
                            last_instr = current.instrument;
                        }
                    }
                    _ => {
                        // Version 20+: portamento uses 14-bit value across 2 bytes
                        // Byte 1: bit 7=portamento, bit 6=direction, bits 5-0=high 6 bits
                        // Byte 2: low 8 bits of portamento speed
                        if byte(index) & 0b1000_0000 == 0 {
                            current.instrument = Some((byte(index) & 0b0111_1111) as usize);
                            last_instr = current.instrument;
                        } else if index + 2 >= source.len() {
                            // International Karate v20 has phrase bytes that
                            // request a 2-byte portamento payload but only one
                            // byte remains. Roll back two positions so the
                            // outer loop's `index += 1` (at the phrase tail)
                            // lands on a sentinel — defensive but matches the
                            // observed playback in the original engine.
                            index = index.saturating_sub(2);
                        } else {
                            let direction_down = byte(index) & 0b0100_0000 != 0;
                            let p: u16 =
                                ((byte(index) as u16 & 0b0011_1111) << 8) | byte(index + 1) as u16;
                            index += 1;
                            if p != 0 {
                                // Scale 14-bit value (0-16383) to 8-bit (0-255)
                                current.effect_parameter = (p >> 6).min(255) as u8;
                                if direction_down {
                                    current.effect_type = 2; // portamento down
                                } else {
                                    current.effect_type = 1; // portamento up
                                }
                            }
                        }
                    }
                }
                index += 1;
            }

            // Note byte: bits 0-6 = note number (0-84 for 7 octaves).
            // Values > 96 are overflows in the original SID data where
            // the player reads from memory locations instead of the
            // frequency table; the hardcoded fixups below reproduce
            // the original (buggy) behaviour.
            let n = byte(index) & 0b0111_1111;
            let note = if n > 8 * 12 {
                match n {
                    96 => 0,
                    97 => 0,
                    98 => 12,
                    100 => 3,
                    104 => 65,
                    105 => 65,
                    107 => 6,
                    127 => 0,
                    _ => n & 0b0011_1111, // clamp to 0-63 for unmapped overflows
                }
            } else {
                n
            };
            current.note = match Pitch::try_from(note) {
                Ok(p) => CellNote::Play(p),
                Err(_) => CellNote::Empty,
            };
        }

        // Bit 7 of the note byte is unused in versions 10/15/20.
        // Version 30 (Delta) may use it as a reset-effect flag, but
        // no disassembly confirms this — left unimplemented.

        if release {
            if matches!(current.note, CellNote::Empty) {
                current.note = CellNote::KeyOff;
            } else {
                current.volume = 0x50; // set volume 64 (XM encoding)
            }
            current.instrument = last_instr;
        }

        index += 1;
        phrase.push(current);

        if length != 0 {
            let empty = PatternSlot::default();
            for _ in 0..length {
                phrase.push(empty);
            }
        }
    }
    phrase
}
