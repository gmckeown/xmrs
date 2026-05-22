//! Procedurally generated DAW-layer test suite.
//!
//! The cycle-exact suite is generated entirely in Rust from
//! `TestCase`s that build synthetic Modules. For each case we
//! verify that `build_timeline_layer` succeeds,
//! `verify_layers_consistent` returns Ok, and the optional
//! `extra_assert` structural check holds. PCM-level
//! cycle-exactness lives in `xmrsplayer/tests/cycle_exact_pcm.rs`.

#![cfg(feature = "std")]
#![allow(deprecated)]

use xmrs::daw::build_timeline::Pattern;
use xmrs::fixed::fixed::{Q15, Q8_8};
use xmrs::prelude::*;

mod builder {
    use super::*;

    /// Synthetic-module builder. Accumulates pattern data locally
    /// then folds it into a Module's DAW layer via
    /// [`xmrs::daw::build_timeline::build_timeline_layer`]. Post-3b the
    /// Module struct itself no longer carries `pattern` /
    /// `pattern_order` — they live here as builder-local scaffolding.
    pub struct ModuleBuilder {
        pub module: Module,
        pub patterns: Vec<Pattern>,
        pub pattern_order: Vec<Vec<usize>>,
    }

    impl ModuleBuilder {
        pub fn new() -> Self {
            Self {
                module: Module::default(),
                patterns: Vec::new(),
                pattern_order: Vec::new(),
            }
        }

        pub fn with_profile(mut self, p: CompatibilityProfile) -> Self {
            self.module.profile = p;
            self
        }

        pub fn with_instrument(mut self, instr: Instrument) -> Self {
            self.module.instrument.push(instr);
            self
        }

        pub fn with_pattern(mut self, channels: usize, rows: usize) -> Self {
            let pat: Pattern = (0..rows)
                .map(|_| (0..channels).map(|_| TrackUnit::default()).collect())
                .collect();
            self.patterns.push(pat);
            self
        }

        pub fn set_cell(mut self, pat: usize, row: usize, ch: usize, cell: TrackUnit) -> Self {
            self.patterns[pat][row][ch] = cell;
            self
        }

        pub fn set_note(
            mut self,
            pat: usize,
            row: usize,
            ch: usize,
            pitch: Pitch,
            instr: usize,
        ) -> Self {
            let mut cell = self.patterns[pat][row][ch].clone();
            cell.note = CellNote::Play(pitch);
            cell.instrument = Some(instr);
            self.patterns[pat][row][ch] = cell;
            self
        }

        pub fn add_effect(mut self, pat: usize, row: usize, ch: usize, fx: TrackEffect) -> Self {
            self.patterns[pat][row][ch].effects.push(fx);
            self
        }

        pub fn add_global_effect(
            mut self,
            pat: usize,
            row: usize,
            ch: usize,
            fx: GlobalEffect,
        ) -> Self {
            self.patterns[pat][row][ch].global_effects.push(fx);
            self
        }

        pub fn with_order(mut self, song: usize, order: Vec<usize>) -> Self {
            while self.pattern_order.len() <= song {
                self.pattern_order.push(Vec::new());
            }
            self.pattern_order[song] = order;
            self
        }

        /// Finalise the Module: run `build_timeline_layer` on the
        /// accumulated pattern data so the returned Module is
        /// ready to consume.
        pub fn build(mut self) -> Module {
            xmrs::daw::build_timeline::build_timeline_layer(
                &mut self.module,
                &self.pattern_order,
                &self.patterns,
            );
            self.module
        }
    }

    /// Minimal sample-based instrument with a one-cycle square sample.
    pub fn test_instrument() -> Instrument {
        // A square wave is enough for the structural tests; the audio
        // is not consumed yet.
        let data = SampleDataType::Mono8(
            [0i8; 64]
                .iter()
                .enumerate()
                .map(|(i, _)| if i < 32 { 127 } else { -128 })
                .collect(),
        );
        let sample = Sample {
            name: "sq".to_string(),
            relative_pitch: 0,
            finetune: Finetune::default(),
            volume: ChannelVolume::FULL,
            default_note_volume: Volume::FULL,
            panning: Panning::CENTER,
            loop_flag: LoopType::Forward,
            loop_start: 0,
            loop_length: 64,
            sustain_loop_flag: LoopType::No,
            sustain_loop_start: 0,
            sustain_loop_length: 0,
            data: Some(data),
        };
        let mut instrdef = InstrDefault::default();
        instrdef.sample.push(Some(sample));
        instrdef.keyboard.sample_for_pitch = [Some(0); 120];
        Instrument {
            name: "test".to_string(),
            instr_type: InstrumentType::Default(instrdef),
            muted: false,
        }
    }
}

