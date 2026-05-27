//! IT mix-plugin table parser (OpenMPT extension). Decodes the
//! `XTPM`-style `MIX_PLUGIN_INFO` blocks into a
//! [`crate::tracker::mix_plugin::MixPlugins`] table — opaque to the
//! sample engine but preserved for downstream VST hosts that wrap
//! the player.

use alloc::vec;
use alloc::vec::Vec;

use crate::tracker::import::bin_reader::{BinReader, ImportError};
use crate::tracker::mix_plugin::{
    MixPlugin as PublicMixPlugin, MixPluginInfo, MixPlugins as PublicMixPlugins,
};

const MAX_MIXPLUGINS: usize = 64;

/// On-disk size of an `FXxx` plugin's `SndMixPluginInfo`: four
/// `u32` + four reserved `u32` = 32 bytes.
const SND_MIX_PLUGIN_INFO_SIZE: usize = 32;

#[derive(Debug, Clone)]
pub struct SndMixPluginInfo {
    pub id1: u32,
    pub id2: u32,
    pub input_routing: u32,
    pub output_routing: u32,
    pub reserved: [u32; 4],
}

impl SndMixPluginInfo {
    fn read(r: &mut BinReader) -> Result<Self, ImportError> {
        let id1 = r.read_u32_le()?;
        let id2 = r.read_u32_le()?;
        let input_routing = r.read_u32_le()?;
        let output_routing = r.read_u32_le()?;
        let reserved = [
            r.read_u32_le()?,
            r.read_u32_le()?,
            r.read_u32_le()?,
            r.read_u32_le()?,
        ];
        Ok(Self {
            id1,
            id2,
            input_routing,
            output_routing,
            reserved,
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
                    info: SndMixPluginInfo {
                        id1: 0,
                        id2: 0,
                        input_routing: 0,
                        output_routing: 0,
                        reserved: [0; 4],
                    },
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
            let plugin_size = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;

            if data.len() < plugin_size {
                return Err(ImportError::OutOfRange("Plugin Size too big!?"));
            }

            if plugin_id == u32::from_le_bytes(*b"CHFX") {
                data = &data[8..];
                for ch in 0..64 {
                    if ch * 4 < plugin_size {
                        plugins.channel_settings[ch] =
                            u32::from_le_bytes(data[ch * 4..(ch + 1) * 4].try_into().unwrap());
                    } else {
                        return Err(ImportError::UnexpectedEof);
                    }
                }
                data = &data[plugin_size..];
            } else if data[0] == b'F' && data[1] == b'X' && data[2] >= b'0' && data[3] >= b'0' {
                // ASCII digits only — letters past '9' would overflow
                // the u8 arithmetic below.
                if data[2] > b'9' || data[3] > b'9' {
                    return Err(ImportError::OutOfRange("FX plugin index"));
                }
                if plugin_size < SND_MIX_PLUGIN_INFO_SIZE {
                    return Err(ImportError::OutOfRange("FX plugin size"));
                }
                let plugin_index = ((data[2] - b'0') * 10 + (data[3] - b'0')) as usize;
                data = &data[8..];
                if plugin_index >= MAX_MIXPLUGINS {
                    return Err(ImportError::OutOfRange("Plugin Index overflow!?"));
                }
                let mut r = BinReader::new(data);
                plugins.mix[plugin_index].info = SndMixPluginInfo::read(&mut r)?;
                data = &data[SND_MIX_PLUGIN_INFO_SIZE..];

                let extra_size = plugin_size - SND_MIX_PLUGIN_INFO_SIZE;
                if extra_size > data.len() {
                    return Err(ImportError::UnexpectedEof);
                }
                if extra_size != 0 {
                    plugins.mix[plugin_index].data = Some(data[0..extra_size].to_vec());
                }
                data = &data[extra_size..];
            } else {
                break;
            }
        }
        Ok((plugins, source.len() - data.len()))
    }

    /// Project the internal, file-shaped representation into the
    /// public `MixPlugins` type that lives on `Module`. Drops the
    /// `reserved` slot of `SndMixPluginInfo` (it's purely a file
    /// padding field with no editor meaning) and otherwise just
    /// re-shapes the data — both vectors keep their indexing so
    /// `channel_assignments[ch] - 1` still selects the right
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
                        input_routing: p.info.input_routing,
                        output_routing: p.info.output_routing,
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
