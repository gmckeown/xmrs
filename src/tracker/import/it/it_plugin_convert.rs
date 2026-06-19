//! Convert IT/MPTM mix-plugin tables into native DAW [`Device`] insert
//! chains (RFC §6.2 "real DSP content").
//!
//! The IT importer runs this projection **automatically** at the end of
//! [`super::it_module::ItModule::to_module`] (via
//! [`convert_mix_plugins_to_devices`]): a plugin-bearing module comes
//! back already rendering its effects, the way the author mixed it.
//! Modules with no mix-plugin table are untouched, so their render stays
//! **bit-identical** to the pre-conversion player. The raw
//! `Module::mix_plugins` blobs are preserved alongside the projected
//! devices, and the conversion entry point stays `pub` so a dry render
//! can still be obtained by stripping the attached inserts.
//!
//! It decodes the DirectX-DMO parameter blobs and the routing graph into
//! [`crate::core::daw::device`] inserts; opaque VSTs are substituted by a
//! native equivalent where one is known (see the `VST_*` ids) or left
//! unconverted.
//!
//! ## Blob format (reverse-engineered from the OpenMPT source)
//!
//! For a native DMO slot (`id1 == 'DXMO'`), OpenMPT serialises the
//! parameters with `IMixPlugin::SaveAllParameters`: a `uint32 type = 0`
//! followed by `numParams` little-endian `float32`s, each the normalised
//! `[0, 1]` parameter. The IT importer now parses the full 128-byte
//! `SNDMIXPLUGININFO` and stores only this `pluginData` blob in
//! `MixPlugin.data`:
//!
//! ```text
//! [0..4]  type            (= 0 for a positional float array)
//! [4..]   numParams × float32 LE   ← the normalised parameters
//! ```
//!
//! Confirmed against the OpenMPT source (`soundlib/Load_it.cpp`
//! `SaveMixPlugins`, `soundlib/plugins/PlugInterface.cpp`
//! `SaveAllParameters`, `PluginStructs.h` `SNDMIXPLUGININFO` = 128 bytes)
//! and empirically against every `.it` in the reference corpus. The
//! per-effect parameter formulas are transcribed from
//! `soundlib/plugins/dmo/*.cpp`.

use alloc::vec;
use alloc::vec::Vec;

use crate::core::daw::device::{Device, DeviceChain, ModDelayMode, ReverbMode};
use crate::core::daw::dsp;
use crate::core::fixed::fixed::Q15;
use crate::core::module::Module;
use crate::tracker::mix_plugin::{MixPlugin, MixPlugins};

/// `'DXMO'` (DirectX Media Object), big-endian packed.
const DXMO: u32 = 0x4458_4D4F;
/// `'VstP'` (a real VST 2.x slot), big-endian packed.
const VSTP: u32 = 0x5673_7450;

// DMO unique IDs (id2) — from the census / OpenMPT `PluginManager.cpp`.
const ID_COMPRESSOR: u32 = 0xEF01_1F79;
const ID_DISTORTION: u32 = 0xEF11_4C90;
const ID_ECHO: u32 = 0xEF3E_932C;
const ID_CHORUS: u32 = 0xEFE6_629C;
const ID_FLANGER: u32 = 0xEFCA_3D92;
const ID_PARAMEQ: u32 = 0x120C_ED89;
const ID_WAVESREVERB: u32 = 0x87FC_0268;
const ID_I3DL2REVERB: u32 = 0xEF98_5E71;
const ID_GARGLE: u32 = 0xDAFD_8210;

// Known VST unique IDs we substitute with a native device.
const VST_JS_COMPRESSOR: u32 = 0x4761_696E; // 'Gain' — "JS Compresseur"
const VST_FREEVERB: u32 = 0x4A7A_5232; // 'JzR2' — Freeverb

/// Offset of the first parameter float inside `MixPlugin.data`, after the
/// `u32` type tag.
const DMO_PARAM_BASE: usize = 4;

/// `routingFlags` bit marking a bypassed plugin (OpenMPT `irBypass`).
const IR_BYPASS: u8 = 0x02;
/// `routingFlags` bit marking a plugin applied to the **master** mix
/// rather than a single channel (OpenMPT `irApplyToMaster`).
const IR_APPLY_TO_MASTER: u8 = 0x01;

#[inline]
fn clampf(x: f32, lo: f32, hi: f32) -> f32 {
    if x < lo {
        lo
    } else if x > hi {
        hi
    } else {
        x
    }
}

