//! Reusable sequence placed on the timeline by one or more clips.
//!
//! Several [`crate::core::daw::clip::Clip`]s can point at the same `Track`
//! (shared link: editing the track affects every occurrence).
//! The note variants are split per instrument at import so that the
//! `instrument` field is invariant for the track's lifetime — a change
//! of instrument starts a new Track.
//!
//! A Track has one of three shapes:
//!
//! - [`Track::Notes`] — a vector of [`Cell`]s, one per row. The
//!   classic tracker representation; covers manual editing and every
//!   format importer's output.
//! - [`Track::Euclidean`] — a generator described by Bjorklund
//!   `(events, steps, rotation)` plus a single prototype cell. The
//!   row resolver synthesises one cell per playback row from the
//!   Bjorklund pattern.
//! - [`Track::Audio`] — a recorded-audio clip (no note grid): a
//!   [`Sample`] played from its placement and resampled to the output
//!   rate. Its waveform is an [`AudioSource`] — owned inline, or a
//!   reference into the module-level asset pool (RFC §4). The
//!   DAW-generalization track kind; no tracker importer produces it.

use crate::core::cell::Cell;
use crate::core::fixed::fixed::Q15;
use crate::core::sample::Sample;
use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};

/// RFC §4 — where a [`Track::Audio`] gets its recorded waveform.
///
/// This is the indirection that turns the per-instrument `Arc` sharing
/// of §1A into a real **asset pool**: a track can either own its
/// waveform inline or *reference* a shared entry in
/// [`Module.assets`](crate::core::module::Module::assets) by index, so
/// one recording placed many times on the timeline is stored once.
///
/// Resolution against the pool lives on `Module`
/// ([`track_audio`](crate::core::module::Module::track_audio)) because
/// only the module owns the pool; the track-level
/// [`Track::audio_sample`] convenience resolves the [`Self::Inline`]
/// case only.
///
/// `Inline` is the default shape every existing builder produces, so
/// nothing references the pool until an editor interns an asset — the
/// tracker corpus keeps an empty pool and a bit-identical render.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum AudioSource {
    /// Waveform owned inline by the track.
    Inline(Sample),
    /// Reference into the shared `Module.assets` pool by index.
    Pooled(usize),
}

/// One reusable cell sequence in the DAW layer.
///
/// Tracks are placed on the timeline through
/// [`Clip`](crate::core::daw::clip::Clip)s — one Track can be played
/// multiple times on multiple channels by binding multiple clips to it.
/// The instrument index is fixed for the Track's lifetime: a change of
/// instrument produces a new Track.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Track {
    /// Classic per-row cell sequence — one [`Cell`] per playback row.
    /// Produced by every format importer (XM / MOD / S3M / IT / SID)
    /// and by manual editing.
    Notes {
        name: String,
        /// Index into `Module.instrument`. Constant for the lifetime
        /// of this Track.
        ///
        /// Special value:
        /// [`EFFECT_ONLY_INSTRUMENT`](crate::tracker::import::build::EFFECT_ONLY_INSTRUMENT)
        /// marks a track that holds effect-only cells (vibrato,
        /// portamento, …) intended to modify a voice inherited from
        /// a previous pattern. The player processes only the effects.
        instrument: usize,
        /// Cells with effect memory already resolved.
        rows: Vec<Cell>,
        muted: bool,
    },
    /// Euclidean (Bjorklund) rhythm generator. The row resolver
    /// emits `prototype` on each pulse position of the canonical
    /// Bjorklund(`events`, `steps`) pattern rotated right by
    /// `rotation`, and an empty cell on the off-pulses. The track
    /// loops the pattern for the duration of every clip that
    /// references it.
    ///
    /// The Bjorklund parameters and the prototype were either
    /// authored directly in the editor or produced by the
    /// auto-conversion pass on import (a strict Bjorklund
    /// `Track::Notes` collapses to this form, lossless).
    Euclidean {
        name: String,
        /// Index into `Module.instrument`. Same constancy contract
        /// as [`Self::Notes::instrument`].
        instrument: usize,
        muted: bool,
        /// Number of pulses (`k` in Bjorklund(k, n) literature).
        events: u8,
        /// Length of one rhythm cycle in track rows
        /// (`n` in Bjorklund(k, n)).
        steps: u8,
        /// Right-rotation of the canonical Bresenham pattern.
        /// `0` = canonical (pulse on row 0).
        rotation: u8,
        /// Uniform cell rendered on every pulse position. Always a
        /// [`CellEvent::NoteOn`](crate::core::cell::CellEvent::NoteOn)
        /// in practice (the conversion pass requires fresh triggers).
        prototype: Cell,
        /// Maximum advance (in ticks) randomly applied to each
        /// pulse by the player when humanisation is on. `0` means
        /// no humanisation — cycle-exact strict.
        humanize_advance_max_ticks: u8,
        /// Probability that a given pulse is advanced.
        /// `Q15::ZERO` = never; `Q15::ONE` = every pulse. Drawn
        /// independently per pulse by the player.
        humanize_probability: Q15,
    },
    /// RFC §3C — a **recorded-audio** track: a single PCM clip
    /// (Arc-backed via §1A) played at its natural rate from its
    /// placement on the timeline, rather than triggering instrument
    /// notes. Additive — no tracker importer ever produces it, and the
    /// note/cell playback path is untouched, so existing renders are
    /// bit-identical. The actual audio-clip streaming/resampling in the
    /// player is a separate subsystem (see the RFC §3C status note);
    /// this is the data model.
    Audio {
        name: String,
        muted: bool,
        /// The recorded waveform — owned inline, or referenced from the
        /// shared [`Module.assets`](crate::core::module::Module::assets)
        /// pool (RFC §4). Reuses [`Sample`]'s Arc-backed PCM (§1A) and
        /// optional loop window in either case.
        source: AudioSource,
        /// PCM sample rate in Hz. The player resamples from this to its
        /// output rate. `0` means "already at the output rate" (no
        /// resampling). `#[serde(default)]` keeps older modules readable.
        #[serde(default)]
        source_rate: u32,
    },
}

