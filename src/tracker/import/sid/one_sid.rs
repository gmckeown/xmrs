//! Embedded SID demo songs — a bundle of `(name, payload)` for
//! historical Rob-Hubbard tunes shipped with the crate so the
//! `import_sid` test suite and the cpal-player `--sid-test-player`
//! switch have something to play without an external `.sid` file.
//! Each helper returns the corresponding [`SidModule`] ready to
//! call `to_modules()` on.

use crate::{
    prelude::*,
    tracker::instr_robsid::{ArpMode, BounceRange, RobEffects, SkydiveMode},
    tracker::instr_sid::SidVoice,
};
use alloc::{vec, vec::Vec};

use super::sid_module::SidModule;
use super::sound_fx::SoundFx;
use super::voices::Voices;

/// The instrument-fx-byte layout of a v20/v25 replayer. Rob Hubbard rewrote
/// the `play` routine across tunes, so a single "v20" bucket actually holds two
/// distinct fx-bit schemes (Ghidra-verified per tune):
/// - `Simple` (International Karate) — the v10 scheme: bit0=drum, bit2=arp(−5),
///   bit3=PW-low-byte-mode. No wave-alt / vib-table / two-phase-pitch.
/// - `Rich` (Sanxion, Lightforce, Ace 2, Human Race) — bit0=drum, bit1=wave-alt,
///   bit2=two-phase-wave, bit3=arp, bit4=vib-table, bit5=filter, bit6=two-phase-
///   pitch; the control register is `wave & gate_mask`.
///
/// Only consulted for versions 20..29 (`one_sid` decode); other versions ignore it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidFxLayout {
    Simple,
    Rich,
}

#[derive(Clone, Debug)]
pub struct OneSid {
    pub song: &'static [u8],
    pub name: &'static str,
    pub author: &'static str,
    pub copyright: &'static str,
    version: usize,
    load_adress: usize,
    song_track_qty: usize,
    song_list_offset: usize,
    song_list_qty: usize,
    patt_ptl_offset: usize,
    patt_pth_offset: usize,
    patt_qty: usize,
    instr_offset: usize,
    instr_qty: usize,
    fx_v1_offset: usize,
    fx_v1_qty: usize,
    fx_v2_offset: usize,
    fx_v2_qty: usize,
    pub resetspd: usize,
    /// v20/v25 instrument-fx-byte layout (see [`SidFxLayout`]). Default `Rich`;
    /// only International Karate is `Simple`. Ignored for other versions.
    pub fx_layout: SidFxLayout,
    /// Per-sub-song ticks-per-row override (`resetspd` value, indexed by song
    /// number). The replayer sets the speed per song in its init routine; most
    /// tunes use one speed (this stays `&[]` → fall back to `resetspd`), but
    /// some (commando = `[2,3,2]`) differ per sub-song. Verified vs the oracle's
    /// note period / drum-noise-row length.
    pub resetspd_songs: &'static [usize],
    skydive_v1_when: usize, // Skydive active when note length > this value
    skydive_v1_add: isize,  // Added to frequency each frame (can be negative)
    /// Per-tune note-table overflow substitutions `(raw_note, real_note)`
    /// for note bytes > 95 that index past `RH_FREQ_HEX` into engine
    /// work-RAM (tune-specific pitch); empty = use the decoder's default.
    overflow_overrides: &'static [(u8, u8)],
    /// Per-tune `(instrument_index, raw_tempvdif_reg)` for instruments played
    /// **only** with an overflow note whose vibrato `tempvdif` the replayer
    /// reads from `RH_FREQ_HEX` past the chromatic table (engine work-RAM). A
    /// note→note `overflow_overrides` substitution recovers the base pitch but
    /// not this excursion (the chromatic semitone can't reach it), so the
    /// instrument's vibrato gets the measured raw register step instead. Monty:
    /// instr 12 plays only note 104 → 0x84D0 holds `voice_ctrl`/`notenum`, so
    /// the vibrato step is `(notenum_v0 − ctrl)<<8 >> 2 ≈ 0x540` (1344). Empty
    /// = none (the common case). See [`crate::tracker::instr_robsid::Vibrato::overflow_reg`].
    overflow_vib: &'static [(usize, u16)],
    /// Periodic-frame-skip reset value `N` for replayers that open `play` with
    /// `DEC cnt; BPL ok; LDA #N; STA cnt; RTS` (skip 1 frame every `N+1`,
    /// playing the tune at `N/(N+1)` of 50 Hz — a fractional-tempo trick). The
    /// importer scales `default_bpm` by `N/(N+1)` to match. 0 = no skip (the
    /// common case). Read from the binary (the `LDA #N` at the play entry):
    /// International Karate = 10 (1-in-11), Thrust = 9 (1-in-10).
    pub frame_skip_reset: usize,
    /// Engine address of the per-instrument **SID filter** parameter table
    /// (8-byte records, parallel to the instrument table by index): byte `+6` =
    /// `resfilt` (`$D417`), byte `+7` = signed cutoff-hi `step` (`$D416`). A
    /// record with both bytes 0 = that instrument has no filter. 0 = the tune
    /// uses no filter (the common case). ace_2 = `$E624`. See the Ghidra
    /// reference and `generators/sid/SID_FILTER_PLAN.md`.
    filter_table: usize,
    /// Cutoff-hi value the replayer's note-on loads into the filter accumulator
    /// (`LDA #imm ; STA <acc>`), a per-tune constant. ace_2 = 48. Only meaningful
    /// when `filter_table != 0`.
    filter_seed: usize,
    /// Effective **frames-per-row** as an exact fraction `(num, den)` when the
    /// replayer's tempo is *fractional* — i.e. a pure 50 Hz player advancing the
    /// note stream every integer number of frames cannot reproduce it. Rob
    /// Hubbard's later drivers (ace_2) gate the note-step on a **dual counter**
    /// (`$e08e`: `DEC A; BPL .b; LDA #Ka; ...; .b: DEC B; BPL ok; LDA R; STA B`
    /// then advance only when `A!=0 && B==R`) which yields a non-integer average
    /// — ace_2 = `Ka=3, R=1` → the step fires on a repeating `[3,3,2]`-frame
    /// pattern = `8/3 ≈ 2.667` frames/row (uniform `1+resetspd=2` plays it 33 %
    /// too fast). The importer bakes this as a non-uniform per-row tick cadence
    /// (`tick(r) = ⌊r·num/den⌋`, `speed_at_row = tick(r+1)−tick(r)`); the SID
    /// chip still advances one frame per tick (50 Hz) so the per-frame filter
    /// sweep / vibrato stay correct, while the note stream advances at the true
    /// fractional rate. `None` = integer tempo `1+resetspd` (the common case).
    /// NB: only sound when every voice pass length is a multiple of `den` (so
    /// the per-lane loop span stays a multiple of the cadence period and the
    /// free-run fold lands on real timeline ticks); ace_2's voices are all 5880
    /// rows (÷3), the only tune using this so far.
    pub tempo_frac: Option<(u32, u32)>,
    /// Engine address of the per-tune **three-note arpeggio master table**
    /// (fxmask bit4, `0x10`): a flat array of SIGNED semitone-offset PAIRS
    /// (`off_a, off_b`), selected per instrument by `fx_v2[i][0] * 2`. The
    /// importer bakes the chosen pair into [`ArpMode::ThreeNote`]. Lightforce =
    /// `$f60c` (`fc f9 fb f7 …` → `(-4,-7) (-5,-9) …`, Ghidra `play $f0bf`,
    /// `DAT_f5f2 & 0x10`). 0 = the tune does not use the three-note arp (the
    /// common case; bit4 is then left to its other meaning / vestigial). Only
    /// wired for tunes whose replayer actually tests bit4 and whose table address
    /// is Ghidra-verified — NOT IK / spellbound (vestigial there).
    pub arp3_table: usize,
    /// Enable the instrument **period-interpolation vibrato** (`play $09f1`):
    /// instruments whose record byte +5 (`$cfc`,Y) is non-zero swing the pitch
    /// by a triangle whose half-depth is `(b5 & 0x78)>>3` semitone-period steps
    /// each shifted right `b5 & 7` times. This is a SEPARATE, wider vibrato than
    /// the `vib_depth` one. Gated per-tune (default false) because other coupled
    /// tunes (commando golden, zoids) also have non-zero +5 bytes the synth
    /// already renders acceptably — enabling globally would change their audio.
    /// Thrust's intro instrument 26 (b5 = 0x30 → ±3-semitone vibrato) is rendered
    /// as a static note without it. Only `true` for Thrust so far.
    pub interp_vibrato: bool,
}