/// Q1.15 full-scale, as `f32`, for the import-time float→Q15 conversions
/// below (the importer is allowed to use `f32`; the render path is not).
const Q15_FULL_SCALE_F32: f32 = 32_767.0;

/// Normalised `[0, 1]` → `Q15`.
#[inline]
fn q15(x: f32) -> Q15 {
    Q15::from_raw((clampf(x, 0.0, 1.0) * Q15_FULL_SCALE_F32) as i16)
}

/// Signed `[-1, 1]` → `Q15`.
#[inline]
fn q15s(x: f32) -> Q15 {
    Q15::from_raw((clampf(x, -1.0, 1.0) * Q15_FULL_SCALE_F32) as i16)
}

/// Read `n` normalised parameter floats from a DMO state blob, or `None`
/// if the blob is not a positional float array (e.g. a VST chunk) or is
/// too short.
fn read_dmo_floats(data: &[u8], n: usize) -> Option<[f32; 13]> {
    if data.len() < 4 {
        return None;
    }
    // `type` word must be 0 (individual parameters, not a VST chunk).
    if u32::from_le_bytes(data[0..4].try_into().ok()?) != 0 {
        return None;
    }
    if data.len() < DMO_PARAM_BASE + 4 * n {
        return None;
    }
    let mut out = [0f32; 13];
    for (i, slot) in out.iter_mut().enumerate().take(n) {
        let o = DMO_PARAM_BASE + 4 * i;
        *slot = f32::from_le_bytes(data[o..o + 4].try_into().ok()?);
    }
    Some(out)
}

// --- Per-effect conversions (formulas verbatim from OpenMPT dmo/*.cpp) -

fn dmo_compressor(p: &[f32; 13]) -> Device {
    // 0 Gain(make-up) -60+x·120 dB · 1 Attack 0.01+x·499.99 ms ·
    // 2 Release 50+x·2950 ms · 3 Threshold -60+x·60 dB ·
    // 4 Ratio 1+x·99 · 5 Predelay (no native equivalent — ignored).
    let makeup_mdb = ((-60.0 + p[0] * 120.0) * 1000.0) as i32;
    let attack_us = ((0.01 + p[1] * 499.99) * 1000.0) as u32;
    let release_us = ((50.0 + p[2] * 2950.0) * 1000.0) as u32;
    let thr_mdb = ((-60.0 + p[3] * 60.0) * 1000.0) as i32;
    let ratio_q8_8 = ((1.0 + p[4] * 99.0) * 256.0) as u16;
    Device::compressor(thr_mdb, ratio_q8_8, attack_us, release_us, makeup_mdb)
}

fn dmo_distortion(p: &[f32; 13]) -> Device {
    // 0 Gain -60+x·60 dB · 1 Edge (normalised) · 2 PreLowpass ·
    // 3 PostEQCenter · 4 PostEQBandwidth, all 100+x·7900 Hz.
    let gain_mdb = ((-60.0 + p[0] * 60.0) * 1000.0) as i32;
    let pre_lp = (100.0 + p[2] * 7900.0) as u16;
    let post_c = (100.0 + p[3] * 7900.0) as u16;
    let post_bw = (100.0 + p[4] * 7900.0) as u16;
    Device::distortion(gain_mdb, q15(p[1]), post_c, post_bw, pre_lp)
}

fn dmo_echo(p: &[f32; 13]) -> Device {
    // 0 WetDryMix · 1 Feedback (normalised) · 2 LeftDelay · 3 RightDelay
    // 1+x·1999 ms · 4 PanDelay (bool).
    Device::echo(
        q15(p[0]),
        q15(p[1]),
        (1.0 + p[2] * 1999.0) as u16,
        (1.0 + p[3] * 1999.0) as u16,
        p[4] > 0.5,
    )
}

fn dmo_chorus(p: &[f32; 13]) -> Device {
    // 0 WetDry · 1 Depth · 2 Frequency x·10 Hz · 3 WaveShape(<1 square,
    // else sine) · 4 Phase · 5 Feedback -99+x·198 % · 6 Delay x·20 ms.
    Device::mod_delay(
        ModDelayMode::Chorus,
        q15(p[0]),
        q15(p[1]),
        q15s((-99.0 + p[5] * 198.0) / 100.0),
        (p[2] * 10.0 * 1000.0) as u32,
        p[3] >= 0.5,
        (p[6] * 20.0 * 256.0) as u16,
        p[4] != 0.5,
    )
}

