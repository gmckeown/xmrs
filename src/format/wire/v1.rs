//! Schema **v1** — the first frozen wire layer. Every type here is a v1
//! payload; once this ships it is never edited (a later schema redefines only
//! what changed and re-exports the rest — §6.4).
//!
//! ## The chunk-id registry
//!
//! The full §3 catalogue is declared here as [`ChunkId`] constants and
//! gathered in [`KNOWN`], even for chunks whose wire types are not yet
//! written — the registry is what
//! [`Container::require_known`](super::super::container::Container::require_known)
//! consults to refuse an unknown *critical* chunk (§2.1), so it must be
//! complete.
//!
//! ## The leaf boundary
//!
//! A v1 wire struct mirrors the model's **structure** explicitly — the field
//! set, `usize → u32`, no `#[cfg]`, `mix_plugins` dropped (§3.1). For *stable
//! primitive leaves* ([`Volume`], [`FrequencyType`], [`PatternHighlight`],
//! [`ChannelDefault`], …) it rides the model's own `Serialize` rather than
//! cloning a twin: these are `Q`-format newtypes and small enums that do not
//! churn, and CBOR makes an additive change to them backward-compatible. A
//! *structural* change to such a leaf (a moved field, a changed unit) is
//! exactly what triggers a `schema_version` bump + migration (§6.2), so the
//! freeze guarantee is not weakened — it is enforced at the version boundary
//! instead of duplicated per leaf.

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::core::fixed::units::Volume;
use crate::core::module::{ChannelDefault, Module, PatternHighlight};
use crate::tracker::period::FrequencyType;

use super::super::ChunkId;

// ---- §3 chunk-id registry -------------------------------------------------

/// Song header metadata (scalars + channel tables).
pub const MHDR: ChunkId = ChunkId::new(b"MHDR");
/// `PlaybackQuirks` — the only field playback reads for bug emulation.
pub const QRKS: ChunkId = ChunkId::new(b"QRKS");
/// Instrument bank (sample *headers*; PCM by reference into `PCM `).
pub const INST: ChunkId = ChunkId::new(b"INST");
/// `Vec<Track>` — the note/euclidean/audio tracks.
pub const TRKS: ChunkId = ChunkId::new(b"TRKS");
/// Clip placements on the timeline.
pub const CLIP: ChunkId = ChunkId::new(b"CLIP");
/// Automation lanes.
pub const AUTO: ChunkId = ChunkId::new(b"AUTO");
/// Linearised timeline map.
pub const TMAP: ChunkId = ChunkId::new(b"TMAP");
/// Loop regions (`song_loop_to`, `channel_loops`).
pub const LOOP: ChunkId = ChunkId::new(b"LOOP");
/// Signal graph (inserts, buses, sends, master chain).
pub const GRAF: ChunkId = ChunkId::new(b"GRAF");
/// MIDI macros.
pub const MIDI: ChunkId = ChunkId::new(b"MIDI");
/// Asset directory for the blob region.
pub const ASET: ChunkId = ChunkId::new(b"ASET");
/// Raw PCM blob region (never CBOR — §4).
pub const PCM: ChunkId = ChunkId::new(b"PCM ");
/// Source-format provenance (ancillary).
pub const ORGN: ChunkId = ChunkId::new(b"orgn");
/// Writer provenance — crate name + semver (ancillary, §6).
pub const GENR: ChunkId = ChunkId::new(b"genr");
/// Edit history (ancillary, §8).
pub const HSTC: ChunkId = ChunkId::new(b"hstc");

/// Every chunk id this build can **process** (§2.1): a *critical* chunk absent
/// from this list makes the reader refuse the file; an *ancillary* one is
/// skipped and reported. This is the handled set, so it grows as chunks are
/// implemented.
pub const KNOWN: &[ChunkId] = &[
    MHDR, QRKS, INST, TRKS, CLIP, AUTO, TMAP, LOOP, GRAF, MIDI, ASET, PCM, ORGN, GENR, HSTC,
];

// ---- min_reader_version (§6.1) --------------------------------------------

/// The oldest reader that can interpret `module`, computed from its content
/// (§6.1). Every v1 feature was introduced at schema 1, so a pure-v1 file is
/// always `1`. As later schemas add features, this consults an introduced-in
/// table over the module's actual content and returns the max — a plain XM
/// import with no post-v1 feature keeps writing `1`, so a v1 reader can still
/// open it.
pub fn min_reader_version(_module: &Module) -> u32 {
    1
}

// ---- genr: writer provenance (§6) -----------------------------------------

/// Writer provenance — the `cwtv` half of IT's version pair (§6): *which xmrs
/// wrote this file*. Regenerated on every write and **never read to make a
/// compat decision**; a reader gates only on the header's version fields.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    /// Writing crate name (`env!("CARGO_PKG_NAME")`).
    pub writer_name: String,
    /// Writing crate semver (`env!("CARGO_PKG_VERSION")`).
    pub writer_version: String,
}

