//! Build the DAW timeline layer (`tracks`, `clips`, `timeline_map`)
//! from `pattern_order` + `pattern` inputs supplied by importers.
//!
//! Pipeline:
//! 1. `build_timeline_map` linearises `pattern_order` to absolute
//!    ticks via the internal `timeline_map` walker.
//! 2. `extract_tracks_and_clips` splits each channel by instrument
//!    changes and emits one Clip per `(order entry × segment)`
//!    (private `segments` sub-module).
//! 3. `dedupe_tracks_by_content` fuses Tracks whose
//!    `(instrument, rows)` match bit-for-bit (private `dedup`
//!    sub-module).
//! 4. [`crate::core::daw::euclidean::auto_convert_strict_euclidean_tracks`]
//!    rewrites tracks whose cells form a strict Bjorklund pattern
//!    in place as a `Track::Euclidean` variant (lossless).
//!
//! `split_rows_by_instrument` is also exposed for track-native
//! importers (SID) that don't go through a pattern grid.
//!
//! The historical "restart byte" (Soundtracker MOD, FastTracker
//! XM) is realised by `set_song_loop_to_restart_byte`, which sets
//! `Module::song_loop_to` to the absolute tick the player should
//! wrap to at end-of-song. DAW migration Phase 3c.2 retired the
//! previous cell-side `NavigationEffect::PositionJump` injection:
//! the TimelineMap-driven sequencer drives the wrap entirely from
//! `song_loop_to`.

use alloc::vec::Vec;

use crate::core::module::Module;
use crate::tracker::import::unit::TrackImportUnit;

mod dedup;
mod segments;
mod timeline_map;

pub use dedup::dedupe_tracks_by_content;
pub use segments::{extract_tracks_and_clips, split_rows_by_instrument, TrackSegment};
pub use timeline_map::build_timeline_map;

/// A row of cells, one per channel, as emitted by the importers
/// before the DAW layer is built. Carries [`TrackImportUnit`]s
/// rather than the runtime [`crate::core::cell::Cell`] type so the
/// per-channel split-by-instrument pass can still see each cell's
/// explicit `instrument: Option<usize>` (the runtime `Cell`'s
/// [`crate::core::cell::CellEvent`] no longer surfaces the index — that
/// information moves onto the owning Track).
pub type Row = Vec<TrackImportUnit>;

/// A pattern as the importers see it: a vector of [`Row`]s.
/// Transient, never stored on [`Module`].
pub type Pattern = Vec<Row>;

/// Sentinel used as [`TrackSegment::instrument`] (and therefore
/// [`crate::core::daw::track::Track::instrument`]) for an "effect-only"
/// segment — a stretch of cells that carry meaningful effects
/// (vibrato, portamento, …) but no explicit instrument trigger of
/// their own. These effects are intended to modify a voice that's
/// still playing from a previous pattern, so the player processes
/// the cell's effects without performing an instrument load.
pub const EFFECT_ONLY_INSTRUMENT: usize = usize::MAX;

/// Pad `module.instrument` with empty slots so every Track's instrument
/// index is in range. Some real modules reference instrument slots beyond
/// the declared count (e.g. `lamb_-_among_the_stars.xm` cites instrument
/// 30 with 2 declared; `strobe-paralysicical13.xm` cites 17 with 17). FT2
/// keeps all 128 XM instrument slots allocated — an empty slot triggers
/// the note but has no sample, so it plays silence — whereas importers
/// trim to the declared count. Without this the out-of-range index trips
/// `verify_layers_consistent` (`TrackInstrumentMismatch`) and would index
/// out of bounds at playback. Default (empty) slots reproduce FT2's
/// "reference an empty instrument → no sound". Format-agnostic.
fn pad_instruments_to_track_refs(module: &mut Module) {
    let max_ref = module
        .tracks
        .iter()
        .map(|t| t.instrument())
        .filter(|&i| i != EFFECT_ONLY_INSTRUMENT)
        .max();
    if let Some(max_ref) = max_ref {
        if max_ref >= module.instrument.len() {
            module
                .instrument
                .resize_with(max_ref + 1, crate::core::instrument::Instrument::default);
        }
    }
}

