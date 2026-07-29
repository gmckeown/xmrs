//! Golden corpus for the native `.xmr` **schema v1** (RFC §7).
//!
//! What the round-trip tests inside `src/format/` cannot see: they write and
//! read with the *same* build, so they stay green even if the meaning of a v1
//! byte changes. This file freezes an actual v1 image on disk and holds every
//! later build to it — that is the whole point of a schema freeze, and it is
//! also the safety net for rewriting the nine model-derive chunks as real wire
//! types in v2 (§5.1).
//!
//! **`genr` is excluded from every comparison.** It carries the writing
//! crate's name and version, so it changes at each release by design; comparing
//! it would turn "the format is stable" into "the version number did not
//! move". Everything else is compared byte for byte, which is stronger than
//! comparing the decoded module and catches an encoder change the model's
//! `PartialEq` would hide.
//!
//! **Re-baking.** A failure here means the v1 wire changed — the answer is
//! normally a `schema_version` bump plus a migration, *not* a new fixture.
//! Re-bake only when the change is deliberate and the format is not yet
//! published:
//!
//! ```sh
//! XMRS_BAKE_GOLDEN=1 cargo test --features format --test format_golden_v1
//! ```
//!
//! **Why this needs an importer**, when the format itself needs none: `orgn`
//! (source-format provenance) is the one chunk whose model field —
//! `Module::origin` — is `#[cfg]`-gated on the importers. Without one the
//! corpus could neither produce that chunk nor re-emit it, so the freeze would
//! silently not cover it. The dependency is on the *field*, not on any import
//! code path: nothing here parses a tracker file.

#![cfg(all(feature = "format", feature = "import_xm"))]

use xmrs::core::daw::automation::{AutomationLane, AutomationTarget, LaneKind, SlideEvent};
use xmrs::core::daw::clip::Clip;
use xmrs::core::daw::device::{Bus, Device, DeviceChain, ReverbMode, Send};
use xmrs::core::daw::loop_region::ChannelLoop;
use xmrs::core::daw::sorted_clips::SortedClips;
use xmrs::core::daw::timeline::{TimelineEntry, TimelineMap};
use xmrs::core::daw::track::Track;
use xmrs::core::fixed::fixed::Q15;
use xmrs::core::fixed::units::{ChannelVolume, Finetune, Panning, Volume};
use xmrs::core::instr_default::InstrDefault;
use xmrs::core::instr_midi::InstrMidi;
use xmrs::core::instr_opl::InstrOpl;
use xmrs::core::instr_sid::InstrSid;
use xmrs::core::instrument::{Instrument, InstrumentType};
use xmrs::core::module::{ChannelDefault, MidiMacros, Module, PatternHighlight};
use xmrs::core::sample::{LoopType, Sample, SampleDataType};
use xmrs::format::{container, document, wire::v1, ChunkId};

/// The frozen v1 image. Empty on a fresh checkout only until it is baked once.
const GOLDEN: &[u8] = include_bytes!("fixtures/format_v1.xmr");

const FIXTURE_PATH: &str = "tests/fixtures/format_v1.xmr";

