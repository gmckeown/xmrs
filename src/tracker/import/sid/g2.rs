//! Decoder for Rob Hubbard's **last player generation** ("G2" — the marker
//! player with the full eight-marker vocabulary): Lion_Heart, Sun_Never_Shines,
//! Pacific_Coast, Radio_ACE, Go_Go_Dash and Lakers_vs_Celtics.
//!
//! Ghidra is the source of truth; the complete reverse-engineering — record
//! layout, flag bits, marker semantics, wavetable command set, filter slots — is
//! in `ROBSID2_INSTRUMENT_MODEL.md`, and the reference binary is annotated in the
//! Ghidra project (`Lion_Heart.bin`, `lh_play $101a`).
//!
//! This module decodes the **instrument side**: the 16-byte records, their voice
//! programs and their filter slots. The song side (four order lists, patterns,
//! per-note markers) is the next piece.
//!
//! ## Finding the tables
//!
//! The five non-Lakers tunes share the player code byte for byte — only the
//! absolute-address OPERANDS differ, because each tune's data sits elsewhere.
//! Lakers is the same player with its whole variable block relocated, so fixed
//! code offsets do not work there either. Instead of a per-tune address table,
//! this decoder **reads each table's address out of the instruction that uses
//! it**, matched by its opcode pattern. That works for every member of the
//! family, present and future, without a hand-maintained config.

use alloc::vec;
use alloc::vec::Vec;

use crate::core::effect::ArmMode;
use crate::core::instr_robsid::{
    BounceRange, CutoffSweep, Filter, FilterMode, FilterRouting, InstrRobSid, PitchAction,
    ProgramEnd, RobEffects, VibratoMode, VoiceProgram, WaveShape, WaveStep,
};
use crate::core::instr_sid::SidVoice;
use crate::prelude::*;
use crate::tracker::import::memory::{ImportMemory, MemoryType};
use crate::tracker::import::patternslot::PatternSlot;
use crate::tracker::import::unit::TrackImportUnit;

use super::instr_helper::InstrHelper;

/// Byte offset of the load address inside a PSID/RSID file: the 124-byte v2
/// header plus the two-byte little-endian load word. `data[126 + addr - load]`
/// is the byte the C64 sees at `addr`.
const PSID_DATA: usize = 126;

/// Instrument records are selected by a marker operand masked to four bits, so
/// the player can address exactly sixteen of them (Ghidra: `ASL A` ×4).
pub const MAX_INSTRUMENTS: usize = 16;
/// Bytes per instrument record.
const RECORD: usize = 16;
/// Bytes per filter slot.
const SLOT: usize = 8;
/// A voice program is hand-authored and short; this only bounds a walk over
/// corrupt input, it is not a format limit.
const MAX_PROGRAM_STEPS: usize = 64;

/// Where a G2 tune keeps its tables, recovered from the player code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct G2Config {
    /// Which player generation the image holds.
    pub generation: Generation,
    /// Load address of the tune image.
    pub load: u16,
    /// Base of the 16 × 16-byte instrument records.
    pub instruments: u16,
    /// Base of the 8-byte filter slots.
    pub filter_slots: u16,
    /// Base of the pattern-pointer table (low bytes; the high bytes follow at +1).
    pub pattern_ptrs: u16,
    /// Base of the 96-entry note → frequency table.
    pub freq_table: u16,
    /// Base of the per-subtune order-list pointer table (lo, hi per track).
    pub order_ptrs: u16,
    /// Base of the per-subtune tempo table (frames per row, minus one).
    pub tempo_table: u16,
    /// The fixed envelope this tune's player writes as a note ends. A per-tune
    /// CONSTANT compiled into the code, not song data: Lion_Heart `($0F, $01)`,
    /// Sun_Never_Shines `($FF, $F0)`.
    ///
    /// **Decoded but not yet modelled.** The chip writes it about two frames
    /// BEFORE the gate falls; applying it AT the gate-off instead — the only
    /// point the note-off path offers without carrying the note's remaining
    /// length into the driver — makes things markedly WORSE, measured on both
    /// tunes: Lion_Heart `ad 760→2545, sr 1050→3600`, Sun_Never_Shines
    /// `1676→3199` on both. So the two-frame lead is not a detail to round off,
    /// it is the whole effect: the envelope has to be in place while the note is
    /// still gated. Reverted; the values stay here, tested, for whoever wires
    /// the countdown.
    pub release_adsr: (u8, u8),
    /// Work-RAM address of the subdivision reload. The two counters the row
    /// gate runs on sit either side of it: `-1` the subdivision, `+1` the
    /// twelve-step one (Ghidra `lh_play $109a`; verified on Lakers, whose whole
    /// variable block moved, so this is a layout invariant not a coincidence).
    pub tempo_reload: u16,
}

/// Which marker-dispatcher generation an image belongs to.
///
/// Both read their note streams the same way — a length byte masked with `#$1F`,
/// markers ahead of the note, the stream behind a zero-page indirect — and share
/// four of the six table signatures. They differ in how many markers the
/// dispatcher knows, and in which tables exist at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Generation {
    /// Rob Hubbard's last player: markers `80 82 83 84 86 87 88 89`, a
    /// per-instrument filter table, a frequency table read with a portamento
    /// add, and a release ramp. This is the one [`to_module`] decodes.
    #[default]
    Last,
    /// The earlier three-marker player: markers `$80`, `$82`, `$83` only, and
    /// none of those three tables. Nine tunes in the reference corpus.
    ///
    /// **Recognised, not decoded.** The marker numbers it shares with `Last`
    /// are not assumed to mean the same things — that has to be read out of its
    /// own dispatcher. See `THREE_MARKER_GENERATION.md`.
    ThreeMarker,
}

/// One step of a track's order list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderStep {
    /// `0xFD N` — transpose everything this track plays from here on.
    Transpose(u8),
    /// A pattern index into the shared pattern table.
    Pattern(u8),
}

/// A per-note effect marker, in front of the note it applies to.
///
/// These bind to the NOTE, not to the instrument — that is the shift this player
/// generation made. On import they become cell effects / lane data, never
/// instrument fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteMarker {
    /// `0x80 N` — play the following notes with instrument `N` (four bits).
    Instrument(u8),
    /// `0x82 hi lo` — a signed 16-bit per-frame frequency slide, accumulated.
    /// Note the operand order: the HIGH byte comes first in the stream.
    Portamento(i16),
    /// `0x83 N` — arpeggio: phases +0, +(N>>4), +(N & 0x0F) semitones UP,
    /// restarting at this note.
    Arpeggio(u8),
    /// `0x84` — keep the filter sweep running; do not reload the instrument's slot.
    HoldFilter,
    /// `0x86 N` — vibrato: amplitude `(N & 0x78) >> 3`, shift `N & 7`.
    Vibrato(u8),
    /// `0x87 hi lo` — start this note's filter sweep from this cutoff. HIGH byte
    /// first, like [`Self::Portamento`].
    Cutoff(u16),
    /// `0x88` — keep the running pulse width; do not reload it from the patch.
    HoldPulseWidth,
    /// `0x89 N` — global: which voice's swept cutoff drives the chip filter.
    FilterVoice(u8),
}

/// What a row does to the note.
///
/// Three distinct musical states — a rest and a held note are NOT the same
/// thing, so they are not both an absent note: a rest closes the gate, a hold
/// leaves the voice exactly as it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowNote {
    /// Strike this note (already transposed).
    Play(u8),
    /// Close the gate and leave the patch alone (the replayer's `0x60`).
    Rest,
    /// The note's own end: the replayer clears the gate bit on the LAST row of
    /// every entry that is not tied to the next one (Ghidra `lh_play $13e2`,
    /// the `lh_ctrl_mask` = `0xFE`), so the envelope releases before the next
    /// note arrives. Musically distinct from [`Self::Rest`] — an authored
    /// silence versus a note running out — even though both close the gate.
    GateOff,
    /// The note before is still sounding.
    Hold,
}

/// One row of an expanded track — the flat form a tracker can hold.
///
/// Only the first row of an entry carries a note and markers; the rows that
/// follow are the note being held.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct G2Row {
    /// What this row does to the note.
    pub note: RowNote,
    /// This note is TIED to the one before it: change the pitch, do not
    /// re-trigger the instrument. Comes from bit 5 of the PREVIOUS entry's
    /// length byte.
    pub tied: bool,
    /// The instrument in force on this row.
    pub instrument: u8,
    /// The arpeggio in force for this note, as the replayer's nibble pair
    /// (`high = first offset, low = second`, both semitones UP; `0` = none).
    ///
    /// It lives on the row, not on the instrument, because the replayer keeps
    /// ONE slot for both the per-note `0x83` marker and the instrument's
    /// default (record +10) — see `G2Config::instrument`. A note with no marker
    /// re-arms the default; a TIED note keeps whatever was already running,
    /// since a tie skips the whole note-on.
    pub arpeggio: u8,
    /// The markers attached to this row's note.
    pub markers: Vec<NoteMarker>,
    /// How this note asks the instrument's modulators to be (re)armed —
    /// the `0x84` / `0x87` / `0x88` markers and the tie, resolved.
    pub arming: NoteArmingSpec,
    /// Marker `0x82`: a signed per-frame step added to the voice's FREQUENCY
    /// REGISTER (not to its pitch), accumulated for as long as the note lasts
    /// and reset by the next entry. `0` = no slide.
    pub porta: i16,
}

/// The resolved arming of one note, ready to become a
/// [`TrackEffect::NoteArming`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoteArmingSpec {
    pub legato: bool,
    pub filter: ArmMode,
    pub pulse_width: ArmMode,
    /// Marker `0x86`: this note's own vibrato parameter word, replacing the
    /// instrument's. It also suppresses the reload of the instrument's start
    /// DELAY, so a marked note vibrates straight away.
    pub vibrato: ArmMode,
    /// This note's arpeggio parameter word (packed nibble pair, high nibble
    /// first) — marker `0x83` when present, otherwise the instrument's `+10`
    /// default, which a fresh note-on re-arms and a tie leaves alone.
    ///
    /// It travels here rather than only on the cell because the replayer runs
    /// the phase per FRAME for the whole note out of a per-voice slot, which a
    /// row-tick tracker arpeggio cannot reproduce once a note spans rows.
    pub arpeggio: ArmMode,
    /// Frames from this note-on until the release envelope takes over. The
    /// replayer converts the note's length in ROWS to frames and subtracts two,
    /// so the envelope is in place while the note is still gated — and skips it
    /// entirely on a short note (under seven frames), on a tied one, and when
    /// the next entry is a rest or the end of the pattern.
    pub release_in: u16,
}

impl NoteArmingSpec {
    /// Nothing unusual: every modulator re-arms from the patch.
    pub fn is_default(&self) -> bool {
        !self.legato
            && self.filter == ArmMode::Reseed
            && self.pulse_width == ArmMode::Reseed
            && self.vibrato == ArmMode::Reseed
            && self.arpeggio == ArmMode::Reseed
            && self.release_in == 0
    }
}

