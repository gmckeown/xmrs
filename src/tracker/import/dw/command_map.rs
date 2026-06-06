#![forbid(unsafe_code)]

//! Per-module **low-command profile** for David Whittaker replayers
//! whose track commands `0x80..0x9F` are dispatched through a 68k
//! *jump table* (`JMP (0,A3,A2)`) rather than the canonical
//! fixed cmd-code layout decoded in [`super::event::decode_track`].
//!
//! ## Why this exists
//!
//! Most new-player modules (xenon2, tetris, …) lay their commands
//! out in the canonical order baked into `decode_track`'s
//! `match b & 0x7F` (cmd 1 = Slide, cmd 6 = StartVibrato, …). But
//! some builds — the `bubble bobble` / C3 family — remap the low
//! commands through a per-module jump table. Their cmd codes mean
//! *different effects with different argument counts*:
//!
//! | code | canonical            | bubble bobble (real)   |
//! |------|----------------------|------------------------|
//! | 1    | Slide (2 args)       | **Vibrato** (2)        |
//! | 6    | StartVibrato (2)     | **channel transpose** (1) |
//! | 7    | StopVibrato (0)      | **Slide ON** (2)       |
//! | 8    | Effect8 (1)          | **Slide OFF** (0)      |
//!
//! Decoding such a module with the canonical table consumes the
//! wrong number of argument bytes after cmds 6/7/8 → the whole
//! track stream desyncs → chaotic playback (the C3 symptom).
//!
//! ## How it works
//!
//! All addresses are Ghidra-confirmed against `bubble bobble.dw`
//! (project xenon2; `do_track_cmd @0x1a8`, dispatch table `@0x54c`).
//! Because `image_base == 0`, a Ghidra address equals a payload
//! file offset.
//!
//! The dispatcher tail in `do_track_cmd` is:
//!
//! ```text
//!   ADD.B  D0,D0                 ; D0 = (note-0x80)*2
//!   LEA    (d16,PC),A2           ; A2 = table base
//!   MOVEA.W (0,A2,D0.w),A2       ; A2 = table[cmd]
//!   JMP    (0,A3,A2.w)           ; handler = A3 + table[cmd]
//! ```
//!
//! `A3` is the replayer's base register, set by `dw_init`'s
//! `LEA (-d,PC),A3` to a *negative* offset (`-0x442` on bubble
//! bobble). So `handler_file_offset = (table[cmd] + a3_base) as
//! payload offset`. We resolve the table, walk its entries, and
//! classify each handler by an opcode **signature** near its
//! entry. The argument count is implied by the classified kind
//! (the eleven handlers were reverse-engineered by hand).

use alloc::vec::Vec;

/// The effect a remapped low-command performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DwCmdKind {
    /// Arm the channel pitch-LFO (`BSET #1,(0,A0)` + speed/depth).
    Vibrato,
    /// Canonical 2-arg pitch slide (cmd 1 on the new player). Its
    /// handler clears the slide accumulator (`CLR.W (0x20,A0)` =
    /// `42 68 00 20`), reads `speed`/`counter` from the stream, then
    /// `BSET #1,(0,A0)`. The `BSET #1` is shared with the
    /// bubble-bobble family's *vibrato* arm, so this entry-anchored
    /// `CLR.W (0x20,A0)` signature is what disambiguates them — without
    /// it the canonical slide is misread as a vibrato and every slid
    /// note plays its raw (un-glided) pitch.
    Slide,
    /// Arm a linear pitch slide (`ST (1,A0)` + speed/target).
    SlideOn,
    /// Clear the pitch slide (`SF (1,A0)`).
    SlideOff,
    /// Per-channel transpose (`MOVE.B (A1)+,(0x2f,A0)`).
    ChannelTranspose,
    /// Song-global transpose (`MOVE.B (A1)+,(global via A3)`).
    GlobalTranspose,
    /// Silence / note-off (re-points Paula at the silence sample).
    NoteStop,
    /// Re-trigger the held note (jumps to the finish-note path).
    Retrigger,
    /// Reset / all-notes-off (`FUN_ec`: zeroes voices + Paula).
    ResetAll,
    /// Advance the per-channel position sequencer.
    SeqAdvance,
    /// Set the per-channel position-sequencer pointer.
    SeqPtr,
    /// Write a song-global parameter word (`MOVE.W D0,(global)`).
    GlobalParam,
    /// A handler we located but could not classify.
    Unknown,
}

