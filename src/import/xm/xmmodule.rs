/// Original XM Module
use crate::import::bin_reader::ImportError;

use alloc::format;
use alloc::{vec, vec::Vec};

use super::xmheader::{XmFlagType, XmHeader};
use super::xminstrument::XmInstrument;
use super::xmpattern::XmPattern;
use super::xmsample::XMSAMPLE_HEADER_SIZE;

use crate::codepage::Codepage;
use crate::compatibility_profile::CompatibilityProfile;
use crate::fixed::units::Volume;
use crate::import::import_memory::{ImportMemory, MemoryType};
use crate::import::orders_helper;
use crate::import::patternslot::PatternSlot;
use crate::module::Module;
use crate::period_helper::FrequencyType;

#[derive(Default, Debug)]
pub struct XmModule {
    pub header: XmHeader,
    pub pattern_order: Vec<u8>,
    pub pattern: Vec<XmPattern>,
    pub instrument: Vec<XmInstrument>,
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
        let mut pattern: Vec<XmPattern> = vec![];
        for _i in 0..header.number_of_patterns {
            let (d2, xmp) = XmPattern::load(data, header.number_of_channels)?;
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
            // all on. Editor-authored modules that leave `profile`
            // at default get clean playback without any of these
            // historical edges.
            profile: CompatibilityProfile::ft2(),
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
            pattern_highlight: crate::module::PatternHighlight::default(),
            // XM has no mix-plugin section.
            mix_plugins: None,
            // XM doesn't carry a MIDI pitch-wheel-depth field. The
            // GM default of 2 semitones is what any MIDI receiver
            // would assume in the absence of an explicit RPN setup —
            // matches the `Module::default()` value.
            pitch_wheel_depth: 2,
            // XM has no mix-volume byte in its header. Schism
            // (`fmt/xm.c:885`) hard-codes `mixing_volume = 48` for
            // every imported XM, which normalised gives 48/128 =
            // 0.375. We mirror that so the player's mixer chain
            // applies the same headroom for XM as for IT — the same
            // engine-side `MIXING_ATTENUATION` shift expects this
            // pre-attenuation as input.
            mix_volume: Volume::from_ratio(48, 128),
            // DAW layer is filled below by `build_timeline_layer`.
            tracks: vec![],
            clips: crate::daw::sorted_clips::SortedClips::new(),
            automation: vec![],
            timeline_map: crate::daw::timeline::TimelineMap::default(),
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
        // when the song reaches the end. Realised below as a
        // synthetic `GlobalEffect::PositionJump` on the last played
        // row, so the existing position-jump pipeline handles it.
        let mut pattern_order_owned = pattern_order;
        let mut pattern_owned = pattern;
        crate::daw::build_timeline::inject_restart_position_jump(
            &mut pattern_order_owned[0],
            &mut pattern_owned,
            self.header.restart_position as usize,
        );

        crate::daw::build_timeline::build_timeline_layer(
            &mut module,
            &pattern_order_owned,
            &pattern_owned,
        );

        module
    }
}