impl OneSid {
    /// Per-instrument "short-gate" flag for the v30 (Delta) replayer: an
    /// instrument with fxmask bit0 set (Rob-Hubbard's "drum" flag) has its gate
    /// cleared after a single row (Ghidra Delta `play` FUN_c110 $c124,
    /// unconditional under bit0), so its notes are short percussive accents
    /// rather than sustained tones. The importer reproduces this GATE timing —
    /// not the drum *timbre*, which stays unmodelled (the wave-alt/arp on those
    /// instruments dominate it) — see [`super::voices::decode_phrase`]. Returns
    /// all-false for non-v30 tunes (whose bit0 means drum/skydive and is decoded
    /// into `RobEffects` instead).
    pub fn short_gate_instrs(&self) -> Vec<bool> {
        if self.version != 30 {
            return vec![false; self.instr_qty];
        }
        let file_offset = 126 + self.instr_offset - self.load_adress;
        (0..self.instr_qty)
            .map(|i| {
                self.song.get(file_offset + i * 8 + 7).copied().unwrap_or(0) & 0b0000_0001 != 0
            })
            .collect()
    }

    /// Per-sub-song fractional tempo for v30 (Delta). The init routine ($c357)
    /// loads, indexed by the sub-song A: `R = $c311[A]` (→ work-RAM $c32b) and
    /// `Ka = $c31e[A]` (→ the self-modified immediate at $be3e). The note stream
    /// then advances one row every **(R+1)·(Ka+1)/Ka** frames — the closed form
    /// of the `play` $be38 dual-counter, verified against the sidplay oracle on
    /// all 13 sub-songs (e.g. A=11 → R=1,Ka=4 → 2.5; A=3 → R=3,Ka=8 → 4.5; A=1 →
    /// R=3,Ka=4 → 5.0). Without this every sub-song inherited the single baked
    /// `tempo_frac` (sub-song 12's 2.5), so the R≠1 tunes (sidplay songs 2-11)
    /// ran at the wrong speed. Non-v30 tunes keep their global `tempo_frac`.
    pub fn tempo_frac_for(&self, song: usize) -> Option<(u32, u32)> {
        if self.version != 30 {
            return self.tempo_frac;
        }
        let rf = 126 + 0xC311 - self.load_adress;
        let kf = 126 + 0xC31E - self.load_adress;
        let r = *self.song.get(rf + song)? as u32;
        let ka = *self.song.get(kf + song)? as u32;
        if ka == 0 {
            return self.tempo_frac;
        }
        let (mut num, mut den) = ((r + 1) * (ka + 1), ka);
        // reduce the fraction (small Euclid, no_std-friendly). `num = (r+1)·(ka+1)
        // >= 1`, so the gcd `a` is always >= 1 — no divide-by-zero guard needed.
        let (mut a, mut b) = (num, den);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        num /= a;
        den /= a;
        Some((num, den))
    }

