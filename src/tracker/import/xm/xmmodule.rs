//! FastTracker II (.XM) file loader — entry point for the
//! `import_xm` feature. Parses the `Extended Module` 60-byte
//! header, the global pattern-header (orders, default tempo /
//! BPM, restart byte), the patterns themselves, and the
//! instrument blocks (each carrying any number of samples).
//! Produces a `Module` populated with
//! `crate::tracker::profiles::ft2()` quirks.

use crate::tracker::import::bin_reader::ImportError;

use alloc::format;
use alloc::{vec, vec::Vec};

use super::xmheader::{XmFlagType, XmHeader};
use super::xminstrument::XmInstrument;
use super::xmpattern::XmPattern;
use super::xmsample::XMSAMPLE_HEADER_SIZE;

use crate::core::fixed::units::Volume;
use crate::core::module::Module;
use crate::tracker::codepage::Codepage;
use crate::tracker::import::memory::{ImportMemory, MemoryType};
use crate::tracker::import::orders_helper;
use crate::tracker::import::patternslot::PatternSlot;
use crate::tracker::period::FrequencyType;

#[derive(Default, Debug)]
pub struct XmModule {
    header: XmHeader,
    pattern_order: Vec<u8>,
    pattern: Vec<XmPattern>,
    instrument: Vec<XmInstrument>,
}

