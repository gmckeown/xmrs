//! Detect the legacy 8-bit codepage of a text field and decode it
//! to UTF-8.
//!
//! Module / XM / S3M / IT containers store song, sample, and
//! instrument names as fixed-width 8-bit byte slabs without any
//! encoding declaration. Authors of the early-90s tracker scene
//! routinely used:
//!
//! * **CP437** (IBM PC code page) — for ASCII-art logos drawn with
//!   the box-drawing glyphs in the `0xB0..=0xDF` range
//!   (▒░▓█▌▐▀▄┌─┐│└┘╔═╗ etc.). The dominant choice on PC trackers
//!   (ScreamTracker, FastTracker II, Impulse Tracker) and the
//!   reason a raw UTF-8 conversion of MOD/XM/S3M/IT files
//!   typically returns garbage.
//! * **Latin-1 / ISO-8859-1** — for accented Western-European
//!   letters (`Café`, `François`, `Wrocław`). The Amiga's native
//!   ECMA-94 encoding is also a Latin-1 superset, so this covers
//!   both the Amiga MOD lineage and any cross-platform text.
//! * **Plain ASCII** — for English-only names; trivially a subset
//!   of both above.
//!
//! Without knowing which codepage a file uses, a naive
//! `String::from_utf8_lossy` replaces every high byte with
//! `U+FFFD` and silently destroys the original information — a
//! "GURU" logo drawn in CP437 blocks (`0xDB 0xDC 0xDF`) becomes
//! a row of replacement characters.
//!
//! This module picks a codepage from the byte distribution itself
//! using two complementary signals:
//!
//! 1. **Bytes in `0x80..=0x9F`** are decisive for CP437 — Latin-1
//!    leaves the C1-control range undefined, so any byte there
//!    means we are *not* looking at Latin-1.
//! 2. **High-byte density** above ~50 % of the non-NUL field
//!    length is typical of ASCII art (long runs of identical
//!    block glyphs like `0xDB 0xDB 0xDB ...`); ordinary
//!    accented-letter text rarely exceeds ~20 %.
//!
//! Combined-field detection ([`crate::tracker::codepage::Codepage::detect_from_fields`]) is
//! stronger than per-field detection because the aggregate has
//! more bytes to score on — a single 22-byte name with two
//! accents is ambiguous on its own.
//!
//! # Example
//!
//! ```
//! use xmrs::tracker::codepage::Codepage;
//!
//! // GURU.MOD sample-name bytes: 'guru' drawn in CP437 blocks.
//! let bytes: &[u8] = &[
//!     0xdc, 0xdb, 0xdb, 0xdb, 0xdf, 0xdb, 0xdb, 0xdc,
//!     0x20, 0xdc, 0xdb, 0xdb, 0xdc, 0x20,
//!     0xdc, 0xdb, 0xdb, 0xdb, 0xdf, 0xdb, 0xdb, 0x00,
//! ];
//! let cp = Codepage::detect(bytes);
//! assert_eq!(cp, Codepage::Cp437);
//!
//! let decoded = cp.decode(bytes);
//! // The block-drawing glyphs are now valid Unicode and the
//! // logo survives the conversion.
//! assert!(decoded.contains('█'));
//! assert!(decoded.contains('▀'));
//! assert!(decoded.contains('▄'));
//! ```

use alloc::string::String;

/// Which 8-bit codepage a byte slice is most plausibly written in.
///
/// Restricted on purpose to the three encodings that cover the
/// vast majority of historical tracker files; further variants
/// (Windows-1252, ISO-8859-2, Mac Roman, …) can be added without
/// breaking the API because all decoding goes through
/// [`Codepage::decode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Codepage {
    /// Pure 7-bit ASCII — no high bytes present. Decodes
    /// identically under any 8-bit superset of ASCII.
    Ascii,
    /// IBM PC code page 437. Default for PC-tracker authors and
    /// the encoding used by scene composers for sample-name
    /// ASCII art.
    Cp437,
    /// ISO-8859-1 / Latin-1. Identity mapping (byte `b` → `U+00b`).
    ///
    /// Also covers the **Amiga native character set** (ECMA-94),
    /// which the workbench, Topaz font, and trackers like
    /// ProTracker / NoiseTracker / OctaMED used for sample-name
    /// display. ECMA-94 was the 1985 draft that became ISO-8859-1
    /// in 1987; in the printable ranges `0x20..=0x7E` and
    /// `0xA0..=0xFF` the two are byte-for-byte identical, and
    /// both leave the C1 range `0x80..=0x9F` undefined. So when an
    /// Amiga user types `François` in ProTracker the saved byte
    /// for `ç` is `0xE7` — exactly the Latin-1 value. One enum
    /// variant correctly handles both PC-Latin-1 and the Amiga
    /// lineage.
    Latin1,
}