    /// Decode this tune's embedded payload + per-tune config into a playable
    /// [`SidModule`] (instruments, voice layout, tempo, sub-songs).
    pub fn to_sidmodule(&self) -> SidModule {
        // ********* CHANNELS
        let mut channels_ptr: Vec<usize> = vec![];
        for i in 0..self.song_list_qty {
            let offset =
                126 + self.song_list_offset - self.load_adress + i * 2 * self.song_track_qty;
            for j in 0..self.song_track_qty {
                channels_ptr.push(
                    self.song[offset + j] as usize
                        | (self.song[offset + j + self.song_track_qty] as usize) << 8,
                );
            }
        }

        let mut channels: Vec<Vec<u8>> = vec![];
        for offset in &channels_ptr {
            let file_offset = *offset + 126 - self.load_adress;
            let mut raw: Vec<u8> = vec![];
            let mut j = 0;
            while self.song[file_offset + j] & 0x80 == 0 {
                raw.push(self.song[file_offset + j]);
                j += 1;
            }
            let tracks = if self.version == 30 {
                // Version 30 (Delta) order stream is **interleaved
                // `[phrase, count, phrase, count, …]`**, not a flat phrase list:
                // the replayer's order walker (`play` $bf85–$bf97) keeps a
                // per-voice repeat counter `c354` (init 1) and, when a phrase
                // ends, reads the next byte as a repeat *count* for the byte
                // after it. So `phrase[0]` plays once, then each following pair
                // `(count, phrase)` plays `phrase` `count` times. Expand to the
                // flat phrase sequence the rest of the importer expects. (This
                // also explains the apparent "111/112 overflow" the old hack
                // masked — those were repeat counts, never phrase indices.)
                let mut out: Vec<u8> = Vec::new();
                if let Some(&first) = raw.first() {
                    out.push(first);
                }
                let mut k = 1;
                while k + 1 < raw.len() {
                    let count = raw[k];
                    let phrase = raw[k + 1];
                    for _ in 0..count {
                        out.push(phrase);
                    }
                    k += 2;
                }
                out
            } else {
                raw
            };
            channels.push(tracks);
        }

        // ********* SONGS
        let mut songs: Vec<Vec<usize>> = vec![];
        for i in 0..self.song_list_qty {
            let mut s: Vec<usize> = vec![];
            for j in 0..self.song_track_qty {
                //let k = channels_ptr[i * self.song_track_qty + j] + 126 - self.load_adress;
                let k = channels_ptr[i * self.song_track_qty + j];
                let index: usize = channels_ptr
                    .iter()
                    .position(|&x| x == k)
                    .unwrap_or_default();
                s.push(index);
            }
            songs.push(s);
        }

        // ********* TRACKS
        let mut tracks_ptr: Vec<u16> = vec![];
        for i in 0..self.patt_qty {
            let offset_low_file = 126 + self.patt_ptl_offset - self.load_adress;
            let offset_high_file = 126 + self.patt_pth_offset - self.load_adress;
            let offset: u16 = self.song[offset_low_file + i] as u16
                | (self.song[offset_high_file + i] as u16) << 8;
            tracks_ptr.push(offset);
        }

        let mut tracks: Vec<Vec<u8>> = vec![];
        for &tp in tracks_ptr.iter().take(self.patt_qty) {
            let file_offset = 126 + tp as usize - self.load_adress;
            let mut track: Vec<u8> = vec![];
            let mut j = 0;
            loop {
                track.push(self.song[file_offset + j]);
                if self.song[file_offset + j] == 0xff {
                    break;
                }
                j += 1;
            }
            tracks.push(track);
        }

        // ********* INSTRUMENTS

        let mut instruments: Vec<InstrRobSid> = vec![];
        for i in 0..self.instr_qty {
            let file_offset = 126 + self.instr_offset - self.load_adress;
            let start = file_offset + i * 8;

            let mut voice = SidVoice::default();
            voice.pw = self.song[start] as u16 | (self.song[start + 1] as u16) << 8;
            voice.update_from_ctrl_register(self.song[start + 2]);
            voice.ad = self.song[start + 3];
            voice.sr = self.song[start + 4];

            let mut re = RobEffects::default();
            // AD+SR are zeroed when a note ends (v15 `sb_play` $e198; the v10
            // routine does the same), so notes cut sharp rather than ringing out
            // on the instrument's release nibble. Verified per version against the
            // sidplay regdump (the oracle writes 0x00 to $d405/$d406 at note-end).
            // v30 (Delta) does NOT zero them (its notes ring out) — see the memory.
            re.hard_cut_release = self.version == 10 || self.version == 15;
            if self.version == 10 {
                re.vibrato.depth = self.song[start + 5] << 2; // original seems 0..3
                re.vibrato.speed_div = 7;
            } else {
                re.vibrato.depth = (self.song[start + 5] & 0b0111_1000) >> 3;
                re.vibrato.speed_div = self.song[start + 5] & 0b0000_0111;
            }
            // v15 (Spellbound) vibrato is the CENTERED period-interpolation model
            // (Ghidra `sb_play` $e012: instrument byte +5 → half-depth `(b5&0x78)>>3`,
            // shift `b5&7`, a triangle swinging symmetrically around the note), handled
            // by the `interp_vib` block below (gated on `interp_vibrato`). The upward
            // `vib_depth` model (base + positive-only triangle) gave the wrong shape —
            // an audible warble a third too high on the held leads. So skip the upward
            // flag for v15; the interp path owns its vibrato.
            if re.vibrato.depth != 0 && self.version != 15 {
                re.vibrato.enable = true;
            }

            // The v20/v25/v30 replayers that carry an fx_v2 table (Sanxion,
            // Lightforce, Delta) all share ONE pulse-width routine: the
            // instrument's byte +6 is the FULL per-frame step, added to the
            // 12-bit PW *every* frame (no speed|delay split), bouncing on the PW
            // high nibble between the `fx_v2[i]+5` nibbles (hi = upper, lo =
            // lower). Ghidra-verified register-exact: Delta `play $c094`, Sanxion
            // `play_san $be20` (`LDA instr+6; ADC pw_lo` each frame; bounce
            // compares patched from `fx_v2+5`). The masked speed|delay model
            // below is wrong here — e.g. Sanxion voice 1's byte6=$30 must step
            // +$30/frame, not the masked +$20 every 17 frames.
            if self.fx_v2_offset != 0 && self.version >= 20 {
                re.pulse_sweep.speed = self.song[start + 6] as i8;
                re.pulse_sweep.delay = 0;
                let bnd = self
                    .song
                    .get(126 + self.fx_v2_offset - self.load_adress + i * 8 + 5)
                    .copied()
                    .unwrap_or(0);
                // Explicit bounce range (hi nibble = upper, lo nibble = lower). A
                // lo bound of 0 is legitimate (Sanxion v1 sweeps the high nibble
                // 0..2), so it is carried as `Some` rather than the legacy
                // 0x08..=0x0E fallback the player uses when this is `None`.
                re.pulse_sweep.bounce = Some(BounceRange {
                    lo: bnd & 0x0f,
                    hi: bnd >> 4,
                });
                if self.version == 30 {
                    // v30 gates vibrato on note length (Ghidra `dovib` $c04c: skip
                    // when `(flags & 0x1f) < 3`). Flag it so the player suppresses
                    // vibrato on short/staccato notes (approximated by hold time).
                    re.vibrato.length_gate = true;
                }
            } else {
                // PW step|delay split: byte+6's HIGH nibble = per-bounce step,
                // LOW nibble = delay. v15 (Spellbound `sb_play`) AND the v20
                // SIMPLE layout (International Karate `play $ae0c`, Ghidra: PW
                // routine reads `instr+6`, `step = b6 & 0xf0` added to the PW low
                // byte every `(b6 & 0x0f)` frames, bouncing the high nibble 8..E)
                // use this nibble split. The v10/v20-Rich routine instead masks
                // the top 3 bits for the step (register-exact vs the Commando
                // oracle — leave it). Without the nibble split, IK's bass instr6
                // (`b6 = 0x14`) decoded `step = 0` → a STATIC pulse where the chip
                // sweeps the PW (audible timbre + ~1.3× too loud in the body).
                let nibble_split = self.version == 15 || self.fx_layout == SidFxLayout::Simple;
                let mask_lo: u8 = if nibble_split {
                    0b0000_1111
                } else {
                    0b0001_1111
                };
                let mask_hi: u8 = if nibble_split {
                    0b1111_0000
                } else {
                    0b1110_0000
                };

                re.pulse_sweep.delay = (self.song[start + 6] & mask_lo) as u16;
                re.pulse_sweep.speed = (self.song[start + 6] & mask_hi) as i8;
            }
            // The v20/v25 replayer (Lightforce/Ace 2/Sanxion) keeps the swept PW
            // in per-voice RAM and reloads the instrument base PW on every
            // note-setup, so the sweep restarts each note. The v10 routine
            // (commando/zoids) mutates the instrument table in place and lets the
            // sweep persist; the v30 routine (Delta) only reseeds on a fresh note
            // (note byte bit 7 clear) and ties otherwise — our note stream can't
            // tell, and continuation already renders Delta register-exact — so
            // restrict the unconditional reseed to the v20 family.
            re.pulse_sweep.reseed_on_note = (20..30).contains(&self.version);

            // Instrument FX byte (byte 7 of instrument data). The bit MEANINGS
            // depend on the Rob-Hubbard player VERSION — he rewrote the routine
            // several times. The v10 routine (Commando/Monty, Ghidra-confirmed):
            //   Bit 0: drum (noise + rapid freq slide)
            //   Bit 1: skydive (slow freq down)
            //   Bit 2: octave arpeggio
            // The v20 routine (Lightforce/ace_2/IK/sanxion, Ghidra `play $f0bf`)
            // reassigns the low bits entirely:
            //   Bit 0: freq effect      Bit 1: per-frame waveform alternation
            //   Bit 2: two-phase waveform (attack `$f6c9` for `$f6cb` frames →
            //          sustain), Bit 3: octave arpeggio, Bit 4: vibrato-table,
            //   Bit 5: filter (handled via filter_table below), Bit 6: freq-table.
            // The v30 routine (Delta, Ghidra `play $bde4`, fx-application
            // `$c110-$c204`, mask byte `$c33b`) is different again — each bit of
            // the instrument byte+7 selects (Y = instr*8 into the fx_v2 table at
            // `$c43e`):
            //   Bit 0 (0x01): drum (frame-0 noise `$c13b`, then freq slide + gate)
            //   Bit 1 (0x02): per-frame waveform alternation (`$c14a`: counter&1
            //                 → instr ctrl `$c390` else alt ctrl `$c440`=fx_v2+2)
            //   Bit 2 (0x04): two-phase waveform attack (`$c164`: fx_v2+1 ctrl
            //                 `$c43f` for fx_v2+3 frames `$c441`, then instr ctrl)
            //                 — IDENTICAL mechanism + offsets to v20 bit 2.
            //   Bit 3 (0x08): 2-note arpeggio (`$c17f`: counter&1 → played note
            //                 else fx_v2+4 note `$c442`)
            //   Bit 5 (0x20): filter (handled via filter_table below)
            //   Bit 6 (0x40): two-phase pitch (`$c1c3`: fx_v2+0 note `$c43e` for
            //                 fx_v2+3 frames, then played note)
            //   Bit 7 (0x80): slow filter-base ramp (minor; not modelled)
            let fxmask = self.song[start + 7];
            // v20/v25 family — two distinct replayer fx layouts (see
            // `SidFxLayout`, Ghidra-verified per tune). Bit 0 = DRUM in BOTH
            // (identical to the v10 routine: `instrfx&1 && freq_hi!=0 &&
            // lengthleft!=0` → noise burst on the first row, then freq-high slide
            // + gate-off). The rest of the byte differs by layout.
            if (20..30).contains(&self.version) {
                if fxmask & 0b0000_0001 != 0 {
                    re.drum.enable = true;
                }
                match self.fx_layout {
                    SidFxLayout::Simple => {
                        // International Karate (Ghidra `play` $ae0c). bit2 = arp
                        // alternating note / note−5 (a fixed two-note drop,
                        // exactly the v15 alt-arp with N=5); bit3 = PW low-byte
                        // increment (same as the v10/commando bit3 mode).
                        if fxmask & 0b0000_0100 != 0 {
                            re.arpeggio = ArpMode::TwoNoteInterval(5);
                        }
                        if fxmask & 0b0000_1000 != 0 {
                            re.pulse_sweep.low_byte_mode = true;
                            re.pulse_sweep.speed = self.song[start + 6] as i8;
                            re.pulse_sweep.delay = 0;
                        }
                    }
                    SidFxLayout::Rich => {
                        // Sanxion / Lightforce / Ace 2 / Human Race (Ghidra
                        // `play_san` $be20 etc.): bit3 = arp, bit2 = two-phase
                        // waveform attack. NB the rich family ALSO has bit1
                        // (wave-alt), bit4 (vib-table) and bit6 (two-phase-pitch),
                        // but their fx_v2 byte offsets are NOT uniform across the
                        // tunes (e.g. Sanxion's two-phase frame count is not at
                        // `fx_v2+3` like Lightforce/Delta), so modelling them
                        // generically regressed Sanxion — they need per-tune
                        // offset verification before being enabled. Kept to the
                        // filter_table-anchored two-phase that Lightforce/Ace 2
                        // already rely on.
                        if fxmask & 0b0000_1000 != 0 {
                            re.arpeggio = ArpMode::Octave;
                        }
                        if fxmask & 0b0000_0100 != 0 && self.filter_table != 0 {
                            let foff = 126 + self.filter_table - self.load_adress + i * 8;
                            let attack = self.song[foff + 1];
                            if attack != 0 {
                                re.two_phase.attack_ctrl = attack;
                                re.two_phase.attack_frames = self.song[foff + 3];
                            }
                        }
                        // Bit 6 (0x40): two-phase PITCH attack — an ABSOLUTE attack
                        // note (`fx_v2[i]+0`, the replayer's `freq_table[note*2]`)
                        // held for the shared attack window before the struck note
                        // (Ghidra sanxion `$be20`, `DAT_b548 & 0x40`). Shares
                        // `attack_frames` (`fx_v2+3`) with the bit2 waveform attack;
                        // the replayer burns the shared counter once per active
                        // two-phase bit, so a `0x44` lead's attack lasts half as
                        // long (handled in the coupled driver's double-decrement).
                        // filter_table-anchored, like bit2 — so it is wired for the
                        // tunes whose record sits there (Lightforce / Ace 2), NOT
                        // Sanxion (filter_table 0: its frame count is not at +3,
                        // the non-uniformity that regressed a generic wiring).
                        if fxmask & 0b0100_0000 != 0 && self.filter_table != 0 {
                            let foff = 126 + self.filter_table - self.load_adress + i * 8;
                            re.two_phase.attack_note = Some(self.song[foff]);
                            if re.two_phase.attack_frames == 0 {
                                re.two_phase.attack_frames = self.song[foff + 3];
                            }
                        }
                        // Bit 4 (0x10): three-note arpeggio (Ghidra lightforce
                        // `play $f0bf`, `DAT_f5f2 & 0x10`). `fx_v2[i][0]` selects a
                        // SIGNED semitone-offset pair from the per-tune master table
                        // (`arp3_table`); bake it into `ArpMode::ThreeNote`. Set
                        // last so it wins over the bit3 octave arp (they are
                        // distinct bits — no instrument sets both — but be explicit).
                        // Only enabled where the master-table address is verified
                        // (Lightforce); `arp3_table == 0` leaves bit4 alone.
                        if fxmask & 0b0001_0000 != 0 && self.arp3_table != 0 {
                            let idx = self.song[126 + self.fx_v2_offset - self.load_adress + i * 8]
                                as usize;
                            let poff = 126 + self.arp3_table - self.load_adress + idx * 2;
                            re.arpeggio = ArpMode::ThreeNote {
                                off_a: self.song[poff] as i8,
                                off_b: self.song[poff + 1] as i8,
                            };
                        }
                        // Bit 1 (wave-alt, fx_v2+2) and bit 4's vib-table meaning on
                        // tunes WITHOUT a verified `arp3_table` are still not decoded
                        // (Sanxion's offsets are non-uniform; they need an audio
                        // oracle to validate). Ghidra offsets: alt=fx_v2+2.
                    }
                }
            } else if self.version == 30 {
                let foff = 126 + self.filter_table - self.load_adress + i * 8;
                // Bit 2: two-phase waveform attack (same mechanism + offsets as
                // v20). This is the one v30 effect modelled so far; it lifts the
                // default song (12) cos 0.905→0.938. (The other masks below are
                // decoded in the doc comment but unmodelled — instruments that
                // rely on them, e.g. the 0x44 lead in songs 4/5, lose the lucky
                // v10-fallback octave-arp and sit a touch lower; net corpus
                // average still rises. Revisit when the oracle is stable.)
                if fxmask & 0b0000_0100 != 0 && self.filter_table != 0 {
                    let attack = self.song[foff + 1];
                    if attack != 0 {
                        re.two_phase.attack_ctrl = attack;
                        re.two_phase.attack_frames = self.song[foff + 3];
                    }
                }
                // Bit 1: per-frame waveform alternation (alt ctrl `fx_v2+2`).
                if fxmask & 0b0000_0010 != 0 && self.filter_table != 0 {
                    re.wave_alt.alt_ctrl = self.song[foff + 2];
                }
                // Bit 3: 2-note arpeggio (fixed note `fx_v2+4`). Bake its PAL SID
                // frequency register from the replayer note→freq table `$c226`.
                if fxmask & 0b0000_1000 != 0 && self.filter_table != 0 {
                    let note = self.song[foff + 4] as usize;
                    let toff = 126 + 0xC226 - self.load_adress + note * 2;
                    re.arpeggio = ArpMode::TwoNoteFixed(
                        self.song[toff] as u16 | (self.song[toff + 1] as u16) << 8,
                    );
                }
                // Bit 6 (0x40): two-phase PITCH attack (`$c1c3`): an ABSOLUTE
                // attack note (`fx_v2+0`) held for `fx_v2+3` frames before the
                // struck note — identical mechanism + offsets to v20. The `$c345`
                // counter is shared with the bit2 waveform attack and decremented
                // by BOTH bits, so a `0x44` lead's attack lasts half as long; the
                // coupled driver's double-decrement now models exactly that (the
                // reason a first naive pass measured net-negative).
                if fxmask & 0b0100_0000 != 0 && self.filter_table != 0 {
                    re.two_phase.attack_note = Some(self.song[foff]);
                    if re.two_phase.attack_frames == 0 {
                        re.two_phase.attack_frames = self.song[foff + 3];
                    }
                }
                // Bit 0 (drum) decoded in the doc comment above but not modelled:
                // its ctrl/freq are dominated by wave-alt/arp on the instruments
                // that set it (e.g. i17). Bit 7 = slow filter-base ramp (minor).
            } else {
                if fxmask & 0b0000_0001 != 0 {
                    re.drum.enable = true;
                    // Enable the register-exact drum in the coupled driver for the
                    // versions verified vs the sidplay regdump (v10 Commando,
                    // v15 Spellbound). The v10 routine slides the freq-high byte
                    // down each frame; the v15 routine keeps it constant.
                    if self.version == 10 || self.version == 15 {
                        re.drum.no_freq_slide = self.version == 15;
                    }
                }
                if fxmask & 0b0000_0010 != 0 {
                    re.skydive.enable = true;
                }
                // Bit 2 = octave arpeggio (base × 2 per frame) ONLY in the v10
                // routine. The v15 routine (Spellbound, Ghidra `sb_play` $e012)
                // has NO frequency-doubling: bit 2 toggles the note DOWN by N
                // semitones on alternate frames (a two-note arp), where N is the
                // fx byte's HIGH NIBBLE — the replayer self-modifies the `SBC #imm`
                // operand from `fx >> 4` (`$e332 STA $e34a` → `$e349 SBC`). So
                // 0x56→−5 (fourth), 0x25→−2, 0x35→−3. The old v10 octave model
                // clamped the freq register to 0xFFFF (screech on voice 1). For
                // v15 carry the per-instrument interval; for v10 keep octave arp.
                if fxmask & 0b0000_0100 != 0 {
                    if self.version == 15 {
                        re.arpeggio = ArpMode::TwoNoteInterval(fxmask >> 4);
                    } else {
                        re.arpeggio = ArpMode::Octave;
                    }
                }
                // Bit 3 (v10 `DAT_5523 & 8`): PW low-byte increment mode. The
                // normal bouncing sweep is replaced by `pw_lo += byte+6` (the
                // FULL value, not the `&0xE0` step) each frame, low byte only,
                // 8-bit wrap, no delay/bounce. Overrides the masked pw_speed set
                // above with the full byte. (commando/zoids hard-sync voice 2.)
                if self.version == 10 && fxmask & 0b0000_1000 != 0 {
                    re.pulse_sweep.low_byte_mode = true;
                    re.pulse_sweep.speed = self.song[start + 6] as i8;
                    re.pulse_sweep.delay = 0;
                }
            }

            // Period-interpolation vibrato (`play $09f1`): instrument byte +5
            // (`$cfc`,Y) — half-depth `(b5 & 0x78)>>3` semitone-period steps,
            // each `>> (b5 & 7)`. Only when the tune opts in (Thrust); other
            // coupled tunes' +5 bytes mean other things / render fine already.
            if self.interp_vibrato {
                let b5 = self.song[start + 5];
                if b5 != 0 {
                    re.interp.enable = true;
                    re.interp.half_depth = (b5 & 0x78) >> 3;
                    re.interp.shift = b5 & 0x07;
                }
            }

            re.skydive.length_gate = self.skydive_v1_when as u8;
            re.skydive.mode = if self.skydive_v1_add == 0 {
                SkydiveMode::Climb
            } else {
                SkydiveMode::Add(self.skydive_v1_add as i16)
            };

            // Overflow-note vibrato excursion (work-RAM `tempvdif`, see field
            // doc): force the raw register step for instruments that only ever
            // play an overflow note.
            if let Some(&(_, t)) = self.overflow_vib.iter().find(|&&(idx, _)| idx == i) {
                re.vibrato.enable = true;
                re.compat.vib_overflow_reg = t;
            }

            // SID filter: per-instrument record in the parallel filter table
            // (byte +6 = resfilt `$D417`, byte +7 = signed cutoff-hi step
            // `$D416`). A record with both 0 means this instrument has no
            // filter. Only the coupled driver acts on these.
            //
            // The version-30 player (Delta) gates the per-frame filter sweep on
            // fx-mask bit 5 (`$c33b AND #$20`, `play $c1a4`) — the instrument
            // byte+7 mask, NOT the record contents. Delta's fx_v2 table holds
            // non-zero filter bytes for instruments whose mask bit is *clear*
            // (e.g. instr 4: record `f2 18`, mask `00`), so the record-only
            // test below would wrongly filter them. Require the mask bit for
            // v30. (v10/v20 keep the record-only behaviour: lightforce/etc. have
            // zero records when unfiltered, so they're unaffected.)
            if self.filter_table != 0 {
                let foff = 126 + self.filter_table - self.load_adress + i * 8;
                let resfilt = self.song[foff + 6];
                let step = self.song[foff + 7] as i8;
                let mask_ok = self.version != 30 || (self.song[start + 7] & 0b0010_0000) != 0;
                if mask_ok && (resfilt != 0 || step != 0) {
                    re.filter.enable = true;
                    re.filter.resonance = resfilt >> 4;
                    re.filter.routing = resfilt & 0x0F;
                    re.filter.cutoff_step = step;
                    re.filter.cutoff_seed = self.filter_seed as u8;
                    // v30 (Delta) zeroes the cutoff accumulator ($c34e) on every
                    // note-setup ($beaf), so the sweep restarts per note instead
                    // of running legato across same-instrument notes (Ghidra
                    // play $c1a8 + note-fetch $beaf). Older filter replayers
                    // (ace_2/lightforce) are left continuing — unverified there.
                    // Re-seed the cutoff accumulator to `filter_seed` on every
                    // note (vs. legato continuation across same-instrument
                    // notes). Verified per replayer against the sidplay oracle:
                    // the v25+ revision (Lightforce v25, Delta v30) zeroes the
                    // accumulator each note so the cutoff pulses per note; true
                    // v20 (Ace 2) sweeps continuously and wraps.
                    re.filter.reseed_each_note = self.version >= 25;
                }
            }

            let mut irsid = InstrRobSid::default();
            irsid.voice = voice;
            irsid.fx = re;

            instruments.push(irsid);
        }

        // ********* SOUNDFX
        let mut sfxs: Vec<SoundFx> = vec![];
        for i in 0..self.fx_v1_qty {
            let file_offset = 126 + self.fx_v1_offset - self.load_adress;
            let start = file_offset + i * 16;

            // play current music freq at end of incdec_counter counter
            let incdec_start_at_end = (self.song[start] & 0b1000_0000) == 0;
            let incdec_counter = if (self.song[start] & 0b0011_0000) == 0b0010_0000 {
                self.song[start] as i8 & 0b0000_1111
            } else {
                -(self.song[start] as i8 & 0b0000_1111)
            };
            let note_start = self.song[start + 1]; // as u16|(self.song[start+2] as u16)<<8;
            let note_delta = self.song[start + 8] & 0b0011_1111;
            let note_end = self.song[start + 15] & 0b0011_1111;
            let flipflop_voice1_ctrl =
                ((self.song[start + 8] as u16 | (self.song[start + 9] as u16) << 8) & 0b0100_0000)
                    != 0;
            let voice0_ctrl = (self.song[start + 15] & 0b1000_0000) != 0;
            let voice1_ctrl = (self.song[start + 15] & 0b0100_0000) != 0;

            let mut voice0 = SidVoice::default();
            voice0.pw = self.song[start + 3] as u16 | (self.song[start + 4] as u16) << 8;
            voice0.update_from_ctrl_register(self.song[start + 5]);
            voice0.ad = self.song[start + 6];
            voice0.sr = self.song[start + 7];

            let mut voice1 = SidVoice::default();
            voice1.pw = self.song[start + 10] as u16 | (self.song[start + 11] as u16) << 8;
            voice0.update_from_ctrl_register(self.song[start + 12]);
            voice1.ad = self.song[start + 13];
            voice1.sr = self.song[start + 14];

            let sfx = SoundFx {
                incdec_start_at_end,
                incdec_counter,
                note_start,
                note_delta,
                note_end,
                flipflop_voice1_ctrl,
                voice0_ctrl,
                voice1_ctrl,
                voice0,
                voice1,
            };
            sfxs.push(sfx);
        }

        SidModule {
            sid: self.clone(),
            voices: Voices::new(
                self.version,
                songs,
                channels,
                tracks,
                self.overflow_overrides,
            ),
            instruments,
            soundfx: sfxs,
        }
    }
}

