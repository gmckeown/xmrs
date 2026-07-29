//! The pre-DAW [`RawModule`] view must agree with the normalised
//! `Module` on everything both of them describe.
//!
//! The view exists so corpus tools can read what the *file* said
//! without paying for the DAW build. That is only worth anything if
//! the two never disagree about the same fact — a fingerprint built
//! on the raw layer would otherwise silently describe a different
//! song than the one the player renders.

use xmrs::prelude::*;
use xmrs::tracker::import::raw::{RawModule, RawPcm};
use xmrs::tracker::import::xm::xmmodule::XmModule;

const NOTE_XM: &[u8] = include_bytes!("../examples/note.xm");
const EMPTY_XM: &[u8] = include_bytes!("../examples/empty.xm");

#[test]
fn raw_title_matches_normalised() {
    let raw = XmModule::load(NOTE_XM).expect("note.xm should load");
    let module = raw.to_module();
    assert_eq!(raw.raw_title(), module.name);
}

#[test]
fn raw_orders_are_truncated_to_song_length() {
    let raw = XmModule::load(NOTE_XM).expect("note.xm should load");
    // Not the fixed on-disk table: every entry must index a real
    // pattern, which is the property that makes the order list
    // usable as a signature without further filtering.
    assert!(!raw.raw_orders().is_empty());
    for &ord in raw.raw_orders() {
        assert!(
            (ord as usize) < raw.raw_pattern_count(),
            "order entry {ord} past the {} patterns",
            raw.raw_pattern_count()
        );
    }
}

#[test]
fn raw_patterns_are_rectangular_row_by_channel() {
    let raw = XmModule::load(NOTE_XM).expect("note.xm should load");
    assert_eq!(raw.raw_patterns().count(), raw.raw_pattern_count());

    for (idx, pattern) in raw.raw_patterns().enumerate() {
        assert!(!pattern.is_empty(), "pattern {idx} has no rows");
        // Row count may vary between patterns, but the channel count
        // may not vary inside one.
        let channels = pattern[0].len();
        for (row_idx, row) in pattern.iter().enumerate() {
            assert_eq!(
                row.len(),
                channels,
                "pattern {idx} row {row_idx} is a different width"
            );
        }
    }
}

/// The one piece of real logic in the XM impl: XM nests samples
/// inside instruments, so `raw_sample(i)` walks a flattened index
/// across instrument boundaries. Check it against the same flatten
/// derived from the normalised `Module`, which is an independent
/// path to the same data.
#[test]
fn raw_samples_flatten_across_instruments_in_file_order() {
    let raw = XmModule::load(NOTE_XM).expect("note.xm should load");
    let module = raw.to_module();

    let expected: Vec<(String, usize)> = module
        .instrument
        .iter()
        .filter_map(|i| match &i.instr_type {
            InstrumentType::Default(idef) => Some(idef),
            _ => None,
        })
        .flat_map(|idef| idef.sample.iter().flatten())
        .map(|s| {
            (
                s.name.clone(),
                s.data.as_ref().map(|d| d.len()).unwrap_or(0),
            )
        })
        .collect();

    assert_eq!(raw.raw_sample_count(), expected.len());
    assert_eq!(raw.raw_samples().count(), expected.len());

    // The indexed accessor and the iterator must agree.
    for (idx, from_iter) in raw.raw_samples().enumerate() {
        let from_index = raw.raw_sample(idx).expect("sample in range");
        assert_eq!(from_iter.name, from_index.name, "sample {idx}");
        assert_eq!(from_iter.pcm, from_index.pcm, "sample {idx}");
        assert_eq!(
            (from_index.name.to_string(), from_index.pcm.len()),
            expected[idx],
            "sample {idx}"
        );
    }

    // One past the end is None, not a panic or a wrap.
    assert!(raw.raw_sample(expected.len()).is_none());
}

/// An instrument slot is not a sample slot on XM, and the names live
/// in different places.
#[test]
fn instrument_count_is_not_the_sample_count() {
    let raw = XmModule::load(NOTE_XM).expect("note.xm should load");
    let module = raw.to_module();

    assert_eq!(raw.raw_instrument_count(), module.instrument.len());
    for (idx, instr) in module.instrument.iter().enumerate() {
        assert_eq!(raw.raw_instrument_name(idx), Some(instr.name.as_str()));
    }
    assert!(raw
        .raw_instrument_name(raw.raw_instrument_count())
        .is_none());
}

