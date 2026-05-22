use super::amiga_sample::AmigaSample;
use super::patternslot::PatternSlot;
use crate::import::bin_reader::ImportError;

use crate::codepage::Codepage;
use crate::fixed::units::Volume;
use crate::import::import_memory::ImportMemory;
use crate::import::import_memory::MemoryType;
use crate::prelude::*;

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
        let (num_samples, num_tracks, header_size) = match variant {
            AmigaVariant::Fifteen => (15usize, 4u8, MOD_15_HEADER_SIZE),
            AmigaVariant::ThirtyOne { num_tracks } => (31, num_tracks, MOD_31_HEADER_SIZE),
        };

        if data.len() < header_size {
            return Err(ImportError::Other("File too short for MOD header"));
        }

        // Title bytes 0..20 must be printable ASCII or NUL —
        // pre-rejects nearly all random binary.
        if !is_clean_text(&data[..20]) {
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
            if !is_clean_text(&data[off..off + 22]) {
                return Err(ImportError::Other("MOD sample name is not ASCII"));
            }
            let length_div2 = u16::from_be_bytes([data[off + 22], data[off + 23]]);
            let finetune = data[off + 24];
            let volume = data[off + 25];
            if finetune > 0x0F {
                return Err(ImportError::Other("MOD sample finetune > 15"));
            }
            if volume > 0x40 {
                return Err(ImportError::Other("MOD sample volume > 64"));
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
        // Only the first `song_length` entries are meaningful; the
        // rest is don't-care padding written as zeros by every
        // historical tracker but liable to be garbage in fuzz input.
        let max_pos = *positions[..song_length as usize].iter().max().unwrap_or(&0);
        if max_pos >= 128 {
            return Err(ImportError::Other("MOD position byte out of range"));
        }

        // The decisive check: every byte the loader would read must
        // actually be present in the file. Headers + patterns +
        // sample data, summed exactly the way the rest of `load`
        // walks them. Random blobs almost never satisfy this.
        let pattern_size: u32 =
            num_tracks as u32 * AMIGA_ROWS_PER_PATTERN as u32 * AMIGA_SLOT_SIZE as u32;
        let num_patterns: u32 = max_pos as u32 + 1;
        let expected: u32 = (header_size as u32)
            .saturating_add(pattern_size.saturating_mul(num_patterns))
            .saturating_add(total_sample_bytes);
        if expected as usize > data.len() {
            return Err(ImportError::Other("MOD file shorter than declared content"));
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
        let number_of_tracks = match amiga.get_number_of_tracks() {
            Some(n) => n as usize,
            None => 4, // default is 4...return Result::Err(ImportError::Other("Not an amiga module?")),
        };

        let number_of_patterns = amiga.get_number_of_patterns();
        for _p in 0..number_of_patterns {
            let mut pattern: Vec<Vec<PatternSlot>> = vec![];
            for _row in 0..64 {
                let mut row: Vec<PatternSlot> = vec![];
                for _elt in 0..number_of_tracks {
                    let e = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                    let element = PatternSlot::deserialize(e);
                    row.push(element);
                    data = &data[4..];
                }
                pattern.push(row);
            }
            amiga.patterns.push(pattern);
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
        sample.data = Some(SampleDataType::Mono8(self.audio[sample_index].clone()));

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
        module.profile = CompatibilityProfile::pt();
        module.frequency_type = FrequencyType::AmigaFrequencies;
        // MOD has no mix-volume byte. Schism initialises the mixer
        // chain at `mixing_volume = 48` (`csndfile.c:55`,
        // `fmt/xm.c:885`) — same default as XM. We mirror that so
        // every format feeds the player's mixer with the same
        // pre-attenuated level.
        module.mix_volume = Volume::from_ratio(48, 128);
        module.default_tempo = 6;
        module.default_bpm = 125;
        // MOD restart byte (offset 0x3B7). Soundtracker / Noisetracker
        // used it as a real loop point; ProTracker writes 0x7F (127)
        // as a "no restart" sentinel. Realised below as a synthetic
        // `GlobalEffect::PositionJump` on the last played row.
        let mut pattern_order = vec![self.positions[..usize::from(self.song_length)]
            .to_vec()
            .iter()
            .map(|&x| x as usize)
            .collect()];
        let mut im = ImportMemory::default();
        let mut pattern = im.unpack_patterns(
            FrequencyType::AmigaFrequencies,
            MemoryType::Mod,
            &pattern_order,
            &self.patterns,
        );
        crate::daw::build_timeline::inject_restart_position_jump(
            &mut pattern_order[0],
            &mut pattern,
            self.restart_position as usize,
        );

        for i in 0..self.samples.len() {
            let instr = self.to_instr(i);
            module.instrument.push(instr);
        }

        crate::daw::build_timeline::build_timeline_layer(&mut module, &pattern_order, &pattern);

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
}
