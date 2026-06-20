//! Amiga MOD / ProTracker file loader — entry point for the
//! `import_mod` feature. Parses the 1084-byte fixed header, the
//! 31- or 15-sample variants, the order table, the raw 64-row
//! patterns, and the trailing sample payloads, then produces a
//! `Module` populated with `crate::tracker::profiles::pt()` quirks.

use super::amiga_sample::AmigaSample;
use super::patternslot::PatternSlot;
use crate::core::cell_note::CellNote;
use crate::tracker::import::bin_reader::ImportError;

use crate::core::fixed::units::Volume;
use crate::prelude::*;
use crate::tracker::codepage::Codepage;
use crate::tracker::import::memory::ImportMemory;
use crate::tracker::import::memory::MemoryType;

use alloc::format;
use alloc::string::String;
use alloc::string::ToString;
use alloc::{vec, vec::Vec};

/// Length, in bytes, of the fixed header preceding pattern data
/// for each MOD variant. 600 = 20 (title) + 15 × 30 (samples) +
/// 1 + 1 + 128 (song bytes); 1084 = 20 + 31 × 30 + 1 + 1 + 128 + 4
/// (tag).
const MOD_15_HEADER_SIZE: usize = 600;
const MOD_31_HEADER_SIZE: usize = 1084;
const AMIGA_SAMPLE_RECORD_SIZE: usize = 30;
const POSITION_TABLE_SIZE: usize = 128;
const AMIGA_ROWS_PER_PATTERN: usize = 64;
const AMIGA_SLOT_SIZE: usize = 4;

/// Map a 4-character format tag to its channel count, returning
/// `None` for tags we don't recognise (which the loader treats as
/// "no tag" — i.e., 15-sample Soundtracker layout). Centralised so
/// `get_number_of_tracks` and the variant-detection path share one
/// truth table.
fn tag_str_to_num_tracks(tag: &str) -> Option<u8> {
    match tag {
        "TDZ1" => Some(1),
        "2CHN" | "TDZ2" => Some(2),
        "TDZ3" => Some(3),
        "M.K." | "M!K!" | "FLT4" | "NSMS" | "LARD" | "PATT" | "EXO4" | "N.T." | "M&K!" | "FEST"
        | "CD61" => Some(4),
        "5CHN" => Some(5),
        "6CHN" => Some(6),
        "7CHN" => Some(7),
        "8CHN" | "CD81" | "OKTA" | "OCTA" | "FLT8" | "EXO8" => Some(8),
        "9CHN" => Some(9),
        t if t.ends_with("CH") || t.ends_with("CN") => {
            match t[..t.len() - 2].parse::<u8>().unwrap_or(0) {
                0 => None,
                v => Some(v),
            }
        }
        _ => None,
    }
}

/// Per-tracker decode quirks selected by the 4-character format tag.
///
/// The channel count is already resolved by `tag_str_to_num_tracks`;
/// these flags capture behaviours that change how the *pattern bytes
/// and sample headers are interpreted*, not just how wide a pattern
/// is. They are derived from observable `.mod` format behaviour
/// (independently re-expressed here, not transcribed from any
/// player's source).
#[derive(Copy, Clone, Debug, Default)]
struct ModDialect {
    /// NoiseTracker family (`N.T.`, plus the His Master's Noise tags
    /// `M&K!` / `FEST` which descend from it): a `Dxx` pattern break
    /// ignores its parameter and always lands on row 0.
    is_noisetracker: bool,
    /// His Master's Noise (`M&K!` / `FEST`): the sample finetune byte
    /// is the full 8-bit value taken as `-(byte << 3)` through an
    /// `i8`, not the standard signed low nibble shifted into an `i8`.
    is_hmnt: bool,
    /// StarTrekker / Exolon (`FLT4` `FLT8` `EXO4` `EXO8`): the `Exx`
    /// slot triggers assembly macros rather than the ProTracker
    /// extended-command set, and the tracker caps ticks-per-row at 31.
    is_startrekker: bool,
    /// StarTrekker 8-voice layout (`FLT8` / `EXO8`): each logical
    /// 8-channel pattern is stored on disk as two consecutive
    /// 4-channel patterns (channels 0-3 then 4-7), and order entries
    /// count stored patterns, so they are halved.
    is_flt8: bool,
}

/// Resolve the decode quirks for a format tag.
fn tag_dialect(tag: &str) -> ModDialect {
    let is_hmnt = matches!(tag, "M&K!" | "FEST");
    ModDialect {
        is_noisetracker: is_hmnt || tag == "N.T.",
        is_hmnt,
        is_startrekker: matches!(tag, "FLT4" | "FLT8" | "EXO4" | "EXO8"),
        is_flt8: matches!(tag, "FLT8" | "EXO8"),
    }
}

/// Apply tag-driven decode quirks to one freshly-parsed pattern slot.
fn apply_mod_dialect(slot: &mut PatternSlot, dialect: &ModDialect) {
    const PATTERN_BREAK: u8 = 0x0D;
    const EXTENDED: u8 = 0x0E;
    const SET_SPEED: u8 = 0x0F;
    if dialect.is_noisetracker && slot.effect_type == PATTERN_BREAK {
        slot.effect_parameter = 0;
    }
    if dialect.is_startrekker {
        if slot.effect_type == EXTENDED {
            // Assembly-macro trigger, not ProTracker `Exx` — drop it.
            slot.effect_type = 0;
            slot.effect_parameter = 0;
        } else if slot.effect_type == SET_SPEED && slot.effect_parameter > 0x1F {
            slot.effect_parameter = 0x1F;
        }
    }
}

/// SoundTracker family sophistication ordering for the untagged
/// 15-sample variants. Higher = a later, more capable tracker. The
/// `<= Ust1_80` boundary selects the Ultimate Soundtracker effect
/// interpretation, where commands 1 and 2 are arpeggio and pitch-bend
/// rather than the ProTracker portamentos, and only commands 0-2 do
/// anything at all. The fingerprints used to place a file on this
/// scale (sample-size limits, disk-label sample names, a non-default
/// tempo byte, and which effect commands appear) are observable facts
/// of the format, re-expressed here independently.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum StkVersion {
    Ust1_00,
    Ust1_80,
    St2_00Exterminator,
    StIii,
    StIx,
    Mst1_00,
    St2_00,
}