impl Codepage {
    /// Detect the codepage of a single byte slice.
    ///
    /// Equivalent to `Codepage::detect_from_fields(&[bytes])` but
    /// usable as a one-liner when only one field is at hand.
    /// Prefer [`Self::detect_from_fields`] when the caller has
    /// access to multiple name fields from the same file — more
    /// bytes give a much more reliable verdict.
    #[inline]
    pub fn detect(bytes: &[u8]) -> Codepage {
        Codepage::detect_from_fields(&[bytes])
    }

    /// Detect the codepage shared by several name fields read out
    /// of the same file (title plus sample names plus instrument
    /// names plus more). Pooling the bytes across every field
    /// gives the detector more signal: a single name with one
    /// accented letter is ambiguous on its own, but the song's
    /// *aggregate* byte distribution is decisive.
    ///
    /// The verdict is whichever of the supported codepages best
    /// matches the byte distribution. NUL bytes (used as
    /// fixed-width padding) are ignored.
    pub fn detect_from_fields(fields: &[&[u8]]) -> Codepage {
        let mut non_nul: usize = 0;
        let mut high: usize = 0;
        let mut c1_controls: usize = 0;
        let mut shade_glyphs: usize = 0; // 0xB0..=0xB2 (░▒▓)
        let mut block_glyphs: usize = 0; // 0xDB..=0xDF (█▄▌▐▀)

        for field in fields {
            for &b in *field {
                if b == 0 {
                    continue; // padding, ignore
                }
                non_nul += 1;
                if b >= 0x80 {
                    high += 1;
                }
                match b {
                    0x80..=0x9F => c1_controls += 1,
                    0xB0..=0xB2 => shade_glyphs += 1,
                    0xDB..=0xDF => block_glyphs += 1,
                    _ => {}
                }
            }
        }

        if non_nul == 0 || high == 0 {
            // No high bytes anywhere → pure ASCII, decoding is
            // unambiguous.
            return Codepage::Ascii;
        }

        // CP437 signals, any one of which commits to CP437:
        //
        //   1. Bytes in `0x80..=0x9F`. Latin-1 leaves the C1
        //      range formally undefined; CP437 has 32 ordinary
        //      glyphs there (Çüéâäàåç …). Even one byte rules
        //      out Latin-1.
        //   2. Bytes in `0xB0..=0xB2` (░▒▓ shades). In Latin-1
        //      these are °±² — almost never seen in tracker
        //      names. A single occurrence is decisive.
        //   3. Three or more bytes in `0xDB..=0xDF`. In CP437
        //      these are the block elements █▄▌▐▀ used to draw
        //      filled regions of sample-name art; in Latin-1
        //      they are ÛÜÝÞß. Setting the threshold at 3 keeps
        //      ordinary German names (which carry one or two ß
        //      / Ü across all fields) on the Latin-1 side
        //      while still catching any real ASCII-art module:
        //      e.g. GURU.MOD pools 100+ bytes in this range.
        //   4. High-byte density past 50 % of the non-NUL bytes.
        //      Latin-1 names mix accents with regular letters
        //      (typically < 20 % accents); ASCII art that uses
        //      only the horizontal/vertical box-drawing range
        //      (`0xC4 ─`, `0xCD ═`, etc., outside the block
        //      window of signal 3) still packs density past
        //      85 %.
        if c1_controls > 0 || shade_glyphs > 0 || block_glyphs >= 3 || high * 2 > non_nul {
            return Codepage::Cp437;
        }

        Codepage::Latin1
    }

