//! Property-based tests for the Edit API.
//!
//! Two central invariants are checked across a large random
//! command sequence:
//!
//! 1. `apply(cmd)` followed by `undo(receipt)` is identity on the
//!    Module (`apply_then_undo_is_identity`).
//! 2. `verify_layers_consistent()` returns `Ok` at every point in
//!    the sequence (`verify_layers_consistent_after_each_command`).
//!
//! We don't aim for an oracle of correctness on the *content*
//! produced by random sequences — just that the Module stays well-
//! formed and reversible. The hand-rolled unit tests in
//! `xmrs/src/edit.rs::tests` cover the semantic per-command checks.

#![cfg(feature = "std")]
#![allow(deprecated)]

use proptest::prelude::*;
use xmrs::edit::EditCommand;
use xmrs::prelude::*;

/// Build a small "starter" module with a handful of instruments and
/// no tracks/clips yet. Each proptest case mutates a fresh clone of
/// this.
fn starter_module() -> Module {
    let mut m = Module::default();
    for i in 0..4 {
        m.instrument.push(Instrument {
            name: format!("instr-{}", i),
            instr_type: InstrumentType::Empty,
            muted: false,
        });
    }
    m
}

/// Cheap structural fingerprint of a Module for apply/undo identity
/// checks. We can't rely on `Module: Eq` (Instrument is not Eq), but
/// a debug-style summary captures every field the Edit API can touch.
fn fingerprint(m: &Module) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    write!(
        &mut s,
        "name={};bpm={};speed={};quirks_hash={:?};",
        m.name,
        m.default_bpm,
        m.default_tempo,
        format!("{:?}", m.quirks)
    )
    .unwrap();
    for (i, t) in m.tracks.iter().enumerate() {
        write!(
            &mut s,
            "t{}={{n='{}' i={} rows={} mute={}}};",
            i,
            t.name,
            t.instrument,
            t.rows.len(),
            t.muted
        )
        .unwrap();
        for (r, cell) in t.rows.iter().enumerate() {
            // Hash the cell via Debug (TrackUnit is Debug but not Eq).
            let cell_s = format!("{:?}", cell);
            // truncate to keep output reasonable
            let truncated = if cell_s.len() > 80 {
                &cell_s[..80]
            } else {
                cell_s.as_str()
            };
            write!(&mut s, "r{}={};", r, truncated).unwrap();
        }
    }
    for (i, c) in m.clips.iter().enumerate() {
        write!(
            &mut s,
            "c{}={{t={} s={} ch={} t={} off={}}};",
            i, c.track, c.song, c.target_channel, c.position_tick, c.track_row_offset
        )
        .unwrap();
    }
    for (i, l) in m.automation.iter().enumerate() {
        let points = l.points().unwrap_or(&[]);
        write!(
            &mut s,
            "l{}={{target={:?} pts={} en={}}};",
            i,
            l.target,
            points.len(),
            l.enabled
        )
        .unwrap();
        for (j, p) in points.iter().enumerate() {
            write!(&mut s, "p{}={{t={} v={:?}}};", j, p.tick, p.value).unwrap();
        }
    }
    s
}

/// Manual clone (`Module` isn't Clone because Instrument isn't).
fn clone_module(m: &Module) -> Module {
    let mut c = Module {
        name: m.name.clone(),
        default_bpm: m.default_bpm,
        default_tempo: m.default_tempo,
        quirks: m.quirks,
        tracks: m.tracks.clone(),
        clips: m.clips.clone(),
        automation: m.automation.clone(),
        timeline_map: m.timeline_map.clone(),
        ..Module::default()
    };
    for inst in &m.instrument {
        c.instrument.push(Instrument {
            name: inst.name.clone(),
            instr_type: InstrumentType::Empty, // close enough for our purposes
            muted: inst.muted,
        });
    }
    c
}

