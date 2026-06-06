#![forbid(unsafe_code)]

//! Detection of a `.dw` file and extraction of its init-time
//! constants.
//!
//! No 68000 disassembler is needed. The replayer carries a handful
//! of PC-relative instructions whose operands are exactly the
//! offsets we want; their opcode prefixes are unique enough (2
//! bytes) that scanning for them is robust. This module exposes
//! one entry point — [`detect`] — that walks the payload, returns
//! `None` for non-`.dw` files, and otherwise returns a
//! [`DwLayout`] holding everything the parser needs to know about
//! where the data lives.
//!
//! The opcode prefixes searched here are documented in `FORMAT.md`
//! (this directory, the canonical reference) and in the structural
//! spec `whittaker-dw-format.md` at the repo root, whose `§` section
//! numbers the comments below refer to.

use alloc::collections::BTreeSet;
use alloc::collections::VecDeque;

use super::header::DW_NUM_CHANNELS;
use super::tables::PeriodTable;

/// Module-level feature flags lifted from the replayer code.
///
/// Whittaker's replayer is parametric: each module turns specific
/// features on or off by emitting (or omitting) blocks of code,
/// and the meaning of `Effect8` / `Effect9` is fixed at init time
/// by *which* of these flags is set (spec §10.7). The detection
/// layer extracts the flags so the runtime can pick the right
/// interpretation for every byte.
///
/// Empty defaults represent the "no probe matched" case — a
/// future refinement of the probe catalogue may turn `false`
/// into `true` for modules whose code shape we don't yet
/// recognise.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DwFeatures {
    /// A `MOVE.B disp(PC), Dn` (`10 3A`) at the top of `Play` —
    /// reads a per-frame counter that throttles command parsing
    /// to every Nth tick.
    pub enable_delay_counter: bool,
    /// Same probe extended with `C0 FC` (`MULU.W`) — the counter
    /// is multiplied by a per-module constant for finer rates.
    pub enable_delay_multiply: bool,
    /// A `LEA disp(PC), A1` + `MOVE.W disp(PC), Dn`
    /// (`20 7A` + `30 3A`) pair near the top of `Play` —
    /// morphs a sample on the fly between bounded positions to
    /// simulate a moving square-wave oscillator.
    pub enable_square_waveform: bool,
    /// A `6B 00` (`BMI.W`) right after the track-byte read,
    /// followed by `D0 2D` (`ADD.B disp(A5), Dn`) — applies
    /// `sample.transpose` to every note before period lookup.
    pub enable_sample_transpose: bool,
    /// A `D0 28` (`ADD.B disp(A0), Dn`) eight bytes into
    /// `do_frame_stuff` — applies `channel.transpose` to every
    /// note. Mutually exclusive with [`Self::enable_volume_fade`]
    /// at the `Effect8` dispatch.
    pub enable_channel_transpose: bool,
    /// The `Effect8` handler starts with `0x17 0x59`
    /// (`MOVE.B (Ax)+, ...`) — interprets `Effect8 <arg>` as
    /// a global volume fade.
    pub enable_volume_fade: bool,
    /// The `Effect8` / `Effect9` handlers start with `50 E8` /
    /// `51 E8` (`ADDQ.B #8` / `SUBQ.B #8`) — `Effect8` toggles
    /// `EnableHalfVolume` on, `Effect9` toggles it off.
    pub enable_half_volume: bool,
    /// The vibrato handler (command 6) starts with `50 E8` and
    /// `11 59` four bytes in — vibrato is active in this module.
    pub enable_vibrato: bool,
    /// Per-module arpeggio table is referenced — `Effect8`/`9`
    /// can route into per-note pitch offsets.
    pub enable_arpeggio: bool,
    /// Per-module envelope table is referenced — channel volume
    /// is driven by stepped values rather than only sample
    /// defaults.
    pub enable_envelopes: bool,
    /// The play loop guards its one-frame-early DMA clear with
    /// `CMPI.B #$83,(A1)` (`0C 11 00 83`): when the *next* track
    /// byte is `0x83`, the clear is skipped, so the channel's DMA
    /// stays on and the `0x83` re-affirms it without a 0→1 strike.
    /// That makes `0x83` a **tie** (extend the current note, no
    /// re-attack) rather than the genuine re-trigger of the
    /// families that lack the guard (bubble bobble / grimblood:
    /// they always clear, so their `0x83` *does* re-strike).
    ///
    /// Both `0x83` jump-table targets begin with `MOVE.L A1,(0x4,A0)`,
    /// so [`super::command_map::classify_handler`] cannot tell them
    /// apart — the only discriminator is this guard in the play loop.
    /// Confirmed present in xenon2 / beast / bad company / empire and
    /// absent in bubble bobble / grimblood / obliterator (Ghidra +
    /// Paula oracle: xenon2 ch1 holds one B-3 from row 0x20 with a
    /// decaying volume, no re-strike at row 0x38).
    pub note_repeat_is_tie: bool,
}

/// Sticky output of detection: everything the importer needs to
/// turn the raw payload into structured data.
#[derive(Debug, Clone)]
pub struct DwLayout {
    /// Replayer family — old (QBall) vs new (current).
    pub variant: DwVariant,

    /// Period table the replayer uses.
    pub period_table: PeriodTable,

    /// Absolute file offset where the init function starts. Used
    /// as the cursor anchor for all later probes.
    pub init_offset: usize,

    /// The replayer's base-pointer (`A3`) **file address**, decoded
    /// from the init `LEA -d(PC), A3` anchor:
    /// `start_offset = signed_disp16 + (init_offset + 2)`.
    ///
    /// Every offset the player dereferences through `A3` — the
    /// per-channel position lists, the track pointers they hold,
    /// and the loop/restart targets — is stored *relative to this
    /// base*, so the real file position is `value + start_offset`.
    /// On almost every module `A3` points at file offset 0 and this
    /// is `0` (a no-op); a few builds (e.g. `bubble bobble.dw`,
    /// `start_offset = -0x442`) point `A3` *before* the image, so
    /// every position/track offset must be rebased through it or the
    /// player reads garbage. Table *locations* found via PC-relative
    /// LEAs (sub-song list, sample data, env/arp tables) are already
    /// absolute and must **not** be rebased again.
    pub start_offset: isize,

    /// Absolute file offset where the play (per-tick) function
    /// starts.
    pub play_offset: Option<usize>,

    /// File offset of the sample-info table (per-sample metadata).
    pub sample_info_offset: Option<usize>,

    /// File offset of the sample-data table (length + freq + PCM
    /// per sample, concatenated).
    pub sample_data_offset: Option<usize>,

    /// File offset of the sub-song list.
    pub sub_song_list_offset: Option<usize>,

    /// Number of samples declared by the init code. Pulled from
    /// the `moveq #N, D1` immediate (+1, matching the replayer's
    /// `dbf` post-decrement convention).
    pub number_of_samples: Option<u32>,

    /// Module-level feature flags — fix the meaning of
    /// `Effect8`/`Effect9` and gate optional engine features.
    pub features: DwFeatures,

    /// Track-byte dispatcher thresholds — see [`DwDispatcher`].
    pub dispatcher: DwDispatcher,

    /// Per-module low-command profile for jump-table-
    /// dispatched builds (e.g. the bubble bobble / C3
    /// family). `None` when the module uses the canonical
    /// fixed cmd-code layout — the common case.
    pub command_map: Option<super::command_map::DwCommandMap>,

    /// File offset of the per-channel volume-envelope pointer
    /// table — the LEA-target inside the `0xA0..` bracket of the
    /// dispatcher cascade. Each entry is a big-endian `u16`
    /// PC-relative offset into the replayer image; following it
    /// yields the actual envelope byte stream consumed by
    /// `Play+0x412` (one byte every `chan[+0x2A]` frames, low 7
    /// bits → Paula AUD0VOL after global-volume scaling).
    ///
    /// `None` when the module has no `0xA0..` bracket (rare —
    /// most new-player modules use this slot for per-note volume
    /// shaping).
    pub volume_envelope_table_offset: Option<usize>,

    /// Number of entries in [`Self::volume_envelope_table_offset`].
    /// Derived from `threshold[1] .. threshold[0]` (e.g. for the
    /// canonical `0xA0..0xAF` bracket = 16 entries). Always paired
    /// with the offset — `None` if the offset itself is `None`.
    pub volume_envelope_table_len: Option<u8>,

    /// `true` when the replayer reads position-list entries and
    /// channel-base offsets as 32-bit big-endian values; `false`
    /// (the default, new-player layout) reads them as 16-bit.
    ///
    /// Anchor: the `MULU.W #18` step that converts the host's
    /// sub-song index into a row offset (qball uses 18-byte rows
    /// = 2 status bytes + 4 × u32 channel offsets, whereas the
    /// new player uses 10-byte rows = 2 status bytes + 4 × u16).
    pub uses_32bit_pointers: bool,

    /// Byte width of a sub-song table row — `10` for the canonical
    /// new-player layout (`MULU #10`), `18` for qball-style 32-bit
    /// builds (`MULU #18`). Determined by the same probe that
    /// sets [`Self::uses_32bit_pointers`].
    pub sub_song_row_width: usize,

    /// Bytes of header (speed/delay/param prefix) at the start of a
    /// sub-song row, before the channel-pointer array. The replayer's
    /// `init_main` reads channel `ch`'s position base from
    /// `word[table + ss*row + header + ch*ptr]` — `header` is the
    /// displacement of that indexed `MOVEA.W (d8,A0,Dn),A0`
    /// (`30 70 00 <d8>`). 10/18-byte rows always have a 2-byte
    /// header; 8-byte rows come in two shapes — a 4-voice
    /// `[ch0..ch3]` (header 0, e.g. sentinel) and a **3-voice**
    /// `[param, ch0, ch1, ch2]` (header 2, e.g. archipelagos /
    /// fright night). The number of voices is then
    /// `(row - header) / ptr_width`. Reads it from `init_main` so
    /// the 3-voice modules stop being mis-parsed as 4-voice (which
    /// took the param word as ch0's base — into the period table —
    /// and invented a phantom 4th voice).
    pub sub_song_header: usize,

    /// File offset of the per-channel **pitch arpeggio** pointer
    /// table — the LEA target inside the third bracket
    /// (`0x90..` family) of the dispatcher cascade. Each entry is
    /// a big-endian `u16` PC-relative offset to an arpeggio byte
    /// stream (semitone offsets, last byte flagged with `0x80`;
    /// spec §4.4 / §10.12).
    ///
    /// `None` when the module has no `0x90..` bracket (it then
    /// has no inline pitch-arpeggio commands).
    pub arpeggio_table_offset: Option<usize>,

    /// Number of entries in [`Self::arpeggio_table_offset`] —
    /// `threshold[1] .. threshold[2]` (the `0x90..0x9F` span =
    /// 16 entries on the canonical cascade). `None` when the
    /// offset itself is `None`.
    pub arpeggio_table_len: Option<u8>,

    /// `true` when the `SetVolumeEnvelope` dispatcher bracket (the
    /// second/`A0` family) arms a **pitch arpeggio** rather than a
    /// volume envelope. Set on 2-bracket modules whose A0 table
    /// holds semitone offsets (tetris); the runtime then routes
    /// those bytes to `current_arpeggio` and leaves channel volume
    /// at full. `false` for the canonical 3-bracket cascade where
    /// A0 is the volume envelope.
    pub volume_bracket_is_pitch: bool,

