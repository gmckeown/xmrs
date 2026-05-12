//! `CompatibilityProfile` — a named bundle of format-specific
//! playback rules.
//!
//! A module's playback isn't decided by `format` alone; it's decided
//! by the combination of:
//!
//! - which historical tracker authored it (FT2, IT 2.14, IT 2.15,
//!   ST3, ProTracker, modern editors like Schism / OpenMPT),
//! - the per-file flags that override sub-quirks (IT's "Old Effects"
//!   bit, S3M's amigalimits masterflag, etc.).
//!
//! Before `CompatibilityProfile`, callers had to reach into
//! `Module.format` and `Module.quirks` separately, and importers
//! built up `PlaybackQuirks { ..default() }` block-by-block,
//! knowing which knobs each format flips. That works but lets the
//! two structures drift: nothing prevents a `format = Xm` Module
//! shipping without `ft2_arpeggio_lut`, even though the combination
//! is meaningless.
//!
//! `CompatibilityProfile` consolidates `format` + `quirks` behind
//! named constructors. Each constructor is the *canonical*
//! configuration for one tracker generation — everything an
//! importer needs to materialise the right playback flags from a
//! single call:
//!
//! ```
//! use xmrs::compatibility_profile::CompatibilityProfile;
//! let it214 = CompatibilityProfile::it214();
//! let ft2   = CompatibilityProfile::ft2();
//! ```
//!
//! Importers pick the constructor that matches the source file,
//! then optionally tweak the per-file quirks (IT's `is_old_effects`,
//! S3M's amigalimits) on the returned profile. Editor-authored
//! modules use [`CompatibilityProfile::modern`](crate::compatibility_profile::CompatibilityProfile::modern) — clean, no historical bug
//! emulation.

use serde::{Deserialize, Serialize};

use crate::module::{ModuleFormat, PanResetPolicy, PlaybackQuirks};

/// Bundles a module's source format with the playback quirks that
/// reproduce that format's authoring tracker. Use the named
/// constructors instead of building this directly — they're the
/// only configurations the player has been validated against.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct CompatibilityProfile {
    /// What kind of file this came from. Informational; no playback
    /// branching reads it. Use the `quirks` field for behavioural
    /// decisions.
    pub format: ModuleFormat,

    /// Per-format playback rules. See [`PlaybackQuirks`] for the
    /// individual flags; the constructors on
    /// [`CompatibilityProfile`] set them in coherent groups.
    pub quirks: PlaybackQuirks,
}

impl Default for CompatibilityProfile {
    /// `modern()` — the safest default for editor-authored content
    /// where no historical bugs should be reproduced.
    fn default() -> Self {
        Self::modern()
    }
}

impl CompatibilityProfile {
    /// FastTracker 2 (XM). Reproduces FT2's signature behaviours —
    /// the arpeggio LUT, the K00 quirk, the E60 pattern-loop bug,
    /// vol-column `Bx` advancing the vibrato LFO, the
    /// portamento-down signed-overflow, and the cut-without-envelope
    /// keyoff. Mirrors the `ft2_replayer.c` reference. Authored
    /// XM modules generally rely on at least some of these.
    pub fn ft2() -> Self {
        Self {
            format: ModuleFormat::Xm,
            quirks: PlaybackQuirks {
                ft2_pitch_slide_overflow: true,
                ft2_arpeggio_lut: true,
                ft2_arpeggio_note_clamp: true,
                keyoff_cuts_without_vol_env: true,
                k00_eats_note: true,
                volcol_b_advances_vibrato: true,
                e60_leaks_to_next_pattern: true,
                pan_reset_policy: PanResetPolicy::Always,
                // FT2 itself sets `song.speed = 0` on `F00` and
                // then freezes (no song-end concept). Every
                // modern XM player (OpenMPT, MilkyTracker,
                // XMPlay, Schism) interprets `F00` as
                // end-of-song instead, and the scene composers
                // who actually used `F00` did so *with that
                // intent* — the song was finished. We follow
                // the modern convention here over strict-FT2
                // freeze, because the strict behaviour is
                // useless to a listener.
                speed_zero_ends_song: true,
                ..PlaybackQuirks::default()
            },
        }
    }

    /// Impulse Tracker 2.14. The canonical IT compatibility level
    /// for "modern" IT modules: tremor state persists across rows,
    /// no FT2-style arpeggio LUT, no E60 leak. The two header-
    /// driven flags (`it_old_effects`, `it_link_gxx_memory`) are
    /// `false` here — the importer reads them off the `IT` header
    /// flags byte and overlays them on top of this profile.
    pub fn it214() -> Self {
        Self {
            format: ModuleFormat::It,
            quirks: PlaybackQuirks {
                tremor_state_persists: true,
                pan_reset_policy: PanResetPolicy::OnInstrumentChange,
                it_vibrato_ticks_at_row_zero: true,
                sample_volume_is_static_gain: true,
                ..PlaybackQuirks::default()
            },
        }
    }

    /// Impulse Tracker 2.15 / OpenMPT IT extensions. Identical to
    /// `it214` for the flags currently modelled — kept as a
    /// distinct constructor so callers can express intent and so
    /// future IT 2.15-only quirks have a place to land without
    /// silently changing `it214`'s meaning.
    pub fn it215() -> Self {
        Self::it214()
    }

    /// ScreamTracker 3 (S3M). Period-clamp depends on the file's
    /// amigalimits masterflag — the importer overlays
    /// `quirks.period_clamp = Some(...)` on top of this profile
    /// when the flag is set. The other three knobs are always on
    /// for ST3.
    pub fn st3() -> Self {
        Self {
            format: ModuleFormat::S3m,
            quirks: PlaybackQuirks {
                allow_zero_period: true,
                pattern_loop_resumes: true,
                tremor_state_persists: true,
                pan_reset_policy: PanResetPolicy::Never,
                ..PlaybackQuirks::default()
            },
        }
    }

    /// ProTracker (Amiga MOD). The MOD format predates almost every
    /// quirk we model: no tremor, no IT old-effects, no FT2 bugs.
    /// Equivalent to `modern()` for now, but kept distinct so any
    /// future ProTracker-specific quirk (e.g. period clamping
    /// matching ProTracker's `113..856` range) has a single named
    /// home.
    pub fn pt() -> Self {
        Self {
            format: ModuleFormat::Mod,
            quirks: PlaybackQuirks {
                // F00 = "stop song" is canonical MOD/ProTracker
                // behaviour. Every other knob stays at its
                // default value.
                speed_zero_ends_song: true,
                ..PlaybackQuirks::default()
            },
        }
    }

    /// Modern editor-authored content (Schism / OpenMPT new files,
    /// hand-built Modules). All quirks off; clean playback semantics.
    /// Use this when you're constructing a Module programmatically
    /// and don't want any historical bug emulation.
    pub fn modern() -> Self {
        Self {
            format: ModuleFormat::Unknown,
            quirks: PlaybackQuirks::default(),
        }
    }
}