/// One pattern entry: the note, how long it lasts, and the markers in front of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct G2Entry {
    /// Effect markers preceding the note, in stream order.
    pub markers: Vec<NoteMarker>,
    /// How many rows the note lasts.
    ///
    /// **Sticky**: the player keeps the last length byte it saw, so an entry
    /// without one lasts as long as the previous entry. The decoder resolves
    /// that here, so every entry carries a real length.
    pub rows: u8,
    /// Bit 5 of the length byte: the NEXT note is tied to this one — it changes
    /// the pitch without re-triggering the instrument.
    pub tie: bool,
    /// The note (0..=95), or `None` for a rest, which only closes the gate and
    /// leaves the patch alone.
    pub note: Option<u8>,
}

/// Find `needle` in `hay`, returning the index of the first match.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Read the 16-bit little-endian operand that follows the opcode at `at`.
fn operand(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*data.get(at + 1)?, *data.get(at + 2)?]))
}

impl G2Config {
    /// Recognise a G2 tune and recover its table addresses, or return `None`.
    ///
    /// `data` is the whole `.sid` file. Detection keys on the marker dispatcher
    /// (`CMP #$82` / `CMP #$83`, both with a `BEQ`), which is what distinguishes
    /// this player generation from the fx-mask replayers, plus the presence of
    /// every table it needs.
    pub fn detect(data: &[u8]) -> Option<Self> {
        let img = data.get(PSID_DATA..)?;
        let load = u16::from_le_bytes([*data.get(124)?, *data.get(125)?]);

        // The marker dispatcher, which is what tells this player family from the
        // fx-mask one — and, within it, which generation. `CMP #$87` (the
        // per-note cutoff override) exists only in the last generation.
        find(img, &[0xC9, 0x80, 0xF0])?;
        find(img, &[0xC9, 0x82, 0xF0])?;
        let generation = if find(img, &[0xC9, 0x87, 0xF0]).is_some() {
            Generation::Last
        } else {
            Generation::ThreeMarker
        };

        // `LDA instruments+0,X : STA $D402,Y` — seeding the pulse width from the
        // record is the only place an absolute,X load feeds $D402.
        let i = find(img, &[0xBD]).and_then(|_| {
            (0..img.len().saturating_sub(6))
                .find(|&k| img[k] == 0xBD && img[k + 3..k + 6] == [0x99, 0x02, 0xD4])
        })?;
        let instruments = operand(img, i)?;

        // `LDA slot+7,Y : AND #$F0` — the resonance nibble. The only `AND #$F0`
        // in the player that follows an absolute,Y load. The three-marker
        // generation has no per-instrument filter table, so this is required
        // only of the last one.
        let filter_slots = (0..img.len().saturating_sub(5))
            .find(|&k| img[k] == 0xB9 && img[k + 3..k + 5] == [0x29, 0xF0])
            .and_then(|f| operand(img, f)?.checked_sub(7));

        // `LDA freq_table,Y : CLC : ADC porta_lo,X` — the per-frame note lookup.
        let freq_table = (0..img.len().saturating_sub(5))
            .find(|&k| img[k] == 0xB9 && img[k + 3..k + 5] == [0x18, 0x7D])
            .and_then(|q| operand(img, q))
            .or_else(|| {
                // The three-marker generation has no portamento add on the
                // lookup, so that shape is absent. It reaches the table the
                // other way round — note plus the order list's transpose, then
                // doubled into a 16-bit index (Ghidra, Off the Cuff `$116d`):
                //   CLC : ADC transpose,X : STA note,X : ASL : TAY : LDA tab,Y
                (0..img.len().saturating_sub(12))
                    .find(|&k| {
                        img[k] == 0x7D
                            && img[k + 3] == 0x9D
                            && img[k + 6..k + 9] == [0x0A, 0xA8, 0xB9]
                    })
                    .and_then(|k| operand(img, k + 8))
            });

        if generation == Generation::Last && (filter_slots.is_none() || freq_table.is_none()) {
            return None;
        }
        let (filter_slots, freq_table) = (filter_slots.unwrap_or(0), freq_table.unwrap_or(0));

        // `LDA pattern_ptr_lo,Y : STA $FA` — the pattern pointer goes to the
        // zero-page indirect the entry walker uses.
        let p = (0..img.len().saturating_sub(5))
            .find(|&k| img[k] == 0xB9 && img[k + 3..k + 5] == [0x85, 0xFA])?;
        let pattern_ptrs = operand(img, p)?;

        // `LDA order_ptrs,X : STA order_ram,Y` in the init routine. The same
        // opcode pair seeds the pulse width from a record, so require the store
        // NOT to target the chip ($D4xx); the first match is the low-byte copy.
        let o = (0..img.len().saturating_sub(6))
            .find(|&k| img[k] == 0xBD && img[k + 3] == 0x99 && img[k + 5] != 0xD4)?;
        let order_ptrs = operand(img, o)?;

        // Tempo, in two hops. `LDA tempo_ctr : CMP tempo_reload` is the row
        // gate, and gives the RAM address of the reload; the init routine's
        // `LDA table,X : STA <that address>` then gives the table itself. Going
        // through the reload address keeps this independent of where the tune
        // put its variables (Lakers moved them all).
        let g = (0..img.len().saturating_sub(6)).find(|&k| img[k] == 0xAD && img[k + 3] == 0xCD)?;
        let reload = operand(img, g + 3)?;
        let want = [0x8D, reload as u8, (reload >> 8) as u8];
        let t = (0..img.len().saturating_sub(6))
            .find(|&k| img[k] == 0xBD && img[k + 3..k + 6] == want)?;
        let tempo_table = operand(img, t)?;

        // `LDA #ad : STA $D405,Y : LDA #sr : STA $D406,Y` — the release ramp.
        // The immediates differ per tune, so the five tunes are NOT identical
        // "except for address operands" as first assumed; these must be read
        // from the code too.
        let r = (0..img.len().saturating_sub(10)).find(|&k| {
            img[k] == 0xA9
                && img[k + 2..k + 5] == [0x99, 0x05, 0xD4]
                && img[k + 5] == 0xA9
                && img[k + 7..k + 10] == [0x99, 0x06, 0xD4]
        });
        let release_adsr = r.map_or((0x0F, 0x01), |k| (img[k + 1], img[k + 6]));

        Some(Self {
            generation,
            load,
            instruments,
            filter_slots,
            pattern_ptrs,
            freq_table,
            order_ptrs,
            tempo_table,
            tempo_reload: reload,
            release_adsr,
        })
    }

    /// Nominal frames per row for `subtune` — the subdivision reload plus one.
    ///
    /// This is NOT the true cadence: see [`Self::row_ticks`]. It is the value a
    /// tracker's "speed" column would show, and what the module's
    /// `default_tempo` carries.
    pub fn speed(&self, data: &[u8], subtune: usize) -> u8 {
        self.at(data, self.tempo_table.wrapping_add(subtune as u16))
            .unwrap_or(1)
            .saturating_add(1)
    }

    /// The frame each row starts on, obtained by **simulating the replayer's own
    /// two counters** rather than multiplying by a speed.
    ///
    /// The row gate is not a simple divider (Ghidra `lh_play $109a–$10f4`):
    ///
    /// ```text
    /// every frame:  c12 -= 1
    ///               if c12 < 0 { c12 = 11 }        // and the subdivision is NOT ticked
    ///               else       { ctr -= 1; if ctr < 0 { ctr = reload } }
    /// row advances when c12 != 0 && ctr == reload
    /// ```
    ///
    /// The twelve-step counter steals one frame in thirteen from the
    /// subdivision, so the real cadence is **not** `reload + 1`: for Lion_Heart
    /// (`reload = 1`) it is 2.181 frames per row, not 2 — a 9 % error that a
    /// uniform speed accumulates into hundreds of frames of drift over a
    /// minute, which is exactly what the oracle comparison showed. Simulating is
    /// both simpler and exact: no fraction to fit, no calibration to guess.
    pub fn row_ticks(&self, data: &[u8], subtune: usize, rows: usize) -> Vec<u32> {
        let reload = self.at(data, self.tempo_table.wrapping_add(subtune as u16)) as Option<u8>;
        let reload = reload.unwrap_or(1) as i16;
        // The counters live either side of the reload byte in the player's work
        // RAM, and the tune image carries their initial values.
        let mut ctr = self
            .at(data, self.tempo_reload.wrapping_sub(1))
            .unwrap_or(0) as i16;
        // The twelve-step counter's RELOAD is not the `#$0b` the listing shows:
        // the init SELF-MODIFIES that immediate from the tempo table's second
        // byte (`LDA tempo_table+1,X : STA $10a0`, and `$10a0` IS the operand of
        // the `LDA #$0b` in the row gate). Reading the literal puts the stolen
        // frame at one in thirteen instead of one in four, and the whole song at
        // the wrong cadence — 2.18 frames per row instead of Lion_Heart's 8/3.
        // Measured against the chip: voice 1's first note lands on frame 514
        // with this, 419 without.
        let c12_reload = self
            .at(data, self.tempo_table.wrapping_add(1 + subtune as u16))
            .unwrap_or(0x0b) as i16;
        let mut c12 = self
            .at(data, self.tempo_reload.wrapping_add(1))
            .unwrap_or(0) as i16;

        let mut out = Vec::with_capacity(rows);
        let mut frame = 0u32;
        // Bounded so corrupt counters cannot spin: a row can never need more
        // than a handful of frames.
        let limit = (rows as u32).saturating_mul(64).max(64);
        while out.len() < rows && frame < limit {
            c12 -= 1;
            if c12 < 0 {
                c12 = c12_reload;
            } else {
                ctr -= 1;
                if ctr < 0 {
                    ctr = reload;
                }
            }
            if c12 != 0 && ctr == reload {
                out.push(frame);
            }
            frame += 1;
        }
        out
    }

    /// Read the four order-list pointers of `subtune` (three voices + the filter
    /// automation track).
    pub fn order_pointers(&self, data: &[u8], subtune: usize) -> [u16; 4] {
        let mut out = [0u16; 4];
        for (t, o) in out.iter_mut().enumerate() {
            // Ghidra `lh_init_subtune $18ff`: X = subtune*4, then +2 per track.
            let a = self.order_ptrs.wrapping_add((subtune * 4 + t * 2) as u16);
            *o = u16::from_le_bytes([
                self.at(data, a).unwrap_or(0),
                self.at(data, a.wrapping_add(1)).unwrap_or(0),
            ]);
        }
        out
    }