    /// File offset of the **old-player per-channel volume table** —
    /// `DW_NUM_CHANNELS` big-endian `u16`s the qball-era replayer
    /// loads into `AUDxVOL` on every note trigger
    /// (`SetAmigaVolume(channelVolumes[ch])`). It has no envelope or
    /// per-sample scaling: this static value *is* the channel's
    /// loudness. Located via the `LEA d16(A3),A0 ; LSL` prologue
    /// (`41 EB <d16> E3 4F`) that indexes it; the offset is
    /// `d16 + start_offset`. `None` on new-player modules (their
    /// loudness comes from the volume envelope) and when the probe
    /// can't find the prologue. Gated on the old player (Ghidra: only
    /// the qball-era replayer loads this table).
    pub channel_volume_offset: Option<usize>,

    /// File offset of the static **global master-volume** word —
    /// the `USHORT` the empire-era replayer multiplies every Paula
    /// volume by before the `>> 6` (`AUDxVOL = volByte × master >>
    /// 6`). Located via the volume-scale prologue
    /// `MOVE.W (d16,PC),D2 ; MULU.W D2,D1 ; LSR.W #6,D1`
    /// (bytes `34 3A <d16> C2 C2 EC 49`); the word lives at
    /// `instr_addr + 2 + d16`. Both the note-trigger and the
    /// per-tick envelope-animation writes go through it, so the
    /// importer scales *all* emitted Paula volumes by `master / 64`.
    ///
    /// `None` on modules without the prologue. New-player builds
    /// carry the same MULU but with `master = 64` (the identity), so
    /// even if a future probe matched one, the scaling would be a
    /// no-op; qball-style old players write `channelVolumes[ch]` raw
    /// (no multiply) and never match. The empire word is `24`.
    pub master_volume_offset: Option<usize>,

    /// `true` when the replayer computes `AUDxPER` the new-player way —
    /// a **full-range** note index into a word period table (P2/P3)
    /// scaled by the per-sample fine-tune (`table[note] × mult >> 10`)
    /// — rather than the qball-era one-octave composite
    /// `PERIODS1[note % 12]` (no fine-tune). Always `true` on the new
    /// player; `true` on the old-stream players that nonetheless carry
    /// the fine-tune idiom (leviathan C7 → P2, empire C8 → P3); `false`
    /// on genuine qball-style composite players. Drives both the note
    /// decode (direct vs `sample × 12 + pitch`) and the period formula.
    pub period_via_finetune: bool,

    /// File offset of the leviathan-era **per-instrument volume table**
    /// — the 16-byte instrument records whose `+0xE` word the replayer
    /// writes straight to `AUDxVOL` on every note (a static per-sample
    /// loudness, no envelope). Located via the `SetSample` arming idiom
    /// `LEA (d16,PC),A5 ; MULU.W #0x10,D0`. Read (gated on
    /// [`Self::period_via_finetune`]) into each `DwSample::volume`.
    /// `None` on every other player (qball uses a per-channel table; the
    /// new player and empire use volume envelopes).
    pub instrument_volume_offset: Option<usize>,
}

/// Per-module dispatch thresholds for the three table-driven
/// command families (SetSample, SetVolumeEnvelope, SetPitchArpeggio).
///
/// Whittaker's track byte stream multiplexes these three families
/// onto byte ranges separated by `CMP.B / BLT.B / SUBI.B` cascades
/// at the top of the dispatch routine. The exact thresholds are
/// per-module; the canonical 3-bracket cascade is
/// `0xB0..` → SetSample, `0xA0..0xAF` → SetVolumeEnvelope,
/// `0x90..0x9F` → SetPitchArpeggio. A module with no arpeggios drops
/// the `0x90` bracket; one with no envelopes drops the `0xA0` bracket
/// (and may collapse to the single `0xB0..` → SetSample split).
///
/// `thresholds` lists each detected bracket's CMP immediate in
/// *descending* order. By convention the first entry is always
/// SetSample, the second (if present) SetVolumeEnvelope (`0xA0`
/// bracket → Paula volume), the third SetPitchArpeggio (`0x90`
/// bracket → Paula period). The decoder uses this ordering directly
/// to classify each above-`0x80` byte without re-tracing the binary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DwDispatcher {
    /// CMP immediates of the dispatcher cascade, descending. Empty
    /// when no cascade could be located (the decoder then falls
    /// back to the canonical 0xB0 / 0xA0 / 0x90 split that matches
    /// most modules).
    pub thresholds: alloc::vec::Vec<u8>,
}

impl DwDispatcher {
    /// `Some(t)` for the SetSample threshold — bytes `>= t` are
    /// SetSample commands. `None` when no cascade was detected.
    #[inline]
    pub fn sample_threshold(&self) -> Option<u8> {
        self.thresholds.first().copied()
    }

    /// `Some(t)` for the SetVolumeEnvelope threshold (the `0xA0`
    /// bracket). `None` when the module has no volume-envelope
    /// dispatcher.
    #[inline]
    pub fn volume_envelope_threshold(&self) -> Option<u8> {
        self.thresholds.get(1).copied()
    }

    /// `Some(t)` for the SetPitchArpeggio threshold (the `0x90`
    /// bracket). `None` when the module has no pitch-arpeggio
    /// dispatcher.
    #[inline]
    pub fn pitch_arpeggio_threshold(&self) -> Option<u8> {
        self.thresholds.get(2).copied()
    }
}

/// Which replayer family this file targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DwVariant {
    /// Early "QBall" player — 1-octave [`PERIODS1`](super::tables::PERIODS1)
    /// table, composite `note / 12` + `note % 12` byte encoding.
    Old,
    /// Standard new player — used by Xenon 2 and most of the
    /// catalogue. Period table is [`PERIODS2`](super::tables::PERIODS2)
    /// or [`PERIODS3`](super::tables::PERIODS3) depending on the
    /// first LEA target inside `Play()`.
    New,
}

/// Reject SC68 files (Atari ST format that occasionally lands
/// near `.dw` modules in user dumps).
const SC68_MAGIC: [u8; 4] = [b'S', b'C', b'6', b'8'];

