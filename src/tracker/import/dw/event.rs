#![forbid(unsafe_code)]

//! Structured representation of a track byte stream.
//!
//! A `.dw` track is a one-dimensional sequence of bytes whose
//! grammar follows spec §5: bytes `0x00..=0x7F` are notes,
//! bytes `0x80..=0xFF` are commands with module-dependent
//! parameter widths. This module turns the raw stream read by
//! `dw_module::read_track_bytes` into a typed event list that
//! downstream layers (runtime tick, DAW conversion) can walk
//! without re-parsing the binary.

use alloc::vec::Vec;

/// One decoded item from a track stream. The variants mirror the
/// 14 named commands of spec §5.2 plus three "above-threshold"
/// table accesses ([`Self::SetSample`] / [`Self::SetPitchArpeggio`] /
/// [`Self::SetVolumeEnvelope`]) and the long-form wait
/// ([`Self::LongWait`]).
///
/// Parameter widths follow the spec; for the three commands
/// whose parameter count is module-dependent (`Effect8`,
/// `Effect9`, `StartOrStopSoundFx`) the decoder assumes the
/// "one parameter byte present" interpretation — refined when
/// the detection layer exposes the relevant feature flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DwTrackEvent {
    /// A note value in `0..=0x7F`. Interpretation depends on
    /// the variant: old-player splits the byte as
    /// `(sample, pitch_in_octave)`, new-player treats it as a
    /// direct period-table index. Both are handled at runtime,
    /// not at decode time.
    Note(u8),

    /// Long-form wait command (`0xE0..=0xFF`). The replayer
    /// waits `(byte - 0xDF) * channel.speed` frames before
    /// reading the next event. The raw byte is preserved so
    /// the runtime can apply the speed multiplier itself.
    LongWait(u8),

    /// Select the active sample for subsequent notes
    /// (new-player only). Carries the raw on-disk byte; the runtime
    /// subtracts the per-module SetSample threshold
    /// (`DwDispatcher::sample_threshold`) to get the table index.
    SetSample(u8),

    /// Above-threshold table access from the `0x90` dispatcher
    /// bracket — arms a per-tick **pitch arpeggio** on the channel
    /// (Ghidra: the `0x90` handler feeds its byte cycle to
    /// `AUDxPER`). Carries the raw on-disk byte; the runtime
    /// subtracts the per-module `pitch_arpeggio_threshold` to get the
    /// arpeggio-table index.
    SetPitchArpeggio(u8),

    /// Above-threshold table access from the `0xA0` dispatcher
    /// bracket — arms a per-channel **volume envelope** (Ghidra:
    /// the `0xA0` handler at `Play+0x412` drives `AUD0VOL`). Carries
    /// the raw on-disk byte; the runtime subtracts the per-module
    /// `volume_envelope_threshold` to get the envelope-table index.
    /// (On 2-bracket modules like tetris this bracket instead carries
    /// pitch — see `volume_bracket_is_pitch`.)
    SetVolumeEnvelope(u8),

    /// Standard commands (spec §5.2, codes 0..=14).
    EndOfTrack,
    Slide {
        speed: i8,
        counter: u8,
    },
    Mute,
    WaitUntilNextRow,
    StopSong,
    GlobalTranspose(i8),
    StartVibrato {
        speed: u8,
        max: u8,
    },
    StopVibrato,
    Effect8(u8),
    Effect9(u8),
    SetSpeed(u8),
    GlobalVolumeFade(u8),
    SetGlobalVolume(u8),
    StartOrStopSoundFx(u8),
    StopSoundFx,

    /// Re-trigger the channel's **current** note (jump-table family,
    /// cmd 0x83). Its handler (@0x4fa → @0x2fc) rewrites the channel
    /// stream pointer to the position just after the byte, reloads the
    /// duration counter from `(0x1c,A0)`, and re-strikes Paula DMA —
    /// i.e. it sounds the same note again (same sample/envelope/
    /// arpeggio) without consuming a new note from the stream. Without
    /// modelling it a channel loses one strike per occurrence and its
    /// melody drifts out of phase against the others (verified on
    /// bubble bobble song 1 ch2: desync starts exactly at the first
    /// `0x83`, row 244). Carries no inline args.
    Retrigger,

    /// Position-sequencer pointer set (`cmd 0x89` on the new player:
    /// `MOVE.B (A1)+,(0x8,A0)` ×2 → `chan+0x600`, index reset). Carries
    /// the raw 2-byte **sub-sequence pointer** (A3-relative, pre-rebase).
    /// At runtime it redirects which list `SeqAdvance` (0x80) walks, so a
    /// track ending `SeqPtr(X) ; SeqAdvance` makes the channel continue
    /// (and loop) inside the sub-sequence at `X` rather than the initial
    /// position list — the importer resolves this into the position
    /// list's `loop_to` at load (see `follow_seq_ptr_loops`). The runtime
    /// itself treats it as a no-op (the rebuilt list already encodes it).
    SeqPtr(u16),
}