fn dmo_flanger(p: &[f32; 13]) -> Device {
    // Note the Flanger parameter order differs from Chorus:
    // 0 WetDry · 1 WaveShape · 2 Frequency x·10 Hz · 3 Depth ·
    // 4 Phase · 5 Feedback -99+x·198 % · 6 Delay x·4 ms.
    Device::mod_delay(
        ModDelayMode::Flanger,
        q15(p[0]),
        q15(p[3]),
        q15s((-99.0 + p[5] * 198.0) / 100.0),
        (p[2] * 10.0 * 1000.0) as u32,
        p[1] >= 0.5,
        (p[6] * 4.0 * 256.0) as u16,
        p[4] != 0.5,
    )
}

fn dmo_parameq(p: &[f32; 13]) -> Device {
    // 0 Center 80+x·15920 Hz · 1 Bandwidth 1+x·35 semi · 2 Gain
    // (x-0.5)·30 dB.
    Device::param_eq(
        (80.0 + p[0] * 15920.0) as u16,
        (1.0 + p[1] * 35.0 + 0.5) as u8,
        ((p[2] - 0.5) * 30.0 * 1000.0) as i16,
    )
}

fn dmo_wavesreverb(p: &[f32; 13]) -> Device {
    // 0 InGain (folded into wet) · 1 ReverbMix (normalised → wet) ·
    // 2 ReverbTime (normalised → room size) · 3 HighFreqRTRatio
    // (more ratio = brighter = less damping).
    Device::reverb(
        ReverbMode::WavesReverb,
        q15(p[2]),
        q15(1.0 - p[3]),
        Q15::ONE,
        q15(p[1] / 3.0),         // wet (×scalewet=3 ⇒ ≈ ReverbMix)
        q15((1.0 - p[1]) / 2.0), // dry (×scaledry=2 ⇒ ≈ 1 − ReverbMix)
    )
}

fn dmo_gargle(p: &[f32; 13]) -> Device {
    // 0 Rate: round(x·999)+1 Hz · 1 WaveShape: <0.5 triangle, else square.
    let rate_hz = (p[0] * 999.0 + 0.5) as u16 + 1;
    Device::gargle(rate_hz, p[1] >= 0.5)
}

fn dmo_i3dl2(p: &[f32; 13]) -> Device {
    // 0 Room -10000+x·10000 mB · 3 DecayTime 0.1+x·19.9 s ·
    // 4 DecayHFRatio 0.1+x·1.9 · 5 Reflections -10000+x·11000 mB ·
    // 6 ReflectionsDelay x·0.3 s · 7 Reverb -10000+x·12000 mB ·
    // 8 ReverbDelay x·0.1 s. (RoomHF/Rolloff/Diffusion/Density/
    // HFReference/Quality not modelled by the C1-engine approximation.)
    Device::i3dl2_reverb(
        (-10000.0 + p[0] * 10000.0) as i16,
        ((0.1 + p[3] * 19.9) * 1000.0) as u16,
        ((0.1 + p[4] * 1.9) * 1000.0) as u16,
        (-10000.0 + p[5] * 11000.0) as i16,
        (p[6] * 0.3 * 1000.0) as u16,
        (-10000.0 + p[7] * 12000.0) as i16,
        (p[8] * 0.1 * 1000.0) as u16,
    )
}

/// Freeverb2 (`JzR2`) parameter map (from Ghidra `getParameterName` on
/// `Freeverb2.dll`): `0 RoomSize · 1 Damping · 2 Predelay · 3 Lowpass ·
/// 4 Highpass · 5 Wet · 6 Dry`. The wet/dry scaling (`scalewet = 3`,
/// `scaledry = 2`) is applied internally by [`Device::Reverb`], so the
/// stored `[0, 1]` values map straight onto its `wet`/`dry`. Predelay and
/// the low/high-pass tone controls are not modelled by the native engine
/// (Freeverb v3 has neither) and are dropped.
fn vst_freeverb(p: &[f32; 13]) -> Device {
    Device::reverb(
        ReverbMode::Freeverb,
        q15(p[0]), // room size
        q15(p[1]), // damping
        Q15::ONE,  // width (Freeverb2 has none; the engine default is 1.0)
        q15(p[5]), // wet level
        q15(p[6]), // dry level
    )
}