/// A deterministic module that lights up **every** v1 chunk.
///
/// Deliberately not an imported tracker module: an import can only produce the
/// chunks its format has, and the ones with no tracker equivalent (`GRAF`,
/// `AUTO`, `LOOP`, `ASET`, `MIDI`) are exactly the ones nothing else would
/// freeze. Values are arbitrary but distinctive, so a field that silently stops
/// round-tripping shows up as a byte difference rather than as a default that
/// happens to match.
fn golden_module() -> Module {
    let wave: [i8; 8] = [0, 40, -40, 127, -128, 7, -1, 64];

    let sample = Sample {
        name: "wave".into(),
        relative_pitch: 2,
        finetune: Finetune::ZERO,
        volume: ChannelVolume::FULL,
        default_note_volume: Volume::FULL,
        panning: Panning::CENTER,
        loop_flag: LoopType::Forward,
        loop_start: 1,
        loop_length: 4,
        sustain_loop_flag: LoopType::No,
        sustain_loop_start: 0,
        sustain_loop_length: 0,
        data: Some(SampleDataType::Mono8(wave.to_vec().into())),
    };

    let pcm = InstrDefault {
        sample: vec![Some(sample.clone())],
        ..Default::default()
    };

    let midi = InstrMidi {
        muted: false,
        channel: 9,
        program: 42,
        bank: 3,
        bend: 7,
    };

    let opl = InstrOpl {
        volume: 61,
        relative_pitch: -12,
        ..Default::default()
    };

    let sid = InstrSid {
        fc: 1234,
        band_pass: true,
        ..Default::default()
    };

    let mut module = Module {
        name: "golden v1".into(),
        comment: "every chunk, frozen".into(),
        default_tempo: 6,
        default_bpm: 137,
        pitch_wheel_depth: 12,
        pattern_highlight: PatternHighlight {
            beat: 4,
            measure: 16,
        },
        channel_names: vec!["left".into(), "right".into()],
        channel_defaults: vec![
            ChannelDefault {
                panning: Some(Panning::CENTER),
                volume: None,
                muted: false,
                surround: false,
            },
            ChannelDefault {
                panning: None,
                volume: Some(ChannelVolume::FULL),
                muted: true,
                surround: true,
            },
        ],
        instrument: vec![
            Instrument {
                name: "empty".into(),
                instr_type: InstrumentType::Empty,
                muted: false,
            },
            Instrument {
                name: "pcm".into(),
                instr_type: InstrumentType::Default(pcm),
                muted: false,
            },
            Instrument {
                name: "midi".into(),
                instr_type: InstrumentType::Midi(midi),
                muted: false,
            },
            Instrument {
                name: "opl".into(),
                instr_type: InstrumentType::Opl(opl),
                muted: true,
            },
            Instrument {
                name: "sid".into(),
                instr_type: InstrumentType::Sid(sid),
                muted: false,
            },
        ],
        // MIDI macros: three ragged tables, so a flattening bug shows.
        midi_macros: Some(MidiMacros {
            global: vec![b"F0F00080".to_vec()],
            parametric: vec![b"F0F001z".to_vec(), b"F0F002z".to_vec()],
            fixed: vec![b"F0F1".to_vec()],
        }),
        // Song-level loop back to the top of the second row.
        song_loop_to: Some(1),
        // `orgn`. The field only exists when an importer is compiled in, which
        // is why this test requires one — see the module header.
        origin: Some(xmrs::tracker::format::ModuleFormat::It),
        ..Module::default()
    };

    // QRKS is only written when it differs from the default (§3), so it has to
    // be non-default here or the chunk would be absent from the golden.
    module.quirks.ft2_pitch_slide_overflow = true;
    module.quirks.pitch_slide_ticks_at_row_zero = true;

    // TRKS — two note lanes, one carrying an effect so `TrackEffect` is frozen
    // too.
    module.tracks = vec![
        Track::Notes {
            name: "lead".into(),
            instrument: 1,
            rows: vec![
                xmrs::core::cell::Cell::from_columns(
                    xmrs::core::cell_note::CellNote::Play(xmrs::core::pitch::Pitch::C4),
                    Some(1),
                    Volume::FULL,
                    vec![xmrs::core::effect::TrackEffect::Arpeggio { half1: 3, half2: 7 }],
                ),
                xmrs::core::cell::Cell::from_columns(
                    xmrs::core::cell_note::CellNote::KeyOff,
                    None,
                    Volume::FULL,
                    vec![],
                ),
            ],
            muted: false,
        },
        Track::Notes {
            name: "bass".into(),
            instrument: 4,
            rows: vec![xmrs::core::cell::Cell::from_columns(
                xmrs::core::cell_note::CellNote::Play(xmrs::core::pitch::Pitch::C2),
                Some(4),
                Volume::FULL,
                vec![],
            )],
            muted: false,
        },
    ];

    // CLIP — two placements, deliberately pushed out of order so the sort the
    // reader applies is part of what is frozen.
    module.clips = SortedClips::from_unsorted(vec![
        Clip {
            track: 1,
            song: 0,
            target_channel: 1,
            position_tick: 12,
            speed_at_start: 6,
            track_row_offset: 0,
            source_start_row: 0,
            end_tick: 18,
        },
        Clip {
            track: 0,
            song: 0,
            target_channel: 0,
            position_tick: 0,
            speed_at_start: 6,
            track_row_offset: 0,
            source_start_row: 0,
            end_tick: 12,
        },
    ]);

    // AUTO — a pitch slide lane, the shape the DW arpeggio work produces.
    module.automation = vec![AutomationLane {
        target: AutomationTarget::TrackPitch(0),
        kind: LaneKind::Slide {
            events: vec![
                SlideEvent::Set {
                    tick: 0,
                    rate: Q15::from_raw(-256),
                    fine: false,
                },
                SlideEvent::Clear { tick: 6 },
            ],
        },
        enabled: true,
        scope: Default::default(),
        song: 0,
    }];

    // TMAP — two rows of linearised timeline.
    module.timeline_map = TimelineMap {
        entries: vec![
            TimelineEntry {
                song: 0,
                order_idx: 0,
                pattern_idx: 0,
                row_idx: 0,
                loop_iter: 0,
                tick: 0,
                speed_at_row: 6,
                bpm_at_row: 137,
            },
            TimelineEntry {
                song: 0,
                order_idx: 0,
                pattern_idx: 0,
                row_idx: 1,
                loop_iter: 0,
                tick: 6,
                speed_at_row: 6,
                bpm_at_row: 137,
            },
        ],
    };

    // LOOP — the per-channel half (`song_loop_to` is set above).
    module.channel_loops = vec![ChannelLoop {
        song: 0,
        channel: 1,
        start_tick: 0,
        end_tick: 12,
    }];

    // GRAF — inserts, a return bus, a send into it, and a master chain. Three
    // device kinds so a variant that stops encoding says so.
    module.channel_inserts = vec![
        DeviceChain {
            devices: vec![Device::Gain(Volume::from_q15(Q15::from_raw(16_000)))],
            bypass: false,
        },
        DeviceChain {
            devices: vec![Device::param_eq(3_000, 12, 4_500)],
            bypass: true,
        },
    ];
    module.buses = vec![Bus {
        name: "reverb return".into(),
        chain: DeviceChain {
            devices: vec![Device::reverb(
                ReverbMode::Freeverb,
                Q15::from_raw(20_000),
                Q15::from_raw(8_000),
                Q15::from_raw(32_000),
                Q15::from_raw(9_000),
                Q15::from_raw(30_000),
            )],
            bypass: false,
        },
    }];
    module.channel_sends = vec![
        vec![Send {
            bus: 0,
            level: Volume::from_q15(Q15::from_raw(12_000)),
        }],
        vec![],
    ];
    module.master_chain = DeviceChain {
        devices: vec![Device::compressor(-18_000, 1_024, 5_000, 200_000, 2_000)],
        bypass: false,
    };

    // ASET — a pool asset carrying its own PCM, so the blob region holds two
    // distinct buffers and the asset directory is exercised.
    module.assets = vec![Sample {
        name: "asset".into(),
        ..sample
    }];

    module
}