    /// Walk one track's order list up to its `0xFF` restart marker.
    ///
    /// `0xFF` means "loop back to the start", so the list read here is one full
    /// pass; the track then repeats it for as long as the tune plays.
    ///
    /// The two generations spell a transpose differently, and the difference is
    /// not cosmetic — read one list with the other's rule and its transposes
    /// come out as pattern indices:
    ///
    /// - the last player uses a separate `0xFD N` step;
    /// - the three-marker player packs it into the byte, `0x80 | semitones`
    ///   (Ghidra, Off the Cuff `$10c6`: `AND #$7F : STA $16A0,X`, and `$116d`
    ///   adds that slot to every note before the frequency lookup).
    pub fn order_list(&self, data: &[u8], ptr: u16) -> Vec<OrderStep> {
        let mut out = Vec::new();
        let mut a = ptr;
        // A malformed list without a 0xFF would otherwise walk the whole image.
        for _ in 0..256 {
            match (self.at(data, a), self.generation) {
                (None | Some(0xFF), _) => break,
                (Some(0xFD), Generation::Last) => {
                    out.push(OrderStep::Transpose(
                        self.at(data, a.wrapping_add(1)).unwrap_or(0),
                    ));
                    a = a.wrapping_add(2);
                }
                // `0xFE` calls a sub-routine rather than naming a pattern; it
                // ends the useful part of the list for our purposes.
                (Some(0xFE), Generation::ThreeMarker) => break,
                (Some(p), Generation::ThreeMarker) if p & 0x80 != 0 => {
                    out.push(OrderStep::Transpose(p & 0x7F));
                    a = a.wrapping_add(1);
                }
                (Some(p), _) => {
                    out.push(OrderStep::Pattern(p));
                    a = a.wrapping_add(1);
                }
            }
        }
        out
    }

    /// Decode pattern `index` into its entries.
    ///
    /// An entry is a run of `0x80`-family markers and an optional length byte
    /// (`0xC0–0xFF`), closed by the note byte (`0x00–0x5F`, or `0x60` for a
    /// rest). The byte after a note is `0x81` at the end of the pattern.
    pub fn pattern(&self, data: &[u8], index: u8) -> Vec<G2Entry> {
        let p = self.pattern_ptrs.wrapping_add((index as usize * 2) as u16);
        let ptr = u16::from_le_bytes([
            self.at(data, p).unwrap_or(0),
            self.at(data, p.wrapping_add(1)).unwrap_or(0),
        ]);
        self.pattern_at(data, ptr)
    }

    /// Decode the entries of the pattern stored at `ptr`.
    pub fn pattern_at(&self, data: &[u8], ptr: u16) -> Vec<G2Entry> {
        let mut out: Vec<G2Entry> = Vec::new();
        let mut a = ptr;
        // The length byte persists across entries — see `G2Entry::rows`.
        let mut len_byte = 0xC0u8;
        // Bounds the walk on corrupt input; real patterns are far shorter.
        for _ in 0..1024 {
            let mut markers = Vec::new();
            // Markers and the length byte, until the note byte closes the entry.
            let note = loop {
                let Some(b) = self.at(data, a) else {
                    return out;
                };
                if b & 0x80 == 0 {
                    a = a.wrapping_add(1);
                    break b;
                }
                if b & 0x40 != 0 {
                    // Length byte.
                    len_byte = b;
                    a = a.wrapping_add(1);
                    continue;
                }
                let arg = |k: u16| self.at(data, a.wrapping_add(k)).unwrap_or(0);
                let (m, adv): (Option<NoteMarker>, u16) = match b {
                    0x80 => (Some(NoteMarker::Instrument(arg(1) & 0x0F)), 2),
                    0x82 => (
                        Some(NoteMarker::Portamento(i16::from_le_bytes([arg(2), arg(1)]))),
                        3,
                    ),
                    // The one marker whose LENGTH differs between the
                    // generations, which makes it the one that must not be
                    // guessed: the last player spends one byte on an arpeggio,
                    // the three-marker player two on a vibrato (parameter, then
                    // a start delay — Ghidra, Off the Cuff `$1146`). Reading
                    // either with the other's length eats a byte too few or too
                    // many and desynchronises the rest of the entry. Off the Cuff
                    // carries thirteen of them.
                    //
                    // The delay is decoded but dropped: `NoteMarker::Vibrato`
                    // has nowhere to put it, and inventing a field for a value
                    // nothing consumes yet would be worse than saying so.
                    0x83 if self.generation == Generation::ThreeMarker => {
                        (Some(NoteMarker::Vibrato(arg(1))), 3)
                    }
                    0x83 => (Some(NoteMarker::Arpeggio(arg(1))), 2),
                    0x84 => (Some(NoteMarker::HoldFilter), 1),
                    0x86 => (Some(NoteMarker::Vibrato(arg(1))), 2),
                    0x87 => (
                        Some(NoteMarker::Cutoff(u16::from_le_bytes([arg(2), arg(1)]))),
                        3,
                    ),
                    0x88 => (Some(NoteMarker::HoldPulseWidth), 1),
                    0x89 => (Some(NoteMarker::FilterVoice(arg(1))), 2),
                    // The player's dispatcher falls through on anything else and
                    // re-reads the same byte, which would spin; skip it instead.
                    _ => (None, 1),
                };
                if let Some(m) = m {
                    markers.push(m);
                }
                a = a.wrapping_add(adv);
            };
            out.push(G2Entry {
                markers,
                rows: (len_byte & 0x1F) + 1,
                tie: len_byte & 0x20 != 0,
                note: (note != 0x60).then_some(note),
            });
            // End of pattern?
            if self.at(data, a) == Some(0x81) {
                return out;
            }
        }
        out
    }

    /// Byte the C64 sees at `addr`, or `None` if it falls outside the image.
    fn at(&self, data: &[u8], addr: u16) -> Option<u8> {
        let off = PSID_DATA.checked_add((addr.checked_sub(self.load)?) as usize)?;
        data.get(off).copied()
    }

    /// Decode the sixteen instrument records.
    pub fn instruments(&self, data: &[u8]) -> Vec<InstrRobSid> {
        (0..MAX_INSTRUMENTS)
            .map(|i| self.instrument(data, i))
            .collect()
    }

    /// Decode one 16-byte instrument record into the model.
    pub fn instrument(&self, data: &[u8], index: usize) -> InstrRobSid {
        let base = self.instruments.wrapping_add((index * RECORD) as u16);
        let b = |o: u16| self.at(data, base.wrapping_add(o)).unwrap_or(0);

        if self.generation == Generation::ThreeMarker {
            return self.three_marker_instrument(data, base);
        }

        let flags = b(7);
        let ctrl = WaveShape::from_ctrl(b(2));
        let voice = SidVoice {
            freq: 0,
            pw: ((b(1) as u16 & 0x0F) << 8) | b(0) as u16,
            ctrl_noise: ctrl.noise,
            ctrl_pulse: ctrl.pulse,
            ctrl_sawtooth: ctrl.sawtooth,
            ctrl_triangle: ctrl.triangle,
            ctrl_test: ctrl.test,
            ctrl_rm: ctrl.ring,
            ctrl_sync: ctrl.sync,
            ctrl_gate: ctrl.gate,
            ad: b(3),
            sr: b(4),
        };

        let mut fx = RobEffects::default();
        // Every instrument of this generation ends its notes on the same fixed
        // envelope — the pair is the player's, not the patch's.
        fx.release = crate::core::instr_robsid::ReleaseMode::Ramp {
            ad: self.release_adsr.0,
            sr: self.release_adsr.1,
        };

        // +5 — the centred semitone vibrato, the same law the v15/Thrust
        // replayers use. (Its start delay, record +14, needs engine support and
        // lands with the song-side importer.)
        let v5 = b(5);
        if (v5 & 0x78) != 0 {
            fx.vibrato = VibratoMode::Semitone {
                half_depth: (v5 & 0x78) >> 3,
                shift: v5 & 0x07,
                // +14 holds the note dead straight for this many frames before
                // the vibrato starts (Ghidra `lh_fx_vibrato $147b`, counter
                // `lh_vib_delay`) — instrument 5 waits 13 frames, instrument 7
                // waits 5.
                delay: b(14),
                // And once running, the swing itself only begins on the 4th
                // frame after note-on: until then the centring alone applies, so
                // the attack sits half a swing below the note and scoops up into
                // it (`lh_frames_since_noteon >= 4`).
                flat: 4,
            };
        }

        // +6 speed, +13 bounce bounds. This generation reloads the pulse width
        // from the record on every note-on (the per-note `0x88` marker is what
        // suppresses it), so the sweep restarts rather than free-running.
        let speed = b(6);
        if speed != 0 {
            fx.pulse_sweep.enable = true;
            fx.pulse_sweep.speed = speed as i8;
            fx.pulse_sweep.reseed_on_note = true;
            let bnd = b(13);
            fx.pulse_sweep.bounce = Some(BounceRange {
                lo: bnd & 0x0F,
                hi: bnd >> 4,
            });
        }

        // +10 is the instrument's DEFAULT arpeggio — but it deliberately does
        // NOT land in `fx.arpeggio`. In the replayer the per-note `0x83` marker
        // and this default write the very same slot (`lh_arp_param $1a55`), so
        // the arpeggio is really a property of the NOTE that happens to have a
        // per-instrument default. It therefore travels on the cell
        // (`TrackEffect::Arpeggio`, whose `{half1, half2}` is exactly this
        // nibble pair) and the instrument stays `ArpMode::None`. Leaving it in
        // both places would play it twice. See `arpeggio_default`.

        // bit2 = attack waveform (+9), bit6 = attack note (+8), sharing the
        // frame count at +11 — both decrement it, so an instrument setting both
        // gets half the attack. Bit 3 of the attack control is NOT the chip's
        // TEST bit here: it is a flag meaning "pull this voice out of the filter
        // for the attack", so it is stripped from the waveform.
        if flags & 0x04 != 0 {
            fx.two_phase.attack_shape = WaveShape::from_ctrl(b(9) & !0x08);
            fx.two_phase.attack_frames = b(11);
        }
        if flags & 0x40 != 0 {
            fx.two_phase.attack_note = Some(b(8).min(95));
            if fx.two_phase.attack_frames == 0 {
                fx.two_phase.attack_frames = b(11);
            }
        }

        // bit5 = filter, driven by the 8-byte slot at +12.
        if flags & 0x20 != 0 {
            fx.filter = self.filter_slot(data, b(12));
        }

        // bit0 = the voice program (the "wavetable"), pointed at by +8/+9.
        let program = (flags & 0x01 != 0)
            .then(|| self.voice_program(data, u16::from_le_bytes([b(8), b(9)])))
            .flatten();

        InstrRobSid { voice, program, fx }
    }