impl OneSid {
    pub fn get_sid_commando() -> Self {
        OneSid {
            song: include_bytes!("songs/commando.sid"),
            name: "Commando",
            author: "Rob Hubbard",
            copyright: "1985 Elite",
            version: 10,
            load_adress: 0x5000,
            song_track_qty: 3,
            song_list_offset: 0x56FF,
            song_list_qty: 3,
            patt_ptl_offset: 0x5711,
            patt_pth_offset: 0x573E,
            patt_qty: 45,
            instr_offset: 0x5591,
            instr_qty: 13,
            fx_v1_offset: 0x55F9,
            fx_v1_qty: 16,
            fx_v2_offset: 0,
            fx_v2_qty: 0,
            resetspd: 2,
            // Per-sub-song speed (Ghidra init / oracle note period): song 0 & 2
            // run at ticks-per-row 3 (resetspd 2), song 1 at 4 (resetspd 3).
            resetspd_songs: &[2, 3, 2],
            skydive_v1_when: 2,
            skydive_v1_add: 512,
            // Note 104 overflows RH_FREQ_HEX into the per-voice waveform store
            // (`$54F8` = `DAT_54f8`), so its "freq" = `(v0_ctrl, v1_ctrl)` read as
            // a 16-bit value. The intro voice-1 line reads ~0x4315 (v0 0x15, v1
            // 0x43), so map it to `RH_FREQ_HEX[71]` (0x41b8, freq-high 0x41 — the
            // closest table note, like monty), not 0 (0x0116 = ~6 octaves too
            // low). Verified vs the sidplay regdump. (The first two rows read 0
            // before the voices are set up; a static override can't track that.)
            overflow_overrides: &[(104, 71), (105, 71)],
            overflow_vib: &[],
            fx_layout: SidFxLayout::Rich,
            frame_skip_reset: 0,
            filter_table: 0,
            filter_seed: 0,
            tempo_frac: None,
            arp3_table: 0,
            interp_vibrato: false,
        }
    }