/// `20·log10(lin)` in **milli-decibels**, via the integer `log2` helper
/// (no transcendental / no libm). `20000·log10(x) = log2(x)·20000/log2(10)`
/// and `20000/3.321928 ≈ 6021`.
fn lin_to_milli_db(lin: f32) -> i32 {
    let x_q16_16 = (clampf(lin, 0.0001, 64.0) * Q15_FULL_SCALE_F32 * 2.0) as i64;
    ((dsp::log2_q16_16(x_q16_16) as i64 * 6021) / 65536) as i32
}

/// "JS Compresseur V1.1" (`id2 = 'Gain'`, a modified Steinberg AGain
/// example). 3 params, from Ghidra `getParameterName` on the binary:
/// `0 Gain In · 1 Threshold · 2 Gain Out`. The gains are a `[0, 1]` value
/// scaled **×4** internally (`_DAT_1000501c`, so `0.25` = unity = 0 dB);
/// Threshold is a raw linear amplitude. The DSP is a soft **exponential
/// limiter** above the threshold — no ratio / attack / release controls —
/// so we approximate it as a fast, high-ratio [`Device::Compressor`].
fn vst_js_compresseur(p: &[f32; 13]) -> Device {
    const GAIN_SCALE: f32 = 4.0; // _DAT_1000501c
    let gain_in_mdb = lin_to_milli_db(p[0] * GAIN_SCALE);
    // Input gain pushes the signal into the threshold ⇒ fold it into a
    // lower effective threshold (the native compressor has no input gain).
    let threshold_mdb = (lin_to_milli_db(p[1]) - gain_in_mdb).clamp(-60_000, 0);
    let makeup_mdb = lin_to_milli_db(p[2] * GAIN_SCALE).clamp(-60_000, 60_000);
    // ~20:1 + instantaneous (the original has no time-domain envelope).
    Device::compressor(threshold_mdb, 20 * 256, 10, 50_000, makeup_mdb)
}

/// Decode a single mix-plugin slot into a native [`Device`], or `None`
/// when the slot is empty or an unrecognised plugin. All known plugins —
/// the 9 DMOs and both census VSTs (**Freeverb2** and **JS Compresseur**,
/// whose params turned out to be plain positional float arrays, the
/// orderings reverse-engineered from their binaries in Ghidra) — are
/// decoded from their actual stored settings.
pub fn device_from_mix_plugin(plug: &MixPlugin) -> Option<Device> {
    match plug.info.id1 {
        DXMO => {
            let data = plug.data.as_ref()?;
            match plug.info.id2 {
                ID_COMPRESSOR => read_dmo_floats(data, 6).map(|p| dmo_compressor(&p)),
                ID_DISTORTION => read_dmo_floats(data, 5).map(|p| dmo_distortion(&p)),
                ID_ECHO => read_dmo_floats(data, 5).map(|p| dmo_echo(&p)),
                ID_CHORUS => read_dmo_floats(data, 7).map(|p| dmo_chorus(&p)),
                ID_FLANGER => read_dmo_floats(data, 7).map(|p| dmo_flanger(&p)),
                ID_PARAMEQ => read_dmo_floats(data, 3).map(|p| dmo_parameq(&p)),
                ID_WAVESREVERB => read_dmo_floats(data, 4).map(|p| dmo_wavesreverb(&p)),
                ID_I3DL2REVERB => read_dmo_floats(data, 13).map(|p| dmo_i3dl2(&p)),
                ID_GARGLE => read_dmo_floats(data, 2).map(|p| dmo_gargle(&p)),
                _ => None,
            }
        }
        VSTP => match plug.info.id2 {
            // Freeverb2 saves its params positionally (type=0), so decode
            // them like a DMO rather than substituting.
            VST_FREEVERB => plug
                .data
                .as_ref()
                .and_then(|d| read_dmo_floats(d, 7))
                .map(|p| vst_freeverb(&p)),
            // JS Compresseur stores 3 positional floats (type=0) too.
            VST_JS_COMPRESSOR => plug
                .data
                .as_ref()
                .and_then(|d| read_dmo_floats(d, 3))
                .map(|p| vst_js_compresseur(&p)),
            _ => None,
        },
        _ => None,
    }
}