impl Default for Track {
    fn default() -> Self {
        Track::Notes {
            name: String::new(),
            instrument: 0,
            rows: Vec::new(),
            muted: false,
        }
    }
}

impl Track {
    /// Display name, regardless of variant.
    #[inline]
    pub fn name(&self) -> &str {
        match self {
            Track::Notes { name, .. }
            | Track::Euclidean { name, .. }
            | Track::Audio { name, .. } => name,
        }
    }

    /// Mutable reference to the display name.
    #[inline]
    pub fn name_mut(&mut self) -> &mut String {
        match self {
            Track::Notes { name, .. }
            | Track::Euclidean { name, .. }
            | Track::Audio { name, .. } => name,
        }
    }

    /// Instrument index this track binds to.
    #[inline]
    pub fn instrument(&self) -> usize {
        match self {
            Track::Notes { instrument, .. } | Track::Euclidean { instrument, .. } => *instrument,
            // Audio tracks bind no instrument (they play raw PCM); the
            // value is never used because they trigger no note cells.
            Track::Audio { .. } => 0,
        }
    }

    /// Set the instrument index. Use sparingly — the field is
    /// expected to be constant for the lifetime of the track.
    #[inline]
    pub fn set_instrument(&mut self, idx: usize) {
        match self {
            Track::Notes { instrument, .. } | Track::Euclidean { instrument, .. } => {
                *instrument = idx;
            }
            Track::Audio { .. } => {}
        }
    }

    /// Per-track mute flag.
    #[inline]
    pub fn muted(&self) -> bool {
        match self {
            Track::Notes { muted, .. }
            | Track::Euclidean { muted, .. }
            | Track::Audio { muted, .. } => *muted,
        }
    }

    /// Set the mute flag.
    #[inline]
    pub fn set_muted(&mut self, m: bool) {
        match self {
            Track::Notes { muted, .. }
            | Track::Euclidean { muted, .. }
            | Track::Audio { muted, .. } => {
                *muted = m;
            }
        }
    }

    /// `true` if this is a [`Self::Euclidean`] variant.
    #[inline]
    pub fn is_euclidean(&self) -> bool {
        matches!(self, Track::Euclidean { .. })
    }

    /// Direct view on the per-row cell storage — `Some` only for
    /// [`Self::Notes`]. Generator variants synthesise cells via
    /// [`Self::cell_at`] instead.
    #[inline]
    pub fn rows(&self) -> Option<&[Cell]> {
        match self {
            Track::Notes { rows, .. } => Some(rows),
            Track::Euclidean { .. } | Track::Audio { .. } => None,
        }
    }

    /// Mutable access to the per-row cell storage — `Some` only for
    /// [`Self::Notes`]. Cell-editing commands gate on this returning
    /// `Some`; on a generator variant the editor must first convert
    /// to [`Self::Notes`] (expansion of the rhythm into explicit
    /// cells) before editing rows.
    #[inline]
    pub fn rows_mut(&mut self) -> Option<&mut Vec<Cell>> {
        match self {
            Track::Notes { rows, .. } => Some(rows),
            Track::Euclidean { .. } | Track::Audio { .. } => None,
        }
    }

    /// Like [`Self::rows`] but panics if the track is not
    /// [`Self::Notes`]. Test-helper for code paths that know they
    /// just created a Notes track.
    #[doc(hidden)]
    #[track_caller]
    pub fn expect_notes_rows(&self) -> &[Cell] {
        self.rows().expect("Track is not Track::Notes")
    }