    pub fn get_sid_crazy_comets() -> Self {
        OneSid {
            song: include_bytes!("songs/crazy_comets.sid"),
            name: "Crazy Comets",
            author: "Rob Hubbard",
            copyright: "1985 Martech",
            version: 10,
            load_adress: 0x5000,
            song_track_qty: 3,
            song_list_offset: 0x5732,
            song_list_qty: 2,
            patt_ptl_offset: 0x573E,
            patt_pth_offset: 0x5773,
            patt_qty: 53,
            instr_offset: 0x5574,
            instr_qty: 23,
            fx_v1_offset: 0x562C,
            fx_v1_qty: 1,
            fx_v2_offset: 0,
            fx_v2_qty: 0,
            resetspd: 2,
            resetspd_songs: &[],
            skydive_v1_when: 16,
            skydive_v1_add: -256,
            overflow_overrides: &[],
            overflow_vib: &[],
            fx_layout: SidFxLayout::Rich,
            frame_skip_reset: 0,
            filter_table: 0,
            filter_seed: 0,
            tempo_frac: None,
            arp3_table: 0,
            interp_vibrato: false,
        }
    }

    pub fn get_sid_last_v8() -> Self {
        OneSid {
            song: include_bytes!("songs/last_v8.sid"),
            name: "The Last V8",
            author: "Rob Hubbard",
            copyright: "1985 MAD/Mastertronic",
            version: 10,
            load_adress: 0x8010,
            song_track_qty: 3,
            song_list_offset: 0x8797,
            song_list_qty: 3,
            patt_ptl_offset: 0x87A9,
            patt_pth_offset: 0x87C6,
            patt_qty: 28,
            instr_offset: 0x85A1,
            instr_qty: 19,
            fx_v1_offset: 0x8699,
            fx_v1_qty: 12,
            fx_v2_offset: 0,
            fx_v2_qty: 0,
            resetspd: 1,
            resetspd_songs: &[],
            skydive_v1_when: 0,
            skydive_v1_add: -256,
            // Note 124 (1×) is negligible — last_v8's loud voice 1 (where the
            // oracle is silent, ctrl=0) is a separate note-gating issue, not an
            // overflow-pitch one. Default fixup left in place.
            overflow_overrides: &[],
            overflow_vib: &[],
            fx_layout: SidFxLayout::Rich,
            frame_skip_reset: 0,
            filter_table: 0,
            filter_seed: 0,
            tempo_frac: None,
            arp3_table: 0,
            interp_vibrato: false,
        }
    }

