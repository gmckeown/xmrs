//! Read-only view of the importer-side, **pre-DAW** representation.
//!
//! Every format importer already parses its file into a faithful
//! mirror of the on-disk structures (`AmigaModule`, `XmModule`,
//! `S3mModule`, `ItModule`) before `to_module()` folds that into the
//! normalised [`Module`] and `build_timeline_layer` projects it onto
//! the DAW layer. Those per-format structs keep their fields private
//! — their layout tracks the file formats, not a public contract —
//! so until now the only way out of an importer was the normalised
//! `Module`.
//!
//! That is the wrong altitude for a whole class of consumers:
//! corpus analysis, censuses, near-duplicate detection, format
//! forensics. They want what the *file* said, not what the player
//! needs:
//!
//! * [`PatternSlot::effect_type`] / [`PatternSlot::effect_parameter`]
//!   are the **raw bytes**, not a semantic [`TrackEffect`]. A
//!   fingerprint built on them is stable across xmrs releases; one
//!   built on the normalised effect enum moves whenever an effect's
//!   translation is refined.
//! * The order list is a plain `&[u8]`, not dissolved into a
//!   `TimelineMap`.
//! * A pattern is a `[row][channel]` grid, directly hashable, with
//!   no clip/segment reconstruction.
//! * Nothing downstream of `load()` runs — no segment splitting, no
//!   automation extraction, no DAW build.
//!
//! The trade-off is the flip side of the same coin: this layer is
//! **not comparable across formats**. A `effect_type` of `0x04` is
//! Vibrato in MOD and a volume slide in S3M. Cross-format work needs
//! either the normalised [`Module`] or the one thing that *is*
//! format-neutral here — the sample PCM, exposed via [`RawPcm`].
//!
//! Written against the trait rather than one importer's type: this module is
//! compiled as soon as *any* of MOD / XM / S3M / IT is enabled, so naming a
//! concrete one (`ItModule::load(&bytes)`, the obvious way to reach it) would
//! be an example that does not compile in half the builds that have the module.
//!
//! ```
//! use xmrs::tracker::import::raw::RawModule;
//!
//! /// Sum every raw effect byte in a file — a fingerprint that is stable
//! /// across xmrs releases, because it never touches the normalised effects.
//! fn effect_fingerprint<M: RawModule>(m: &M) -> u64 {
//!     let mut sum = 0u64;
//!     for pat in m.raw_patterns() {
//!         for row in pat {
//!             for slot in row {
//!                 sum += slot.effect_type as u64 + slot.effect_parameter as u64;
//!             }
//!         }
//!     }
//!     sum
//! }
//! ```
//!
//! [`Module`]: crate::core::module::Module
//! [`TrackEffect`]: crate::core::effect::TrackEffect
// The `pub mod raw;` declaration in the parent carries its own doc comment, and
// rustdoc resolves this whole merged block in the *parent's* scope — where
// neither `PatternSlot` (its module is `pub(crate)`) nor `RawPcm` is nameable.
// Spelling the targets out absolutely is what keeps the links live.
//! [`PatternSlot::effect_type`]: crate::tracker::import::raw::PatternSlot::effect_type
//! [`PatternSlot::effect_parameter`]: crate::tracker::import::raw::PatternSlot::effect_parameter
//! [`RawPcm`]: crate::tracker::import::raw::RawPcm

use alloc::vec::Vec;
use core::time::Duration;

use crate::core::sample::SampleDataType;

/// The shared, importer-side pattern cell. Re-exported here because
/// the defining module stays `pub(crate)`: this is the one supported
/// path to it.
pub use crate::tracker::import::patternslot::PatternSlot;

