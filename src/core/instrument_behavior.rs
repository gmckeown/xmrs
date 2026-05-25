//! `InstrumentBehavior` — how a channel reacts when an instrument is
//! retriggered.
//!
//! Splits cleanly into NNA (New Note Action — what happens to the
//! voice that was playing) and DCT/DCA (Duplicate Check Type and
//! Action — what happens when a new note matches some property of
//! an already-playing voice). The rules are best read together as a
//! truth table; see the table at the top of this module.

use serde::{Deserialize, Serialize};

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

/// Determines what happens when a new note is played on a channel
/// that is already occupied by another note.
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

/// How an instrument reacts when retriggered: which voice survives,
/// what happens to the previous one, what counts as a "duplicate".
#[derive(Default, Clone, Serialize, Deserialize, Debug)]
pub struct InstrumentBehavior {
    pub duplicate_check: DuplicateCheckType,
}