    pub fn get_sid_monty_on_the_run() -> Self {
        OneSid {
            song: include_bytes!("songs/monty_on_the_run.sid"),
            name: "Monty on the Run",
            author: "Rob Hubbard",
            copyright: "1985 Gremlin Graphics",
            version: 10,
            load_adress: 0x8000,
            song_track_qty: 3,
            song_list_offset: 0x856C,
            song_list_qty: 3,
            patt_ptl_offset: 0x857E,
            patt_pth_offset: 0x85CB,
            patt_qty: 77,
            instr_offset: 0x93B4,
            instr_qty: 20,
            fx_v1_offset: 0x9454,
            fx_v1_qty: 16,
            fx_v2_offset: 0,
            fx_v2_qty: 0,
            resetspd: 1,
            resetspd_songs: &[],
            skydive_v1_when: 0,
            skydive_v1_add: -256,
            // Note 104 reads voice_ctrl (0x41) → freq-high 0x41 = `RH_FREQ_HEX[71]`
            // (0x41b8). The shared default 65 (F-5, 0x2e) is ~6 semitones too low.
            overflow_overrides: &[(104, 71)],
            overflow_vib: &[(12, 1344)],
            fx_layout: SidFxLayout::Rich,
            frame_skip_reset: 0,
            filter_table: 0,
            filter_seed: 0,
            tempo_frac: None,
            arp3_table: 0,
            interp_vibrato: false,
        }
    }

    pub fn get_sid_thing_on_a_spring() -> Self {
        OneSid {
            song: include_bytes!("songs/thing_on_a_spring.sid"),
            name: "Thing on a Spring",
            author: "Rob Hubbard",
            copyright: "1985 Gremlin Graphics",
            version: 10,
            load_adress: 0xC000,
            song_track_qty: 3,
            song_list_offset: 0xC509,
            song_list_qty: 1,
            patt_ptl_offset: 0xC50F,
            patt_pth_offset: 0xC533,
            patt_qty: 36,
            instr_offset: 0xCD2A,
            instr_qty: 45,
            fx_v1_offset: 0xCE92,
            fx_v1_qty: 1,
            fx_v2_offset: 0,
            fx_v2_qty: 0,
            resetspd: 1,
            resetspd_songs: &[],
            skydive_v1_when: 0,
            skydive_v1_add: 0,
            // Voice-1 noise drum plays overflow note 96, which the replayer reads
            // from work-RAM as a freq-register high byte of 0x07 (oracle 1792 →
            // 1536 as the drum sweeps it down). Note 33 reproduces that 0x07 start
            // (reg 0x0751); the shared default `96 => 0` would instead start the
            // drum an octave-plus too low (reg 0x0116).
            overflow_overrides: &[(96, 33)],
            overflow_vib: &[],
            fx_layout: SidFxLayout::Rich,
            frame_skip_reset: 0,
            filter_table: 0,
            filter_seed: 0,
            tempo_frac: None,
            arp3_table: 0,
            interp_vibrato: false,
        }
    }

    pub fn get_sid_zoid() -> Self {
        OneSid {
            song: include_bytes!("songs/zoids.sid"),
            name: "Zoids",
            author: "Rob Hubbard",
            copyright: "1986 Martech",
            version: 10,
            load_adress: 0x1000,
            song_track_qty: 3,
            song_list_offset: 0x14FC,
            song_list_qty: 3,
            patt_ptl_offset: 0x150E,
            patt_pth_offset: 0x152D,
            patt_qty: 31,
            instr_offset: 0x147E,
            instr_qty: 15,
            fx_v1_offset: 0,
            fx_v1_qty: 0,
            fx_v2_offset: 0,
            fx_v2_qty: 0,
            // Ghidra (Zoids.sid @ $1000, play $1006): single tempo counter
            // `$146e` reloaded from `$146f` = 2 → note-step every `146f+1 = 3`
            // frames. The hardcoded `1` here mis-set the reload (→ tempo 2),
            // playing Zoids 1.5× too fast (measured gate-attack gaps 24 oracle
            // vs 16 synth). `default_tempo = 1 + resetspd = 3` now matches.
            resetspd: 2,
            resetspd_songs: &[],
            skydive_v1_when: 0,
            skydive_v1_add: 0,
            overflow_overrides: &[],
            overflow_vib: &[],
            fx_layout: SidFxLayout::Rich,
            frame_skip_reset: 0,
            filter_table: 0,
            filter_seed: 0,
            tempo_frac: None,
            arp3_table: 0,
            interp_vibrato: false,
        }
    }

    //------------------------------------------