impl DwCmdKind {
    /// Number of inline argument bytes the handler pulls from the
    /// track stream (`(A1)+` reads). Hand-verified per handler.
    pub fn arg_bytes(self) -> u8 {
        match self {
            DwCmdKind::Vibrato => 2,
            DwCmdKind::Slide => 2,
            DwCmdKind::SlideOn => 2,
            DwCmdKind::SlideOff => 0,
            DwCmdKind::ChannelTranspose => 1,
            DwCmdKind::GlobalTranspose => 1,
            DwCmdKind::NoteStop => 0,
            DwCmdKind::Retrigger => 0,
            DwCmdKind::ResetAll => 0,
            DwCmdKind::SeqAdvance => 0,
            DwCmdKind::SeqPtr => 2,
            DwCmdKind::GlobalParam => 1,
            // Be conservative: an unknown handler is assumed to
            // consume no bytes (so we don't *introduce* a desync).
            DwCmdKind::Unknown => 0,
        }
    }
}

/// One resolved low-command: its kind and inline argument count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DwCmd {
    pub kind: DwCmdKind,
    pub args: u8,
}

/// Per-module command profile for jump-table-dispatched builds.
///
/// `entries[i]` describes track command byte `0x80 + i`. `None`
/// means the table slot is padding / unused (the handler address
/// was zero or out of range).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DwCommandMap {
    pub entries: Vec<Option<DwCmd>>,
}

impl DwCommandMap {
    /// Look up command byte `b` (`0x80..=0x8F`). Returns `None`
    /// for notes/brackets/long-waits or unmapped slots.
    pub fn get(&self, b: u8) -> Option<DwCmd> {
        if b < 0x80 {
            return None;
        }
        let idx = (b - 0x80) as usize;
        self.entries.get(idx).copied().flatten()
    }
}

/// Opcode constants used by the signature matcher (big-endian
/// 16-bit words at a handler's entry).
mod sig {
    /// `BSET #1,(0,A0)` — vibrato arm. Full 6-byte form:
    /// `08 E8 00 01 00 00`.
    pub const BSET1_A0: [u8; 6] = [0x08, 0xE8, 0x00, 0x01, 0x00, 0x00];
    /// `CLR.W (0x20,A0)` — entry of the canonical 2-arg slide handler
    /// (clears the slide accumulator `chan+0x618`). `42 68 00 20`.
    /// Anchors [`super::DwCmdKind::Slide`] ahead of the shared
    /// `BSET #1,(0,A0)` so the canonical slide isn't read as a vibrato.
    pub const CLRW_20_A0: [u8; 4] = [0x42, 0x68, 0x00, 0x20];
    /// `ST (0x1,A0)` — slide on. `50 E8 00 01`.
    pub const ST_1_A0: [u8; 4] = [0x50, 0xE8, 0x00, 0x01];
    /// `SF (0x1,A0)` — slide off. `51 E8 00 01`.
    pub const SF_1_A0: [u8; 4] = [0x51, 0xE8, 0x00, 0x01];
    /// `MOVE.B (A1)+,(0x2f,A0)` — channel transpose. `11 59 00 2F`.
    pub const MOVB_A1_2F: [u8; 4] = [0x11, 0x59, 0x00, 0x2F];
    /// `MOVE.B (A1)+,(0x3,A0)` — the empire-era per-channel transpose.
    /// Same opcode as [`MOVB_A1_2F`] but the transpose slot lives at
    /// channel offset `0x3` instead of `0x2f` (Ghidra: empire `play`
    /// handler @0x53c, read back by the note trigger's
    /// `ADD.B (0x3,A0),D0` @0x29a). Both displacements are confirmed
    /// channel-transpose slots in the Whittaker family; no other
    /// command writes `(A1)+` into either slot.
    pub const MOVB_A1_03: [u8; 4] = [0x11, 0x59, 0x00, 0x03];
}

