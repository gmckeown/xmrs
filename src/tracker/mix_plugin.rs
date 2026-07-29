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

use alloc::string::String;
use alloc::vec::Vec;

/// Identification + routing fields for a single plugin slot, mirroring
/// the relevant parts of OpenMPT's 128-byte `SNDMIXPLUGININFO`.
/// Identifiers are kept as raw `u32`s because the four-character codes
/// used by the format are not always valid UTF-8 (and aren't always
/// meaningful as FOURCCs — some are numeric IDs).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct MixPluginInfo {
    /// Primary plugin ID (a packed four-character code such as `'VstP'`
    /// for a VST 2.x slot or `'DXMO'` for a DirectX Media Object).
    pub id1: u32,
    /// Secondary plugin ID, disambiguating plugins that share `id1`
    /// (a vendor FOURCC for VST, a GUID fragment for DMO).
    pub id2: u32,
    /// Routing flags (`SNDMIXPLUGININFO::routingFlags`): bit `0x01`
    /// `irApplyToMaster`, `0x02` `irBypass`, `0x04` `irDryMix`, …
    pub routing_flags: u8,
    /// Mix mode (`SNDMIXPLUGININFO::mixMode`).
    pub mix_mode: u8,
    /// Plugin gain, tenths (real gain = `gain / 10`).
    pub gain: u8,
    /// `SNDMIXPLUGININFO::reserved` — the padding byte after `gain`,
    /// carried through rather than dropped.
    #[serde(default)]
    pub reserved: u8,
    /// Output routing (`dwOutputRouting`): `0` = master, `0x80 + x` =
    /// chained into plugin `x`.
    pub output_routing: u32,
    /// Which child to load inside a *shell* plugin — a single DLL
    /// hosting several effects.
    ///
    /// Required to identify such a plugin: OpenMPT assigns every
    /// child of one shell the **same** [`Self::id1`] and
    /// [`Self::id2`], and disambiguates on this third value. A
    /// census keyed on the ID pair alone merges a whole bundle into
    /// one entry. `0` for ordinary, non-shell plugins.
    #[serde(default)]
    pub shell_plugin_id: u32,
    /// The three words OpenMPT reserves for routing information.
    /// Always zero in practice; preserved so a reader sees the file
    /// as it is rather than as the format promises it should be.
    #[serde(default)]
    pub reserved3: [u32; 3],
    /// User-chosen display name (`szName`, ANSI in the file).
    pub name: String,
    /// Original library / DLL name (`szLibraryName`, UTF-8 in the file).
    pub library_name: String,
}

/// One plugin slot. The `data` blob is the plugin's serialised parameter
/// state (`pluginData`): for a native DMO it is a `u32` type tag (`0`)
/// followed by the normalised `f32` parameters; for a VST it is the
/// opaque `effGetChunk` dump. xmrs does not host plugins, but the
/// importer can convert known DMOs into native devices (see
/// [`crate::tracker::import::it::it_plugin_convert`]).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct MixPlugin {
    pub info: MixPluginInfo,
    /// Plugin-specific parameter blob. `None` when the slot exists in the
    /// file but carries no data.
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