impl DwTrackEvent {
    /// `true` for the `EndOfTrack` terminator — useful when
    /// walking events to know where the channel hands off to
    /// `advance_position` (spec §9.2).
    #[inline]
    pub fn is_terminator(&self) -> bool {
        matches!(self, DwTrackEvent::EndOfTrack)
    }

    /// `true` for plain note events (the only family that
    /// triggers a Paula sample on the current channel).
    #[inline]
    pub fn is_note(&self) -> bool {
        matches!(self, DwTrackEvent::Note(_))
    }
}

/// Decode a raw track byte stream into a list of typed events.
///
/// `dispatcher` carries the per-module SetSample / SetVolumeEnvelope /
/// SetPitchArpeggio thresholds extracted by detection's
/// `find_dispatcher_cascade_with_offsets`. When detection
/// fails or yields no thresholds, the decoder falls back to the
/// canonical `0xB0 / 0xA0 / 0x90` cascade — that's the historical
/// default and matches most modules in the wild, but not all
/// (Xenon 2 title uses `0xA0 / 0x90` with no envelope, so the
/// fallback misclassifies 16 bytes per affected module). Always
/// pass a real dispatcher when you have one.
pub fn decode_track(
    bytes: &[u8],
    dispatcher: &super::detect::DwDispatcher,
    features: &super::detect::DwFeatures,
    command_map: Option<&super::command_map::DwCommandMap>,
) -> Vec<DwTrackEvent> {
    decode_track_counted(bytes, dispatcher, features, command_map).0
}

