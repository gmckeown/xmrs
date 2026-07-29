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

use alloc::string::String;
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

/// How a known plugin decodes: how many normalised floats it stores, and
/// the function turning them into a native device.
type PluginDecoder = (usize, fn(&[f32; 13]) -> Device);

/// How many parameter floats a known plugin stores, and the function that
/// turns them into a native device — the **one** identity table.
///
/// Kept separate from the decode so that "do I know this plugin?" and "did
/// its blob decode?" are answerable independently: the first is what a
/// future build can change (a wider census), the second is a property of
/// the file. [`inspect_mix_plugins`] reports them as distinct reasons and
/// would drift from reality if it carried its own copy of the ids.
fn known_decoder(id1: u32, id2: u32) -> Option<PluginDecoder> {
    let entry: PluginDecoder = match (id1, id2) {
        (DXMO, ID_COMPRESSOR) => (6, dmo_compressor),
        (DXMO, ID_DISTORTION) => (5, dmo_distortion),
        (DXMO, ID_ECHO) => (5, dmo_echo),
        (DXMO, ID_CHORUS) => (7, dmo_chorus),
        (DXMO, ID_FLANGER) => (7, dmo_flanger),
        (DXMO, ID_PARAMEQ) => (3, dmo_parameq),
        (DXMO, ID_WAVESREVERB) => (4, dmo_wavesreverb),
        (DXMO, ID_I3DL2REVERB) => (13, dmo_i3dl2),
        (DXMO, ID_GARGLE) => (2, dmo_gargle),
        // Freeverb2 and JS Compresseur save their params positionally
        // (type=0) like a DMO, so they decode rather than get substituted.
        (VSTP, VST_FREEVERB) => (7, vst_freeverb),
        (VSTP, VST_JS_COMPRESSOR) => (3, vst_js_compresseur),
        _ => return None,
    };
    Some(entry)
}

/// Decode a single mix-plugin slot into a native [`Device`], or `None`
/// when the slot is empty or an unrecognised plugin. All known plugins —
/// the 9 DMOs and both census VSTs (**Freeverb2** and **JS Compresseur**,
/// whose params turned out to be plain positional float arrays, the
/// orderings reverse-engineered from their binaries in Ghidra) — are
/// decoded from their actual stored settings.
///
/// A `None` here is not always the same failure; [`inspect_mix_plugins`]
/// says which one it was.
pub fn device_from_mix_plugin(plug: &MixPlugin) -> Option<Device> {
    let (nparams, build) = known_decoder(plug.info.id1, plug.info.id2)?;
    let data = plug.data.as_ref()?;
    read_dmo_floats(data, nparams).map(|p| build(&p))
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
    for_each_channel_slot(plugins, num_channels, |ch, _slot1, plug| {
        if plug.info.routing_flags & IR_BYPASS == 0 {
            if let Some(dev) = device_from_mix_plugin(plug) {
                out[ch].devices.push(dev);
            }
        }
    });
    out
}

/// Walk the per-channel routing chains, calling `f(channel, slot1, plugin)`
/// on every slot reached — the traversal the projection and the report must
/// agree on. Each channel starts fresh (a plugin shared by two channels is
/// visited twice); the 64-step guard is what stops a routing cycle.
fn for_each_channel_slot(
    plugins: &MixPlugins,
    num_channels: usize,
    mut f: impl FnMut(usize, usize, &MixPlugin),
) {
    let n_plugins = plugins.plugins.len();
    for ch in 0..num_channels {
        let mut slot1 = plugins.channel_assignments.get(ch).copied().unwrap_or(0) as usize;
        let mut guard = 0;
        while slot1 >= 1 && slot1 <= n_plugins && guard < 64 {
            let plug = &plugins.plugins[slot1 - 1];
            f(ch, slot1, plug);
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
}

/// Project the **master**-mix plugins (those flagged `irApplyToMaster`)
/// into a single serial chain, following their `output_routing` links.
/// Each plugin is added at most once.
pub fn project_master_chain(plugins: &MixPlugins) -> DeviceChain {
    let mut chain = DeviceChain::default();
    for_each_master_slot(plugins, |_slot1, plug| {
        if plug.info.routing_flags & IR_BYPASS == 0 {
            if let Some(dev) = device_from_mix_plugin(plug) {
                chain.devices.push(dev);
            }
        }
    });
    chain
}

/// Walk the master-flagged routing chains, calling `f(slot1, plugin)` on
/// every slot reached. Unlike the per-channel walk each slot is visited at
/// most once, so a plugin chained into by two master roots is added once.
fn for_each_master_slot(plugins: &MixPlugins, mut f: impl FnMut(usize, &MixPlugin)) {
    let n = plugins.plugins.len();
    let mut visited = vec![false; n];
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
            f(slot1, plug);
            let out_r = plug.info.output_routing;
            if out_r & 0x80 != 0 {
                slot1 = (out_r & 0x7F) as usize + 1;
            } else {
                break;
            }
            guard += 1;
        }
    }
}