/// Top-level entry point. Returns `None` if the payload doesn't
/// look like a David Whittaker module.
pub fn detect(payload: &[u8]) -> Option<DwLayout> {
    if payload.len() < 0x40 {
        return None;
    }
    if payload[0..4] == SC68_MAGIC {
        return None;
    }

    // A real `.dw` opens with one of many possible prologues —
    // MOVEM-stub jump tables, MOVE.L stack-arg reads, BCHG of the
    // Amiga LED address, plain MOVEQ #0 / MOVE.W #0 setup, or
    // even a bare `LEA -d(PC), A3` at file offset 0. The unifying
    // structural signal is the `LEA -d(PC), A3` (`47 FA <hi> <lo>`
    // with `disp_hi >= 0xF0`) that every Whittaker replayer emits
    // to load its base pointer. Scan the first kilobyte of code
    // for that anchor; if it's missing, reject the payload.
    //
    // The window has to reach past long jump-table preambles
    // (modules like `chip's challenge.dw` ship 7+ stubs before
    // Init, putting the first LEA A3 around offset 0x39C).
    const INIT_SCAN_LIMIT: usize = 0x1000;
    let init_anchor = find_lea_a3_pc_neg(payload, 0, INIT_SCAN_LIMIT)?;
    let init_offset = init_anchor;

    // The `A3` base file address: `LEA -d(PC), A3` puts
    // `(init_offset + 2) + signed_disp16` into A3. Every offset the
    // player later dereferences through A3 (position lists, track
    // pointers, loop targets) is relative to this. See
    // `DwLayout::start_offset`.
    let start_offset = {
        let disp =
            i16::from_be_bytes([payload[init_offset + 2], payload[init_offset + 3]]) as isize;
        (init_offset as isize + 2) + disp
    };

    // Sub-song row width comes from the `MULU.W #imm, D0` step that
    // turns a sub-song index into a row offset. `#10` is the
    // canonical new-player layout (4 × u16 channel offsets);
    // `#18` is the qball-era 32-bit layout (4 × u32 channel
    // offsets). The probe scans the whole code region — no init
    // anchor is needed because the MULU constant is rare enough
    // not to clash with data.
    let sub_song_row_width = find_sub_song_row_width(payload).unwrap_or(10);
    let uses_32bit_pointers = sub_song_row_width == 18;

    // Header (param prefix) before the channel pointers in each
    // sub-song row. 10/18-byte rows always carry a 2-byte header;
    // 8-byte rows are ambiguous (4-voice header-0 vs 3-voice
    // header-2), so read the actual displacement from `init_main`'s
    // indexed channel-base load. See `find_sub_song_header`.
    let sub_song_header = if sub_song_row_width == 8 {
        find_sub_song_header(payload).unwrap_or(0)
    } else {
        2
    };

    // New vs old: two independent cues converge.
    //
    // - The old (qball-era) player uses 32-bit pointers — that's
    //   the most reliable structural signal we can read off the
    //   binary, so it takes precedence.
    // - Otherwise fall back to the `4A 2B` (TST.B disp(A3)) probe
    //   from the spec, which fingerprints the very early "QBall"
    //   builds that still ship 16-bit pointers.
    let variant = if uses_32bit_pointers || matches_at(payload, init_offset + 4, &[0x4A, 0x2B]) {
        DwVariant::Old
    } else {
        DwVariant::New
    };

    // Period-table selection — heuristic: scan the rest of the
    // payload for a 16-bit value that exists at the start of one
    // of the three known tables (8192 → P3, 4096 → P2, 256 → P1).
    // The dispatcher will refine this once we trace `Play()`.
    //
    // The `(table, period_via_finetune)` pair also records *how* the
    // period is computed. The new player always indexes a full-range
    // word table scaled by the per-sample fine-tune (`AUDxPER =
    // table[note] × mult >> 10`). Most old (qball-era) players instead
    // use the one-octave composite `PERIODS1[note % 12]` with no
    // fine-tune. A *third* shape exists — an old command/stream layout
    // that nonetheless computes its period the new way: leviathan (C7,
    // P2) and the empire (C8, P3). They are betrayed by carrying the
    // exact fine-tune idiom in their play body, so the same LEA probe
    // that picks the new player's table picks theirs; when it fires on
    // an "old" binary, switch that module onto the fine-tune path.
    // `guess_new_period_table` (a whole-payload word scan) is *not*
    // used as a fallback for old binaries — it would false-match a
    // 4096/8192 word inside qball's sample PCM and wrongly promote it.
    let (period_table, period_via_finetune) = match variant {
        DwVariant::New => (
            find_period_table_via_lea(payload)
                .or_else(|| guess_new_period_table(payload))
                .unwrap_or(PeriodTable::P2),
            true,
        ),
        DwVariant::Old => match find_period_table_via_lea(payload) {
            Some(table) => (table, true),
            None => (PeriodTable::P1, false),
        },
    };

    // The remaining constants are best-effort: parsers should
    // gracefully degrade to "samples-only" when these can't be
    // located. They sit in the init function — scan forward from
    // `init_offset` looking for the canonical opcode anchors.
    // Whittaker's init dispatches a lot of work through `BSR.W`
    // helpers; sample-loading in particular lives one or two
    // calls in. The probes therefore do a small breadth-first walk
    // through the call graph — running the byte-pattern scan in
    // each function body and descending into every `BSR.W` target
    // until they hit an `RTS`. Visited offsets are tracked to
    // avoid cycling; depth is capped at 3 (every Whittaker module
    // observed so far is reachable inside that bound).
    //
    // The sample loader has three co-located anchors that must be
    // matched together — picking the right MOVEQ requires the
    // full triple, otherwise unrelated `72 xx` bytes lead the
    // walker astray. The qball-era player addresses both bases
    // through A3 (`41 EB / 43 EB`) rather than the new player's
    // PC-relative `41 FA / 4B FA`, so we try both encodings and
    // keep whichever matched.
    // Four-stage lookup, most-specific first:
    //   1. call-graph walk from init for the PC-relative loader
    //      (`41 FA / 4B FA / 72 N`);
    //   2. call-graph walk for the qball A3-relative loader
    //      (`41 EB / 43 EB / 72 N`);
    //   3+4. flat scans of the whole early code region for the
    //      same two patterns.
    // The flat-scan fallbacks rescue modules whose `init_offset`
    // anchor lands on a tiny jump-table stub that `RTS`es before
    // ever reaching the real Init (the call-graph walk then stops
    // short of the loader — e.g. aquaventura.dw, where the first
    // `LEA A3` at 0x62 is a 20-byte stub and the loader lives at
    // 0x104A behind two more stubs).
    let (sample_data_offset, sample_info_offset, number_of_samples) = {
        let pc = walk_for_sample_loader(payload, init_offset);
        if pc.0.is_some() {
            pc
        } else {
            let a3 = walk_for_sample_loader_a3(payload, init_offset);
            if a3.0.is_some() {
                a3
            } else {
                let flat_pc = flat_scan_sample_loader(payload, false);
                if flat_pc.0.is_some() {
                    flat_pc
                } else {
                    flat_scan_sample_loader(payload, true)
                }
            }
        }
    };

    // Sub-song header lookup. The reliable signature is the
    // `MULU.W #N, D0` (`C0 FC 00 0A` / `C0 FC 00 12`) that turns
    // a sub-song index into a row offset, immediately followed
    // by a `LEA disp(PC), A0` (`41 FA <hi> <lo>`) — or, on the
    // qball-era player, a `LEA disp(A3), A0` (`41 EB <hi> <lo>`)
    // — that loads the table base.
    let sub_song_list_offset = walk_for_sub_song_table(payload, init_offset);

    // Scan for `Play` across the whole code region — earlier
    // versions started at `init_offset + 0x200`, which on this
    // module overshoots Play (0x1B6 sits below init at 0x66 +
    // 0x200 = 0x266). The LEA/TST/BEQ triple is rare enough
    // that scanning from offset 0 finds the right one
    // unambiguously, and bounding the scan to the early file
    // region keeps the cost negligible.
    let play_scan_end = 0x2000.min(payload.len());
    let play_offset = find_play_anchor(payload, 0, play_scan_end);

    // Feature flags — see `DwFeatures`. The probes here cover the
    // subset that can be matched against `play_offset` directly.
    // Flags whose patterns live behind a jump-table indirection
    // (`enable_vibrato`, `enable_half_volume`, `enable_volume_fade`)
    // need the dispatch table address — they're left at default
    // `false` until a follow-up locates that table for this
    // module.
    let mut features = match play_offset {
        Some(p) => detect_features(payload, p),
        None => DwFeatures::default(),
    };

    // `enable_half_volume` precise pass. The blind heuristic in
    // `detect_features` matches `ST/SF (1,A0)` (`50 E8 00 01` /
    // `51 E8 00 01`) anywhere — but that pattern is *also* how the
    // StartVibrato / StopVibrato handlers toggle the vibrato bit, so
    // virtually every module (anything with vibrato) was flagged
    // `half_vol = true`. That only matters for the `Effect9` byte
    // count (0 bytes when half-volume, else 2): on the modules that
    // actually emit `Effect9` (e.g. bubble bobble, where cmd 9 is a
    // 2-byte position-restart) the false flag dropped 2 arg bytes,
    // misaligning the stream into spurious high notes (squeals).
    //
    // The reliable signal (Ghidra) is the **effect jump table**: it
    // only counts as half-volume when the *effect-8* handler opens
    // with `ST (1,A0)` AND the *effect-9* handler opens with
    // `SF (1,A0)`. Locate the table and apply that exact test; default
    // to `false` when the table can't be found. Since the flag feeds
    // only the `Effect9` width, modules without an `Effect9` command
    // are unaffected regardless.
    features.enable_half_volume = find_effect_jump_table(payload, start_offset)
        .map(|table| effect_handler_is_half_volume(payload, table, start_offset))
        .unwrap_or(false);

    // Dispatcher cascade — walk the per-tick CMP.B / BLT.B / SUBI.B
    // triples that bracket the above-0x80 command byte ranges.
    // Independent of `play_offset` because the cascade lives a
    // few hundred bytes into the per-channel routine and we scan
    // the whole code region anyway; this keeps the probe robust
    // when `find_play_anchor` returns the wrong stub (multiple
    // anchors match the play signature in practice).
    let (dispatcher, bracket_offsets) =
        find_dispatcher_cascade_with_offsets(payload).unwrap_or_default();

    // Per-channel volume envelope table — the LEA inside the
    // second bracket (`0xA0..` family) of the dispatcher cascade.
    // The handler layout is fixed:
    //   bracket+0:  CMP.B #t, D0
    //   bracket+4:  BLT.B disp
    //   bracket+6:  SUBI.B #t, D0
    //   bracket+10: ADD.W D0, D0
    //   bracket+12: LEA disp16(PC), A2
    // so the LEA opcode `45 FA` should land at bracket+12 with
    // its 16-bit displacement at bracket+14.
    let (volume_envelope_table_offset, volume_envelope_table_len) = match (
        bracket_offsets.get(1),
        dispatcher.thresholds.first(),
        dispatcher.thresholds.get(1),
    ) {
        (Some(&bracket_addr), Some(&top), Some(&bottom)) => {
            let table = read_lea_a2_at(payload, bracket_addr + 12);
            let len = top.saturating_sub(bottom);
            (table, Some(len))
        }
        _ => (None, None),
    };

    // Per-channel pitch-arpeggio table — the LEA inside the third
    // bracket (`0x90..` family). The handler shares the volume
    // bracket's shape (`CMP / BLT / SUBI / ADD.W D0,D0 / LEA
    // disp16(PC), An`) but may load a different address register,
    // so we scan the bracket's first 0x18 bytes for any
    // `LEA disp16(PC), An` rather than hard-coding `45 FA`.
    let (arpeggio_table_offset, arpeggio_table_len) = match (
        bracket_offsets.get(2),
        dispatcher.thresholds.get(1),
        dispatcher.thresholds.get(2),
    ) {
        (Some(&bracket_addr), Some(&top), Some(&bottom)) => {
            let table = find_lea_an_pc_in(payload, bracket_addr, bracket_addr + 0x18);
            let len = top.saturating_sub(bottom);
            (table, Some(len))
        }
        _ => (None, None),
    };

    // Bracket-semantic disambiguation.
    //
    // The 3-bracket cascade (xenon2, beast1: `[sample, A0, 90]`)
    // is unambiguous — `A0` arms a per-channel VOLUME envelope and
    // `90` a pitch ARPEGGIO. But a 2-bracket cascade
    // (`[sample, X]`) is module-dependent: on tetris the second
    // bracket (`A0`) is a pitch ARPEGGIO (its handler feeds the
    // value cycle to `AUDxPER`), not a volume envelope. Applying
    // tetris's arpeggio offsets (`0,3,7,12…`) as volumes scales
    // every note down to near-silence — exactly the "no sound"
    // symptom.
    //
    // Discriminate by the shape of the pointed data: a volume
    // envelope ramps toward Paula's 0x40 full-scale, whereas an
    // arpeggio is small signed semitone offsets (≤ ~0x18) with
    // `0x80`-set terminators. When the only non-sample bracket's
    // table reads as arpeggio, reclassify it as the pitch table
    // and drop the (bogus) volume table — `volume_bracket_is_pitch`
    // then tells the runtime to arm pitch-arp from that bracket's
    // bytes and leave channel volume at full.
    let bracket1_is_arp = bracket_offsets.get(2).is_none()
        && volume_envelope_table_offset
            .zip(volume_envelope_table_len)
            .map(|(off, len)| table_looks_like_arpeggio(payload, off, len))
            .unwrap_or(false);
    let (
        volume_envelope_table_offset,
        volume_envelope_table_len,
        arpeggio_table_offset,
        arpeggio_table_len,
        volume_bracket_is_pitch,
    ) = if bracket1_is_arp {
        (
            None,
            None,
            volume_envelope_table_offset,
            volume_envelope_table_len,
            true,
        )
    } else {
        (
            volume_envelope_table_offset,
            volume_envelope_table_len,
            arpeggio_table_offset,
            arpeggio_table_len,
            false,
        )
    };

    // Old-player per-channel volume table. The qball-era replayer
    // loads it through `LEA d16(A3),A0 ; LSL.? Dn` — the byte
    // signature `41 EB <d16hi> <d16lo> E3 4F` (Ghidra: qball's
    // per-note volume load). The table address is `d16 +
    // start_offset` (the A3 base). New players read loudness from the
    // volume envelope instead, so only probe the old variant — that
    // also avoids a stray `41 EB .. E3 4F` in unrelated code arming a
    // bogus volume on a module that doesn't use one.
    let channel_volume_offset = if matches!(variant, DwVariant::Old) {
        find_channel_volume_table(payload, start_offset)
    } else {
        None
    };

    // Global master volume (empire family): the prologue
    // `MOVE.W (d16,PC),D2 ; MULU.W D2,D1 ; LSR.W #6,D1` scales every
    // Paula volume by `master / 64`. Probe unconditionally — the byte
    // pattern is specific to that register allocation, qball (raw
    // write) and the other clusters don't carry it, and a new-player
    // match would only ever read `master = 64` (the identity).
    let master_volume_offset = find_master_volume_offset(payload);

    // Per-instrument static volume table (leviathan / C7 old-stream
    // fine-tune player). Probed unconditionally — the idiom (`LEA
    // (d16,PC),A5 ; MULU.W #0x10`) is specific — but only *consumed*
    // when `period_via_finetune` is set (parse_samples), so a stray
    // match on another player can't disturb its sample volumes.
    let instrument_volume_offset = find_instrument_volume_table(payload);

    // Jump-table (command_map) family: cmd 0x88's handler is
    // `MOVE.B (A1)+, (0x2f,A0)` — a per-channel **transpose**, added to
    // the note index in the period formula (`PERIODS3[(transpose + note)
    // & 0xff]`). Verified against the Paula oracle on bubble bobble: the
    // transposed channels' base period only matches the golden with the
    // signed transpose applied (e.g. note 67, transpose −36 → index 31 →
    // period 353; without it index 67 → 53). Arm `enable_channel_transpose`
    // so the runtime's `Effect8` path binds the arg to a transpose rather
    // than the global-volume offset used by the tetris family.
    let command_map = super::command_map::detect_command_map(payload, start_offset);
    let features = {
        let mut f = features;
        if command_map.is_some() {
            f.enable_channel_transpose = true;
        }
        f
    };

    Some(DwLayout {
        variant,
        period_table,
        init_offset,
        start_offset,
        play_offset,
        sample_info_offset,
        sample_data_offset,
        sub_song_list_offset,
        number_of_samples,
        features,
        dispatcher,
        command_map,
        volume_envelope_table_offset,
        volume_envelope_table_len,
        uses_32bit_pointers,
        sub_song_row_width,
        sub_song_header,
        arpeggio_table_offset,
        arpeggio_table_len,
        volume_bracket_is_pitch,
        channel_volume_offset,
        master_volume_offset,
        period_via_finetune,
        instrument_volume_offset,
    })
}