/// `true` when `region` contains the DMACON channel-kill =
/// `MOVE.W #0x000X, (0x00DFF096).L` (`33 FC 00 0X 00 DF F0 96`) where
/// `0X` is the active-channel mask cleared on a song STOP: `0x0F` on a
/// 4-voice player, `0x07` on a 3-voice one (sentinel). The high byte is
/// `0x00` (no SET/CLR bit) so it CLEARS the DMA bits. Only the
/// stop/silence routine ever writes this — per-note DMA writes set the
/// bits (`...| 0x8200`) — so a match unambiguously identifies a stop.
fn dmacon_kill_in(region: &[u8]) -> bool {
    region.windows(8).any(|c| {
        c[0] == 0x33
            && c[1] == 0xFC
            && c[2] == 0x00
            && (c[3] == 0x0F || c[3] == 0x07)
            && c[4] == 0x00
            && c[5] == 0xDF
            && c[6] == 0xF0
            && c[7] == 0x96
    })
}

/// Classify a single handler given the payload and its file
/// offset. Looks at a small window of opcodes from the entry.
fn classify_handler(payload: &[u8], off: usize) -> DwCmd {
    // Bound the window to *this* handler's own body. Every handler
    // ends in `BRA.W` (`60 00 <disp16>`), and 68k instructions are
    // word-aligned, so cut at the first even-offset `60 00`. A
    // 0x40-byte cap stops a missing terminator from bleeding across
    // the image. Without this the window overruns into the next
    // handler and `has()` finds that neighbour's opcodes (e.g. the
    // 8-byte GlobalTranspose handler would inherit the following
    // ChannelTranspose's `MOVE.B (A1)+,(0x2f,A0)`).
    //
    // A bare `BRA.W` (`60 00 <disp16>`) handler has an empty body (the
    // branch is at offset 0), so the opcode-signature scan below can't
    // see it. Resolve the branch target and classify by what lives
    // there. Two such handlers exist on the bubble bobble family:
    //  - cmd 0x84 → the global silence routine writing `DMACON = 0x000F`
    //    (kill all Paula DMA) = a song STOP. Verified vs the Paula oracle
    //    (song 0 goes hard-silent at tick 444 when ch1 hits this byte).
    //  - cmd 0x83 → the re-trigger routine @0x2fc, which starts with
    //    `MOVE.L A1,(0x4,A0)` (rewrite the channel's stream pointer to
    //    just-after the byte) then re-strikes Paula DMA = re-sound the
    //    CURRENT note without consuming a new one. Verified vs the oracle
    //    (song 1 ch2 desyncs exactly at the first 0x83, tick 732: the
    //    replayer holds/repeats the note, the un-modelled importer skips
    //    the extra strike and drifts).
    // Resolve a branch/call at the handler entry and classify by what
    // lives at the target. The stop (cmd 0x84) and re-trigger (cmd 0x83)
    // handlers don't carry their own opcodes — they jump/call a shared
    // routine. Forms seen: bare `BRA.W` (bubble bobble), `BSR.W`/`BSR.B`
    // (sentinel's 3-voice stop calls a silence helper), and `JSR d16(PC)`.
    let branch_target: Option<usize> = match payload.get(off..off + 2) {
        // BRA.W / BSR.W / JSR d16(PC): 16-bit PC-relative displacement.
        Some(&[0x60, 0x00]) | Some(&[0x61, 0x00]) | Some(&[0x4E, 0xBA]) => payload
            .get(off + 2..off + 4)
            .map(|d| off as isize + 2 + i16::from_be_bytes([d[0], d[1]]) as isize),
        // BSR.B: 8-bit PC-relative displacement (the `61 00` BSR.W form
        // is handled above; any other low byte is the BSR.B displacement).
        Some(&[0x61, b]) => Some(off as isize + 2 + (b as i8) as isize),
        _ => None,
    }
    .filter(|&t| t >= 0)
    .map(|t| t as usize);
    if let Some(t) = branch_target {
        let scan_end = (t + 0x60).min(payload.len());
        // `MOVE.L A1,(0x4,A0)` at the target entry — the re-trigger
        // routine's first instruction.
        const RETRIG_ENTRY: [u8; 4] = [0x21, 0x49, 0x00, 0x04];
        if payload.get(t..t + 4) == Some(&RETRIG_ENTRY) {
            return DwCmd {
                kind: DwCmdKind::Retrigger,
                args: DwCmdKind::Retrigger.arg_bytes(),
            };
        }
        if payload.get(t..scan_end).map(dmacon_kill_in).unwrap_or(false) {
            return DwCmd {
                kind: DwCmdKind::ResetAll,
                args: DwCmdKind::ResetAll.arg_bytes(),
            };
        }
    }

    let cap = (off + 0x40).min(payload.len());
    let mut end = cap;
    let mut j = off;
    while j + 1 < cap {
        if payload[j] == 0x60 && payload[j + 1] == 0x00 {
            end = j;
            break;
        }
        j += 2;
    }
    let w = match payload.get(off..end) {
        Some(s) => s,
        None => return DwCmd { kind: DwCmdKind::Unknown, args: 0 },
    };

    // Song STOP with the DMACON kill *inline* in the handler body
    // (`MOVE.W #0x000F, (0x00DFF096).L` = clear the channel DMA bits,
    // halting the song). The `BRA.W`-to-kill form above covers bad
    // bobble-family stops that branch to a shared routine; some
    // canonical modules (archipelagos / fright night) instead inline
    // the silence in the cmd 0x84 handler's own `case` block. Only the
    // stop routine ever writes `DMACON = 0x000F`, so finding the kill
    // anywhere in the bounded body unambiguously identifies a stop.
    // Verified vs the Paula oracle (archipelagos: `DAT_630` playing-flag
    // clears + `dma 0x207→0x200` exactly when ch0 reads the 0x84 byte).
    if dmacon_kill_in(w) {
        return DwCmd {
            kind: DwCmdKind::ResetAll,
            args: DwCmdKind::ResetAll.arg_bytes(),
        };
    }

    // `has` = contains anywhere in the bounded body; `starts` =
    // at the handler entry. Prefer `starts` for entry signatures.
    let has = |needle: &[u8]| -> bool { w.windows(needle.len()).any(|c| c == needle) };
    let starts = |needle: &[u8]| -> bool { w.starts_with(needle) };

    // Order matters: most-specific / entry-anchored signatures first.
    let kind = if starts(&sig::CLRW_20_A0) {
        // CLR.W (0x20,A0) entry → the canonical 2-arg slide. Must come
        // before the `BSET #1,(0,A0)` rule below: this handler also
        // contains `BSET #1` (it sets the slide-active flag), but the
        // bubble-bobble *vibrato* uses the same `BSET #1` — only the
        // slide clears the accumulator at `0x20(A0)` on entry.
        DwCmdKind::Slide
    } else if starts(&sig::SF_1_A0) {
        DwCmdKind::SlideOff
    } else if starts(&sig::ST_1_A0) {
        DwCmdKind::SlideOn
    } else if has(&sig::BSET1_A0) {
        // BSET #1,(0,A0) (after the clr + 2 arg reads) → vibrato.
        DwCmdKind::Vibrato
    } else if starts(&sig::MOVB_A1_2F) || starts(&sig::MOVB_A1_03) {
        // MOVE.B (A1)+,(<slot>,A0) → per-channel transpose. The slot
        // is 0x2f on the bubble-bobble family and 0x3 on the empire
        // family; both are the channel-transpose byte the note trigger
        // adds to the period index.
        DwCmdKind::ChannelTranspose
    } else if starts(&[0x11, 0x59, 0x00, 0x08]) {
        // MOVE.B (A1)+,(0x8,A0) → set position-sequencer pointer.
        DwCmdKind::SeqPtr
    } else if starts(&[0x30, 0x28, 0x00, 0x0A]) {
        // MOVE.W (0xa,A0),D0 → position-sequencer advance.
        DwCmdKind::SeqAdvance
    } else if starts(&[0x21, 0x49, 0x00, 0x04]) {
        // MOVE.L A1,(0x4,A0) *inline* at the handler entry → the
        // note-hold / re-trigger routine (cmd 0x83 on the totalrecall
        // family). It is followed by `MOVE.W (0x1c,A0),(0x1e,A0)`
        // (reload the channel duration counter) with NO DMA clear, so
        // the note is *held* for another LongWait window rather than
        // re-attacked. Same entry the `branch_target` check above
        // recognises for the bobble family, but the totalrecall build
        // inlines it instead of `BRA.W`-ing to a shared routine. Left
        // as `Retrigger`; `event.rs` maps it to `WaitUntilNextRow`
        // under `note_repeat_is_tie` (the `CMPI.B #$83,(A1)` guard,
        // present here), exactly like the canonical `0x83`. Without
        // this the handler fell through to `Unknown` (0 args, no
        // event) and every `0x83` hold was silently dropped —
        // halving sustained notes (totalrecall-ingame drones).
        DwCmdKind::Retrigger
    } else if starts(&[0x17, 0x59]) {
        // MOVE.B (A1)+,(d16,A3) → a song-global byte (transpose).
        DwCmdKind::GlobalTranspose
    } else if has(&[0x2D, 0x7A]) {
        // MOVE.L (d16,PC),(0xa0,A6) → writes Paula AUDxLC = silence.
        DwCmdKind::NoteStop
    } else if has(&[0x37, 0x40]) {
        // MOVE.W D0,(d16,A3) → song-global parameter word.
        DwCmdKind::GlobalParam
    } else {
        DwCmdKind::Unknown
    };

    DwCmd { kind, args: kind.arg_bytes() }
}