/// Rewrite one 15-sample SoundTracker pattern slot's effect into the
/// equivalent ProTracker `(command, parameter)` that the shared
/// pattern→`TrackEffect` mapper already understands.
fn remap_stk_slot(slot: &mut PatternSlot, version: StkVersion, use_auto_slides: bool) {
    let mut command = slot.effect_type & 0x0F;
    let mut param = slot.effect_parameter;
    if command == 0 && param == 0 {
        return;
    }

    // Command fixups that apply regardless of the UST / standard split.
    if command == 0x0D {
        if version != StkVersion::St2_00 {
            // Pre-2.0 SoundTracker: Dxy is a volume slide, not a break.
            command = 0x0A;
        } else {
            param = 0; // pattern break ignores its parameter
        }
    } else if command == 0x0C {
        param &= 0x7F; // volume byte; the chip ignores the top bit
    } else if command == 0x0E && (param > 0x01 || version < StkVersion::StIx) && use_auto_slides {
        // Auto volume-slide. xmrs has no continuous-slide primitive, so
        // approximate with a per-row volume slide (Axx).
        slot.effect_type = 0x0A;
        slot.effect_parameter = param;
        return;
    } else if command == 0x0F {
        param &= 0x0F; // only the low nibble is the tick speed
    }

    if version <= StkVersion::Ust1_80 {
        // Ultimate Soundtracker effect set.
        match command {
            0 => {
                if param < 0x03 {
                    // Too small to be an arpeggio — nothing.
                    command = 0;
                    param = 0;
                }
                // else: arpeggio (command 0 already)
            }
            1 => {
                // Arpeggio, not portamento up.
                command = 0;
            }
            2 => {
                if param & 0x0F != 0 {
                    command = 1; // portamento up by the low nibble
                    param &= 0x0F;
                } else if param >> 4 != 0 {
                    command = 2; // portamento down by the high nibble
                    param >>= 4;
                } else {
                    command = 0;
                    param = 0;
                }
            }
            _ => {
                // All other commands do nothing in Ultimate Soundtracker.
                command = 0;
                param = 0;
            }
        }
    }

    slot.effect_type = command;
    slot.effect_parameter = param;
}

/// `Some(num_tracks)` if the four bytes spell a recognised tag.
/// Restricted to ASCII so we never pattern-match against bytes
/// produced by `String::from_utf8_lossy` substituting U+FFFD.
fn tag_bytes_to_num_tracks(tag: &[u8]) -> Option<u8> {
    if tag.len() != 4 {
        return None;
    }
    let s = core::str::from_utf8(tag).ok()?;
    tag_str_to_num_tracks(s)
}

/// True when every byte is plausibly part of a fixed-width name
/// field (title or sample name).
///
/// MOD files in the wild carry names in any of several 8-bit
/// encodings — plain ASCII most often, but scene composers
/// routinely embed **CP437 box-drawing and block glyphs**
/// (`0xB0..=0xDF`) for ASCII-art logos in the sample-name area,
/// and European authors use **Latin-1 / ISO-8859-* accented
/// letters** (`0x80..=0xFF`). The original "NUL or printable
/// ASCII" rule rejected all of those, so legitimate files like
/// GURU.MOD (Scorpik / Surprise! Productions) were misidentified
/// as non-MOD.
///
/// Since the file has no encoding declaration, we can't tell
/// CP437 from Latin-1 from MacRoman — so we accept every high
/// byte and reject only what's certainly not text:
///
/// * **C0 control bytes** `0x01..=0x1F` (NUL is allowed as
///   padding),
/// * **DEL** `0x7F`.
///
/// Per-byte pass rate against a uniform-random blob is
/// `224/256 ≈ 87.5 %`; over the 31×22 = 682 bytes of sample
/// names in a 31-sample MOD that's `~10⁻⁴⁰`, still vastly
/// overshadowed (and complemented) by the `volume ≤ 64` and
/// `finetune ≤ 15` range checks that follow.
fn is_clean_text(bytes: &[u8]) -> bool {
    bytes.iter().all(|&b| !matches!(b, 0x01..=0x1F | 0x7F))
}

/// Which MOD layout the file matches.
#[derive(Copy, Clone, Debug)]
enum AmigaVariant {
    /// 15-sample Ultimate Soundtracker era. 600-byte header,
    /// no tag at offset 0x438 (those bytes are pattern data),
    /// always 4 channels.
    Fifteen,
    /// 31-sample MOD with a recognised tag at offset 0x438.
    /// 1084-byte header, channel count given by the tag.
    ThirtyOne { num_tracks: u8 },
}

#[derive(Default, Debug)]
pub struct AmigaModule {
    title: String,
    samples: Vec<AmigaSample>, // 15 or 31
    song_length: u8,
    restart_position: u8,
    positions: Vec<u8>, // 128
    tag: String,
    patterns: Vec<Vec<Vec<PatternSlot>>>, // pattern, row, element
    audio: Vec<Vec<i8>>,
    /// His Master's Noise (`M&K!` / `FEST`): selects the inverted
    /// `-(byte << 3)` finetune interpretation in `to_instr`.
    is_hmnt: bool,
}

impl AmigaModule {
    fn get_number_of_tracks(&self) -> Option<u8> {
        tag_str_to_num_tracks(self.tag.as_str())
    }

    fn get_number_of_samples(&self) -> usize {
        match self.get_number_of_tracks() {
            None => 15,
            _ => 31,
        }
    }

    fn get_number_of_patterns(&self) -> usize {
        1 + *self.positions.iter().max().unwrap_or(&0) as usize
    }

    /// True when a `M.K.`-tagged file is really a "Mod's Grave" `.wow`
    /// — a Composer-669-to-MOD conversion that keeps the 4-channel
    /// magic but stores 8 channels of pattern data. Identified purely
    /// by the file-size arithmetic closing for an 8-channel reading,
    /// gated by the header constraints a 669 conversion always meets:
    /// no sample finetune, full default volume on every used sample,
    /// and no restart position. The 8-channel size never coincides
    /// with a genuine 4-channel `M.K.` file — that would require the
    /// pattern region to be zero bytes — so the equality is decisive.
    fn is_mods_grave(&self, file_len: usize) -> bool {
        if self.tag != "M.K." || self.restart_position != 0 {
            return false;
        }
        let mut total_sample_bytes = 0usize;
        for s in &self.samples {
            if s.finetune != 0 {
                return false;
            }
            if s.length_div2 != 0 && s.volume != 64 {
                return false;
            }
            total_sample_bytes += 2 * s.length_div2 as usize;
        }
        let num_patterns = self.get_number_of_patterns();
        let eight_ch_bytes = 8 * AMIGA_ROWS_PER_PATTERN * AMIGA_SLOT_SIZE; // 2048
                                                                           // MOD files cannot have an odd length; round down before
                                                                           // comparing (some .wow files carry one trailing byte).
        MOD_31_HEADER_SIZE + total_sample_bytes + num_patterns * eight_ch_bytes == (file_len & !1)
    }

