//! The per-frame **voice program** interpreter — Rob Hubbard's "wavetable".
//!
//! One step is consumed per frame (50 Hz). A step sets the voice's control
//! register (waveform + modifiers) and does one of two things to the pitch:
//! bend it by a signed amount that ACCUMULATES across steps, or pin the
//! frequency register's high byte outright. After the last step the program
//! either freezes on it or jumps back and cycles.
//!
//! This is the runtime half of [`crate::core::instr_robsid::VoiceProgram`]; the
//! model half is what an instrument editor binds to. The engine works on a
//! flattened, `Copy` mirror ([`ProgramSteps`]) for the same reason `SidFx`
//! flattens `Option<BounceRange>`: the driver's per-voice state must stay `Copy`
//! and allocation-free.
//!
//! **Ownership.** While a program runs it owns the voice's waveform and pitch —
//! the per-frame effects that would otherwise write them do not apply.
//!
//! **Gate asymmetry (Ghidra `lh_wave_engine $1699`).** A `Fixed` step writes the
//! control and frequency registers itself and bypasses the end-of-note gate-off
//! mask; `Bend` steps and the frozen tail go through the masked write and DO get
//! gated off. Reproduced here by [`StepOutput::masked`].

use crate::core::instr_robsid::{PitchAction, ProgramEnd, VoiceProgram};

/// Engine-side capacity for one program. Hand-authored replayer tables are
/// short — the longest in the reference tune (Lion_Heart) is 11 steps — so this
/// is a generous bound that keeps the per-voice state `Copy` and heap-free.
/// The conversion clamps and reports what it dropped rather than truncating
/// silently.
pub const MAX_STEPS: usize = 32;

/// One program step in the engine's flattened form.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct FlatStep {
    /// Raw `$D404` control byte for this frame.
    pub ctrl: u8,
    /// Signed pitch increment added to the running offset (`Bend`), or 0.
    pub bend: i16,
    /// Absolute frequency-register HIGH byte (`Fixed`).
    pub fixed_hi: u8,
    /// Selects which of the two above applies.
    pub is_fixed: bool,
}

/// A program flattened for the engine: `Copy`, fixed capacity, no allocation.
#[derive(Clone, Copy)]
pub struct ProgramSteps {
    steps: [FlatStep; MAX_STEPS],
    len: u8,
    /// `Some(step)` = loop back there; `None` = freeze on the last step.
    loop_to: Option<u8>,
}

impl Default for ProgramSteps {
    fn default() -> Self {
        Self {
            steps: [FlatStep::default(); MAX_STEPS],
            len: 0,
            loop_to: None,
        }
    }
}

impl ProgramSteps {
    /// Is there a program to run at all?
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Flatten a model [`VoiceProgram`]. Returns the flattened program and the
    /// number of steps that did NOT fit, so a caller can report the truncation
    /// instead of it passing unnoticed.
    pub fn from_model(p: &VoiceProgram) -> (Self, usize) {
        let mut out = Self::default();
        let n = p.steps.len().min(MAX_STEPS);
        for (i, s) in p.steps.iter().take(n).enumerate() {
            out.steps[i] = FlatStep {
                ctrl: s.shape.to_ctrl(),
                bend: match s.pitch {
                    PitchAction::Bend(b) => b,
                    PitchAction::Fixed(_) => 0,
                },
                fixed_hi: match s.pitch {
                    PitchAction::Fixed(h) => h,
                    PitchAction::Bend(_) => 0,
                },
                is_fixed: matches!(s.pitch, PitchAction::Fixed(_)),
            };
        }
        out.len = n as u8;
        out.loop_to = match p.end {
            ProgramEnd::Hold => None,
            // A loop target past the end would hang the cursor outside the
            // program; clamp it to the last step (only reachable on corrupt or
            // hand-edited data — the replayer itself never bound-checks).
            ProgramEnd::Loop { step } => Some((step as usize).min(n.saturating_sub(1)) as u8),
        };
        (out, p.steps.len() - n)
    }
}

/// Per-voice program state. Reset at every note-on.
#[derive(Clone, Copy, Default)]
pub struct ProgramState {
    /// Index of the step to play on the next frame.
    pub cursor: u8,
    /// Running pitch offset, the sum of every `Bend` executed so far. It is
    /// SUBTRACTED from the played note (the replayer accumulates downward).
    pub bend_acc: i16,
}