/// Why a plugin slot produced no native [`Device`].
///
/// The first two are verdicts on the **plugin**, the last two on the
/// **file's routing** — and the distinction is the useful one, because
/// only the first is something a later xmrs can change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginSkip {
    /// The `(id1, id2)` pair is outside this build's table. **The one a
    /// wider census fixes** — the plugin is perfectly convertible, this
    /// build just does not know it yet.
    UnknownPlugin,
    /// A plugin this build *does* know, whose parameter blob is missing,
    /// truncated, or not a positional float array (an opaque VST chunk
    /// dump). No table will rescue it: the settings are not in the file in
    /// a form anything but the original plugin can read.
    UndecodableBlob,
    /// Convertible and routed, but flagged `irBypass` — the author had it
    /// switched off, so it is deliberately not projected. Audibly nothing
    /// is lost; what is lost is the ability to switch it back on.
    Bypassed,
    /// An orphan slot: present in the table but neither assigned to a
    /// channel nor master-flagged, so no routing walk ever reaches it.
    Unrouted,
}

/// One plugin slot that did not become a native [`Device`].
///
/// Carries the identity a census needs — note that
/// [`shell_plugin_id`](crate::tracker::mix_plugin::MixPluginInfo::shell_plugin_id)
/// is part of it: keyed on the id pair alone, every effect inside one shell
/// DLL collapses to a single entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedPlugin {
    /// Slot number as IT addresses it (1-based).
    pub slot: usize,
    pub id1: u32,
    pub id2: u32,
    pub shell_plugin_id: u32,
    /// Author-chosen display name (`szName`).
    pub name: String,
    /// Library / DLL name (`szLibraryName`).
    pub library_name: String,
    pub reason: PluginSkip,
}

/// What a mix-plugin projection did, and what it could not do.
///
/// The reason this exists at all: `mix_plugins` is **not** archived by the
/// native format (`FORMAT_RFC.md` §3.1) and an unconverted slot leaves no
/// trace in `GRAF` either, so saving a module whose report is not
/// [`is_complete`](Self::is_complete) discards those plugins for good — a
/// later build with a wider table has nothing left to re-project. This
/// turns that from a silent loss into an announced one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginReport {
    /// How many chains (channels + the master chain) received at least one
    /// device.
    pub chains_touched: usize,
    /// Every occupied slot that produced no device, in slot order.
    pub skipped: Vec<SkippedPlugin>,
}

impl PluginReport {
    /// True when every occupied slot became a native device.
    pub fn is_complete(&self) -> bool {
        self.skipped.is_empty()
    }

    /// The slots a **wider census would rescue** — those skipped for
    /// [`PluginSkip::UnknownPlugin`]. Feed these to a corpus sweep; the
    /// other reasons are properties of the file, not of this build.
    pub fn unrecognised(&self) -> impl Iterator<Item = &SkippedPlugin> {
        self.skipped
            .iter()
            .filter(|s| s.reason == PluginSkip::UnknownPlugin)
    }
}

/// A slot is *occupied* when it names a plugin or carries data; the IT
/// table is a fixed 64 entries, most of them blank.
fn slot_is_occupied(p: &MixPlugin) -> bool {
    p.info.id1 != 0 || p.info.id2 != 0 || p.data.is_some()
}

