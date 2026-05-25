//! `ModuleFormat` — the tag identifying which tracker authored a
//! module. Metadata only: no runtime branching reads it. Playback
//! decisions go through [`crate::core::compatibility::PlaybackQuirks`].

use serde::{Deserialize, Serialize};

/// Which tracker convention this module was imported from.
///
/// **Metadata only**. Runtime playback decisions are driven entirely
/// by [`crate::core::compatibility::PlaybackQuirks`] on
/// [`crate::core::module::Module`], which an editor can set
/// independently of the format tag.
///
/// Modules constructed from scratch leave `Module::origin` at `None`.
/// Importers populate it with the matching variant after a successful
/// load.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModuleFormat {
    /// Unknown / hand-constructed / non-tracker source.
    #[default]
    Unknown,
    /// ProTracker-style MOD (Amiga Soundtracker family).
    Mod,
    /// ScreamTracker III (S3M).
    S3m,
    /// Fasttracker II (XM).
    Xm,
    /// Impulse Tracker (IT), including OpenMPT's IT extensions.
    It,
    /// MOS6581 SID (Commodore 64).
    Sid,
}

impl ModuleFormat {
    /// `true` when this tag is `Xm`.
    #[inline]
    pub fn is_xm(&self) -> bool {
        matches!(self, ModuleFormat::Xm)
    }

    /// `true` when this tag is `S3m`.
    #[inline]
    pub fn is_s3m(&self) -> bool {
        matches!(self, ModuleFormat::S3m)
    }

    /// `true` when this tag is `It`.
    #[inline]
    pub fn is_it(&self) -> bool {
        matches!(self, ModuleFormat::It)
    }

    /// `true` when this tag is `Mod`.
    #[inline]
    pub fn is_mod(&self) -> bool {
        matches!(self, ModuleFormat::Mod)
    }
}