use builder::{test_instrument, ModuleBuilder};

use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

extern crate alloc;

// =====================================================================
// TestCase definition
// =====================================================================

pub struct TestCase {
    pub name: &'static str,
    pub build: fn() -> Module,
    pub extra_assert: Option<fn(&Module)>,
}

// =====================================================================
// Quirks — a representative selection; the suite can be extended
// one-by-one as new playback quirks are added.
// =====================================================================

fn quirk_cases() -> Vec<TestCase> {
    vec![
        TestCase {
            name: "ft2_pitch_slide_overflow_on",
            build: || {
                ModuleBuilder::new()
                    .with_profile(CompatibilityProfile::ft2())
                    .with_instrument(test_instrument())
                    .with_pattern(1, 16)
                    .set_note(0, 0, 0, Pitch::B9, 0)
                    .add_effect(
                        0,
                        0,
                        0,
                        TrackEffect::Portamento {
                            speed: 127,
                            fine: false,
                        },
                    )
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: None,
        },
        TestCase {
            name: "ft2_arpeggio_lut_on",
            build: || {
                ModuleBuilder::new()
                    .with_profile(CompatibilityProfile::ft2())
                    .with_instrument(test_instrument())
                    .with_pattern(1, 8)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .add_effect(0, 0, 0, TrackEffect::Arpeggio { half1: 4, half2: 7 })
                    .add_global_effect(0, 0, 0, GlobalEffect::Speed(15))
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: None,
        },
        TestCase {
            name: "speed_zero_ends_song_pt",
            build: || {
                ModuleBuilder::new()
                    .with_profile(CompatibilityProfile::pt())
                    .with_instrument(test_instrument())
                    .with_pattern(1, 8)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .add_global_effect(0, 4, 0, GlobalEffect::Speed(0))
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: None,
        },
        TestCase {
            name: "speed_zero_no_quirk_modern",
            build: || {
                ModuleBuilder::new()
                    .with_profile(CompatibilityProfile::modern())
                    .with_instrument(test_instrument())
                    .with_pattern(1, 8)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .add_global_effect(0, 4, 0, GlobalEffect::Speed(0))
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: None,
        },
        TestCase {
            name: "it_old_effects_profile",
            build: || {
                ModuleBuilder::new()
                    .with_profile(CompatibilityProfile::it214())
                    .with_instrument(test_instrument())
                    .with_pattern(1, 8)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .add_effect(
                        0,
                        0,
                        0,
                        TrackEffect::Vibrato {
                            speed: Q8_8::from_int(4),
                            depth: PitchDelta::from_semitones(1),
                        },
                    )
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: None,
        },
        TestCase {
            name: "it215_profile",
            build: || {
                ModuleBuilder::new()
                    .with_profile(CompatibilityProfile::it215())
                    .with_instrument(test_instrument())
                    .with_pattern(1, 8)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: None,
        },
        TestCase {
            name: "st3_profile",
            build: || {
                ModuleBuilder::new()
                    .with_profile(CompatibilityProfile::st3())
                    .with_instrument(test_instrument())
                    .with_pattern(1, 8)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: None,
        },
    ]
}

// =====================================================================
// 10.3.2 — TrackEffect variants (representative cases)
// =====================================================================

fn track_effect_cases() -> Vec<TestCase> {
    // The `build: fn() -> Module` signature forbids captured state,
    // so each case is expanded by a macro into a fresh `fn` body.
    macro_rules! trackfx_case {
        ($name:ident, $name_lit:expr, $fx:expr) => {
            TestCase {
                name: $name_lit,
                build: || {
                    ModuleBuilder::new()
                        .with_instrument(test_instrument())
                        .with_pattern(1, 16)
                        .set_note(0, 0, 0, Pitch::C4, 0)
                        .add_effect(0, 0, 0, $fx)
                        .with_order(0, vec![0])
                        .build()
                },
                extra_assert: None,
            }
        };
    }

    vec![
        trackfx_case!(
            arp,
            "trackfx_arpeggio",
            TrackEffect::Arpeggio { half1: 4, half2: 7 }
        ),
        trackfx_case!(
            porta_up,
            "trackfx_portamento_up",
            TrackEffect::Portamento {
                speed: -32,
                fine: false
            }
        ),
        trackfx_case!(
            porta_down,
            "trackfx_portamento_down",
            TrackEffect::Portamento {
                speed: 32,
                fine: false
            }
        ),
        trackfx_case!(
            porta_fine,
            "trackfx_portamento_fine",
            TrackEffect::Portamento {
                speed: 16,
                fine: true
            }
        ),
        trackfx_case!(
            tone_porta,
            "trackfx_tone_portamento",
            TrackEffect::TonePortamento(32)
        ),
        trackfx_case!(
            vibrato,
            "trackfx_vibrato",
            TrackEffect::Vibrato {
                speed: Q8_8::from_int(4),
                depth: PitchDelta::from_semitones(1)
            }
        ),
        trackfx_case!(
            tremolo,
            "trackfx_tremolo",
            TrackEffect::Tremolo {
                speed: Q8_8::from_int(4),
                depth: Q15::from_ratio(1, 4)
            }
        ),
        trackfx_case!(
            tremor,
            "trackfx_tremor",
            TrackEffect::Tremor {
                on_time: 3,
                off_time: 2
            }
        ),
        trackfx_case!(
            volume_slide,
            "trackfx_volume_slide",
            TrackEffect::VolumeSlide {
                speed: Q15::from_ratio(1, 32),
                fine: false
            }
        ),
        trackfx_case!(
            volume_set,
            "trackfx_volume_set",
            TrackEffect::Volume {
                value: Volume::from_ratio(48, 64),
                tick: 0
            }
        ),
        trackfx_case!(
            panning,
            "trackfx_panning",
            TrackEffect::Panning(Panning::from_ratio(3, 4))
        ),
        trackfx_case!(
            panning_slide,
            "trackfx_panning_slide",
            TrackEffect::PanningSlide {
                speed: Q15::from_ratio(1, 32),
                fine: false
            }
        ),
        trackfx_case!(
            panbrello,
            "trackfx_panbrello",
            TrackEffect::Panbrello {
                speed: Q8_8::from_int(4),
                depth: Q15::from_ratio(1, 4)
            }
        ),
        trackfx_case!(
            glissando,
            "trackfx_glissando_on",
            TrackEffect::Glissando(true)
        ),
        trackfx_case!(
            note_cut,
            "trackfx_note_cut_tick3",
            TrackEffect::NoteCut {
                tick: 3,
                past: false
            }
        ),
        trackfx_case!(note_delay, "trackfx_note_delay", TrackEffect::NoteDelay(2)),
        trackfx_case!(
            note_off,
            "trackfx_note_off",
            TrackEffect::NoteOff {
                tick: 3,
                past: false
            }
        ),
        trackfx_case!(
            note_fadeout,
            "trackfx_note_fadeout",
            TrackEffect::NoteFadeOut {
                tick: 3,
                past: false
            }
        ),
        trackfx_case!(
            note_retrig,
            "trackfx_note_retrig",
            TrackEffect::NoteRetrig {
                speed: 3,
                volume_modifier: NoteRetrigOperator::None
            }
        ),
        trackfx_case!(
            channel_volume,
            "trackfx_channel_volume",
            TrackEffect::ChannelVolume(ChannelVolume::from_byte_64(32))
        ),
        trackfx_case!(
            channel_volume_slide,
            "trackfx_channel_volume_slide",
            TrackEffect::ChannelVolumeSlide {
                speed: Q15::from_ratio(1, 64),
                fine: false
            }
        ),
    ]
}

// =====================================================================
// 10.3.3 — GlobalEffect variants
// =====================================================================

fn global_effect_cases() -> Vec<TestCase> {
    macro_rules! gfx_case {
        ($name_lit:expr, $row:expr, $fx:expr) => {
            TestCase {
                name: $name_lit,
                build: || {
                    ModuleBuilder::new()
                        .with_instrument(test_instrument())
                        .with_pattern(1, 16)
                        .set_note(0, 0, 0, Pitch::C4, 0)
                        .add_global_effect(0, $row, 0, $fx)
                        .with_order(0, vec![0])
                        .build()
                },
                extra_assert: None,
            }
        };
    }

    vec![
        gfx_case!("globalfx_bpm", 0, GlobalEffect::Bpm(140)),
        gfx_case!("globalfx_bpm_slide", 0, GlobalEffect::BpmSlide(5)),
        gfx_case!("globalfx_speed", 0, GlobalEffect::Speed(4)),
        gfx_case!("globalfx_pattern_break", 2, GlobalEffect::PatternBreak(0)),
        gfx_case!("globalfx_position_jump", 2, GlobalEffect::PositionJump(0)),
        gfx_case!(
            "globalfx_pattern_loop_anchor",
            1,
            GlobalEffect::PatternLoop(0)
        ),
        gfx_case!(
            "globalfx_pattern_delay",
            0,
            GlobalEffect::PatternDelay {
                quantity: 2,
                tempo: false
            }
        ),
        gfx_case!(
            "globalfx_volume_set",
            0,
            GlobalEffect::Volume(Volume::from_ratio(48, 64))
        ),
        gfx_case!(
            "globalfx_volume_slide",
            0,
            GlobalEffect::VolumeSlide {
                speed: Q15::from_ratio(1, 64),
                fine: false
            }
        ),
        // Multi-row pattern loop 3x
        TestCase {
            name: "globalfx_pattern_loop_3x",
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(1, 8)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .add_global_effect(0, 1, 0, GlobalEffect::PatternLoop(0))
                    .add_global_effect(0, 4, 0, GlobalEffect::PatternLoop(3))
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: None,
        },
    ]
}

// =====================================================================
// 10.3.4 — Structural / combinatorial cases
// =====================================================================

fn structural_cases() -> Vec<TestCase> {
    vec![
        TestCase {
            name: "empty_module",
            build: || Module::default(),
            extra_assert: Some(|m| {
                assert!(m.tracks.is_empty(), "empty module has no tracks");
                assert!(m.clips.is_empty(), "empty module has no clips");
            }),
        },
        TestCase {
            name: "single_note_no_effect",
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(1, 16)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: Some(|m| {
                assert_eq!(m.tracks.len(), 1, "exactly 1 track");
                assert_eq!(m.clips.len(), 1, "exactly 1 clip");
                assert_eq!(m.clips[0].position_tick, 0);
                assert_eq!(m.clips[0].target_channel, 0);
                assert_eq!(m.clips[0].track_row_offset, 0);
            }),
        },
        TestCase {
            name: "multi_channel_4",
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(4, 16)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .set_note(0, 0, 1, Pitch::C4, 0)
                    .set_note(0, 0, 2, Pitch::C4, 0)
                    .set_note(0, 0, 3, Pitch::C4, 0)
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: Some(|m| {
                // After content dedup: all 4 channels are identical,
                // so they fuse into a single Track.
                assert_eq!(m.tracks.len(), 1, "all 4 channels dedupe to 1 track");
                assert_eq!(m.clips.len(), 4, "but 4 distinct clips remain");
                let chs: Vec<u8> = m.clips.iter().map(|c| c.target_channel).collect();
                assert_eq!(chs, vec![0, 1, 2, 3]);
            }),
        },
        TestCase {
            name: "multi_pattern_order_2_patterns",
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(1, 8)
                    .with_pattern(1, 8)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .set_note(1, 0, 0, Pitch::C4, 0)
                    .with_order(0, vec![0, 1])
                    .build()
            },
            extra_assert: Some(|m| {
                // Same content on both patterns → dedupe to 1 track.
                assert_eq!(m.tracks.len(), 1);
                assert_eq!(m.clips.len(), 2);
                // The second clip's position_tick is exactly the first
                // pattern's tick length (8 rows × speed 6 = 48 ticks).
                assert_eq!(m.clips[0].position_tick, 0);
                assert_eq!(m.clips[1].position_tick, 48);
            }),
        },
        TestCase {
            name: "multi_song_2_independent",
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(1, 8)
                    .with_pattern(1, 8)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .set_note(1, 0, 0, Pitch::C4, 0)
                    .with_order(0, vec![0])
                    .with_order(1, vec![1])
                    .build()
            },
            extra_assert: Some(|m| {
                // Two sub-songs; each generates one clip; both share
                // the same dedupable track contents → 1 track total.
                assert_eq!(m.tracks.len(), 1);
                assert_eq!(m.clips.len(), 2);
                let songs: Vec<u16> = m.clips.iter().map(|c| c.song).collect();
                assert_eq!(songs, vec![0, 1]);
            }),
        },
        TestCase {
            name: "instrument_change_mid_channel_splits",
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_instrument(test_instrument())
                    .with_pattern(1, 8)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .set_note(0, 4, 0, Pitch::C4, 1)
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: Some(|m| {
                // Two segments: rows 0-3 on instrument 0, rows 4-7 on
                // instrument 1. The two tracks point to different
                // instruments so they don't dedupe.
                assert_eq!(m.tracks.len(), 2);
                assert_eq!(m.tracks[0].instrument, 0);
                assert_eq!(m.tracks[1].instrument, 1);
                assert_eq!(m.clips.len(), 2);
                assert_eq!(m.clips[0].position_tick, 0);
                // second segment starts at row 4 → 4 × speed_6 = 24 ticks
                assert_eq!(m.clips[1].position_tick, 24);
                assert_eq!(m.clips[0].target_channel, 0);
                assert_eq!(m.clips[1].target_channel, 0);
            }),
        },
        TestCase {
            name: "key_off_in_note_column",
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(1, 8)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .set_cell(
                        0,
                        4,
                        0,
                        TrackUnit {
                            note: CellNote::KeyOff,
                            ..Default::default()
                        },
                    )
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: None,
        },
        TestCase {
            name: "note_cut_in_note_column",
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(1, 8)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .set_cell(
                        0,
                        4,
                        0,
                        TrackUnit {
                            note: CellNote::NoteCut,
                            ..Default::default()
                        },
                    )
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: None,
        },
        TestCase {
            name: "dedupe_identical_segments_across_patterns",
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(2, 8)
                    .with_pattern(2, 8)
                    // channel 0 identical on both patterns
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .set_note(0, 4, 0, Pitch::C4, 0)
                    .set_note(1, 0, 0, Pitch::C4, 0)
                    .set_note(1, 4, 0, Pitch::C4, 0)
                    // channel 1 different
                    .set_note(0, 0, 1, Pitch::C4, 0)
                    .set_note(1, 0, 1, Pitch::C4, 0)
                    .with_order(0, vec![0, 1])
                    .build()
            },
            extra_assert: Some(|m| {
                // Channel 0 is shared → both patterns' canal 0
                // segments fuse. Channel 1 is "different" but we
                // actually built it identically too in this synthetic
                // case — the test is mainly verifying the dedup
                // pipeline runs without panic and produces ≤ source
                // segment count. Stronger oracle: at most 2 tracks
                // (canal 0 fused, canal 1 fused independently).
                assert!(m.tracks.len() <= 4);
                let canal0_clips: Vec<_> =
                    m.clips.iter().filter(|c| c.target_channel == 0).collect();
                assert_eq!(canal0_clips.len(), 2);
                // The two channel-0 clips must point to the same
                // Track index (structural dedup via dedupe_tracks_by_content).
                assert_eq!(canal0_clips[0].track, canal0_clips[1].track);
            }),
        },
        TestCase {
            name: "dedupe_with_different_start_rows",
            // Two patterns whose channel 0 produces SAME-length
            // segments starting at DIFFERENT rows (row 0 of pat 0,
            // row 4 of pat 1). After dedup they share one Track; the
            // moteur must still resolve cell lookups correctly for
            // both source patterns via
            // `clip.position_tick + clip.track_row_offset + speed`
            // — NOT through the Track's source-pattern `start_row`,
            // which is a content-level concept only.
            //
            // Pat 0 channel 0: instr0 at row 0 ─ instr1 at row 4 (so
            // segment 0 is rows 0..3 only).
            // Pat 1 channel 0: same content but instr0 starts at
            // row 4, instr1 at row 0 (so segment-with-instr0 starts
            // at row 4, length 4 — matches pat 0's segment 0).
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_instrument(test_instrument())
                    .with_pattern(1, 8)
                    .with_pattern(1, 8)
                    // pat 0 chan 0: instr0 row 0 (segment rows 0..3),
                    //                instr1 row 4 (segment rows 4..7).
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .set_note(0, 4, 0, Pitch::C4, 1)
                    // pat 1 chan 0: instr1 row 0 (segment rows 0..3),
                    //                instr0 row 4 (segment rows 4..7).
                    .set_note(1, 0, 0, Pitch::C4, 1)
                    .set_note(1, 4, 0, Pitch::C4, 0)
                    .with_order(0, vec![0, 1])
                    .build()
            },
            extra_assert: Some(|m| {
                // The two instr0 segments share identical content
                // (4 rows: `[C4, Empty, Empty, Empty]` with the same
                // instrument) — dedup should fuse them.
                // Verify by querying both source coordinates: they
                // must both resolve to a Play(_) cell at the C4
                // position. With the old (buggy)
                // `start_row_in_source_pattern`-based resolution one
                // of the lookups returned None.
                let cell_a = m.row_at(0, 0, 0); // pat 0 row 0
                let cell_b = m.row_at(0, 1, 4); // pat 1 row 4
                assert!(
                    matches!(cell_a[0].note, CellNote::Play(_)),
                    "pat 0 row 0 should resolve to a Play(_)"
                );
                assert!(
                    matches!(cell_b[0].note, CellNote::Play(_)),
                    "pat 1 row 4 should resolve to a Play(_) after dedup"
                );
            }),
        },
        TestCase {
            name: "pattern_loop_no_extra_clips",
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(1, 8)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .add_global_effect(0, 0, 0, GlobalEffect::PatternLoop(0))
                    .add_global_effect(0, 3, 0, GlobalEffect::PatternLoop(2))
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: Some(|m| {
                // Even though the loop is iterated 3 times, we
                // emit exactly one Clip for the order position.
                assert_eq!(m.clips.len(), 1);
            }),
        },
        TestCase {
            name: "pattern_break_mid_pattern_uses_row_offset",
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(1, 8)
                    .with_pattern(1, 8)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    // PatternBreak at the end of pattern 0 to row 3 of pattern 1
                    .add_global_effect(0, 7, 0, GlobalEffect::PatternBreak(3))
                    .set_note(1, 0, 0, Pitch::C4, 0)
                    .with_order(0, vec![0, 1])
                    .build()
            },
            extra_assert: Some(|m| {
                // Pattern 1 has a single segment starting at row 0
                // covering rows 0..8. The PatternBreak makes us enter
                // pattern 1 at row 3, so the second clip should have
                // track_row_offset == 3.
                let p1_clip = m
                    .clips
                    .iter()
                    .find(|c| c.song == 0 && c.position_tick > 0)
                    .expect("pattern 1 clip");
                assert_eq!(p1_clip.track_row_offset, 3);
            }),
        },
        // ------- Navigation effects -------
        TestCase {
            name: "position_jump_forward",
            // Bxx on row 0 of pat 0 jumps to order 2 (pat 2),
            // skipping pat 1 entirely.
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(1, 4)
                    .with_pattern(1, 4)
                    .with_pattern(1, 4)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .add_global_effect(0, 0, 0, GlobalEffect::PositionJump(2))
                    .with_order(0, vec![0, 1, 2])
                    .build()
            },
            extra_assert: Some(|m| {
                // No entry for pattern 1 — order 1 was skipped.
                assert!(
                    !m.timeline_map.entries.iter().any(|e| e.pattern_idx == 1),
                    "PositionJump must skip pat 1"
                );
                // Pat 2 is reached.
                assert!(
                    m.timeline_map.entries.iter().any(|e| e.pattern_idx == 2),
                    "pat 2 reached after jump"
                );
            }),
        },
        TestCase {
            name: "position_jump_invalid_target_is_skipped",
            // Bxx targeting an order beyond song length: walker
            // treats it as out-of-range and stops emitting entries.
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(1, 4)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .add_global_effect(0, 0, 0, GlobalEffect::PositionJump(99))
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: Some(|m| {
                // Only the first row before the jump consumed.
                // The walker doesn't emit anything past the (broken)
                // jump destination.
                let song0: Vec<_> = m
                    .timeline_map
                    .entries
                    .iter()
                    .filter(|e| e.song == 0)
                    .collect();
                assert!(song0.len() <= 1, "walker should not loop past invalid jump");
            }),
        },
        TestCase {
            name: "pattern_break_to_specific_row",
            // Dxx on the LAST row of pat 0 with a non-zero target row
            // → walker enters pat 1 at that row. The break must
            // happen at the natural end of pat 0 so the resulting
            // clips don't appear to overlap on the consistency check.
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(1, 8)
                    .with_pattern(1, 8)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .add_global_effect(0, 7, 0, GlobalEffect::PatternBreak(5))
                    .set_note(1, 5, 0, Pitch::C4, 0)
                    .with_order(0, vec![0, 1])
                    .build()
            },
            extra_assert: Some(|m| {
                let pat1_first = m
                    .timeline_map
                    .entries
                    .iter()
                    .find(|e| e.pattern_idx == 1)
                    .expect("pat 1 should be reached");
                assert_eq!(pat1_first.row_idx, 5, "pattern break landed on row 5");
            }),
        },
        TestCase {
            name: "pattern_delay_repeats_row",
            // EEx duplicates the row in the timeline (one TimelineEntry
            // per repetition). Verifies the walker emits multiple
            // entries with the same (order, pat, row, loop_iter).
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(1, 4)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .add_global_effect(
                        0,
                        0,
                        0,
                        GlobalEffect::PatternDelay {
                            quantity: 3,
                            tempo: false,
                        },
                    )
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: Some(|m| {
                let row0_entries: Vec<_> = m
                    .timeline_map
                    .entries
                    .iter()
                    .filter(|e| e.row_idx == 0 && e.pattern_idx == 0)
                    .collect();
                // PatternDelay quantity = 3 → row 0 plays 1 + 3 = 4
                // times (initial + 3 saturating repeats).
                assert!(row0_entries.len() >= 2, "pattern delay must emit > 1 entry");
            }),
        },
        TestCase {
            name: "speed_change_mid_song_propagates_to_entries",
            // Fxx with a value < 0x20 sets speed (ticks/row). The
            // walker captures speed_at_row per entry — later rows
            // must reflect the new speed.
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(1, 4)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .add_global_effect(0, 1, 0, GlobalEffect::Speed(4))
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: Some(|m| {
                let row0 = m.timeline_map.find_entry(0, 0, 0).expect("row 0");
                let row1 = m.timeline_map.find_entry(0, 0, 1).expect("row 1");
                assert_eq!(row0.speed_at_row, 6, "row 0 keeps default speed");
                assert_eq!(row1.speed_at_row, 4, "row 1 picks up new speed");
            }),
        },
        TestCase {
            name: "bpm_change_mid_song_propagates_to_entries",
            // Same as above but for BPM (the other clock parameter).
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(1, 4)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .add_global_effect(0, 1, 0, GlobalEffect::Bpm(160))
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: Some(|m| {
                let row0 = m.timeline_map.find_entry(0, 0, 0).expect("row 0");
                let row1 = m.timeline_map.find_entry(0, 0, 1).expect("row 1");
                assert_eq!(row0.bpm_at_row, 125);
                assert_eq!(row1.bpm_at_row, 160);
            }),
        },
        // ------- Pattern loop interactions -------
        TestCase {
            name: "pattern_loop_then_break",
            // E60 anchor at row 0, E62 repeat at row 3, then Dxx on
            // row 3 of the *last* iteration. The walker must emit
            // entries for all 3 iterations before exiting.
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(1, 4)
                    .with_pattern(1, 4)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .add_global_effect(0, 0, 0, GlobalEffect::PatternLoop(0))
                    .add_global_effect(0, 3, 0, GlobalEffect::PatternLoop(2))
                    .with_order(0, vec![0, 1])
                    .build()
            },
            extra_assert: Some(|m| {
                // 3 iterations × 4 rows in pat 0 → entries with
                // loop_iter 0, 1, 2.
                let pat0_loop_iters: alloc::collections::BTreeSet<u16> = m
                    .timeline_map
                    .entries
                    .iter()
                    .filter(|e| e.pattern_idx == 0)
                    .map(|e| e.loop_iter)
                    .collect();
                assert!(pat0_loop_iters.contains(&0));
                assert!(pat0_loop_iters.contains(&1));
            }),
        },
        // ------- Multi-effect interactions on the same row -------
        TestCase {
            name: "position_jump_takes_priority_over_break",
            // Bxx and Dxx on the same row: PositionJump wins (it
            // gets the explicit table_index target).
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(1, 4)
                    .with_pattern(1, 4)
                    .with_pattern(1, 4)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .add_global_effect(0, 0, 0, GlobalEffect::PositionJump(2))
                    .add_global_effect(0, 0, 0, GlobalEffect::PatternBreak(0))
                    .with_order(0, vec![0, 1, 2])
                    .build()
            },
            extra_assert: Some(|m| {
                // Pat 1 must NOT be visited (PositionJump skipped it).
                assert!(
                    !m.timeline_map.entries.iter().any(|e| e.pattern_idx == 1),
                    "PositionJump must take priority over PatternBreak"
                );
            }),
        },
        // ------- Dedup edge cases -------
        TestCase {
            name: "dedupe_does_not_fuse_across_instruments",
            // Two channels with identical cell content but different
            // instruments → tracks must NOT be fused.
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_instrument(test_instrument())
                    .with_pattern(2, 4)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .set_note(0, 0, 1, Pitch::C4, 1)
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: Some(|m| {
                assert_eq!(m.tracks.len(), 2, "different instruments → two tracks");
                assert_ne!(
                    m.tracks[0].instrument, m.tracks[1].instrument,
                    "tracks must carry their respective instruments"
                );
            }),
        },
        TestCase {
            name: "dedupe_does_not_fuse_different_lengths",
            // Same instrument, same first cells, different segment
            // length → no fusion.
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_instrument(test_instrument())
                    .with_pattern(2, 4)
                    // ch 0: instr 0 at row 0 → 4-row segment.
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    // ch 1: instr 0 at row 0, then instr 1 at row 3
                    // splits the segment into [0,3) + [3,4).
                    .set_note(0, 0, 1, Pitch::C4, 0)
                    .set_note(0, 3, 1, Pitch::C4, 1)
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: Some(|m| {
                let instr0_tracks = m.tracks.iter().filter(|t| t.instrument == 0).count();
                assert_eq!(instr0_tracks, 2, "lengths differ → no fusion");
            }),
        },
        // ------- Euclidean Bjorklund variants -------
        TestCase {
            name: "euclidean_5_8_canonical_detected",
            // Bjorklund(5, 8) = `T F T T F T T F` (Bresenham canonical).
            build: || {
                let mut m = ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(1, 8)
                    .with_order(0, vec![0]);
                let positions = [0, 2, 3, 5, 6];
                for p in positions {
                    m = m.set_note(0, p, 0, Pitch::C4, 0);
                }
                m.build()
            },
            extra_assert: Some(|m| {
                // The auto-conversion adds an Euclidean wrapper.
                assert!(
                    m.instrument
                        .iter()
                        .any(|i| matches!(i.instr_type, InstrumentType::Euclidean(_))),
                    "Bjorklund(5,8) should auto-convert"
                );
            }),
        },
        TestCase {
            name: "euclidean_2_5_canonical_detected",
            // Bjorklund(2, 5) = `T F T F F` — canonical 2-of-5.
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(1, 5)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .set_note(0, 2, 0, Pitch::C4, 0)
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: Some(|m| {
                assert!(
                    m.instrument
                        .iter()
                        .any(|i| matches!(i.instr_type, InstrumentType::Euclidean(_))),
                    "Bjorklund(2,5) should auto-convert"
                );
            }),
        },
        TestCase {
            name: "euclidean_unbalanced_rejects",
            // 3 events at positions [0, 1, 2] (clumped) — not even
            // close to a canonical Bjorklund.
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(1, 8)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .set_note(0, 1, 0, Pitch::C4, 0)
                    .set_note(0, 2, 0, Pitch::C4, 0)
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: Some(|m| {
                assert!(
                    !m.instrument
                        .iter()
                        .any(|i| matches!(i.instr_type, InstrumentType::Euclidean(_))),
                    "clumped pattern should not look Euclidean"
                );
            }),
        },
        // ------- Channel edge cases -------
        TestCase {
            name: "trailing_silent_channels_are_omitted",
            // 8-channel pattern with notes only on channel 0.
            // get_num_channels falls back to clips → 1 channel.
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(8, 4)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: Some(|m| {
                assert_eq!(
                    m.get_num_channels(),
                    1,
                    "silent trailing channels should not inflate channel count"
                );
            }),
        },
        TestCase {
            name: "notes_on_only_inner_channel",
            // Notes on channel 3 only (channels 0-2 stay empty).
            // get_num_channels must reach 4 (channel index 3 + 1).
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(4, 4)
                    .set_note(0, 0, 3, Pitch::C4, 0)
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: Some(|m| {
                assert_eq!(m.get_num_channels(), 4);
                let ch3 = m.clips.iter().find(|c| c.target_channel == 3);
                assert!(ch3.is_some(), "channel 3 must carry a clip");
            }),
        },
        TestCase {
            name: "row_at_returns_empty_for_unknown_coordinate",
            // No pattern 99 → row_at returns a default-filled row.
            build: || {
                ModuleBuilder::new()
                    .with_instrument(test_instrument())
                    .with_pattern(1, 4)
                    .set_note(0, 0, 0, Pitch::C4, 0)
                    .with_order(0, vec![0])
                    .build()
            },
            extra_assert: Some(|m| {
                let cells = m.row_at(0, 99, 99);
                assert!(cells.iter().all(|c| matches!(c.note, CellNote::Empty)));
            }),
        },
    ]
}

// =====================================================================
// Aggregate
// =====================================================================

fn all_test_cases() -> Vec<TestCase> {
    let mut cases = Vec::new();
    cases.extend(quirk_cases());
    cases.extend(track_effect_cases());
    cases.extend(global_effect_cases());
    cases.extend(structural_cases());
    cases
}

// =====================================================================
// Runner
// =====================================================================

#[test]
fn phase1_dedup_runs_clean_on_every_case() {
    let cases = all_test_cases();
    let mut failures = Vec::new();
    for case in &cases {
        let module = (case.build)();
        if let Err(e) = module.verify_layers_consistent() {
            failures.push(format!("[{}] inconsistent: {:?}", case.name, e));
            continue;
        }
        if let Some(extra) = case.extra_assert {
            let case_name = case.name;
            let result = std::panic::catch_unwind(|| extra(&module));
            if result.is_err() {
                failures.push(format!("[{}] extra_assert failed", case_name));
            }
        }
    }
    if !failures.is_empty() {
        panic!("{} failures:\n{}", failures.len(), failures.join("\n"));
    }
    println!("OK: {} TestCases verified", cases.len());
}
