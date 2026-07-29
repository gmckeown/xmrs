//! Recover a v10..v30 Rob Hubbard tune's table layout from its `.sid` image.
//!
//! The fifteen [`OneSid`](super::one_sid::OneSid) records are hand-transcribed
//! address tables — which is why a tune outside that list cannot be imported at
//! all. This module does for the fx-mask replayer generations what
//! [`G2Config::detect`](super::g2::G2Config::detect) does for the last one: it
//! scans the 6502 image for the instructions that reference each table and reads
//! the address out of the operand.
//!
//! Every signature below was found by cross-referencing the fifteen known
//! layouts against their own binaries (`examples/sid_xref_probe.rs`): each table
//! turns out to have **exactly one** referencing instruction in every tune, so
//! there is nothing to disambiguate once the right instruction shape is known.
//!
//! What this does *not* recover is the behavioural half of a `OneSid` record —
//! fractional tempo, skydive shape, fx-byte layout, per-tune note-table overflow
//! fixups. Those were each established against the sidplay oracle for one tune
//! and do not follow from the image; a detected tune gets the defaults.

/// How a tune's note stream encodes the optional payload byte that follows an
/// entry header whose bit 7 is set.
///
/// Read straight out of the note-fetch routine, which is where it belongs: the
/// hand-written `OneSid.version` stands in for this today, and disagrees with
/// the binary on three of the fifteen known tunes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StreamLayout {
    /// One payload byte — an instrument select when its bit 7 is clear, else a
    /// portamento with the speed and direction packed into the remaining bits.
    /// `INY : LDA (zp),Y : BPL instr : STA porta,X : JMP`.
    #[default]
    PortaByte,
    /// Two payload bytes for a portamento (a 14-bit register delta); an
    /// instrument select is still one. `… STA porta_hi,X : INY : LDA (zp),Y :
    /// STA porta_lo,X`.
    PortaWord,
    /// No portamento branch at all: the payload is always an instrument select.
    /// `INY : LDA (zp),Y : AND #$1F` (Thing on a Spring) or `… : STA zp,X`
    /// (Spellbound).
    InstrumentOnly,
}

/// Where a tune's tables live, as recovered from its image.
///
/// Addresses are **engine** addresses (what the 6502 sees), matching the
/// `OneSid` fields of the same name. `0` means "this tune has no such table",
/// again as in `OneSid`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SidLayout {
    /// Load address, from the two bytes ahead of the image.
    pub load: u16,
    /// Voices per sub-song (3, except The Human Race's 2).
    pub song_track_qty: usize,
    /// Sub-song table: `song_track_qty` low bytes then as many high bytes, per
    /// sub-song.
    pub song_list: u16,
    pub song_list_qty: usize,
    /// Split pattern-pointer table: all low bytes, then all high bytes.
    pub patt_ptl: u16,
    pub patt_pth: u16,
    pub patt_qty: usize,
    /// Instrument table, eight bytes per record.
    pub instr: u16,
    pub instr_qty: usize,
    /// Sound-effect table (v10/v15 only), sixteen bytes per effect, or 0. Not a
    /// per-instrument table despite sitting right behind the instruments in most
    /// tunes — the engine blasts fourteen of its bytes straight at `$D400`.
    pub fx_v1: u16,
    /// Per-instrument extended-fx table (v20+), eight bytes per record, or 0.
    /// Byte +6 is the filter resonance, +7 the signed cutoff step — which is why
    /// the filter table and this one are the same address whenever both exist.
    pub fx_v2: u16,
    /// Note-stream payload encoding, from the note-fetch routine.
    pub stream: StreamLayout,
    /// Zero-page pointer the note stream is read through.
    pub stream_zp: u8,
    /// Reload value of the periodic frame-skip counter some replayers open
    /// `play` with, or 0 for the tunes that run every frame. `N` means the
    /// replayer does nothing one frame in `N + 1`, so the tune plays at
    /// `N / (N + 1)` of 50 Hz.
    pub frame_skip_reset: u8,
    /// Engine address of the row-cadence counter, and of the byte it reloads
    /// from. `(0, 0)` if the gate was not found.
    ///
    /// The replayer counts this down once a frame and advances the note stream
    /// only on the frame the counter equals its reload, so a row lasts
    /// `reload + 1` frames (Ghidra, Zoids `play` `$1048`):
    /// ```text
    ///   DEC ctr : BPL .keep : LDA reload : STA ctr
    ///   .keep   : LDA ctr   : CMP reload : BNE .no_row
    /// ```
    pub tempo_ctr: u16,
    pub tempo_reload: u16,
    /// The reload value itself — the `resetspd` of a `OneSid` record.
    pub resetspd: u8,
    /// Engine address of the per-sub-song reload table the init routine copies
    /// into `tempo_reload`, or 0 when the tune has a single cadence. Indexed by
    /// sub-song.
    pub resetspd_table: u16,
    /// Reload of the *second* counter, the one that makes the cadence
    /// fractional, or 0 when the tune has none.
    ///
    /// A 50 Hz replayer whose row counter is an integer can only reach tempos of
    /// `50/n`. These players get in between by running a second counter that,
    /// on the frame it wraps, **skips** the row counter — so a row lasts
    /// `(R+1)·(Ka+1)/Ka` frames on average. Ghidra, Lightforce `play` `$f10e`:
    /// ```text
    ///   DEC ka : BPL .row : LDA #Ka : STA ka : JMP .after
    ///   .row   : DEC ctr  : BPL .after : LDA reload : STA ctr
    /// ```
    /// This is the same block as [`Self::frame_skip_reset`] down to the last
    /// byte, save the terminator: `RTS` there drops the whole frame, `JMP` here
    /// drops only the row. Both are the same trick, and the same formula covers
    /// them.
    pub tempo_ka: u8,
    /// Engine address of the `LDA #Ka` immediate, which the init routine patches
    /// when the tune varies `Ka` per sub-song (Delta). 0 if not found.
    pub tempo_ka_imm: u16,
    /// Engine address of the per-sub-song `Ka` table, or 0.
    pub tempo_ka_table: u16,
    /// Value the note-on loads into the filter's cutoff accumulator, or 0.
    ///
    /// The sweep is `LDA acc,X : CLC : ADC step,Y : STA acc,X : STA $D416`, so
    /// the accumulator falls out of the cutoff write; the note-on then seeds it
    /// with an immediate (Ghidra, Ace 2 `$e0fa`: `LDA #$30 : STA $e58d,X`).
    pub filter_seed: u8,
    /// Engine address of the three-note arpeggio table (pairs of signed
    /// semitones, indexed by the instrument's fx byte 0), or 0.
    ///
    /// Ghidra, Lightforce `$f45b`: `LDA fx_v2,Y : ASL : TAY : LDA arp3,Y`.
    pub arp3_table: u16,
    /// Whether the sub-song order lists are run-length compressed, as
    /// `phrase, (count, phrase)…` rather than a flat list of phrase indices.
    ///
    /// This is the v30 (Delta) order walker, and it is the costliest thing to
    /// get wrong: read a compressed list flat and the tune plays the repeat
    /// counts as if they were phrases — wrong music, not merely wrong tempo.
    /// Ghidra, Delta `play` `$bf85`:
    /// ```text
    ///   DEC rep,X : BNE .keep : INC idx,X : LDY idx,X
    ///   LDA (zp),Y : BMI .keep : STA rep,X : INC idx,X
    /// ```
    pub order_compressed: bool,
    /// The v10 "skydive": how long a note must be before fx-mask bit 1 starts
    /// walking its pitch, and by how much per active frame.
    ///
    /// Ghidra, Commando `play` `$5336`:
    /// ```text
    ///   LDA fxmask : AND #$02 : BEQ .skip
    ///   LDA len,X  : AND #$1F : CMP #when+1 : BCC .skip
    ///   LDA frame  : AND #$01 : BEQ .skip
    ///   INC hi,X   : INC hi,X            ; two steps of the HIGH byte = +512
    /// ```
    /// The step is always a whole number of high-byte increments, so the add is
    /// `±256 ×` however many `INC`/`DEC` the block spends. A tune whose bit 1
    /// touches no frequency byte at all reports `0`, which is the "climb the
    /// note index instead" shape that Spellbound uses.
    pub skydive_when: u8,
    pub skydive_add: i16,
    /// Whether the effect block computes a full 16-bit frequency — the mark of
    /// the period-interpolation vibrato.
    ///
    /// Most effects nudge the frequency register's high byte and write `$D400`
    /// or `$D401` alone (Ghidra, Commando `$5352`: `INC hi,X : INC hi,X :
    /// STA $D401,Y`). A vibrato that interpolates *between* two table periods
    /// has to produce a whole 16-bit value, so its block writes **both** halves
    /// (Spellbound `$e2a4`: the shift-and-subtract that builds `$00ac/$00ab`,
    /// then `STA $D400,Y : STA $D401,X`).
    ///
    /// That is exactly the `interp_vibrato` distinction — true for Spellbound
    /// and Thrust and nothing else, which is what the records say. It is NOT the
    /// v10-against-v15 split it looks like at first: Thrust is a v10 tune that
    /// happens to share the mechanism.
    pub interp_vibrato: bool,
    /// Semitone offset of the fx-mask bit-2 arpeggio, or 0 when the tune's bit 2
    /// is not an arpeggio at all.
    ///
    /// This is the v10-against-v15 tell that the note stream cannot give: both
    /// families decode as `InstrumentOnly` on some tunes, but their bit-2 blocks
    /// differ. v10 jumps an octave (Ghidra, Commando `$536c`: `CLC : ADC #$0C`),
    /// the v15/Simple family drops a fourth (Spellbound `$e32e`, International
    /// Karate: `SEC : SBC #$05`).
    ///
    /// Read from *inside* the bit-2 block. The bare byte pairs occur all over an
    /// image — Last V8 carries both and The Human Race carries the `-5` while
    /// being neither — so the anchor is the whole signal here.
    pub arp_semitones: i8,
}