/// Project a mix-plugin table onto per-mixer-channel insert chains.
///
/// `channel_assignments[ch]` (1-based) is the plugin directly on the
/// channel; that plugin's `output_routing` (`0x80 + x` = "send to plugin
/// x") is followed to build a **serial** insert chain. Bypassed slots
/// (`irBypass`) and unconvertible slots (opaque VSTs, unknown effects)
/// are skipped; the returned vector has one entry per channel (empty
/// chains for channels with no plugins).
pub fn project_channel_inserts(plugins: &MixPlugins, num_channels: usize) -> Vec<DeviceChain> {
    let mut out = vec![DeviceChain::default(); num_channels];
    let n_plugins = plugins.plugins.len();
    for (ch, chain) in out.iter_mut().enumerate() {
        let mut slot1 = plugins.channel_assignments.get(ch).copied().unwrap_or(0) as usize;
        let mut guard = 0;
        while slot1 >= 1 && slot1 <= n_plugins && guard < 64 {
            let plug = &plugins.plugins[slot1 - 1];
            if plug.info.routing_flags & IR_BYPASS == 0 {
                if let Some(dev) = device_from_mix_plugin(plug) {
                    chain.devices.push(dev);
                }
            }
            // Follow the output routing to the next chained plugin.
            let out_r = plug.info.output_routing;
            if out_r & 0x80 != 0 {
                slot1 = (out_r & 0x7F) as usize + 1;
            } else {
                break;
            }
            guard += 1;
        }
    }
    out
}

/// Project the **master**-mix plugins (those flagged `irApplyToMaster`)
/// into a single serial chain, following their `output_routing` links.
/// Each plugin is added at most once.
pub fn project_master_chain(plugins: &MixPlugins) -> DeviceChain {
    let n = plugins.plugins.len();
    let mut visited = vec![false; n];
    let mut chain = DeviceChain::default();
    for start in 0..n {
        if plugins.plugins[start].info.routing_flags & IR_APPLY_TO_MASTER == 0 {
            continue;
        }
        let mut slot1 = start + 1;
        let mut guard = 0;
        while slot1 >= 1 && slot1 <= n && guard < 64 {
            if visited[slot1 - 1] {
                break;
            }
            visited[slot1 - 1] = true;
            let plug = &plugins.plugins[slot1 - 1];
            if plug.info.routing_flags & IR_BYPASS == 0 {
                if let Some(dev) = device_from_mix_plugin(plug) {
                    chain.devices.push(dev);
                }
            }
            let out_r = plug.info.output_routing;
            if out_r & 0x80 != 0 {
                slot1 = (out_r & 0x7F) as usize + 1;
            } else {
                break;
            }
            guard += 1;
        }
    }
    chain
}