/// Like [`decode_track`] but also returns the number of input bytes
/// consumed — i.e. the position just past the `EndOfTrack`
/// terminator (or `bytes.len()` if none was reached). This is the
/// **single authority** on per-command parameter widths: callers that
/// need to know where a track's raw byte span ends (e.g.
/// `read_track_bytes`, which reads a generous window and trims it)
/// use this count instead of re-deriving the widths, so the two can
/// never disagree.
pub fn decode_track_counted(
    bytes: &[u8],
    dispatcher: &super::detect::DwDispatcher,
    features: &super::detect::DwFeatures,
    command_map: Option<&super::command_map::DwCommandMap>,
) -> (Vec<DwTrackEvent>, usize) {
    let mut out: Vec<DwTrackEvent> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        i += 1;

        // 1. Notes
        if b < 0x80 {
            out.push(DwTrackEvent::Note(b));
            continue;
        }

        // 2. Long-form wait
        if b >= 0xE0 {
            out.push(DwTrackEvent::LongWait(b));
            continue;
        }

        // 3. Above-threshold table dispatchers. Each Whittaker
        //    new-player build stacks up to three of these on top
        //    of the 15 standard commands, in descending threshold
        //    order:
        //
        //      [dispatcher.thresholds[0] .. 0xE0)  →  SetSample
        //      [dispatcher.thresholds[1] .. t[0])  →  SetVolumeEnvelope (if present)
        //      [dispatcher.thresholds[2] .. t[1])  →  SetPitchArpeggio (if present)
        //
        //    Bytes between `0x80` and the smallest detected
        //    threshold flow through to the standard-command
        //    branch. Thresholds come from detection's
        //    `find_dispatcher_cascade_with_offsets`, which
        //    walks the per-tick CMP / BLT / SUBI cascade on a
        //    per-module basis. When detection failed we fall back
        //    to `0xB0 / 0xA0 / 0x90` — historically the most
        //    common shape, but only a heuristic.
        let (sample_t, vol_t, arp_t) = dispatch_thresholds(dispatcher);
        if b >= sample_t {
            out.push(DwTrackEvent::SetSample(b));
            continue;
        }
        if let Some(t) = vol_t {
            if b >= t {
                out.push(DwTrackEvent::SetVolumeEnvelope(b));
                continue;
            }
        }
        if let Some(t) = arp_t {
            if b >= t {
                out.push(DwTrackEvent::SetPitchArpeggio(b));
                continue;
            }
        }

        // 3b. Per-module jump-table command remap (C3 / bubble bobble
        //     family). When a `command_map` is present the low command
        //     bytes 0x80..0x8F mean different effects than the canonical
        //     layout; emit the matching event. Argument counts already
        //     align with the canonical table, so this never desyncs the
        //     stream — only the *meaning* changes (e.g. cmd 1 is Vibrato
        //     here, not Slide; cmd 6 is Slide, not StartVibrato).
        if let Some(cmd) = command_map.and_then(|m| m.get(b)) {
            use super::command_map::DwCmdKind as K;
            let ev: Option<DwTrackEvent> = match cmd.kind {
                K::SeqAdvance => Some(DwTrackEvent::EndOfTrack),
                K::Slide => {
                    // Canonical 2-arg slide (same layout as the standard
                    // cmd 1): signed period step, then a counter.
                    let speed = take_byte(bytes, &mut i) as i8;
                    let counter = take_byte(bytes, &mut i);
                    Some(DwTrackEvent::Slide { speed, counter })
                }
                K::Vibrato => {
                    let speed = take_byte(bytes, &mut i);
                    let max = take_byte(bytes, &mut i);
                    Some(DwTrackEvent::StartVibrato { speed, max })
                }
                K::NoteStop => Some(DwTrackEvent::Mute),
                K::GlobalTranspose => {
                    Some(DwTrackEvent::GlobalTranspose(take_byte(bytes, &mut i) as i8))
                }
                K::SlideOn => {
                    // cmd 0x86 (handler @0x512) is NOT a linear slide: it
                    // arms the bounded **triangle** oscillation in
                    // `do_track_cmd` (accumulator ramps `0↔target` by
                    // `speed`, period `±accumulator`, sign flipping at the
                    // bounds). The Paula oracle on bubble bobble ch0 shows
                    // `353 ± {0,3,6}` — a centred triangle vibrato, not a
                    // ramp. So map it to the vibrato LFO: `speed` byte =
                    // ramp step, `target` byte = amplitude. (Listening
                    // confirmed the old linear `Slide` mapping was
                    // audibly wrong — a pitch that drifts off instead of
                    // wobbling.) `attach_vibrato_lanes` then yields a
                    // `BipolarTriangle` LFO whose rate `64·speed/target`
                    // matches the replayer's `4·target/speed`-frame cycle.
                    let speed = take_byte(bytes, &mut i);
                    let max = take_byte(bytes, &mut i);
                    Some(DwTrackEvent::StartVibrato { speed, max })
                }
                // cmd 0x87 clears the slide/vibrato flag → end the wobble.
                K::SlideOff => Some(DwTrackEvent::StopVibrato),
                K::ChannelTranspose => Some(DwTrackEvent::Effect8(take_byte(bytes, &mut i))),
                // cmd 0x84's handler (@0x4fe) is `BRA.W FUN_ec`, which
                // kills all Paula DMA (`DMACON = 0x000f`) and zeroes the
                // channel outputs — a global **song stop** (verified:
                // bubble bobble song 0 goes hard-silent at tick 444 when
                // ch1 hits this byte, matching the Paula oracle). Halt the
                // song here; the player then loops the real ~148-row tune
                // instead of running off into 10+ phantom patterns.
                K::ResetAll => Some(DwTrackEvent::StopSong),
                // cmd 0x83: classified `Retrigger` because both `0x83`
                // jump-table targets begin with `MOVE.L A1,(0x4,A0)`. But
                // that opcode alone doesn't decide re-strike vs tie — the
                // play loop's `CMPI.B #$83,(A1)` guard does. When present
                // (`note_repeat_is_tie`), the replayer skips its DMA clear
                // before a `0x83`, so the note is *held* (extended for
                // another wait window, no re-attack) — identical to the
                // standard-path `WaitUntilNextRow`. Without the guard
                // (bubble bobble / grimblood) it's a genuine re-strike.
                // Ghidra + Paula oracle: xenon2 ch1 holds one B-3 across
                // row 0x38 with a decaying volume rather than re-attacking.
                K::Retrigger => Some(if features.note_repeat_is_tie {
                    DwTrackEvent::WaitUntilNextRow
                } else {
                    DwTrackEvent::Retrigger
                }),
                // Position-sequencer pointer set (2 inline bytes = the
                // sub-sequence pointer). Surface it so the importer can
                // resolve the channel's real loop target (a track ending
                // `SeqPtr(X) ; SeqAdvance` loops inside `X`, not the
                // initial position list). The runtime treats it as inert.
                K::SeqPtr => {
                    let hi = take_byte(bytes, &mut i);
                    let lo = take_byte(bytes, &mut i);
                    Some(DwTrackEvent::SeqPtr(u16::from_be_bytes([hi, lo])))
                }
                // Structural / not-yet-modelled handlers: consume their
                // inline args so the stream stays aligned, emit nothing.
                K::GlobalParam | K::Unknown => {
                    for _ in 0..cmd.args {
                        let _ = take_byte(bytes, &mut i);
                    }
                    None
                }
            };
            if let Some(e) = ev {
                let is_term = matches!(e, DwTrackEvent::EndOfTrack);
                out.push(e);
                if is_term {
                    break;
                }
            }
            continue;
        }

        // 4. Standard command (cmd code 0..=14, byte 0x80..=0x8E).
        let event = match b & 0x7F {
            0 => DwTrackEvent::EndOfTrack,
            1 => {
                let speed = take_byte(bytes, &mut i) as i8;
                let counter = take_byte(bytes, &mut i);
                DwTrackEvent::Slide { speed, counter }
            }
            2 => DwTrackEvent::Mute,
            3 => DwTrackEvent::WaitUntilNextRow,
            4 => DwTrackEvent::StopSong,
            5 => DwTrackEvent::GlobalTranspose(take_byte(bytes, &mut i) as i8),
            6 => {
                let speed = take_byte(bytes, &mut i);
                let max = take_byte(bytes, &mut i);
                DwTrackEvent::StartVibrato { speed, max }
            }
            7 => DwTrackEvent::StopVibrato,
            8 => DwTrackEvent::Effect8(take_byte(bytes, &mut i)),
            9 => {
                // Effect9's parameter width is module-dependent
                // (Ghidra: the effect-handler's arg consumption): **0
                // bytes** when the module's `Effect9` toggles
                // half-volume off, otherwise **2 bytes** (a
                // position-restart pair consumed but handled at
                // load time). Reading a fixed 1 byte — as this
                // decoder used to — misaligns every event after an
                // `Effect9` on modules that use it (xenon2/tetris
                // happen to have none, which is why they were
                // unaffected).
                if features.enable_half_volume {
                    DwTrackEvent::Effect9(0)
                } else {
                    let a = take_byte(bytes, &mut i);
                    let _b = take_byte(bytes, &mut i);
                    DwTrackEvent::Effect9(a)
                }
            }
            10 => DwTrackEvent::SetSpeed(take_byte(bytes, &mut i)),
            11 => DwTrackEvent::GlobalVolumeFade(take_byte(bytes, &mut i)),
            12 => DwTrackEvent::SetGlobalVolume(take_byte(bytes, &mut i)),
            13 => DwTrackEvent::StartOrStopSoundFx(take_byte(bytes, &mut i)),
            14 => DwTrackEvent::StopSoundFx,
            // Unreachable on a well-formed `.dw`: every byte
            // 0x80..=0x8E maps into one of the arms above. A
            // future-proof default is `EndOfTrack` so a
            // truncated stream still terminates cleanly.
            _ => DwTrackEvent::EndOfTrack,
        };
        let is_term = matches!(event, DwTrackEvent::EndOfTrack);
        out.push(event);
        if is_term {
            break;
        }
    }
    (out, i)
}