/// Read a container and return its chunks as `(id, payload)`, dropping `genr`
/// — the one chunk that is *supposed* to differ between releases.
fn comparable_chunks(bytes: &[u8]) -> Vec<(ChunkId, Vec<u8>)> {
    container::read(bytes)
        .expect("golden is a readable container")
        .chunks
        .iter()
        .filter(|c| c.id != v1::GENR)
        .map(|c| (c.id, c.payload.to_vec()))
        .collect()
}

/// Report the first differing chunk rather than "two byte vectors differ".
fn assert_same_chunks(expected: &[u8], actual: &[u8], what: &str) {
    let a = comparable_chunks(expected);
    let b = comparable_chunks(actual);

    let ids_a: Vec<_> = a.iter().map(|(id, _)| *id).collect();
    let ids_b: Vec<_> = b.iter().map(|(id, _)| *id).collect();
    assert_eq!(
        ids_a
            .iter()
            .map(|i| String::from_utf8_lossy(&i.0).into_owned())
            .collect::<Vec<_>>(),
        ids_b
            .iter()
            .map(|i| String::from_utf8_lossy(&i.0).into_owned())
            .collect::<Vec<_>>(),
        "{what}: the chunk set changed"
    );

    for ((id, pa), (_, pb)) in a.iter().zip(b.iter()) {
        assert_eq!(
            pa,
            pb,
            "{what}: chunk {} changed ({} → {} bytes). The v1 wire is frozen: \
             this normally calls for a schema bump + migration, not a re-bake.",
            String::from_utf8_lossy(&id.0),
            pa.len(),
            pb.len()
        );
    }
}

/// Bake the fixture when explicitly asked. Returns `true` when it did, so the
/// calling test can skip its assertions on that run.
fn baked(bytes: &[u8]) -> bool {
    if std::env::var_os("XMRS_BAKE_GOLDEN").is_none() {
        return false;
    }
    std::fs::create_dir_all("tests/fixtures").expect("create fixtures dir");
    std::fs::write(FIXTURE_PATH, bytes).expect("write fixture");
    eprintln!("baked {} ({} bytes)", FIXTURE_PATH, bytes.len());
    true
}