/// **Opt-in** conversion: decode `module.mix_plugins` into native device
/// chains — per-channel inserts on `module.channel_inserts` and any
/// master-flagged plugins onto `module.master_chain`. Returns the number
/// of chains (channels + the master chain) that received at least one
/// device.
///
/// The importer never calls this, so a default import stays
/// byte-identical (empty inserts ⇒ inert). Invoke it explicitly to make
/// a plugin-bearing IT/MPTM module render its effects. A module with no
/// `mix_plugins` is left untouched. Plugins that are neither
/// channel-routed nor master-flagged (orphan slots) are left unconverted.
pub fn convert_mix_plugins_to_devices(module: &mut Module) -> usize {
    let Some(mp) = module.mix_plugins.clone() else {
        return 0;
    };
    let num_channels = module.get_num_channels();
    let inserts = project_channel_inserts(&mp, num_channels);
    let master = project_master_chain(&mp);

    let mut touched = inserts.iter().filter(|c| !c.devices.is_empty()).count();
    if !master.devices.is_empty() {
        touched += 1;
    }
    module.channel_inserts = inserts;
    // Append (don't clobber any pre-existing master chain).
    module.master_chain.devices.extend(master.devices);
    touched
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::mix_plugin::MixPluginInfo;

    /// Build a synthetic DMO state blob (`[type=0][f32 params]`).
    fn dmo_blob(params: &[f32]) -> Vec<u8> {
        let mut v = 0u32.to_le_bytes().to_vec(); // type tag
        for &p in params {
            v.extend_from_slice(&p.to_le_bytes());
        }
        v
    }

    fn dmo_plugin(id2: u32, params: &[f32]) -> MixPlugin {
        MixPlugin {
            info: MixPluginInfo {
                id1: DXMO,
                id2,
                ..Default::default()
            },
            data: Some(dmo_blob(params)),
        }
    }

    #[test]
    fn decode_compressor_params() {
        // Threshold -20 dB, ratio 4:1 ⇒ x3 = 40/60, x4 = 3/99.
        let p = [0.5, 0.02, 0.0508, 40.0 / 60.0, 3.0 / 99.0, 0.25];
        let dev = device_from_mix_plugin(&dmo_plugin(ID_COMPRESSOR, &p)).unwrap();
        match dev {
            Device::Compressor {
                threshold_milli_db,
                ratio_q8_8,
                ..
            } => {
                assert!(
                    (threshold_milli_db + 20_000).abs() < 200,
                    "thr {threshold_milli_db}"
                );
                assert!(
                    (ratio_q8_8 as i32 - 4 * 256).abs() < 8,
                    "ratio {ratio_q8_8}"
                );
            }
            _ => panic!("expected Compressor"),
        }
    }

    #[test]
    fn decode_parameq_center_and_gain() {
        // Center 8000 Hz (x0 = (8000-80)/15920), +6 dB (x2 = 0.5 + 6/30).
        let p = [(8000.0 - 80.0) / 15920.0, 0.314, 0.5 + 6.0 / 30.0];
        let dev = device_from_mix_plugin(&dmo_plugin(ID_PARAMEQ, &p)).unwrap();
        match dev {
            Device::ParamEq {
                center_hz,
                gain_milli_db,
                ..
            } => {
                assert!((center_hz as i32 - 8000).abs() < 60, "center {center_hz}");
                assert!(
                    (gain_milli_db as i32 - 6000).abs() < 60,
                    "gain {gain_milli_db}"
                );
            }
            _ => panic!("expected ParamEq"),
        }
    }

    #[test]
    fn decode_echo_delays() {
        // Left 500 ms, right 250 ms ⇒ x = (ms-1)/1999.
        let p = [0.5, 0.5, 499.0 / 1999.0, 249.0 / 1999.0, 1.0];
        let dev = device_from_mix_plugin(&dmo_plugin(ID_ECHO, &p)).unwrap();
        match dev {
            Device::Echo {
                left_delay_ms,
                right_delay_ms,
                pan_delay,
                ..
            } => {
                assert!((left_delay_ms as i32 - 500).abs() <= 1);
                assert!((right_delay_ms as i32 - 250).abs() <= 1);
                assert!(pan_delay);
            }
            _ => panic!("expected Echo"),
        }
    }

    #[test]
    fn vst_chunk_blob_is_not_decoded_as_dmo() {
        // A non-zero `type` word (a VST 'fEvN' chunk shape) must be
        // rejected rather than mis-read as floats.
        let plug = MixPlugin {
            info: MixPluginInfo {
                id1: DXMO,
                id2: ID_ECHO,
                ..Default::default()
            },
            data: Some(b"fEvN\0\0\0\0".to_vec()),
        };
        assert!(device_from_mix_plugin(&plug).is_none());
    }

    #[test]
    fn vst_without_data_is_none() {
        // A VST slot with no parameter blob can't be decoded.
        let comp = MixPlugin {
            info: MixPluginInfo {
                id1: VSTP,
                id2: VST_JS_COMPRESSOR,
                ..Default::default()
            },
            data: None,
        };
        assert!(device_from_mix_plugin(&comp).is_none());
    }

    #[test]
    fn routing_one_plugin_per_channel() {
        let plugins = MixPlugins {
            channel_assignments: vec![0, 1, 0, 2],
            plugins: vec![
                dmo_plugin(ID_ECHO, &[0.5, 0.5, 0.25, 0.25, 0.0]),
                dmo_plugin(ID_PARAMEQ, &[0.5, 0.3, 0.5]),
            ],
        };
        let inserts = project_channel_inserts(&plugins, 4);
        assert!(inserts[0].devices.is_empty());
        assert_eq!(inserts[1].devices.len(), 1); // Echo on ch1
        assert!(inserts[2].devices.is_empty());
        assert_eq!(inserts[3].devices.len(), 1); // ParamEq on ch3
    }

    #[test]
    fn routing_follows_output_chain() {
        // Channel 0 → plugin 1, whose output routes to plugin 2 (0x80|1).
        let mut p0 = dmo_plugin(ID_ECHO, &[0.5, 0.5, 0.25, 0.25, 0.0]);
        p0.info.output_routing = 0x80 | 1; // → plugin index 1 (slot 2)
        let plugins = MixPlugins {
            channel_assignments: vec![1],
            plugins: vec![p0, dmo_plugin(ID_PARAMEQ, &[0.5, 0.3, 0.5])],
        };
        let inserts = project_channel_inserts(&plugins, 1);
        assert_eq!(inserts[0].devices.len(), 2, "serial chain not followed");
    }

    #[test]
    fn decode_gargle_rate_and_shape() {
        // Rate 200 Hz ⇒ x0 = (200-1)/999 ; square ⇒ x1 = 1.0.
        let p = [(200.0 - 1.0) / 999.0, 1.0];
        let dev = device_from_mix_plugin(&dmo_plugin(ID_GARGLE, &p)).unwrap();
        match dev {
            Device::Gargle {
                rate_hz,
                wave_square,
                ..
            } => {
                assert!((rate_hz as i32 - 200).abs() <= 1, "rate {rate_hz}");
                assert!(wave_square);
            }
            _ => panic!("expected Gargle"),
        }
    }

    #[test]
    fn decode_vst_freeverb_params() {
        // Freeverb2 stores 7 positional floats (type=0), like a DMO:
        // RoomSize, Damping, Predelay, Lowpass, Highpass, Wet, Dry.
        // The corpus value: a wet send (room 0.68, wet 0.25, dry 0).
        let p = [0.681, 0.5, 0.0, 1.0, 0.0, 0.25, 0.0];
        let plug = MixPlugin {
            info: MixPluginInfo {
                id1: VSTP,
                id2: VST_FREEVERB,
                ..Default::default()
            },
            data: Some(dmo_blob(&p)),
        };
        match device_from_mix_plugin(&plug).unwrap() {
            Device::Reverb {
                mode,
                room_size,
                wet,
                dry,
                ..
            } => {
                assert!(matches!(mode, ReverbMode::Freeverb));
                assert_eq!(room_size, q15(0.681)); // param 0
                assert_eq!(wet, q15(0.25)); // param 5
                assert_eq!(dry, Q15::ZERO); // param 6
            }
            _ => panic!("expected Reverb"),
        }
    }

    #[test]
    fn decode_vst_js_compresseur_params() {
        // [Gain In 0.25 (×4 = 0 dB), Threshold 0.5 (−6 dB), Gain Out
        // 0.2498 (≈ 0 dB)] ⇒ a soft limiter at −6 dB with neutral gains.
        let p = [0.25, 0.5, 0.2498];
        let plug = MixPlugin {
            info: MixPluginInfo {
                id1: VSTP,
                id2: VST_JS_COMPRESSOR,
                ..Default::default()
            },
            data: Some(dmo_blob(&p)),
        };
        match device_from_mix_plugin(&plug).unwrap() {
            Device::Compressor {
                threshold_milli_db,
                makeup_milli_db,
                ..
            } => {
                assert!(
                    (threshold_milli_db + 6_021).abs() < 300,
                    "thr {threshold_milli_db}"
                );
                assert!(makeup_milli_db.abs() < 300, "makeup {makeup_milli_db}");
            }
            _ => panic!("expected Compressor"),
        }
    }

    #[test]
    fn master_flagged_plugin_goes_to_master_chain() {
        let mut p = dmo_plugin(ID_COMPRESSOR, &[0.5, 0.02, 0.05, 0.5, 0.03, 0.25]);
        p.info.routing_flags = IR_APPLY_TO_MASTER;
        let plugins = MixPlugins {
            channel_assignments: vec![0, 0],
            plugins: vec![p],
        };
        // Not on any channel ⇒ no channel inserts...
        assert!(project_channel_inserts(&plugins, 2)
            .iter()
            .all(|c| c.devices.is_empty()));
        // ...but it lands on the master chain.
        let master = project_master_chain(&plugins);
        assert_eq!(master.devices.len(), 1);
    }

    #[test]
    fn bypassed_plugin_is_skipped() {
        let mut p = dmo_plugin(ID_ECHO, &[0.5, 0.5, 0.25, 0.25, 0.0]);
        p.info.routing_flags = IR_BYPASS;
        let plugins = MixPlugins {
            channel_assignments: vec![1],
            plugins: vec![p],
        };
        let inserts = project_channel_inserts(&plugins, 1);
        assert!(inserts[0].devices.is_empty());
    }
}