impl XmModule {
    pub fn load(data: &[u8]) -> Result<Self, ImportError> {
        // Keep the original slice for the post-load codepage pass —
        // we need byte-accurate offsets back into the file to
        // re-decode every name field under the detected codepage.
        let original_data = data;

        let (data_after_header, header, pattern_order) = XmHeader::load(data)?;
        // Track absolute byte offset within `original_data` as we
        // walk the variable-length records.
        let mut cursor = original_data.len() - data_after_header.len();
        let mut data = data_after_header;

        // Create patterns from xm
        //
        // Whole-module allocation budget: a pattern's row count is
        // declared in a 9-byte header, so a few KB of input can ask
        // for 256 patterns of `XM_MAX_PATTERN_ROWS` rows each and
        // turn the empty-row padding into gigabytes of
        // `PatternSlot` (OOM DoS). Cap the module at the slot
        // envelope of a maximal *format-conformant* file — 256
        // patterns x 256 rows x 64 channels — which no genuine
        // module reaches, whatever the shape of its patterns.
        const MAX_TOTAL_SLOTS: usize = 256 * 256 * 64;
        let mut total_slots: usize = 0;
        let mut pattern: Vec<XmPattern> = vec![];
        for _i in 0..header.number_of_patterns {
            let (d2, xmp) = XmPattern::load(data, header.number_of_channels)?;
            total_slots += xmp.pattern.len() * header.number_of_channels as usize;
            if total_slots > MAX_TOTAL_SLOTS {
                return Err(ImportError::OutOfRange(
                    "XmModule: total pattern rows exceed the XM allocation budget",
                ));
            }
            cursor += data.len() - d2.len();
            data = d2;
            pattern.push(xmp);
        }

        // Add empty patterns
        if pattern_order.len() > pattern.len() {
            let empty_ones = pattern_order.len() - pattern.len();
            let empty = XmPattern::new(64, header.number_of_channels.into());
            pattern.extend(core::iter::repeat_n(empty, empty_ones));
        }

        // Track the file-relative start offset of each instrument
        // record. Needed by the codepage pass to find the
        // instrument name (at `+4`, 22 bytes) and each sample's
        // name (at `instrument_header_len + i * XMSAMPLE_HEADER_SIZE
        // + 18`, 22 bytes — the layout of `XmSampleHeader`).
        let mut instrument_starts: Vec<usize> =
            Vec::with_capacity(header.number_of_instruments as usize);
        let mut instrument: Vec<XmInstrument> = vec![];
        for _i in 0..header.number_of_instruments {
            instrument_starts.push(cursor);
            // Create instruments form xm
            let (d2, xmi) = XmInstrument::load(data)?;
            cursor += data.len() - d2.len();
            data = d2;
            instrument.push(xmi);
        }

        let mut xm = XmModule {
            header,
            pattern_order,
            pattern,
            instrument,
        };

        // ---- codepage detection + re-decode ----
        //
        // XM was authored on Fasttracker II (DOS/CP437), and
        // every text field on disk is an 8-bit fixed-width
        // slot. The detector pools every name in the file —
        // header `name` (20 bytes @ offset 17), header
        // `tracker_name` (20 @ 38), every instrument name (22 @
        // `instr_start + 4`), and every sample name (22 @
        // `instr_start + instrument_header_len + sample_i * 40
        // + 18`) — and picks the single codepage that best
        // fits the aggregate byte distribution. The constants
        // below come from the on-disk layouts of `XmHeader`,
        // `XmInstrumentHeader`, and `XmSampleHeader`.
        const HEADER_NAME_OFF: usize = 17;
        const HEADER_NAME_LEN: usize = 20;
        const HEADER_TRACKER_NAME_OFF: usize = 38;
        const HEADER_TRACKER_NAME_LEN: usize = 20;
        const INSTR_NAME_OFF_IN_RECORD: usize = 4; // 4-byte length prefix
        const INSTR_NAME_LEN: usize = 22;
        const SAMPLE_NAME_OFF_IN_HEADER: usize = 18; // 4+4+4+1+1+1+1+1+1 = 18
        const SAMPLE_NAME_LEN: usize = 22;

        let n = original_data.len();
        let mut text_fields: Vec<&[u8]> = Vec::new();
        if n >= HEADER_NAME_OFF + HEADER_NAME_LEN {
            text_fields.push(&original_data[HEADER_NAME_OFF..HEADER_NAME_OFF + HEADER_NAME_LEN]);
        }
        if n >= HEADER_TRACKER_NAME_OFF + HEADER_TRACKER_NAME_LEN {
            text_fields.push(
                &original_data
                    [HEADER_TRACKER_NAME_OFF..HEADER_TRACKER_NAME_OFF + HEADER_TRACKER_NAME_LEN],
            );
        }
        for (i, &start) in instrument_starts.iter().enumerate() {
            let name_start = start + INSTR_NAME_OFF_IN_RECORD;
            if name_start + INSTR_NAME_LEN <= n {
                text_fields.push(&original_data[name_start..name_start + INSTR_NAME_LEN]);
            }
            let xmih_len = xm.instrument[i].instrument_header_len as usize;
            for s_i in 0..xm.instrument[i].sample.len() {
                let sample_header_start = start + xmih_len + s_i * XMSAMPLE_HEADER_SIZE;
                let name_start = sample_header_start + SAMPLE_NAME_OFF_IN_HEADER;
                if name_start + SAMPLE_NAME_LEN <= n {
                    text_fields.push(&original_data[name_start..name_start + SAMPLE_NAME_LEN]);
                }
            }
        }
        let codepage = Codepage::detect_from_fields(&text_fields);

        // Re-decode every field with the detected codepage.
        if n >= HEADER_NAME_OFF + HEADER_NAME_LEN {
            xm.header.name = codepage
                .decode_name(&original_data[HEADER_NAME_OFF..HEADER_NAME_OFF + HEADER_NAME_LEN]);
        }
        if n >= HEADER_TRACKER_NAME_OFF + HEADER_TRACKER_NAME_LEN {
            xm.header.tracker_name = codepage.decode_name(
                &original_data
                    [HEADER_TRACKER_NAME_OFF..HEADER_TRACKER_NAME_OFF + HEADER_TRACKER_NAME_LEN],
            );
        }
        for (i, &start) in instrument_starts.iter().enumerate() {
            let name_start = start + INSTR_NAME_OFF_IN_RECORD;
            if name_start + INSTR_NAME_LEN <= n {
                xm.instrument[i].header.name =
                    codepage.decode_name(&original_data[name_start..name_start + INSTR_NAME_LEN]);
            }
            let xmih_len = xm.instrument[i].instrument_header_len as usize;
            for s_i in 0..xm.instrument[i].sample.len() {
                let sample_header_start = start + xmih_len + s_i * XMSAMPLE_HEADER_SIZE;
                let name_start = sample_header_start + SAMPLE_NAME_OFF_IN_HEADER;
                if name_start + SAMPLE_NAME_LEN <= n {
                    let name = codepage
                        .decode_name(&original_data[name_start..name_start + SAMPLE_NAME_LEN]);
                    xm.instrument[i].sample[s_i].set_name(name);
                }
            }
        }

        Ok(xm)
    }

