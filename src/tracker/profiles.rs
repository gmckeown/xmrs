//! Preset [`PlaybackQuirks`] for the historical trackers we model.
//! Each constructor mirrors the authoring tracker's signature
//! behaviours.
//!
//! There is no `modern()` constructor — for an editor-authored module
//! with no historical bug emulation, use
//! [`PlaybackQuirks::default()`](crate::core::compatibility::PlaybackQuirks::default).

use crate::core::compatibility::{PanResetPolicy, PlaybackQuirks};

/// FastTracker 2 (XM). Reproduces FT2's signature behaviours: the
/// arpeggio LUT, the K00 quirk, the E60 pattern-loop bug, vol-column
/// `Bx` advancing the vibrato LFO, the portamento-down signed-
/// overflow, and the cut-without-envelope keyoff. Mirrors the
/// `ft2_replayer.c` reference.
pub fn ft2() -> PlaybackQuirks {
    PlaybackQuirks {
        ft2_pitch_slide_overflow: true,
        ft2_arpeggio_lut: true,
        ft2_arpeggio_note_clamp: true,
        keyoff_cuts_without_vol_env: true,
        k00_eats_note: true,
        volcol_b_advances_vibrato: true,
        e60_leaks_to_next_pattern: true,
        pan_reset_policy: PanResetPolicy::Always,
        // FT2 itself sets `song.speed = 0` on `F00` and then freezes
        // (no song-end concept). Every modern XM player (OpenMPT,
        // MilkyTracker, XMPlay, Schism) interprets `F00` as
        // end-of-song instead, matching scene-era composer intent.
        speed_zero_ends_song: true,
        ..PlaybackQuirks::default()
    }
}

/// Impulse Tracker 2.14. Tremor state persists across rows, no
/// FT2-style arpeggio LUT, no E60 leak. The two header-driven flags
/// (`it_old_effects`, `it_link_gxx_memory`) start `false` here — the
/// importer overlays them from the IT header flags byte.
pub fn it214() -> PlaybackQuirks {
    PlaybackQuirks {
        tremor_state_persists: true,
        pan_reset_policy: PanResetPolicy::OnInstrumentChange,
        it_vibrato_ticks_at_row_zero: true,
        sample_volume_is_static_gain: true,
        // IT's `fx_pattern_loop` re-anchors to `row + 1` when the
        // counter expires and suppresses a same-row pattern-break
        // while any loop is still iterating (schism `effects.c`).
        pattern_loop_resumes: true,
        active_loop_suppresses_break: true,
        ..PlaybackQuirks::default()
    }
}

/// Impulse Tracker 2.15 / OpenMPT IT extensions. Identical to
/// [`it214`] for the flags currently modelled — kept as a distinct
/// constructor so callers can express intent and future IT 2.15-only
/// quirks have a place to land without silently changing `it214`'s
/// meaning.
pub fn it215() -> PlaybackQuirks {
    it214()
}

/// ScreamTracker 3 (S3M). The period clamp from the file's
/// amigalimits masterflag is layered on top — the importer overlays
/// `quirks.period_clamp = Some(...)` after calling [`st3`].
pub fn st3() -> PlaybackQuirks {
    PlaybackQuirks {
        allow_zero_period: true,
        pattern_loop_resumes: true,
        tremor_state_persists: true,
        pan_reset_policy: PanResetPolicy::Never,
        ..PlaybackQuirks::default()
    }
}

/// ProTracker (Amiga MOD). The MOD format predates almost every
/// quirk we model. `F00 = stop song` is the one canonical ProTracker
/// behaviour we preserve via [`PlaybackQuirks::speed_zero_ends_song`].
pub fn pt() -> PlaybackQuirks {
    PlaybackQuirks {
        speed_zero_ends_song: true,
        // PT clears the shared break position in its loop branch, so a
        // same-row loop-jump + break lands at row 0, not the break
        // target (`pt2_replayer.c:1429`).
        pattern_loop_break_resets_row: true,
        // Amiga channels are hard-panned by the hardware in a fixed
        // L-R-R-L layout (set by the importer's `channel_defaults`).
        // That pan is a property of the channel, not the sample, so a
        // note retrigger must NOT reset it to the sample's centre pan —
        // mirror S3M, whose Amiga-derived model is `Never`.
        pan_reset_policy: PanResetPolicy::Never,
        ..PlaybackQuirks::default()
    }
}