/// Locate the old-player per-channel volume table via its load
/// prologue `LEA d16(A3),A0 ; LSL` — bytes `41 EB <d16> E3 4F`.
/// Returns `d16 + start_offset` (the resolved file offset of the
/// table) when the anchor is found and resolves inside the payload,
/// else `None`. Scans for `41 EB .. .. E3 4F`, takes the signed
/// 16-bit displacement, and adds the start offset (the A3 base).
fn find_channel_volume_table(payload: &[u8], start_offset: isize) -> Option<usize> {
    let mut i = 0usize;
    while i + 6 <= payload.len() {
        if payload[i] == 0x41
            && payload[i + 1] == 0xEB
            && payload[i + 4] == 0xE3
            && payload[i + 5] == 0x4F
        {
            let disp = i16::from_be_bytes([payload[i + 2], payload[i + 3]]) as isize;
            let off = start_offset + disp;
            if off >= 0 && (off as usize) + 2 * DW_NUM_CHANNELS <= payload.len() {
                return Some(off as usize);
            }
        }
        i += 2;
    }
    None
}

/// Locate the static global master-volume word via the empire-era
/// volume-scale prologue
/// `MOVE.W (d16,PC),D2 ; MULU.W D2,D1 ; LSR.W #6,D1` — bytes
/// `34 3A <d16> C2 C2 EC 49`. The master word lives PC-relative at
/// `i + 2 + d16` (the displacement is taken from just after the
/// `MOVE.W` opcode word). Returns that file offset when the anchor is
/// found, resolves inside the payload, and holds a sane volume
/// (`1..=64`); else `None`. The empire replayer references the word
/// from both its note-trigger and envelope-animation paths with the
/// identical opcode sequence, so the first match suffices.
fn find_master_volume_offset(payload: &[u8]) -> Option<usize> {
    let mut i = 0usize;
    while i + 8 <= payload.len() {
        if payload[i] == 0x34
            && payload[i + 1] == 0x3A
            && payload[i + 4] == 0xC2
            && payload[i + 5] == 0xC2
            && payload[i + 6] == 0xEC
            && payload[i + 7] == 0x49
        {
            let disp = i16::from_be_bytes([payload[i + 2], payload[i + 3]]) as isize;
            let off = i as isize + 2 + disp;
            if off >= 0 {
                let off = off as usize;
                if let Some(bytes) = payload.get(off..off + 2) {
                    let master = u16::from_be_bytes([bytes[0], bytes[1]]);
                    if (1..=64).contains(&master) {
                        return Some(off);
                    }
                }
            }
        }
        i += 2;
    }
    None
}

/// Heuristic: does the `u16`-pointer table at `table_off` (with
/// `n` entries) point at **arpeggio** byte streams rather than
/// **volume-envelope** streams?
///
/// Arpeggio streams are small signed semitone offsets terminated
/// by a byte with bit 7 set; their magnitudes stay within a few
/// octaves (`low 7 bits ≤ ~0x18`). Volume envelopes ramp toward
/// Paula's `0x40` full-scale, so at least one value reaches
/// `≥ 0x20`. The classifier samples up to the first 8 entries and
/// returns `true` only when every byte stays in the arpeggio
/// range *and* at least one real terminator was seen — keeping it
/// from mis-tagging an empty or garbage region.
fn table_looks_like_arpeggio(payload: &[u8], table_off: usize, n: u8) -> bool {
    const PROBE_ENTRIES: usize = 8;
    const MAX_STEPS: usize = 32;
    const ARP_MAX: u8 = 0x18;

    let entries = (n as usize).min(PROBE_ENTRIES);
    if entries == 0 {
        return false;
    }
    let mut saw_terminator = false;
    for idx in 0..entries {
        let entry_off = table_off + idx * 2;
        if entry_off + 2 > payload.len() {
            return false;
        }
        let stream = u16::from_be_bytes([payload[entry_off], payload[entry_off + 1]]) as usize;
        if stream == 0 || stream >= payload.len() {
            continue;
        }
        let mut cursor = stream;
        let mut steps = 0;
        while cursor < payload.len() && steps < MAX_STEPS {
            let b = payload[cursor];
            if (b & 0x7F) > ARP_MAX {
                // A value too large to be a semitone offset — this
                // is volume-envelope data, not an arpeggio.
                return false;
            }
            cursor += 1;
            steps += 1;
            if b & 0x80 != 0 {
                saw_terminator = true;
                break;
            }
        }
    }
    saw_terminator
}

/// Scan `[start, end)` for the first `LEA disp16(PC), An`
/// instruction — opcode high byte `0x4N` with bit pattern
/// `4{1,3,5,7,9,B} FA` (An = A0..A5 via the odd nibbles) — and
/// return its PC-relative resolved target. Used to lift a table
/// base out of a dispatcher-bracket handler whose LEA may target
/// any address register.
fn find_lea_an_pc_in(payload: &[u8], start: usize, end: usize) -> Option<usize> {
    let end = end.min(payload.len().saturating_sub(3));
    let mut i = start;
    while i + 3 < end {
        // `LEA disp16(PC), An` = `4{reg}FA <hi> <lo>`, where the
        // high byte is `0x41/0x43/0x45/0x47/0x49/0x4B` (A0..A5).
        let hi = payload[i];
        if (hi & 0xF1) == 0x41 && payload[i + 1] == 0xFA {
            let disp = i16::from_be_bytes([payload[i + 2], payload[i + 3]]) as i32;
            let target = i as i32 + 2 + disp;
            if target >= 0 && (target as usize) < payload.len() {
                return Some(target as usize);
            }
        }
        i += 2;
    }
    None
}

/// Scan the whole code region for the canonical `MULU.W #imm, D0`
/// (`C0 FC 00 imm`) step the replayer uses to convert the host's
/// sub-song index into a row offset. The immediate is the row
/// width in bytes — `8` (= 4 × u16, no header, used on some
/// new-player rips like sentinel.dw), `10` (= 2 + 4 × u16, the
/// canonical new player), or `18` (= 2 + 4 × u32, qball-era
/// 32-bit build).
///
/// Returns `Some(width)` on match, `None` when no MULU pattern is
/// found in the first 4 KB of code (callers fall back to 10).
fn find_sub_song_row_width(payload: &[u8]) -> Option<usize> {
    let end = 0x1000.min(payload.len().saturating_sub(3));
    let mut i = 0;
    while i + 3 < end {
        if payload[i] == 0xC0 && payload[i + 1] == 0xFC && payload[i + 2] == 0x00 {
            let imm = payload[i + 3];
            if imm == 0x08 || imm == 0x0A || imm == 0x12 {
                return Some(imm as usize);
            }
        }
        i += 2;
    }
    None
}

/// Header (param-prefix byte count) of an **8-byte** sub-song row,
/// read from `init_main`'s indexed channel-base load.
///
/// The setup loop does (Ghidra `archipelagos init_main`):
/// ```text
/// C0 FC 00 08        MULU.W #8, D0          ; D0 = subsong * row(8)
/// ...
/// 41 FA <hi> <lo>    LEA    d16(PC), A0     ; A0 = sub-song table
/// 30 70 00 <d8>      MOVEA.W (d8,A0,D0.w), A0 ; chan base = word[table + D0 + d8]
/// ```
/// The `MOVEA.W` displacement `d8` is the header: `2` for the
/// 3-voice `[param, ch0, ch1, ch2]` shape (archipelagos / fright
/// night), `0` for the 4-voice `[ch0..ch3]` shape (sentinel). Only
/// `0`/`2` are accepted; anything else returns `None` so the caller
/// keeps the safe default. Anchored to the first `MULU.W #8` so the
/// distinctive `30 70 00 0X` opcode can't clash with unrelated code.
fn find_sub_song_header(payload: &[u8]) -> Option<usize> {
    let end = 0x1000.min(payload.len().saturating_sub(3));
    let mut i = 0;
    while i + 3 < end {
        if payload[i] == 0xC0
            && payload[i + 1] == 0xFC
            && payload[i + 2] == 0x00
            && payload[i + 3] == 0x08
        {
            // Scan init_main's setup that follows the MULU for the
            // indexed channel-base load `MOVEA.W (d8,A0,D0.w),A0`
            // (`30 70 00 <d8>`, extension word `00 d8` = D0.w ×1).
            let scan_end = (i + 0x80).min(payload.len().saturating_sub(3));
            let mut k = i + 4;
            while k + 3 < scan_end {
                if payload[k] == 0x30
                    && payload[k + 1] == 0x70
                    && payload[k + 2] == 0x00
                    && matches!(payload[k + 3], 0x00 | 0x02)
                {
                    return Some(payload[k + 3] as usize);
                }
                k += 2;
            }
        }
        i += 2;
    }
    None
}

/// `LEA d16(PC), A2` = `45 FA <hi> <lo>`. Returns the PC-relative
/// resolved target offset when the opcode is present at `at`,
/// otherwise `None`. PC-relative for `LEA` resolves from the
/// address of the extension word (= opcode + 2).
fn read_lea_a2_at(payload: &[u8], at: usize) -> Option<usize> {
    if at + 3 >= payload.len() {
        return None;
    }
    if payload[at] != 0x45 || payload[at + 1] != 0xFA {
        return None;
    }
    let disp = i16::from_be_bytes([payload[at + 2], payload[at + 3]]) as i32;
    let target = (at as i32 + 2 + disp) as i64;
    if target < 0 || (target as usize) >= payload.len() {
        None
    } else {
        Some(target as usize)
    }
}

/// Scan the code region for the canonical `CMP.B #imm,D0 ;
/// BLT.B disp ; SUBI.B #imm,D0` dispatcher triple, then chain
/// each bracket's `BLT` target to pick up the next one.
///
/// Encoding (68000):
///
/// ```text
/// B0 3C 00 XX        CMP.B  #XX, D0
/// 6D YY              BLT.B  (PC + 2 + YY)
/// 04 00 00 XX        SUBI.B #XX, D0
/// ```
///
/// The `B0 3C` opcode (`CMP.B` immediate, destination D0) is
/// distinctive enough that the first match is essentially always
/// the dispatcher entry; the matching `SUBI.B` immediate (same
/// `XX`) confirms the pairing. Returns a [`DwDispatcher`] with
/// thresholds in descending order — the first entry is the
/// SetSample threshold, subsequent entries arpeggio / envelope.
///
/// Also returns the file offset where each bracket starts, which
/// downstream probes use to lift per-bracket handler artefacts (the
/// LEA targets for the arpeggio / envelope tables, for instance).
fn find_dispatcher_cascade_with_offsets(
    payload: &[u8],
) -> Option<(DwDispatcher, alloc::vec::Vec<usize>)> {
    // Prefer the strict `CMP / BLT / SUBI` cascade (the exact
    // form every previously-validated module uses). Only when it
    // finds nothing do we fall back to the looser branch-based
    // scan that covers variants like tetris.dw, whose dispatch
    // adjusts D0 with `ADDI.B #0x40` / `SUBI.W` and branches with
    // `BCS` instead of `BLT`. Keeping strict-first guarantees the
    // working corpus's thresholds are byte-for-byte unchanged.
    chase_cascade(payload, is_dispatcher_bracket)
        .or_else(|| chase_cascade(payload, is_dispatcher_bracket_relaxed))
}