    /// Estimate which SoundTracker (15-sample) authored this file, from
    /// the same observable fingerprints the format is known to carry:
    /// sample-size limits, disk-label sample names, the tempo byte, and
    /// the set of effect commands actually used. Returns the version
    /// plus whether the auto-volume-slide (`Exx`) behaviour is active.
    /// Run after patterns are parsed (the scan inspects every slot).
    fn detect_stk_version(&self) -> (StkVersion, bool) {
        use StkVersion::*;
        let mut version = Ust1_00;
        // True while no sample carries a disk-label name (`st-NN:`),
        // which Ultimate Soundtracker 1.8 / D.O.C. IX always do.
        let mut no_disk_names = true;

        for s in &self.samples {
            let n = s.name.as_bytes();
            let is_disk = n.len() >= 6 && n[..3].eq_ignore_ascii_case(b"st-") && n[5] == b':';
            if is_disk {
                no_disk_names = false;
            }
            // Ultimate Soundtracker is capped at ~10 KB samples; larger
            // ones imply Master Soundtracker 1.0 or later.
            if s.length_div2 > 4999 || s.repeat_offset_div2 > 9999 {
                version = version.max(Mst1_00);
            }
        }

        // The restart byte doubles as a default-tempo store; 0 means the
        // 0x78 (120 BPM) default. Anything else implies variable-tempo
        // support, which only later trackers had.
        let restart = if self.restart_position == 0 {
            0x78
        } else {
            self.restart_position
        };
        if restart != 0x78 {
            if version > Ust1_80 {
                version = version.max(if no_disk_names { StIx } else { Mst1_00 });
            } else {
                version = version.max(if no_disk_names {
                    Ust1_80
                } else {
                    St2_00Exterminator
                });
            }
        }

        // Pattern scan. Mutations are sequential and order-sensitive:
        // some effect fingerprints pull the estimate back down to UST.
        let num_patterns = self.get_number_of_patterns();
        let mut use_auto_slides = false;
        let mut total_num_dxx: u32 = 0;
        for pat in &self.patterns {
            let mut num_dxx: u32 = 0;
            let mut auto_slides: u32 = 0;
            let mut empty_run: u32 = 0;
            for (row_idx, row) in pat.iter().enumerate() {
                for slot in row {
                    let eff = slot.effect_type & 0x0F;
                    let param = slot.effect_parameter;
                    let is_empty = matches!(slot.note, CellNote::Empty)
                        && slot.instrument.is_none()
                        && slot.effect_type == 0
                        && slot.effect_parameter == 0;
                    // A long run of empty cells after a Dxx suggests the
                    // Dxx really was a pattern break (latest versions).
                    if empty_run != 0 && is_empty {
                        empty_run += 1;
                        if empty_run > 32 {
                            version = St2_00;
                        }
                    } else {
                        empty_run = 0;
                    }

                    match eff {
                        1 | 2 => {
                            if param > 0x1F && version == Ust1_80 {
                                version = if no_disk_names { Ust1_80 } else { Ust1_00 };
                            } else if eff == 1 && (1..0x03).contains(&param) {
                                // A 1xy with a tiny parameter is not an arpeggio.
                                version = version.max(St2_00Exterminator);
                            } else if eff == 1
                                && (param == 0x37 || param == 0x47)
                                && version <= St2_00Exterminator
                            {
                                // Strongly arpeggio-shaped → pull back to UST.
                                version = if no_disk_names { Ust1_80 } else { Ust1_00 };
                            }
                        }
                        0x0B => version = St2_00,
                        0x0C..=0x0E => {
                            version = version.max(St2_00Exterminator);
                            if eff == 0x0D {
                                empty_run = 1;
                                // A D00 on row 0 is almost certainly a
                                // tracking slip, not a real break.
                                if !(param == 0 && row_idx == 0) {
                                    num_dxx += 1;
                                }
                            } else if eff == 0x0E {
                                auto_slides += 1;
                                if param > 1 || auto_slides > 1 {
                                    use_auto_slides = true;
                                }
                            }
                        }
                        0x0F => version = version.max(StIii),
                        _ => {}
                    }
                }
            }
            // A handful of Dxx in a pattern reads as deliberate breaks.
            if (1..3).contains(&num_dxx) {
                version = St2_00;
            }
            total_num_dxx += num_dxx;
        }
        // Conversely, a flood of Dxx can't be pattern breaks.
        if total_num_dxx > num_patterns as u32 + 32 && version == St2_00 {
            version = Mst1_00;
        }

        (version, use_auto_slides)
    }

    /// Decide which MOD variant `data` matches, or refuse it. Run
    /// before any byte-level parsing so non-MOD blobs fail fast
    /// instead of being silently accepted as 15-sample garbage.
    /// Neither variant has a magic signature, so the verdict comes
    /// from header field ranges plus a file-size equation —
    /// each on its own is weak, but the conjunction is decisive.
    fn detect_variant(data: &[u8]) -> Result<AmigaVariant, ImportError> {
        // Try the high-confidence path first: the bytes at 0x438
        // must spell a recognised tag AND the rest of the structure
        // must validate against that variant. If both hold we
        // commit to 31-sample without considering 15-sample at all.
        if data.len() >= MOD_31_HEADER_SIZE {
            if let Some(num_tracks) = tag_bytes_to_num_tracks(&data[0x438..0x438 + 4]) {
                let v = AmigaVariant::ThirtyOne { num_tracks };
                if Self::validate_structure(data, v).is_ok() {
                    return Ok(v);
                }
            }
        }
        // Fall back to 15-sample. The Soundtracker layout has no
        // tag at all (offset 0x438 is inside pattern data), so the
        // structural checks are the only available evidence.
        if Self::validate_structure(data, AmigaVariant::Fifteen).is_ok() {
            return Ok(AmigaVariant::Fifteen);
        }
        Err(ImportError::Other("Not an Amiga MOD module"))
    }