/// Locate the `JMP (0,A3,A2.w)` dispatcher and resolve its table.
///
/// Returns the table's file offset. A handler file offset is then
/// `table_entry + a3_base`, where `a3_base` is the resolution of
/// `dw_init`'s `LEA (-d,PC),A3` — we do not re-derive it here, the
/// caller passes the value the detection layer already computed for
/// the module's base register.
fn find_dispatch_table(payload: &[u8]) -> Option<usize> {
    // Signature: `ADD.B D0,D0` (`D0 00`) ; `LEA (d16,PC),A2`
    // (`45 FA <d16>`) ; `MOVEA.W (0,A2,D0.w),A2` (`34 72 00 00`) ;
    // `JMP (0,A3,A2.w)` (`4E F3 A0 00`). We anchor on the LEA →
    // MOVEA → JMP triple to avoid false positives.
    let mut i = 0;
    while i + 12 <= payload.len() {
        if payload[i] == 0x45
            && payload[i + 1] == 0xFA
            && payload[i + 4] == 0x34
            && payload[i + 5] == 0x72
            && payload[i + 6] == 0x00
            && payload[i + 7] == 0x00
            && payload[i + 8] == 0x4E
            && payload[i + 9] == 0xF3
            && payload[i + 10] == 0xA0
            && payload[i + 11] == 0x00
        {
            // LEA (d16,PC),A2 : table = (i+2) + d16.
            let disp = i16::from_be_bytes([payload[i + 2], payload[i + 3]]) as isize;
            let table = (i as isize) + 2 + disp;
            if table >= 0 && (table as usize) < payload.len() {
                return Some(table as usize);
            }
        }
        i += 2;
    }
    None
}