/// Resolve `(sample, volume-envelope, pitch-arpeggio)` thresholds
/// from a [`DwDispatcher`], falling back to the canonical split when
/// detection didn't yield anything. The canonical values reflect
/// the most common Whittaker module shape but are not universal —
/// modules like `dw.xenon 2 (title)` use `0xA0 / 0x90` without
/// envelope, and the fallback misclassifies 16 byte values for
/// those. Always plumb a detected `DwDispatcher` through when you
/// have one.
fn dispatch_thresholds(d: &super::detect::DwDispatcher) -> (u8, Option<u8>, Option<u8>) {
    if d.thresholds.is_empty() {
        // Canonical default — three brackets.
        (0xB0, Some(0xA0), Some(0x90))
    } else {
        (
            d.sample_threshold().unwrap_or(0xB0),
            d.volume_envelope_threshold(),
            d.pitch_arpeggio_threshold(),
        )
    }
}

#[inline]
fn take_byte(bytes: &[u8], cursor: &mut usize) -> u8 {
    if *cursor < bytes.len() {
        let v = bytes[*cursor];
        *cursor += 1;
        v
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_notes_and_end() {
        // Three notes, then terminator
        let bytes = [0x10, 0x20, 0x30, 0x80];
        let events = decode_track(&bytes, &Default::default(), &Default::default(), None);
        assert_eq!(
            events,
            alloc::vec![
                DwTrackEvent::Note(0x10),
                DwTrackEvent::Note(0x20),
                DwTrackEvent::Note(0x30),
                DwTrackEvent::EndOfTrack,
            ]
        );
    }

    #[test]
    fn decodes_slide_with_two_params() {
        // Slide #1 with speed=-3, counter=5 — then EndOfTrack.
        let bytes = [0x81, 0xFD, 0x05, 0x80];
        let events = decode_track(&bytes, &Default::default(), &Default::default(), None);
        assert_eq!(
            events,
            alloc::vec![
                DwTrackEvent::Slide {
                    speed: -3,
                    counter: 5
                },
                DwTrackEvent::EndOfTrack,
            ]
        );
    }

    #[test]
    fn decodes_long_wait_and_set_sample() {
        // SetSample (0xB2 = sample index 2 with the canonical
        // 0xB0 threshold) — then long wait (0xE3 = wait 4*speed
        // frames) — then EndOfTrack.
        let bytes = [0xB2, 0xE3, 0x80];
        let events = decode_track(&bytes, &Default::default(), &Default::default(), None);
        assert_eq!(
            events,
            alloc::vec![
                DwTrackEvent::SetSample(0xB2),
                DwTrackEvent::LongWait(0xE3),
                DwTrackEvent::EndOfTrack,
            ]
        );
    }

    #[test]
    fn decodes_table_dispatch_ranges() {
        // Bytes 0x90..0x9F → SetPitchArpeggio, 0xA0..0xAF → SetVolumeEnvelope,
        // 0xB0..0xDF → SetSample. Hand-craft one of each then a
        // terminator.
        let bytes = [0x95, 0xA3, 0xC1, 0x80];
        let events = decode_track(&bytes, &Default::default(), &Default::default(), None);
        assert_eq!(
            events,
            alloc::vec![
                DwTrackEvent::SetPitchArpeggio(0x95),
                DwTrackEvent::SetVolumeEnvelope(0xA3),
                DwTrackEvent::SetSample(0xC1),
                DwTrackEvent::EndOfTrack,
            ]
        );
    }

    #[test]
    fn terminator_truncates_trailing_garbage() {
        // EndOfTrack should stop the decoder; trailing bytes are
        // ignored even when they look like valid commands.
        let bytes = [0x80, 0x10, 0x20];
        let events = decode_track(&bytes, &Default::default(), &Default::default(), None);
        assert_eq!(events, alloc::vec![DwTrackEvent::EndOfTrack]);
    }
}