/// Examine a module's mix-plugin table **without touching it**: which slots
/// convert to native devices and which do not, with a reason each.
///
/// Read-only, and independent of whether the projection has already run —
/// the importer preserves `Module::mix_plugins`, so this answers the same
/// question before or after. Two uses: checking what a save to `.xmr` is
/// about to discard, and sweeping a corpus for plugin ids worth adding to
/// the table (see [`PluginReport::unrecognised`]).
///
/// A module with no plugin table reports nothing skipped.
pub fn inspect_mix_plugins(module: &Module) -> PluginReport {
    let Some(mp) = module.mix_plugins.as_ref() else {
        return PluginReport::default();
    };
    let num_channels = module.get_num_channels();

    // Routing reachability, from the same walks the projection uses.
    let mut reached = vec![false; mp.plugins.len()];
    for_each_channel_slot(mp, num_channels, |_ch, slot1, _p| reached[slot1 - 1] = true);
    for_each_master_slot(mp, |slot1, _p| reached[slot1 - 1] = true);

    let inserts = project_channel_inserts(mp, num_channels);
    let master = project_master_chain(mp);
    let mut chains_touched = inserts.iter().filter(|c| !c.devices.is_empty()).count();
    if !master.devices.is_empty() {
        chains_touched += 1;
    }

    let mut skipped = Vec::new();
    for (i, plug) in mp.plugins.iter().enumerate() {
        if !slot_is_occupied(plug) {
            continue;
        }
        // Identity first, routing second: a build can only ever change the
        // identity verdict, so that is the one worth surfacing when a slot
        // fails both ways.
        let reason = if known_decoder(plug.info.id1, plug.info.id2).is_none() {
            PluginSkip::UnknownPlugin
        } else if device_from_mix_plugin(plug).is_none() {
            PluginSkip::UndecodableBlob
        } else if plug.info.routing_flags & IR_BYPASS != 0 {
            PluginSkip::Bypassed
        } else if !reached[i] {
            PluginSkip::Unrouted
        } else {
            continue; // converted
        };
        skipped.push(SkippedPlugin {
            slot: i + 1,
            id1: plug.info.id1,
            id2: plug.info.id2,
            shell_plugin_id: plug.info.shell_plugin_id,
            name: plug.info.name.clone(),
            library_name: plug.info.library_name.clone(),
            reason,
        });
    }

    PluginReport {
        chains_touched,
        skipped,
    }
}