/// Borrowed sample PCM, in whatever width and channel count the file
/// actually stored. The owning variant is
/// [`SampleDataType`]; this is its zero-copy counterpart, so walking
/// a corpus never clones a sample bank.
///
/// Unlike everything else in this module, PCM **is** comparable
/// across formats: the same 8-bit loop lifted from a MOD into an XM
/// is the same bytes in both.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RawPcm<'a> {
    /// The slot exists but carries no sample data (an empty
    /// instrument, or a header with no body).
    Empty,
    Mono8(&'a [i8]),
    Mono16(&'a [i16]),
    Stereo8(&'a [i8]),
    Stereo16(&'a [i16]),
    StereoFloat(&'a [f32]),
}

impl<'a> RawPcm<'a> {
    /// Length in **frames** (stereo variants count a left+right pair
    /// once), matching [`SampleDataType::len`].
    pub fn len(&self) -> usize {
        match self {
            RawPcm::Empty => 0,
            RawPcm::Mono8(v) => v.len(),
            RawPcm::Mono16(v) => v.len(),
            RawPcm::Stereo8(v) => v.len() / 2,
            RawPcm::Stereo16(v) => v.len() / 2,
            RawPcm::StereoFloat(v) => v.len() / 2,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Size of the payload in bytes, i.e. what a byte-oriented view
    /// of it would span.
    pub fn byte_len(&self) -> usize {
        match self {
            RawPcm::Empty => 0,
            RawPcm::Mono8(v) | RawPcm::Stereo8(v) => v.len(),
            RawPcm::Mono16(v) | RawPcm::Stereo16(v) => v.len() * 2,
            RawPcm::StereoFloat(v) => v.len() * 4,
        }
    }
}

// There is deliberately no `as_bytes()`: reinterpreting `&[i16]` as
// `&[u8]` needs `unsafe`, and this module is under
// `#![forbid(unsafe_code)]`. A consumer that wants to hash the
// payload should match on the variant and feed the typed slice to
// its hasher, or reinterpret it on its own side (`bytemuck::cast_
// slice`) where it — not xmrs — owns that decision. Note that any
// byte-level hash of a 16-bit or float variant is host-endian, and
// so not portable across architectures.

impl<'a> From<&'a SampleDataType> for RawPcm<'a> {
    fn from(d: &'a SampleDataType) -> Self {
        match d {
            SampleDataType::Mono8(v) => RawPcm::Mono8(v),
            SampleDataType::Mono16(v) => RawPcm::Mono16(v),
            SampleDataType::Stereo8(v) => RawPcm::Stereo8(v),
            SampleDataType::Stereo16(v) => RawPcm::Stereo16(v),
            SampleDataType::StereoFloat(v) => RawPcm::StereoFloat(v),
        }
    }
}

/// One sample slot as the file declared it.
///
/// Deliberately minimal. Volume, panning, finetune and relative
/// pitch are *not* here: each format encodes them on its own scale
/// (MOD's finetune nibble is a signed 4-bit index, XM's is `i8/127`,
/// IT's is a C5 frequency), so a single field would have to pick an
/// interpretation — which is exactly what this layer exists to
/// avoid. Read those from the normalised [`Sample`] instead.
///
/// [`Sample`]: crate::core::sample::Sample
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawSample<'a> {
    /// The name as stored, trimmed of padding. Often not a name at
    /// all: empty sample slots are the traditional place to leave
    /// greetings, credits and dates.
    pub name: &'a str,
    pub pcm: RawPcm<'a>,
    /// Loop start, **in frames**. Formats that store this in bytes
    /// (XM, 16-bit) or in words (MOD) are converted, because that is
    /// a unit, not an interpretation. The file's value is passed
    /// through otherwise — in particular the out-of-range clamping
    /// that `to_sample()` applies is *not* done here, so a nonsense
    /// loop stays visible as nonsense.
    pub loop_start: u32,
    /// Loop length, in frames. Same conversion rules as
    /// [`Self::loop_start`].
    pub loop_length: u32,
    /// Playback rate the file assigns to the sample's reference
    /// note, in Hz — S3M's `C2Spd` and IT's `C5Speed`. `None` on
    /// MOD and XM, which have no such field: they express pitch
    /// relative to an Amiga period table via a finetune nibble
    /// (MOD) or a relative-note plus finetune pair (XM).
    ///
    /// Two samples holding identical PCM at different rates are
    /// *not* the same sound, so this belongs in any identity
    /// comparison that crosses S3M/IT boundaries.
    pub sample_rate: Option<u32>,
}

/// One entry of IT's edit-history block.
///
/// Impulse Tracker 2.07+ and OpenMPT append one record per editing
/// session: when it started, and how long it lasted. Nothing else in
/// a tracker module is dated, which makes this the only intrinsic
/// evidence of *when* a file was worked on — and, between two files
/// sharing a history prefix, of which one came later.
///
/// Coverage is partial by nature: files written by older trackers
/// carry no block at all, and OpenMPT can be configured not to emit
/// one. An empty history means "not recorded", never "never edited".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawEditEntry {
    /// `(year, month, day)`, decoded from the FAT date word.
    pub date: (u16, u8, u8),
    /// `(hour, minute, second)`, decoded from the FAT time word.
    /// FAT stores seconds in units of two, so this is even.
    pub time: (u8, u8, u8),
    /// How long the session lasted, converted from MS-DOS ticks.
    pub run_time: Duration,
}

/// What the file says about the tool that wrote it.
///
/// The four formats disagree on how to answer that, so every field
/// is optional and the caller reads whichever its format populates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RawTracker<'a> {
    /// Free-form tag: XM's `tracker_name` (a 20-byte string such as
    /// `"FastTracker v2.00"`, which trackers routinely lie in), or
    /// MOD's four-character format tag (`M.K.`, `6CHN`, `FLT8`, …).
    /// `None` on IT and S3M, which identify their tool numerically.
    pub tag: Option<&'a str>,
    /// Version word as stored: XM's `version_number`, IT's
    /// `created_with_tracker`, S3M's `version`.
    pub version: Option<u16>,
    /// IT only: `compatible_with_tracker`, the oldest version that
    /// can still load the file. Together with [`Self::version`] it
    /// brackets the authoring tool more tightly than either alone.
    pub compatible_with: Option<u16>,
}

/// Read-only access to an importer's pre-DAW state.
///
/// Implemented by [`AmigaModule`], [`XmModule`], [`S3mModule`] and
/// [`ItModule`]. Not implemented by `SidModule` or `DwModule`: both
/// are synthesiser-native (register streams, not sample banks over a
/// pattern grid), and `DwModule` already exposes its fields
/// directly.
///
/// [`AmigaModule`]: crate::tracker::import::amiga::amiga_module::AmigaModule
/// [`XmModule`]: crate::tracker::import::xm::xmmodule::XmModule
/// [`S3mModule`]: crate::tracker::import::s3m::s3m_module::S3mModule
/// [`ItModule`]: crate::tracker::import::it::it_module::ItModule
pub trait RawModule {
    /// Song title as stored. May be empty.
    fn raw_title(&self) -> &str;

    /// Free-text song message (IT and S3M carry one; MOD and XM do
    /// not and return `""`). Line endings are normalised to `\n`.
    fn raw_message(&self) -> &str {
        ""
    }

    /// The order list, truncated to the song length the file
    /// declares — not the fixed-size on-disk array. May contain the
    /// format's separator and end markers (`0xFE` / `0xFF` in
    /// S3M/IT); they are passed through rather than filtered, since
    /// which values are markers is format knowledge.
    fn raw_orders(&self) -> &[u8];

    fn raw_pattern_count(&self) -> usize;

    /// One pattern as a `[row][channel]` grid. `None` past the end.
    ///
    /// Row counts vary per pattern (XM and IT both allow it) and
    /// empty patterns are legal, so neither dimension can be assumed
    /// constant across the return values.
    fn raw_pattern(&self, idx: usize) -> Option<&[Vec<PatternSlot>]>;

    fn raw_sample_count(&self) -> usize;

    /// One sample slot. `None` past the end; a slot that exists but
    /// holds no PCM yields [`RawPcm::Empty`], which is a different
    /// thing and worth keeping distinct.
    fn raw_sample(&self, idx: usize) -> Option<RawSample<'_>>;

    /// Number of *instrument* slots, which is not the sample count
    /// on formats where the two are distinct: IT and XM map notes to
    /// samples through an instrument layer, so one instrument can
    /// reference many samples. MOD and S3M have no such layer and
    /// default to the sample count.
    fn raw_instrument_count(&self) -> usize {
        self.raw_sample_count()
    }

    /// The instrument slot's own name, distinct from the names of
    /// the samples it references. Defaults to the sample name on
    /// formats without an instrument layer.
    fn raw_instrument_name(&self, idx: usize) -> Option<&str> {
        self.raw_sample(idx).map(|s| s.name)
    }

    /// Ticks per row at song start (the "speed" register, `Axx` in
    /// MOD/XM, `Axx` in S3M/IT). `None` on MOD, whose format has no
    /// field for it — ProTracker starts at 6 by convention and any
    /// other value arrives through an `Fxx` effect.
    fn raw_initial_speed(&self) -> Option<u8> {
        None
    }

    /// Beats per minute at song start. `None` on MOD for the same
    /// reason as [`Self::raw_initial_speed`]; the convention is 125.
    ///
    /// This is the *initial* value only. Any `Fxx`/`Txx` in the
    /// patterns moves it, so a module's tempo is a trajectory, not a
    /// number — treat this as a starting point, not a description.
    fn raw_initial_bpm(&self) -> Option<u8> {
        None
    }

    /// Number of channels. Defaults to the width of the first
    /// non-empty pattern, which is what the patterns actually use;
    /// formats that state it in their header override this.
    fn raw_channel_count(&self) -> usize {
        for idx in 0..self.raw_pattern_count() {
            if let Some(rows) = self.raw_pattern(idx) {
                if let Some(row) = rows.first() {
                    if !row.is_empty() {
                        return row.len();
                    }
                }
            }
        }
        0
    }

    /// What the file claims about its authoring tool.
    fn raw_tracker(&self) -> RawTracker<'_> {
        RawTracker::default()
    }

    /// Author-given name of one pattern. IT only (the `PNAM` chunk);
    /// `None` elsewhere, and `None` for an unnamed pattern.
    fn raw_pattern_name(&self, _idx: usize) -> Option<&str> {
        None
    }

    /// Author-given name of one channel. IT only (the `CNAM` chunk);
    /// `None` elsewhere. Frequently used for section labels and
    /// credits rather than for instrument roles.
    fn raw_channel_name(&self, _idx: usize) -> Option<&str> {
        None
    }

    /// IT's edit history, oldest first. Empty on every other format,
    /// and on IT files that carry no such block — see
    /// [`RawEditEntry`] for why an empty result is not evidence of
    /// anything.
    fn raw_edit_history(&self) -> Vec<RawEditEntry> {
        Vec::new()
    }

    /// Iterator over every pattern grid.
    fn raw_patterns(&self) -> RawPatterns<'_, Self>
    where
        Self: Sized,
    {
        RawPatterns {
            module: self,
            next: 0,
        }
    }

    /// Iterator over every sample slot.
    fn raw_samples(&self) -> RawSamples<'_, Self>
    where
        Self: Sized,
    {
        RawSamples {
            module: self,
            next: 0,
        }
    }
}

/// Iterator returned by [`RawModule::raw_patterns`].
pub struct RawPatterns<'a, M: RawModule> {
    module: &'a M,
    next: usize,
}

impl<'a, M: RawModule> Iterator for RawPatterns<'a, M> {
    type Item = &'a [Vec<PatternSlot>];

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.module.raw_pattern(self.next)?;
        self.next += 1;
        Some(item)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.module.raw_pattern_count().saturating_sub(self.next);
        (n, Some(n))
    }
}

/// Iterator returned by [`RawModule::raw_samples`].
pub struct RawSamples<'a, M: RawModule> {
    module: &'a M,
    next: usize,
}

impl<'a, M: RawModule> Iterator for RawSamples<'a, M> {
    type Item = RawSample<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.module.raw_sample(self.next)?;
        self.next += 1;
        Some(item)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.module.raw_sample_count().saturating_sub(self.next);
        (n, Some(n))
    }
}
