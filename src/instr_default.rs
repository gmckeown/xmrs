use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;

use crate::instr_midi::InstrMidi;
use crate::sample::Sample;
use crate::vibrato::Vibrato;
use crate::{envelope::Envelope, pitch::Pitch};

use alloc::{vec, vec::Vec};

/*

| **Step 1: DCT**                  | **Step 2: NNA**       | **Detailed Explanation (in English)**                                                                 | **XM Compatible?** |
|----------------------------------|-----------------------|--------------------------------------------------------------------------------------------------------|--------------------|
| `Off`                            | `NoteCut`             | No duplicate checking. The new note immediately stops the previous one.                                | ✅ Yes             |
| `Off`                            | `Continue`            | No duplicate checking. The new note is played alongside the previous one.                              | ❌ No              |
| `Off`                            | `NoteOff`             | No duplicate checking. The previous note receives a **Note Off**, and the new one is played.           | ❌ No              |
| `Off`                            | `NoteFadeOut`         | No duplicate checking. The previous note begins a fade out, and the new one is played.                 | ❌ No              |
| `Note(NoteCut)`                  | `NoteCut`             | If a duplicate note (same pitch) is detected, it is immediately stopped, and a new note is played.     | ✅ Yes             |
| `Note(NoteCut)`                  | `Continue`            | If a duplicate note is detected, it is ignored, and the previous one continues playing.                | ❌ No              |
| `Note(NoteCut)`                  | `NoteOff`             | If a duplicate note is detected, the previous one receives a **Note Off**, and a new note is played.   | ❌ No              |
| `Note(NoteCut)`                  | `NoteFadeOut`         | If a duplicate note is detected, the previous one begins a fade out, and a new note is played.         | ❌ No              |
| `Note(NoteOff)`                  | `NoteCut`             | If a duplicate note is detected, the previous one receives a **Note Off**, and the new one stops it.   | ❌ No              |
| `Note(NoteOff)`                  | `Continue`            | If a duplicate note is detected, the previous one receives a **Note Off** and continues alongside the new one. | ❌ No        |
| `Note(NoteOff)`                  | `NoteOff`             | If a duplicate note is detected, the previous one receives a **Note Off**, and a new note is played.   | ❌ No              |
| `Note(NoteOff)`                  | `NoteFadeOut`         | If a duplicate note is detected, the previous one begins a fade out, and a new note is played.         | ❌ No              |
| `Note(NoteFadeOut)`              | `NoteCut`             | If a duplicate note is detected, the previous one begins a fade out, and the new note stops it.        | ❌ No              |
| `Note(NoteFadeOut)`              | `Continue`            | If a duplicate note is detected, the previous one begins a fade out but continues alongside the new one.| ❌ No              |
| `Note(NoteFadeOut)`              | `NoteOff`             | If a duplicate note is detected, the previous one begins a fade out, and a new note is played.         | ❌ No              |
| `Note(NoteFadeOut)`              | `NoteFadeOut`         | If a duplicate note is detected, the previous one begins a fade out, and a new note is played.         | ❌ No              |
| `Sample(NoteCut)`                | `NoteCut`             | If a note uses the same sample, the previous one is immediately stopped, and the new note is played.   | ❌ No              |
| `Sample(NoteCut)`                | `Continue`            | If a note uses the same sample, it is ignored, and the previous one continues playing.                 | ❌ No              |
| `Sample(NoteCut)`                | `NoteOff`             | If a note uses the same sample, the previous one receives a **Note Off**, and a new note is played.    | ❌ No              |
| `Sample(NoteCut)`                | `NoteFadeOut`         | If a note uses the same sample, the previous one begins a fade out, and a new note is played.          | ❌ No              |
| `Instrument(NoteCut)`            | `NoteCut`             | If a note uses the same instrument, the previous one is immediately stopped, and a new note is played. | ❌ No              |
| `Instrument(NoteCut)`            | `Continue`            | If a note uses the same instrument, it is ignored, and the previous one continues playing.             | ❌ No              |
| `Instrument(NoteCut)`            | `NoteOff`             | If a note uses the same instrument, the previous one receives a **Note Off**, and a new note is played.| ❌ No              |
| `Instrument(NoteCut)`            | `NoteFadeOut`         | If a note uses the same instrument, the previous one begins a fade out, and a new note is played.      | ❌ No              |
*/