    /// Decode `bytes` to UTF-8 under this codepage.
    ///
    /// NUL bytes are preserved as `'\0'` so the caller can choose
    /// whether to trim them or stop at the first one (typical for
    /// fixed-width C-string padding). Use [`Self::decode_name`]
    /// for the common "trim NUL + whitespace" behaviour.
    ///
    /// Latin-1 and Ascii both use the identity mapping; only the
    /// CP437 variant consults a translation table.
    pub fn decode(self, bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len());
        match self {
            Codepage::Ascii | Codepage::Latin1 => {
                // Latin-1: byte `b` → `U+00b`. ASCII fits as the
                // 0x00..=0x7F prefix of that mapping.
                for &b in bytes {
                    out.push(b as char);
                }
            }
            Codepage::Cp437 => {
                for &b in bytes {
                    if b < 0x80 {
                        out.push(b as char);
                    } else {
                        out.push(CP437_HIGH[(b - 0x80) as usize]);
                    }
                }
            }
        }
        out
    }

    /// Decode and clean up a fixed-width name field. Stops at
    /// the first NUL (C-string convention used in every
    /// tracker container) and strips *trailing* ASCII
    /// whitespace — the field's natural space-padding to the
    /// slot width.
    ///
    /// Leading whitespace is **preserved**. Scene authors
    /// routinely used leading spaces to center their sample-
    /// name lines under multi-line ASCII-art logos: a line
    /// like `"          POLAND"` is a deliberate part of the
    /// vertical layout, not stray padding. Stripping it
    /// would left-align that line against the logo, breaking
    /// the artist's composition.
    pub fn decode_name(self, bytes: &[u8]) -> String {
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        let s = self.decode(&bytes[..end]);
        // `trim_end_matches(char::is_whitespace)` would strip
        // ALL Unicode whitespace; we deliberately scope the cut
        // to ASCII space — CP437 0xFF (NBSP / U+00A0) is a
        // valid printable-ish glyph in tracker art and must not
        // be eaten by the cleanup.
        s.trim_end_matches(' ').into()
    }
}

/// Detect-then-decode in one call, with name cleanup. Convenience
/// for callers that only have one byte slice to handle.
#[inline]
pub fn decode_autodetect(bytes: &[u8]) -> String {
    Codepage::detect(bytes).decode_name(bytes)
}

// ---------------------------------------------------------------
// CP437 → Unicode table
// ---------------------------------------------------------------

/// Unicode codepoints for CP437 bytes `0x80..=0xFF`.
///
/// Reference: <https://en.wikipedia.org/wiki/Code_page_437>.
/// Indexed by `byte - 0x80`. The low half `0x00..=0x7F` is
/// identical to ASCII and is decoded directly without consulting
/// this table.
#[rustfmt::skip]
static CP437_HIGH: [char; 128] = [
    // 0x80..=0x8F
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å',
    // 0x90..=0x9F
    'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ',
    // 0xA0..=0xAF
    'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»',
    // 0xB0..=0xBF — light shade / box-drawing
    '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐',
    // 0xC0..=0xCF — box-drawing
    '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧',
    // 0xD0..=0xDF — box-drawing & blocks
    '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐', '▀',
    // 0xE0..=0xEF — Greek + math
    'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩',
    // 0xF0..=0xFF — math + misc
    '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{A0}',
];