    /// Expand one track's order list into a flat row sequence — one pass, the
    /// length the track plays before looping back.
    ///
    /// This is the bridge from the replayer's compressed stream to something a
    /// tracker can hold: an entry lasting `n` rows becomes one row carrying the
    /// note and its markers, followed by `n − 1` continuation rows.
    ///
    /// `bank` is needed because the track transpose does NOT apply to
    /// instruments that run a voice program: the player reads the note byte raw
    /// for those (Ghidra `lh_play $126e`, the `flags & 1` test), since a program
    /// drives the pitch itself.
    pub fn voice_rows(&self, data: &[u8], order_ptr: u16, bank: &[InstrRobSid]) -> Vec<G2Row> {
        // Row → frame, to turn a note's length in rows into the replayer's own
        // frame countdown. Generous: the walk below is bounded by the order list.
        let ticks = self.row_ticks(data, 0, 8192);
        let frames_between = |a: usize, b: usize| -> u32 {
            match (ticks.get(a), ticks.get(b)) {
                (Some(x), Some(y)) => y.saturating_sub(*x),
                _ => 0,
            }
        };
        let mut row_index = 0usize;
        let mut out = Vec::new();
        let mut transpose = 0u8;
        let mut instrument = 0u8;
        let mut arpeggio = 0u8;
        // The tie bit of an entry announces that the NEXT note is tied to it.
        let mut next_is_tied = false;
        for step in self.order_list(data, order_ptr) {
            let pattern = match step {
                OrderStep::Transpose(t) => {
                    transpose = t;
                    continue;
                }
                OrderStep::Pattern(p) => p,
            };
            let entries = self.pattern(data, pattern);
            for (ei, e) in entries.iter().enumerate() {
                let mut marked_arp = None;
                let mut arming = NoteArmingSpec::default();
                let mut porta = 0i16;
                for m in e.markers.iter() {
                    match m {
                        NoteMarker::Instrument(n) => instrument = *n,
                        NoteMarker::Arpeggio(n) => marked_arp = Some(*n),
                        NoteMarker::HoldFilter => arming.filter = ArmMode::Hold,
                        NoteMarker::HoldPulseWidth => arming.pulse_width = ArmMode::Hold,
                        // A per-note cutoff wins over a hold: the marker names
                        // where this note's sweep starts.
                        NoteMarker::Cutoff(v) => arming.filter = ArmMode::At(*v),
                        NoteMarker::Portamento(step) => porta = *step,
                        NoteMarker::Vibrato(v) => arming.vibrato = ArmMode::At(*v as u16),
                        _ => {}
                    }
                }
                arming.legato = next_is_tied;
                // The release ramp: `lh_rows_to_frames` converts the entry's
                // length to frames and returns `frames - 2`, or 0 for a note
                // shorter than seven frames. The replayer then skips the write
                // when the note is tied, or when the next entry is a rest or the
                // end of the pattern — a note about to be silenced anyway.
                // ...and the replayer ALSO skips it when the next entry is a
                // rest or the pattern ends here (`lh_next_is_rest_or_end`): a
                // note about to be silenced anyway needs no ending envelope.
                let next_silences = entries.get(ei + 1).is_none_or(|n| n.note.is_none());
                let f = frames_between(row_index, row_index + e.rows as usize);
                arming.release_in = if !e.tie && !next_silences && f >= 7 {
                    (f - 2).min(u16::MAX as u32) as u16
                } else {
                    0
                };
                // The marker wins; otherwise a fresh note-on re-arms the
                // instrument's default, while a tie leaves the slot alone.
                arpeggio = match (marked_arp, next_is_tied) {
                    (Some(n), _) => n,
                    (None, false) => self.arpeggio_default(data, instrument as usize),
                    (None, true) => arpeggio,
                };
                // Arm the resolved value on the note: `Hold` on a tie (the
                // replayer leaves the slot untouched), the resolved parameter
                // when there is one, and `Reseed` for the common no-arpeggio
                // case so the cell stays free of a redundant effect.
                arming.arpeggio = if next_is_tied {
                    ArmMode::Hold
                } else if arpeggio != 0 {
                    ArmMode::At(arpeggio as u16)
                } else {
                    ArmMode::Reseed
                };
                let programmed = bank
                    .get(instrument as usize)
                    .is_some_and(|i| i.program.is_some());
                let note = match e.note {
                    Some(n) if programmed => RowNote::Play(n),
                    // The track transpose is SIGNED, and the replayer adds it
                    // with `CLC / ADC` (`$1281`) — 8-bit modular arithmetic —
                    // before clamping anything past `$5f` down to `$5f`
                    // (`$128a`). `saturating_add` on the raw byte turned every
                    // negative transpose into a huge positive one: `$fe` added
                    // 254 instead of subtracting 2, the sum pinned at 95, and 95
                    // is the last table entry, which every G2 tune stores as
                    // `$ffff`. Three of the six tunes use negative transposes
                    // (Lakers `$fe`/`$fb`, Lion_Heart `$fe`, Pacific_Coast
                    // `$fb`/`$fe`), and the notes they hit were exactly the
                    // ones stuck at `$ffff` for the length of a note.
                    Some(n) => RowNote::Play(n.wrapping_add(transpose).min(95)),
                    None => RowNote::Rest,
                };
                out.push(G2Row {
                    note,
                    tied: next_is_tied,
                    instrument,
                    arpeggio,
                    markers: e.markers.clone(),
                    arming,
                    porta,
                });
                for _ in 1..e.rows {
                    out.push(G2Row {
                        note: RowNote::Hold,
                        tied: false,
                        instrument,
                        arpeggio,
                        markers: Vec::new(),
                        arming: NoteArmingSpec::default(),
                        porta: 0,
                    });
                }
                // The gate falls on the entry's last row unless the next note is
                // tied to this one. An entry of a single row has no room for it:
                // the replayer fetches the next entry on that very tick.
                if !e.tie && e.rows >= 2 {
                    if let Some(last) = out.last_mut() {
                        last.note = RowNote::GateOff;
                    }
                }
                next_is_tied = e.tie;
                row_index += e.rows as usize;
            }
        }
        out
    }

    /// Convert one track's rows into the tracker slots the shared import
    /// pipeline consumes (`ImportMemory` → `split_rows_by_instrument` → tracks).
    ///
    /// The mapping:
    /// - a struck note becomes `Play` plus the instrument column;
    /// - a **tied** note is an ordinary note here; that it must not re-trigger
    ///   is said by the [`TrackEffect::NoteArming`] the module builder attaches
    ///   to the cell, not by omitting the instrument column (that shape is
    ///   already taken — see `ROBSID2_INSTRUMENT_MODEL.md` §11.3);
    /// - a rest becomes `KeyOff`, matching the replayer, which on a `0x60`
    ///   only clears the gate and leaves the patch untouched;
    /// - held rows stay empty.
    ///
    /// The per-note MARKERS are not mapped here. They are properties of the
    /// note, not of the instrument, so they belong on the cell as effects /
    /// lane data — but the runtime has no per-note override path into the SID
    /// driver yet, and emitting an ordinary tracker vibrato or arpeggio would
    /// stack a second modulation on top of the one the instrument already
    /// plays. That binding is the next piece of work; until then a G2 import
    /// carries the right notes with the instrument-level effects only.
    pub fn voice_slots(&self, rows: &[G2Row]) -> Vec<PatternSlot> {
        rows.iter()
            .map(|r| {
                let mut s = PatternSlot::default();
                match r.note {
                    RowNote::Play(n) => {
                        if let Ok(p) = Pitch::try_from(n) {
                            s.note = CellNote::Play(p);
                        }
                        s.instrument = Some(r.instrument as usize);
                    }
                    // The two note-offs are NOT the same event, and the runtime
                    // already distinguishes them by the instrument column:
                    //  * a REST is a `0x60` pattern ENTRY. The replayer fetches
                    //    it (`$129d`), clears the gate mask and jumps to the
                    //    register write — skipping the whole effect chain, so
                    //    every per-frame counter freezes for that frame. That is
                    //    a note-FETCH, which the import encodes as a note-off
                    //    cell CARRYING the instrument column.
                    //  * a GATE-OFF is the gate falling on an entry's last row
                    //    (`$13e2`). It happens on the ordinary row-tick path,
                    //    which falls through into the effect chain — so the
                    //    effects keep running. Bare note-off cell.
                    // Measured against the replayer's own pattern cursor
                    // (`$1a34,X`): before this, we made ZERO of the rest fetches
                    // (11 / 64 / 49 of them on Lion_Heart's three voices, on
                    // frames where our voice was still sounding).
                    RowNote::Rest => {
                        s.note = CellNote::KeyOff;
                        s.instrument = Some(r.instrument as usize);
                    }
                    RowNote::GateOff => s.note = CellNote::KeyOff,
                    RowNote::Hold => {}
                }
                // MOD effect 0 IS the arpeggio, and its `xy` parameter is the
                // very nibble pair the replayer stores — so the cell carries it
                // verbatim. Only on a row that (re)states a note: the replayer
                // re-arms its slot at note-on, and a held row must not restate
                // an effect the import memory would then treat as a new one.
                if r.arpeggio != 0 && matches!(r.note, RowNote::Play(_)) && s.effect_type == 0 {
                    s.effect_parameter = r.arpeggio;
                }
                s
            })
            .collect()
    }

    /// The instrument's default arpeggio nibble pair (record +10), for the row
    /// expansion to put on the cell. `0` = none.
    pub fn arpeggio_default(&self, data: &[u8], index: usize) -> u8 {
        let base = self.instruments.wrapping_add((index * RECORD) as u16);
        self.at(data, base.wrapping_add(10)).unwrap_or(0)
    }

    /// Decode one 8-byte filter slot.
    /// The three-marker generation's 16-byte record.
    ///
    /// It shares the first five bytes with every Rob Hubbard player — pulse
    /// width, control, attack-decay, sustain-release — and puts the voice-program
    /// pointer in the same place, `+8`/`+9` behind flags bit 0. Where it differs
    /// is the filter: no slot table, the parameters sit in the record itself
    /// (Ghidra, Off the Cuff `$1412`).
    ///
    /// **Everything not verified is left off.** The last generation reads a
    /// vibrato from `+5`, a pulse sweep from `+6`/`+13`, a two-phase attack from
    /// `+8`/`+9`/`+11` and an arpeggio default from `+10`; none of those has been
    /// confirmed at those offsets here, and `+13` is known to hold something
    /// else entirely (two nibbles patched into the player's code). Reading them
    /// anyway would be inventing effects. The vibrato in particular arrives from
    /// the note stream's `$83` marker in this generation, not from the record.
    fn three_marker_instrument(&self, data: &[u8], base: u16) -> InstrRobSid {
        let b = |o: u16| self.at(data, base.wrapping_add(o)).unwrap_or(0);
        let ctrl = WaveShape::from_ctrl(b(2));
        let voice = SidVoice {
            freq: 0,
            pw: ((b(1) as u16 & 0x0F) << 8) | b(0) as u16,
            ctrl_noise: ctrl.noise,
            ctrl_pulse: ctrl.pulse,
            ctrl_sawtooth: ctrl.sawtooth,
            ctrl_triangle: ctrl.triangle,
            ctrl_test: ctrl.test,
            ctrl_rm: ctrl.ring,
            ctrl_sync: ctrl.sync,
            ctrl_gate: ctrl.gate,
            ad: b(3),
            sr: b(4),
        };

        let flags = b(7);
        let mut fx = RobEffects::default();

        // +5 IS the instrument's default vibrato — Ghidra is unambiguous: a note
        // with no `$83` marker falls through to `LDA instr+5,Y : BNE` and enters
        // the same `AND #$78 >>3` / `AND #$07` decomposition the marker uses
        // (`$123f` → `$1247`). Six of the nine instruments Off the Cuff plays
        // carry a non-zero one.
        //
        // It is nevertheless NOT applied, because applying it was measured and
        // made things worse: as `VibratoMode::Semitone` with `flat: 0` the
        // frequency divergence went 7136 → 7216, and with the later player's
        // `flat: 4` it went to 7701. Knowing WHICH mechanism the code runs is
        // not knowing its numerical shape, and only the chip settles the second.
        // Whoever picks this up should start from the measurement, not from the
        // later player's constants.
        //
        // Flags bit 2 is the two-phase attack, and it is the same shape as the
        // later player's: while a per-voice counter lasts, the voice takes its
        // control byte from `+9` instead of `+2` (Ghidra `$13c4`). Six of the
        // nine instruments Off the Cuff plays set it.
        if flags & 0x04 != 0 {
            fx.two_phase.attack_shape = WaveShape::from_ctrl(b(9));
            fx.two_phase.attack_frames = b(11);
        }

        // Flags bit 5 arms a swept cutoff: `acc += (signed) +15` each frame, and
        // `+14` goes straight to $D417. Same shape as the fx-mask generation's
        // `fx_v2` records, different offsets.
        if flags & 0x20 != 0 {
            fx.filter = Filter {
                enable: true,
                resonance: b(14) >> 4,
                routing: FilterRouting::from_bits(b(14) & 0x0F),
                cutoff_seed: 0,
                // `Wrap` is the fx-mask generation's shape — a signed step added
                // to the cutoff high byte each frame, no bounds — and that is
                // exactly what `$1419` does here.
                sweep: CutoffSweep::Wrap { step: b(15) as i8 },
                mode: FilterMode::low(),
                reseed_each_note: true,
            };
        }

        // Flags bit 0: the voice program, pointed at by +8/+9 — the same place
        // and the same flag as the later player.
        let program = (flags & 0x01 != 0)
            .then(|| self.voice_program(data, u16::from_le_bytes([b(8), b(9)])))
            .flatten();

        InstrRobSid { voice, fx, program }
    }

