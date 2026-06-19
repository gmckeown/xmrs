//! DAW layer for `Module`: reusable tracks, their placements on a
//! timeline (clips), continuous parameter curves (automation), and
//! the linearised playback map.
//!
//! The build pipeline (pattern grid → tracks/clips/timeline_map +
//! automation lanes) lives under [`crate::tracker::import::build`] and
//! [`crate::tracker::import::extract`] — it's the import-side step, not part
//! of the core data model.

pub mod automation;
pub mod clip;
pub mod device;
pub mod dsp;
pub mod euclidean;
pub mod loop_region;
pub mod sorted_clips;
pub mod timeline;
pub mod track;
