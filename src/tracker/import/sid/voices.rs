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

use super::detect::StreamLayout;
use crate::prelude::*;
use crate::tracker::import::patternslot::PatternSlot;
use alloc::{vec, vec::Vec};

#[derive(Debug)]
pub struct Voices {
    pub version: usize,
    /// Note-stream payload encoding, when it must differ from what `version`
    /// implies. `None` — every bundled tune — follows `version`, which is how
    /// this behaved before the encoding became separately knowable.
    ///
    /// The two are not the same thing: `version` also selects the release mode,
    /// the vibrato model and the drum semantics, whereas this decides only how
    /// many bytes an entry's payload occupies. The binary disagrees with the
    /// transcribed `version` on that point for three tunes (see
    /// [`super::detect::StreamLayout`]), so it needs its own knob to be
    /// measurable independently.
    pub stream: Option<StreamLayout>,
    pub songs: Vec<Vec<usize>>,
    pub orderings: Vec<Vec<u8>>,
    pub phrases: Vec<Vec<u8>>,
    /// Per-tune note-table overflow substitutions `(raw_note, real_note)`,
    /// consulted by `decode_phrase` before the shared default fixup. Needed
    /// because a note byte > 95 reads engine work-RAM whose value is tune- and
    /// frame-dependent (see `decode_phrase`); empty for tunes that use the
    /// shared default.
    pub overflow_overrides: &'static [(u8, u8)],
}

impl Voices {
    /// Assemble the parsed SID voice layout from the decoded tables: replayer
    /// `version`, per-sub-song order indices (`songs`), per-channel order lists
    /// (`orderings`), the shared phrase/pattern pool (`phrases`), and the
    /// per-tune overflow-note overrides.
    pub fn new(
        version: usize,
        songs: Vec<Vec<usize>>,
        orderings: Vec<Vec<u8>>,
        phrases: Vec<Vec<u8>>,
        overflow_overrides: &'static [(u8, u8)],
    ) -> Self {
        Self {
            version,
            stream: None,
            songs,
            orderings,
            phrases,
            overflow_overrides,
        }
    }

    /// Override the note-stream payload encoding. `None` restores "follow
    /// `version`". See [`Self::stream`].
    pub fn with_stream(mut self, stream: Option<StreamLayout>) -> Self {
        self.stream = stream;
        self
    }

    /// The encoding actually used to decode this tune's phrases.
    fn stream_layout(&self) -> StreamLayout {
        self.stream.unwrap_or(match self.version {
            10 => StreamLayout::PortaByte,
            15 => StreamLayout::InstrumentOnly,
            _ => StreamLayout::PortaWord,
        })
    }

    /// Each voice's full single-pass row sequence, as [`PatternSlot`]s: the
    /// concatenation of every phrase in that voice's ordering (until the `254`
    /// end-of-song marker, or the ordering ends).
    ///
    /// Voices are **not** cycled into a common grid — each one is its own
    /// length and loops independently via a `ChannelLoop`, exactly as the three
    /// SID voices run on the real hardware (no global realignment). This
    /// replaced the old `get_voice_grid` LCM hack, which materialised
    /// `lcm(voice lengths)` rows (thousands per song) to fit a single
    /// pattern-style grid — only needed before the DAW layer had per-lane
    /// loops. With it went its `MAX_GRID_ROWS` runaway cap: a single pass is
    /// bounded by the phrase data itself.
    ///
    /// `short_gate_instrs[i]` true ⇒ instrument `i` is drum-flagged (fxmask
    /// bit0) and uses a 1-row gate in the v30 replayer — see `decode_phrase`.
    /// Empty (or all-false) reproduces the pre-fix behaviour exactly.
    pub fn voice_passes(
        &self,
        song_number: usize,
        short_gate_instrs: &[bool],
    ) -> Vec<Vec<PatternSlot>> {
        let phrases = self.decode_all_phrases(short_gate_instrs);
        self.songs[song_number]
            .iter()
            .map(|&oi| {
                let mut rows: Vec<PatternSlot> = Vec::new();
                for &p in &self.orderings[oi] {
                    if p == 254 {
                        break; // original SID end-of-song marker
                    }
                    rows.extend_from_slice(pick_phrase(&phrases, p));
                }
                rows
            })
            .collect()
    }

    fn decode_all_phrases(&self, short_gate_instrs: &[bool]) -> Vec<Vec<PatternSlot>> {
        let mut sink = Vec::new();
        self.phrases
            .iter()
            .map(|p| {
                decode_phrase(
                    self.version,
                    self.stream_layout(),
                    p,
                    self.overflow_overrides,
                    short_gate_instrs,
                    &mut sink,
                )
            })
            .collect()
    }