/// The raw PCM is the same payload the normalised `Sample` carries.
/// This is the load-bearing claim for cross-format work: sample data
/// is the one thing comparable between MOD, XM, S3M and IT.
#[test]
fn raw_pcm_matches_normalised_sample_data() {
    let raw = XmModule::load(NOTE_XM).expect("note.xm should load");
    let module = raw.to_module();

    let mut flat = 0usize;
    for instr in &module.instrument {
        let InstrumentType::Default(idef) = &instr.instr_type else {
            continue;
        };
        for sample in idef.sample.iter().flatten() {
            let got = raw.raw_sample(flat).expect("raw sample in range");
            match (&got.pcm, sample.data.as_ref()) {
                (RawPcm::Mono8(a), Some(SampleDataType::Mono8(b))) => assert_eq!(*a, &b[..]),
                (RawPcm::Mono16(a), Some(SampleDataType::Mono16(b))) => assert_eq!(*a, &b[..]),
                (RawPcm::Empty, None) => {}
                (RawPcm::Empty, Some(d)) => assert_eq!(d.len(), 0),
                (a, b) => panic!("sample {flat}: raw {a:?} vs normalised {b:?}"),
            }
            flat += 1;
        }
    }
    assert_eq!(flat, raw.raw_sample_count());
}

/// The tempo pair is the header's, and must agree with what the
/// normalised `Module` starts at.
#[test]
fn initial_tempo_matches_normalised() {
    let raw = XmModule::load(NOTE_XM).expect("note.xm should load");
    let module = raw.to_module();

    assert_eq!(
        raw.raw_initial_speed().map(usize::from),
        Some(module.default_tempo)
    );
    assert_eq!(
        raw.raw_initial_bpm().map(usize::from),
        Some(module.default_bpm)
    );
}

/// XM states its channel count in the header; it must match what the
/// patterns are actually shaped like, or one of the two is lying.
#[test]
fn channel_count_agrees_with_pattern_width() {
    let raw = XmModule::load(NOTE_XM).expect("note.xm should load");
    let from_header = raw.raw_channel_count();
    assert!(from_header > 0);

    for (idx, pattern) in raw.raw_patterns().enumerate() {
        for (row_idx, row) in pattern.iter().enumerate() {
            assert_eq!(
                row.len(),
                from_header,
                "pattern {idx} row {row_idx} is {} wide, header says {from_header}",
                row.len()
            );
        }
    }
}

#[test]
fn tracker_tag_is_reported_for_xm() {
    let raw = XmModule::load(NOTE_XM).expect("note.xm should load");
    let tracker = raw.raw_tracker();
    // XM always carries a 20-byte tracker string and a version word.
    assert!(tracker.tag.is_some(), "XM should report a tracker tag");
    assert!(tracker.version.is_some());
    // `compatible_with` is an IT concept; XM must not invent one.
    assert_eq!(tracker.compatible_with, None);
}

/// The IT-only accessors must return "absent" on other formats
/// rather than panicking or fabricating a value. A corpus walker
/// calls these on every file regardless of format.
#[test]
fn it_only_accessors_are_absent_on_xm() {
    let raw = XmModule::load(NOTE_XM).expect("note.xm should load");
    assert_eq!(raw.raw_pattern_name(0), None);
    assert_eq!(raw.raw_channel_name(0), None);
    assert!(raw.raw_edit_history().is_empty());
    assert_eq!(raw.raw_message(), "");
    // XM pitches by relative-note + finetune, never by an absolute rate.
    for sample in raw.raw_samples() {
        assert_eq!(sample.sample_rate, None);
    }
}

/// A module with no instruments and no samples must still answer
/// every query — with empties, not with a panic. Corpus scans hit
/// degenerate files constantly and one panic loses the whole run.
#[test]
fn empty_module_answers_without_panicking() {
    let raw = XmModule::load(EMPTY_XM).expect("empty.xm should load");
    assert_eq!(raw.raw_sample_count(), 0);
    assert!(raw.raw_sample(0).is_none());
    assert!(raw.raw_instrument_name(0).is_none());
    assert_eq!(raw.raw_samples().count(), 0);
    assert_eq!(raw.raw_message(), "");
    assert_eq!(raw.raw_patterns().count(), raw.raw_pattern_count());
    // The accessors added on top must be just as total.
    assert!(raw.raw_initial_speed().is_some());
    assert!(raw.raw_initial_bpm().is_some());
    assert!(raw.raw_edit_history().is_empty());
    assert_eq!(raw.raw_pattern_name(0), None);
    assert_eq!(raw.raw_channel_name(0), None);
    let _ = raw.raw_channel_count();
    let _ = raw.raw_tracker();
}