/// Set [`Module::song_loop_to`] from the `restart_byte` (typically
/// the XM `restart_position` or MOD restart byte) by looking up the
/// absolute tick of `(restart_byte, row=0, loop_iter=0)` in the
/// already-built [`Module::timeline_map`]. The TimelineMap-driven
/// sequencer (DAW migration Phase 3c.2) then wraps to that tick
/// at end-of-song instead of falling back to order 0.
///
/// Defensive interpretation of `restart_byte`:
///
/// * `song_loop_to` already populated — no-op. An in-pattern `Bxx` /
///   `Dxx` jump that points back into walked rows takes precedence
///   over the header byte: actual playback follows the in-row
///   effect first and never observes the natural end-of-timeline
///   where the restart byte would kick in. `build_timeline_layer`
///   sets this from the walker, so this helper just respects what
///   was decided there.
/// * `0` — no-op. The default wrap-to-order-0 already does this.
/// * out-of-range — no-op. Covers ProTracker's `0x7F` sentinel
///   and any other byte whose order has no `(row=0, loop_iter=0)`
///   entry in the timeline_map.
///
/// Must be called **after** [`build_timeline_layer`] so the
/// timeline_map is populated.
pub fn set_song_loop_to_restart_byte(module: &mut Module, song: u16, restart_byte: usize) {
    if module.song_loop_to.is_some() {
        return;
    }
    if restart_byte == 0 {
        return;
    }
    let entry = module
        .timeline_map
        .entries
        .iter()
        .find(|e| {
            e.song == song
                && e.order_idx as usize == restart_byte
                && e.row_idx == 0
                && e.loop_iter == 0
        })
        .copied();
    if let Some(e) = entry {
        module.song_loop_to = Some(e.tick);
    }
}

/// Build (or rebuild) the DAW timeline layer of `module` from its
/// (synthetic) `pattern_order` + `pattern` inputs. Idempotent.
///
/// The Module struct itself does not carry a pattern grid;
/// importers build it locally and pass it here. `automation` is
/// left untouched — populated only via the [`crate::edit`] API.
///
/// The 4th pass
/// ([`crate::core::daw::euclidean::auto_convert_strict_euclidean_tracks`])
/// runs after dedup: any [`Track::Notes`] whose cells form a strict
/// Bjorklund pattern is rewritten lossless to a [`Track::Euclidean`].
///
/// [`Track::Notes`]: crate::core::daw::track::Track::Notes
/// [`Track::Euclidean`]: crate::core::daw::track::Track::Euclidean
pub fn build_timeline_layer(
    module: &mut Module,
    pattern_order: &[Vec<usize>],
    pattern: &[Pattern],
) {
    // The walker, in addition to producing the linearised timeline,
    // tells us the tick of the row that the song's first explicit
    // `Bxx` / `Dxx` jump re-entered (if any). That tick is the
    // natural end-of-song wrap target — it matches what every other
    // tracker (PT/FT2/OpenMPT/Schism) would observe at runtime when
    // playback hits the jump. We forward it to `song_loop_to` so the
    // sequencer wraps to that row instead of restarting at order 0.
    //
    // The restart byte (XM `restart_position` / MOD restart byte) is
    // applied separately by the importer via
    // `set_song_loop_to_restart_byte`; that helper now defers to a
    // walker-detected wrap (Bxx in the song body takes precedence
    // over the header byte — playback never reaches the natural
    // end-of-timeline when an in-pattern jump fires first).
    let (timeline_map, walker_wrap_tick) = build_timeline_map(module, pattern_order, pattern);
    module.timeline_map = timeline_map;
    if let Some(tick) = walker_wrap_tick {
        module.song_loop_to = Some(tick);
    }
    let (tracks, clips) = extract_tracks_and_clips(pattern, &module.timeline_map);
    module.tracks = tracks;
    module.clips = crate::core::daw::sorted_clips::SortedClips::from_unsorted(clips);
    dedupe_tracks_by_content(module);
    crate::core::daw::euclidean::auto_convert_strict_euclidean_tracks(module);
    pad_instruments_to_track_refs(module);

    // DAW migration Phase 3c.3 — derive AutomationLanes from
    // the importer-side `TrackImportUnit`s, fully decoupled from
    // the runtime `Cell`:
    //
    // * Per-Track LFO/Slide/Glide lanes via
    //   `extract_per_track_lanes_from_patterns` (reads
    //   `TIU.effects`, computes `to_track_effects` per row, runs the
    //   per-class extractors).
    // * Song-level Bpm/Speed/GlobalVolume + slides via
    //   `extract_song_lanes_from_patterns` (reads
    //   `TIU.song_level`).
    //
    // MidiMacro relocation from `TIU.midi_macros` to
    // `Cell.effects::TrackEffect::MidiMacro` is folded into
    // `TrackImportUnit::prepare_cell`.
    let mut lanes = crate::tracker::import::extract::extract_per_track_lanes_from_patterns(
        pattern,
        &module.timeline_map,
        &module.clips,
        &module.quirks,
    );
    lanes.extend(
        crate::tracker::import::extract::extract_song_lanes_from_patterns(
            pattern,
            &module.timeline_map,
        ),
    );
    module.automation.extend(lanes);
}

#[cfg(test)]
mod restart_tests {
    use super::*;
    use alloc::vec;

    fn empty_pattern(channels: usize, rows: usize) -> Pattern {
        (0..rows)
            .map(|_| (0..channels).map(|_| TrackImportUnit::default()).collect())
            .collect()
    }