/// Read the little-endian 16-bit operand that follows the opcode at `at`.
fn operand(img: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*img.get(at + 1)?, *img.get(at + 2)?]))
}

/// First index at which `pred` holds, scanning the whole image.
fn scan(img: &[u8], window: usize, pred: impl Fn(&[u8]) -> bool) -> Option<usize> {
    (0..img.len().checked_sub(window)?).find(|&k| pred(&img[k..k + window]))
}

impl SidLayout {
    /// Recover the layout of a v10..v30 tune, or `None` if the image does not
    /// look like one.
    ///
    /// `data` is the whole `.sid` file, header included.
    pub fn detect(data: &[u8]) -> Option<Self> {
        Self::detect_reason(data).ok()
    }

    /// Everything `detect` recovers, before the layout is checked for sense.
    /// Public as a diagnostic — a rejected image can be inspected rather than
    /// merely counted.
    pub fn detect_unchecked(data: &[u8]) -> Option<Self> {
        let img = data.get(super::PSID_DATA..)?;
        let load = u16::from_le_bytes([*data.get(124)?, *data.get(125)?]);

        // ---- periodic frame skip -------------------------------------------
        // Some replayers open `play` by counting down and returning early on the
        // wrap, which drops one frame in `N + 1`:
        //   DEC ctr : BPL on : LDA #N : STA ctr : RTS       (zero page)
        //   DEC ctr : BPL on : LDA #N : STA ctr : RTS       (absolute)
        // The counter and the store must name the same address, which is what
        // keeps this from matching any other countdown in the engine.
        let play = u16::from_be_bytes([*data.get(12)?, *data.get(13)?]);
        let frame_skip_reset = play
            .checked_sub(load)
            .map(|k| k as usize)
            .and_then(|k| match img.get(k..k + 12)? {
                [0xC6, z, 0x10, _, 0xA9, n, 0x85, z2, 0x60, ..] if z == z2 => Some(*n),
                [0xCE, l, h, 0x10, _, 0xA9, n, 0x8D, l2, h2, 0x60, ..] if (l, h) == (l2, h2) => {
                    Some(*n)
                }
                _ => None,
            })
            .unwrap_or(0);

        // ---- instrument table -------------------------------------------
        // `LDA instr+0,X : STA $D402,Y` — seeding the pulse width low byte from
        // the record. The only absolute,X load in the engine that feeds $D402,
        // and present in all fifteen known tunes.
        let instr = operand(
            img,
            scan(img, 6, |w| w[0] == 0xBD && w[3..6] == [0x99, 0x02, 0xD4])?,
        )?;

        // ---- pattern pointer tables --------------------------------------
        // The pattern-fetch routine, reached by `JMP`/fallthrough into a `TAY`:
        //   TAY : LDA ptl,Y : STA zp : LDA pth,Y : STA zp+1
        // The two stores must hit consecutive zero-page bytes — that pairing is
        // what tells the low table from the high one.
        let p = scan(img, 11, |w| {
            w[0] == 0xA8
                && w[1] == 0xB9
                && w[4] == 0x85
                && w[6] == 0xB9
                && w[9] == 0x85
                && w[10] == w[5].wrapping_add(1)
        })?;
        let patt_ptl = operand(img, p + 1)?;
        let patt_pth = operand(img, p + 6)?;
        let stream_zp = img[p + 5];

        // ---- note-stream payload encoding ---------------------------------
        // The payload read is the first `INY : LDA (zp),Y` in the image; what
        // follows it says how wide the payload is (see [`StreamLayout`]).
        let q = scan(img, 3, |w| w == [0xC8, 0xB1, stream_zp])?;
        let stream = match *img.get(q + 3)? {
            // BPL over the portamento store. Two payload bytes iff the store is
            // followed by a second `INY : LDA (zp),Y`.
            0x10 if img.get(q + 8..q + 11) == Some(&[0xC8, 0xB1, stream_zp]) => {
                StreamLayout::PortaWord
            }
            0x10 => StreamLayout::PortaByte,
            // No branch: the payload is masked (`AND #$1F`) or stored outright.
            0x29 | 0x95 | 0x9D => StreamLayout::InstrumentOnly,
            _ => return None,
        };

        // ---- sub-song table ----------------------------------------------
        // Two idioms, both in the init routine, both giving the same address.
        //
        //  (a) copy the pointers into work RAM:
        //        LDA list,X : STA ram,Y : INX : INY : CPY #n : BNE
        //      `n` counts low AND high bytes, so the voice count is `n/2`.
        //
        //  (b) install them directly:
        //        LDA list,X : STA zp : LDA list+t,X : STA zp+1
        //      the second table is `t` bytes on, so `t` IS the voice count.
        let (song_list, song_track_qty) = if let Some(k) = scan(img, 10, |w| {
            w[0] == 0xBD
                && w[3] == 0x99
                && w[6..9] == [0xE8, 0xC8, 0xC0]
                && w[9] % 2 == 0
                && (2..=8).contains(&w[9])
        }) {
            (operand(img, k)?, img[k + 9] as usize / 2)
        } else {
            let k = scan(img, 10, |w| {
                w[0] == 0xBD
                    && w[3] == 0x85
                    && w[5] == 0xBD
                    && w[8] == 0x85
                    && w[9] == w[4].wrapping_add(1)
            })?;
            let lo = operand(img, k)?;
            let hi = operand(img, k + 5)?;
            let t = hi.checked_sub(lo)?;
            if !(2..=8).contains(&t) {
                return None;
            }
            (lo, t as usize)
        };

        // ---- fx tables ----------------------------------------------------
        // v10/v15: `LDA fx_v1+1,Y : STA $D400,X` — the waveform-alternation
        // table's second byte drives the frequency low byte.
        let fx_v1 = scan(img, 6, |w| w[0] == 0xB9 && w[3..6] == [0x9D, 0x00, 0xD4])
            .and_then(|k| operand(img, k))
            .and_then(|a| a.checked_sub(1))
            .unwrap_or(0);

        // v20+: `LDA fx_v2+6,Y : STA $D417` — the resonance/filter-select byte.
        let fx_v2 = scan(img, 6, |w| w[0] == 0xB9 && w[3..6] == [0x8D, 0x17, 0xD4])
            .and_then(|k| operand(img, k))
            .and_then(|a| a.checked_sub(6))
            .unwrap_or(0);

        // ---- row cadence ---------------------------------------------------
        // The gate is `LDA ctr : CMP reload : BNE skip` — the note stream steps
        // only on the frame the down-counter reaches its reload, so a row lasts
        // `reload + 1` frames. Requiring the `BNE` keeps this off the reload
        // block a few bytes earlier, which touches the same two addresses.
        let (tempo_ctr, tempo_reload) =
            scan(img, 7, |w| w[0] == 0xAD && w[3] == 0xCD && w[6] == 0xD0)
                .and_then(|k| Some((operand(img, k)?, operand(img, k + 3)?)))
                .unwrap_or((0, 0));

        // The reload byte lives in the image, so the cadence can just be read.
        let resetspd = (tempo_reload != 0)
            .then(|| {
                let off = super::PSID_DATA + tempo_reload.checked_sub(load)? as usize;
                data.get(off).copied()
            })
            .flatten()
            .unwrap_or(0);

        // Tunes whose sub-songs run at different speeds set the reload from a
        // table in the init routine: `LDA table,X : STA reload`.
        let want = [0x8D, tempo_reload as u8, (tempo_reload >> 8) as u8];
        let resetspd_table = (tempo_reload != 0)
            .then(|| scan(img, 6, |w| w[0] == 0xBD && w[3..6] == want))
            .flatten()
            .and_then(|k| operand(img, k))
            .unwrap_or(0);

        // The fractional-tempo counter: the same countdown-and-reload block, but
        // ending in a `JMP` past the row counter rather than an `RTS` out of the
        // whole frame.
        //
        // The `DEC` and the `STA` naming one address is not enough on its own —
        // Sanxion has an unrelated countdown of that shape and was read as a
        // 109/36 cadence. What pins it is the position: this block sits
        // *immediately* in front of the row counter it skips, so the byte after
        // its `JMP` must be the `DEC tempo_ctr` found above.
        let ctr = [0xCE, tempo_ctr as u8, (tempo_ctr >> 8) as u8];
        let ka = (tempo_ctr != 0)
            .then(|| {
                scan(img, 16, |w| {
                    w[0] == 0xCE
                        && w[3] == 0x10
                        && w[5] == 0xA9
                        && w[7] == 0x8D
                        && w[8..10] == w[1..3]
                        && w[10] == 0x4C
                        && w[13..16] == ctr
                })
            })
            .flatten();
        let tempo_ka = ka.map_or(0, |k| img[k + 6]);
        let tempo_ka_imm = ka.map_or(0, |k| load.wrapping_add(k as u16).wrapping_add(6));

        // Delta varies `Ka` per sub-song by patching that immediate from a table
        // in its init routine — a store into the middle of an instruction.
        let want_imm = [0x8D, tempo_ka_imm as u8, (tempo_ka_imm >> 8) as u8];
        let tempo_ka_table = (tempo_ka_imm != 0)
            .then(|| scan(img, 6, |w| w[0] == 0xBD && w[3..6] == want_imm))
            .flatten()
            .and_then(|k| operand(img, k))
            .unwrap_or(0);

        // ---- filter cutoff seed --------------------------------------------
        // The per-voice cutoff accumulator is whatever the sweep stores just
        // before it writes $D416; the note-on seeds that same address with an
        // immediate.
        let filter_seed = scan(img, 6, |w| w[0] == 0x9D && w[3..6] == [0x8D, 0x16, 0xD4])
            .and_then(|k| operand(img, k))
            .and_then(|acc| {
                let want = [0x9D, acc as u8, (acc >> 8) as u8];
                scan(img, 5, |w| w[0] == 0xA9 && w[2..5] == want).map(|k| img[k + 1])
            })
            .unwrap_or(0);

        // ---- three-note arpeggio table --------------------------------------
        // `LDA fx_v2+0,Y : ASL : TAY : LDA arp3,Y` — the instrument's fx byte 0
        // doubled into an index of signed semitone pairs.
        let want_fx2 = [0xB9, fx_v2 as u8, (fx_v2 >> 8) as u8];
        let arp3_table = (fx_v2 != 0)
            .then(|| {
                scan(img, 9, |w| {
                    w[0..3] == want_fx2 && w[3..6] == [0x0A, 0xA8, 0xB9]
                })
                .and_then(|k| operand(img, k + 5))
            })
            .flatten()
            .unwrap_or(0);

        // ---- run-length compressed order lists ------------------------------
        // The repeat counter and the order index each appear more than once, and
        // requiring those two addresses to be consistent is what makes seventeen
        // bytes of otherwise ordinary 6502 unmistakable.
        let order_compressed = scan(img, 21, |w| {
            w[0] == 0xDE                    // DEC rep,X
                && w[3] == 0xD0             // BNE .keep
                && w[5] == 0xFE             // INC idx,X
                && w[8] == 0xBC             // LDY idx,X
                && w[11] == 0xB1            // LDA (zp),Y
                && w[13] == 0x30            // BMI .keep
                && w[15] == 0x9D            // STA rep,X
                && w[18] == 0xFE            // INC idx,X
                && w[16..18] == w[1..3]     // …the same repeat counter
                && w[9..11] == w[6..8]      // …the same order index, twice over
                && w[19..21] == w[6..8]
        })
        .is_some();

        // ---- skydive (fx-mask bit 1) ---------------------------------------
        // Anchor on the bit-1 test, then read the two things that follow it: the
        // length gate, and how the frequency high byte is walked. Anchoring
        // matters — the bit-0 drum decrements the same byte, and picking that
        // block up instead would report a downward skydive on every drum tune.
        //
        // Two things pin it. The anchor must be a load of the *fx mask* — the
        // byte the engine tests with several different single-bit masks, not any
        // `AND #$02` — and the increment run must be the one whose result is
        // written to `$D401`, the frequency high byte. Without the second
        // constraint the scan picks up whatever `INC` happens to be nearby and
        // reports a skydive on tunes that have none.
        let (mut skydive_when, mut skydive_add) = (0u8, 0i16);
        // The fx mask lives in absolute memory on some tunes and in zero page on
        // others (Commando `LDA $5523`, Spellbound `LDA $be`), so both loads have
        // to be recognised — miss one and half the family silently reports no
        // effects at all.
        let mask_load = |k: usize| -> Option<(&[u8], usize)> {
            match img.get(k)? {
                0xAD => Some((img.get(k + 1..k + 3)?, 3)),
                0xA5 => Some((img.get(k + 1..k + 2)?, 2)),
                _ => None,
            }
        };
        // How many distinct single bits a candidate is tested against. The fx
        // mask is the byte the engine picks apart bit by bit; two or more
        // separates it from every incidental `AND #$02`.
        let bits_tested = |addr: &[u8]| -> usize {
            [0x01u8, 0x02, 0x04, 0x08, 0x10, 0x20]
                .iter()
                .filter(|&&bit| {
                    (0..img.len().saturating_sub(5)).any(|j| {
                        mask_load(j).is_some_and(|(a, l)| {
                            a == addr && img.get(j + l..j + l + 2) == Some(&[0x29, bit])
                        })
                    })
                })
                .count()
        };
        // The mask's address, and a way back to any one bit's block.
        let mask_addr = (0..img.len().saturating_sub(5))
            .filter_map(|k| mask_load(k).map(|(a, l)| (k, a, l)))
            .find(|&(k, a, l)| {
                img.get(k + l..k + l + 2).is_some_and(|w| w[0] == 0x29) && bits_tested(a) >= 2
            })
            .map(|(_, a, _)| a);
        // Where the engine handles fx-mask bit `bit`, as the bytes that follow.
        let fx_block = |bit: u8| -> Option<&[u8]> {
            let addr = mask_addr?;
            let k = (0..img.len().saturating_sub(5)).find(|&j| {
                mask_load(j).is_some_and(|(a, l)| {
                    a == addr && img.get(j + l..j + l + 2) == Some(&[0x29, bit])
                })
            })?;
            Some(&img[k..(k + 48).min(img.len())])
        };
        let fxmask = (0..img.len().saturating_sub(5)).find(|&k| {
            mask_load(k).is_some_and(|(a, l)| {
                Some(a) == mask_addr && img.get(k + l..k + l + 2) == Some(&[0x29, 0x02])
            })
        });
        if let Some(k) = fxmask {
            let block = &img[k..(k + 40).min(img.len())];
            // `AND #$1F : CMP #n : BCC` — the note must last longer than `n-1`.
            if let Some(g) = block
                .windows(4)
                .position(|w| w[0] == 0x29 && w[1] == 0x1F && w[2] == 0xC9)
            {
                skydive_when = block[g + 3].saturating_sub(1);
            }
            // A run of `INC addr,X` or `DEC addr,X` on one address, each step
            // moving the high byte by one, i.e. the register by 256.
            for (op, sign) in [(0xFEu8, 1i16), (0xDE, -1)] {
                let mut start = 0;
                while let Some(first) = block[start..]
                    .windows(3)
                    .position(|w| w[0] == op)
                    .map(|p| p + start)
                    .filter(|&p| p + 3 <= block.len())
                {
                    let target = &block[first + 1..first + 3];
                    let mut steps = 0i16;
                    let mut i = first;
                    while i + 3 <= block.len() && block[i] == op && &block[i + 1..i + 3] == target {
                        steps += 1;
                        i += 3;
                    }
                    // Only a run that feeds the frequency high byte is a
                    // skydive; anything else in this block is another effect.
                    let writes_d401 = block[i..(i + 9).min(block.len())]
                        .windows(3)
                        .any(|w| matches!(w[0], 0x8D | 0x99 | 0x9D) && w[1..3] == [0x01, 0xD4]);
                    if writes_d401 {
                        skydive_add = sign * steps * 256;
                        break;
                    }
                    start = i;
                }
                if skydive_add != 0 {
                    break;
                }
            }
        }

        // ---- period-interpolation vibrato -----------------------------------
        // Same anchor as the skydive, different question: does any effect here
        // build a whole 16-bit frequency (both chip registers) rather than nudge
        // one byte of it?
        let interp_vibrato = fxmask.is_some_and(|k| {
            let block = &img[k..(k + 48).min(img.len())];
            let writes = |reg: u8| {
                block
                    .windows(3)
                    .any(|w| matches!(w[0], 0x8D | 0x99 | 0x9D) && w[1] == reg && w[2] == 0xD4)
            };
            writes(0x00) && writes(0x01)
        });

        // ---- fx-mask bit 2: which way the arpeggio jumps --------------------
        let arp_semitones = fx_block(0x04).map_or(0i8, |b| {
            b.windows(2)
                .find_map(|w| match w {
                    [0x69, n] => Some(*n as i8),    // CLC : ADC #n  — jump up
                    [0xE9, n] => Some(-(*n as i8)), // SEC : SBC #n — drop down
                    _ => None,
                })
                .unwrap_or(0)
        });

        // ---- sizes ---------------------------------------------------------
        // The two pattern-pointer halves are adjacent, so their gap IS the
        // pattern count; likewise the sub-song table runs up to the pointer
        // table.
        let table_qty = patt_pth.checked_sub(patt_ptl)? as usize;
        let song_list_qty = patt_ptl.checked_sub(song_list)? as usize / (2 * song_track_qty);

        let mut out = Self {
            load,
            song_track_qty,
            song_list,
            song_list_qty,
            patt_ptl,
            patt_pth,
            patt_qty: table_qty,
            instr,
            instr_qty: 0,
            fx_v1,
            fx_v2,
            stream,
            stream_zp,
            frame_skip_reset,
            tempo_ctr,
            tempo_reload,
            resetspd,
            resetspd_table,
            tempo_ka,
            tempo_ka_imm,
            tempo_ka_table,
            filter_seed,
            arp3_table,
            order_compressed,
            skydive_when,
            skydive_add,
            interp_vibrato,

            arp_semitones,
        };
        out.patt_qty = match out.pattern_count(data) {
            0 => table_qty,
            n => n.min(table_qty),
        };
        out.instr_qty = out.instrument_count(data)?;
        let _ = table_qty;
        Some(out)
    }