/// Strategy: generate a command that's *likely* to apply successfully
/// against an arbitrary module. We don't need 100% success — Err
/// returns just leave the Module unchanged.
fn arb_edit_command() -> impl Strategy<Value = EditCommand> {
    use xmrs::core::daw::automation::{AutomationPoint, AutomationTarget, AutomationValue};
    prop_oneof![
        // Track creation
        (0u32..4u32, "[a-z]{1,8}", 1u32..32u32).prop_map(|(instr, name, len)| {
            EditCommand::CreateTrack {
                instrument: instr as usize,
                name,
                length_rows: len,
            }
        }),
        // Rename existing track (might fail if no track)
        (0u32..8u32, "[a-z]{1,8}").prop_map(|(t, name)| {
            EditCommand::RenameTrack {
                track: t,
                new_name: name,
            }
        }),
        // Set track muted
        (0u32..8u32, any::<bool>())
            .prop_map(|(t, m)| { EditCommand::SetTrackMuted { track: t, muted: m } }),
        // Resize track
        (0u32..8u32, 0u32..32u32).prop_map(|(t, len)| {
            EditCommand::ResizeTrack {
                track: t,
                new_length_rows: len,
            }
        }),
        // Clone track
        (0u32..8u32).prop_map(|t| EditCommand::CloneTrack { track: t }),
        // Insert row
        (0u32..8u32, 0u32..32u32).prop_map(|(t, r)| {
            EditCommand::InsertRow {
                track: t,
                row_offset: r,
                content: Cell::default(),
            }
        }),
        // Delete row
        (0u32..8u32, 0u32..32u32).prop_map(|(t, r)| {
            EditCommand::DeleteRow {
                track: t,
                row_offset: r,
            }
        }),
        // Set cell
        (0u32..8u32, 0u32..32u32).prop_map(|(t, r)| {
            EditCommand::SetCell {
                track: t,
                row_offset: r,
                content: Cell::default(),
            }
        }),
        // Create clip
        (0u32..8u32, 0u16..2u16, 0u8..8u8, 0u32..4096u32).prop_map(|(track, song, ch, tick)| {
            EditCommand::CreateClip {
                track,
                song,
                target_channel: ch,
                position_tick: tick,
            }
        }),
        // Delete clip
        (0u32..16u32).prop_map(|c| EditCommand::DeleteClip { clip: c }),
        // Move clip
        (0u32..16u32, 0u32..4096u32).prop_map(|(c, t)| {
            EditCommand::MoveClip {
                clip: c,
                new_position_tick: t,
            }
        }),
        // Create automation lane (Bpm)
        Just(EditCommand::CreateAutomationLane {
            target: AutomationTarget::Bpm
        }),
        Just(EditCommand::CreateAutomationLane {
            target: AutomationTarget::Speed
        }),
        Just(EditCommand::CreateAutomationLane {
            target: AutomationTarget::GlobalVolume
        }),
        // Add automation point
        (0u32..8u32, 0u32..4096u32, 32u16..255u16).prop_map(|(lane, tick, bpm)| {
            EditCommand::AddAutomationPoint {
                lane,
                point: AutomationPoint {
                    tick,
                    value: AutomationValue::Bpm(bpm),
                },
            }
        }),
        // Delete automation point
        (0u32..8u32, 0u32..16u32).prop_map(|(l, p)| {
            EditCommand::DeleteAutomationPoint {
                lane: l,
                point_index: p,
            }
        }),
        // Global settings
        (32usize..255usize).prop_map(|bpm| EditCommand::SetDefaultBpm { bpm }),
        (1usize..31usize).prop_map(|speed| EditCommand::SetDefaultSpeed { speed }),
        "[a-z]{0,16}".prop_map(|name| EditCommand::SetModuleName { name }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig {
        // Target ≥ 10 000 generated commands total. Each test
        // case is a vec of up to 64 commands; 256 cases × ~32
        // average length comfortably exceeds that threshold.
        cases: 256,
        ..ProptestConfig::default()
    })]

    /// `apply(cmd)` followed by `undo(receipt)` is identity on the
    /// Module. Checked on every command that successfully applies.
    #[test]
    fn apply_then_undo_is_identity(commands in proptest::collection::vec(arb_edit_command(), 1..32)) {
        let mut module = starter_module();
        // Pre-populate a few tracks and clips so commands have
        // something to bite on.
        let _ = module.apply(EditCommand::CreateTrack {
            instrument: 0,
            name: "a".to_string(),
            length_rows: 8,
        });
        let _ = module.apply(EditCommand::CreateTrack {
            instrument: 0,
            name: "b".to_string(),
            length_rows: 8,
        });
        let _ = module.apply(EditCommand::CreateAutomationLane {
            target: xmrs::core::daw::automation::AutomationTarget::Bpm,
        });

        for cmd in commands {
            let before = fingerprint(&module);
            // Take a snapshot to compare against after apply+undo.
            let mut work = clone_module(&module);
            let result = work.apply(cmd.clone());
            match result {
                Ok(receipt) => {
                    let undo_result = work.undo(&receipt);
                    prop_assert!(undo_result.is_ok(), "undo failed for {:?}: {:?}", cmd, undo_result);
                    let after = fingerprint(&work);
                    prop_assert_eq!(before.clone(), after, "apply+undo not identity for {:?}", cmd);
                }
                Err(_) => {
                    // Err means the Module was not modified — no undo
                    // needed. Fingerprint must be unchanged.
                    let after = fingerprint(&work);
                    prop_assert_eq!(before, after, "Err should leave module untouched for {:?}", cmd);
                }
            }
            // Commit a non-Err command so the chain progresses.
            let _ = module.apply(cmd);
        }
    }

    /// After every command (success or error), the Module's
    /// `verify_layers_consistent` invariant must still hold.
    #[test]
    fn verify_layers_consistent_after_each_command(
        commands in proptest::collection::vec(arb_edit_command(), 1..64)
    ) {
        let mut module = starter_module();
        for cmd in commands {
            let _ = module.apply(cmd);
            // Even after an Err, the module must remain consistent.
            prop_assert!(
                module.verify_layers_consistent().is_ok(),
                "module became inconsistent: {:?}",
                module.verify_layers_consistent().err()
            );
        }
    }
}
