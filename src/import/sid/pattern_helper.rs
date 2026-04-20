use crate::import::patternslot::PatternSlot;
use crate::prelude::*;
use alloc::{vec, vec::Vec};

#[derive(Debug)]
pub struct PatternHelper {
    pub version: usize,
    pub songs: Vec<Vec<usize>>,
    pub channels: Vec<Vec<u8>>,
    pub tracks: Vec<Vec<u8>>,
}

impl PatternHelper {
    pub fn new(
        version: usize,
        songs: Vec<Vec<usize>>,
        channels: Vec<Vec<u8>>,
        tracks: Vec<Vec<u8>>,
    ) -> Self {
        Self {
            version,
            songs,
            channels,
            tracks,
        }
    }

    fn get_track(&self, _track_index: usize, source: &[u8]) -> Vec<PatternSlot> {
        let mut track: Vec<PatternSlot> = vec![];
        let mut index: usize = 0;
        let mut last_instr: Option<usize> = None;

        while source[index] != 255 {
            let mut current = PatternSlot::default();

            let length = source[index] & 0b0001_1111; // 0-31
            let release = (source[index] & 0b0010_0000) == 0;
            let append = (source[index] & 0b0100_0000) == 0;
            let instr_or_portamento = (source[index] & 0b1000_0000) != 0;

            if append {
                index += 1;
                if instr_or_portamento {
                    match self.version {
                        10 => {
                            if source[index] & 0b1000_0000 == 0 {
                                current.instrument = Some((source[index] & 0b0111_1111) as usize);
                                last_instr = current.instrument;
                            } else {
                                // Portamento: bit 7 = portamento flag, bits 1-6 = speed,
                                // bit 0 = direction (0=up, 1=down).
                                // Speed is raw 16-bit freq delta per frame, >> 1 to scale for XM effects.
                                let p = source[index] & 0b0111_1110;
                                if p != 0 {
                                    current.effect_parameter = p >> 1;
                                    if source[index] & 1 == 0 {
                                        current.effect_type = 1; // portamento up
                                    } else {
                                        current.effect_type = 2; // portamento down
                                    }
                                }
                            }
                        }
                        15 => {
                            // Spellbound variant: byte 2 is always instrument, never portamento
                            if source[index] & 0b1000_0000 == 0 {
                                current.instrument = Some((source[index] & 0b0111_1111) as usize);
                                last_instr = current.instrument;
                            }
                        }
                        _ => {
                            // Version 20+: portamento uses 14-bit value across 2 bytes
                            // Byte 1: bit 7=portamento, bit 6=direction, bits 5-0=high 6 bits
                            // Byte 2: low 8 bits of portamento speed
                            if source[index] & 0b1000_0000 == 0 {
                                current.instrument = Some((source[index] & 0b0111_1111) as usize);
                                last_instr = current.instrument;
                            } else {
                                if index + 2 >= source.len() {
                                    #[cfg(feature = "std")]
                                    println!(
                                        "Track {}, International Karate overflow?",
                                        _track_index
                                    );
                                    index -= 2;
                                } else {
                                    let direction_down = source[index] & 0b0100_0000 != 0;
                                    let p: u16 = ((source[index] as u16 & 0b0011_1111) << 8)
                                        | source[index + 1] as u16;
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
                    }
                    index += 1;
                }

                // Note byte: bits 0-6 = note number (0-84 for 7 octaves)
                // Values > 96 are overflows in the original SID data where the
                // player reads from memory locations instead of the frequency table.
                // The hardcoded fixups below reproduce the original (buggy) behaviour.
                let n = source[index] & 0b0111_1111;
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
                current.note = note.try_into().unwrap_or(Pitch::None);
            }

            // Bit 7 of the note byte is unused in versions 10/15/20.
            // Version 30 (Delta) may use it as a reset-effect flag,
            // but no disassembly confirms this — left unimplemented.

            if release {
                if current.note == Pitch::None {
                    current.note = Pitch::Off;
                } else {
                    current.volume = 0x50; // set volume 64 (XM encoding)
                }
                current.instrument = last_instr;
            }

            index += 1;
            track.push(current);

            if length != 0 {
                let current = PatternSlot::default();
                for _ in 0..length {
                    track.push(current);
                }
            }
        }
        return track;
    }

    fn get_tracks(&self) -> Vec<Vec<PatternSlot>> {
        let mut tracks: Vec<Vec<PatternSlot>> = vec![];
        for (i, t) in self.tracks.iter().enumerate() {
            tracks.push(self.get_track(i, t));
        }
        return tracks;
    }

    fn get_pattern_order(&self, song_number: usize) -> Vec<&Vec<u8>> {
        let mut pattern_order: Vec<&Vec<u8>> = vec![];
        for s_index in &self.songs[song_number] {
            pattern_order.push(&self.channels[*s_index]);
        }
        pattern_order
    }

    pub fn get_patterns(&self, song_number: usize) -> Vec<Vec<Vec<PatternSlot>>> {
        let tracks = self.get_tracks();
        let pattern_order = self.get_pattern_order(song_number);
        let po_len = pattern_order.len();
        let mut all_ok: Vec<bool> = vec![false; po_len];
        let mut i_n: [usize; 3] = [0; 3];
        let mut patterns: Vec<Vec<Vec<PatternSlot>>> = vec![];

        loop {
            let mut trks: Vec<&Vec<PatternSlot>> = vec![];
            for k in 0..po_len {
                if pattern_order[k][i_n[k]] as usize >= tracks.len() {
                    #[cfg(feature = "std")]
                    println!("No way!? {}", pattern_order[k][i_n[k]]);
                    // special case for commando!?
                    let what_to_do: usize = match pattern_order[k][i_n[k]] {
                        111 => 3,
                        112 => 2,
                        _ => 0,
                    };
                    trks.push(&tracks[what_to_do]);
                } else {
                    trks.push(&tracks[pattern_order[k][i_n[k]] as usize]);
                }
            }
            let mut trks_total_len = trks.iter().map(|sublist| sublist.len()).max().unwrap_or(0);
            let mut pattern: Vec<Vec<PatternSlot>> = vec![];
            let mut j: [usize; 3] = [0; 3];
            while trks_total_len != 0 {
                let mut line: Vec<PatternSlot> = vec![];
                for k in 0..po_len {
                    if j[k] >= trks[k].len() {
                        i_n[k] += 1;
                        if i_n[k] >= pattern_order[k].len() {
                            i_n[k] = 0;
                        }
                        j[k] = 0;
                        if pattern_order[k][i_n[k]] as usize >= tracks.len() {
                            #[cfg(feature = "std")]
                            println!("No way!? {}", pattern_order[k][i_n[k]]);
                            // special case for commando!?
                            let what_to_do: usize = match pattern_order[k][i_n[k]] {
                                111 => 3,
                                112 => 2,
                                _ => 0,
                            };
                            trks[k] = &tracks[what_to_do];
                        } else {
                            trks[k] = &tracks[pattern_order[k][i_n[k]] as usize];
                        }
                        if trks[k].len() > trks_total_len {
                            trks_total_len = trks[k].len();
                        }
                    }
                    line.push(trks[k][j[k]]);
                    j[k] += 1;
                }
                trks_total_len -= 1;
                pattern.push(line);
            }

            patterns.push(pattern);
            for k in 0..po_len {
                i_n[k] += 1;
                if i_n[k] >= pattern_order[k].len() {
                    i_n[k] = 0;
                    // A hack to exit
                    all_ok[k] = true;
                    if all_ok.iter().all(|&b| b) {
                        return patterns;
                    }
                } else {
                    // Original way to exit
                    if pattern_order[k][i_n[k]] == 254 {
                        return patterns;
                    }
                }
            }
        }
    }

    pub fn cleanup_patterns(
        source: &[Vec<Vec<PatternSlot>>],
    ) -> (Vec<Vec<Vec<PatternSlot>>>, Vec<usize>) {
        let mut dest: Vec<Vec<Vec<PatternSlot>>> = Vec::new();
        let mut order: Vec<usize> = Vec::new();
        let mut seen_map: Vec<(Vec<Vec<PatternSlot>>, usize)> = Vec::new(); // Vec of (Pattern, index in dest)

        for pattern in source.iter() {
            if let Some(&(_, idx)) = seen_map.iter().find(|(p, _)| p == pattern) {
                order.push(idx);
            } else {
                let new_idx = dest.len();
                dest.push(pattern.clone());
                seen_map.push((pattern.clone(), new_idx));
                order.push(new_idx);
            }
        }

        (dest, order)
    }

    pub fn split_large_patterns(patterns: &mut Vec<Vec<Vec<PatternSlot>>>) {
        let max_size = 256;
        let mut i = 0;

        while i < patterns.len() {
            let current_pattern = &patterns[i];

            if current_pattern.len() > max_size {
                let num_splits = (current_pattern.len() + max_size - 1) / max_size;
                let mut new_patterns = Vec::new();

                for j in 0..num_splits {
                    let start = j * max_size;
                    let end = current_pattern.len().min(start + max_size);
                    let new_pattern = current_pattern[start..end].to_vec();
                    new_patterns.push(new_pattern);
                }

                patterns.splice(i..=i, new_patterns);
            } else {
                i += 1;
            }
        }
    }
}