    pub fn to_module(&self) -> Module {
        // Create module from xm
        let mut module = Module {
            name: self.header.name.clone(),
            comment: format!(
                "{} ({}.{:02})",
                self.header.tracker_name,
                self.header.version_number >> 8,
                self.header.version_number & 0xFF
            ),
            // FT2 canonical replay behaviour — every XM module is
            // authored against these, so the XM importer opts them
            // all on. Editor-authored modules that leave `quirks`
            // at default get clean playback without any of these
            // historical edges.
            quirks: crate::tracker::profiles::ft2(),
            origin: Some(crate::tracker::format::ModuleFormat::Xm),
            frequency_type: match self.header.flags {
                XmFlagType::XmAmigaFrequencies => FrequencyType::AmigaFrequencies,
                XmFlagType::XmLinearFrequencies => FrequencyType::LinearFrequencies,
            },
            default_tempo: self.header.default_tempo as usize,
            default_bpm: self.header.default_bpm as usize,
            channel_names: vec![],
            // XM has no per-channel defaults in its header — every
            // channel starts centred and unmuted.
            channel_defaults: vec![],
            instrument: vec![],
            // XM has no MIDI-macro concept — the feature is
            // IT-specific.
            midi_macros: None,
            // XM has no pattern-highlight metadata in its header —
            // inherit the editor-friendly default cadence (4/16).
            pattern_highlight: crate::core::module::PatternHighlight::default(),
            // XM has no mix-plugin section. The field itself only exists when
            // the IT importer is compiled in — it holds IT's opaque host-only
            // VST chunks — so the gate has to be repeated here: `import_xm`
            // alone must still build.
            #[cfg(feature = "import_it")]
            mix_plugins: None,
            // XM doesn't carry a MIDI pitch-wheel-depth field. The
            // GM default of 2 semitones is what any MIDI receiver
            // would assume in the absence of an explicit RPN setup —
            // matches the `Module::default()` value.
            pitch_wheel_depth: 2,
            // XM has no mix-volume byte in its header, so this is a
            // convention constant: the master gain that makes our mixer
            // match FT2's output level. Calibrated against **ft2-clone**
            // (the cycle-accurate FT2 reproduction) at its factory amp —
            // rendering a 12-module sample through ft2-clone vs our
            // player put us a tight −1.48 dB (median, env-corr > 0.9)
            // below it at the old 48/128, so 48/128 × 10^(1.48/20) ≈
            // 57/128 centres XM on FT2. See `tracker/import/it/BASELINE.md`.
            mix_volume: Volume::from_ratio(57, 128),
            // DAW layer is filled below by `build_timeline_layer`.
            tracks: vec![],
            clips: crate::core::daw::sorted_clips::SortedClips::new(),
            automation: vec![],
            timeline_map: crate::core::daw::timeline::TimelineMap::default(),
            // Phase 3a schema seat; Phase 3b will fill this from the
            // XM restart byte.
            song_loop_to: None,
            // Tracker formats wrap as a whole (song_loop_to); no
            // per-lane loops.
            channel_loops: alloc::vec::Vec::new(),
            channel_inserts: alloc::vec::Vec::new(),
            buses: alloc::vec::Vec::new(),
            channel_sends: alloc::vec::Vec::new(),
            master_chain: crate::core::daw::device::DeviceChain::default(),
            assets: alloc::vec::Vec::new(),
        };

        let raw_patterns: Vec<Vec<Vec<PatternSlot>>> =
            self.pattern.iter().map(|p| p.pattern.clone()).collect();
        let pattern_order = orders_helper::parse_orders(&self.pattern_order);
        let mut im = ImportMemory::default();
        let pattern = im.unpack_patterns(
            module.frequency_type,
            MemoryType::Xm,
            &pattern_order,
            &raw_patterns,
        );

        for i in &self.instrument {
            module.instrument.push(i.to_instrument())
        }

        // FastTracker's restart_position: order index to jump to
        // when the song reaches the end. DAW migration
        // Phase 3c.2 step: no longer injects a synthetic
        // `NavigationEffect::PositionJump` on the last row — the
        // sequencer's `post_pattern_change` handles the wrap
        // natively via `module.song_loop_to`. Modules with
        // `restart_position = 0` get `song_loop_to = None` and
        // wrap to order 0 (the default).
        crate::tracker::import::build::build_timeline_layer(&mut module, &pattern_order, &pattern);
        crate::tracker::import::build::set_song_loop_to_restart_byte(
            &mut module,
            0,
            self.header.restart_position as usize,
        );

        module
    }
}

impl crate::tracker::import::raw::RawModule for XmModule {
    fn raw_title(&self) -> &str {
        &self.header.name
    }

    fn raw_orders(&self) -> &[u8] {
        // Already truncated to the header's `song_length` by
        // `XmHeader::get_pattern_order`.
        &self.pattern_order
    }