/// Build the per-module command map, or `None` when the module
/// does not use a jump-table dispatcher (the vast majority —
/// they keep the canonical [`super::event::decode_track`] path).
///
/// * `payload` — the raw module image (file offset == address).
/// * `a3_base` — the module's base register value (`dw_init`'s
///   `LEA (-d,PC),A3`), as a signed offset; handler file offset
///   is `table_entry + a3_base`.
pub fn detect_command_map(payload: &[u8], a3_base: isize) -> Option<DwCommandMap> {
    let table = find_dispatch_table(payload)?;

    // The jump table covers command bytes 0x80..0x8F (16 word
    // slots). Real handlers cluster at the low codes; high slots
    // are zero padding.
    let mut entries: Vec<Option<DwCmd>> = Vec::with_capacity(16);
    let mut any = false;
    for k in 0..16usize {
        let eoff = table + k * 2;
        let raw = match payload.get(eoff..eoff + 2) {
            Some(b) => u16::from_be_bytes([b[0], b[1]]),
            None => break,
        };
        if raw == 0 {
            entries.push(None);
            continue;
        }
        let handler = raw as isize + a3_base;
        if handler < 0 || (handler as usize) >= payload.len() {
            entries.push(None);
            continue;
        }
        let cmd = classify_handler(payload, handler as usize);
        any = true;
        entries.push(Some(cmd));
    }

    if !any {
        return None;
    }
    Some(DwCommandMap { entries })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real handler byte sequences decoded from `bubble bobble.dw`
    // (Ghidra), each starting at its handler entry.

    #[test]
    fn classifies_canonical_slide() {
        // H @0x4b8: clr.w (0x20,A0); 2× move.b (A1)+; bset #1,(0,A0); bra.
        // This is the canonical 2-arg SLIDE: it clears the slide
        // accumulator at `0x20(A0)`, reads speed+counter, then sets the
        // slide-active flag (`BSET #1`). It was previously misread as a
        // vibrato — the `BSET #1` is shared with the bubble-bobble
        // vibrato, but only the slide clears `0x20(A0)` on entry. Ghidra
        // (`xenon2/beast play_tick` case 0x81) confirms the slide
        // semantics (period drift via `0x60c/0x60d`).
        let h = [
            0x42, 0x68, 0x00, 0x20, 0x11, 0x59, 0x00, 0x14, 0x11, 0x59, 0x00, 0x15, 0x08, 0xE8,
            0x00, 0x01, 0x00, 0x00, 0x60, 0x00, 0xFD, 0xB2,
        ];
        let c = classify_handler(&h, 0);
        assert_eq!(c.kind, DwCmdKind::Slide);
        assert_eq!(c.args, 2);
    }

    #[test]
    fn classifies_slide_on_off() {
        // SlideOn @0x512: st (0x1,A0); 2× move.b (A1)+; clr.b (0x2d,A0); bra.
        let on = [
            0x50, 0xE8, 0x00, 0x01, 0x11, 0x59, 0x00, 0x2C, 0x11, 0x59, 0x00, 0x2E, 0x42, 0x28,
            0x00, 0x2D, 0x60, 0x00, 0xFD, 0x5A,
        ];
        let c = classify_handler(&on, 0);
        assert_eq!(c.kind, DwCmdKind::SlideOn);
        assert_eq!(c.args, 2);

        // SlideOff @0x526: sf (0x1,A0); bra.
        let off = [0x51, 0xE8, 0x00, 0x01, 0x60, 0x00, 0xFD, 0x52];
        let c = classify_handler(&off, 0);
        assert_eq!(c.kind, DwCmdKind::SlideOff);
        assert_eq!(c.args, 0);
    }

    #[test]
    fn classifies_transposes() {
        // Channel transpose @0x50a: move.b (A1)+,(0x2f,A0); bra.
        let ch = [0x11, 0x59, 0x00, 0x2F, 0x60, 0x00, 0xFD, 0x6E];
        let c = classify_handler(&ch, 0);
        assert_eq!(c.kind, DwCmdKind::ChannelTranspose);
        assert_eq!(c.args, 1);

        // Channel transpose, empire-era slot 0x3 (Ghidra: empire `play`
        // handler @0x53c — `move.b (A1)+,(0x3,A0)`, read back by the
        // note trigger's `add.b (0x3,A0),D0`). Must also classify as
        // ChannelTranspose (args=1) — otherwise the value byte is left
        // in the stream and every later per-channel event misaligns.
        let ch03 = [0x11, 0x59, 0x00, 0x03, 0x60, 0x00, 0xFD, 0x6E];
        let c = classify_handler(&ch03, 0);
        assert_eq!(c.kind, DwCmdKind::ChannelTranspose);
        assert_eq!(c.args, 1);

        // Global transpose @0x502: move.b (A1)+,(0x9a6,A3); bra.
        let gl = [0x17, 0x59, 0x09, 0xA6, 0x60, 0x00, 0xFD, 0x76];
        let c = classify_handler(&gl, 0);
        assert_eq!(c.kind, DwCmdKind::GlobalTranspose);
        assert_eq!(c.args, 1);
    }

    #[test]
    fn classifies_note_stop() {
        // NoteStop @0x4ce (truncated to the distinctive part):
        // move.w (0x1c,A0),(0x1e,A0); move.l A1,(0x4,A0);
        // move.l (d16,PC),(0,A0); move.w #0x20,(0x4,A0);
        // tst.b (0xb,A0); bne; move.l (d16,PC),(0xa0,A6); …
        let h = [
            0x31, 0x68, 0x00, 0x1C, 0x00, 0x1E, 0x21, 0x49, 0x00, 0x04, 0x29, 0x7A, 0x00, 0x92,
            0x00, 0x00, 0x39, 0x7C, 0x00, 0x20, 0x00, 0x04, 0x4A, 0x2C, 0x00, 0x0B, 0x66, 0x0C,
            0x2D, 0x7A, 0x00, 0x80, 0x00, 0xA0,
        ];
        let c = classify_handler(&h, 0);
        assert_eq!(c.kind, DwCmdKind::NoteStop);
        assert_eq!(c.args, 0);
    }

    #[test]
    fn arg_counts_match_handcheck() {
        // The desync-relevant divergences from the canonical table.
        assert_eq!(DwCmdKind::Vibrato.arg_bytes(), 2); // code 1
        assert_eq!(DwCmdKind::ChannelTranspose.arg_bytes(), 1); // code 6 (canon=2)
        assert_eq!(DwCmdKind::SlideOn.arg_bytes(), 2); // code 7 (canon=0)
        assert_eq!(DwCmdKind::SlideOff.arg_bytes(), 0); // code 8 (canon=1)
    }

    /// End-to-end: detect the command map on the real
    /// `bubble bobble.dw` and confirm the C3-cluster remap. Skips
    /// silently when the corpus file isn't present.
    #[test]
    fn detects_bubble_bobble_map() {
        let home = match std::env::var("HOME") {
            Ok(h) => h,
            Err(_) => return,
        };
        let path = std::path::Path::new(&home).join("Music/dw/bubble bobble.dw");
        let payload = match std::fs::read(&path) {
            Ok(p) => p,
            Err(_) => return,
        };
        // a3_base = (init_offset + 2) + signed_disp16, exactly as
        // `detect::detect` computes `start_offset`.
        let layout = super::super::detect::detect(&payload).expect("bubble bobble must parse");
        let io = layout.init_offset;
        let disp = i16::from_be_bytes([payload[io + 2], payload[io + 3]]) as isize;
        let a3_base = (io as isize + 2) + disp;
        assert_eq!(a3_base, -0x442, "bubble bobble A3 base");

        let map = detect_command_map(&payload, a3_base).expect("jump-table dispatcher");
        // Real mapping read off the @0x54c table (handlers are NOT
        // laid out in command-code order in memory — the table is
        // the only authority).
        assert_eq!(map.get(0x80).map(|c| c.kind), Some(DwCmdKind::SeqAdvance));
        // cmd 0x81's handler (@0x4b8 = CLR.W (0x20,A0) + BSET #1) is the
        // canonical SLIDE, not a vibrato (the two share `BSET #1`; only
        // the slide clears the accumulator on entry). Verified vs the
        // oracle: reclassifying it Slide leaves bubble bobble's trace
        // byte-identical (it makes no audible use of cmd 0x81 in song 0)
        // while fixing the slide-as-vibrato defect on the rest of the
        // canonical-jump-table family (xenon2/beast/obliterator/bad
        // company/empire).
        assert_eq!(map.get(0x81).map(|c| c.kind), Some(DwCmdKind::Slide));
        assert_eq!(map.get(0x82).map(|c| c.kind), Some(DwCmdKind::NoteStop));
        assert_eq!(map.get(0x85).map(|c| c.kind), Some(DwCmdKind::GlobalTranspose));
        assert_eq!(map.get(0x86).map(|c| c.kind), Some(DwCmdKind::SlideOn));
        assert_eq!(map.get(0x87).map(|c| c.kind), Some(DwCmdKind::SlideOff));
        assert_eq!(map.get(0x88).map(|c| c.kind), Some(DwCmdKind::ChannelTranspose));
        assert_eq!(map.get(0x89).map(|c| c.kind), Some(DwCmdKind::SeqPtr));
        assert_eq!(map.get(0x8A).map(|c| c.kind), Some(DwCmdKind::GlobalParam));
        // Arg counts (these happen to match the canonical layout, so
        // the C3 defect is semantic, not a stream desync).
        assert_eq!(map.get(0x81).map(|c| c.args), Some(2)); // Vibrato
        assert_eq!(map.get(0x86).map(|c| c.args), Some(2)); // SlideOn
        assert_eq!(map.get(0x87).map(|c| c.args), Some(0)); // SlideOff
        assert_eq!(map.get(0x88).map(|c| c.args), Some(1)); // ChannelTranspose
    }
}