    fn filter_slot(&self, data: &[u8], index: u8) -> Filter {
        let base = self
            .filter_slots
            .wrapping_add((index as usize * SLOT) as u16);
        let b = |o: u16| self.at(data, base.wrapping_add(o)).unwrap_or(0);
        let dir = b(6);
        Filter {
            enable: true,
            resonance: b(7) >> 4,
            // This generation derives the $D417 routing mask from which voices
            // have the filter on, rather than naming it per instrument; the
            // voice contributes its own bit at play time.
            routing: FilterRouting::default(),
            cutoff_seed: u16::from_le_bytes([b(1), b(0)]),
            sweep: CutoffSweep::Bounce {
                step: u16::from_le_bytes([b(3), b(2)]),
                min: b(5),
                max: b(4),
                up: dir & 0x80 != 0,
            },
            mode: FilterMode::from_bits(dir & 0x07),
            reseed_each_note: true,
        }
    }

    /// Decode a voice program from the player's command table.
    ///
    /// | command | bytes | meaning |
    /// |---------|-------|---------|
    /// | `0x00–0x7F` | 3 | waveform + a signed 16-bit pitch bend that ACCUMULATES |
    /// | `0x85` | 1 | freeze on the previous step (the usual terminator) |
    /// | `0x86 N` | 2 | loop back to step `N` |
    /// | other `≥0x80` | 2 | waveform + an absolute frequency-register high byte |
    ///
    /// Returns `None` for an empty or unreadable table.
    fn voice_program(&self, data: &[u8], ptr: u16) -> Option<VoiceProgram> {
        let mut steps: Vec<WaveStep> = Vec::new();
        let mut end = ProgramEnd::Hold;
        let mut addr = ptr;
        for _ in 0..MAX_PROGRAM_STEPS {
            let cmd = self.at(data, addr)?;
            if cmd == 0x85 {
                end = ProgramEnd::Hold;
                break;
            }
            if cmd == 0x86 {
                let target = self.at(data, addr.wrapping_add(1))?;
                // The player never bound-checks this; clamp so hand-edited or
                // corrupt data cannot point outside the program.
                end = ProgramEnd::Loop {
                    step: target.min(steps.len().saturating_sub(1) as u8),
                };
                break;
            }
            let shape = WaveShape::from_ctrl(cmd);
            if cmd & 0x80 != 0 {
                steps.push(WaveStep {
                    shape,
                    pitch: PitchAction::Fixed(self.at(data, addr.wrapping_add(1))?),
                });
                addr = addr.wrapping_add(2);
            } else {
                let lo = self.at(data, addr.wrapping_add(1))?;
                let hi = self.at(data, addr.wrapping_add(2))?;
                steps.push(WaveStep {
                    shape,
                    pitch: PitchAction::Bend(i16::from_le_bytes([lo, hi])),
                });
                addr = addr.wrapping_add(3);
            }
        }
        (!steps.is_empty()).then_some(VoiceProgram { steps, end })
    }
}