    pub fn get_sid_ace_2() -> Self {
        OneSid {
            song: include_bytes!("songs/ace_2.sid"),
            name: "ACE II",
            author: "Rob Hubbard",
            copyright: "1987 Arcade",
            version: 20,
            load_adress: 0xE000,
            song_track_qty: 3,
            song_list_offset: 0xE67C,
            song_list_qty: 1,
            patt_ptl_offset: 0xE682,
            patt_pth_offset: 0xE6A8,
            patt_qty: 38,
            instr_offset: 0xE5CB,
            instr_qty: 10,
            fx_v1_offset: 0,
            fx_v1_qty: 0,
            fx_v2_offset: 0,
            fx_v2_qty: 0,
            resetspd: 1,
            resetspd_songs: &[],
            skydive_v1_when: 0,
            skydive_v1_add: 0,
            overflow_overrides: &[],
            overflow_vib: &[],
            fx_layout: SidFxLayout::Rich,
            frame_skip_reset: 0,
            // SID filter table @ $E624 (resfilt @ +6, step @ +7, stride 8);
            // note-on cutoff seed = 48. Ghidra-confirmed (instr 0 = 0xF4/−3).
            filter_table: 0xE624,
            filter_seed: 48,
            // Dual-counter fractional tempo (Ghidra $e08e, Ka=3, R=resetspd=1):
            // note-step on a repeating [3,3,2]-frame pattern = 8/3 frames/row.
            tempo_frac: Some((8, 3)),
            arp3_table: 0,
            interp_vibrato: false,
        }
    }

    pub fn get_sid_delta() -> Self {
        OneSid {
            song: include_bytes!("songs/delta.sid"),
            name: "Delta",
            author: "Rob Hubbard",
            copyright: "1987 Thalamus",
            version: 30, // Compression _and_ pattern loop in channels
            load_adress: 0xBC00,
            song_track_qty: 3,
            song_list_offset: 0xC4F4,
            song_list_qty: 13,
            patt_ptl_offset: 0xC542,
            patt_pth_offset: 0xC5AF,
            patt_qty: 109,
            instr_offset: 0xC38E,
            instr_qty: 22,
            fx_v1_offset: 0,
            fx_v1_qty: 0,
            fx_v2_offset: 0xC43E,
            fx_v2_qty: 22,
            resetspd: 1,
            resetspd_songs: &[],
            skydive_v1_when: 0,
            skydive_v1_add: 0,
            overflow_overrides: &[],
            overflow_vib: &[],
            fx_layout: SidFxLayout::Rich,
            frame_skip_reset: 0,
            // Delta low-passes all three voices (master `$d418 = $1f`, bit4 LP
            // on, vol 15) and sweeps the cutoff per instrument via fx-mask bit 5
            // (`play $c1a4`: `$d416 = base + Σstep`, `$d417 = resfilt`). The
            // per-instrument filter records live in the fx_v2 table (`$c43e`,
            // byte +6 = resfilt, +7 = signed cutoff step) — same layout as the
            // v20 filter table — so point `filter_table` at it. Without this the
            // synth applied no filter at all (bass −62 %, treble +13 % vs the
            // sidplay oracle). New-note resets the cutoff accumulator (`play
            // $beaf`) so seed 0 is correct.
            filter_table: 0xC43E,
            filter_seed: 0,
            // Dual-counter fractional tempo (Ghidra Delta.sid @ $bc00, play
            // $bde4 → $be38: Ka=4 `LDA #$4 STA $c32c`, R=`$c32b`=1) → note-step
            // on a repeating [3,2]-frame pattern = 5/2 = 2.5 frames/row. Uniform
            // `1+resetspd=2` played Delta 1.25× too fast. Voices all ÷2 → the
            // per-lane fold stays aligned. (Same device as ace_2, Ka one higher.)
            tempo_frac: Some((5, 2)),
            arp3_table: 0,
            interp_vibrato: false,
        }
    }

    pub fn get_sid_human_race() -> Self {
        OneSid {
            song: include_bytes!("songs/human_race.sid"),
            name: "The Human Race",
            author: "Rob Hubbard",
            copyright: "1985 Mastertronic",
            version: 20,
            load_adress: 0x0980,
            song_track_qty: 2,
            song_list_offset: 0x0E9F,
            song_list_qty: 3,
            patt_ptl_offset: 0x0EB3,
            patt_pth_offset: 0x0F02,
            patt_qty: 58,
            instr_offset: 0x0DE3,
            instr_qty: 23,
            fx_v1_offset: 0,
            fx_v1_qty: 0,
            fx_v2_offset: 0,
            fx_v2_qty: 0,
            resetspd: 3,
            resetspd_songs: &[],
            skydive_v1_when: 0,
            skydive_v1_add: -256,
            overflow_overrides: &[],
            overflow_vib: &[],
            fx_layout: SidFxLayout::Rich,
            frame_skip_reset: 0,
            filter_table: 0,
            filter_seed: 0,
            tempo_frac: None,
            arp3_table: 0,
            interp_vibrato: false,
        }
    }

    pub fn get_sid_international_karate() -> Self {
        OneSid {
            song: include_bytes!("songs/international_karate.sid"),
            name: "International Karate",
            author: "Rob Hubbard",
            copyright: "1986 System 3",
            version: 20,
            load_adress: 0xAE00,
            song_track_qty: 3,
            song_list_offset: 0xB3B0,
            song_list_qty: 1,
            patt_ptl_offset: 0xB3B6,
            patt_pth_offset: 0xB3EB,
            patt_qty: 53,
            instr_offset: 0xB308,
            instr_qty: 20,
            fx_v1_offset: 0,
            fx_v1_qty: 0,
            fx_v2_offset: 0,
            fx_v2_qty: 0,
            resetspd: 2,
            resetspd_songs: &[],
            skydive_v1_when: 0,
            skydive_v1_add: 0,
            overflow_overrides: &[],
            overflow_vib: &[],
            // play $ae0c opens with `DEC; BPL; LDA #0xa; STA; RTS` → skips
            // 1 frame in 11 (fractional tempo, register-verified).
            // IK uses the SIMPLE (v10-like) fx layout: bit0=drum, bit2=arp(−5),
            // bit3=PW-low-byte-mode — NOT the rich wave-alt/vib/two-phase scheme.
            fx_layout: SidFxLayout::Simple,
            frame_skip_reset: 0,
            filter_table: 0,
            filter_seed: 0,
            tempo_frac: Some((33, 10)),
            arp3_table: 0,
            interp_vibrato: false,
        }
    }

    pub fn get_sid_lightforce() -> Self {
        OneSid {
            song: include_bytes!("songs/lightforce.sid"),
            name: "Lightforce",
            author: "Rob Hubbard",
            copyright: "1986 Faster Than Light (FTL)",
            // v25 = the v20 note/fx/portamento engine, but a later replayer
            // revision whose note-setup ZEROES the filter cutoff accumulator
            // each note (cutoff pulses per note, like Delta v30) instead of
            // sweeping continuously (Ace 2, true v20). Verified vs the sidplay
            // oracle. The v20-family fx decode (`(20..30)` below) still applies;
            // only the per-note filter reset (`version >= 25`) is gained.
            version: 25,
            load_adress: 0xF000,
            song_track_qty: 3,
            song_list_offset: 0xF778,
            song_list_qty: 1,
            patt_ptl_offset: 0xF77E,
            patt_pth_offset: 0xF79D,
            patt_qty: 31,
            instr_offset: 0xF618,
            instr_qty: 22,
            fx_v1_offset: 0,
            fx_v1_qty: 0,
            fx_v2_offset: 0xF6C8,
            fx_v2_qty: 22,
            resetspd: 2,
            resetspd_songs: &[],
            skydive_v1_when: 0,
            skydive_v1_add: 0,
            overflow_overrides: &[],
            overflow_vib: &[],
            fx_layout: SidFxLayout::Rich,
            frame_skip_reset: 0,
            // Lightforce filter: per-instrument records @ $f6c8 (= fx_v2_offset),
            // stride 8, resfilt @ +6 (`f49b: LDA $f6ce,Y; STA $D417`), signed
            // cutoff-hi step @ +7 (`f492: ADC $f6cf,Y; STA $D416`), accumulator
            // `$f605,X` seeded to 0 on note-on (`f186`). The engine gates the
            // filter write on bit5 of the instrument's flags byte (`f487: LDA
            // $f61f,Y; AND #$20`); the {1,2,4,7,15,16,18} instruments that set
            // it are EXACTLY those with nonzero resfilt/step in $f6c8, so the
            // generic `resfilt!=0||step!=0` enable selects the same set.
            filter_table: 0xF6C8,
            filter_seed: 0,
            // Lightforce.sid @ $f000, play $f0bf: DUAL counter like ace_2/delta.
            // A (`$f5e2`) reloads to 6 (`f113`), B (`$f5e0`) reloads to
            // `$f5e1`=2 (`f120`); A counts every frame, B every frame EXCEPT
            // A's reload frame (1 in 7). Note-step gate `f12d: LDA A; BEQ skip;
            // LDA B; CMP reloadB; BNE skip` fires only when `A!=0 && B==2`.
            // Simulated → steps on a repeating [3,4]-frame pattern = 7/2 = 3.5
            // frames/row. Uniform `1+resetspd=3` ran it 16.7% too fast. All 3
            // voices step on the same global frames (A/B are global) and their
            // passes are even-length → fold-aligned (den=2).
            tempo_frac: Some((7, 2)),
            arp3_table: 0xF60C,
            interp_vibrato: false,
        }
    }

