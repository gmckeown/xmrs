//! `Keyboard` — per-input-note mapping for an instrument.
//!
//! IT-style instruments can remap each of the 120 chromatic input
//! keys to two independent things:
//!
//! - which sample to play (drum-kit: kick on C, snare on D, etc.)
//! - what pitch to play it at (transposing each key, so a single
//!   sample plays multiple notes)
//!
//! Other formats (XM/MOD/S3M/Amiga) don't have a per-input-note
//! mapping at all. For them every entry stays at `None` and the
//! playback logic uses the input note directly. Materialising this
//! split into its own type makes that distinction explicit and
//! takes the lookup tables out of the already-busy `InstrDefault`.

use serde::{Deserialize, Serialize};

/// Per-input-note keyboard layout.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Keyboard {
    /// Per-input-note sample selection: `sample_for_pitch[input_note]
    /// = Some(sample_index)` when this instrument's keyboard table
    /// directs `input_note` to a specific sample.
    ///
    /// IT-format instruments accompany this with a sibling
    /// `note_for_pitch` table: the IT keyboard layout pairs each
    /// input note with `(output_note, sample)`. The `output_note`
    /// half is the note actually played (i.e. the transposition the
    /// drum-kit author wired in); the `sample` half lands here.
    /// Other formats (XM/MOD/S3M/Amiga) leave `note_for_pitch` at
    /// `None` and the playback note is the input note itself.
    #[serde(with = "big_array_120")]
    pub sample_for_pitch: [Option<usize>; 120],

    /// Per-input-note transposition map: `note_for_pitch[input_note]
    /// = Some(output_note)` when this instrument's keyboard table
    /// remaps `input_note` to a different pitch — IT drum-kit
    /// instruments use this to make several keys trigger the same
    /// underlying sample at different pitches, or different samples
    /// at the same pitch (sample-mapped percussion).
    ///
    /// `None` means "play this input note at its own pitch" — the
    /// identity remap. XM/MOD/S3M leave the entire array at `None`
    /// since those formats don't have a per-note transposition
    /// concept; for them the field is inert.
    ///
    /// The output note is stored as a raw `u8` (the same scale as
    /// `Pitch::value()`) rather than a `Pitch`, so the importer
    /// doesn't need to materialise a `Pitch` per entry. The
    /// player's `played_pitch_for` helper handles the `Pitch`
    /// conversion lazily on lookup.
    #[serde(with = "big_array_120")]
    pub note_for_pitch: [Option<u8>; 120],
}

impl Default for Keyboard {
    fn default() -> Self {
        Self {
            sample_for_pitch: [None; 120],
            note_for_pitch: [None; 120],
        }
    }
}

impl Keyboard {
    /// Set every input-note's sample mapping to `sample_index`.
    /// Used by importers that have a single sample but want it
    /// triggered uniformly across the whole keyboard (i.e. classic
    /// non-drum-kit instruments).
    pub fn map_all_to(&mut self, sample_index: usize) {
        self.sample_for_pitch
            .iter_mut()
            .for_each(|elem| *elem = Some(sample_index));
    }
}

/// Inline replacement for `serde_big_array::BigArray` specialised
/// for the keyboard's `[T; 120]` shape. Serde supports `[T; N]`
/// natively only for `N ≤ 32`, so we provide our own (de)serialiser
/// for `[Option<usize>; 120]` and `[Option<u8>; 120]`. Replaces a
/// whole crate dep for ~50 lines.
mod big_array_120 {
    use core::fmt;
    use core::marker::PhantomData;
    use serde::de::{Error, SeqAccess, Visitor};
    use serde::ser::SerializeTuple;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    const N: usize = 120;

    pub fn serialize<S, T>(arr: &[T; N], ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        T: Serialize,
    {
        let mut tup = ser.serialize_tuple(N)?;
        for v in arr {
            tup.serialize_element(v)?;
        }
        tup.end()
    }

    pub fn deserialize<'de, D, T>(de: D) -> Result<[T; N], D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de> + Default + Copy,
    {
        struct ArrayVisitor<T> {
            _t: PhantomData<T>,
        }

        impl<'de, T> Visitor<'de> for ArrayVisitor<T>
        where
            T: Deserialize<'de> + Default + Copy,
        {
            type Value = [T; N];

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "an array of length {N}")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<[T; N], A::Error> {
                let mut out = [T::default(); N];
                for (i, slot) in out.iter_mut().enumerate() {
                    *slot = seq
                        .next_element()?
                        .ok_or_else(|| A::Error::invalid_length(i, &self))?;
                }
                Ok(out)
            }
        }

        de.deserialize_tuple(N, ArrayVisitor { _t: PhantomData })
    }
}