impl Provenance {
    /// The provenance for a file written by *this* build.
    pub fn current() -> Self {
        Self {
            writer_name: env!("CARGO_PKG_NAME").into(),
            writer_version: env!("CARGO_PKG_VERSION").into(),
        }
    }
}

// ---- MHDR: song header ----------------------------------------------------

/// The `MHDR` payload (§3): song-level scalars and the per-channel tables.
/// `default_tempo` / `default_bpm` are `usize` in the model and pinned to
/// `u32` here so the file does not differ across 32/64-bit hosts (§5.1).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct MHdr {
    pub name: String,
    pub comment: String,
    pub frequency_type: FrequencyType,
    pub default_tempo: u32,
    pub default_bpm: u32,
    pub pattern_highlight: PatternHighlight,
    pub pitch_wheel_depth: u8,
    pub mix_volume: Volume,
    pub channel_names: Vec<String>,
    pub channel_defaults: Vec<ChannelDefault>,
}

impl MHdr {
    /// Project the header fields out of a live `Module`.
    pub fn from_module(m: &Module) -> Self {
        Self {
            name: m.name.clone(),
            comment: m.comment.clone(),
            frequency_type: m.frequency_type,
            default_tempo: m.default_tempo as u32,
            default_bpm: m.default_bpm as u32,
            pattern_highlight: m.pattern_highlight,
            pitch_wheel_depth: m.pitch_wheel_depth,
            mix_volume: m.mix_volume,
            channel_names: m.channel_names.clone(),
            channel_defaults: m.channel_defaults.clone(),
        }
    }

    /// Write the header fields back onto a `Module`.
    pub fn apply(self, m: &mut Module) {
        m.name = self.name;
        m.comment = self.comment;
        m.frequency_type = self.frequency_type;
        m.default_tempo = self.default_tempo as usize;
        m.default_bpm = self.default_bpm as usize;
        m.pattern_highlight = self.pattern_highlight;
        m.pitch_wheel_depth = self.pitch_wheel_depth;
        m.mix_volume = self.mix_volume;
        m.channel_names = self.channel_names;
        m.channel_defaults = self.channel_defaults;
    }
}

// ---- orgn: source-format provenance --------------------------------------

/// Source-format provenance — a wire-local mirror of the model's
/// `ModuleFormat` (cited in code, not linked: it does not exist in a build
/// with no importer, and this doc does). The model enum is `#[cfg]`-gated
/// on the importer features (and so is `Module::origin`), but **this type is
/// compiled in every build** (§5.1), so every reader can at least *parse* the
/// chunk. The cfg gap is crossed only inside the private `origin_bridge` and
/// never leaks into the wire type.
///
/// Parsing is not the whole story: a build with no importer has nowhere to put
/// the value, and a writer regenerates chunks from the model. That is why
/// [`apply_origin`] reports whether it could store it — the reader then carries
/// the raw chunk through §8 retention instead, which is what actually makes an
/// `orgn` survive a load/save in a format-only build.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Mod,
    S3m,
    Xm,
    It,
    Sid,
    Dw,
    Tfmx,
}

/// The `orgn` payload for a module, or `None` when there is nothing to record
/// — no importer compiled in, or `origin` is absent / `Unknown` (§3: a
/// default-valued chunk is not written).
pub fn origin_of(m: &Module) -> Option<Origin> {
    origin_bridge::read(m)
}

/// Write an `orgn` value back onto a module.
///
/// Returns `false` when this build has nowhere to put it — the `origin` field
/// is `#[cfg]`-gated on the importers, so a format-only build parses the chunk
/// and can do nothing with it. The caller is expected to carry the raw bytes
/// through instead (§8), which is what keeps a rewrite from silently dropping
/// provenance the reader understood perfectly well.
#[must_use]
pub fn apply_origin(origin: Origin, m: &mut Module) -> bool {
    origin_bridge::write(origin, m)
}

// The one place the wire touches the `#[cfg]`-gated model field. Split in two
// so the wire type and its public API stay uncfg'd; only this bridge changes
// shape with the feature set.
#[cfg(any(
    feature = "import_mod",
    feature = "import_xm",
    feature = "import_s3m",
    feature = "import_it",
    feature = "import_sid",
    feature = "import_dw",
    feature = "import_tfmx",
))]
mod origin_bridge {
    use super::{Module, Origin};
    use crate::tracker::format::ModuleFormat;

    pub fn read(m: &Module) -> Option<Origin> {
        match m.origin {
            None | Some(ModuleFormat::Unknown) => None,
            Some(ModuleFormat::Mod) => Some(Origin::Mod),
            Some(ModuleFormat::S3m) => Some(Origin::S3m),
            Some(ModuleFormat::Xm) => Some(Origin::Xm),
            Some(ModuleFormat::It) => Some(Origin::It),
            Some(ModuleFormat::Sid) => Some(Origin::Sid),
            Some(ModuleFormat::Dw) => Some(Origin::Dw),
            Some(ModuleFormat::Tfmx) => Some(Origin::Tfmx),
        }
    }