/// What a program frame does to the voice's pitch.
///
/// The three cases are three different exits in the replayer's engine, and they
/// differ by more than which value is written — see [`step`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProgramPitch {
    /// A `Bend` step: subtract the running accumulator from the played note.
    /// The replayer does this IN the step (`$16f4`), so it happens on bend
    /// frames only — not on every frame of the program.
    Bend,
    /// A `Fixed` step: write this frequency-register HIGH byte and nothing
    /// else, leaving `$d400` as the last full write left it (`$16cb`).
    FixedHi(u8),
    /// The program has ended (`$85`): it no longer touches the pitch at all,
    /// and the played note — bend accumulator NOT applied — is what sounds.
    Released,
}

/// What one frame of a program asks the driver to write.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StepOutput {
    /// Control byte for this frame.
    pub ctrl: u8,
    /// What this frame does to the pitch.
    pub pitch: ProgramPitch,
    /// Does the end-of-note gate-off mask apply to `ctrl` this frame? False on
    /// a `Fixed` step, which the replayer writes raw (see the module docs).
    pub masked: bool,
}

/// Advance one frame. Returns what to write, or `None` when there is no program.
///
/// `Hold` does NOT mean "re-emit the last step forever". The `$85` terminator
/// (Ghidra `$16bc`) jumps straight to the register write at `$17b7` without
/// touching either the control or the frequency: the cursor parks on the
/// terminator byte and, from that frame on, the program stops contributing.
/// The waveform stays whatever the last step left in the control shadow
/// (`$1a43,X`), but the PITCH goes back to the played note — vibrato,
/// arpeggio, portamento and the accumulated bend all applying normally. Holding
/// the last step's pinned frequency instead left Lion_Heart's voice 0 stuck a
/// major seventh above the score for the whole tail of every note.
pub fn step(prog: &ProgramSteps, st: &mut ProgramState) -> Option<StepOutput> {
    if prog.len == 0 {
        return None;
    }
    let last = prog.len - 1;
    if st.cursor >= prog.len {
        match prog.loop_to {
            // `$86 N` re-dispatches within the SAME frame (`$16b7` jumps back
            // to the engine's entry), so a loop costs no frame of its own.
            Some(t) => st.cursor = t,
            None => {
                return Some(StepOutput {
                    ctrl: prog.steps[last as usize].ctrl,
                    pitch: ProgramPitch::Released,
                    masked: true,
                })
            }
        }
    }
    let s = prog.steps[st.cursor as usize];
    let out = if s.is_fixed {
        StepOutput {
            ctrl: s.ctrl,
            pitch: ProgramPitch::FixedHi(s.fixed_hi),
            masked: false,
        }
    } else {
        st.bend_acc = st.bend_acc.wrapping_add(s.bend);
        StepOutput {
            ctrl: s.ctrl,
            pitch: ProgramPitch::Bend,
            masked: true,
        }
    };
    // Past the last step the cursor parks on the terminator (`len`), which the
    // block above resolves — looping back, or ending the program for good.
    st.cursor += 1;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::instr_robsid::{WaveShape, WaveStep};
    use alloc::vec;

    fn shape(ctrl: u8) -> WaveShape {
        WaveShape::from_ctrl(ctrl)
    }

    fn bend(ctrl: u8, b: i16) -> WaveStep {
        WaveStep {
            shape: shape(ctrl),
            pitch: PitchAction::Bend(b),
        }
    }

    fn fixed(ctrl: u8, hi: u8) -> WaveStep {
        WaveStep {
            shape: shape(ctrl),
            pitch: PitchAction::Fixed(hi),
        }
    }

    /// The control byte must survive a round trip through the editor-facing
    /// `WaveShape` fields — otherwise a UI edit would silently change the timbre.
    #[test]
    fn wave_shape_round_trips_every_control_byte() {
        for b in 0u8..=255 {
            assert_eq!(WaveShape::from_ctrl(b).to_ctrl(), b, "ctrl {b:#04x}");
        }
    }

    /// Lion_Heart instrument 0, decoded from the real table at `$1c4b`
    /// (`81 20 | 41 01 04 | 40 40 02 | 80 30 | … | 85`): a noise hit pinned at
    /// freq-hi `$20`, then two bending pulse steps — the second with the gate
    /// CLEARED, releasing the note mid-program — then noise steps, then freeze.
    #[test]
    fn lion_heart_instr0_program_matches_the_replayer_table() {
        let p = VoiceProgram {
            steps: vec![
                fixed(0x81, 0x20),
                bend(0x41, 0x0401),
                bend(0x40, 0x0240),
                fixed(0x80, 0x30),
                fixed(0x80, 0x15),
            ],
            end: ProgramEnd::Hold,
        };
        let (flat, dropped) = ProgramSteps::from_model(&p);
        assert_eq!(dropped, 0);
        let mut st = ProgramState::default();

        // Frame 1: noise pinned at $20, written raw (no gate mask).
        let o = step(&flat, &mut st).unwrap();
        assert_eq!(o.pitch, ProgramPitch::FixedHi(0x20));
        assert!(!o.masked, "a Fixed step bypasses the end-of-note gate mask");
        assert_eq!(st.bend_acc, 0, "a Fixed step does not touch the bend");

        // Frames 2-3: the bend ACCUMULATES, and step 3 clears the gate.
        let o = step(&flat, &mut st).unwrap();
        assert_eq!(o.pitch, ProgramPitch::Bend);
        assert!(o.masked);
        assert_eq!(st.bend_acc, 0x0401);
        let o = step(&flat, &mut st).unwrap();
        assert_eq!(
            st.bend_acc,
            0x0401 + 0x0240,
            "bends sum, they do not replace"
        );
        assert!(
            !WaveShape::from_ctrl(o.ctrl).gate,
            "step 3 releases the note mid-program"
        );

        // Frames 4-5: back to pinned noise.
        assert_eq!(
            step(&flat, &mut st).unwrap().pitch,
            ProgramPitch::FixedHi(0x30)
        );
        assert_eq!(
            step(&flat, &mut st).unwrap().pitch,
            ProgramPitch::FixedHi(0x15)
        );

        // Past the end the `$85` terminator stops the program: the waveform
        // stays where the last step left it, but the pitch is handed back to
        // the played note (no more pinning) and the bend stops growing.
        for _ in 0..4 {
            let o = step(&flat, &mut st).unwrap();
            assert_eq!(
                o.pitch,
                ProgramPitch::Released,
                "the terminator no longer pins the pitch"
            );
            assert_eq!(o.ctrl, 0x80, "the last step's waveform is what persists");
            assert!(
                o.masked,
                "the terminator goes through the gate-masked write"
            );
        }
        assert_eq!(
            st.bend_acc,
            0x0401 + 0x0240,
            "the terminator must not keep bending the pitch"
        );
    }

    /// `Loop` cycles instead of freezing — and keeps accumulating the bend, so a
    /// looping program of bends is a continuous glide, not a repeating one.
    #[test]
    fn loop_cycles_and_keeps_accumulating() {
        let p = VoiceProgram {
            steps: vec![bend(0x11, 10), bend(0x21, 20), bend(0x41, 30)],
            end: ProgramEnd::Loop { step: 1 },
        };
        let (flat, _) = ProgramSteps::from_model(&p);
        let mut st = ProgramState::default();
        let ctrls: alloc::vec::Vec<u8> =
            (0..7).map(|_| step(&flat, &mut st).unwrap().ctrl).collect();
        assert_eq!(ctrls, vec![0x11, 0x21, 0x41, 0x21, 0x41, 0x21, 0x41]);
        assert_eq!(st.bend_acc, 10 + 20 + 30 + 20 + 30 + 20 + 30);
    }

    /// An out-of-range loop target is hand-editable data; it must clamp, never
    /// leave the cursor outside the program.
    #[test]
    fn out_of_range_loop_target_clamps() {
        let p = VoiceProgram {
            steps: vec![bend(0x11, 0), bend(0x21, 0)],
            end: ProgramEnd::Loop { step: 99 },
        };
        let (flat, _) = ProgramSteps::from_model(&p);
        let mut st = ProgramState::default();
        for _ in 0..6 {
            let o = step(&flat, &mut st).unwrap();
            assert!(o.ctrl == 0x11 || o.ctrl == 0x21);
        }
    }

    /// Over-long programs clamp and REPORT the drop — a silent truncation would
    /// read as "imported fine" when part of the timbre went missing.
    #[test]
    fn oversized_program_reports_what_it_dropped() {
        let p = VoiceProgram {
            steps: vec![bend(0x11, 1); MAX_STEPS + 5],
            end: ProgramEnd::Hold,
        };
        let (flat, dropped) = ProgramSteps::from_model(&p);
        assert_eq!(dropped, 5);
        assert!(!flat.is_empty());
    }

    /// No program at all: the engine must say so, not emit a silent step, so the
    /// ordinary per-frame effects keep the voice.
    #[test]
    fn empty_program_yields_nothing() {
        let (flat, _) = ProgramSteps::from_model(&VoiceProgram::default());
        let mut st = ProgramState::default();
        assert!(flat.is_empty());
        assert!(step(&flat, &mut st).is_none());
    }
}
