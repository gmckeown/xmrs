//! IT mix-plugin table parser (OpenMPT extension). Decodes the
//! `XTPM`-style `MIX_PLUGIN_INFO` blocks into a
//! [`crate::tracker::mix_plugin::MixPlugins`] table — opaque to the
//! sample engine but preserved for downstream VST hosts that wrap
//! the player.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::tracker::import::bin_reader::{BinReader, ImportError};
use crate::tracker::mix_plugin::{
    MixPlugin as PublicMixPlugin, MixPluginInfo, MixPlugins as PublicMixPlugins,
};

const MAX_MIXPLUGINS: usize = 64;

/// On-disk size of OpenMPT's `SNDMIXPLUGININFO`, written verbatim to the
/// file (`MPT_BINARY_STRUCT(SNDMIXPLUGININFO, 128)`): id1(4) · id2(4) ·
/// routingFlags(1) · mixMode(1) · gain(1) · reserved(1) ·
/// dwOutputRouting(4) · shellPluginID(4) · dwReserved[3](12) ·
/// szName[32] · szLibraryName[64] = 128 bytes.
const SND_MIX_PLUGIN_INFO_SIZE: usize = 128;

/// Decode a fixed-size, null-terminated byte field into a `String`
/// (lossy UTF-8; the trailing zero padding is dropped).
fn read_cstr(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

#[derive(Debug, Clone, Default)]
pub struct SndMixPluginInfo {
    pub id1: u32,
    pub id2: u32,
    pub routing_flags: u8,
    pub mix_mode: u8,
    pub gain: u8,
    pub output_routing: u32,
    pub name: String,
    pub library_name: String,
}

impl SndMixPluginInfo {
    /// Read the full 128-byte `SNDMIXPLUGININFO`.
    fn read(r: &mut BinReader) -> Result<Self, ImportError> {
        let id1 = r.read_u32_le()?;
        let id2 = r.read_u32_le()?;
        let routing_flags = r.read_u8()?;
        let mix_mode = r.read_u8()?;
        let gain = r.read_u8()?;
        let _reserved = r.read_u8()?;
        let output_routing = r.read_u32_le()?;
        let _shell_plugin_id = r.read_u32_le()?;
        let _reserved3 = [r.read_u32_le()?, r.read_u32_le()?, r.read_u32_le()?];
        let name = r.read_array::<32>()?;
        let library_name = r.read_array::<64>()?;
        Ok(Self {
            id1,
            id2,
            routing_flags,
            mix_mode,
            gain,
            output_routing,
            name: read_cstr(&name),
            library_name: read_cstr(&library_name),
        })
    }
}

#[derive(Debug, Clone)]
pub struct MixPlugin {
    pub info: SndMixPluginInfo,
    pub data: Option<Vec<u8>>,
}

#[derive(Debug)]
pub struct Plugins {
    pub channel_settings: Vec<u32>,
    pub mix: Vec<MixPlugin>,
}

impl Default for Plugins {
    fn default() -> Self {
        Self {
            channel_settings: vec![0; 64],
            mix: vec![
                MixPlugin {
                    info: SndMixPluginInfo::default(),
                    data: None,
                };
                MAX_MIXPLUGINS
            ],
        }
    }
}

impl Plugins {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load(source: &[u8]) -> Result<(Self, usize), ImportError> {
        let mut data = source;
        let mut plugins = Self::new();

        while data.len() >= 8 {
            let plugin_id = u32::from_le_bytes(data[0..4].try_into().unwrap());
            // `plugin_size` is the chunk **body** size (the 8-byte
            // `tag + size` header is separate); a chunk therefore
            // occupies `8 + plugin_size` bytes. Validate the body fits
            // only once the chunk type is recognised — trailing,
            // non-plugin data (its `[4..8]` reinterpreted as a "size")
            // must end the scan cleanly, not abort the whole table.
            let plugin_size = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;

            if plugin_id == u32::from_le_bytes(*b"CHFX") {
                if data.len() < 8 + plugin_size {
                    return Err(ImportError::OutOfRange("Plugin Size too big!?"));
                }
                // One `u32` channel→slot assignment per channel. Modules
                // with fewer than 64 channels write a shorter table, so
                // read exactly what the chunk carries (capped at the 64
                // the format reserves) and stop — running to a fixed 64
                // here used to error out on every < 64-channel module,
                // discarding an otherwise-valid plugin table.
                let body = &data[8..8 + plugin_size];
                for (ch, slot) in body.chunks_exact(4).take(64).enumerate() {
                    plugins.channel_settings[ch] = u32::from_le_bytes(slot.try_into().unwrap());
                }
                data = &data[8 + plugin_size..];
            } else if data[0] == b'F' && data[1] == b'X' && data[2] >= b'0' && data[3] >= b'0' {
                // ASCII digits only — letters past '9' would overflow
                // the u8 arithmetic below.
                if data[2] > b'9' || data[3] > b'9' {
                    return Err(ImportError::OutOfRange("FX plugin index"));
                }
                if plugin_size < SND_MIX_PLUGIN_INFO_SIZE || data.len() < 8 + plugin_size {
                    return Err(ImportError::OutOfRange("FX plugin size"));
                }
                let plugin_index = ((data[2] - b'0') * 10 + (data[3] - b'0')) as usize;
                if plugin_index >= MAX_MIXPLUGINS {
                    return Err(ImportError::OutOfRange("Plugin Index overflow!?"));
                }
                let body = &data[8..8 + plugin_size];
                let mut r = BinReader::new(body);
                plugins.mix[plugin_index].info = SndMixPluginInfo::read(&mut r)?;
                // After the 128-byte info: a `u32` plugDataSize then that
                // many bytes of `pluginData` (the plugin's real parameter
                // blob). Trailing modular extra-data (DWRT/PROG) is
                // ignored. This is the clean layout — the blob no longer
                // carries the info tail the old 32-byte misread left in.
                if body.len() >= SND_MIX_PLUGIN_INFO_SIZE + 4 {
                    let pds = u32::from_le_bytes(
                        body[SND_MIX_PLUGIN_INFO_SIZE..SND_MIX_PLUGIN_INFO_SIZE + 4]
                            .try_into()
                            .unwrap(),
                    ) as usize;
                    let start = SND_MIX_PLUGIN_INFO_SIZE + 4;
                    let end = start.saturating_add(pds).min(body.len());
                    if end > start {
                        plugins.mix[plugin_index].data = Some(body[start..end].to_vec());
                    }
                }
                data = &data[8 + plugin_size..];
            } else {
                break;
            }
        }
        Ok((plugins, source.len() - data.len()))
    }

    /// Project the internal, file-shaped representation into the public
    /// `MixPlugins` type that lives on `Module`. Both vectors keep their
    /// indexing so `channel_assignments[ch] - 1` still selects the right
    /// plugin slot.
    pub fn to_public(&self) -> PublicMixPlugins {
        PublicMixPlugins {
            channel_assignments: self.channel_settings.clone(),
            plugins: self
                .mix
                .iter()
                .map(|p| PublicMixPlugin {
                    info: MixPluginInfo {
                        id1: p.info.id1,
                        id2: p.info.id2,
                        routing_flags: p.info.routing_flags,
                        mix_mode: p.info.mix_mode,
                        gain: p.info.gain,
                        output_routing: p.info.output_routing,
                        name: p.info.name.clone(),
                        library_name: p.info.library_name.clone(),
                    },
                    data: p.data.clone(),
                })
                .collect(),
        }
    }

    /// `true` when the table carries no useful information — every
    /// channel routes to "no plugin" (0) and every slot is empty.
    /// The IT importer uses this to decide whether to attach the
    /// table to `Module::mix_plugins` at all (vs. leaving it `None`).
    pub fn is_empty(&self) -> bool {
        self.channel_settings.iter().all(|&v| v == 0)
            && self
                .mix
                .iter()
                .all(|p| p.info.id1 == 0 && p.info.id2 == 0 && p.data.is_none())
    }
}