    /// Like [`Self::rows_mut`] but panics if the track is not
    /// [`Self::Notes`].
    #[doc(hidden)]
    #[track_caller]
    pub fn expect_notes_rows_mut(&mut self) -> &mut Vec<Cell> {
        self.rows_mut().expect("Track is not Track::Notes")
    }

    /// Cell visible at row index `row` after expansion of any
    /// generator variant. For [`Self::Notes`] this is just
    /// `rows.get(row).cloned().unwrap_or_default()`. For
    /// [`Self::Euclidean`] the Bjorklund pattern is applied: the
    /// prototype is returned on pulse positions, `Cell::default()`
    /// elsewhere. The Euclidean rhythm loops every `steps` rows.
    pub fn cell_at(&self, row: u32) -> Cell {
        match self {
            Track::Notes { rows, .. } => rows.get(row as usize).cloned().unwrap_or_default(),
            Track::Euclidean {
                events,
                steps,
                rotation,
                prototype,
                ..
            } => {
                let steps_u32 = (*steps).max(1) as u32;
                let pos = row % steps_u32;
                let rot = (*rotation as u32) % steps_u32;
                let bjork_idx = ((pos + steps_u32 - rot) % steps_u32) as usize;
                let canonical = crate::core::daw::euclidean::euclidean_pattern(*events, *steps);
                if canonical.get(bjork_idx).copied().unwrap_or(false) {
                    prototype.clone()
                } else {
                    Cell::default()
                }
            }
            // Audio tracks carry no cells.
            Track::Audio { .. } => Cell::default(),
        }
    }

    /// Natural playback length of the track in rows. For
    /// [`Self::Notes`] this is `rows.len()`. For [`Self::Euclidean`]
    /// the pattern loops; this returns the *cycle length* (`steps`).
    pub fn natural_length(&self) -> usize {
        match self {
            Track::Notes { rows, .. } => rows.len(),
            Track::Euclidean { steps, .. } => *steps as usize,
            // Audio length is a duration on the timeline (clip.end_tick),
            // not a row count.
            Track::Audio { .. } => 0,
        }
    }

    /// `true` if this is a [`Self::Audio`] (recorded-PCM) track.
    #[inline]
    pub fn is_audio(&self) -> bool {
        matches!(self, Track::Audio { .. })
    }

    /// The waveform source of a [`Self::Audio`] track (inline or a pool
    /// reference); `None` for note/generator variants.
    #[inline]
    pub fn audio_source(&self) -> Option<&AudioSource> {
        match self {
            Track::Audio { source, .. } => Some(source),
            _ => None,
        }
    }

    /// The **inline** recorded waveform of a [`Self::Audio`] track.
    /// `None` for note/generator variants **and** for a track whose
    /// `source` is [`AudioSource::Pooled`] — pooled audio can only be
    /// resolved against the module's asset pool, so use
    /// [`Module::track_audio`](crate::core::module::Module::track_audio)
    /// for the pool-aware lookup.
    #[inline]
    pub fn audio_sample(&self) -> Option<&Sample> {
        match self {
            Track::Audio {
                source: AudioSource::Inline(sample),
                ..
            } => Some(sample),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::cell::CellEvent;
    use crate::core::fixed::units::{ChannelVolume, Finetune, Panning, Volume};
    use crate::core::sample::{LoopType, Sample, SampleDataType};

    fn audio_track() -> Track {
        let sample = Sample {
            name: "rec".into(),
            relative_pitch: 0,
            finetune: Finetune::default(),
            volume: ChannelVolume::FULL,
            default_note_volume: Volume::FULL,
            panning: Panning::CENTER,
            loop_flag: LoopType::No,
            loop_start: 0,
            loop_length: 0,
            sustain_loop_flag: LoopType::No,
            sustain_loop_start: 0,
            sustain_loop_length: 0,
            data: Some(SampleDataType::Mono8([0i8; 8].into())),
        };
        Track::Audio {
            name: "rec".into(),
            muted: false,
            source: AudioSource::Inline(sample),
            source_rate: 0,
        }
    }

    #[test]
    fn audio_track_api_is_additive_and_inert_on_cells() {
        let mut t = audio_track();
        assert!(t.is_audio());
        assert!(!t.is_euclidean());
        assert_eq!(t.name(), "rec");
        assert!(!t.muted());
        t.set_muted(true);
        assert!(t.muted());
        // No instrument binding, no cells, no row-based length.
        assert_eq!(t.instrument(), 0);
        t.set_instrument(3); // no-op
        assert_eq!(t.instrument(), 0);
        assert!(t.rows().is_none());
        assert!(t.rows_mut().is_none());
        assert_eq!(t.natural_length(), 0);
        assert_eq!(t.cell_at(0).event, CellEvent::None);
        assert_eq!(t.cell_at(99).event, CellEvent::None);
        // The recorded waveform is reachable.
        let s = t.audio_sample().expect("audio sample");
        assert_eq!(s.len(), 8);
    }
}