    fn raw_pattern_count(&self) -> usize {
        self.pattern.len()
    }

    fn raw_pattern(&self, idx: usize) -> Option<&[Vec<PatternSlot>]> {
        self.pattern.get(idx).map(|p| p.pattern.as_slice())
    }

    fn raw_sample_count(&self) -> usize {
        self.instrument.iter().map(|i| i.sample.len()).sum()
    }

    fn raw_sample(&self, idx: usize) -> Option<crate::tracker::import::raw::RawSample<'_>> {
        use crate::tracker::import::raw::{RawPcm, RawSample};

        // XM nests samples inside instruments; flatten in file order
        // so the index is stable and comparable with the flat formats.
        let mut rest = idx;
        for instr in &self.instrument {
            match instr.sample.get(rest) {
                Some(s) => {
                    return Some(RawSample {
                        name: s.name(),
                        pcm: s.data().map(RawPcm::from).unwrap_or(RawPcm::Empty),
                        loop_start: s.loop_start_frames(),
                        loop_length: s.loop_length_frames(),
                        // XM expresses pitch as relative-note +
                        // finetune against a fixed reference, never
                        // as an absolute rate.
                        sample_rate: None,
                    });
                }
                None => rest -= instr.sample.len(),
            }
        }
        None
    }

    fn raw_initial_speed(&self) -> Option<u8> {
        // XM calls it "tempo"; it is the ticks-per-row register that
        // every other format calls speed. Stored as u16, but the
        // meaningful range is a byte.
        Some(self.header.default_tempo.min(u8::MAX as u16) as u8)
    }

    fn raw_initial_bpm(&self) -> Option<u8> {
        Some(self.header.default_bpm.min(u8::MAX as u16) as u8)
    }

    fn raw_channel_count(&self) -> usize {
        self.header.number_of_channels as usize
    }

    fn raw_tracker(&self) -> crate::tracker::import::raw::RawTracker<'_> {
        crate::tracker::import::raw::RawTracker {
            tag: if self.header.tracker_name.is_empty() {
                None
            } else {
                Some(&self.header.tracker_name)
            },
            version: Some(self.header.version_number),
            compatible_with: None,
        }
    }

    fn raw_instrument_count(&self) -> usize {
        self.instrument.len()
    }

    fn raw_instrument_name(&self, idx: usize) -> Option<&str> {
        self.instrument.get(idx).map(|i| i.header.name.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smallest XM that parses: a header, then `patterns` empty
    /// pattern headers declaring `rows` rows each.
    fn xm_bytes(patterns: u16, rows: u16, channels: u16) -> Vec<u8> {
        let mut b: Vec<u8> = Vec::new();
        b.extend_from_slice(b"Extended Module: ");
        b.extend_from_slice(&[b' '; 20]); // module name
        b.push(0x1a);
        b.extend_from_slice(&[b' '; 20]); // tracker name
        b.extend_from_slice(&0x0104u16.to_le_bytes());
        b.extend_from_slice(&20u32.to_le_bytes()); // header_size
        b.extend_from_slice(&0u16.to_le_bytes()); // song_length
        b.extend_from_slice(&0u16.to_le_bytes()); // restart_position
        b.extend_from_slice(&channels.to_le_bytes());
        b.extend_from_slice(&patterns.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes()); // instruments
        b.extend_from_slice(&1u16.to_le_bytes()); // linear frequencies
        b.extend_from_slice(&6u16.to_le_bytes()); // tempo
        b.extend_from_slice(&125u16.to_le_bytes()); // bpm
        for _ in 0..patterns {
            b.extend_from_slice(&9u32.to_le_bytes()); // pattern_header_len
            b.push(0); // packing type
            b.extend_from_slice(&rows.to_le_bytes());
            b.extend_from_slice(&0u16.to_le_bytes()); // empty body
        }
        b
    }

    /// An XM whose single 8-bit looped sample declares
    /// `sample_len` bytes but stops after `present` — the shape a
    /// cruncher leaves behind. The body is a run of `1` deltas, so
    /// the decoded PCM is `1, 2, 3, …` and any padded tail shows
    /// up as a hold on the last real value.
    fn xm_truncated_sample(
        sample_len: u32,
        loop_start: u32,
        loop_length: u32,
        present: usize,
    ) -> Vec<u8> {
        let mut b = xm_bytes(0, 0, 4);
        // instruments count: 60-byte preamble + song_length,
        // restart, channels, patterns (2 bytes each).
        b[72..74].copy_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&instr_full(0, sample_len, loop_start, loop_length, present));
        b
    }

    /// One 248-byte instrument record (4 + 25 + 4 +
    /// `XMINSTRDEFAULT_SIZE`) carrying a single 8-bit looped
    /// sample, followed by `present` bytes of body.
    fn instr_full(
        name_byte: u8,
        sample_len: u32,
        loop_start: u32,
        loop_length: u32,
        present: usize,
    ) -> Vec<u8> {
        let mut b: Vec<u8> = Vec::new();
        b.extend_from_slice(&248u32.to_le_bytes()); // instrument_header_len
        b.extend_from_slice(&[name_byte; 22]); // name
        b.push(0); // instr_type
        b.extend_from_slice(&1u16.to_le_bytes()); // num_samples
        b.extend_from_slice(&40u32.to_le_bytes()); // sample_header_size
        b.extend_from_slice(&[0u8; 215]); // XmInstrDefault

        // XmSampleHeader
        b.extend_from_slice(&sample_len.to_le_bytes());
        b.extend_from_slice(&loop_start.to_le_bytes());
        b.extend_from_slice(&loop_length.to_le_bytes());
        b.push(64); // volume
        b.push(0); // finetune
        b.push(1); // flags: forward loop, 8-bit
        b.push(128); // panning
        b.push(0); // relative_pitch
        b.push(0); // reserved
        b.extend_from_slice(&[0u8; 22]); // name

        // Body, cut short.
        b.extend_from_slice(&vec![1u8; present]);
        b
    }

    fn pcm8(xm: &XmModule) -> Vec<i8> {
        match xm.instrument[0].sample[0].to_sample().data {
            Some(crate::core::sample::SampleDataType::Mono8(d)) => d.to_vec(),
            _ => panic!("expected 8-bit PCM"),
        }
    }

    #[test]
    fn a_tail_cut_after_the_loop_end_still_loads() {
        // 16 bytes declared, loop 4..12, file stops at 12: the
        // loop is intact, only the one-shot tail is gone. FT2
        // loads this; so must we.
        let xm = XmModule::load(&xm_truncated_sample(16, 4, 8, 12)).unwrap();
        let s = xm.instrument[0].sample[0].to_sample();
        assert_eq!(s.loop_start, 4);
        assert_eq!(s.loop_length, 8);

        let pcm = pcm8(&xm);
        assert_eq!(pcm.len(), 16, "declared length must be rebuilt");
        assert_eq!(&pcm[..12], &(1..=12).collect::<Vec<i8>>()[..]);
        // Padded tail holds the last real value — no click.
        assert!(pcm[12..].iter().all(|&v| v == 12), "{:?}", pcm);
    }

    #[test]
    fn a_tail_cut_inside_the_loop_keeps_the_loop_period() {
        // 16 bytes declared, loop 4..14, file stops at 6. Without
        // the pad, `to_sample` would shrink the loop to 4..6 and
        // the sustained note would come out at the wrong pitch.
        let xm = XmModule::load(&xm_truncated_sample(16, 4, 10, 6)).unwrap();
        let s = xm.instrument[0].sample[0].to_sample();
        assert_eq!(s.loop_start, 4);
        assert_eq!(s.loop_length, 10, "loop period must survive the cut");

        let pcm = pcm8(&xm);
        assert_eq!(pcm.len(), 16);
        assert_eq!(&pcm[..6], &[1, 2, 3, 4, 5, 6]);
        assert!(pcm[6..].iter().all(|&v| v == 6), "{:?}", pcm);
    }

    /// Byte offsets inside `xm_truncated_sample`: a 60-byte
    /// header, no order table (`header_size` = 20), then the
    /// instrument record (248 bytes of fixed header) and its one
    /// 40-byte sample header.
    const INSTR_AT: usize = 80;
    const SAMPLE_HDR_AT: usize = INSTR_AT + 248;

    fn cut(mut b: Vec<u8>, at: usize) -> Vec<u8> {
        b.truncate(at);
        b
    }

    #[test]
    fn an_eight_byte_destroyed_instrument_does_not_eat_the_next_one() {
        // BoobieSqueezer's "Destroy Instrument" rewrites an unused
        // instrument as an 8-byte header. FT2 loads the result, so
        // the record's declared length — not a fixed 25-byte read
        // — must bound the parse: otherwise `num_samples` is taken
        // from the NEXT instrument's name bytes (0x4141 here) and
        // that instrument is parsed as sample headers and lost.
        let mut b = xm_bytes(0, 0, 4);
        b[72..74].copy_from_slice(&2u16.to_le_bytes()); // 2 instruments
        b.extend_from_slice(&[8, 0, 0, 0, 0xAA, 0xAA, 0xAA, 0xAA]); // destroyed
        b.extend_from_slice(&instr_full(b'A', 8, 0, 0, 8)); // intact

        let xm = XmModule::load(&b).unwrap();
        assert_eq!(xm.instrument.len(), 2);
        assert!(xm.instrument[0].sample.is_empty(), "stub must stay empty");
        assert_eq!(xm.instrument[1].sample.len(), 1, "second must survive");
        assert_eq!(
            xm.instrument[1].sample[0].to_sample().data.unwrap().len(),
            8
        );
    }

    #[test]
    fn a_cut_inside_a_sample_header_still_loads() {
        // The 40-byte sample header itself is severed halfway.
        // FT2 reads the missing half as zeros; so do we.
        let b = cut(xm_truncated_sample(16, 4, 8, 16), SAMPLE_HDR_AT + 20);
        let xm = XmModule::load(&b).unwrap();
        assert_eq!(xm.instrument.len(), 1);
        assert_eq!(xm.instrument[0].sample.len(), 1);
        // Nothing was left for a body, and nothing is minted from
        // zero bytes — the sample is simply empty.
        assert!(xm.instrument[0].sample[0].is_empty());
    }

    #[test]
    fn a_cut_inside_the_instrument_header_still_loads() {
        // Ten bytes into the instrument record: past the length
        // tag, inside the name. A zeroed remainder means
        // `num_samples == 0`, i.e. an empty instrument.
        let b = cut(xm_truncated_sample(16, 4, 8, 16), INSTR_AT + 10);
        let xm = XmModule::load(&b).unwrap();
        assert_eq!(xm.instrument.len(), 1);
        assert!(xm.instrument[0].sample.is_empty());
    }

    #[test]
    fn sample_headers_are_never_minted_past_eof() {
        // `num_samples` is file data: 64 declared, file ends right
        // after the instrument header. Tolerating a *partial*
        // record is FT2 parity; conjuring 64 whole ones out of
        // zero remaining bytes is an allocation vector.
        let mut b = xm_truncated_sample(16, 4, 8, 16);
        b[INSTR_AT + 27..INSTR_AT + 29].copy_from_slice(&64u16.to_le_bytes());
        let xm = XmModule::load(&cut(b, SAMPLE_HDR_AT)).unwrap();
        assert!(xm.instrument[0].sample.is_empty());
    }

    #[test]
    fn an_implausible_declared_length_is_clamped_not_allocated() {
        // 2 GiB declared out of 8 bytes present: a corrupt header,
        // not a crunched tail. The pad budget refuses it and we
        // keep what is there rather than minting the difference.
        let xm = XmModule::load(&xm_truncated_sample(0x8000_0000, 0, 0, 8)).unwrap();
        assert_eq!(pcm8(&xm).len(), 8);
    }

    #[test]
    fn patterns_longer_than_256_rows_load() {
        // FT2 stops at 256 rows, other trackers do not, and their
        // files play everywhere else — so they must load here too.
        let xm = XmModule::load(&xm_bytes(1, 1024, 8)).unwrap();
        assert_eq!(xm.pattern[0].pattern.len(), 1024);
        assert_eq!(xm.pattern[0].pattern[1023].len(), 8);
    }

    #[test]
    fn row_count_past_the_ceiling_is_clamped_not_rejected() {
        let xm = XmModule::load(&xm_bytes(1, 5000, 4)).unwrap();
        assert_eq!(xm.pattern[0].pattern.len(), 4096);
    }

    #[test]
    fn a_zero_row_pattern_is_the_64_row_pattern_ft2_loads() {
        let xm = XmModule::load(&xm_bytes(1, 0, 4)).unwrap();
        assert_eq!(xm.pattern[0].pattern.len(), 64);
    }

    #[test]
    fn declared_rows_cannot_outgrow_the_module_budget() {
        // 32 x 4096 rows x 64 channels asks for ~8M slots out of a
        // few hundred header bytes: the padding, not the file, is
        // what allocates.
        let err = XmModule::load(&xm_bytes(32, 4096, 64)).unwrap_err();
        assert!(matches!(err, ImportError::OutOfRange(_)), "{:?}", err);
    }
}
