//! DAW layer for `Module`: reusable tracks, their placements on a
//! timeline (clips), continuous parameter curves (automation), and
//! the linearised playback map.
//!
//! Importers build this layer through
//! [`crate::daw::build_timeline::build_timeline_layer`], which orchestrates:
//!
//! 1. Linearise the source `pattern_order` into a `TimelineMap`
//!    of absolute ticks.
//! 2. Split each channel by instrument changes; emit one Clip per
//!    `(order entry × segment)`.
//! 3. Fuse Tracks whose `(instrument, rows)` match bit-for-bit.
//! 4. Detect bit-strict Bjorklund patterns and rewrite them
//!    losslessly as `InstrumentType::Euclidean` wrappers
//!    ([`crate::daw::euclidean`]).

pub mod automation;
pub mod build_timeline;
pub mod clip;
pub mod euclidean;
pub mod sorted_clips;
pub mod timeline;
pub mod track;
