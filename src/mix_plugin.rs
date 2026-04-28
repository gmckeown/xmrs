//! IT-style mix-plugin table (OpenMPT extension).
//!
//! ScreamTracker / Impulse Tracker file formats can carry an
//! optional plugin section, originally introduced by OpenMPT, that
//! describes a chain of DSP / VST plugins applied to channels and
//! buses at mix time. The plugin payloads themselves are opaque
//! binary blobs (typically VST chunk dumps) — xmrs does not host
//! the plugins, but it preserves them verbatim so a round-trip
//! through the editor doesn't lose the author's mix settings.
//!
//! Non-IT formats leave the containing `Module::mix_plugins` field
//! at `None`.

use serde::{Deserialize, Serialize};

use alloc::vec::Vec;

/// Identification + routing fields for a single plugin slot,
/// mirroring OpenMPT's `SNDMIXPLUGININFO`. Identifiers are kept as
/// raw `u32`s because the four-character codes used by the format
/// are not always valid UTF-8 (and aren't always meaningful as
/// FOURCCs — some are numeric IDs).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct MixPluginInfo {
    /// Primary plugin ID (typically a packed four-character code
    /// such as `'VstP'` for a generic VST host slot, or a built-in
    /// identifier for OpenMPT's bundled effects).
    pub id1: u32,
    /// Secondary plugin ID, used by the host to disambiguate
    /// between plugins that share the same `id1`.
    pub id2: u32,
    /// Source bus for plugin input. Routing semantics follow
    /// OpenMPT — see its docs for the exact field encoding.
    pub input_routing: u32,
    /// Destination bus for plugin output.
    pub output_routing: u32,
}

/// One plugin slot. The `data` blob is the plugin's serialised
/// state (typically a VST chunk produced by `effGetChunk`); xmrs
/// treats it as opaque bytes.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct MixPlugin {
    pub info: MixPluginInfo,
    /// Plugin-specific binary state. `None` when the slot exists
    /// in the file but carries no chunk (i.e., a default-state
    /// plugin or an empty slot kept for indexing).
    pub data: Option<Vec<u8>>,
}

/// Mix-plugin table for one module. Two parallel pieces of data:
/// per-channel routing assignments, and a flat list of plugin
/// slots referenced by those assignments.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct MixPlugins {
    /// Per-channel plugin assignment. Index = channel number.
    /// Value `0` = no plugin on that channel; value `N` (1-based)
    /// means the channel is routed through `plugins[N - 1]`. The
    /// IT format reserves up to 64 entries.
    pub channel_assignments: Vec<u32>,
    /// Flat list of plugin slots, addressed 1-based by
    /// `channel_assignments`. Up to 64 entries.
    pub plugins: Vec<MixPlugin>,
}