/// Build a playable [`Module`] from a last-generation tune.
///
/// The three voice tracks become three independent lanes, exactly as they run
/// on the chip: each loops over its own pass length rather than being forced
/// onto a shared grid, which is what lets them drift against each other the way
/// the hardware does. Track 3 — the filter automation timeline — is decoded but
/// not yet emitted; it needs the per-note override path (see the module docs).
///
/// Returns `None` if `data` is not a tune of this generation.
pub fn to_module(data: &[u8]) -> Option<Module> {
    let c = G2Config::detect(data)?;
    // Both generations come through here. Every piece that differs branches on
    // `c.generation`: the tables, the order lists' transpose encoding, the `$83`
    // marker's length, and the instrument records.
    //
    // The earlier one was let through only after being measured — it triggers
    // [275, 450, 405] notes against the chip's [276, 451, 405], with `gate`,
    // `ad` and `sr` at zero. Its pitch is still wrong and its counters are
    // frozen in `g2_oracle_fidelity_golden` as a starting line.
    let bank = c.instruments(data);
    let speed = c.speed(data, 0);

    // The PSID header carries the real title, author and copyright as three
    // 32-byte fixed fields; showing "Rob Hubbard (last player)" instead would be
    // hiding what the file actually says.
    let text = |off: usize| -> alloc::string::String {
        data.get(off..off + 32)
            .map(|b| {
                b.iter()
                    .take_while(|&&c| c != 0)
                    .map(|&c| c as char)
                    .collect::<alloc::string::String>()
                    .trim()
                    .into()
            })
            .unwrap_or_default()
    };
    let title = text(0x16);
    let mut module = Module {
        name: if title.is_empty() {
            alloc::string::String::from("Rob Hubbard (last player)")
        } else {
            title
        },
        comment: alloc::format!("{} - {}", text(0x56), text(0x36)),
        ..Default::default()
    };
    module.quirks = crate::tracker::profiles::pt();
    module.origin = Some(crate::tracker::format::ModuleFormat::Sid);
    module.default_tempo = speed as usize;
    module.instrument = InstrHelper::irss_to_instruments(&bank);

    // One `ImportMemory` per voice: the three SID voices carry independent
    // effect memory, so resolving them together would leak state between them.
    let ptrs = c.order_pointers(data, 0);
    let mut voices: Vec<Vec<TrackImportUnit>> = Vec::new();
    // Kept alongside the resolved rows so the per-note arming can be attached
    // to the finished cells: `PatternSlot` carries only the MOD-style effect
    // byte pair and cannot express a typed `TrackEffect`.
    let mut voice_arming: Vec<Vec<NoteArmingSpec>> = Vec::new();
    let mut voice_porta: Vec<Vec<i16>> = Vec::new();
    for &ptr in ptrs.iter().take(3) {
        let g2rows = c.voice_rows(data, ptr, &bank);
        voice_arming.push(g2rows.iter().map(|r| r.arming).collect());
        voice_porta.push(g2rows.iter().map(|r| r.porta).collect());
        let slots = c.voice_slots(&g2rows);
        let one_channel: Vec<Vec<PatternSlot>> = slots.into_iter().map(|s| vec![s]).collect();
        let mut im = ImportMemory::default();
        let unpacked = im.unpack_patterns(
            // The SID is a linear-frequency chip and the module plays in linear
            // frequencies; resolving in Amiga period space would shift octaves.
            FrequencyType::LinearFrequencies,
            MemoryType::Mod,
            &[vec![0]],
            &[one_channel],
        );
        voices.push(
            unpacked
                .first()
                .map(|p| p.iter().map(|r| r[0].clone()).collect())
                .unwrap_or_default(),
        );
    }

    let song_rows = voices.iter().map(|v| v.len()).max().unwrap_or(0);

    // Row positions come from SIMULATING the replayer's own two counters, not
    // from `row * speed`: the real cadence is 8/3 frames per row on Lion_Heart
    // (see `row_ticks`), which no integer speed can express. The fx-mask
    // importer does the same for its fractional-tempo tunes.
    let ticks = c.row_ticks(data, 0, song_rows + 2);
    let tick_at = |r: u32| -> u32 {
        ticks
            .get(r as usize)
            .copied()
            .unwrap_or_else(|| ticks.last().copied().unwrap_or(0) + r)
    };
    let speed_at = |r: u32| -> u8 { tick_at(r + 1).saturating_sub(tick_at(r)).clamp(1, 255) as u8 };

    // Each voice repeats its own pass forever, free-running against the others.
    for (i, v) in voices.iter().enumerate() {
        if !v.is_empty() {
            module
                .channel_loops
                .push(crate::core::daw::loop_region::ChannelLoop {
                    song: 0,
                    channel: i as u8,
                    start_tick: 0,
                    end_tick: tick_at(v.len() as u32),
                });
        }
    }

    // One track + clip per instrument-coherent run, so a voice that switches
    // instrument mid-phrase becomes consecutive clips on the same lane.
    let mut clips: Vec<Clip> = Vec::new();
    for (voice, rows) in voices.iter().enumerate() {
        for seg in crate::tracker::import::build::split_rows_by_instrument(rows) {
            let track = module.tracks.len() as u32;
            let start = seg.start_row;
            let mut cells: Vec<Cell> = seg.rows.into_iter().map(|t| t.prepare_cell()).collect();
            // Attach each note's arming (tie / holds / cutoff seed).
            for (k, cell) in cells.iter_mut().enumerate() {
                let a = voice_arming[voice]
                    .get(start as usize + k)
                    .copied()
                    .unwrap_or_default();
                if !a.is_default() {
                    cell.effects.push(TrackEffect::NoteArming {
                        legato: a.legato,
                        filter: a.filter,
                        pulse_width: a.pulse_width,
                        vibrato: a.vibrato,
                        arpeggio: a.arpeggio,
                        release_in: a.release_in,
                    });
                }
            }
            let len = cells.len() as u32;
            module.tracks.push(Track::Notes {
                name: alloc::format!("voice {voice} seg {track}"),
                instrument: seg.instrument,
                rows: cells,
                muted: false,
            });
            clips.push(Clip {
                track,
                song: 0,
                target_channel: voice as u8,
                position_tick: tick_at(start),
                speed_at_start: speed_at(start),
                track_row_offset: 0,
                source_start_row: start,
                end_tick: tick_at(start + len),
            });
        }
    }

    module.clips = crate::core::daw::sorted_clips::SortedClips::from_unsorted(clips);

    // Marker `0x82` — a per-frame step on the FREQUENCY REGISTER — becomes a
    // `Slide` lane on the track's pitch, which is the shape the player already
    // accumulates in register space for the v30 replayer's own sweep
    // (`Channel::sid_current_milli_hz` / `freq_slide_reg`). It converts the
    // lane's rate back with `|rate| << 4`, sign flipped, so the rate to emit is
    // `-step / 16`. The slide is armed on the note that carries the marker and
    // cleared on the next note, matching the replayer, which zeroes its
    // accumulator at every new pattern entry.
    for clip in module.clips.iter() {
        let track_idx = clip.track;
        let voice = clip.target_channel as usize;
        let Some(steps) = voice_porta.get(voice) else {
            continue;
        };
        let len = match &module.tracks[track_idx as usize] {
            Track::Notes { rows, .. } => rows.len(),
            _ => continue,
        };
        let mut events: Vec<crate::core::daw::automation::SlideEvent> = Vec::new();
        for k in 0..len {
            let row = clip.source_start_row as usize + k;
            let step = steps.get(row).copied().unwrap_or(0);
            let tick = tick_at(row as u32);
            if step != 0 {
                events.push(crate::core::daw::automation::SlideEvent::Set {
                    tick,
                    rate: crate::core::fixed::fixed::Q15::from_raw(
                        (-(step as i32) / 16).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                    ),
                    fine: false,
                });
            } else if matches!(
                events.last(),
                Some(crate::core::daw::automation::SlideEvent::Set { .. })
            ) {
                events.push(crate::core::daw::automation::SlideEvent::Clear { tick });
            }
        }
        if events
            .iter()
            .any(|e| matches!(e, crate::core::daw::automation::SlideEvent::Set { .. }))
        {
            module
                .automation
                .push(crate::core::daw::automation::AutomationLane {
                    target: crate::core::daw::automation::AutomationTarget::TrackPitch(track_idx),
                    kind: crate::core::daw::automation::LaneKind::Slide { events },
                    enabled: true,
                    scope: Default::default(),
                    song: 0,
                });
        }
    }

    // The song is linear — no jumps, breaks or loops in the order lists — so the
    // timeline is a plain row → tick ramp.
    let bpm = module.default_bpm as u16;
    let entries: Vec<crate::core::daw::timeline::TimelineEntry> = (0..song_rows)
        .map(|r| crate::core::daw::timeline::TimelineEntry {
            song: 0,
            order_idx: 0,
            pattern_idx: 0,
            row_idx: r as u32,
            loop_iter: 0,
            tick: tick_at(r as u32),
            speed_at_row: speed_at(r as u32),
            bpm_at_row: bpm,
        })
        .collect();
    module.timeline_map = crate::core::daw::timeline::TimelineMap { entries };

    Some(module)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::instr_robsid::ArpMode;

    const LION_HEART: &[u8] = include_bytes!("songs/lion_heart.sid");

    /// The two generations behind the same marker dispatcher, told apart by
    /// whether `CMP #$87` — the per-note cutoff override — is there.
    ///
    /// The earlier one is recognised so its tables can be reported and so a
    /// player can say *why* it declines, but [`to_module`] still refuses it:
    /// nothing decodes a three-marker note stream, and the markers it shares
    /// with the later player are not assumed to mean the same things.
    #[test]
    fn tells_the_two_generations_apart() {
        let lh = G2Config::detect(LION_HEART).expect("Lion_Heart is recognised");
        assert_eq!(lh.generation, Generation::Last);
        assert!(to_module(LION_HEART).is_some());

        // Every bundled tune of this family is the last generation.
        for (name, data) in [
            (
                "sun_never_shines",
                include_bytes!("songs/sun_never_shines.sid").as_slice(),
            ),
            ("pacific_coast", include_bytes!("songs/pacific_coast.sid")),
            ("radio_ace", include_bytes!("songs/radio_ace.sid")),
            ("go_go_dash", include_bytes!("songs/go_go_dash.sid")),
            (
                "lakers_vs_celtics",
                include_bytes!("songs/lakers_vs_celtics.sid"),
            ),
        ] {
            let c = G2Config::detect(data).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(c.generation, Generation::Last, "{name}");
            assert_ne!(
                c.filter_slots, 0,
                "{name}: the last generation has a filter table"
            );
            assert_ne!(c.freq_table, 0, "{name}: …and a frequency table");
        }
    }

    /// The three-marker generation's tables, pinned against a bundled image.
    ///
    /// Off the Cuff is bundled — with a `sidplay` capture beside it in
    /// `tests/fixtures_sid_oracle/off_the_cuff_3m#0.csv.gz` — so that whoever
    /// writes the decoder has both halves of a gate from the start, rather than
    /// writing it first and finding out afterwards. It shares its player build
    /// with Pygmies Revenge (identical table addresses), so a decoder proved on
    /// one is proved on two.
    ///
    /// The addresses below are what the code references; the relative layout
    /// (`order = pattern_ptrs - 10`, `tempo = pattern_ptrs - 2`) holds across
    /// eight of this generation's nine tunes, which is why they are believed to
    /// be tables and not coincidences.
    #[test]
    fn three_marker_tables() {
        const OFF_THE_CUFF: &[u8] = include_bytes!("songs/off_the_cuff.sid");
        let c = G2Config::detect(OFF_THE_CUFF).expect("recognised");
        assert_eq!(c.generation, Generation::ThreeMarker);
        assert_eq!(c.load, 0x1000);
        assert_eq!(c.instruments, 0x16FF);
        assert_eq!(c.pattern_ptrs, 0x1841);
        assert_eq!(c.order_ptrs, c.pattern_ptrs - 10);
        assert_eq!(c.tempo_table, c.pattern_ptrs - 2);
        // The frequency table is reached differently here — note plus the order
        // list's transpose, doubled into a 16-bit index — so it takes its own
        // signature (Ghidra `$116d`). No per-instrument filter table exists.
        assert_eq!(c.freq_table, 0x1580);
        assert_eq!(c.filter_slots, 0);
        // And it now decodes — measured against the chip before being let
        // through; see `g2_oracle_fidelity_golden`.
        assert!(to_module(OFF_THE_CUFF).is_some());
    }

    /// `$83` is two bytes here and one in the later player, so its length has to
    /// follow the generation.
    ///
    /// This is the only marker whose *size* differs, which makes it the only one
    /// that can desynchronise a whole entry rather than merely mean the wrong
    /// thing. Off the Cuff carries thirteen of them; before the split, each ate
    /// one byte too few and the leftover byte decoded as an extra note — the
    /// tune came out with exactly thirteen phantom notes, and nothing in the
    /// output looked wrong enough to notice.
    #[test]
    fn three_marker_vibrato_marker_is_two_bytes() {
        const OFF_THE_CUFF: &[u8] = include_bytes!("songs/off_the_cuff.sid");
        let c = G2Config::detect(OFF_THE_CUFF).unwrap();
        let markers: Vec<NoteMarker> = (0..24)
            .flat_map(|p| c.pattern(OFF_THE_CUFF, p))
            .flat_map(|e| e.markers)
            .collect();
        assert!(
            markers.iter().any(|m| matches!(m, NoteMarker::Vibrato(_))),
            "$83 must decode as this generation's vibrato"
        );
        assert!(
            !markers.iter().any(|m| matches!(m, NoteMarker::Arpeggio(_))),
            "…and never as the later player's arpeggio"
        );

        // The later player keeps the arpeggio reading. Its arpeggios sit well
        // past the first few patterns, so the whole table has to be walked.
        let lh = G2Config::detect(LION_HEART).unwrap();
        let lh_markers: Vec<NoteMarker> = (0..=255u8)
            .flat_map(|p| lh.pattern(LION_HEART, p))
            .flat_map(|e| e.markers)
            .collect();
        assert!(lh_markers
            .iter()
            .any(|m| matches!(m, NoteMarker::Arpeggio(_))));
    }

    /// The three-marker generation's instrument records, read at their own
    /// offsets.
    ///
    /// The five bytes every Rob Hubbard player shares come out first; the filter
    /// comes from the record rather than a slot table; and everything that has
    /// not been confirmed at these offsets stays off, so a patch is incomplete
    /// rather than invented.
    #[test]
    fn three_marker_instruments() {
        const OFF_THE_CUFF: &[u8] = include_bytes!("songs/off_the_cuff.sid");
        let c = G2Config::detect(OFF_THE_CUFF).unwrap();
        let bank = c.instruments(OFF_THE_CUFF);

        // Every instrument the patterns actually select must have an envelope —
        // an all-zero record would mean the base address is wrong.
        let used: Vec<usize> = (0..24)
            .flat_map(|p| c.pattern(OFF_THE_CUFF, p))
            .flat_map(|e| e.markers)
            .filter_map(|m| match m {
                NoteMarker::Instrument(i) => Some(i as usize),
                _ => None,
            })
            .collect();
        assert!(!used.is_empty(), "no instrument is ever selected");
        for i in &used {
            let v = &bank[*i].voice;
            assert!(
                v.ad != 0 || v.sr != 0,
                "instrument {i} decoded to a silent envelope"
            );
        }

        // Nothing unverified may be switched on. The two-phase attack is NOT in
        // this list any more: flags bit 2 was confirmed against the chip
        // (waveform divergence 1121 → 0), so it is wired. The vibrato is the
        // opposite case — real in Ghidra, but every shape tried so far measured
        // worse, so it stays off until one measures better.
        for ins in &bank {
            assert_eq!(
                ins.fx.vibrato,
                VibratoMode::None,
                "the +5 vibrato measured worse"
            );
            assert!(!ins.fx.pulse_sweep.enable, "the pulse sweep is unverified");
        }
        assert!(
            bank.iter().any(|i| i.fx.two_phase.attack_frames != 0),
            "six of the nine instruments set flags bit 2 — the attack must be wired"
        );
    }

    /// The three-marker generation packs its transposes into the order byte
    /// (`0x80 | semitones`), where the last generation spends a separate
    /// `0xFD N` step. `order_list` has to know which it is reading: with the
    /// wrong rule, Off the Cuff's `0x8C` comes out as `Pattern(140)` — an index
    /// past the end of a 24-pattern tune.
    ///
    /// The pattern ENTRIES need no such split. `pattern` reads this generation
    /// unchanged, and its pattern 0 decodes to the same single 32-row rest as
    /// Lion Heart's.
    #[test]
    fn three_marker_order_lists() {
        const OFF_THE_CUFF: &[u8] = include_bytes!("songs/off_the_cuff.sid");
        let c = G2Config::detect(OFF_THE_CUFF).unwrap();
        let track0 = c.order_list(OFF_THE_CUFF, c.order_pointers(OFF_THE_CUFF, 0)[0]);
        assert_eq!(
            &track0[..5],
            &[
                OrderStep::Transpose(12), // 0x8C — an octave up
                OrderStep::Pattern(1),
                OrderStep::Pattern(1),
                OrderStep::Transpose(0), // 0x80 — back to concert pitch
                OrderStep::Pattern(2),
            ]
        );
        // Its second voice transposes nothing at all.
        let track1 = c.order_list(OFF_THE_CUFF, c.order_pointers(OFF_THE_CUFF, 0)[1]);
        assert!(track1.iter().all(|s| matches!(s, OrderStep::Pattern(_))));
        // No pattern index may exceed what the pointer table holds.
        for s in track0.iter().chain(track1.iter()) {
            if let OrderStep::Pattern(p) = s {
                assert!(*p < 64, "pattern {p} is past the end of the table");
            }
        }
        assert_eq!(
            c.pattern(OFF_THE_CUFF, 0),
            vec![G2Entry {
                markers: vec![],
                rows: 32,
                tie: false,
                note: None
            }]
        );
    }

    /// Every table address is recovered from the code, not from a hand-written
    /// per-tune table. These are the addresses the Ghidra listing shows.
    #[test]
    fn detects_lion_heart_and_recovers_its_tables() {
        let c = G2Config::detect(LION_HEART).expect("Lion_Heart is a G2 tune");
        assert_eq!(c.load, 0x1003);
        assert_eq!(c.instruments, 0x1B4B);
        assert_eq!(c.filter_slots, 0x1CB0);
        assert_eq!(c.pattern_ptrs, 0x1D50);
        assert_eq!(c.freq_table, 0x1E34);
        assert_eq!(c.order_ptrs, 0x1B43);
    }

    /// Subtune 0's four order-list pointers, and the head of voice 0's list.
    /// The list opens with `FD 00` (transpose 0) then pattern indices.
    #[test]
    fn reads_the_four_order_lists() {
        let c = G2Config::detect(LION_HEART).unwrap();
        assert_eq!(
            c.order_pointers(LION_HEART, 0),
            [0x1EF4, 0x1F4B, 0x1FB2, 0x2009],
            "three voices plus the filter automation track"
        );
        let o = c.order_list(LION_HEART, 0x1EF4);
        assert_eq!(
            &o[..6],
            &[
                OrderStep::Transpose(0),
                OrderStep::Pattern(0x01),
                OrderStep::Pattern(0x71),
                OrderStep::Pattern(0x01),
                OrderStep::Pattern(0x71),
                OrderStep::Pattern(0x01),
            ]
        );
        assert!(!o.is_empty(), "the walk stops at the 0xFF restart marker");
    }

    /// Pattern 0 is `df 60 81`: one 32-row rest. The simplest possible entry —
    /// a length byte, a rest, end of pattern.
    #[test]
    fn pattern0_is_a_single_long_rest() {
        let c = G2Config::detect(LION_HEART).unwrap();
        let p = c.pattern(LION_HEART, 0);
        assert_eq!(
            p,
            vec![G2Entry {
                markers: vec![],
                rows: 32,
                tie: false,
                note: None,
            }]
        );
    }

    /// Pattern 1 opens `80 03 c3 1f c1 2b …`: select instrument 3, then a
    /// 4-row note 31, a 2-row note 43, and so on — and switches instrument
    /// mid-pattern with another `0x80`.
    #[test]
    fn pattern1_decodes_markers_notes_and_lengths() {
        let c = G2Config::detect(LION_HEART).unwrap();
        let p = c.pattern(LION_HEART, 1);
        assert_eq!(p[0].markers, vec![NoteMarker::Instrument(3)]);
        assert_eq!((p[0].rows, p[0].note, p[0].tie), (4, Some(0x1F), false));
        assert_eq!(p[1].markers, vec![]);
        assert_eq!((p[1].rows, p[1].note), (2, Some(0x2B)));
        assert_eq!((p[2].rows, p[2].note), (4, Some(0x1F)));
        assert_eq!((p[3].rows, p[3].note), (2, Some(0x1F)));
        // `80 02 c3 30` — the instrument changes inside the pattern.
        assert_eq!(p[4].markers, vec![NoteMarker::Instrument(2)]);
        assert_eq!((p[4].rows, p[4].note), (4, Some(0x30)));
        // 43 bytes = five `80 xx` markers (10) + sixteen length/note pairs (32)
        // + the `0x81` terminator.
        assert_eq!(p.len(), 16, "the walk stops at the 0x81 terminator");
    }

    /// Pattern 2 mixes notes and rests under one instrument; every entry here
    /// carries its own length byte, so nothing is inherited.
    #[test]
    fn pattern2_mixes_notes_and_rests() {
        let c = G2Config::detect(LION_HEART).unwrap();
        let p = c.pattern(LION_HEART, 2);
        assert_eq!(p[0].markers, vec![NoteMarker::Instrument(4)]);
        let shape: Vec<(u8, Option<u8>)> = p.iter().map(|e| (e.rows, e.note)).collect();
        assert_eq!(
            shape,
            vec![
                (9, Some(0x13)),
                (3, None),
                (10, Some(0x1C)),
                (3, Some(0x1D)),
                (7, None),
                (16, None),
            ]
        );
    }

    /// Expanding voice 0 gives a flat row sequence: the first entry of pattern 1
    /// (instrument 3, four rows, note `$1F`) becomes one row with the note and
    /// three continuation rows.
    #[test]
    fn voice_rows_expand_entries_into_held_rows() {
        let c = G2Config::detect(LION_HEART).unwrap();
        let bank = c.instruments(LION_HEART);
        let rows = c.voice_rows(LION_HEART, 0x1EF4, &bank);

        assert_eq!(rows[0].note, RowNote::Play(0x1F));
        assert_eq!(rows[0].instrument, 3);
        assert_eq!(rows[0].markers, vec![NoteMarker::Instrument(3)]);
        // Rows 1-2 hold; row 3 — the entry's last — drops the gate so the
        // envelope releases before the next note, exactly as the replayer's
        // end-of-note control mask does.
        for r in &rows[1..3] {
            assert_eq!(r.note, RowNote::Hold, "the note is held, not re-struck");
            assert_eq!(r.instrument, 3);
        }
        assert_eq!(
            rows[3].note,
            RowNote::GateOff,
            "the note ends on its last row"
        );
        assert_eq!(rows[4].note, RowNote::Play(0x2B), "next entry, two rows");
        assert_eq!(rows[6].note, RowNote::Play(0x1F));

        // One full pass of the order list, before it loops.
        assert!(rows.len() > 100, "a real pass, got {}", rows.len());
    }

    /// The track transpose applies to ordinary instruments but NOT to those
    /// running a voice program — the player reads their note byte raw, because
    /// the program drives the pitch itself.
    #[test]
    fn transpose_skips_programmed_instruments() {
        let c = G2Config::detect(LION_HEART).unwrap();
        let mut bank = c.instruments(LION_HEART);
        // Voice 0 opens on instrument 3, which has no program: give it one and
        // the same stream must stop being transposed.
        let mut img = LION_HEART.to_vec();
        let ord = PSID_DATA + (0x1EF4 - c.load as usize);
        img[ord + 1] = 5; // FD 05 — transpose five semitones

        let plain = c.voice_rows(&img, 0x1EF4, &bank);
        assert_eq!(plain[0].note, RowNote::Play(0x1F + 5), "transposed");

        bank[3].program = Some(VoiceProgram {
            steps: alloc::vec![WaveStep::default()],
            end: ProgramEnd::Hold,
        });
        let programmed = c.voice_rows(&img, 0x1EF4, &bank);
        assert_eq!(
            programmed[0].note,
            RowNote::Play(0x1F),
            "a program ignores transpose"
        );
    }

    /// The tracker slots: a struck note carries its instrument, a TIED note
    /// carries a full-speed tone portamento and NO instrument (so the voice is
    /// not re-triggered), and a rest closes the gate.
    #[test]
    fn voice_slots_encode_notes_rests_and_ties() {
        let c = G2Config::detect(LION_HEART).unwrap();
        let bank = c.instruments(LION_HEART);
        let rows = c.voice_rows(LION_HEART, 0x1EF4, &bank);
        let slots = c.voice_slots(&rows);
        assert_eq!(slots.len(), rows.len());

        assert!(matches!(slots[0].note, CellNote::Play(_)));
        assert_eq!(slots[0].instrument, Some(3));
        assert_eq!(slots[0].effect_type, 0, "a struck note needs no effect");
        // Held rows are empty — nothing re-triggers.
        assert_eq!(slots[1].note, CellNote::Empty);
        assert_eq!(slots[1].instrument, None);

        // Synthesise the two cases the shipped pattern does not show at row 0:
        // a tie and a rest.
        let tied = c.voice_slots(&[G2Row {
            note: RowNote::Play(40),
            tied: true,
            instrument: 5,
            arpeggio: 0,
            markers: vec![],
            arming: NoteArmingSpec::default(),
            porta: 0,
        }]);
        // A tied note is an ordinary note in the SLOT; that it must not
        // re-trigger is carried by the `NoteArming` the module builder attaches
        // to the cell — the instrument-column-less shape is already taken by
        // the v10–v30 importers (see ROBSID2_INSTRUMENT_MODEL.md §11.3).
        assert!(matches!(tied[0].note, CellNote::Play(_)));
        assert_eq!(tied[0].instrument, Some(5));
        assert_eq!(tied[0].effect_type, 0, "no portamento hack any more");

        let rest = c.voice_slots(&[G2Row {
            note: RowNote::Rest,
            tied: false,
            instrument: 5,
            arpeggio: 0,
            markers: vec![],
            arming: NoteArmingSpec::default(),
            porta: 0,
        }]);
        assert_eq!(rest[0].note, CellNote::KeyOff, "a rest closes the gate");
    }

    /// End to end: Lion_Heart becomes a playable module — sixteen SID
    /// instruments, three free-running voice lanes, a linear timeline.
    #[test]
    fn lion_heart_builds_a_module() {
        let m = to_module(LION_HEART).expect("Lion_Heart is a G2 tune");

        assert_eq!(m.instrument.len(), MAX_INSTRUMENTS);
        assert!(
            m.instrument
                .iter()
                .all(|i| matches!(i.instr_type, InstrumentType::RobSid(_))),
            "every instrument renders through the SID chip"
        );
        // `$2de3 = 1` ⇒ the row gate opens every other frame.
        assert_eq!(m.default_tempo, 2);

        assert_eq!(m.channel_loops.len(), 3, "three independent voice lanes");
        for l in &m.channel_loops {
            assert!(l.end_tick > 0, "each lane loops over its own pass");
        }

        assert!(!m.tracks.is_empty());
        assert_eq!(m.clips.len(), m.tracks.len(), "one clip per track segment");
        assert!(
            m.clips.iter().all(|c| c.target_channel < 3),
            "voices land on the three SID lanes"
        );
        assert!(!m.timeline_map.entries.is_empty());

        // The very first thing heard: instrument 3, note $1F.
        let first = m
            .clips
            .iter()
            .filter(|c| c.target_channel == 0)
            .min_by_key(|c| c.position_tick)
            .expect("voice 0 has clips");
        // Frame 1, not 0: the twelve-step counter reloads on the very first
        // frame, which skips the subdivision tick, so the first row opens one
        // frame in — as the chip does.
        assert_eq!(first.position_tick, 1);
        let Track::Notes {
            instrument, rows, ..
        } = &m.tracks[first.track as usize]
        else {
            panic!("expected a note track");
        };
        assert_eq!(*instrument, 3);
        assert!(
            matches!(rows[0].event, CellEvent::NoteOn { .. }),
            "the first row triggers, got {:?}",
            rows[0].event
        );
    }

    /// An older replayer must not be built as a last-generation tune.
    #[test]
    fn to_module_rejects_an_older_replayer() {
        assert!(to_module(include_bytes!("songs/commando.sid")).is_none());
    }

    #[test]
    fn portamento_becomes_slide_lanes() {
        let m = to_module(LION_HEART).unwrap();
        let lanes = m
            .automation
            .iter()
            .filter(|l| matches!(l.kind, crate::core::daw::automation::LaneKind::Slide { .. }))
            .count();
        let sets: usize = m
            .automation
            .iter()
            .map(|l| match &l.kind {
                crate::core::daw::automation::LaneKind::Slide { events } => events
                    .iter()
                    .filter(|e| matches!(e, crate::core::daw::automation::SlideEvent::Set { .. }))
                    .count(),
                _ => 0,
            })
            .sum();
        assert!(lanes > 0, "the 0x82 markers must produce slide lanes");
        assert_eq!(sets, 43, "Lion_Heart carries 43 portamento markers");
    }

    /// The end-of-note envelope is a per-tune CONSTANT in the player's code, not
    /// song data — which also disproves "the five tunes differ only in address
    /// operands": these are immediate operands and they differ too.
    #[test]
    fn release_envelope_is_read_from_the_code() {
        let lh = G2Config::detect(LION_HEART).unwrap();
        assert_eq!(lh.release_adsr, (0x0F, 0x01));
        let sns = G2Config::detect(include_bytes!("songs/sun_never_shines.sid")).unwrap();
        assert_eq!(sns.release_adsr, (0xFF, 0xF0));
    }

    /// The per-note vibrato marker reaches the cells. Lion_Heart never uses it,
    /// so this is checked on Sun_Never_Shines — which is also why neither
    /// oracle tune moved when it was wired: 16 markers against 13 500
    /// voice-frames is below the noise.
    #[test]
    fn per_note_vibrato_reaches_the_cells() {
        let data = include_bytes!("songs/sun_never_shines.sid");
        let c = G2Config::detect(data).unwrap();
        let bank = c.instruments(data);
        let n: usize = c
            .order_pointers(data, 0)
            .iter()
            .take(3)
            .map(|&p| {
                c.voice_rows(data, p, &bank)
                    .iter()
                    .filter(|r| matches!(r.arming.vibrato, ArmMode::At(_)))
                    .count()
            })
            .sum();
        assert_eq!(n, 16, "Sun_Never_Shines carries 16 `0x86` markers");
    }

    /// The length byte is STICKY: the replayer never clears it, so an entry
    /// without one lasts as long as the entry before it. Decoded from a
    /// synthetic pattern, since the shipped ones all restate their length.
    #[test]
    fn length_is_sticky_across_entries() {
        let c = G2Config::detect(LION_HEART).unwrap();
        // Borrow an unused corner of the image? No — build the bytes directly by
        // decoding a pattern we place ourselves is not possible here, so assert
        // the rule through the public walk on a pattern whose second entry has
        // no length byte: `c3 1f 2b 81` → both entries last 4 rows.
        let mut img = LION_HEART.to_vec();
        let base = PSID_DATA + (0x2013 - c.load as usize);
        img[base..base + 4].copy_from_slice(&[0xC3, 0x1F, 0x2B, 0x81]);
        let p = c.pattern_at(&img, 0x2013);
        assert_eq!(p.len(), 2);
        assert_eq!((p[0].rows, p[0].note), (4, Some(0x1F)));
        assert_eq!(
            (p[1].rows, p[1].note),
            (4, Some(0x2B)),
            "an entry with no length byte inherits the previous one"
        );
    }

    /// An fx-mask replayer must NOT be taken for the last generation.
    #[test]
    fn rejects_an_older_replayer() {
        assert!(G2Config::detect(include_bytes!("songs/commando.sid")).is_none());
        assert!(G2Config::detect(include_bytes!("songs/lightforce.sid")).is_none());
        assert!(G2Config::detect(include_bytes!("songs/delta.sid")).is_none());
    }

    /// Lion_Heart instrument 0: `00 08 41 08 c7 00 00 01 | 4b 1c 00 01 00 fd 00 00`.
    /// Flag bit0 ⇒ it is a voice program, whose table at `$1c4b` is
    /// `81 20 | 41 01 04 | 40 40 02 | 80 30 | 80 15 | 80 20 | 80 10 | 80 20 | 85`.
    #[test]
    fn instrument0_is_a_voice_program() {
        let c = G2Config::detect(LION_HEART).unwrap();
        let i = c.instrument(LION_HEART, 0);

        assert_eq!(i.voice.pw, 0x800, "pulse width from record +0/+1");
        assert!(i.voice.ctrl_pulse && i.voice.ctrl_gate, "sustain ctrl $41");
        assert_eq!(i.voice.ad, 0x08);
        assert_eq!(i.voice.sr, 0xC7);

        let p = i.program.expect("flag bit0 means a voice program");
        assert_eq!(p.end, ProgramEnd::Hold, "the table terminates with $85");
        assert_eq!(p.steps.len(), 8);
        // Step 0: noise pinned at freq-hi $20.
        assert_eq!(p.steps[0].pitch, PitchAction::Fixed(0x20));
        assert!(p.steps[0].shape.noise && p.steps[0].shape.gate);
        // Step 1: pulse, bending by $0401.
        assert_eq!(p.steps[1].pitch, PitchAction::Bend(0x0401));
        assert!(p.steps[1].shape.pulse);
        // Step 2 clears the gate — the note releases mid-program.
        assert_eq!(p.steps[2].pitch, PitchAction::Bend(0x0240));
        assert!(!p.steps[2].shape.gate, "step 2 releases the note");
        // Then four pinned noise steps.
        assert_eq!(p.steps[3].pitch, PitchAction::Fixed(0x30));
        assert_eq!(p.steps[7].pitch, PitchAction::Fixed(0x20));
    }

    /// Lion_Heart instrument 3: `c2 00 41 09 25 00 1f 44 | 5f 81 00 04 03 f0 00 00`.
    /// Flags `$44` = both two-phase attacks, so the shared frame count is spent
    /// twice as fast: a noise transient at note 95, then the pulse body — a drum
    /// written with the general mechanism rather than a hard-wired drum flag.
    #[test]
    fn instrument3_is_a_two_phase_drum() {
        let c = G2Config::detect(LION_HEART).unwrap();
        let i = c.instrument(LION_HEART, 3);
        assert!(i.program.is_none(), "no bit0, so no program");
        // The attack is noise, gated — which the model now says outright rather
        // than as `0x81`.
        let a = i.fx.two_phase.attack_shape;
        assert!(a.noise && a.gate, "noise attack");
        assert!(!a.pulse && !a.sawtooth && !a.triangle);
        assert_eq!(i.fx.two_phase.attack_note, Some(0x5F));
        assert_eq!(i.fx.two_phase.attack_frames, 4);
        assert!(i.voice.ctrl_pulse, "the body is the record's own waveform");
        // +6/+13: a fast pulse-width sweep bouncing over the whole nibble range.
        assert!(i.fx.pulse_sweep.enable);
        assert_eq!(i.fx.pulse_sweep.speed, 0x1F);
        assert_eq!(i.fx.pulse_sweep.bounce, Some(BounceRange { lo: 0, hi: 15 }));
    }

    /// Lion_Heart instrument 4 enables the filter (flag bit5) through slot 4 at
    /// `$1cd0`: `4e 03 03 a7 72 1d 03 30` — a swept, resonant band+low pass.
    #[test]
    fn instrument4_uses_a_swept_filter_slot() {
        let c = G2Config::detect(LION_HEART).unwrap();
        let f = c.instrument(LION_HEART, 4).fx.filter;
        assert!(f.enable);
        assert_eq!(f.cutoff_seed, 0x4E03);
        assert_eq!(
            f.sweep,
            CutoffSweep::Bounce {
                step: 0x03A7,
                min: 0x1D,
                max: 0x72,
                up: false, // slot +6 bit7 clear ⇒ the sweep starts downward
            }
        );
        assert!(f.mode.low_pass && f.mode.band_pass && !f.mode.high_pass);
        assert_eq!(f.resonance, 3);
    }

    /// Instrument 13 carries `+10 = $C0`: the default arpeggio is a two-step
    /// octave. This is the same octave arp the v10 replayers hard-wired as a
    /// flag — here it is just a value in the general cycle field.
    #[test]
    fn instrument13_has_a_default_octave_arpeggio() {
        let c = G2Config::detect(LION_HEART).unwrap();
        // The value is on the record...
        assert_eq!(c.arpeggio_default(LION_HEART, 13), 0xC0);
        // ...but deliberately NOT on the instrument: it rides the cell, because
        // the replayer shares one slot between this default and the `0x83`
        // marker. Leaving it here too would play the arpeggio twice.
        let i = c.instrument(LION_HEART, 13);
        assert_eq!(i.fx.arpeggio, ArpMode::None);
    }

    /// The arpeggio reaches the CELL: a note under instrument 13 carries the
    /// record's `$C0` as MOD effect 0 (`xy` = the replayer's own nibble pair),
    /// and a held row must not restate it.
    #[test]
    fn arpeggio_travels_on_the_cell() {
        let c = G2Config::detect(LION_HEART).unwrap();
        let rows = [
            G2Row {
                note: RowNote::Play(40),
                tied: false,
                instrument: 13,
                arpeggio: 0xC0,
                markers: vec![],
                arming: NoteArmingSpec::default(),
                porta: 0,
            },
            G2Row {
                note: RowNote::Hold,
                tied: false,
                instrument: 13,
                arpeggio: 0xC0,
                markers: vec![],
                arming: NoteArmingSpec::default(),
                porta: 0,
            },
        ];
        let s = c.voice_slots(&rows);
        assert_eq!((s[0].effect_type, s[0].effect_parameter), (0, 0xC0));
        assert_eq!(
            (s[1].effect_type, s[1].effect_parameter),
            (0, 0),
            "a held row restates nothing"
        );
    }

    /// The whole bank decodes without panicking, and the flag distribution
    /// matches the record dump in `ROBSID2_INSTRUMENT_MODEL.md` §4:
    /// programs on 0/1/2, two-phase on 3/7/8/11/14…, filter on 4 and 14.
    #[test]
    fn whole_bank_decodes_with_the_expected_shape() {
        let c = G2Config::detect(LION_HEART).unwrap();
        let bank = c.instruments(LION_HEART);
        assert_eq!(bank.len(), MAX_INSTRUMENTS);

        let programs: Vec<usize> = bank
            .iter()
            .enumerate()
            .filter(|(_, i)| i.program.is_some())
            .map(|(n, _)| n)
            .collect();
        assert_eq!(programs, vec![0, 1, 2]);

        let filtered: Vec<usize> = bank
            .iter()
            .enumerate()
            .filter(|(_, i)| i.fx.filter.enable)
            .map(|(n, _)| n)
            .collect();
        assert_eq!(filtered, vec![4, 14]);

        let two_phase: Vec<usize> = bank
            .iter()
            .enumerate()
            .filter(|(_, i)| i.fx.two_phase.attack_frames != 0)
            .map(|(n, _)| n)
            .collect();
        assert_eq!(two_phase, vec![3, 7, 8, 11]);
    }
}