/// Decode `module.mix_plugins` into native device chains — per-channel
/// inserts on `module.channel_inserts` and any master-flagged plugins onto
/// `module.master_chain` — and report what did **not** convert.
///
/// The IT importer runs this itself at the end of `to_module`, so a
/// plugin-bearing module arrives already rendering its effects; it is
/// public so a caller can re-run or inspect the projection. A module with
/// no `mix_plugins` is left untouched (and the render stays bit-identical
/// to the pre-conversion player). `mix_plugins` is preserved either way.
///
/// The importer discards the returned report — call
/// [`inspect_mix_plugins`] on the imported module to get it back, in
/// particular before writing the module to `.xmr`, which drops the raw
/// plugin table (`FORMAT_RFC.md` §3.1).
pub fn convert_mix_plugins_to_devices(module: &mut Module) -> PluginReport {
    let Some(mp) = module.mix_plugins.clone() else {
        return PluginReport::default();
    };
    let report = inspect_mix_plugins(module);
    let num_channels = module.get_num_channels();
    module.channel_inserts = project_channel_inserts(&mp, num_channels);
    // Append (don't clobber any pre-existing master chain).
    module
        .master_chain
        .devices
        .extend(project_master_chain(&mp).devices);
    report
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

    // --- the loss report -------------------------------------------------

    /// A module carrying `plugins` and nothing else. `Module::default()`
    /// has no clips, hence zero channels, so a slot is reached only by the
    /// master flag — which is what isolates the routing reasons.
    fn module_with(plugins: Vec<MixPlugin>) -> Module {
        Module {
            mix_plugins: Some(MixPlugins {
                channel_assignments: Vec::new(),
                plugins,
            }),
            ..Module::default()
        }
    }

    /// Master-flag a slot so the routing walk reaches it.
    fn on_master(mut p: MixPlugin) -> MixPlugin {
        p.info.routing_flags |= IR_APPLY_TO_MASTER;
        p
    }

    #[test]
    fn report_is_clean_when_everything_converts() {
        let m = module_with(vec![on_master(dmo_plugin(
            ID_ECHO,
            &[0.5, 0.5, 0.25, 0.25, 0.0],
        ))]);
        let r = inspect_mix_plugins(&m);
        assert!(r.is_complete(), "{:?}", r.skipped);
        assert_eq!(r.chains_touched, 1);
    }

    #[test]
    fn report_names_an_unknown_plugin_for_the_census() {
        // A VST this build has no entry for: convertible in principle,
        // just absent from the table — the one reason a wider census fixes.
        let plug = on_master(MixPlugin {
            info: MixPluginInfo {
                id1: VSTP,
                id2: 0x1234_5678,
                shell_plugin_id: 7,
                name: "My Chorus".into(),
                library_name: "shell.dll".into(),
                ..Default::default()
            },
            data: Some(dmo_blob(&[0.5])),
        });
        let r = inspect_mix_plugins(&module_with(vec![plug]));
        assert!(!r.is_complete());
        let s = &r.skipped[0];
        assert_eq!(s.reason, PluginSkip::UnknownPlugin);
        assert_eq!((s.slot, s.id2, s.shell_plugin_id), (1, 0x1234_5678, 7));
        assert_eq!(s.library_name, "shell.dll");
        assert_eq!(r.unrecognised().count(), 1);
    }

    #[test]
    fn report_separates_an_undecodable_blob_from_an_unknown_id() {
        // A plugin this build *knows*, carrying an opaque VST chunk dump
        // instead of positional floats. No table will ever rescue it, so it
        // must not show up as census material.
        let plug = on_master(MixPlugin {
            info: MixPluginInfo {
                id1: DXMO,
                id2: ID_ECHO,
                ..Default::default()
            },
            data: Some(b"fEvN\0\0\0\0".to_vec()),
        });
        let r = inspect_mix_plugins(&module_with(vec![plug]));
        assert_eq!(r.skipped[0].reason, PluginSkip::UndecodableBlob);
        assert_eq!(r.unrecognised().count(), 0);
    }

    #[test]
    fn report_distinguishes_bypassed_from_unrouted() {
        let mut bypassed = on_master(dmo_plugin(ID_ECHO, &[0.5, 0.5, 0.25, 0.25, 0.0]));
        bypassed.info.routing_flags |= IR_BYPASS;
        // Second slot: decodable, but on no channel and not master-flagged.
        let orphan = dmo_plugin(ID_PARAMEQ, &[0.5, 0.3, 0.5]);
        let r = inspect_mix_plugins(&module_with(vec![bypassed, orphan]));
        assert_eq!(r.skipped.len(), 2);
        assert_eq!(r.skipped[0].reason, PluginSkip::Bypassed);
        assert_eq!(r.skipped[1].reason, PluginSkip::Unrouted);
        assert_eq!(r.chains_touched, 0);
    }

    #[test]
    fn report_ignores_the_blank_slots_of_the_fixed_table() {
        // The IT table is 64 fixed entries; blank ones are not losses.
        let mut plugins = vec![MixPlugin::default(); 64];
        plugins[0] = on_master(dmo_plugin(ID_PARAMEQ, &[0.5, 0.3, 0.5]));
        let r = inspect_mix_plugins(&module_with(plugins));
        assert!(r.is_complete(), "{:?}", r.skipped);
    }

    #[test]
    fn inspect_agrees_with_conversion_and_leaves_the_table_alone() {
        let mut m = module_with(vec![
            on_master(dmo_plugin(ID_ECHO, &[0.5, 0.5, 0.25, 0.25, 0.0])),
            dmo_plugin(ID_PARAMEQ, &[0.5, 0.3, 0.5]), // orphan
        ]);
        let before = inspect_mix_plugins(&m);
        let returned = convert_mix_plugins_to_devices(&mut m);
        assert_eq!(before, returned);
        // The raw table survives the projection, so the answer is stable.
        assert!(m.mix_plugins.is_some());
        assert_eq!(inspect_mix_plugins(&m), returned);
        assert_eq!(m.master_chain.devices.len(), 1);
    }
}