    /// Cheap range / size sanity checks for one variant. Each
    /// individual check (volume ≤ 64, finetune ≤ 15, ASCII-only
    /// names, file size ≥ declared content) is weak alone but
    /// cumulatively rejects a uniform-random binary blob with
    /// overwhelming probability — across 15 sample slots
    /// `volume ≤ 64` × `finetune ≤ 15` is on the order of 1e-27.
    fn validate_structure(data: &[u8], variant: AmigaVariant) -> Result<(), ImportError> {
        let (num_samples, num_tracks, header_size, tagged) = match variant {
            AmigaVariant::Fifteen => (15usize, 4u8, MOD_15_HEADER_SIZE, false),
            AmigaVariant::ThirtyOne { num_tracks } => (31, num_tracks, MOD_31_HEADER_SIZE, true),
        };

        if data.len() < header_size {
            return Err(ImportError::Other("File too short for MOD header"));
        }

        // Name-field cleanliness is *only* load-bearing for the
        // untagged 15-sample variant. A recognised 4-byte tag at
        // 0x438 (`M.K.`, `FLT4`, `8CHN`, …) is already a ~1-in-2³²
        // signature, and real scene 31-sample MODs routinely leave
        // control bytes (0x01..=0x1F) in sample-name slots — strings
        // like effect cues or just uninitialised memory — so demanding
        // clean text there spuriously rejects genuine files. With no
        // tag the 15-sample layout has no anchor, so its names stay
        // load-bearing evidence.
        if !tagged && !is_clean_text(&data[..20]) {
            return Err(ImportError::Other("MOD title is not ASCII"));
        }

        // u32 is comfortably enough for every quantity below.
        // Worst-case Amiga MOD: 31 samples × `2 × u16::MAX` bytes ≈
        // 4 MB of sample data; 128 patterns × 32 ch × 64 rows × 4 B
        // ≈ 1 MB of pattern data. Well below `u32::MAX = 4 GB`.
        // `saturating_*` defends against the unlikely fuzzed input
        // where `length_div2 = u16::MAX` on every slot anyway.
        let mut total_sample_bytes: u32 = 0;
        for i in 0..num_samples {
            let off = 20 + i * AMIGA_SAMPLE_RECORD_SIZE;
            if !tagged && !is_clean_text(&data[off..off + 22]) {
                return Err(ImportError::Other("MOD sample name is not ASCII"));
            }
            let length_div2 = u16::from_be_bytes([data[off + 22], data[off + 23]]);
            // The finetune ≤ 15 / volume ≤ 64 range checks are evidence
            // for the untagged 15-sample path only. Real tagged MODs do
            // carry out-of-spec bytes here — ProTracker masks finetune
            // to its low nibble (`to_sample` does the same `<< 4`), so a
            // slot with finetune 0x20 is a valid file, not garbage.
            if !tagged {
                let finetune = data[off + 24];
                let volume = data[off + 25];
                if finetune > 0x0F {
                    return Err(ImportError::Other("MOD sample finetune > 15"));
                }
                if volume > 0x40 {
                    return Err(ImportError::Other("MOD sample volume > 64"));
                }
            }
            total_sample_bytes = total_sample_bytes.saturating_add(2 * length_div2 as u32);
        }

        let song_length_off = 20 + num_samples * AMIGA_SAMPLE_RECORD_SIZE;
        let song_length = data[song_length_off];
        if song_length == 0 || song_length > 128 {
            return Err(ImportError::Other("MOD song_length out of range"));
        }

        let positions_off = song_length_off + 2;
        let positions = &data[positions_off..positions_off + POSITION_TABLE_SIZE];
        // `load` reads `1 + max(all 128 positions)` patterns (via
        // `get_number_of_patterns`), so the safety check below must use
        // that same maximum, not just the first `song_length` entries —
        // otherwise a high padding byte could make `load` read past the
        // file. Historical trackers zero the padding, so this is the
        // same value in practice, but matching `load` exactly keeps the
        // pattern read provably in-bounds.
        let max_pos = *positions.iter().max().unwrap_or(&0);
        if max_pos >= 128 {
            return Err(ImportError::Other("MOD position byte out of range"));
        }

        // Patterns are read with direct 4-byte indexing in `load`, so
        // every pattern byte MUST be present or the parse would panic;
        // this is a hard requirement for both variants. Sample data,
        // by contrast, is clamped to the bytes that remain (the
        // COUNTRY.MOD hack in `load`), so a file whose final sample is
        // truncated still loads cleanly — common in the wild.
        let num_patterns: u32 = max_pos as u32 + 1;
        // StarTrekker 8-voice (`FLT8` / `EXO8`) stores its 8 channels
        // as two stacked 4-channel patterns, so its on-disk pattern
        // region is `(num_patterns + 1)` *4-channel* blocks — narrower
        // and one longer than a flat `num_tracks`-wide reading. Sizing
        // it as 8-wide here wrongly rejected genuine FLT8 files as "too
        // short". Mirror `load`'s actual byte consumption per layout.
        let is_flt8 = tagged
            && matches!(
                core::str::from_utf8(&data[0x438..0x438 + 4]),
                Ok("FLT8") | Ok("EXO8")
            );
        let block: u32 = AMIGA_ROWS_PER_PATTERN as u32 * AMIGA_SLOT_SIZE as u32;
        let pattern_bytes: u32 = if is_flt8 {
            num_patterns.saturating_add(1).saturating_mul(4 * block)
        } else {
            num_patterns.saturating_mul(num_tracks as u32 * block)
        };
        let patterns_end: u32 = (header_size as u32).saturating_add(pattern_bytes);
        if patterns_end as usize > data.len() {
            return Err(ImportError::Other("MOD file shorter than declared content"));
        }

        // For the untagged 15-sample variant the full size equation is
        // the main evidence it's really a MOD (no tag to anchor on), so
        // there we additionally require all declared sample bytes to be
        // present. The tagged variant relies on its tag instead and
        // tolerates a truncated tail.
        if !tagged {
            let expected = patterns_end.saturating_add(total_sample_bytes);
            if expected as usize > data.len() {
                return Err(ImportError::Other("MOD file shorter than declared content"));
            }
        }

        Ok(())
    }