    pub fn write(origin: Origin, m: &mut Module) -> bool {
        m.origin = Some(match origin {
            Origin::Mod => ModuleFormat::Mod,
            Origin::S3m => ModuleFormat::S3m,
            Origin::Xm => ModuleFormat::Xm,
            Origin::It => ModuleFormat::It,
            Origin::Sid => ModuleFormat::Sid,
            Origin::Dw => ModuleFormat::Dw,
            Origin::Tfmx => ModuleFormat::Tfmx,
        });
        true
    }
}

#[cfg(not(any(
    feature = "import_mod",
    feature = "import_xm",
    feature = "import_s3m",
    feature = "import_it",
    feature = "import_sid",
    feature = "import_dw",
    feature = "import_tfmx",
)))]
mod origin_bridge {
    use super::{Module, Origin};

    /// No importer ⇒ `Module` has no `origin` field ⇒ nothing to record.
    pub fn read(_m: &Module) -> Option<Origin> {
        None
    }

    /// No importer ⇒ no `origin` field to write. Saying so is the point: the
    /// caller carries the chunk's bytes through the rewrite instead of
    /// dropping provenance it parsed without difficulty.
    pub fn write(_origin: Origin, _m: &mut Module) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::super::{from_cbor, to_cbor};
    use super::*;
    use alloc::vec;

    #[test]
    fn registry_ids_well_cased() {
        // Structural chunks are critical (byte 0 uppercase); provenance / edit
        // history are ancillary (byte 0 lowercase).
        assert!(MHDR.is_critical() && PCM.is_critical());
        assert!(ORGN.is_ancillary() && GENR.is_ancillary() && HSTC.is_ancillary());
        // Ancillary chunks are safe to copy through a rewrite.
        assert!(ORGN.is_safe_to_copy() && GENR.is_safe_to_copy());
        // PCM separation (§4) is implemented, so the blob region and its
        // directory are in the handled set.
        assert!(KNOWN.contains(&PCM) && KNOWN.contains(&ASET));
        assert!(KNOWN.contains(&MHDR) && KNOWN.contains(&INST));
    }

    #[test]
    fn mhdr_round_trips_through_cbor() {
        let m = Module {
            name: "hello".into(),
            comment: "world".into(),
            default_tempo: 6,
            default_bpm: 125,
            pitch_wheel_depth: 12,
            channel_names: vec!["L".into(), "R".into()],
            ..Module::default()
        };

        let w = MHdr::from_module(&m);
        let bytes = to_cbor(&w).unwrap();
        let back: MHdr = from_cbor(&bytes).unwrap();
        assert_eq!(w, back);

        let mut m2 = Module::default();
        back.apply(&mut m2);
        assert_eq!(m2.name, "hello");
        assert_eq!(m2.comment, "world");
        assert_eq!(m2.default_tempo, 6);
        assert_eq!(m2.default_bpm, 125);
        assert_eq!(m2.pitch_wheel_depth, 12);
        assert_eq!(m2.channel_names, vec!["L".to_string(), "R".to_string()]);
    }

    #[test]
    fn provenance_carries_this_crate() {
        let p = Provenance::current();
        assert_eq!(p.writer_name, "xmrs");
        assert!(!p.writer_version.is_empty());
        let bytes = to_cbor(&p).unwrap();
        assert_eq!(from_cbor::<Provenance>(&bytes).unwrap(), p);
    }

    #[test]
    fn v1_min_reader_is_one() {
        assert_eq!(min_reader_version(&Module::default()), 1);
    }

    #[test]
    fn origin_enum_round_trips_through_cbor() {
        for o in [
            Origin::Mod,
            Origin::S3m,
            Origin::Xm,
            Origin::It,
            Origin::Sid,
            Origin::Dw,
        ] {
            let bytes = to_cbor(&o).unwrap();
            assert_eq!(from_cbor::<Origin>(&bytes).unwrap(), o);
        }
    }

    // The bridge only has a field to touch when an importer is compiled in;
    // the no-importer path (always `None`) is exercised by the bare-metal
    // feature-matrix build.
    #[cfg(any(
        feature = "import_mod",
        feature = "import_xm",
        feature = "import_s3m",
        feature = "import_it",
        feature = "import_sid",
        feature = "import_dw",
        feature = "import_tfmx",
    ))]
    #[test]
    fn origin_bridge_round_trips_with_importers() {
        let mut m = Module::default();
        assert_eq!(origin_of(&m), None); // default origin ⇒ no chunk
        assert!(
            apply_origin(Origin::It, &mut m),
            "with an importer compiled in, the field exists and takes the value"
        );
        assert_eq!(origin_of(&m), Some(Origin::It));
    }
}