    /// As [`Self::detect`], but naming the consistency rule that turned the
    /// image down. `Err` with `None` inside means a signature was missing
    /// outright rather than a rule failing.
    fn detect_reason(data: &[u8]) -> Result<Self, Option<&'static str>> {
        // Re-run detection, then re-check: cheap, and keeps one copy of the
        // rules rather than a diagnostic that can drift from the real thing.
        let Some(out) = Self::detect_unchecked(data) else {
            return Err(None);
        };
        let table_qty = out.patt_pth.wrapping_sub(out.patt_ptl) as usize;
        out.is_consistent(data, table_qty)
            .map(|()| out)
            .map_err(Some)
    }

    /// Whether the recovered layout hangs together as a tune of this family.
    ///
    /// Every signature above is a short byte sequence, and a long enough image
    /// from a *different* player will eventually contain one by accident — the
    /// last generation's does. What it will not do is produce four addresses
    /// that sit in the right order, inside the image, spaced by a whole number
    /// of table entries, pointing at note streams that all terminate. Requiring
    /// the whole picture is what makes a wrong family fail here instead of
    /// importing as noise.
    fn is_consistent(&self, data: &[u8], table_qty: usize) -> Result<(), &'static str> {
        let inside = |a: u16| self.at(data, a).is_some();
        if !(self.song_list < self.patt_ptl && self.patt_ptl < self.patt_pth) {
            return Err("the three tables are not in ascending order");
        }
        if ![self.song_list, self.patt_ptl, self.patt_pth, self.instr]
            .into_iter()
            .all(inside)
        {
            return Err("a table address falls outside the image");
        }
        // A pattern is selected by one byte of an order list, so there can be at
        // most 256 of them; likewise the sub-song table must hold a whole number
        // of entries, and at least one.
        if !(1..=256).contains(&table_qty) {
            return Err("the pointer table cannot hold that many patterns");
        }
        if !((self.patt_ptl - self.song_list) as usize).is_multiple_of(2 * self.song_track_qty) {
            return Err("the sub-song table is not a whole number of entries");
        }
        if self.song_list_qty == 0 || self.patt_qty == 0 {
            return Err("no sub-songs or no patterns");
        }
        // Every pattern the order lists reach must be a note stream that ends on
        // its sentinel.
        if self.walk_patterns(data, self.porta_bytes()).0 != self.patt_qty {
            return Err("a pattern's note stream never reaches its sentinel");
        }
        Ok(())
    }

    /// How many patterns the sub-song order lists actually reach, or 0 if they
    /// reach none and the caller should fall back to the table extent.
    ///
    /// The gap between the two halves of the pointer table is only an upper
    /// bound — it can carry a byte or two of padding, and it counts patterns
    /// belonging to sub-songs the tune never plays. An order list is a run of
    /// pattern indices closed by any byte with bit 7 set, so the highest index
    /// reached over every sub-song and voice is the real count.
    ///
    /// Some tunes open an order list on a byte that already has bit 7 set, so
    /// the walk reads no index at all. That is a silent way to end up claiming a
    /// twenty-instrument game score has one pattern, so it reports "nothing
    /// read" rather than "one pattern".
    fn pattern_count(&self, data: &[u8]) -> usize {
        let Some(list) = self.at(data, self.song_list) else {
            return 0;
        };
        let mut read_any = false;
        let mut highest = 0usize;
        for s in 0..self.song_list_qty {
            for v in 0..self.song_track_qty {
                let base = list + s * 2 * self.song_track_qty + v;
                let (Some(&lo), Some(&hi)) = (data.get(base), data.get(base + self.song_track_qty))
                else {
                    continue;
                };
                let Some(mut j) = self.at(data, u16::from_le_bytes([lo, hi])) else {
                    continue;
                };
                while let Some(&b) = data.get(j) {
                    if b & 0x80 != 0 {
                        break;
                    }
                    highest = highest.max(b as usize);
                    read_any = true;
                    j += 1;
                }
            }
        }
        if read_any {
            highest + 1
        } else {
            0
        }
    }

    /// Why [`Self::detect`] turned an image down, as the first thing it could
    /// not find. `None` when the image IS recognised.
    ///
    /// Diagnostic: it says whether a rejected file is a different player
    /// generation (no instrument table, no note fetch) or one of this family
    /// that detection is failing to read (everything present, consistency
    /// refused). Those want very different follow-up.
    pub fn why_not(data: &[u8]) -> Option<&'static str> {
        if Self::detect(data).is_some() {
            return None;
        }
        let Some(img) = data.get(super::PSID_DATA..) else {
            return Some("shorter than a PSID header");
        };
        let has = |window: usize, pred: &dyn Fn(&[u8]) -> bool| scan(img, window, pred).is_some();

        if !has(6, &|w: &[u8]| w[0] == 0xBD && w[3..6] == [0x99, 0x02, 0xD4]) {
            return Some("no instrument table (LDA instr,X : STA $D402,Y)");
        }
        if !has(11, &|w: &[u8]| {
            w[0] == 0xA8
                && w[1] == 0xB9
                && w[4] == 0x85
                && w[6] == 0xB9
                && w[9] == 0x85
                && w[10] == w[5].wrapping_add(1)
        }) {
            return Some("no pattern-pointer fetch (TAY : LDA ptl,Y : STA zp : …)");
        }
        if !has(10, &|w: &[u8]| {
            (w[0] == 0xBD && w[3] == 0x99 && w[6..9] == [0xE8, 0xC8, 0xC0])
                || (w[0] == 0xBD && w[3] == 0x85 && w[5] == 0xBD && w[8] == 0x85)
        }) {
            return Some("no sub-song pointer setup");
        }
        match Self::detect_reason(data) {
            Ok(_) => None,
            Err(Some(rule)) => Some(rule),
            Err(None) => Some("a signature is missing"),
        }
    }

    /// Byte offset of an engine address inside the `.sid` file, if in range.
    fn at(&self, data: &[u8], addr: u16) -> Option<usize> {
        let off = super::PSID_DATA.checked_add(addr.checked_sub(self.load)? as usize)?;
        (off < data.len()).then_some(off)
    }

    /// How many instrument records the tune actually uses.
    ///
    /// The fx table sits immediately after the instrument table whenever the
    /// tune has one, which settles the count exactly (records are eight bytes)
    /// and needs no decoding. Otherwise the patterns are walked and the highest
    /// instrument index they select decides.
    /// Average frames per row, as a reduced fraction, for the given cadence
    /// pair — or `None` when the tune has no fractional device and a row is
    /// simply `r + 1` frames.
    ///
    /// One row costs `r + 1` frames, except that one frame in every `ka + 1` is
    /// taken away from the row counter, so the average is
    /// `(r + 1) · (ka + 1) / ka`. The same closed form covers both spellings of
    /// the trick — the whole-frame skip and the row-counter skip — which is why
    /// there is one function and not two.
    ///
    /// It reproduces all five transcribed `tempo_frac` values: Ace 2 `(1,3) →
    /// 8/3`, Delta `(1,4) → 5/2`, Lightforce `(2,6) → 7/2`, International Karate
    /// `(2,10) → 33/10`, Spellbound `(1,10) → 11/5`.
    pub fn tempo_frac(r: u32, ka: u32) -> Option<(u32, u32)> {
        if ka == 0 {
            return None;
        }
        let (mut num, mut den) = ((r + 1) * (ka + 1), ka);
        let (mut a, mut b) = (num, den);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        num /= a;
        den /= a;
        Some((num, den))
    }

    /// The replayer generation, as far as the image can tell.
    ///
    /// Three signals, in the order they can be trusted:
    ///
    /// - **v30** — the order lists are run-length compressed. Unambiguous, and
    ///   the one that must not be missed.
    /// - **v15** — the bit-2 arpeggio drops a fourth *and* the note stream has
    ///   no portamento branch. Neither alone is enough: International Karate
    ///   also drops a fourth but is a v20 tune with the `Simple` fx layout, and
    ///   Thing on a Spring also has no portamento branch but is v10.
    /// - otherwise the payload width decides, v10 against v20.
    ///
    /// v25 is deliberately not attempted: the only thing it changes is a filter
    /// flag that measures as inert on the whole corpus, so `20` covers it.
    pub fn version(&self) -> usize {
        if self.order_compressed {
            30
        } else if self.arp_semitones == -5 && self.stream == StreamLayout::InstrumentOnly {
            15
        } else if self.stream == StreamLayout::PortaWord {
            20
        } else {
            10
        }
    }

    /// Width of the note stream's portamento payload, in bytes.
    pub fn porta_bytes(&self) -> usize {
        match self.stream {
            StreamLayout::PortaWord => 2,
            _ => 1,
        }
    }

    /// How many instrument records the tune actually uses.
    ///
    /// Only `fx_v2` is parallel to the instrument table (same eight-byte stride,
    /// same index), so it — and never the sound-effect table, which is neither —
    /// settles the count outright when the tune has one.
    ///
    /// Otherwise the patterns are walked and the highest instrument index they
    /// select decides, capped at what the space between the instrument table and
    /// the next table can physically hold: an index past the end of the table is
    /// a walk that lost sync, not evidence of an instrument.
    fn instrument_count(&self, data: &[u8]) -> Option<usize> {
        if self.fx_v2 > self.instr {
            let span = (self.fx_v2 - self.instr) as usize;
            if span.is_multiple_of(8) {
                return Some(span / 8);
            }
        }
        let ceiling = [self.fx_v1, self.fx_v2, self.song_list, self.patt_ptl]
            .into_iter()
            .filter(|&a| a > self.instr)
            .min()
            .map_or(usize::MAX, |a| (a - self.instr) as usize / 8);
        Some((self.walk_patterns(data, self.porta_bytes()).1 + 1).min(ceiling))
    }

    /// `(clean, highest_instrument)` for a one- and a two-byte portamento
    /// payload. Diagnostic for `examples/sid_xref_probe`.
    pub fn walk_stats(&self, data: &[u8]) -> [(usize, usize); 2] {
        [self.walk_patterns(data, 1), self.walk_patterns(data, 2)]
    }

    /// How often each instrument index is selected across all patterns.
    /// Diagnostic: an index used once at the very top of the range is usually a
    /// walk that slipped, one used dozens of times is a real instrument.
    pub fn instrument_histogram(&self, data: &[u8]) -> [u32; 128] {
        let mut hist = [0u32; 128];
        self.walk_inner(data, self.porta_bytes(), &mut |sel| hist[sel] += 1);
        hist
    }

    /// Walk every pattern's note stream, returning how many ended on their
    /// sentinel and the highest instrument index selected along the way.
    ///
    /// One entry is a header byte, then — when bit 6 is clear, meaning the entry
    /// carries a note — an optional payload (present when bit 7 is set: an
    /// instrument select if its top bit is clear, else a portamento of
    /// `porta_bytes`) and the note byte itself.
    fn walk_patterns(&self, data: &[u8], porta_bytes: usize) -> (usize, usize) {
        let mut highest = 0usize;
        let clean = self.walk_inner(data, porta_bytes, &mut |sel| highest = highest.max(sel));
        (clean, highest)
    }

    /// The walk itself: calls `on_instrument` for every instrument select and
    /// returns how many patterns ended on their `0xFF` sentinel.
    fn walk_inner(
        &self,
        data: &[u8],
        porta_bytes: usize,
        on_instrument: &mut dyn FnMut(usize),
    ) -> usize {
        let (Some(ptl), Some(pth)) = (self.at(data, self.patt_ptl), self.at(data, self.patt_pth))
        else {
            return 0;
        };
        let mut clean = 0usize;
        for i in 0..self.patt_qty {
            let (Some(&lo), Some(&hi)) = (data.get(ptl + i), data.get(pth + i)) else {
                continue;
            };
            let Some(start) = self.at(data, u16::from_le_bytes([lo, hi])) else {
                continue;
            };
            let mut j = start;
            while let Some(&head) = data.get(j) {
                if head == 0xFF {
                    clean += 1;
                    break;
                }
                j += 1;
                if head & 0b0100_0000 != 0 {
                    continue; // a bare release entry: no payload, no note
                }
                if head & 0b1000_0000 != 0 {
                    let Some(&sel) = data.get(j) else { break };
                    if sel & 0b1000_0000 == 0 || self.stream == StreamLayout::InstrumentOnly {
                        on_instrument((sel & 0b0111_1111) as usize);
                        j += 1;
                    } else {
                        j += porta_bytes;
                    }
                }
                j += 1; // the note byte
            }
        }
        clean
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fourteen distinct images behind the fifteen hand-written `OneSid`
    /// records, with the layout each record claims:
    /// `(file, load, song_list, slq, trk, ptl, pth, pq, instr, iq, fx_v1, fx_v2)`.
    ///
    /// Sanxion is deliberately listed once. Its image holds two independent
    /// banks (the second at `$C5F5`) which the hand records split into
    /// `sanxion1`/`sanxion2`; one detection pass over one image can only find
    /// the first, so the second bank is a known limit, not a failure.
    #[allow(clippy::type_complexity)]
    const KNOWN: &[(&str, &[u8], [u16; 4], [usize; 4], [u16; 2])] = &[
        (
            "commando",
            include_bytes!("songs/commando.sid"),
            [0x5000, 0x56FF, 0x5711, 0x573E],
            [3, 3, 45, 13],
            [0x55F9, 0],
        ),
        (
            "crazy_comets",
            include_bytes!("songs/crazy_comets.sid"),
            [0x5000, 0x5732, 0x573E, 0x5773],
            [2, 3, 53, 23],
            [0x562C, 0],
        ),
        (
            "last_v8",
            include_bytes!("songs/last_v8.sid"),
            [0x8010, 0x8797, 0x87A9, 0x87C6],
            [3, 3, 28, 19],
            [0x8699, 0],
        ),
        (
            "monty",
            include_bytes!("songs/monty_on_the_run.sid"),
            [0x8000, 0x856C, 0x857E, 0x85CB],
            [3, 3, 77, 20],
            [0x9454, 0],
        ),
        (
            "thing",
            include_bytes!("songs/thing_on_a_spring.sid"),
            [0xC000, 0xC509, 0xC50F, 0xC533],
            [1, 3, 36, 15],
            [0xCDA2, 0],
        ),
        (
            "zoids",
            include_bytes!("songs/zoids.sid"),
            [0x1000, 0x14FC, 0x150E, 0x152D],
            [3, 3, 31, 13],
            [0, 0],
        ),
        (
            "thrust",
            include_bytes!("songs/thrust.sid"),
            [0x0800, 0x0DD7, 0x0DDD, 0x0E04],
            [1, 3, 39, 28],
            [0, 0],
        ),
        (
            "spellbound",
            include_bytes!("songs/spellbound.sid"),
            [0xE000, 0xE6B6, 0xE6C8, 0xE6F2],
            [3, 3, 42, 25],
            [0xE610, 0],
        ),
        (
            "ace_2",
            include_bytes!("songs/ace_2.sid"),
            [0xE000, 0xE67C, 0xE682, 0xE6A8],
            [1, 3, 38, 10],
            [0, 0xE624],
        ),
        (
            "human_race",
            include_bytes!("songs/human_race.sid"),
            [0x0980, 0x0E9F, 0x0EB3, 0x0F02],
            [5, 2, 79, 23],
            [0, 0],
        ),
        (
            "int_karate",
            include_bytes!("songs/international_karate.sid"),
            [0xAE00, 0xB3B0, 0xB3B6, 0xB3EB],
            [1, 3, 53, 20],
            [0, 0],
        ),
        (
            "sanxion",
            include_bytes!("songs/sanxion.sid"),
            [0xB000, 0xB73C, 0xB742, 0xB75D],
            [1, 3, 27, 29],
            [0, 0xB654],
        ),
        (
            "lightforce",
            include_bytes!("songs/lightforce.sid"),
            [0xF000, 0xF778, 0xF77E, 0xF79D],
            [1, 3, 31, 22],
            [0, 0xF6C8],
        ),
        (
            "delta",
            include_bytes!("songs/delta.sid"),
            [0xBC00, 0xC4F4, 0xC542, 0xC5AF],
            [13, 3, 109, 22],
            [0, 0xC43E],
        ),
    ];

    /// Detection reproduces the hand-transcribed layout of every bundled tune.
    ///
    /// The four places where this table departs from the `OneSid` records are
    /// each a case of the binary contradicting the transcription, and are called
    /// out in `disagreements_with_the_hand_written_records` below.
    #[test]
    fn recovers_every_known_layout() {
        for (name, data, addrs, sizes, fx) in KNOWN {
            let d = SidLayout::detect(data).unwrap_or_else(|| panic!("{name}: not detected"));
            let got = (
                [d.load, d.song_list, d.patt_ptl, d.patt_pth],
                [d.song_list_qty, d.song_track_qty, d.patt_qty, d.instr_qty],
                [d.fx_v1, d.fx_v2],
            );
            assert_eq!(got, (*addrs, *sizes, *fx), "{name}");
        }
    }

    /// The instrument table is found by the same instruction in every tune:
    /// `LDA instr+0,X : STA $D402,Y`, seeding the pulse width from the record.
    #[test]
    fn instrument_tables() {
        let want = [
            0x5591, 0x5574, 0x85A1, 0x93B4, 0xCD2A, 0x147E, 0x0CF7, 0xE548, 0xE5CB, 0x0DE3, 0xB308,
            0xB56C, 0xF618, 0xC38E,
        ];
        for ((name, data, ..), want) in KNOWN.iter().zip(want) {
            assert_eq!(SidLayout::detect(data).unwrap().instr, want, "{name}");
        }
    }

    /// The note stream's payload width comes from the note-fetch routine, and
    /// splits the corpus differently from the hand-written `version` field:
    /// Thrust is recorded as v10 yet reads a two-byte portamento, and The Human
    /// Race is recorded as v20 yet reads a one-byte one.
    ///
    /// Neither is audible: no pattern either tune's sub-songs reach takes the
    /// portamento branch at all, so both widths decode to the same rows. Proved
    /// by `sid_detected_layout_vs_hand_written` in the player's oracle gate —
    /// do not re-open it as a fidelity bug.
    #[test]
    fn stream_layouts() {
        use StreamLayout::*;
        let want = [
            PortaByte,
            PortaByte,
            PortaByte,
            PortaByte,
            InstrumentOnly,
            PortaByte,
            PortaWord,
            InstrumentOnly,
            PortaWord,
            PortaByte,
            PortaWord,
            PortaWord,
            PortaWord,
            PortaWord,
        ];
        for ((name, data, ..), want) in KNOWN.iter().zip(want) {
            assert_eq!(SidLayout::detect(data).unwrap().stream, want, "{name}");
        }
    }

    /// Only three tunes drop a frame periodically, and the reload value matches
    /// what each record's comment says was read out of the binary by hand.
    #[test]
    fn frame_skips() {
        let want = [("thrust", 9u8), ("spellbound", 10), ("int_karate", 10)];
        for (name, data, ..) in KNOWN {
            let got = SidLayout::detect(data).unwrap().frame_skip_reset;
            let expect = want.iter().find(|(n, _)| n == name).map_or(0, |(_, v)| *v);
            assert_eq!(got, expect, "{name}");
        }
    }

    /// The row cadence comes out of the image on every tune.
    ///
    /// `resetspd` is a data byte — the reload of the counter the replayer gates
    /// note-fetch on — so it never needed transcribing at all. Fifteen for
    /// fifteen against the hand-written values.
    #[test]
    fn row_cadence() {
        let want = [2u8, 2, 1, 1, 1, 2, 2, 1, 1, 3, 2, 2, 2, 1];
        for ((name, data, ..), want) in KNOWN.iter().zip(want) {
            let d = SidLayout::detect(data).unwrap();
            assert_eq!(d.resetspd, want, "{name}");
            assert_ne!(d.tempo_reload, 0, "{name}: no cadence gate found");
        }
    }

    /// Tunes whose sub-songs run at different speeds set the counter's reload
    /// from a table, and that table is readable too.
    ///
    /// Commando's reads `[2, 3, 2]` — byte for byte the `resetspd_songs` someone
    /// established from the init routine and the oracle's note period. The Human
    /// Race's `[3, 3, 2, 3, 1]` was NOT transcribed, and applying it takes its
    /// third sub-song's gate divergence from 465 to 0 against the chip.
    #[test]
    fn per_sub_song_cadence() {
        let table = |i: usize| -> Vec<u8> {
            let (_, data, addrs, sizes, _) = KNOWN[i];
            let d = SidLayout::detect(data).unwrap();
            let base = super::super::PSID_DATA + (d.resetspd_table - addrs[0]) as usize;
            (0..sizes[0]).map(|s| data[base + s]).collect()
        };
        assert_eq!(table(0), vec![2, 3, 2], "commando");
        assert_eq!(table(9), vec![3, 3, 2, 3, 1], "human_race");
        // Delta's is the `R` half of its dual-counter fractional tempo, at the
        // address `tempo_frac_for` reaches for by hand.
        assert_eq!(
            SidLayout::detect(KNOWN[13].1).unwrap().resetspd_table,
            0xC311
        );
    }

    /// The fractional cadence comes out of the image too, and one closed form
    /// covers both spellings of the trick.
    ///
    /// All five transcribed `tempo_frac` values fall out of `(R+1)(Ka+1)/Ka`,
    /// with `Ka` read either from the dual counter's `LDA #Ka` or — for the
    /// tunes that spell it as a whole-frame skip — from the skip reload.
    #[test]
    fn fractional_cadence() {
        // (tune index, expected fraction). Thrust's record deliberately keeps
        // `None` and scales bpm instead, so it is not asserted here.
        for (i, want) in [
            (7usize, (11u32, 5u32)), // spellbound, via the frame skip
            (8, (8, 3)),             // ace_2, dual counter
            (10, (33, 10)),          // int_karate, via the frame skip
            (12, (7, 2)),            // lightforce, dual counter
            (13, (5, 2)),            // delta, dual counter
        ] {
            let d = SidLayout::detect(KNOWN[i].1).unwrap();
            let ka = if d.tempo_ka != 0 {
                d.tempo_ka
            } else {
                d.frame_skip_reset
            };
            assert_eq!(
                SidLayout::tempo_frac(d.resetspd as u32, ka as u32),
                Some(want),
                "{}",
                KNOWN[i].0
            );
        }

        // Sanxion has the dual counter too — `LDA #$6c` at `$b055`, the same
        // eleven bytes as Lightforce — which nobody had transcribed. Applying it
        // took its first sub-song's gate divergence from 426 to 165.
        let s = SidLayout::detect(KNOWN[11].1).unwrap();
        assert_eq!(s.tempo_ka, 108);
        assert_eq!(
            SidLayout::tempo_frac(s.resetspd as u32, 108),
            Some((109, 36))
        );

        // Delta is the one that varies `Ka` per sub-song, by patching the
        // immediate from a table — the address `tempo_frac_for` used to hardcode.
        assert_eq!(
            SidLayout::detect(KNOWN[13].1).unwrap().tempo_ka_table,
            0xC31E
        );
    }

    /// The filter's note-on cutoff seed, and the three-note arpeggio table.
    #[test]
    fn filter_seed_and_arpeggio_table() {
        // Ace 2 seeds the accumulator with 48; every other tune starts at 0.
        for (i, (name, data, ..)) in KNOWN.iter().enumerate() {
            let want = if *name == "ace_2" { 48 } else { 0 };
            assert_eq!(
                SidLayout::detect(data).unwrap().filter_seed,
                want,
                "{name} (#{i})"
            );
        }
        // Lightforce's arpeggio table is the transcribed one. Ace 2 and Sanxion
        // carry the same block, but applying theirs was measured and rejected —
        // see the note at the bit-4 decode in `one_sid.rs`.
        assert_eq!(SidLayout::detect(KNOWN[12].1).unwrap().arp3_table, 0xF60C);
        assert_eq!(SidLayout::detect(KNOWN[8].1).unwrap().arp3_table, 0xE597);
        assert_eq!(SidLayout::detect(KNOWN[11].1).unwrap().arp3_table, 0xB562);
    }

    /// Run-length compressed order lists pick out the v30 replayer, and only it.
    ///
    /// This is the one version distinction worth detecting above all others:
    /// reading a compressed list flat plays the repeat counts as phrase indices,
    /// so the tune comes out as different music rather than merely mistimed.
    #[test]
    fn compressed_order_lists() {
        for (name, data, ..) in KNOWN {
            let got = SidLayout::detect(data).unwrap().order_compressed;
            assert_eq!(got, *name == "delta", "{name}");
        }
    }

    /// The period-interpolation vibrato is readable, and reading it beats
    /// guessing at the version.
    ///
    /// It was found while looking for something else: fx-mask bit 1 walks the
    /// frequency register on v10 and the note index on v15, so "does the effect
    /// block write both chip frequency registers?" looked like a v10/v15 test.
    /// It is not — it comes out true for Spellbound (v15) *and* Thrust (v10),
    /// which is exactly the pair the records flag `interp_vibrato`. Building a
    /// whole 16-bit frequency is what interpolating between two table periods
    /// requires, so the signal belongs to the vibrato, not to the version.
    #[test]
    fn period_interpolation_vibrato() {
        for (name, data, ..) in KNOWN {
            let got = SidLayout::detect(data).unwrap().interp_vibrato;
            let want = matches!(*name, "spellbound" | "thrust");
            assert_eq!(got, want, "{name}");
        }
    }

    /// The bit-2 arpeggio direction separates the v15/Simple family from v10 —
    /// the `fx_layout` distinction, which nothing else in the image gives.
    ///
    /// `-5` comes out for exactly Spellbound and International Karate, which are
    /// exactly the two records where `nibble_split` is true (one by `version ==
    /// 15`, the other by `fx_layout == Simple`). The plain v10 tunes all read
    /// `+12`, and the v20+ Rich family reads nothing at all.
    ///
    /// Only the `-5` marker is trusted. Thing on a Spring reads `+24` and Thrust
    /// `-15`; those blocks do something else and their numbers are not claimed
    /// to mean anything. What matters is that neither is `-5`, so neither is
    /// misfiled into the wrong family.
    #[test]
    fn arpeggio_direction_separates_the_simple_family() {
        for (name, data, ..) in KNOWN {
            let got = SidLayout::detect(data).unwrap().arp_semitones;
            let simple = matches!(*name, "spellbound" | "int_karate");
            assert_eq!(got == -5, simple, "{name} read {got}");
        }
        // The plain v10 family, for the record.
        for i in [0usize, 1, 2, 3, 5] {
            assert_eq!(
                SidLayout::detect(KNOWN[i].1).unwrap().arp_semitones,
                12,
                "{}",
                KNOWN[i].0
            );
        }
        // And the v20+ Rich family has no bit-2 arpeggio.
        for i in [8usize, 11, 12, 13] {
            assert_eq!(
                SidLayout::detect(KNOWN[i].1).unwrap().arp_semitones,
                0,
                "{}",
                KNOWN[i].0
            );
        }
    }

    /// The replayer generation, derived from the image, against what the records
    /// say.
    ///
    /// Twelve of fifteen agree outright. The three that do not are all cases
    /// already established elsewhere, not new failures:
    ///
    /// - Thrust reads 20 against a recorded 10, because its note fetch really
    ///   does take a two-byte portamento (`stream_layouts`). Inert either way —
    ///   no pattern it reaches uses one.
    /// - The Human Race reads 10 against a recorded 20, for the mirror reason:
    ///   its fetch takes a one-byte payload. Also inert.
    /// - Lightforce reads 20 against a recorded 25, and v25's only effect
    ///   measures as inert on the whole corpus.
    ///
    /// Note what carries the v15 call: the arpeggio direction AND the stream
    /// encoding together. Thing on a Spring shares Spellbound's `InstrumentOnly`
    /// stream and would be misfiled as v15 by the encoding alone.
    #[test]
    fn replayer_generation() {
        let recorded = [10, 10, 10, 10, 10, 10, 10, 15, 20, 20, 20, 20, 25, 30];
        let expected = [10, 10, 10, 10, 10, 10, 20, 15, 20, 10, 20, 20, 20, 30];
        for (((name, data, ..), rec), want) in KNOWN.iter().zip(recorded).zip(expected) {
            let got = SidLayout::detect(data).unwrap().version();
            assert_eq!(got, want, "{name}");
            if got != rec {
                assert!(
                    matches!(*name, "thrust" | "human_race" | "lightforce"),
                    "{name}: a NEW disagreement with the records ({got} vs {rec})"
                );
            }
        }
    }

    /// A recognised layout must survive being imported, not merely recognised.
    ///
    /// Offering arbitrary files to the player turned this from a theoretical
    /// concern into a crash: BMX Kidz carries an order list with no terminating
    /// byte, and the channel walk ran off the end of the image and aborted the
    /// process. The walk now stops at the end of the image, and this builds
    /// every bundled tune through the detected path to keep it that way.
    #[test]
    fn every_detected_layout_imports() {
        for (name, data, ..) in KNOWN {
            let sid = super::super::one_sid::OneSid::from_detected(*data)
                .unwrap_or_else(|| panic!("{name}: not detected"));
            let modules = sid.to_sidmodule().to_modules();
            assert!(!modules.is_empty(), "{name}: built no sub-song");
        }
    }

    /// The skydive is READ but NOT USED — pinned here so the state of the
    /// evidence is not lost.
    ///
    /// The fx-mask bit-1 block yields Commando's `2 / +512` exactly, and the
    /// four tunes that have no skydive at all report none. But the sign is not
    /// reliable: Crazy Comets reads `+256` where its record says `-256`, and
    /// applying the detected values moved `fhi` — the metric a pitch walk owns —
    /// the wrong way on both its sub-songs. `one_sid.rs` keeps the transcribed
    /// values; this test exists so a future attempt starts from the measurement
    /// rather than repeating it.
    #[test]
    fn skydive_is_read_but_not_trusted() {
        let commando = SidLayout::detect(KNOWN[0].1).unwrap();
        assert_eq!((commando.skydive_when, commando.skydive_add), (2, 512));

        // Crazy Comets: right length gate, wrong sign — the known failure.
        let crazy = SidLayout::detect(KNOWN[1].1).unwrap();
        assert_eq!(crazy.skydive_when, 16);
        assert_eq!(
            crazy.skydive_add, 256,
            "records -256; detection is not trusted here"
        );

        // The v20+ family has no bit-1 skydive, and detection agrees.
        for i in [8usize, 10, 11, 12, 13] {
            let d = SidLayout::detect(KNOWN[i].1).unwrap();
            assert_eq!((d.skydive_when, d.skydive_add), (0, 0), "{}", KNOWN[i].0);
        }
    }

    /// Where detection and the hand-written records disagree, and why the binary
    /// is taken to be right. Nothing here changes the records — they stay the
    /// importer's source of truth for these fifteen tunes — but a detected tune
    /// outside that list will follow these rules, so they are pinned.
    #[test]
    fn disagreements_with_the_hand_written_records() {
        // Thing on a Spring: the record says 45 instruments and a sound-effect
        // table at $CE92. No instruction anywhere in the image references
        // $CE92; the one that fills $D400 from a sound-effect record names
        // $CDA2, which is exactly 15 instrument records past the table. The
        // patterns select instruments 0..=14 and nothing above.
        let thing = SidLayout::detect(KNOWN[4].1).unwrap();
        assert_eq!((thing.fx_v1, thing.instr_qty), (0xCDA2, 15));
        assert_eq!(
            thing.instrument_histogram(KNOWN[4].1)[15..]
                .iter()
                .sum::<u32>(),
            0
        );

        // Thrust: the record stops the instrument table at 27 records, but the
        // patterns select index 27 (once), which that table cannot hold.
        let thrust = SidLayout::detect(KNOWN[6].1).unwrap();
        assert_eq!(thrust.instr_qty, 28);
        assert_eq!(thrust.instrument_histogram(KNOWN[6].1)[27], 1);

        // The Human Race: the record takes 3 sub-songs, the pointer table holds
        // 5 — as does the PSID header — and the two extra entries are ordinary
        // ascending pointer pairs, not padding.
        let hr = SidLayout::detect(KNOWN[9].1).unwrap();
        assert_eq!(hr.song_list_qty, 5);
        assert_eq!(u16::from_be_bytes([KNOWN[9].1[14], KNOWN[9].1[15]]), 5);

        // Zoids is the one case where detection is the conservative side: the
        // instrument table has room for 15 records but the patterns only ever
        // reach index 12, so the last two are unreachable either way.
        let zoids = SidLayout::detect(KNOWN[5].1).unwrap();
        assert_eq!(zoids.instr_qty, 13);
        assert_eq!(
            zoids.instrument_histogram(KNOWN[5].1)[13..]
                .iter()
                .sum::<u32>(),
            0
        );
    }

    /// Detection declines cleanly on an image that is not a member of this
    /// player family rather than returning a nonsense layout. All six tunes of
    /// the last generation contain at least one of the signatures by accident —
    /// it is the consistency check that turns them away.
    #[test]
    fn declines_the_last_generation() {
        for (name, data) in [
            (
                "lion_heart",
                include_bytes!("songs/lion_heart.sid").as_slice(),
            ),
            (
                "sun_never_shines",
                include_bytes!("songs/sun_never_shines.sid"),
            ),
            ("pacific_coast", include_bytes!("songs/pacific_coast.sid")),
            ("radio_ace", include_bytes!("songs/radio_ace.sid")),
            ("go_go_dash", include_bytes!("songs/go_go_dash.sid")),
            (
                "lakers_vs_celtics",
                include_bytes!("songs/lakers_vs_celtics.sid"),
            ),
        ] {
            assert!(SidLayout::detect(data).is_none(), "{name}");
        }
    }
}