    /// Diagnostic: every raw note byte > 95 (a `RH_FREQ_HEX` overflow) the
    /// decoder encounters, as `(phrase_index, raw_note)`. Used by the
    /// `sid_overflow_scan` example to list which tunes still need
    /// `overflow_overrides` entries (see `decode_phrase`).
    pub fn raw_overflow_notes(&self) -> Vec<(usize, u8)> {
        let mut out = Vec::new();
        for (pi, p) in self.phrases.iter().enumerate() {
            let mut raw = Vec::new();
            // Decode with no overrides so we see the original note bytes.
            let _ = decode_phrase(self.version, self.stream_layout(), p, &[], &[], &mut raw);
            for n in raw {
                out.push((pi, n));
            }
        }
        out
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

fn decode_phrase(
    version: usize,
    stream: StreamLayout,
    source: &[u8],
    overflow_overrides: &[(u8, u8)],
    short_gate_instrs: &[bool],
    raw_overflows: &mut Vec<u8>,
) -> Vec<PatternSlot> {
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
                match stream {
                    StreamLayout::PortaByte => {
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
                    StreamLayout::InstrumentOnly => {
                        // Spellbound variant: byte 2 is always instrument, never portamento
                        if byte(index) & 0b1000_0000 == 0 {
                            current.instrument = Some((byte(index) & 0b0111_1111) as usize);
                            last_instr = current.instrument;
                        }
                    }
                    StreamLayout::PortaWord => {
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
                            // Ghidra IK `play` $ae0c (note-fetch): the 2-byte
                            // portamento is a per-frame 16-bit FREQ-REGISTER delta.
                            // First byte `b2f1` = direction (bit0: 0=up/add,
                            // 1=down/sub) + LOW-byte step (bits1-6) + the bit7 porta
                            // flag; second byte `b2f4` = HIGH-byte step. The slide
                            // adds/subtracts `(b2f4<<8) | (b2f1 & 0x7e)` to $d400/01
                            // each gated frame. (The earlier guess — direction in
                            // bit6, value `(b2&0x3f)<<8 | next` — both mis-combined
                            // the bytes AND took the wrong direction bit, inflating
                            // the step ~15× and railing IK's lead to a screech.)
                            let b2f1 = byte(index) as u16;
                            let b2f4 = byte(index + 1) as u16;
                            let direction_down = byte(index) & 1 != 0;
                            let p: u16 = (b2f4 << 8) | (b2f1 & 0b0111_1110);
                            index += 1;
                            if p != 0 {
                                // Per-frame freq-register delta → 8-bit lane param
                                // (the player recovers `reg_delta = param << 6`; the
                                // >>6 / <<4 round-trip drops the low 6 bits, ≲2 %).
                                current.effect_parameter = (p >> 6).min(255) as u8;
                                current.effect_type = if direction_down { 2 } else { 1 };
                            }
                        }
                    }
                }
                index += 1;
            }

            // Note byte: bits 0-6 = note number. The frequency table
            // `RH_FREQ_HEX` (192 bytes @ 0x5428 in commando) holds only 96
            // notes (0-95); a note byte > 95 makes the original replayer index
            // *past* the table into adjacent engine work-RAM, so its "pitch" is
            // an overflow artefact, not a real note — the replayer reads
            // whatever byte happens to live at `RH_FREQ_HEX + note*2`. We
            // reproduce that at import time by substituting the note the
            // overflow actually sounds, so the produced track carries the right
            // notes.
            //
            // The default `match` below is the shared, empirically-tuned fixup.
            // But the overflow target is engine **work-RAM** (e.g. note 104 in
            // commando lands on `voice_ctrl`), so its value is tune- *and*
            // frame-dependent: verified via `sidplay --regdump` of the 6502+SID,
            // note 104 reads ~0x00 (freq-high 0, a near-DC hard-sync slave) in
            // commando but ~0x41 (a mid note) in monty. A single global value
            // can't be right for both, so `overflow_overrides` lets a specific
            // tune (keyed in `OneSid`) substitute its own correct note first —
            // commando overrides 104/105 → 0 (`RH_FREQ_HEX[0]` = 0x0116, the
            // closest representable note to its 0x00xx overflow) instead of the
            // shared default 65 (F-5), which had made its sync-slave voice 1
            // sound an audible mid tone.
            let n = byte(index) & 0b0111_1111;
            // The freq table `RH_FREQ_HEX` holds 96 notes, indices 0..=95, so the
            // first overflow is note 96 (`>= 8 * 12`), NOT 97. The earlier `n > 96`
            // guard let raw note 96 through as a real pitch — and Pitch 96 maps to
            // ~4.2 kHz, which clamps the SID freq register to 0xFFFF (a screeching
            // rail, e.g. Thing on a Spring's voice-1 noise drum). It also left the
            // `96 => …` overflow arm below permanently dead. `>=` routes 96 into
            // the overflow fixup like every other out-of-table index.
            let note = if n >= 8 * 12 {
                raw_overflows.push(n); // diagnostic: record the raw overflow note
                if let Some(&(_, sub)) = overflow_overrides.iter().find(|&&(k, _)| k == n) {
                    sub
                } else {
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
                }
            } else {
                n
            };
            current.note = match Pitch::try_from(note) {
                Ok(p) => CellNote::Play(p),
                Err(_) => CellNote::Empty,
            };
        } else {
            // Appended note (bit 6 set): the replayer re-programs the voice
            // with the gate cleared (`ctrl & 0xfe`) at the *same* pitch, i.e.
            // it RELEASES the held note (it never reads a new pitch). Emit a
            // KeyOff so the voice stops instead of sustaining forever — without
            // this a note followed by appended entries is held indefinitely
            // (e.g. Monty's voice 1 F-5 + 7×`0x7f`).
            current.note = CellNote::KeyOff;
            current.instrument = last_instr;
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

        // Replicate the replayer's release: a note with the release bit set
        // (`0x20` clear) is killed when its length expires — at `lengthleft==0`,
        // i.e. on the LAST of its `length` empty rows, the player gates the
        // voice off (Rob-Hubbard `ctrl &= 0xfe`, `sustain_release = 0`). Without
        // this the note holds at full sustain until the next note retriggers,
        // leaving sustained passages too loud.
        // v30 drum-flagged instruments (fxmask bit0) use a SHORT, 1-row gate.
        // The real replayer (Ghidra Delta `play` FUN_c110 $c124) clears the
        // gate as soon as `lenleft` drops below its initial value — i.e. after
        // the FIRST row — independent of the note's total `length` and of the
        // release bit, then rests for the remaining rows. Modelling only the
        // general `lenleft==0` release (KeyOff on the LAST row) held these
        // percussive lead accents as sustained notes: Delta voice 1's periodic
        // "breathing" rests were filled in, flattening the dynamics. So for a
        // short-gate instrument, emit the KeyOff on the FIRST empty row.
        let eff_instr = current.instrument.or(last_instr);
        let is_short_gate = version == 30
            && eff_instr.is_some_and(|i| short_gate_instrs.get(i).copied().unwrap_or(false));
        let note_is_play = matches!(current.note, CellNote::Play(_));
        let emit_kill = note_is_play && (release || is_short_gate);
        // 0-based index into the trailing empty rows at which the gate releases.
        let kill_row = if is_short_gate {
            0
        } else {
            length.saturating_sub(1)
        };
        // v30 freq-slide ($c338) continuation: the replayer applies the
        // per-frame slide EVERY frame the note is gated, not just on the
        // trigger frame. We decode it as a portamento (effect_type 1/2) on the
        // note cell, but a porta only runs while the effect is present, so the
        // trailing empty rows would freeze the pitch (the sweep would last one
        // row instead of the whole note — e.g. Delta sub-song 11 voice 1's
        // rising sweep stayed dead-flat). Re-emit the same porta on every held
        // row so the slide runs for the note's full length, matching the chip.
        // v30 only: v30 puts the $c338 slide on effect 1/2; v10/v20 portamento
        // is single-shot per the original decoders and must stay bit-identical.
        let continue_slide = if version == 30 {
            match current.effect_type {
                1 | 2 => Some((current.effect_type, current.effect_parameter)),
                _ => None,
            }
        } else {
            None
        };
        index += 1;
        phrase.push(current);

        if length != 0 {
            for k in 0..length {
                if emit_kill && k == kill_row {
                    let mut kill = PatternSlot::default();
                    kill.note = CellNote::KeyOff;
                    phrase.push(kill);
                } else if let Some((ft, fp)) = continue_slide {
                    let mut held = PatternSlot::default();
                    held.effect_type = ft;
                    held.effect_parameter = fp;
                    phrase.push(held);
                } else {
                    phrase.push(PatternSlot::default());
                }
            }
        }
    }
    phrase
}