    pub fn load(ser_amiga_module: &[u8]) -> Result<AmigaModule, ImportError> {
        // Refuse non-MOD garbage up-front. Without this the loader
        // would happily decode any sufficiently-long binary as a
        // 15-sample MOD because that branch has no signature to
        // anchor on — we'd return a `Module` filled with noise and
        // the auto-detect path in `Module::load` would never get a
        // chance to escalate to a different format.
        let variant = Self::detect_variant(ser_amiga_module)?;

        let mut amiga = AmigaModule {
            ..Default::default()
        };

        // ---- codepage detection ----------------------------
        //
        // MOD has no encoding declaration; the same byte (`0xDF`)
        // can be the CP437 block glyph `▀` or the Latin-1 letter
        // `ß` depending on which tracker authored the file. We
        // pool every text field on the file — the 20-byte title
        // plus all 15 or 31 22-byte sample names — and run the
        // detector once over the whole pool. Pooling gives a far
        // more reliable verdict than per-field detection because
        // a single 22-byte slot rarely carries enough signal to
        // commit on its own. The format tag at `0x438` is always
        // pure ASCII (`M.K.`, `FLT8`, `OCTA`, …) so it
        // contributes no signal and we leave it out of the pool.
        let num_samples = match variant {
            AmigaVariant::Fifteen => 15,
            AmigaVariant::ThirtyOne { .. } => 31,
        };
        let title_bytes = &ser_amiga_module[0..20];
        let mut text_fields: Vec<&[u8]> = Vec::with_capacity(1 + num_samples);
        text_fields.push(title_bytes);
        for i in 0..num_samples {
            let off = 0x14 + i * 30;
            text_fields.push(&ser_amiga_module[off..off + 22]);
        }
        let codepage = Codepage::detect_from_fields(&text_fields);

        // Decode title with the detected codepage. (Note: the
        // previous implementation read 22 bytes here, which
        // included the first 2 bytes of the next field — the
        // first sample's name. Corrected to the spec's 20.)
        amiga.title = codepage.decode_name(title_bytes);

        // Tag only exists in the 31-sample layout. For the 15-sample
        // layout offset 0x438 is inside the first pattern's data, so
        // reading it produces noise that would mis-trigger
        // `tag_str_to_num_tracks` if we ever fed it back through the
        // existing match arms. Leave the tag empty so
        // `get_number_of_samples` returns 15 by the `None` arm.
        // The tag bytes are always pure ASCII, so the codepage
        // choice doesn't matter — we still route through the
        // detected codepage for consistency.
        amiga.tag = match variant {
            AmigaVariant::ThirtyOne { .. } => {
                codepage.decode_name(&ser_amiga_module[0x438..0x438 + 4])
            }
            AmigaVariant::Fifteen => String::new(),
        };

        let mut data = &ser_amiga_module[0x14..];

        // samples struct. AmigaSample::load fills `sample.name`
        // via a UTF-8-lossy decode (the initial parse path); we
        // overwrite it immediately afterwards with a codepage-aware
        // decode of the same byte range so CP437 art and Latin-1
        // accents both survive intact.
        for i in 0..num_samples {
            let (d2, sample) = AmigaSample::load(data)?;
            data = d2;
            amiga.samples.push(sample);
            let off = 0x14 + i * 30;
            amiga.samples[i].name = codepage.decode_name(&ser_amiga_module[off..off + 22]);
        }

        // After the sample-header loop, the header block continues:
        //   1 byte  song_length
        //   1 byte  restart_position
        //   128     positions (order list)
        //   4       optional tag (31-sample variants only)
        // The previous code indexed these directly. `validate_structure`
        // checks the *total* expected size but a malformed sample
        // record could still consume more bytes than expected, so we
        // re-check that the remaining slice is large enough.
        let trailer_len = 2
            + 128
            + if amiga.get_number_of_samples() != 15 {
                4
            } else {
                0
            };
        if data.len() < trailer_len {
            return Err(ImportError::UnexpectedEof);
        }
        amiga.song_length = data[0];
        amiga.restart_position = data[1];
        data = &data[2..];

        // positions
        amiga.positions.extend_from_slice(&data[..128]);
        data = &data[128..];

        // tag?
        if amiga.get_number_of_samples() != 15 {
            data = &data[4..];
        }

        // patterns
        let dialect = tag_dialect(&amiga.tag);
        amiga.is_hmnt = dialect.is_hmnt;

        // Resolve the effective channel count. The nominal count comes
        // from the tag; a `M.K.` file whose size only closes for an
        // 8-channel reading is a "Mod's Grave" .wow masquerading as
        // 4-channel ProTracker, so widen it to 8.
        let nominal_tracks = match amiga.get_number_of_tracks() {
            Some(n) => n as usize,
            None => 4, // default is 4...return Result::Err(ImportError::Other("Not an amiga module?")),
        };
        let number_of_tracks = if amiga.is_mods_grave(ser_amiga_module.len()) {
            8
        } else {
            nominal_tracks
        };

        let number_of_patterns = amiga.get_number_of_patterns();
        if dialect.is_flt8 {
            // StarTrekker 8-voice: the byte stream is a run of
            // 4-channel patterns. Logical pattern k is assembled from
            // stored pattern 2k (channels 0-3) and 2k+1 (channels
            // 4-7); the order list counts stored patterns, so it is
            // halved. One extra stored pattern beyond `max(order)+1`
            // holds the odd half of the last logical pattern.
            let stored_count = number_of_patterns + 1;
            let num_logical = stored_count.div_ceil(2);
            amiga.patterns =
                vec![vec![vec![PatternSlot::default(); 8]; AMIGA_ROWS_PER_PATTERN]; num_logical];
            for stored in 0..stored_count {
                let lp = stored / 2;
                let ch_base = if stored % 2 == 0 { 0 } else { 4 };
                for row in 0..AMIGA_ROWS_PER_PATTERN {
                    for ch in 0..4 {
                        let e = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                        let mut slot = PatternSlot::deserialize(e);
                        apply_mod_dialect(&mut slot, &dialect);
                        amiga.patterns[lp][row][ch_base + ch] = slot;
                        data = &data[4..];
                    }
                }
            }
            // Collapse the even order entries to logical-pattern indices.
            for p in amiga.positions.iter_mut() {
                *p /= 2;
            }
        } else {
            for _p in 0..number_of_patterns {
                let mut pattern: Vec<Vec<PatternSlot>> = vec![];
                for _row in 0..AMIGA_ROWS_PER_PATTERN {
                    let mut row: Vec<PatternSlot> = vec![];
                    for _elt in 0..number_of_tracks {
                        let e = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                        let mut element = PatternSlot::deserialize(e);
                        apply_mod_dialect(&mut element, &dialect);
                        row.push(element);
                        data = &data[4..];
                    }
                    pattern.push(row);
                }
                amiga.patterns.push(pattern);
            }
        }

        // 15-sample SoundTracker: the effect set differs from
        // ProTracker (Ultimate Soundtracker repurposes commands 1/2 as
        // arpeggio/pitch-bend, older versions use Dxy as a volume
        // slide, etc.). Estimate the authoring tracker, then rewrite
        // each slot's effect into the ProTracker-equivalent the shared
        // mapper consumes downstream.
        if matches!(variant, AmigaVariant::Fifteen) {
            let (stk_version, use_auto_slides) = amiga.detect_stk_version();
            for pat in amiga.patterns.iter_mut() {
                for row in pat.iter_mut() {
                    for slot in row.iter_mut() {
                        remap_stk_slot(slot, stk_version, use_auto_slides);
                    }
                }
            }
        }

        // audio
        for i_spl in 0..amiga.samples.len() {
            // small hack to force COUNTRY.MOD loading
            let l = if 2 * amiga.samples[i_spl].length_div2 as usize <= data.len() {
                2 * amiga.samples[i_spl].length_div2 as usize
            } else {
                data.len()
            };
            let s = &data[0..l];
            let vec_i8: Vec<i8> = s.iter().map(|&x| x as i8).collect();
            amiga.audio.push(vec_i8);
            data = &data[l..];
        }

        Result::Ok(amiga)
    }

    fn to_instr(&self, sample_index: usize) -> Instrument {
        let mut instr: Instrument = Instrument::default();

        let mut sample: Sample = self.samples[sample_index].to_sample();
        if self.is_hmnt {
            // His Master's Noise reads the finetune byte as a full
            // 8-bit value applied as `-(byte << 3)` through an `i8`
            // — a wider, inverted range than the standard signed
            // nibble that `AmigaSample::to_sample` assumes.
            let ft = self.samples[sample_index].finetune;
            sample.finetune = Finetune::from_ratio(-(((ft << 3) as i8) as i32), 127);
        }
        // Number of leading sample bytes to drop (15-sample SoundTracker
        // plays a looped sample *from its loop start*, discarding the
        // attack — see below).
        let mut skip_bytes = 0usize;
        if self.get_number_of_samples() == 15 {
            // 15-sample SoundTracker stores the loop START in bytes,
            // not words — the shared `to_sample` (ProTracker word
            // units) doubles it, which can push the loop past the
            // sample end and silently drop it (e.g. pretend.mod's
            // sustained "strings2"). The loop LENGTH stays in words.
            // SoundTracker also ignores all sample data before the loop
            // start: the sample is re-based so the loop begins at 0 and
            // the pre-loop attack is dropped (this is what removes the
            // click — and the spurious note-onset transient — in those
            // sustained samples).
            let s = &self.samples[sample_index];
            let len = 2 * s.length_div2 as u32;
            let start = s.repeat_offset_div2 as u32; // bytes
            let length = 2 * s.repeat_length_div2 as u32; // words → bytes
            if length > 2 && start + length <= len {
                skip_bytes = start as usize;
                sample.loop_flag = LoopType::Forward;
                sample.loop_start = 0;
                sample.loop_length = length;
            } else {
                sample.loop_flag = LoopType::No;
                sample.loop_start = 0;
                sample.loop_length = 0;
            }
        }
        let audio = if skip_bytes > 0 && skip_bytes < self.audio[sample_index].len() {
            self.audio[sample_index][skip_bytes..].to_vec()
        } else {
            self.audio[sample_index].clone()
        };
        sample.data = Some(SampleDataType::Mono8(audio.into()));

        instr.name = sample.name.clone();

        let mut idef = InstrDefault::default();
        idef.sample.push(Some(sample));

        idef.keyboard.sample_for_pitch = [Some(0); 120];

        instr.instr_type = InstrumentType::Default(idef);

        instr
    }