/// The header says v1, and the fixture is not accidentally empty.
#[test]
fn golden_is_a_v1_container() {
    let bytes = document::write(&golden_module()).expect("write");
    if baked(&bytes) {
        return;
    }
    assert!(
        !GOLDEN.is_empty(),
        "fixture is empty — bake it once: XMRS_BAKE_GOLDEN=1 cargo test \
         --features format --test format_golden_v1"
    );
    let c = container::read(GOLDEN).expect("golden reads as a container");
    assert_eq!(c.header.schema_version, 1);
    assert_eq!(
        c.header.min_reader_version, 1,
        "a pure-v1 file must stay readable by a v1 reader"
    );
}

/// Every chunk the format defines is actually present in the golden.
///
/// Without this, a chunk could quietly stop being written and the byte
/// comparison below would still pass — it compares what is there, not what
/// should be.
#[test]
fn golden_covers_every_v1_chunk() {
    let bytes = document::write(&golden_module()).expect("write");
    if baked(&bytes) {
        return;
    }
    let c = container::read(GOLDEN).expect("read golden");
    for id in v1::KNOWN {
        // `hstc` (edit history) has no producer yet; everything else in the
        // registry must be exercised by the corpus.
        if *id == v1::HSTC {
            continue;
        }
        assert!(
            c.chunk(*id).is_some(),
            "golden does not exercise chunk {} — the freeze does not cover it",
            String::from_utf8_lossy(&id.0)
        );
    }
}

/// The writer still produces the frozen image.
#[test]
fn writing_the_golden_module_reproduces_the_fixture() {
    let bytes = document::write(&golden_module()).expect("write");
    if baked(&bytes) {
        return;
    }
    assert_same_chunks(GOLDEN, &bytes, "writer");
}

/// The reader still understands the frozen image, and re-writing what it
/// understood reproduces it.
///
/// This is the direction that matters for compatibility: a file written today
/// must still decode to the same thing in every later release.
#[test]
fn reading_the_fixture_and_writing_it_back_is_stable() {
    let bytes = document::write(&golden_module()).expect("write");
    if baked(&bytes) {
        return;
    }
    let (m, report) = document::read(GOLDEN).expect("read golden");
    assert!(
        report.is_clean(),
        "golden must decode without reservations: dropped={:?}, retained={}, \
         inconsistency={:?}",
        report.dropped,
        report.retained.len(),
        report.inconsistency
    );
    assert_same_chunks(GOLDEN, &document::write(&m).expect("rewrite"), "reader");
}

/// The decoded content itself, spot-checked across the chunks a byte
/// comparison would flag but not explain.
#[test]
fn golden_decodes_to_the_expected_content() {
    let bytes = document::write(&golden_module()).expect("write");
    if baked(&bytes) {
        return;
    }
    let (m, _) = document::read(GOLDEN).expect("read golden");

    assert_eq!(m.name, "golden v1");
    assert_eq!(m.default_bpm, 137);
    assert_eq!(m.instrument.len(), 5);
    assert_eq!(m.tracks.len(), 2);
    assert_eq!(m.clips.len(), 2);
    assert_eq!(m.automation.len(), 1);
    assert_eq!(m.timeline_map.entries.len(), 2);
    assert_eq!(m.channel_loops.len(), 1);
    assert_eq!(m.song_loop_to, Some(1));
    assert_eq!(m.buses.len(), 1);
    assert_eq!(m.assets.len(), 1);
    assert!(m.quirks.ft2_pitch_slide_overflow);

    // The PCM came back through the blob region, not inline.
    match &m.instrument[1].instr_type {
        InstrumentType::Default(d) => {
            let s = d.sample[0].as_ref().expect("sample survived");
            match s.data.as_ref().expect("pcm survived") {
                SampleDataType::Mono8(v) => {
                    assert_eq!(&v[..], &[0, 40, -40, 127, -128, 7, -1, 64]);
                }
                other => panic!("wrong pcm kind: {other:?}"),
            }
            assert_eq!(s.loop_length, 4);
        }
        other => panic!("instrument 1 changed kind: {other:?}"),
    }

    // A device parameter survives with its exact value.
    match &m.master_chain.devices[0] {
        Device::Compressor {
            threshold_milli_db,
            ratio_q8_8,
            ..
        } => {
            assert_eq!(*threshold_milli_db, -18_000);
            assert_eq!(*ratio_q8_8, 1_024);
        }
        other => panic!("master chain changed: {other:?}"),
    }
}