    pub fn get_sid_sanxion_song_1() -> Self {
        OneSid {
            song: include_bytes!("songs/sanxion.sid"),
            name: "Sanxion Song 1",
            author: "Rob Hubbard",
            copyright: "1986 Thalamus",
            version: 20,
            load_adress: 0xB000,
            song_track_qty: 3,
            song_list_offset: 0xB73C,
            song_list_qty: 1,
            patt_ptl_offset: 0xB742,
            patt_pth_offset: 0xB75D,
            patt_qty: 27,
            instr_offset: 0xB56C,
            instr_qty: 29,
            fx_v1_offset: 0,
            fx_v1_qty: 0,
            fx_v2_offset: 0xB654,
            fx_v2_qty: 29,
            resetspd: 2,
            resetspd_songs: &[],
            skydive_v1_when: 0,
            skydive_v1_add: 0,
            overflow_overrides: &[],
            overflow_vib: &[],
            fx_layout: SidFxLayout::Rich,
            frame_skip_reset: 0,
            filter_table: 0,
            filter_seed: 0,
            tempo_frac: None,
            arp3_table: 0,
            interp_vibrato: false,
        }
    }

    pub fn get_sid_sanxion_song_2() -> Self {
        OneSid {
            song: include_bytes!("songs/sanxion.sid"),
            name: "Sanxion Song 2",
            author: "Rob Hubbard",
            copyright: "1986 Thalamus",
            version: 20,
            load_adress: 0xB000,
            song_track_qty: 3,
            song_list_offset: 0xC5F5,
            song_list_qty: 1,
            patt_ptl_offset: 0xC5FB,
            patt_pth_offset: 0xC633,
            patt_qty: 56,
            instr_offset: 0xC4B5,
            instr_qty: 20,
            fx_v1_offset: 0,
            fx_v1_qty: 0,
            fx_v2_offset: 0xC5F5,
            fx_v2_qty: 20,
            resetspd: 2,
            resetspd_songs: &[],
            skydive_v1_when: 0,
            skydive_v1_add: 0,
            overflow_overrides: &[],
            overflow_vib: &[],
            fx_layout: SidFxLayout::Rich,
            frame_skip_reset: 0,
            filter_table: 0,
            filter_seed: 0,
            tempo_frac: None,
            arp3_table: 0,
            interp_vibrato: false,
        }
    }

    pub fn get_sid_spellbound() -> Self {
        OneSid {
            song: include_bytes!("songs/spellbound.sid"),
            name: "Spellbound",
            author: "Rob Hubbard",
            copyright: "1986 MAD/Mastertronic",
            version: 15, // Spellbound variant: byte 2 is always instrument (never portamento), different vibrato/pulse bit layout
            load_adress: 0xE000,
            song_track_qty: 3,
            song_list_offset: 0xE6B6,
            song_list_qty: 3,
            patt_ptl_offset: 0xE6C8,
            patt_pth_offset: 0xE6F2,
            patt_qty: 42,
            instr_offset: 0xE548,
            instr_qty: 25,
            fx_v1_offset: 0xE610,
            fx_v1_qty: 16,
            fx_v2_offset: 0,
            fx_v2_qty: 0,
            resetspd: 1,
            resetspd_songs: &[],
            skydive_v1_when: 0,
            // v15 fx bit1 = note-index rise +1 semitone every 4 frames (Ghidra
            // `sb_play` $e012 `(DAT_00be&2) && (DAT_00c1&3)==0 → note++`), i.e. the
            // `skydive_climb` model — NOT a freq-register slide. v15 has no upward
            // freq-add skydive (its bit0 down-slides the freq-hi byte instead), so
            // `add=0` routes bit1 to the climb. The previous 256 made instr-17's
            // lead run away upward (a rising whine) instead of arpeggiating.
            skydive_v1_add: 0,
            overflow_overrides: &[],
            overflow_vib: &[],
            // Ghidra (Spellbound.sid @ $e000, play $e012): the play entry opens
            // with a frame-skip `DEC $c2; BPL .ok; LDA #$0A; STA $c2; RTS` —
            // it does NOTHING once every 11 frames (reset 10), so the whole tune
            // (note-fetch AND per-frame effects) runs at 10/11 of 50 Hz. We were
            // missing it (0) → Spellbound played ~1.1× too fast (gate gaps 16/8
            // synth vs 17/9 oracle). 10 = 1-in-11, like International Karate.
            fx_layout: SidFxLayout::Rich,
            frame_skip_reset: 0,
            filter_table: 0,
            filter_seed: 0,
            tempo_frac: Some((11, 5)),
            // v15 vibrato = centered period-interpolation (Ghidra `sb_play`: byte +5,
            // half-depth `(b5&0x78)>>3`, shift `b5&7`) — the SAME mechanism Thrust uses.
            // Routes the v15 vibrato through the centered `interp_vib` path instead of
            // the upward `vib_depth` model (see the `version != 15` guard above).
            arp3_table: 0,
            interp_vibrato: true,
        }
    }

    pub fn get_sid_thrust() -> Self {
        OneSid {
            song: include_bytes!("songs/thrust.sid"),
            name: "Thrust",
            author: "Rob Hubbard",
            copyright: "1986 Firebird",
            version: 10,
            load_adress: 0x0800,
            song_track_qty: 3,
            song_list_offset: 0x0dd7,
            song_list_qty: 1,
            patt_ptl_offset: 0x0ddd,
            patt_pth_offset: 0x0e04,
            patt_qty: 39,
            instr_offset: 0x0cf7,
            instr_qty: 27,
            fx_v1_offset: 0,
            fx_v1_qty: 0,
            fx_v2_offset: 0,
            fx_v2_qty: 0,
            resetspd: 2,
            resetspd_songs: &[],
            skydive_v1_when: 0,
            skydive_v1_add: 0, // Thrust skydive uses note increment, not freq add
            overflow_overrides: &[],
            overflow_vib: &[],
            // play $0822 opens with `DEC; BPL; LDA #0x9; STA; RTS` → skips
            // 1 frame in 10 (fractional tempo, register-verified).
            fx_layout: SidFxLayout::Rich,
            // Kept on `frame_skip_reset` (bpm-scale), NOT `tempo_frac`: the
            // fractional cadence's `⌈⌉` frame placement fought Thrust's per-frame
            // interp-vibrato → freq-hi regressed (315→866). IK / Spellbound (no
            // per-frame freq effect) DID move to tempo_frac and improved.
            frame_skip_reset: 9,
            filter_table: 0,
            filter_seed: 0,
            tempo_frac: None,
            // Thrust's instruments use the instr+5 period-interpolation vibrato
            // (`play $09f1`); its intro instrument 26 (b5=0x30) is a ±3-semitone
            // swing rendered as a dead static note without it.
            arp3_table: 0,
            interp_vibrato: true,
        }
    }
}