    pub fn to_module(&self) -> Module {
        let mut module = Module::default();

        module.name = self.title.clone();
        // Preserve the format tag (`M.K.`, `FLT8`, `8CHN`, `OCTA`,
        // …) parsed from offset 0x438. For 15-sample Soundtracker
        // MODs there is no tag — fall back to an explicit label.
        module.comment = if self.get_number_of_samples() == 15 {
            "Soundtracker (15 samples, no tag)".to_string()
        } else {
            format!("MOD tag: {}", self.tag)
        };
        module.quirks = crate::tracker::profiles::pt();
        module.origin = Some(crate::tracker::format::ModuleFormat::Mod);
        module.frequency_type = FrequencyType::AmigaFrequencies;
        // MOD has no mix-volume byte; this is a convention constant
        // matching ProTracker's output level. Calibrated against
        // **pt2-clone** (the cycle-accurate ProTracker reproduction):
        // its Paula mixer normalises 4 voices as `NORM_FACTOR(2.0) ×
        // 32768/4`, so one full voice peaks at 16384 (−6 dBFS) — half
        // of our full-scale accumulator, i.e. exactly 0.5. The empirical
        // render (median −2.6 dB below pt2 at the old 48/128 on
        // env-corr-clean modules) confirms 48/128 × 10^(2.6/20) ≈ 0.5.
        // See `tracker/import/it/BASELINE.md`.
        module.mix_volume = Volume::from_ratio(64, 128);
        module.default_tempo = 6;
        module.default_bpm = 125;
        // 15-sample SoundTracker repurposes the restart byte as a
        // default tempo (the 50 Hz VBlank rate expressed as a divider).
        // The sentinel 0x78 means the plain 125 BPM default; any other
        // value maps to a CIA tempo via SoundTracker's divider:
        //   BPM = 709379·125 / (50·122·(240 − restart)).
        if self.get_number_of_samples() == 15 {
            let restart = self.restart_position as usize;
            if restart != 0 && restart != 0x78 && restart < 240 {
                let denom = 6100 * (240 - restart); // 50·122·(240−restart)
                module.default_bpm = (88_672_375 + denom / 2) / denom;
            }
        }
        // MOD restart byte (offset 0x3B7). Soundtracker / Noisetracker
        // used it as a real loop point; ProTracker writes 0x7F (127)
        // as a "no restart" sentinel. Realised below as a synthetic
        // `NavigationEffect::PositionJump` on the last played row.
        let pattern_order = vec![self.positions[..usize::from(self.song_length)]
            .to_vec()
            .iter()
            .map(|&x| x as usize)
            .collect()];
        let mut im = ImportMemory::default();
        let pattern = im.unpack_patterns(
            FrequencyType::AmigaFrequencies,
            MemoryType::Mod,
            &pattern_order,
            &self.patterns,
        );
        // DAW migration Phase 3c.2 step: the historical MOD
        // restart byte no longer injects a synthetic
        // `NavigationEffect::PositionJump` on the last row. The
        // sequencer's `post_pattern_change` honours the wrap
        // natively via `module.song_loop_to` instead.
        for i in 0..self.samples.len() {
            let instr = self.to_instr(i);
            module.instrument.push(instr);
        }

        // Amiga channels are hard-panned by the hardware in an
        // L-R-R-L pattern (channels 0,3 left; 1,2 right; repeating).
        // Use the de-facto reference separation of 0x40/0xC0 — the same
        // soft pan the S3M importer already applies — rather than full
        // hard-pan, which is fatiguing on headphones.
        let channels = self
            .patterns
            .first()
            .and_then(|p| p.first())
            .map_or(0, |row| row.len());
        module.channel_defaults = (0..channels)
            .map(|ch| {
                let right = matches!(ch & 3, 1 | 2);
                crate::core::module::ChannelDefault {
                    panning: Some(Panning::from_byte_255(if right { 0xC0 } else { 0x40 })),
                    ..Default::default()
                }
            })
            .collect();

        crate::tracker::import::build::build_timeline_layer(&mut module, &pattern_order, &pattern);
        // For 15-sample SoundTracker the restart byte is a tempo, not a
        // loop position (handled above), so only ProTracker/NoiseTracker
        // 31-sample files feed it to the restart-loop helper.
        let restart_loop = if self.get_number_of_samples() == 15 {
            0
        } else {
            self.restart_position as usize
        };
        crate::tracker::import::build::set_song_loop_to_restart_byte(&mut module, 0, restart_loop);

        module
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_text_accepts_plain_ascii() {
        assert!(is_clean_text(b"hello world"));
        assert!(is_clean_text(b"=======[AMiGA]======="));
        assert!(is_clean_text(b"Composed and performe"));
    }

    #[test]
    fn clean_text_accepts_nul_padding() {
        // 22-byte slot half-filled, rest NUL — the common case.
        let name = b"hello\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";
        assert_eq!(name.len(), 22);
        assert!(is_clean_text(name));
        // Pure NUL is fine too (unused slot).
        assert!(is_clean_text(&[0u8; 22]));
    }

    #[test]
    fn clean_text_accepts_cp437_box_art() {
        // GURU.MOD sample #0: 'guru' drawn in CP437 block glyphs
        // (0xDC ▄, 0xDB █, 0xDF ▀).
        let name: &[u8] = &[
            0xdc, 0xdb, 0xdb, 0xdb, 0xdf, 0xdb, 0xdb, 0xdc, 0x20, 0xdc, 0xdb, 0xdb, 0xdc, 0x20,
            0xdc, 0xdb, 0xdb, 0xdb, 0xdf, 0xdb, 0xdb, 0x00,
        ];
        assert_eq!(name.len(), 22);
        assert!(is_clean_text(name));
        // Long horizontal CP437 rule, 0xC4 ─.
        let rule = [0xc4u8; 22];
        assert!(is_clean_text(&rule));
    }

    #[test]
    fn clean_text_accepts_latin1_accents() {
        // "Café au lait" in Latin-1: é = 0xE9.
        let name: &[u8] = b"Caf\xe9 au lait\0\0\0\0\0\0\0\0\0\0";
        assert_eq!(name.len(), 22);
        assert!(is_clean_text(name));
        // Mixed CP437 / accented bytes.
        let name: &[u8] = b"\xc4\xc4 Fran\xe7ois \xc4\xc4\0\0\0\0\0\0\0\0";
        assert_eq!(name.len(), 22);
        assert!(is_clean_text(name));
    }

    #[test]
    fn clean_text_rejects_c0_controls() {
        // Tab, LF, CR, escape, etc. — never appear in name fields,
        // common in random binary.
        assert!(!is_clean_text(b"hello\tworld"));
        assert!(!is_clean_text(b"line\nbreak"));
        assert!(!is_clean_text(b"line\rbreak"));
        assert!(!is_clean_text(&[0x1b, b'a', b'b']));
        assert!(!is_clean_text(&[0x01]));
        assert!(!is_clean_text(&[0x1f]));
    }

    #[test]
    fn clean_text_rejects_del() {
        assert!(!is_clean_text(b"hello\x7fworld"));
    }

    #[test]
    fn clean_text_accepts_full_byte_range_minus_controls() {
        // Every byte from 0x20 upward (printable ASCII + every
        // high byte) must pass; the only rejections are C0 and
        // DEL. Exhaustive single-byte check.
        for b in 0u8..=255 {
            let expected = !matches!(b, 0x01..=0x1F | 0x7F);
            assert_eq!(
                is_clean_text(&[b]),
                expected,
                "byte 0x{b:02x} should be {}",
                if expected { "accepted" } else { "rejected" }
            );
        }
    }

    // ---- variant decode tests -------------------------------------
    //
    // Synthetic fixtures exercise the tag-driven decode quirks without
    // depending on any sampled `.mod` file: a 31-sample header with a
    // chosen tag, order list, sample records, and raw pattern bytes.

    use crate::core::cell_note::CellNote;

    /// Build a minimal valid 31-sample MOD image.
    /// `samples` is `(length_div2, finetune, volume)` per slot (padded
    /// to 31 with empties); `pattern_bytes` is the raw pattern stream;
    /// trailing zero sample audio is appended to satisfy size checks.
    fn build_mod31(
        tag: &[u8; 4],
        song_len: u8,
        restart: u8,
        order: &[u8],
        samples: &[(u16, u8, u8)],
        pattern_bytes: &[u8],
    ) -> Vec<u8> {
        let mut v = vec![0u8; MOD_31_HEADER_SIZE];
        for (i, &(len_div2, finetune, vol)) in samples.iter().enumerate() {
            let off = 20 + i * AMIGA_SAMPLE_RECORD_SIZE;
            v[off + 22..off + 24].copy_from_slice(&len_div2.to_be_bytes());
            v[off + 24] = finetune;
            v[off + 25] = vol;
        }
        let song_len_off = 20 + 31 * AMIGA_SAMPLE_RECORD_SIZE;
        v[song_len_off] = song_len;
        v[song_len_off + 1] = restart;
        for (i, &o) in order.iter().enumerate() {
            v[song_len_off + 2 + i] = o;
        }
        v[0x438..0x438 + 4].copy_from_slice(tag);
        v.extend_from_slice(pattern_bytes);
        let audio_bytes: usize = samples.iter().map(|&(l, _, _)| 2 * l as usize).sum();
        v.resize(v.len() + audio_bytes, 0);
        v
    }

    /// One MOD pattern cell: period only (no instrument/effect).
    fn cell_period(period: u16) -> [u8; 4] {
        [(period >> 8) as u8, (period & 0xFF) as u8, 0, 0]
    }

    /// One MOD pattern cell carrying an effect nibble + parameter.
    fn cell_effect(effect: u8, param: u8) -> [u8; 4] {
        [0, 0, effect & 0x0F, param]
    }

    /// A 64-row, `channels`-wide pattern of empty cells, with the row-0
    /// cells overridden by `row0`.
    fn pattern_block(channels: usize, row0: &[[u8; 4]]) -> Vec<u8> {
        let mut p = vec![0u8; channels * AMIGA_ROWS_PER_PATTERN * AMIGA_SLOT_SIZE];
        for (ch, cell) in row0.iter().enumerate() {
            p[ch * 4..ch * 4 + 4].copy_from_slice(cell);
        }
        p
    }

    #[test]
    fn flt8_interleaves_two_stored_halves() {
        // One logical 8-channel pattern = two stored 4-channel halves.
        // Half A (channels 0-3) carries a note on channel 0; half B
        // (channels 4-7) carries a different note on its channel 0,
        // which must land on logical channel 4.
        let mut patterns = pattern_block(4, &[cell_period(428)]); // stored 0
        patterns.extend(pattern_block(4, &[cell_period(480)])); // stored 1
        let img = build_mod31(b"FLT8", 1, 0, &[0], &[], &patterns);

        let m = AmigaModule::load(&img).expect("FLT8 loads");
        assert_eq!(
            m.patterns.len(),
            1,
            "two stored halves → one logical pattern"
        );
        let row0 = &m.patterns[0][0];
        assert_eq!(row0.len(), 8, "logical pattern is 8 channels wide");
        assert!(
            matches!(row0[0].note, CellNote::Play(_)),
            "half A note on ch0"
        );
        assert!(
            matches!(row0[4].note, CellNote::Play(_)),
            "half B note on ch4"
        );
        assert_ne!(
            row0[0].note, row0[4].note,
            "the two halves are distinct notes"
        );
        for ch in [1usize, 2, 3, 5, 6, 7] {
            assert_eq!(row0[ch].note, CellNote::Empty, "ch{ch} stays empty");
        }
    }

    #[test]
    fn mods_grave_wow_widens_to_eight_channels() {
        // A `M.K.`-tagged image whose size only closes for 8 channels
        // is a Mod's Grave .wow: one 8-channel pattern, no samples,
        // restart 0 → 1084 + 0 + 2048 == file length.
        let patterns = pattern_block(8, &[cell_period(428)]);
        let img = build_mod31(b"M.K.", 1, 0, &[0], &[], &patterns);
        assert_eq!(img.len(), MOD_31_HEADER_SIZE + 8 * 64 * 4);

        let m = AmigaModule::load(&img).expect("WOW loads");
        assert_eq!(m.patterns[0][0].len(), 8, "M.K. widened to 8 channels");

        // A genuine 4-channel M.K. of the same shape must NOT widen.
        let patterns4 = pattern_block(4, &[cell_period(428)]);
        let img4 = build_mod31(b"M.K.", 1, 0, &[0], &[], &patterns4);
        let m4 = AmigaModule::load(&img4).expect("4ch M.K. loads");
        assert_eq!(m4.patterns[0][0].len(), 4, "real M.K. stays 4 channels");
    }

    #[test]
    fn noisetracker_pattern_break_drops_parameter() {
        // N.T.: a Dxx pattern break always lands on row 0, so the
        // parameter is zeroed at import.
        let patterns = {
            let mut p = pattern_block(4, &[cell_effect(0x0D, 0x20)]);
            // ProTracker-tagged control: parameter must survive.
            p.extend(pattern_block(4, &[]));
            p
        };
        let nt = build_mod31(b"N.T.", 1, 0, &[0, 1], &[], &patterns);
        let m = AmigaModule::load(&nt).expect("N.T. loads");
        assert_eq!(m.patterns[0][0][0].effect_type, 0x0D);
        assert_eq!(
            m.patterns[0][0][0].effect_parameter, 0,
            "NoiseTracker Dxx param dropped"
        );

        let pt = build_mod31(b"M.K.", 1, 0, &[0, 1], &[], &patterns);
        let mp = AmigaModule::load(&pt).expect("M.K. loads");
        assert_eq!(
            mp.patterns[0][0][0].effect_parameter, 0x20,
            "ProTracker keeps Dxx param"
        );
    }

    fn first_sample_finetune(m: &Module) -> Finetune {
        match &m.instrument[0].instr_type {
            InstrumentType::Default(idef) => idef.sample[0].as_ref().unwrap().finetune,
            _ => panic!("expected default instrument"),
        }
    }

    #[test]
    fn his_masters_noise_finetune_differs_from_standard() {
        // Same finetune byte decodes to opposite signs: the standard
        // path reads the low nibble as a positive offset, HMN reads
        // -(byte << 3) → negative.
        let patterns = pattern_block(4, &[]);
        let hmn = build_mod31(b"M&K!", 1, 0, &[0], &[(1, 0x01, 64)], &patterns);
        let mk = build_mod31(b"M.K.", 1, 0, &[0], &[(1, 0x01, 64)], &patterns);

        let mhmn = AmigaModule::load(&hmn).unwrap();
        assert!(mhmn.is_hmnt, "M&K! flagged as His Master's Noise");
        let f_hmn = first_sample_finetune(&mhmn.to_module()).to_signed_byte(127);
        let f_mk =
            first_sample_finetune(&AmigaModule::load(&mk).unwrap().to_module()).to_signed_byte(127);
        assert_ne!(f_hmn, f_mk, "HMN finetune mapping diverges from standard");
        assert!(
            f_hmn < 0,
            "HMN finetune is negative for byte 0x01 (got {f_hmn})"
        );
        assert!(
            f_mk > 0,
            "standard finetune is positive for nibble 0x01 (got {f_mk})"
        );
    }

    // ---- 15-sample SoundTracker tests -----------------------------

    /// Build a minimal valid 15-sample (untagged) SoundTracker image.
    /// `samples` is `(length_div2, finetune, volume, rep_off, rep_len)`.
    fn build_mod15(
        song_len: u8,
        restart: u8,
        order: &[u8],
        samples: &[(u16, u8, u8, u16, u16)],
        pattern_bytes: &[u8],
    ) -> Vec<u8> {
        let mut v = vec![0u8; MOD_15_HEADER_SIZE];
        for (i, &(len, ft, vol, ro, rl)) in samples.iter().enumerate() {
            let off = 20 + i * AMIGA_SAMPLE_RECORD_SIZE;
            v[off + 22..off + 24].copy_from_slice(&len.to_be_bytes());
            v[off + 24] = ft;
            v[off + 25] = vol;
            v[off + 26..off + 28].copy_from_slice(&ro.to_be_bytes());
            v[off + 28..off + 30].copy_from_slice(&rl.to_be_bytes());
        }
        let song_len_off = 20 + 15 * AMIGA_SAMPLE_RECORD_SIZE;
        v[song_len_off] = song_len;
        v[song_len_off + 1] = restart;
        for (i, &o) in order.iter().enumerate() {
            v[song_len_off + 2 + i] = o;
        }
        v.extend_from_slice(pattern_bytes);
        let audio: usize = samples.iter().map(|&(l, _, _, _, _)| 2 * l as usize).sum();
        v.resize(v.len() + audio, 0);
        v
    }

    /// A pattern cell with an explicit raw command nibble + parameter.
    fn cell_cmd(cmd: u8, param: u8) -> [u8; 4] {
        [0, 0, cmd & 0x0F, param]
    }

    #[test]
    fn ultimate_soundtracker_remaps_arpeggio_and_pitchbend() {
        // Small samples + only 1xx/2xx effects → detected as Ultimate
        // Soundtracker, where command 1 is arpeggio and command 2 is
        // pitch-bend (low nibble up / high nibble down).
        let row0 = [cell_cmd(1, 0x47), cell_cmd(2, 0x30), [0; 4], [0; 4]];
        let patterns = pattern_block(4, &row0);
        let img = build_mod15(1, 0, &[0], &[(10, 0, 64, 0, 1)], &patterns);

        let m = AmigaModule::load(&img).expect("UST loads");
        // command 1 → ProTracker arpeggio (command 0), param preserved.
        assert_eq!(m.patterns[0][0][0].effect_type, 0x00);
        assert_eq!(m.patterns[0][0][0].effect_parameter, 0x47);
        // command 2 high nibble → portamento down (command 2), param>>4.
        assert_eq!(m.patterns[0][0][1].effect_type, 0x02);
        assert_eq!(m.patterns[0][0][1].effect_parameter, 0x03);
    }

    #[test]
    fn later_soundtracker_keeps_protracker_effects() {
        // A large sample (> UST's ~10 KB limit) forces a Master
        // Soundtracker / later estimate, where the standard ProTracker
        // effect set applies: command 1 stays portamento up.
        let row0 = [cell_cmd(1, 0x47), [0; 4], [0; 4], [0; 4]];
        let patterns = pattern_block(4, &row0);
        let img = build_mod15(1, 0, &[0], &[(6000, 0, 64, 0, 1)], &patterns);

        let m = AmigaModule::load(&img).expect("MST loads");
        assert_eq!(
            m.patterns[0][0][0].effect_type, 0x01,
            "command 1 stays portamento up for non-UST SoundTracker"
        );
        assert_eq!(m.patterns[0][0][0].effect_parameter, 0x47);
    }

    #[test]
    fn amiga_channels_pan_lrrl() {
        // 4-channel MOD → hardware L-R-R-L hard-pan at 0x40/0xC0.
        let patterns = pattern_block(4, &[]);
        let img = build_mod31(b"M.K.", 1, 0, &[0], &[], &patterns);
        let m = AmigaModule::load(&img).unwrap().to_module();
        assert_eq!(m.channel_defaults.len(), 4);
        let left = Panning::from_byte_255(0x40);
        let right = Panning::from_byte_255(0xC0);
        assert_eq!(m.channel_defaults[0].panning, Some(left));
        assert_eq!(m.channel_defaults[1].panning, Some(right));
        assert_eq!(m.channel_defaults[2].panning, Some(right));
        assert_eq!(m.channel_defaults[3].panning, Some(left));
    }

    #[test]
    fn soundtracker_loop_start_is_bytes_with_attack_dropped() {
        // SoundTracker loop start is in bytes (not words) and the
        // pre-loop attack is discarded so the loop begins at sample 0.
        // rep_off=4 bytes, rep_len=3 words → loop length 6 bytes.
        let patterns = pattern_block(4, &[]);
        let img = build_mod15(1, 0, &[0], &[(10, 0, 64, 4, 3)], &patterns);
        let m = AmigaModule::load(&img).unwrap().to_module();
        match &m.instrument[0].instr_type {
            InstrumentType::Default(idef) => {
                let s = idef.sample[0].as_ref().unwrap();
                assert_eq!(s.loop_flag, LoopType::Forward);
                assert_eq!(s.loop_start, 0, "loop re-based to 0 after dropping attack");
                assert_eq!(s.loop_length, 6, "loop length = rep_len words → bytes");
            }
            _ => panic!("expected default instrument"),
        }
    }
}