/// Determines what happens when a new note is played on a channel that is already occupied by another note
#[derive(Default, Clone, Copy, Serialize, Deserialize, Debug, PartialEq)]
pub enum NewNoteAction {
    #[default]
    NoteCut,
    Continue,
    NoteOff,
    NoteFadeOut,
}

/// Action to take when a new note is played
#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum DuplicateCheckAction {
    /// The active note is cut off immediately
    NoteCut(NewNoteAction),
    /// The active note receives a Note Off
    NoteOff(NewNoteAction),
    /// The active note fades out to silence
    NoteFadeOut(NewNoteAction),
}

impl Default for DuplicateCheckAction {
    fn default() -> Self {
        Self::NoteCut(NewNoteAction::default())
    }
}

/// Duplicate checking
#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum DuplicateCheckType {
    /// No duplicate checking.
    Off(NewNoteAction),
    /// A new note is compared solely based on its pitch
    Note(DuplicateCheckAction),
    /// The sample used is compared
    Sample(DuplicateCheckAction),
    /// The entire instrument is taken into account
    Instrument(DuplicateCheckAction),
}

impl Default for DuplicateCheckType {
    fn default() -> Self {
        Self::Off(NewNoteAction::default())
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct InstrDefault {
    // === Pitch
    pub pitch_envelope: Envelope,
    pub pitch_envelope_as_low_pass_filter: bool,
    pub vibrato: Vibrato,

    // === Volume
    pub volume_envelope: Envelope,
    pub global_volume: f32,
    pub volume_fadeout: f32,
    /// Random volume variation
    pub random_volume_variation: f32,

    // === Panning
    pub pan_envelope: Envelope,
    pub default_pan: f32,
    // Pitch and pan separation (-1 to 1)
    pub pitch_pan_separation: f32,
    /// Center note for panning
    pub pitch_pan_center: Pitch,
    /// Random panning variation
    pub random_pan_variation: f32,

    // === Handling new note
    pub duplicate_check: DuplicateCheckType,

    /// Initial filter cutoff frequency (0-127)
    /// f = 110*2^(0.25+ce/fe),
    /// where ce is the cutoff frequency
    /// and fe is 24 for standard filter range
    /// or 20 if using OpenMPT's extended filter range.
    pub initial_filter_cutoff: u8,

    /// Initial filter resonance (0-127)
    /// The formula used is 10^((-resonance*24.0)/(128.0f*20.0f))
    pub initial_filter_resonance: u8,

    // === Midi
    pub midi: InstrMidi,
    pub midi_mute_computer: bool,

    // == Sample
    #[serde(with = "BigArray")]
    pub sample_for_pitch: [Option<usize>; 120],
    pub sample: Vec<Option<Sample>>,
}

impl Default for InstrDefault {
    fn default() -> Self {
        Self {
            pitch_envelope: Envelope::default(),
            pitch_envelope_as_low_pass_filter: false,
            vibrato: Vibrato::default(),

            volume_envelope: Envelope::default(),
            global_volume: 1.0,
            volume_fadeout: 0.0,
            random_volume_variation: 0.0,

            pan_envelope: Envelope::default(),
            default_pan: 0.5,
            pitch_pan_separation: 0.0,
            pitch_pan_center: Pitch::C4,
            random_pan_variation: 0.0,

            duplicate_check: DuplicateCheckType::Note(DuplicateCheckAction::NoteCut(
                NewNoteAction::NoteCut,
            )),

            initial_filter_cutoff: 0,
            initial_filter_resonance: 0,

            midi: InstrMidi::default(),
            midi_mute_computer: false,

            sample_for_pitch: [None; 120],
            sample: vec![],
        }
    }
}

impl InstrDefault {
    pub fn change_all_sample_for_pitch(&mut self, sample_index: usize) {
        self.sample_for_pitch
            .iter_mut()
            .for_each(|elem| *elem = Some(sample_index));
    }
}