/// Walk a dispatcher cascade starting at the first `bracket`-match
/// in the code region, chaining each bracket's short conditional
/// branch (`BLT`/`BCS` at `bracket+4`, disp at `bracket+5`) to the
/// next. Returns the thresholds (the `CMP.B` immediates, in chain
/// order) and the bracket offsets.
fn chase_cascade(
    payload: &[u8],
    is_bracket: fn(&[u8], usize) -> bool,
) -> Option<(DwDispatcher, alloc::vec::Vec<usize>)> {
    let scan_end = 0x2000.min(payload.len());
    let mut cursor = 0usize;
    let mut entry_addr = None;
    while cursor + 9 < scan_end {
        if is_bracket(payload, cursor) {
            entry_addr = Some(cursor);
            break;
        }
        cursor += 2;
    }
    let mut next = entry_addr?;
    let mut thresholds = alloc::vec::Vec::with_capacity(3);
    let mut offsets = alloc::vec::Vec::with_capacity(3);
    // Cap the chain at 3 brackets — the spec defines no more
    // than three (sample / arpeggio / envelope).
    for _ in 0..3 {
        if !is_bracket(payload, next) {
            break;
        }
        offsets.push(next);
        let threshold = payload[next + 3];
        thresholds.push(threshold);
        // Short conditional branch (`BLT`/`BCS`) PC-relative
        // target. The opcode lives at offset +4 inside the
        // bracket; the 68000 short-branch formula is
        // `target = (bracket+4+2) + disp8`. A zero displacement
        // means the 16-bit form — outside our canonical encoding,
        // so we stop there.
        let disp = payload[next + 5] as i8 as i32;
        if disp == 0 {
            break;
        }
        let target = (next as i32 + 6 + disp) as usize;
        if target >= scan_end {
            break;
        }
        next = target;
    }
    if thresholds.is_empty() {
        None
    } else {
        Some((DwDispatcher { thresholds }, offsets))
    }
}

/// Recognise the strict 10-byte `CMP.B / BLT.B / SUBI.B` triple.
/// Both immediates must agree, which makes false positives rare
/// even across megabytes of data.
fn is_dispatcher_bracket(payload: &[u8], at: usize) -> bool {
    if at + 9 >= payload.len() {
        return false;
    }
    payload[at] == 0xB0
        && payload[at + 1] == 0x3C
        && payload[at + 2] == 0x00
        && payload[at + 4] == 0x6D
        && payload[at + 6] == 0x04
        && payload[at + 7] == 0x00
        && payload[at + 8] == 0x00
        && payload[at + 9] == payload[at + 3]
}

/// Looser bracket recogniser for non-canonical dispatch forms
/// (tetris.dw and kin): `CMP.B #XX, D0` (`B0 3C 00 XX`) followed
/// at +4 by a short *less-than* conditional branch — `BLT.B`
/// (`6D`) or `BCS.B` (`65`). The D0 adjustment that follows
/// (`SUBI.B` / `SUBI.W` / `ADDI.B #0x40` wrap) varies by module,
/// so it is *not* matched here.
///
/// To keep false positives low without the `SUBI` cross-check,
/// the `CMP` immediate is constrained to the plausible command-
/// threshold window `0x90..=0xD8`: real SetSample / SetVolumeEnvelope /
/// SetPitchArpeggio thresholds live there, while the `0xE0` long-wait
/// boundary (which branches with `BCC`, not `BLT`/`BCS`) and any
/// unrelated small-constant compares fall outside it.
fn is_dispatcher_bracket_relaxed(payload: &[u8], at: usize) -> bool {
    if at + 5 >= payload.len() {
        return false;
    }
    payload[at] == 0xB0
        && payload[at + 1] == 0x3C
        && payload[at + 2] == 0x00
        && (0x90..=0xD8).contains(&payload[at + 3])
        && (payload[at + 4] == 0x6D || payload[at + 4] == 0x65)
}

// -------------------------------------------------------------------
// Internal probes — see spec §3 for the rationale.
// -------------------------------------------------------------------

fn matches_at(payload: &[u8], at: usize, needle: &[u8]) -> bool {
    payload
        .get(at..at + needle.len())
        .is_some_and(|w| w == needle)
}

/// Locate the per-tick effect **jump table**. The command
/// dispatcher resolves an effect handler with
/// `MOVEA.W (table,Dn), A2` and ends with `JMP (d8,A3,Xn)`
/// (`4E F3`) or `JMP (A2)` (`4E D2`); the table base is loaded
/// into A2 a couple of instructions earlier by `LEA d16(PC), A2`
/// (`45 FA`) — absolute — or `LEA d16(A3), A2` (`45 EB`) —
/// A3-relative. Returns the table's file offset. Scans the early
/// code region and returns the first JMP whose preceding LEA resolves
/// in-bounds. (Idiom read from the replayer dispatch in Ghidra.)
fn find_effect_jump_table(payload: &[u8], start_offset: isize) -> Option<usize> {
    let scan_end = 0x2000.min(payload.len().saturating_sub(2));
    let mut i = 0usize;
    while i < scan_end {
        if payload[i] == 0x4E && (payload[i + 1] == 0xD2 || payload[i + 1] == 0xF3) {
            // The LEA sits one (`jmp-8`) or two (`jmp-10`)
            // instructions before the JMP.
            for back in [8usize, 10] {
                if i < back {
                    continue;
                }
                let lea = i - back;
                if payload[lea + 1] == 0xFA && payload[lea] == 0x45 {
                    let disp = i16::from_be_bytes([payload[lea + 2], payload[lea + 3]]) as isize;
                    let table = (lea as isize + 2) + disp;
                    if table >= 0 && (table as usize) < payload.len() {
                        return Some(table as usize);
                    }
                }
                if payload[lea + 1] == 0xEB && payload[lea] == 0x45 {
                    let disp = i16::from_be_bytes([payload[lea + 2], payload[lea + 3]]) as isize;
                    let table = start_offset + disp;
                    if table >= 0 && (table as usize) < payload.len() {
                        return Some(table as usize);
                    }
                }
            }
        }
        i += 2;
    }
    None
}

/// Half-volume test: the module uses the half-volume Effect8/Effect9
/// pair only when the **effect-8** handler opens with `ST (1,A0)`
/// (`50 E8`) AND the **effect-9** handler opens with `SF (1,A0)`
/// (`51 E8`). Handler entries in the jump table are A3-relative
/// `u16`s. (Both handler openings read from Ghidra.)
fn effect_handler_is_half_volume(payload: &[u8], table: usize, start_offset: isize) -> bool {
    let handler = |n: usize| -> Option<usize> {
        let e = table + n * 2;
        if e + 2 > payload.len() {
            return None;
        }
        let v = u16::from_be_bytes([payload[e], payload[e + 1]]) as isize;
        let f = start_offset + v;
        (f >= 0 && (f as usize) + 2 <= payload.len()).then_some(f as usize)
    };
    match (handler(8), handler(9)) {
        (Some(h8), Some(h9)) => {
            payload[h8] == 0x50
                && payload[h8 + 1] == 0xE8
                && payload[h9] == 0x51
                && payload[h9 + 1] == 0xE8
        }
        _ => false,
    }
}

/// Find the first `LEA -d(PC), A3` (`47 FA <disp_hi><disp_lo>` with
/// `disp_hi >= 0xF0` = negative 16-bit displacement) inside the
/// window. Returns the absolute file offset of the opcode prefix.
fn find_lea_a3_pc_neg(payload: &[u8], start: usize, end: usize) -> Option<usize> {
    let end = end.min(payload.len().saturating_sub(3));
    let mut i = start;
    while i < end {
        if payload[i] == 0x47 && payload[i + 1] == 0xFA && payload[i + 2] >= 0xF0 {
            return Some(i);
        }
        i += 2;
    }
    None
}


/// Breadth-first walk: starting at `entry`, scan each reachable
/// function body up to its `RTS`, descending into every `BSR.W`
/// target along the way. `probe` runs on each `(payload, body_start,
/// body_end)` triple; the first probe call that returns `Some(_)`
/// stops the walk and bubbles the value back.
///
/// The walk stops descending once `depth_limit` is exceeded, tracks
/// every visited body start in a [`BTreeSet`] to avoid cycles, and
/// caps each body at `body_limit` bytes (a defensive bound — real
/// Whittaker init helpers are a few hundred bytes long).
fn walk_call_graph<F>(payload: &[u8], entry: usize, depth_limit: usize, probe: F) -> Option<u32>
where
    F: Fn(&[u8], usize, usize) -> Option<u32>,
{
    const BODY_LIMIT: usize = 0x400;

    let mut visited: BTreeSet<usize> = BTreeSet::new();
    let mut queue: VecDeque<(usize, usize)> = VecDeque::new();
    queue.push_back((entry, 0));

    while let Some((start, depth)) = queue.pop_front() {
        if !visited.insert(start) {
            continue;
        }
        // Run the probe over this body.
        let body_end = (start + BODY_LIMIT).min(payload.len());
        if let Some(v) = probe(payload, start, body_end) {
            return Some(v);
        }
        if depth >= depth_limit {
            continue;
        }
        // Walk this body looking for `RTS` (stop) and `BSR.W`
        // (queue the target). Step 2 bytes — opcodes are 2-byte
        // aligned on 68000.
        let mut i = start;
        while i + 1 < body_end {
            // `RTS` = `4E 75` ends the function unconditionally.
            if payload[i] == 0x4E && payload[i + 1] == 0x75 {
                break;
            }
            // `BSR.W disp16` = `61 00 <hi> <lo>` with a signed
            // 16-bit displacement relative to the byte after `61 00`.
            if payload[i] == 0x61 && payload[i + 1] == 0x00 && i + 4 <= body_end {
                let disp = i16::from_be_bytes([payload[i + 2], payload[i + 3]]) as i32;
                let target = i as i32 + 2 + disp;
                if target >= 0 && (target as usize) < payload.len() {
                    queue.push_back((target as usize, depth + 1));
                }
                i += 4;
                continue;
            }
            i += 2;
        }
    }
    None
}

/// Find the sub-song header.
///
/// Two strategies tried in order:
///
/// 1. The canonical anchor pair `MULU.W #N, D0` + `LEA disp(PC),
///    A0` (or `LEA disp(A3), A0` on qball). Locates the table
///    base via the row-arithmetic step the replayer uses to turn
///    a host-supplied sub-song index into a row offset.
/// 2. **Fallback** when no MULU is emitted (modules that ship a
///    single sub-song skip the multiplication): scan the init
///    body for the first `LEA disp(PC), A0` whose target looks
///    like a sub-song row — i.e. four consecutive `u16` channel
///    offsets that all resolve within the file. This is what
///    rescues `millennium.dw` and similar minimal builds.
///
/// Returns the resolved file offset of the table base.
fn walk_for_sub_song_table(payload: &[u8], entry: usize) -> Option<usize> {
    if let Some(off) = walk_call_graph(payload, entry, 3, |p, s, e| {
        find_sub_song_lea(p, s, e).map(|v| v as u32)
    }) {
        return Some(off as usize);
    }
    walk_call_graph(payload, entry, 3, |p, s, e| {
        find_sub_song_lea_fallback(p, s, e).map(|v| v as u32)
    })
    .map(|v| v as usize)
}