    /// `set_song_loop_to_restart_byte` resolves the restart byte
    /// to the absolute tick of `(order=restart_byte, row=0)` in the
    /// freshly-built timeline_map.
    #[test]
    fn song_loop_to_matches_restart_byte_tick() {
        use crate::core::module::Module;

        let order = vec![0, 1, 2];
        let patterns = vec![empty_pattern(1, 4); 3];
        let mut module = Module::default();
        build_timeline_layer(&mut module, &[order], &patterns);
        set_song_loop_to_restart_byte(&mut module, 0, 1);
        // Tick of order 1 row 0 = 4 rows × default_tempo (6 ticks/row)
        // = 24.
        assert_eq!(module.song_loop_to, Some(24));
    }

    #[test]
    fn song_loop_to_noop_for_restart_zero() {
        use crate::core::module::Module;

        let order = vec![0, 1, 2];
        let patterns = vec![empty_pattern(1, 4); 3];
        let mut module = Module::default();
        build_timeline_layer(&mut module, &[order], &patterns);
        set_song_loop_to_restart_byte(&mut module, 0, 0);
        assert_eq!(module.song_loop_to, None);
    }

    #[test]
    fn song_loop_to_noop_for_out_of_range_restart_byte() {
        use crate::core::module::Module;

        let order = vec![0, 1, 2];
        let patterns = vec![empty_pattern(1, 4); 3];
        let mut module = Module::default();
        build_timeline_layer(&mut module, &[order], &patterns);
        // ProTracker's 0x7F sentinel is well past the order length.
        set_song_loop_to_restart_byte(&mut module, 0, 0x7F);
        assert_eq!(module.song_loop_to, None);
    }

    /// DAW migration Phase 3b — verify the integration:
    /// `build_timeline_layer` populates `module.automation` with
    /// lanes mirroring the cell-side continuous effects.
    #[test]
    fn build_timeline_layer_extracts_lanes_in_mirror() {
        use crate::core::cell_note::CellNote;
        use crate::core::daw::automation::{AutomationTarget, LaneKind};
        use crate::core::effect::SongLevelEffect;
        use crate::core::fixed::fixed::Q8_8;
        use crate::core::fixed::units::{PitchDelta, Volume};
        use crate::core::module::Module;
        use crate::core::pitch::Pitch;
        use crate::tracker::import::effect::TrackImportEffect;
        use crate::tracker::import::unit::TrackImportUnit;

        let mut row0 = TrackImportUnit::default();
        row0.note = CellNote::Play(Pitch::C5);
        row0.instrument = Some(0);
        row0.velocity = Volume::FULL;
        row0.effects = alloc::vec![TrackImportEffect::Vibrato(
            Q8_8::from_int(3),
            PitchDelta::from_q8_8_i16(6)
        )];
        row0.song_level = alloc::vec![SongLevelEffect::Bpm(140)];

        let pat: Pattern = (0..4)
            .map(|r| {
                if r == 0 {
                    alloc::vec![row0.clone()]
                } else {
                    alloc::vec![TrackImportUnit::default()]
                }
            })
            .collect();

        let mut module = Module::default();
        module
            .instrument
            .push(crate::core::instrument::Instrument::default());
        build_timeline_layer(&mut module, &[vec![0]], &[pat]);

        let has_vibrato_lane = module.automation.iter().any(|l| {
            matches!(l.target, AutomationTarget::TrackPitch(_))
                && matches!(l.kind, LaneKind::Lfo { .. })
        });
        let has_bpm_points = module
            .automation
            .iter()
            .any(|l| l.target == AutomationTarget::Bpm && matches!(l.kind, LaneKind::Points(_)));
        assert!(
            has_vibrato_lane,
            "expected an Lfo lane on TrackPitch after extraction"
        );
        assert!(
            has_bpm_points,
            "expected a Points lane on Bpm after extraction"
        );
    }

    /// A cell that cites an instrument index past the declared count
    /// (real in e.g. `lamb_-_among_the_stars.xm`) pads the instrument
    /// vector with empty slots so the Track reference is in range and
    /// the layer stays consistent (FT2 empty-slot semantics).
    #[test]
    fn out_of_range_instrument_reference_pads_instrument_vec() {
        use crate::core::cell_note::CellNote;
        use crate::core::module::Module;
        use crate::core::pitch::Pitch;
        use crate::tracker::import::unit::TrackImportUnit;

        let mut module = Module::default();
        module
            .instrument
            .push(crate::core::instrument::Instrument::default()); // only index 0 exists

        let mut cell = TrackImportUnit::default();
        cell.note = CellNote::Play(Pitch::C5);
        cell.instrument = Some(5); // out of range

        let pat: Pattern = alloc::vec![alloc::vec![cell]];
        build_timeline_layer(&mut module, &[alloc::vec![0]], &[pat]);

        assert!(
            module.instrument.len() >= 6,
            "instrument vec padded to cover the out-of-range index 5"
        );
        assert!(module.verify_layers_consistent().is_ok());
    }
}