// ---------------------------------------------------------------
// Tests
// ---------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    // ---- detection ----------------------------------------

    #[test]
    fn detect_ascii_only() {
        assert_eq!(Codepage::detect(b""), Codepage::Ascii);
        assert_eq!(Codepage::detect(b"Hello, world!"), Codepage::Ascii);
        // NUL-only padding counts as empty after the pad strip.
        assert_eq!(Codepage::detect(&[0u8; 22]), Codepage::Ascii);
        // Mixed ASCII + NUL pad: still ASCII.
        assert_eq!(
            Codepage::detect(b"name\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0"),
            Codepage::Ascii
        );
    }

    #[test]
    fn detect_cp437_block_art() {
        // The exact bytes from GURU.MOD sample #0 — 86 % high-byte
        // density, well above the 50 % threshold.
        let guru: &[u8] = &[
            0xdc, 0xdb, 0xdb, 0xdb, 0xdf, 0xdb, 0xdb, 0xdc, 0x20, 0xdc, 0xdb, 0xdb, 0xdc, 0x20,
            0xdc, 0xdb, 0xdb, 0xdb, 0xdf, 0xdb, 0xdb, 0x00,
        ];
        assert_eq!(Codepage::detect(guru), Codepage::Cp437);

        // Rule of horizontal box-drawing characters.
        assert_eq!(Codepage::detect(&[0xc4u8; 22]), Codepage::Cp437);

        // A name with a CP437-only byte (Ç = 0x80) — Latin-1
        // forbids the C1 range, so this is unambiguous.
        let cap_c_cedilla: &[u8] = b"\x80a Project";
        assert_eq!(Codepage::detect(cap_c_cedilla), Codepage::Cp437);
    }

    #[test]
    fn detect_latin1_accented_name() {
        // "Café au lait" — one accent in 12 chars, 8 % density.
        let s: &[u8] = b"Caf\xe9 au lait";
        assert_eq!(Codepage::detect(s), Codepage::Latin1);
        // "François" — one accent, 12 % density.
        let s: &[u8] = b"Fran\xe7ois";
        assert_eq!(Codepage::detect(s), Codepage::Latin1);
        // Several accents but still well under 50 %.
        let s: &[u8] = b"\xc9l\xe9onore d'Acquit\xe9";
        assert_eq!(Codepage::detect(s), Codepage::Latin1);
    }

    #[test]
    fn detect_pooled_fields_more_reliable() {
        // A single accented name is too short for the density rule
        // alone, but combined with a long ASCII title the verdict
        // is unambiguous.
        let title: &[u8] = b"My Great Demo Soundtrack";
        let name1: &[u8] = b"Fran\xe7ois";
        let name2: &[u8] = b"";
        let cp = Codepage::detect_from_fields(&[title, name1, name2]);
        assert_eq!(cp, Codepage::Latin1);

        // Same setup but the sample names are CP437 block art —
        // the verdict flips.
        let block_art: &[u8] = &[0xdbu8; 22];
        let cp = Codepage::detect_from_fields(&[title, block_art, block_art]);
        assert_eq!(cp, Codepage::Cp437);
    }

    #[test]
    fn detect_pooled_guru_mod_layout() {
        // Regression for a real bug: pooling many ASCII contact-
        // info fields with a few CP437-art fields dropped the
        // overall high-byte density below 50 %, and the detector
        // was falling back to Latin1 even though 100+ bytes were
        // clearly CP437 block glyphs. The block-glyph signal
        // (`≥ 3` bytes in `0xDB..=0xDF`) must catch this.
        //
        // Real fields lifted from GURU.MOD — five art slots and
        // four plain-ASCII info slots, mirroring the real file's
        // mix. Total non-NUL ≈ 200 bytes; high-byte density ≈ 30 %;
        // block-glyph count well above the threshold.
        let title: &[u8] = b"guru";
        let art0: &[u8] = &[
            0xdc, 0xdb, 0xdb, 0xdb, 0xdf, 0xdb, 0xdb, 0xdc, 0x20, 0xdc, 0xdb, 0xdb, 0xdc, 0x20,
            0xdc, 0xdb, 0xdb, 0xdb, 0xdf, 0xdb, 0xdb,
        ];
        let art1: &[u8] = &[0xdbu8; 19];
        let art2: &[u8] = &[
            0xdf, 0xdb, 0xdb, 0xdb, 0xdc, 0xdc, 0xdc, 0x20, 0x20, 0xdb, 0xdb, 0xdb, 0xdb, 0x20,
            0xdb, 0xdb, 0xdb, 0xdb,
        ];
        let line: &[u8] = &[0xc4u8; 21];
        let composed: &[u8] = b"Composed and performe";
        let by: &[u8] = b"     by Scorpik of";
        let contact: &[u8] = b"Contact:";
        let address: &[u8] = b"  54-130  Wroclaw 62";

        let cp = Codepage::detect_from_fields(&[
            title, art0, art1, art2, line, composed, by, contact, address,
        ]);
        assert_eq!(cp, Codepage::Cp437);
    }

    #[test]
    fn detect_does_not_misclassify_german_eszett() {
        // 'Großmann' has one `ß` (0xDF), which is also CP437 ▀.
        // A single block byte must not be enough signal to flip
        // detection to CP437 — only a real ASCII-art run does.
        let s: &[u8] = b"Gro\xdfmann";
        assert_eq!(Codepage::detect(s), Codepage::Latin1);

        // Two ß's in a pooled set: 'Straße' + 'Größe' — still
        // ordinary German prose, must stay Latin1.
        let a: &[u8] = b"Stra\xdfe";
        let b: &[u8] = b"Gr\xf6\xdfe";
        assert_eq!(Codepage::detect_from_fields(&[a, b]), Codepage::Latin1);

        // 'MÜLLER' — one byte in the block range (Ü = 0xDC) with
        // ordinary ASCII context, must stay Latin1.
        let s: &[u8] = b"M\xdcLLER";
        assert_eq!(Codepage::detect(s), Codepage::Latin1);
    }

    // ---- decoding -----------------------------------------

    #[test]
    fn decode_ascii_is_identity() {
        assert_eq!(Codepage::Ascii.decode(b"hello"), "hello");
        assert_eq!(Codepage::Latin1.decode(b"hello"), "hello");
        assert_eq!(Codepage::Cp437.decode(b"hello"), "hello");
    }

    #[test]
    fn decode_cp437_box_glyphs() {
        // Single bytes from the box-drawing region.
        assert_eq!(Codepage::Cp437.decode(&[0xdb]), "█");
        assert_eq!(Codepage::Cp437.decode(&[0xdc]), "▄");
        assert_eq!(Codepage::Cp437.decode(&[0xdf]), "▀");
        assert_eq!(Codepage::Cp437.decode(&[0xc4]), "─");
        assert_eq!(Codepage::Cp437.decode(&[0xcd]), "═");
        // GURU.MOD's first sample name decodes to recognisable
        // ASCII-art letters.
        let guru: &[u8] = &[
            0xdc, 0xdb, 0xdb, 0xdb, 0xdf, 0xdb, 0xdb, 0xdc, 0x20, 0xdc, 0xdb, 0xdb, 0xdc, 0x20,
            0xdc, 0xdb, 0xdb, 0xdb, 0xdf, 0xdb, 0xdb,
        ];
        let s = Codepage::Cp437.decode(guru);
        assert!(s.starts_with('▄'));
        assert!(s.contains('▀'));
        assert!(s.contains('█'));
        // Round-trip the embedded literal space.
        assert!(s.contains(' '));
    }

    #[test]
    fn decode_latin1_accents() {
        assert_eq!(Codepage::Latin1.decode(b"Caf\xe9"), "Café");
        assert_eq!(Codepage::Latin1.decode(b"Fran\xe7ois"), "François");
        assert_eq!(Codepage::Latin1.decode(b"\xc4rger"), "Ärger");
    }

    #[test]
    fn decode_cp437_high_letters() {
        // 0x82 is é in CP437 — a name like 'Café' written by a PC
        // composer would have these bytes.
        assert_eq!(Codepage::Cp437.decode(b"Caf\x82"), "Café");
        // 0xE1 is ß in CP437 (Greek/German overlap with the
        // sharp-s; the table maps to ß).
        assert_eq!(Codepage::Cp437.decode(&[0xe1]), "ß");
    }

    #[test]
    fn decode_name_strips_nul_and_trailing_space() {
        // 22-byte slot, name padded with NUL after position 5.
        // Leading space is preserved (could be intentional
        // alignment); trailing field-padding NULs go via the
        // C-string cut.
        let bytes: &[u8] = b" Lead\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";
        assert_eq!(Codepage::Ascii.decode_name(bytes), " Lead");

        // Trailing ASCII space (no NUL) is stripped — that's
        // field padding, not part of the name.
        let bytes: &[u8] = b"Lead   ";
        assert_eq!(Codepage::Ascii.decode_name(bytes), "Lead");

        // Mixed: NUL terminates first, but the bytes before
        // the NUL include trailing spaces → those are stripped.
        let bytes: &[u8] = b"Lead   \0\0\0";
        assert_eq!(Codepage::Ascii.decode_name(bytes), "Lead");

        // Leading spaces survive — scene composers used them
        // to center sample-name lines under ASCII-art logos.
        let bytes: &[u8] = b"       POLAND\0\0\0\0\0\0\0\0\0";
        assert_eq!(Codepage::Ascii.decode_name(bytes), "       POLAND");

        // CP437 art runs all the way to NUL → preserved entirely.
        let bytes: &[u8] = &[0xdb, 0xdb, 0xdb, 0x00, 0x00];
        let s = Codepage::Cp437.decode_name(bytes);
        assert_eq!(s, "███");
    }

    #[test]
    fn autodetect_round_trip_guru() {
        // End-to-end: detect + decode a GURU.MOD-like field.
        let bytes: &[u8] = &[
            0xdc, 0xdb, 0xdb, 0xdb, 0xdf, 0xdb, 0xdb, 0xdc, 0x20, 0xdc, 0xdb, 0xdb, 0xdc, 0x20,
            0xdc, 0xdb, 0xdb, 0xdb, 0xdf, 0xdb, 0xdb, 0x00,
        ];
        let s = decode_autodetect(bytes);
        // No replacement characters — the logo survived.
        assert!(!s.contains('\u{FFFD}'));
        // No trailing NUL.
        assert!(!s.contains('\0'));
        // No trailing whitespace.
        assert_eq!(s, s.trim());
    }

    // ---- CP437 table sanity ------------------------------

    #[test]
    fn cp437_table_has_no_replacement_chars() {
        // Every entry in the table must be a real glyph, not
        // U+FFFD or U+0000 — those would indicate a transcription
        // error.
        for (i, &c) in CP437_HIGH.iter().enumerate() {
            assert_ne!(c, '\u{FFFD}', "entry 0x{:02x} is U+FFFD", 0x80 + i);
            assert_ne!(c, '\0', "entry 0x{:02x} is NUL", 0x80 + i);
        }
    }
}