/// Fallback sub-song-table probe used when no `MULU.W` anchor is
/// present in init (typical of single-sub-song modules where the
/// replayer skips the row multiplication entirely).
///
/// Walks every `LEA disp16(PC), A0` (`41 FA <hi> <lo>`) inside
/// the body and accepts the first target whose 10-byte window
/// starting at `target` matches a plausible sub-song row:
///
/// - Bytes `[0]` and `[1]` look like sane header values (speed
///   byte in `1..=0x40`).
/// - Bytes `[2..10]` decode as four `u16` channel offsets that
///   are all non-zero and within the file.
fn find_sub_song_lea_fallback(payload: &[u8], start: usize, end: usize) -> Option<usize> {
    let end_eff = end.min(payload.len().saturating_sub(3));
    let mut i = start;
    while i + 3 < end_eff {
        if payload[i] == 0x41 && payload[i + 1] == 0xFA {
            let disp = i16::from_be_bytes([payload[i + 2], payload[i + 3]]) as i32;
            // Only forward references — sub-song tables live past
            // the init code, never before it.
            if disp > 0 {
                let target = i as i32 + 2 + disp;
                if target >= 0 && (target as usize) + 10 <= payload.len() {
                    let t = target as usize;
                    if looks_like_sub_song_row(payload, t) {
                        return Some(t);
                    }
                }
            }
        }
        i += 2;
    }
    None
}

/// Heuristic check: does `payload[at..at+10]` plausibly represent
/// a 10-byte sub-song row (`[speed, delay, 4 × u16 channel
/// offsets]`)?
///
/// - Speed byte: `1..=0x40` (Whittaker songs never declare speed
///   = 0 in row 0, and historical max is ~32; 0x40 leaves
///   generous slack).
/// - Delay byte: any.
/// - Each `u16` channel offset: non-zero and within the file.
fn looks_like_sub_song_row(payload: &[u8], at: usize) -> bool {
    if at + 10 > payload.len() {
        return false;
    }
    let speed = payload[at];
    if speed == 0 || speed > 0x40 {
        return false;
    }
    for i in 0..4 {
        let p = at + 2 + 2 * i;
        let off = u16::from_be_bytes([payload[p], payload[p + 1]]) as usize;
        if off == 0 || off >= payload.len() {
            return false;
        }
    }
    true
}

/// Local matcher: `C0 FC 00 0A` or `C0 FC 00 12` (`MULU.W
/// #10/#18, D0`) followed within 8 bytes by a `LEA … , A0`
/// instruction — either `41 FA <hi> <lo>` (PC-relative, new
/// player) or `41 EB <hi> <lo>` (A3-relative, qball-era). The
/// MULU constant is rare enough that the pair is a strong
/// anchor for the sub-song row arithmetic, and the LEA target
/// is the table base.
fn find_sub_song_lea(payload: &[u8], start: usize, end: usize) -> Option<usize> {
    let end_eff = end.min(payload.len().saturating_sub(7));
    let mut i = start;
    while i + 8 < end_eff {
        if payload[i] == 0xC0
            && payload[i + 1] == 0xFC
            && payload[i + 2] == 0x00
            && matches!(payload[i + 3], 0x08 | 0x0A | 0x12)
        {
            // Scan up to 8 bytes after the MULU for the LEA A0.
            let scan_end = (i + 12).min(end_eff);
            let mut k = i + 4;
            while k + 3 < scan_end {
                if payload[k] == 0x41 && payload[k + 1] == 0xFA {
                    let disp = i16::from_be_bytes([payload[k + 2], payload[k + 3]]) as i32;
                    let target = k as i32 + 2 + disp;
                    if target >= 0 && (target as usize) < payload.len() {
                        return Some(target as usize);
                    }
                }
                // qball: `LEA disp(A3), A0` — A3 is the replayer
                // base, which the importer treats as file offset 0,
                // so the displacement IS the absolute file offset
                // (no PC arithmetic needed).
                if payload[k] == 0x41 && payload[k + 1] == 0xEB {
                    let disp = u16::from_be_bytes([payload[k + 2], payload[k + 3]]) as usize;
                    if disp < payload.len() {
                        return Some(disp);
                    }
                }
                k += 2;
            }
        }
        i += 2;
    }
    None
}

/// qball-era sample loader. The replayer keeps one base pointer
/// in A3 and addresses both sample-data and sample-info through
/// `LEA disp(A3), An` rather than the new player's PC-relative
/// `LEA disp(PC), An`. The on-disk anchor is therefore:
///
/// ```text
/// LEA  $XXXX(A3), A0     ; 41 EB <hi> <lo>      ; sample-data base
/// LEA  $YYYY(A3), A1     ; 43 EB <hi> <lo>      ; sample-info base
/// MOVEQ #N-1, D1         ; 72 <N-1>             ; sample count
/// ...
/// DBF  D1, <neg>         ; 51 C9 FF <disp>      ; loop back
/// ```
///
/// Note A1 (not A5) as the info base — that's the second
/// structural divergence we use to disambiguate qball from the
/// new player.
fn walk_for_sample_loader_a3(
    payload: &[u8],
    entry: usize,
) -> (Option<usize>, Option<usize>, Option<u32>) {
    let packed = walk_call_graph_u64(payload, entry, 3, |p, s, e| {
        let (data_off, info_off, n) = find_sample_loader_a3_in_body(p, s, e)?;
        Some(((data_off as u64) << 32) | ((info_off as u64 & 0x00FF_FFFF) << 8) | (n as u64 & 0xFF))
    });
    match packed {
        Some(v) => (
            Some((v >> 32) as usize),
            Some(((v >> 8) & 0x00FF_FFFF) as usize),
            Some((v & 0xFF) as u32),
        ),
        None => (None, None, None),
    }
}

/// Flat (non-call-graph) scan of the early code region for the
/// sample loader. Used as a fallback when the call-graph walk
/// from `init_offset` stalls on a stub that returns before
/// reaching the real loader.
///
/// `a3_relative = false` looks for the new-player PC-relative
/// pair (`41 FA / 4B FA`); `true` looks for the qball-era
/// A3-relative pair (`41 EB / 43 EB`). The scan window is bounded
/// to the first 8 KB — well past every observed loader, but short
/// of the bulk sample-data region where stray `72 xx` bytes could
/// produce false positives.
fn flat_scan_sample_loader(
    payload: &[u8],
    a3_relative: bool,
) -> (Option<usize>, Option<usize>, Option<u32>) {
    const SCAN_END: usize = 0x2000;
    let end = SCAN_END.min(payload.len());
    let found = if a3_relative {
        find_sample_loader_a3_in_body(payload, 0, end)
    } else {
        find_sample_loader_in_body(payload, 0, end)
    };
    match found {
        Some((data, info, n)) => (Some(data), Some(info), Some(n)),
        None => (None, None, None),
    }
}

fn find_sample_loader_a3_in_body(
    payload: &[u8],
    start: usize,
    end: usize,
) -> Option<(usize, usize, u32)> {
    const FORWARD_DBF_WINDOW: usize = 0x40;
    const BACKWARD_LEA_WINDOW: usize = 0x20;

    let end_eff = end.min(payload.len().saturating_sub(1));
    let mut i = start;
    while i + 1 < end_eff {
        if payload[i] == 0x72 {
            let n = payload[i + 1] as u32 + 1;
            if (1..=64).contains(&n) && has_backward_dbf_d1(payload, i + 2, end, FORWARD_DBF_WINDOW)
            {
                if let Some((data_off, info_off)) =
                    find_lea_a0_a1_a3_pair_before(payload, i, BACKWARD_LEA_WINDOW)
                {
                    return Some((data_off, info_off, n));
                }
            }
        }
        i += 2;
    }
    None
}

/// Walk backwards from `anchor` looking for the qball pair:
///
/// ```text
/// 41 EB <hi> <lo>     ; LEA disp(A3), A0  — sample-data base
/// 43 EB <hi> <lo>     ; LEA disp(A3), A1  — sample-info base
/// ```
///
/// A3 is the replayer base, which the importer aligns with file
/// offset 0, so the displacement is the absolute file offset of
/// each table — no PC arithmetic needed.
fn find_lea_a0_a1_a3_pair_before(
    payload: &[u8],
    anchor: usize,
    window: usize,
) -> Option<(usize, usize)> {
    let start = anchor.saturating_sub(window);
    let mut k = anchor.saturating_sub(4);
    while k >= start {
        if k + 7 < payload.len()
            && payload[k] == 0x41
            && payload[k + 1] == 0xEB
            && payload[k + 4] == 0x43
            && payload[k + 5] == 0xEB
        {
            let data_off = u16::from_be_bytes([payload[k + 2], payload[k + 3]]) as usize;
            let info_off = u16::from_be_bytes([payload[k + 6], payload[k + 7]]) as usize;
            if data_off < payload.len() && info_off < payload.len() {
                return Some((data_off, info_off));
            }
        }
        if k < 2 {
            break;
        }
        k -= 2;
    }
    None
}

/// Chained probe used to locate Whittaker's sample-loader helper,
/// derived from a Ghidra trace of the Xenon 2 title module. The
/// loader is built around this quartet of instructions, in order:
///
/// ```text
/// LEA  <disp>(PC), A0     ; 41 FA <hi> <lo>      ; sample-data base (on-disk)
/// LEA  <disp>(PC), A5     ; 4B FA <hi> <lo>      ; sample-info base (in-memory layout, but the
///                                                ;   on-disk bytes there carry the per-sample
///                                                ;   `loop_start` field — the loader fills the
///                                                ;   other slots from sample-data at runtime)
/// MOVEQ #N-1, D1          ; 72 <N-1>             ; loop count
/// ...                                            ; body : read length, freq, advance A0
/// DBF  D1, <neg>          ; 51 C9 FF <disp>      ; backward branch — repeat per sample
/// ```
///
/// Note the indexed `LEA (d8,PC,Xn),An` variant (`41 EB`) **doesn't
/// match the bytes Whittaker actually emits** in this module — the
/// `.dw` uses the plain PC-relative `41 FA`. The probes here are
/// driven by the bytes in the binary (Ghidra), not by any abstract
/// description.
///
/// Returns `(sample_data_offset, sample_info_offset,
/// number_of_samples)`. All three are `None` when no helper in
/// the call graph fits.
fn walk_for_sample_loader(
    payload: &[u8],
    entry: usize,
) -> (Option<usize>, Option<usize>, Option<u32>) {
    // Pack the triple into a u64: high 32 bits = sample-data
    // offset, middle 24 bits = sample-info offset, low 8 bits =
    // sample count. The bit widths fit any realistic Whittaker
    // module (4 GB / 16 MB / ≤ 64 samples).
    let packed = walk_call_graph_u64(payload, entry, 3, |p, s, e| {
        let (data_off, info_off, n) = find_sample_loader_in_body(p, s, e)?;
        Some(((data_off as u64) << 32) | ((info_off as u64 & 0x00FF_FFFF) << 8) | (n as u64 & 0xFF))
    });
    match packed {
        Some(v) => (
            Some((v >> 32) as usize),
            Some(((v >> 8) & 0x00FF_FFFF) as usize),
            Some((v & 0xFF) as u32),
        ),
        None => (None, None, None),
    }
}

/// Local matcher for the LEA / LEA / MOVEQ / DBF sequence inside a
/// single function body. Scans for `MOVEQ #N-1, D1` (rare enough
/// to be a strong anchor), then verifies that a `DBF D1,
/// <backward>` follows within a small window, then walks back to
/// pick up the two `LEA (PC), An` instructions that load the
/// table bases.
///
/// Returns `(sample_data_offset, sample_info_offset, N)`.
fn find_sample_loader_in_body(
    payload: &[u8],
    start: usize,
    end: usize,
) -> Option<(usize, usize, u32)> {
    const FORWARD_DBF_WINDOW: usize = 0x40;
    const BACKWARD_LEA_WINDOW: usize = 0x20;

    let end_eff = end.min(payload.len().saturating_sub(1));
    let mut i = start;
    while i + 1 < end_eff {
        // `MOVEQ #imm, D1` = `72 imm`
        if payload[i] == 0x72 {
            let n = payload[i + 1] as u32 + 1;
            if (1..=64).contains(&n) && has_backward_dbf_d1(payload, i + 2, end, FORWARD_DBF_WINDOW)
            {
                if let Some((data_off, info_off)) =
                    find_lea_pair_before(payload, i, BACKWARD_LEA_WINDOW)
                {
                    return Some((data_off, info_off, n));
                }
            }
        }
        i += 2;
    }
    None
}

/// Walk backwards from `anchor` looking for the PC-relative
/// loader pair
///
/// ```text
/// 41 FA <hi> <lo>      ; LEA disp(PC), A0  — sample-data base
/// 4{B,3} FA <hi> <lo>  ; LEA disp(PC), A5 or A1  — sample-info base
/// ```
///
/// emitted right before the loop's `MOVEQ`. The info base lands
/// in A5 (`4B FA`) on the main new player (xenon2, beast1.*) but
/// in A1 (`43 FA`) on the leaner SFX-style rips (xenon-sfx,
/// leviathan-sfx); both write the same per-sample struct, so we
/// accept either register. Both LEAs resolve via PC-relative
/// displacement (next-instruction PC + signed disp16). Returns
/// `(sample_data_target, sample_info_target)`.
fn find_lea_pair_before(payload: &[u8], anchor: usize, window: usize) -> Option<(usize, usize)> {
    // First, find the `41 FA` (A0). The info-base LEA is expected
    // 4 bytes later (right after the 4-byte `LEA disp(PC), A0`).
    let start = anchor.saturating_sub(window);
    let mut k = anchor.saturating_sub(4);
    while k >= start {
        let info_reg = k + 4 < payload.len() && (payload[k + 4] == 0x4B || payload[k + 4] == 0x43);
        if k + 7 < payload.len()
            && payload[k] == 0x41
            && payload[k + 1] == 0xFA
            && info_reg
            && payload[k + 5] == 0xFA
        {
            let data_disp = i16::from_be_bytes([payload[k + 2], payload[k + 3]]) as i32;
            let info_disp = i16::from_be_bytes([payload[k + 6], payload[k + 7]]) as i32;
            let data_target = k as i32 + 2 + data_disp;
            let info_target = (k + 4) as i32 + 2 + info_disp;
            if data_target >= 0
                && info_target >= 0
                && (data_target as usize) < payload.len()
                && (info_target as usize) < payload.len()
            {
                return Some((data_target as usize, info_target as usize));
            }
        }
        if k < 2 {
            break;
        }
        k -= 2;
    }
    None
}

/// `DBF D1, <neg disp16>` = `51 C9 <hi> <lo>` with `hi >= 0xF0`
/// (signed-negative displacement, i.e. branch backward). Returns
/// true when present within `window` bytes from `start`.
fn has_backward_dbf_d1(payload: &[u8], start: usize, end: usize, window: usize) -> bool {
    let lim = (start + window)
        .min(end)
        .min(payload.len().saturating_sub(3));
    let mut j = start;
    while j + 3 <= lim {
        if payload[j] == 0x51 && payload[j + 1] == 0xC9 && payload[j + 2] >= 0xF0 {
            return true;
        }
        j += 2;
    }
    false
}

/// `walk_call_graph` variant whose probe returns a `u64` payload.
/// Used by [`walk_for_sample_loader`] to bundle two independent
/// offsets into one return slot. The walk semantics are
/// otherwise identical to [`walk_call_graph`].
fn walk_call_graph_u64<F>(payload: &[u8], entry: usize, depth_limit: usize, probe: F) -> Option<u64>
where
    F: Fn(&[u8], usize, usize) -> Option<u64>,
{
    const BODY_LIMIT: usize = 0x400;

    let mut visited: BTreeSet<usize> = BTreeSet::new();
    let mut queue: VecDeque<(usize, usize)> = VecDeque::new();
    queue.push_back((entry, 0));

    while let Some((start, depth)) = queue.pop_front() {
        if !visited.insert(start) {
            continue;
        }
        let body_end = (start + BODY_LIMIT).min(payload.len());
        if let Some(v) = probe(payload, start, body_end) {
            return Some(v);
        }
        if depth >= depth_limit {
            continue;
        }
        let mut i = start;
        while i + 1 < body_end {
            if payload[i] == 0x4E && payload[i + 1] == 0x75 {
                break;
            }
            if payload[i] == 0x61 && payload[i + 1] == 0x00 && i + 4 <= body_end {
                let disp = i16::from_be_bytes([payload[i + 2], payload[i + 3]]) as i32;
                let target = i as i32 + 2 + disp;
                if target >= 0 && (target as usize) < payload.len() {
                    queue.push_back((target as usize, depth + 1));
                }
                i += 4;
                continue;
            }
            i += 2;
        }
    }
    None
}

/// Find the per-tick `Play()` function — characterised by `47 FA`
/// near `4A 2B` near `67` (a `LEA / TST / BEQ` sequence common at
/// the top of the per-channel loop).
fn find_play_anchor(payload: &[u8], start: usize, end: usize) -> Option<usize> {
    let end = end.min(payload.len().saturating_sub(10));
    let mut i = start;
    while i < end {
        if payload[i] == 0x47
            && payload[i + 1] == 0xFA
            && payload[i + 4] == 0x4A
            && payload[i + 5] == 0x2B
            && payload[i + 8] == 0x67
        {
            return Some(i);
        }
        i += 2;
    }
    None
}

// -------------------------------------------------------------------
// Feature-flag detection. Each probe scans a small window around
// `play_offset` (the per-frame entry point) for a fixed byte pattern
// taken from the replayer's effect handlers (Ghidra).
// -------------------------------------------------------------------

const PLAY_FEATURE_WINDOW: usize = 0x100;

fn detect_features(payload: &[u8], play_offset: usize) -> DwFeatures {
    let mut f = DwFeatures::default();
    let window_end = (play_offset + PLAY_FEATURE_WINDOW).min(payload.len());

    // `enable_delay_counter` / `enable_delay_multiply`
    // ------------------------------------------------
    // Top of `Play` reads a per-frame counter via
    // `MOVE.B disp(PC), Dn` (`10 3A <hi> <lo>`). When the
    // counter is multiplied by a per-module constant, the
    // probe finds a `MULU.W #imm` (`C0 FC <hi> <lo>`) six
    // bytes after the `MOVE.B` (op + ext = 4 bytes, then the
    // MULU starts at +6).
    if let Some(i) = find_pair(payload, play_offset, window_end, 0x10, 0x3A) {
        f.enable_delay_counter = true;
        if i + 7 < window_end && payload[i + 6] == 0xC0 && payload[i + 7] == 0xFC {
            f.enable_delay_multiply = true;
        }
    }

    // `enable_square_waveform`
    // ------------------------
    // `LEA <abs.l>, A1` (`20 7A …`) followed two opcode bytes
    // later by `MOVE.W disp(PC), Dn` (`30 3A …`). The pair
    // sets up the rolling sample-data pointer for the square
    // morph; absence means there's no morph.
    if find_pair_with_gap(payload, play_offset, window_end, 0x20, 0x7A, 0x30, 0x3A, 2).is_some() {
        f.enable_square_waveform = true;
    }

    // `enable_sample_transpose`
    // -------------------------
    // After the track-byte read at the top of `read_track_commands`,
    // a `BMI.W` (`6B 00`) skips the transpose-add when the byte is
    // a command (high bit set). The signature pair is `6B 00` at
    // some offset, then `D0 2D` (`ADD.B disp(A5), Dn`) four bytes
    // later.
    if let Some(i) = find_pair(payload, play_offset, window_end, 0x6B, 0x00) {
        if i + 5 < window_end && payload[i + 4] == 0xD0 && payload[i + 5] == 0x2D {
            f.enable_sample_transpose = true;
        }
    }

    // `enable_arpeggio` / `enable_envelopes`
    // --------------------------------------
    // Both probes look for the per-table `MOVE.L (A2), …`
    // pattern (`21 4A`) that resolves an arpeggio / envelope
    // entry pointer. Envelopes additionally have `MOVE.B
    // (A2), …` (`11 6A`) right after. The handlers live in
    // the dispatcher region around `play+0x290` (the
    // `0x90..=0x9F` / `0xA0..=0xAF` byte-range branches we
    // traced in Ghidra at 0x494), well past the original
    // `PLAY_FEATURE_WINDOW` of 0x100 bytes. Use the wider
    // `handler_end` (= 0x1000-byte) scan that the
    // handler-region probes below already use, otherwise
    // both flags stay falsely `false` on a module whose
    // dispatcher does carry the table-driven branches.
    let arp_env_end = 0x1000.min(payload.len());
    if find_pair(payload, play_offset, arp_env_end, 0x21, 0x4A).is_some() {
        f.enable_arpeggio = true;
        if let Some(i) = find_pair(payload, play_offset, arp_env_end, 0x11, 0x6A) {
            // `enable_envelopes` requires both the `21 4A` and
            // the `11 6A` to be present; arpeggios alone keep
            // just the former.
            let _ = i;
            f.enable_envelopes = true;
        }
    }

    // ------------------------------------------------------------------
    // Effect-handler features. The replayers dispatch effects through
    // a handler region whose address comes from an indirect table
    // built by init (Ghidra). Rather than re-discover that table for
    // every module shape, we scan the entire code region (everything
    // below the first sample-data table, capped at ~4 KB) for the
    // *exact* handler-opening byte sequences each effect uses.
    //
    // Two consequences:
    //
    // 1. A module that emits one feature's handler bytes but
    //    doesn't wire them through the dispatcher will be falsely
    //    flagged. We mitigate this by requiring the strongest
    //    discriminator available for each feature — a 4-byte
    //    opcode + operand instead of a bare 2-byte opcode.
    //
    // 2. Multiple Effect8 interpretations can be present at once.
    //    Spec §10.7 disambiguates them by *priority*
    //    (volume_fade > channel_transpose > half_volume), so the
    //    runtime layer applies the precedence rule even when
    //    several flags are true.
    // ------------------------------------------------------------------
    let handler_end = 0x1000.min(payload.len());

    // `enable_volume_fade` — handler writes a fade-speed byte to
    // `$5F1(A3)` via `MOVE.B (A1)+, $5F1(A3)` (`17 59 05 F1`).
    if find_quad(payload, play_offset, handler_end, 0x17, 0x59, 0x05, 0xF1).is_some() {
        f.enable_volume_fade = true;
    }

    // `enable_half_volume` is decided in `detect()` via the effect
    // jump table (the blind `50 E8`/`51 E8` scan that used to live
    // here matched the vibrato handlers and flagged nearly every
    // module — see `find_effect_jump_table`).

    // `enable_vibrato` — StartVibrato handler reads two
    // parameter bytes into chan[+8] / chan[+9] and clears
    // chan[+A] (`11 59 00 08 ; 11 59 00 09`). Both adjacent
    // bytes are required to discriminate from unrelated
    // `MOVE.B (A1)+, disp(A0)` writes.
    if find_quad(payload, play_offset, handler_end, 0x11, 0x59, 0x00, 0x08).is_some()
        && find_quad(payload, play_offset, handler_end, 0x11, 0x59, 0x00, 0x09).is_some()
    {
        f.enable_vibrato = true;
    }

    // `enable_channel_transpose` — `do_frame_stuff` adds
    // `chan[+disp]` to the period via `ADD.B disp(A0), Dn`
    // (`D0 28 00 03`). The `+3` constant matches the
    // transpose slot, so it's a stronger discriminator than
    // the bare `D0 28` opcode.
    if find_quad(payload, play_offset, handler_end, 0xD0, 0x28, 0x00, 0x03).is_some() {
        f.enable_channel_transpose = true;
    }

    // `note_repeat_is_tie` — the play loop's `CMPI.B #$83,(A1)`
    // (`0C 11 00 83`) guard on the one-frame-early DMA clear. Present
    // → cmd `0x83` is a tie (see the field doc). All four confirmed
    // tie families use the `(A1)` form; the retrigger families carry
    // no `CMPI.B #$83` at all, so the exact `(A1)` quad is both
    // sufficient and free of false positives across the corpus.
    if find_quad(payload, play_offset, handler_end, 0x0C, 0x11, 0x00, 0x83).is_some() {
        f.note_repeat_is_tie = true;
    }

    f
}

/// Scan for the first occurrence of a 4-byte sequence
/// `[b0, b1, b2, b3]` at an even offset in `[start, end)`. Returns
/// the position of `b0`. Used for handler-byte discriminators
/// where the 2-byte opcode + 2-byte operand together identify
/// a specific instruction.
fn find_quad(
    payload: &[u8],
    start: usize,
    end: usize,
    b0: u8,
    b1: u8,
    b2: u8,
    b3: u8,
) -> Option<usize> {
    let lim = end.min(payload.len().saturating_sub(3));
    let mut i = start;
    while i + 3 < lim {
        if payload[i] == b0 && payload[i + 1] == b1 && payload[i + 2] == b2 && payload[i + 3] == b3
        {
            return Some(i);
        }
        i += 2;
    }
    None
}

/// Scan for the first occurrence of the 2-byte sequence
/// `[high, low]` at an even offset in `[start, end)`. Returns
/// the position of `high`.
fn find_pair(payload: &[u8], start: usize, end: usize, high: u8, low: u8) -> Option<usize> {
    let lim = end.min(payload.len().saturating_sub(1));
    let mut i = start;
    while i + 1 < lim {
        if payload[i] == high && payload[i + 1] == low {
            return Some(i);
        }
        i += 2;
    }
    None
}

/// Scan for `[h1, l1]` followed exactly `gap` opcode bytes later
/// by `[h2, l2]`. Returns the position of `h1` on match.
#[allow(clippy::too_many_arguments)] // a byte-pattern matcher — each operand is one opcode byte
fn find_pair_with_gap(
    payload: &[u8],
    start: usize,
    end: usize,
    h1: u8,
    l1: u8,
    h2: u8,
    l2: u8,
    gap: usize,
) -> Option<usize> {
    let lim = end.min(payload.len().saturating_sub(3 + gap));
    let mut i = start;
    while i + 3 + gap < lim {
        if payload[i] == h1
            && payload[i + 1] == l1
            && payload[i + 2 + gap] == h2
            && payload[i + 3 + gap] == l2
        {
            return Some(i);
        }
        i += 2;
    }
    None
}

/// Period-table picker for the new player — **resolved from the
/// replayer's own code**, not guessed from a stray data word.
///
/// The new player computes a note's period in `Play()` with the
/// idiom (verified in Ghidra across bad company / grimblood /
/// tetris — every new-player module shares it):
///
/// ```text
/// 45 FA <d16>     LEA    (d16,PC), A2     ; A2 = period table base
/// 32 2D 00 0A     MOVE.W (0xa,A5), D1w    ; D1 = sample fine-tune word
/// 74 0A           MOVEQ  #0xa, D2         ; (then MULU D1,D0 ; LSR.L D2)
/// ```
///
/// so `period = table[note*2] * sample[+0xa] >> 10`. The `LEA`
/// target IS the table the replayer actually uses; reading its
/// first word disambiguates P3 (8192) from P2 (4096) reliably.
/// This replaces the old "first 8192/4096 anywhere in the payload"
/// scan, which mis-detected bad company as P2 (its real table at
/// 0x6f8 starts with 8192 = P3, but a stray 8192 word appears
/// earlier) and played it an octave too high.
///
/// `00 0A` (the `MOVE.W (0xa,A5)` displacement) plus the `MOVEQ
/// #0xa` is distinctive enough to anchor on; the LEA-to-A2 opcode
/// (`45 FA`) is then resolved through [`read_lea_a2_at`]. Falls
/// back to the old data-scan heuristic when the idiom is absent
/// (keeps every currently-loading module loading).
fn find_period_table_via_lea(payload: &[u8]) -> Option<PeriodTable> {
    // Idiom core: 45 FA <d16> 32 2D 00 0A 74 0A
    let mut i = 0;
    while i + 10 <= payload.len() {
        // Idiom core: `45 FA <d16>` (LEA (d16,PC),A2 → period table)
        // immediately followed by `32 2D 00 <m>` (MOVE.W (m,A5),D1 →
        // per-sample fine-tune multiplier) and `74 0A` (MOVEQ #10,D2 →
        // the `>> 10` shift count). The multiplier displacement `<m>`
        // is the fine-tune word's offset inside the sample/instrument
        // record: `0x0A` on the new player and the empire-era old
        // stream (C8), `0x0C` on the leviathan-era old stream (C7).
        // Accept both — every other byte pins the idiom tightly enough
        // that widening this one displacement risks no false positive.
        if payload[i] == 0x45
            && payload[i + 1] == 0xFA
            && payload[i + 4] == 0x32
            && payload[i + 5] == 0x2D
            && payload[i + 6] == 0x00
            && (payload[i + 7] == 0x0A || payload[i + 7] == 0x0C)
            && payload[i + 8] == 0x74
            && payload[i + 9] == 0x0A
        {
            if let Some(table_off) = read_lea_a2_at(payload, i) {
                if let Some(bytes) = payload.get(table_off..table_off + 2) {
                    let w = u16::from_be_bytes([bytes[0], bytes[1]]);
                    return match w {
                        8192 => Some(PeriodTable::P3),
                        4096 => Some(PeriodTable::P2),
                        _ => None,
                    };
                }
            }
        }
        i += 2;
    }
    None
}

/// Locate the **per-instrument volume table** of the leviathan-era
/// old-stream fine-tune player (C7). Its `SetSample` handler arms the
/// instrument record with `LEA (d16,PC),A5 ; MULU.W #0x10,D0` (bytes
/// `4B FA <d16> C0 FC 00 10`), so the records sit at
/// `base = (idiom+2) + d16`, stride `0x10`; the note trigger writes
/// `AUDxVOL = *(0xE,A5)` (Ghidra leviathan `play` @0x17c), i.e. a
/// **static** per-instrument volume word at record byte `+0xE`. Returns
/// `base`. The `MULU.W #0x10` pins the 16-byte stride (the empire family
/// uses `#0xC`, so it never matches here). Only consumed when
/// `period_via_finetune` is set — qball/new players don't carry it.
fn find_instrument_volume_table(payload: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 8 <= payload.len() {
        if payload[i] == 0x4B
            && payload[i + 1] == 0xFA
            && payload[i + 4] == 0xC0
            && payload[i + 5] == 0xFC
            && payload[i + 6] == 0x00
            && payload[i + 7] == 0x10
        {
            let disp = i16::from_be_bytes([payload[i + 2], payload[i + 3]]) as isize;
            let base = i as isize + 2 + disp;
            if base >= 0 && (base as usize) < payload.len() {
                return Some(base as usize);
            }
        }
        i += 2;
    }
    None
}

/// Heuristic period-table picker for the new player. Scans the
/// whole payload for the first occurrence of `0x2000` (= 8192,
/// start of `PERIODS3`) or `0x1000` (= 4096, start of `PERIODS2`),
/// big-endian, and returns the corresponding variant. This stays
/// conservative — when neither marker is found, the caller falls
/// back to `PERIODS2`. Used only as a fallback for
/// [`find_period_table_via_lea`] when the period idiom isn't found.
fn guess_new_period_table(payload: &[u8]) -> Option<PeriodTable> {
    let mut i = 0;
    while i + 2 <= payload.len() {
        let w = u16::from_be_bytes([payload[i], payload[i + 1]]);
        if w == 8192 {
            return Some(PeriodTable::P3);
        }
        if w == 4096 {
            return Some(PeriodTable::P2);
        }
        i += 2;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_short_payloads() {
        assert!(detect(&[0u8; 4]).is_none());
    }

    #[test]
    fn rejects_sc68_magic() {
        let mut buf = [0u8; 64];
        buf[..4].copy_from_slice(&SC68_MAGIC);
        assert!(detect(&buf).is_none());
    }

    #[test]
    fn rejects_non_stub_opening() {
        // Random non-stub bytes
        let buf = [0xFFu8; 64];
        assert!(detect(&buf).is_none());
    }

    #[test]
    fn finds_leviathan_instrument_volume_table() {
        // The leviathan `SetSample` arming idiom `LEA (d16,PC),A5 ;
        // MULU.W #0x10,D0` = `4B FA <d16> C0 FC 00 10`. With the idiom
        // at offset 4, PC = 6, d16 = 0x000A → base = 6 + 10 = 16.
        let mut buf = vec![0u8; 64];
        buf[4..12].copy_from_slice(&[0x4B, 0xFA, 0x00, 0x0A, 0xC0, 0xFC, 0x00, 0x10]);
        assert_eq!(find_instrument_volume_table(&buf), Some(16));

        // The empire stride (`MULU.W #0xC`, `…00 0C`) must NOT match —
        // its instrument records are 12 bytes with envelope-driven volume.
        let mut buf2 = vec![0u8; 64];
        buf2[4..12].copy_from_slice(&[0x4B, 0xFA, 0x00, 0x0A, 0xC0, 0xFC, 0x00, 0x0C]);
        assert_eq!(find_instrument_volume_table(&buf2), None);
    }
}
