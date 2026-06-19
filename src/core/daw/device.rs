//! RFC §3B — native signal-graph building blocks: insert **devices**,
//! ordered **chains**, and aux/return **bus** routing.
//!
//! This is the *plumbing* half of the signal graph. A [`DeviceChain`]
//! is an ordered, bypassable list of inserts processing one mixer
//! channel's stereo output; [`Bus`] / [`Send`] route post-insert
//! signal into aux/return buses. Rich DSP content (EQ, reverb,
//! compressor, …) is out of this RFC's scope (§6.2 — "3B ne livre que
//! la plomberie, pas le contenu"); [`Device`] is the typed slot those
//! will extend. The built-ins today are a stateless linear
//! [`Device::Gain`], a one-pole [`Device::LowPass`], and the native
//! fixed-point effects [`Device::ParamEq`] and [`Device::Distortion`]
//! (RBJ biquad / waveshaper built on [`crate::core::daw::dsp`]). Devices
//! may carry per-channel state — [`Device::process`] takes `&mut self` —
//! and time-based devices precompute coefficients in [`Device::prepare`].
//!
//! It is deliberately **separate** from [`crate::tracker::mix_plugin`]
//! (`MixPlugins`): those are opaque, host-dependent VST chunks the
//! player can't execute, preserved only for round-trip. This chain is
//! native and player-processable.
//!
//! **Compatibility.** Every tracker import leaves the chains empty, so
//! the mixer skips them ([`DeviceChain::is_active`] is `false`) and the
//! render is bit-identical to the pre-3B player. A `DeviceChain` is a
//! pure pass-through when empty or bypassed.

use serde::{Deserialize, Serialize};

use alloc::vec::Vec;

use crate::core::daw::dsp::{self, Biquad};
use crate::core::fixed::fixed::Q15;
use crate::core::fixed::units::{Amp, Volume};

/// Map a normalized Q1.15 automation value (treated as `[0, 1]`,
/// negatives clamped) onto an inclusive integer range `[lo, hi]`.
#[inline]
fn lerp_param(value: Q15, lo: i32, hi: i32) -> i32 {
    let frac = value.raw().max(0) as i64; // 0..=Q15::ONE
    lo + (((hi - lo) as i64 * frac) / Q15::ONE.raw() as i64) as i32
}

/// Recompute the peaking-EQ biquad from its parameters (no-op until the
/// output rate is known).
fn recompute_param_eq(
    eq: &mut Biquad,
    rate_hz: u32,
    center_hz: u16,
    bandwidth_semitones: u8,
    gain_milli_db: i16,
) {
    if rate_hz == 0 {
        return;
    }
    // Bandwidth: semitones → octaves (Q16.16) = semitones/12.
    let bw_oct_q16_16 = ((bandwidth_semitones.max(1) as i64) * 65_536) / 12;
    eq.set_peaking(
        rate_hz,
        center_hz as u32,
        bw_oct_q16_16.max(1),
        gain_milli_db as i32,
    );
}

/// Recompute the distortion's pre/post filters and output gain.
#[allow(clippy::too_many_arguments)]
fn recompute_distortion(
    pre_lp: &mut Biquad,
    post_eq: &mut Biquad,
    out_gain_q15: &mut i32,
    rate_hz: u32,
    gain_milli_db: i32,
    post_eq_center_hz: u16,
    post_eq_bandwidth_hz: u16,
    pre_lowpass_hz: u16,
) {
    if rate_hz == 0 {
        return;
    }
    pre_lp.set_lowpass(
        rate_hz,
        pre_lowpass_hz as u32,
        dsp::TWO_Q_BUTTERWORTH_Q16_16,
    );
    // Post filter as a resonant low-pass: Q ≈ centre / bandwidth.
    let q_q16_16 = ((post_eq_center_hz.max(1) as i64) << 16) / post_eq_bandwidth_hz.max(1) as i64;
    post_eq.set_lowpass(rate_hz, post_eq_center_hz as u32, (2 * q_q16_16).max(1));
    // Output level (≤ 0 dB): Q16.16 linear → Q1.15 (>> 1), clamp.
    let lin = dsp::db_milli_to_lin_q16_16(gain_milli_db);
    *out_gain_q15 = (lin >> 1).clamp(0, Q15::ONE.raw() as i64) as i32;
}

/// Recompute the compressor's detector times and derived log-domain
/// constants (threshold, slope, make-up).
#[allow(clippy::too_many_arguments)]
fn recompute_compressor(
    env: &mut dsp::EnvelopeFollower,
    thresh_oct_q16_16: &mut i32,
    slope_q16_16: &mut i32,
    makeup_oct_q16_16: &mut i32,
    rate_hz: u32,
    threshold_milli_db: i32,
    ratio_q8_8: u16,
    attack_us: u32,
    release_us: u32,
    makeup_milli_db: i32,
) {
    if rate_hz == 0 {
        return;
    }
    env.set_times(rate_hz, attack_us, release_us);
    *thresh_oct_q16_16 = dsp::milli_db_to_oct_q16_16(threshold_milli_db);
    *makeup_oct_q16_16 = dsp::milli_db_to_oct_q16_16(makeup_milli_db);
    // 1/ratio in Q16.16 = (65536·256)/ratio_q8_8 ; slope = 1/ratio − 1.
    let inv_ratio = (16_777_216i64 / ratio_q8_8.max(1) as i64) as i32;
    *slope_q16_16 = inv_ratio - 65_536;
}

/// Topology preset for [`Device::ModDelay`]. Chorus and flanger share
/// one LFO-modulated fractional-delay engine and differ only in their
/// typical delay range and feedback usage.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModDelayMode {
    /// Short base delay (≈ 0–4 ms), feedback creates the swept comb notch.
    Flanger,
    /// Longer base delay (≈ 0–20 ms), little/no feedback, thicker voice.
    Chorus,
}

/// Tuning preset for [`Device::Reverb`]. Both share one Schroeder–Moorer
/// engine; the mode selects the room-size → feedback mapping (WavesReverb
/// reaches longer tails).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReverbMode {
    /// Jezar's Freeverb tuning / feedback curve.
    Freeverb,
    /// Longer-tail mapping approximating the DirectX `DSFXWavesReverb`.
    WavesReverb,
}

/// A built-in mixer insert.
///
/// Devices may be **stateful**: [`Device::process`] takes `&mut self`,
/// so a device carries its own per-channel state across samples (e.g.
/// a filter's previous output). That state is runtime-only — it lives
/// in the player's runtime chain clone and is `#[serde(skip)]`'d so the
/// serialised model holds parameters, not transient signal history.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Device {
    /// Linear gain, Q1.15 in `[0, 1]` (an attenuator —
    /// [`Volume::FULL`] is unity and leaves the signal untouched).
    /// Stateless.
    Gain(Volume),
    /// One-pole low-pass filter (a stateful built-in, mostly to
    /// exercise the `&mut self` processing path). `cutoff` is the
    /// per-sample smoothing coefficient `a` in Q1.15 `[0, 1]`:
    /// `y[n] = y[n-1] + a·(x[n] − y[n-1])`. `a = 1` (`Q15::ONE`) is a
    /// pass-through; smaller `a` rolls off more high end. `state` is
    /// the previous output per channel (runtime-only).
    LowPass {
        cutoff: Q15,
        #[serde(skip)]
        state: (i32, i32),
    },
    /// RBJ **peaking equaliser** (native equivalent of the DirectX
    /// `DSFXParamEq` DMO). A boost/cut at `center_hz` with a
    /// `bandwidth_semitones`-wide skirt. The biquad coefficients and
    /// history are runtime-only ([`Device::prepare`] derives them from
    /// the output rate). Inserted into an empty chain it is the only
    /// device, so an un-prepared instance is the identity pass-through.
    ParamEq {
        /// Centre frequency in Hz (DMO range 80–16000).
        center_hz: u16,
        /// Bandwidth in semitones (DMO range 1–36).
        bandwidth_semitones: u8,
        /// Peak gain in **milli-decibels** (dB × 1000), signed
        /// (DMO range −15000…+15000).
        gain_milli_db: i16,
        #[serde(skip)]
        rate_hz: u32,
        #[serde(skip)]
        eq: Biquad,
    },
    /// **Distortion** (native equivalent of the DirectX `DSFXDistortion`
    /// DMO): an optional pre-low-pass, a memoryless cubic soft-clip whose
    /// drive is set by `edge`, a resonant post-low-pass that tames the
    /// clipping harmonics, and an output level. Filters/coefficients are
    /// runtime-only; an un-prepared instance passes through.
    Distortion {
        /// Output level in **milli-decibels** (DMO `Gain`, −60000…0).
        gain_milli_db: i32,
        /// Distortion amount (DMO `Edge`, 0–100 %) as Q1.15 `[0, 1]`.
        edge: Q15,
        /// Post-EQ centre / resonant low-pass cutoff in Hz (100–8000).
        post_eq_center_hz: u16,
        /// Post-EQ bandwidth in Hz (100–8000); sets the post filter `Q`.
        post_eq_bandwidth_hz: u16,
        /// Pre-low-pass cutoff in Hz (100–8000).
        pre_lowpass_hz: u16,
        #[serde(skip)]
        rate_hz: u32,
        #[serde(skip)]
        pre_lp: Biquad,
        #[serde(skip)]
        post_eq: Biquad,
        /// Derived output gain, Q1.15.
        #[serde(skip)]
        out_gain_q15: i32,
    },
    /// **Compressor** (native equivalent of the DirectX `DSFXCompressor`
    /// DMO; also the substitute for the "JS Compresseur" VST): a
    /// stereo-linked peak [`crate::core::daw::dsp::EnvelopeFollower`]
    /// feeding a log-domain gain computer (hard knee). Above `threshold`
    /// the gain falls at `(1/ratio − 1)`; `makeup` then lifts the whole
    /// signal. Detector state is runtime-only; an un-prepared instance
    /// passes through.
    Compressor {
        /// Threshold in **milli-decibels** (DMO `Threshold`, −60000…0).
        threshold_milli_db: i32,
        /// Ratio as Q8.8 (`256` = 1:1, `25600` = 100:1; DMO `Ratio`).
        ratio_q8_8: u16,
        /// Attack time in microseconds (DMO `Attack`, 10…500000).
        attack_us: u32,
        /// Release time in microseconds (DMO `Release`, 50000…3000000).
        release_us: u32,
        /// Make-up / output gain in milli-dB (DMO `Gain`, −60000…+60000).
        makeup_milli_db: i32,
        #[serde(skip)]
        rate_hz: u32,
        #[serde(skip)]
        env: dsp::EnvelopeFollower,
        /// Derived: threshold as octaves (log2 linear), Q16.16.
        #[serde(skip)]
        thresh_oct_q16_16: i32,
        /// Derived: `(1/ratio − 1)`, Q16.16 (≤ 0).
        #[serde(skip)]
        slope_q16_16: i32,
        /// Derived: make-up gain as octaves, Q16.16.
        #[serde(skip)]
        makeup_oct_q16_16: i32,
    },
    /// **Echo / delay** (native equivalent of the DirectX `DSFXEcho`
    /// DMO): independent left/right delay taps with feedback and a
    /// wet/dry mix. The recirculating path is held in Q15.16 (16
    /// fractional bits) so long tails decay smoothly. Buffers are
    /// allocated in [`Device::prepare`]; un-prepared ⇒ pass-through.
    Echo {
        /// Wet/dry mix as Q1.15 (`0` = dry, `1` = fully wet; DMO
        /// `WetDryMix` 0–100 %).
        wet_dry: Q15,
        /// Feedback amount as Q1.15 `[0, 1)` (DMO `Feedback` 0–100 %).
        feedback: Q15,
        /// Left delay in milliseconds (DMO `LeftDelay`, 1–2000).
        left_delay_ms: u16,
        /// Right delay in milliseconds (DMO `RightDelay`, 1–2000).
        right_delay_ms: u16,
        /// Swap L/R on each successive echo (DMO `PanDelay`).
        pan_delay: bool,
        #[serde(skip)]
        rate_hz: u32,
        #[serde(skip)]
        dl_l: dsp::DelayLine,
        #[serde(skip)]
        dl_r: dsp::DelayLine,
    },
    /// **Modulated delay** — chorus or flanger (native equivalent of the
    /// DirectX `DSFXChorus` / `DSFXFlanger` DMOs, which share a shape). An
    /// LFO sweeps a short fractional delay; feedback (signed, for the
    /// flanger comb) and a wet/dry mix complete it. Buffers/LFO state are
    /// runtime-only; un-prepared ⇒ pass-through.
    ModDelay {
        mode: ModDelayMode,
        /// Wet/dry mix, Q1.15 (DMO `WetDryMix`).
        wet_dry: Q15,
        /// Modulation depth, Q1.15 `[0, 1]` (DMO `Depth`).
        depth: Q15,
        /// Feedback, **signed** Q1.15 `(-1, 1)` (DMO `Feedback`; the
        /// flanger uses negative values for its characteristic notch).
        feedback: Q15,
        /// LFO rate in millihertz (DMO `Frequency`, 0–10000).
        frequency_milli_hz: u32,
        /// LFO shape: sine (`true`) or triangle (`false`) (DMO `Waveform`).
        waveform_sine: bool,
        /// Base (centre) delay in milliseconds, Q8.8 (DMO `Delay`).
        base_delay_ms_q8_8: u16,
        /// 90° L/R LFO offset for stereo width (DMO `Phase`).
        quadrature: bool,
        #[serde(skip)]
        rate_hz: u32,
        #[serde(skip)]
        dl_l: dsp::DelayLine,
        #[serde(skip)]
        dl_r: dsp::DelayLine,
        #[serde(skip)]
        lfo_phase: u32,
        #[serde(skip)]
        lfo_inc: u32,
    },
    /// **Reverb** (native equivalent of the DirectX `DSFXWavesReverb` and
    /// of the Freeverb VST) — a Schroeder–Moorer network (8 parallel
    /// damped combs + 4 series all-passes per channel) built on
    /// [`crate::core::daw::dsp::Comb`] / [`crate::core::daw::dsp::Allpass`].
    /// Banks/state are runtime-only; un-prepared ⇒ pass-through.
    Reverb {
        mode: ReverbMode,
        /// Room size / decay (Q1.15) — larger ⇒ longer tail.
        room_size: Q15,
        /// High-frequency damping (Q1.15) — larger ⇒ darker tail.
        damping: Q15,
        /// Stereo width (Q1.15).
        width: Q15,
        /// Wet (reverb) level, Q1.15. Scaled by Freeverb's `scalewet = 3`
        /// internally, so it is an independent gain (not a wet/dry
        /// crossfade) that can reach 3×.
        wet: Q15,
        /// Dry (direct) level, Q1.15. Scaled by `scaledry = 2` internally.
        dry: Q15,
        /// Freeze: hold the tail with unit feedback and no damping while
        /// muting the input (Freeverb's `freezemode`) — an infinite,
        /// glassy sustain.
        freeze: bool,
        #[serde(skip)]
        rate_hz: u32,
        #[serde(skip)]
        combs_l: Vec<dsp::Comb>,
        #[serde(skip)]
        combs_r: Vec<dsp::Comb>,
        #[serde(skip)]
        ap_l: Vec<dsp::Allpass>,
        #[serde(skip)]
        ap_r: Vec<dsp::Allpass>,
    },
    /// **I3DL2 environmental reverb** (native approximation of the
    /// DirectX `DSFXI3DL2Reverb` DMO) — an early-reflection tap network
    /// summed with the Freeverb late engine, driven by the audibly
    /// dominant I3DL2 parameters. This is the C1-engine approximation the
    /// plan permits (a single corpus file uses the real DMO); it is *not*
    /// a full 12-parameter environmental model. Runtime banks only ⇒
    /// un-prepared passes through.
    I3DL2Reverb {
        /// Overall reverb level in milli-dB (DMO `Room`, −10000…0).
        room_milli_db: i16,
        /// Late-decay time in ms (DMO `DecayTime`, 100…20000).
        decay_time_ms: u16,
        /// HF decay ratio ×1000 (DMO `DecayHFRatio`, 100…2000 = 0.1…2.0).
        decay_hf_milli: u16,
        /// Early-reflections level in milli-dB (DMO `Reflections`).
        reflections_milli_db: i16,
        /// Early-reflections pre-delay in ms (DMO `ReflectionsDelay`,
        /// 0…300).
        reflections_delay_ms: u16,
        /// Late-reverb level in milli-dB (DMO `Reverb`).
        reverb_milli_db: i16,
        /// Late-reverb pre-delay in ms (DMO `ReverbDelay`, 0…100).
        reverb_delay_ms: u16,
        #[serde(skip)]
        rate_hz: u32,
        #[serde(skip)]
        early_l: dsp::DelayLine,
        #[serde(skip)]
        early_r: dsp::DelayLine,
        #[serde(skip)]
        combs_l: Vec<dsp::Comb>,
        #[serde(skip)]
        combs_r: Vec<dsp::Comb>,
        #[serde(skip)]
        ap_l: Vec<dsp::Allpass>,
        #[serde(skip)]
        ap_r: Vec<dsp::Allpass>,
        /// Derived linear gains (Q1.15): early, late, overall room.
        #[serde(skip)]
        early_gain_q15: i32,
        #[serde(skip)]
        reverb_gain_q15: i32,
        #[serde(skip)]
        room_gain_q15: i32,
    },
    /// **Gargle** (native equivalent of the DirectX `DSFXGargle` DMO): a
    /// periodic **amplitude modulation** — the signal is multiplied by a
    /// unipolar `[0, 1]` triangle (ramp up then down) or square (on/off)
    /// wave at `rate_hz`. A coarse, "gargling" tremolo. The cycle counter
    /// is runtime-only; un-prepared ⇒ pass-through.
    Gargle {
        /// Modulation rate in Hz (DMO `Rate`, 1–1000).
        rate_hz: u16,
        /// Waveform: `false` = triangle, `true` = square (DMO `WaveShape`).
        wave_square: bool,
        #[serde(skip)]
        sample_rate: u32,
        /// Cycle length in frames (derived: `sample_rate / rate_hz`, ≥ 2).
        #[serde(skip)]
        period: u32,
        #[serde(skip)]
        counter: u32,
    },
}

/// Recompute an I3DL2 reverb's late-engine coefficients and derived
/// linear gains from its parameters.
#[allow(clippy::too_many_arguments)]
fn recompute_i3dl2(
    decay_time_ms: u16,
    decay_hf_milli: u16,
    reflections_milli_db: i16,
    reverb_milli_db: i16,
    room_milli_db: i16,
    combs_l: &mut [dsp::Comb],
    combs_r: &mut [dsp::Comb],
    early_gain_q15: &mut i32,
    reverb_gain_q15: &mut i32,
    room_gain_q15: &mut i32,
) {
    let fb = i3dl2_feedback_q15(decay_time_ms);
    let damp = i3dl2_damping_q15(decay_hf_milli);
    for c in combs_l.iter_mut().chain(combs_r.iter_mut()) {
        c.set_feedback(fb);
        c.set_damp(damp);
    }
    // milli-dB (≤ 0) → Q1.15 linear gain.
    let to_gain = |mdb: i16| -> i32 {
        (dsp::db_milli_to_lin_q16_16(mdb as i32) >> 1).clamp(0, Q15::ONE.raw() as i64) as i32
    };
    *early_gain_q15 = to_gain(reflections_milli_db);
    *reverb_gain_q15 = to_gain(reverb_milli_db);
    *room_gain_q15 = to_gain(room_milli_db);
}

/// Maximum modulated-delay time the buffers are sized for (covers chorus
/// base delay plus full sweep).
const MODDELAY_MAX_MS: u64 = 50;

// --- Freeverb tuning (canonical comb/all-pass lengths in samples,
// measured at 44100 Hz in `tuning.h`; scaled to the run rate) ----------
const FREEVERB_COMB_TUNING: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const FREEVERB_ALLPASS_TUNING: [usize; 4] = [556, 441, 341, 225];
const FREEVERB_STEREO_SPREAD: usize = 23;
/// The sample rate (Hz) at which the comb/all-pass lengths above were
/// tuned; lengths are rescaled from this to the actual output rate.
const FREEVERB_TUNING_SAMPLE_RATE_HZ: u64 = 44_100;

/// Freeverb input attenuation (`fixedgain = 0.015`), Q1.15.
const REVERB_FIXED_GAIN_Q15: i64 = dsp::q15_ratio(15, 1000) as i64;
/// Freeverb `scalewet` — the wet level can reach 3×.
const REVERB_SCALEWET: i64 = 3;
/// Freeverb `scaledry` — the dry level can reach 2×.
const REVERB_SCALEDRY: i64 = 2;
/// Comb feedback for a **frozen** tail: unit gain (`1.0`) so the
/// recirculating signal neither grows nor decays (Freeverb `roomsize1 = 1`).
const REVERB_FREEZE_FEEDBACK: i32 = dsp::q15_ratio(1, 1);

/// Freeverb room-size mapping: `feedback = room·scaleroom + offsetroom`.
const FREEVERB_SCALEROOM_Q15: i64 = dsp::q15_ratio(28, 100) as i64; // 0.28
const FREEVERB_OFFSETROOM_Q15: i32 = dsp::q15_ratio(70, 100); // 0.70
/// WavesReverb mode reaches longer tails: feedback spans `[min, max]`.
const WAVESREVERB_FEEDBACK_MIN_Q15: i32 = dsp::q15_ratio(79, 100); // 0.79
const WAVESREVERB_FEEDBACK_MAX_Q15: i32 = dsp::q15_ratio(97, 100); // 0.97
/// Stability ceiling on any comb feedback (just under 1.0).
const REVERB_FEEDBACK_MAX_Q15: i32 = dsp::q15_ratio(992, 1000); // 0.992

/// Room-size (Q1.15) → comb feedback (Q1.15 raw) for a reverb mode.
/// Capped below 1.0 for stability.
fn reverb_feedback_q15(mode: ReverbMode, room_size: Q15) -> i32 {
    let room = room_size.raw().max(0) as i64;
    let fb = match mode {
        ReverbMode::Freeverb => {
            ((room * FREEVERB_SCALEROOM_Q15) >> 15) as i32 + FREEVERB_OFFSETROOM_Q15
        }
        ReverbMode::WavesReverb => {
            let span = (WAVESREVERB_FEEDBACK_MAX_Q15 - WAVESREVERB_FEEDBACK_MIN_Q15) as i64;
            ((room * span) >> 15) as i32 + WAVESREVERB_FEEDBACK_MIN_Q15
        }
    };
    fb.min(REVERB_FEEDBACK_MAX_Q15)
}

/// (Re)build a reverb's comb/all-pass banks for `rate_hz` (lengths scaled
/// from the 44.1 kHz tuning; the right channel is offset for stereo
/// decorrelation).
fn build_reverb_banks(
    rate_hz: u32,
    combs_l: &mut Vec<dsp::Comb>,
    combs_r: &mut Vec<dsp::Comb>,
    ap_l: &mut Vec<dsp::Allpass>,
    ap_r: &mut Vec<dsp::Allpass>,
) {
    let scale = |t: usize, off: usize| -> usize {
        (((t + off) as u64 * rate_hz as u64) / FREEVERB_TUNING_SAMPLE_RATE_HZ).max(1) as usize
    };
    combs_l.clear();
    combs_r.clear();
    ap_l.clear();
    ap_r.clear();
    for &t in &FREEVERB_COMB_TUNING {
        let mut c = dsp::Comb::new();
        c.prepare(scale(t, 0));
        combs_l.push(c);
        let mut c = dsp::Comb::new();
        c.prepare(scale(t, FREEVERB_STEREO_SPREAD));
        combs_r.push(c);
    }
    for &t in &FREEVERB_ALLPASS_TUNING {
        let mut a = dsp::Allpass::new();
        a.prepare(scale(t, 0));
        ap_l.push(a);
        let mut a = dsp::Allpass::new();
        a.prepare(scale(t, FREEVERB_STEREO_SPREAD));
        ap_r.push(a);
    }
}

/// Apply room-size / damping (or the frozen preset) to every comb in a
/// reverb's banks. When `freeze` is set, the combs run with unit feedback
/// and no damping (Freeverb's `freezemode`); the caller mutes the input.
fn recompute_reverb(
    mode: ReverbMode,
    room_size: Q15,
    damping: Q15,
    freeze: bool,
    combs_l: &mut [dsp::Comb],
    combs_r: &mut [dsp::Comb],
) {
    let (fb, damp) = if freeze {
        (REVERB_FREEZE_FEEDBACK, 0)
    } else {
        (
            reverb_feedback_q15(mode, room_size),
            damping.raw().max(0) as i32,
        )
    };
    for c in combs_l.iter_mut().chain(combs_r.iter_mut()) {
        c.set_feedback(fb);
        c.set_damp(damp);
    }
}

/// Early-reflection pattern for the I3DL2 approximation: `(delay ms, gain
/// Q1.15)` taps, a sparse decaying set evoking a room's first echoes.
const I3DL2_EARLY_TAPS: [(u32, i32); 6] = [
    (7, dsp::q15_ratio(79, 100)),  // 0.79
    (11, dsp::q15_ratio(58, 100)), // 0.58
    (17, dsp::q15_ratio(46, 100)), // 0.46
    (23, dsp::q15_ratio(34, 100)), // 0.34
    (29, dsp::q15_ratio(26, 100)), // 0.26
    (37, dsp::q15_ratio(20, 100)), // 0.20
];
/// Buffer length (ms) for the I3DL2 early-reflection / pre-delay line
/// (covers max ReflectionsDelay 300 ms + the tap spread).
const I3DL2_EARLY_MAX_MS: u64 = 360;
/// Right-channel early-tap offset (samples-equivalent ms) for stereo.
const I3DL2_EARLY_SPREAD_MS: u32 = 3;

/// I3DL2 `DecayTime` range (ms) and the comb-feedback range it maps to.
const I3DL2_DECAY_MS_MIN: i64 = 100;
const I3DL2_DECAY_MS_MAX: i64 = 20_000;
const I3DL2_FEEDBACK_MIN_Q15: i32 = dsp::q15_ratio(60, 100); // 0.60
const I3DL2_FEEDBACK_MAX_Q15: i32 = dsp::q15_ratio(966, 1000); // 0.966

/// Map a 100…20000 ms I3DL2 `DecayTime` onto a Freeverb comb feedback
/// (Q1.15), monotonically (longer decay ⇒ higher feedback).
fn i3dl2_feedback_q15(decay_time_ms: u16) -> i32 {
    let t = decay_time_ms.clamp(I3DL2_DECAY_MS_MIN as u16, I3DL2_DECAY_MS_MAX as u16) as i64;
    let span = (I3DL2_FEEDBACK_MAX_Q15 - I3DL2_FEEDBACK_MIN_Q15) as i64;
    let fb = I3DL2_FEEDBACK_MIN_Q15 as i64
        + ((t - I3DL2_DECAY_MS_MIN) * span) / (I3DL2_DECAY_MS_MAX - I3DL2_DECAY_MS_MIN);
    fb.clamp(0, REVERB_FEEDBACK_MAX_Q15 as i64) as i32
}

/// I3DL2 `DecayHFRatio` range (milli-ratio: 100 = 0.1, 2000 = 2.0).
const I3DL2_DECAY_HF_MILLI_MIN: i64 = 100;
const I3DL2_DECAY_HF_MILLI_MAX: i64 = 2_000;

/// Map an I3DL2 `DecayHFRatio` onto a damping coefficient (Q1.15): a high
/// ratio (≈ 2.0) ⇒ bright tail (no damping); a low ratio (≈ 0.1) ⇒ dark
/// tail (full damping).
fn i3dl2_damping_q15(decay_hf_milli: u16) -> i32 {
    let r = decay_hf_milli.clamp(
        I3DL2_DECAY_HF_MILLI_MIN as u16,
        I3DL2_DECAY_HF_MILLI_MAX as u16,
    ) as i64;
    let full_damp = Q15::ONE.raw() as i64;
    (((I3DL2_DECAY_HF_MILLI_MAX - r) * full_damp)
        / (I3DL2_DECAY_HF_MILLI_MAX - I3DL2_DECAY_HF_MILLI_MIN))
        .clamp(0, full_damp) as i32
}

/// Feedback parameter ceiling, just under unity (`0.99`), to keep delay
/// lines from self-oscillating. The signed `MIN` is for the flanger comb.
const FEEDBACK_MAX_Q15: Q15 = Q15::from_ratio(99, 100);
const FEEDBACK_MIN_Q15: Q15 = Q15::from_ratio(-99, 100);

/// Maximum echo/delay time the buffers are sized for (DMO range tops out
/// at 2000 ms).
const ECHO_MAX_DELAY_MS: u64 = 2_000;

/// Clamp a Q15.16 recirculating sample to ±(1<<30) — generous headroom
/// (±16384.0) that keeps the feedback sum inside `i32` without ever
/// clipping a musically-sane echo tail.
#[inline]
fn clamp_recirc(v: i64) -> i32 {
    v.clamp(-(1 << 30), 1 << 30) as i32
}

impl Device {
    /// Construct a peaking EQ (DMO-range params). Call
    /// [`Device::prepare`] before processing.
    pub fn param_eq(center_hz: u16, bandwidth_semitones: u8, gain_milli_db: i16) -> Self {
        Device::ParamEq {
            center_hz,
            bandwidth_semitones,
            gain_milli_db,
            rate_hz: 0,
            eq: Biquad::default(),
        }
    }

    /// Construct a distortion (DMO-range params). Call
    /// [`Device::prepare`] before processing.
    pub fn distortion(
        gain_milli_db: i32,
        edge: Q15,
        post_eq_center_hz: u16,
        post_eq_bandwidth_hz: u16,
        pre_lowpass_hz: u16,
    ) -> Self {
        Device::Distortion {
            gain_milli_db,
            edge,
            post_eq_center_hz,
            post_eq_bandwidth_hz,
            pre_lowpass_hz,
            rate_hz: 0,
            pre_lp: Biquad::default(),
            post_eq: Biquad::default(),
            out_gain_q15: 0,
        }
    }

    /// Construct a compressor (DMO-range params). `ratio_q8_8` is the
    /// ratio in Q8.8 (`256` = 1:1). Call [`Device::prepare`] before
    /// processing.
    pub fn compressor(
        threshold_milli_db: i32,
        ratio_q8_8: u16,
        attack_us: u32,
        release_us: u32,
        makeup_milli_db: i32,
    ) -> Self {
        Device::Compressor {
            threshold_milli_db,
            ratio_q8_8,
            attack_us,
            release_us,
            makeup_milli_db,
            rate_hz: 0,
            env: dsp::EnvelopeFollower::default(),
            thresh_oct_q16_16: 0,
            slope_q16_16: 0,
            makeup_oct_q16_16: 0,
        }
    }

    /// Construct an echo/delay (DMO-range params). Call
    /// [`Device::prepare`] before processing.
    pub fn echo(
        wet_dry: Q15,
        feedback: Q15,
        left_delay_ms: u16,
        right_delay_ms: u16,
        pan_delay: bool,
    ) -> Self {
        Device::Echo {
            wet_dry,
            feedback,
            left_delay_ms,
            right_delay_ms,
            pan_delay,
            rate_hz: 0,
            dl_l: dsp::DelayLine::new(),
            dl_r: dsp::DelayLine::new(),
        }
    }

    /// Construct a chorus/flanger (DMO-range params). `base_delay_ms_q8_8`
    /// is the centre delay in Q8.8 ms. Call [`Device::prepare`] before
    /// processing.
    #[allow(clippy::too_many_arguments)]
    pub fn mod_delay(
        mode: ModDelayMode,
        wet_dry: Q15,
        depth: Q15,
        feedback: Q15,
        frequency_milli_hz: u32,
        waveform_sine: bool,
        base_delay_ms_q8_8: u16,
        quadrature: bool,
    ) -> Self {
        Device::ModDelay {
            mode,
            wet_dry,
            depth,
            feedback,
            frequency_milli_hz,
            waveform_sine,
            base_delay_ms_q8_8,
            quadrature,
            rate_hz: 0,
            dl_l: dsp::DelayLine::new(),
            dl_r: dsp::DelayLine::new(),
            lfo_phase: 0,
            lfo_inc: 0,
        }
    }

    /// Construct a reverb (not frozen). `wet`/`dry` are independent levels
    /// (Freeverb scaling: wet ×3, dry ×2). Call [`Device::prepare`] before
    /// processing.
    pub fn reverb(
        mode: ReverbMode,
        room_size: Q15,
        damping: Q15,
        width: Q15,
        wet: Q15,
        dry: Q15,
    ) -> Self {
        Device::Reverb {
            mode,
            room_size,
            damping,
            width,
            wet,
            dry,
            freeze: false,
            rate_hz: 0,
            combs_l: Vec::new(),
            combs_r: Vec::new(),
            ap_l: Vec::new(),
            ap_r: Vec::new(),
        }
    }

    /// Construct an I3DL2 environmental reverb. Call [`Device::prepare`]
    /// before processing.
    #[allow(clippy::too_many_arguments)]
    pub fn i3dl2_reverb(
        room_milli_db: i16,
        decay_time_ms: u16,
        decay_hf_milli: u16,
        reflections_milli_db: i16,
        reflections_delay_ms: u16,
        reverb_milli_db: i16,
        reverb_delay_ms: u16,
    ) -> Self {
        Device::I3DL2Reverb {
            room_milli_db,
            decay_time_ms,
            decay_hf_milli,
            reflections_milli_db,
            reflections_delay_ms,
            reverb_milli_db,
            reverb_delay_ms,
            rate_hz: 0,
            early_l: dsp::DelayLine::new(),
            early_r: dsp::DelayLine::new(),
            combs_l: Vec::new(),
            combs_r: Vec::new(),
            ap_l: Vec::new(),
            ap_r: Vec::new(),
            early_gain_q15: 0,
            reverb_gain_q15: 0,
            room_gain_q15: 0,
        }
    }

    /// Construct a gargle (amplitude-modulation) effect. `rate_hz` is the
    /// modulation rate (1–1000). Call [`Device::prepare`] before
    /// processing.
    pub fn gargle(rate_hz: u16, wave_square: bool) -> Self {
        Device::Gargle {
            rate_hz,
            wave_square,
            sample_rate: 0,
            period: 0,
            counter: 0,
        }
    }
}

impl Device {
    /// Process one stereo `(left, right)` sample, advancing any
    /// internal state.
    #[inline]
    pub fn process(&mut self, sample: (Amp, Amp)) -> (Amp, Amp) {
        match self {
            Device::Gain(g) => (g.apply(sample.0), g.apply(sample.1)),
            Device::LowPass { cutoff, state } => {
                // Q1.15 one-pole: y += (x − y)·a >> 15. State kept in
                // i32 for headroom; output clamps back to Q1.15 `Amp`.
                let a = cutoff.raw().max(0) as i32;
                let step = |y: &mut i32, x: Amp| -> Amp {
                    let xr = x.as_q15_i16() as i32;
                    *y += ((xr - *y) * a) >> 15;
                    Amp::from_q15_i16((*y).clamp(i16::MIN as i32, i16::MAX as i32) as i16)
                };
                (step(&mut state.0, sample.0), step(&mut state.1, sample.1))
            }
            // Un-prepared (`rate_hz == 0`) the biquad is the identity, so
            // this is a pass-through until `prepare` derives coefficients.
            Device::ParamEq { eq, .. } => eq.process(sample.0, sample.1),
            Device::Distortion {
                rate_hz,
                edge,
                pre_lp,
                post_eq,
                out_gain_q15,
                ..
            } => {
                if *rate_hz == 0 {
                    return sample; // un-prepared ⇒ safe pass-through
                }
                // pre-LP → soft-clip → post-EQ → output gain.
                let (l, r) = pre_lp.process(sample.0, sample.1);
                // Drive 1×…32× across edge 0…100 %.
                let drive_q8_8 = 256 + ((edge.raw().max(0) as i32 * (31 * 256)) >> 15);
                let l = dsp::soft_clip_q15(l.as_q15_i16() as i32, drive_q8_8);
                let r = dsp::soft_clip_q15(r.as_q15_i16() as i32, drive_q8_8);
                let (l, r) = post_eq.process(Amp::from_q15_i32_sat(l), Amp::from_q15_i32_sat(r));
                let g = *out_gain_q15;
                (
                    Amp::from_q15_i32_sat((l.as_q15_i16() as i32 * g) >> 15),
                    Amp::from_q15_i32_sat((r.as_q15_i16() as i32 * g) >> 15),
                )
            }
            Device::Compressor {
                rate_hz,
                env,
                thresh_oct_q16_16,
                slope_q16_16,
                makeup_oct_q16_16,
                ..
            } => {
                if *rate_hz == 0 {
                    return sample; // un-prepared ⇒ safe pass-through
                }
                let l = sample.0.as_q15_i16() as i32;
                let r = sample.1.as_q15_i16() as i32;
                // Stereo-linked peak detector.
                let target = l.abs().max(r.abs());
                let e = env.process(target);
                // Log-domain gain (octaves): compress above threshold,
                // then apply make-up. Below threshold ⇒ make-up only.
                let gain_oct = if e > 0 {
                    let level_oct = dsp::log2_q16_16((e as i64) << 1); // Q15→Q16.16
                    let over = level_oct - *thresh_oct_q16_16;
                    if over > 0 {
                        (((over as i64) * (*slope_q16_16 as i64)) >> 16) as i32 + *makeup_oct_q16_16
                    } else {
                        *makeup_oct_q16_16
                    }
                } else {
                    *makeup_oct_q16_16
                };
                let g = dsp::pow2_q16_16(gain_oct); // Q16.16
                let app = |x: i32| -> Amp { Amp::from_q15_i32_sat(((x as i64 * g) >> 16) as i32) };
                (app(l), app(r))
            }
            Device::Echo {
                wet_dry,
                feedback,
                left_delay_ms,
                right_delay_ms,
                pan_delay,
                rate_hz,
                dl_l,
                dl_r,
            } => {
                if *rate_hz == 0 {
                    return sample; // un-prepared ⇒ safe pass-through
                }
                let to_samples =
                    |ms: u16| -> usize { ((ms as u64 * *rate_hz as u64) / 1000).max(1) as usize };
                let ld = to_samples(*left_delay_ms);
                let rd = to_samples(*right_delay_ms);
                // Delayed (recirculated) signal, Q15.16.
                let echo_l = dl_l.tap(ld) as i64;
                let echo_r = dl_r.tap(rd) as i64;

                let dry_l = sample.0.as_q15_i16() as i64;
                let dry_r = sample.1.as_q15_i16() as i64;
                let fb = feedback.raw().max(0) as i64; // 0..32767

                // Write input + feedback·(previous echo). PanDelay feeds
                // each line from the *other* channel's tail so successive
                // echoes bounce L↔R.
                let (src_l, src_r) = if *pan_delay {
                    (echo_r, echo_l)
                } else {
                    (echo_l, echo_r)
                };
                dl_l.write(clamp_recirc((dry_l << 16) + ((src_l * fb) >> 15)));
                dl_r.write(clamp_recirc((dry_r << 16) + ((src_r * fb) >> 15)));

                // Wet/dry mix (wet tap back to Q1.15 via >> 16).
                let wet = wet_dry.raw().max(0) as i64; // 0..32767
                let dry = Q15::ONE.raw() as i64 - wet;
                let out_l = ((dry_l * dry) + ((echo_l >> 16) * wet)) >> 15;
                let out_r = ((dry_r * dry) + ((echo_r >> 16) * wet)) >> 15;
                (
                    Amp::from_q15_i32_sat(out_l as i32),
                    Amp::from_q15_i32_sat(out_r as i32),
                )
            }
            Device::ModDelay {
                wet_dry,
                depth,
                feedback,
                waveform_sine,
                base_delay_ms_q8_8,
                quadrature,
                rate_hz,
                dl_l,
                dl_r,
                lfo_phase,
                lfo_inc,
                ..
            } => {
                if *rate_hz == 0 {
                    return sample; // un-prepared ⇒ safe pass-through
                }
                // Centre delay (Q16.16 samples), at least 2 samples so the
                // fractional tap has neighbours and the sweep stays > 0.
                let base_q16 = (((*base_delay_ms_q8_8 as u64 * *rate_hz as u64 * 256) / 1000)
                    as i64)
                    .max(2 << 16);
                // Sweep ≤ 90 % of the base so modulated delay never ≤ 0.
                let mod_q16 = (base_q16 * depth.raw().max(0) as i64 * 9) / (32_768 * 10);
                let max_delay = (dl_l.len().saturating_sub(1)) as i64;

                let delay_for = |lfo_q15: i32| -> u32 {
                    let d = base_q16 + ((mod_q16 * lfo_q15 as i64) >> 15);
                    d.clamp(1 << 16, max_delay << 16) as u32
                };
                let lfo_l = dsp::lfo_value_q15(*lfo_phase, *waveform_sine);
                let lfo_r = dsp::lfo_value_q15(
                    lfo_phase.wrapping_add(if *quadrature { 1 << 30 } else { 0 }),
                    *waveform_sine,
                );

                let wet_l = dl_l.tap_frac(delay_for(lfo_l)) as i64; // Q15.16
                let wet_r = dl_r.tap_frac(delay_for(lfo_r)) as i64;

                let dry_l = sample.0.as_q15_i16() as i64;
                let dry_r = sample.1.as_q15_i16() as i64;
                let fb = feedback.raw() as i64; // signed

                dl_l.write(clamp_recirc((dry_l << 16) + ((wet_l * fb) >> 15)));
                dl_r.write(clamp_recirc((dry_r << 16) + ((wet_r * fb) >> 15)));

                *lfo_phase = lfo_phase.wrapping_add(*lfo_inc);

                let wet = wet_dry.raw().max(0) as i64;
                let dry = Q15::ONE.raw() as i64 - wet;
                let out_l = ((dry_l * dry) + ((wet_l >> 16) * wet)) >> 15;
                let out_r = ((dry_r * dry) + ((wet_r >> 16) * wet)) >> 15;
                (
                    Amp::from_q15_i32_sat(out_l as i32),
                    Amp::from_q15_i32_sat(out_r as i32),
                )
            }
            Device::Reverb {
                width,
                wet,
                dry,
                freeze,
                rate_hz,
                combs_l,
                combs_r,
                ap_l,
                ap_r,
                ..
            } => {
                if *rate_hz == 0 || combs_l.is_empty() {
                    return sample; // un-prepared ⇒ safe pass-through
                }
                let dry_l = sample.0.as_q15_i16() as i64;
                let dry_r = sample.1.as_q15_i16() as i64;
                // Freeze mutes the input: the frozen tail recirculates with
                // unit feedback on its own (Freeverb `gain = muted`).
                let input = if *freeze {
                    0
                } else {
                    ((((dry_l + dry_r) * REVERB_FIXED_GAIN_Q15) >> 15) << 16) as i32
                };

                // Parallel combs, summed per channel.
                let mut acc_l = 0i64;
                for c in combs_l.iter_mut() {
                    acc_l += c.process(input) as i64;
                }
                let mut acc_r = 0i64;
                for c in combs_r.iter_mut() {
                    acc_r += c.process(input) as i64;
                }
                // Series all-passes.
                let mut sl = acc_l.clamp(-(1 << 30), 1 << 30) as i32;
                for a in ap_l.iter_mut() {
                    sl = a.process(sl);
                }
                let mut sr = acc_r.clamp(-(1 << 30), 1 << 30) as i32;
                for a in ap_r.iter_mut() {
                    sr = a.process(sr);
                }

                // Independent wet (×scalewet) and dry (×scaledry) levels,
                // with the Freeverb stereo cross-mix on the wet path:
                //   out = revL·wet1 + revR·wet2 + dryL·dry  (and mirror).
                let wet_g = (wet.raw().max(0) as i64) * REVERB_SCALEWET;
                let dry_g = (dry.raw().max(0) as i64) * REVERB_SCALEDRY;
                let w = width.raw().max(0) as i64;
                let half = Q15::HALF.raw() as i64; // 0.5
                let wet1 = (wet_g * ((w >> 1) + half)) >> 15; // wet·(width/2 + 0.5)
                let wet2 = (wet_g * (half - (w >> 1))) >> 15; // wet·((1 − width)/2)
                let sl15 = sl as i64 >> 16; // reverb output, Q1.15
                let sr15 = sr as i64 >> 16;
                let out_l = ((sl15 * wet1 + sr15 * wet2) >> 15) + ((dry_l * dry_g) >> 15);
                let out_r = ((sr15 * wet1 + sl15 * wet2) >> 15) + ((dry_r * dry_g) >> 15);
                (
                    Amp::from_q15_i32_sat(out_l as i32),
                    Amp::from_q15_i32_sat(out_r as i32),
                )
            }
            Device::I3DL2Reverb {
                reflections_delay_ms,
                reverb_delay_ms,
                rate_hz,
                early_l,
                early_r,
                combs_l,
                combs_r,
                ap_l,
                ap_r,
                early_gain_q15,
                reverb_gain_q15,
                room_gain_q15,
                ..
            } => {
                if *rate_hz == 0 || combs_l.is_empty() {
                    return sample; // un-prepared ⇒ safe pass-through
                }
                let dry_l = sample.0.as_q15_i16() as i64;
                let dry_r = sample.1.as_q15_i16() as i64;
                let mono = ((dry_l + dry_r) >> 1) as i32; // Q15
                early_l.write(mono << 16);
                early_r.write(mono << 16);

                let ms =
                    |m: u32| -> usize { ((m as u64 * *rate_hz as u64) / 1000).max(1) as usize };
                let refl_base = ms(*reflections_delay_ms as u32);
                let spread = ms(I3DL2_EARLY_SPREAD_MS);

                // Early reflections (Q15.16).
                let mut er_l = 0i64;
                let mut er_r = 0i64;
                for (tap_ms, gain) in I3DL2_EARLY_TAPS {
                    let d = refl_base + ms(tap_ms);
                    er_l += (early_l.tap(d) as i64 * gain as i64) >> 15;
                    er_r += (early_r.tap(d + spread) as i64 * gain as i64) >> 15;
                }
                let er_l = (er_l * *early_gain_q15 as i64) >> 15;
                let er_r = (er_r * *early_gain_q15 as i64) >> 15;

                // Late reverb fed by the pre-delayed, attenuated mono.
                let late_raw = early_l.tap(ms(*reverb_delay_ms as u32)) >> 16; // Q15 mono
                let late_in = (((late_raw as i64 * REVERB_FIXED_GAIN_Q15) >> 15) << 16)
                    .clamp(-(1 << 30), 1 << 30) as i32;
                let mut acc_l = 0i64;
                for c in combs_l.iter_mut() {
                    acc_l += c.process(late_in) as i64;
                }
                let mut acc_r = 0i64;
                for c in combs_r.iter_mut() {
                    acc_r += c.process(late_in) as i64;
                }
                let mut sl = acc_l.clamp(-(1 << 30), 1 << 30) as i32;
                for a in ap_l.iter_mut() {
                    sl = a.process(sl);
                }
                let mut sr = acc_r.clamp(-(1 << 30), 1 << 30) as i32;
                for a in ap_r.iter_mut() {
                    sr = a.process(sr);
                }
                let late_l = (sl as i64 * *reverb_gain_q15 as i64) >> 15;
                let late_r = (sr as i64 * *reverb_gain_q15 as i64) >> 15;

                // Early + late, scaled by overall Room gain, added to dry.
                let wet_l = (((er_l + late_l) * *room_gain_q15 as i64) >> 15) >> 16;
                let wet_r = (((er_r + late_r) * *room_gain_q15 as i64) >> 15) >> 16;
                (
                    Amp::from_q15_i32_sat((dry_l + wet_l) as i32),
                    Amp::from_q15_i32_sat((dry_r + wet_r) as i32),
                )
            }
            Device::Gargle {
                wave_square,
                period,
                counter,
                ..
            } => {
                if *period < 2 {
                    return sample; // un-prepared ⇒ safe pass-through
                }
                let period_half = (*period / 2).max(1);
                // Unipolar [0, 1] gain: triangle ramps 0→1→0 over the
                // cycle; square is full for the first half, silent for the
                // second (mirrors OpenMPT `Gargle::Process`).
                let unity = Q15::ONE.raw() as u32;
                let gain_q15: i32 = if *counter < period_half {
                    if *wave_square {
                        unity as i32
                    } else {
                        ((*counter * unity) / period_half) as i32
                    }
                } else if *wave_square {
                    0
                } else {
                    (((*period - *counter) * unity) / period_half) as i32
                };
                *counter += 1;
                if *counter >= *period {
                    *counter = 0;
                }
                let g = gain_q15.clamp(0, Q15::ONE.raw() as i32);
                (
                    Amp::from_q15_i32_sat((sample.0.as_q15_i16() as i32 * g) >> 15),
                    Amp::from_q15_i32_sat((sample.1.as_q15_i16() as i32 * g) >> 15),
                )
            }
        }
    }

    /// RFC §3A — set a device parameter from a normalized
    /// ([`AutomationValue::Normalized`]) automation value. The `param`
    /// id is device-specific; unknown ids are ignored.
    ///
    /// * [`Device::Gain`] — `param 0` = gain (the Q1.15 value mapped to
    ///   a `[0, 1]` [`Volume`]; negative values clamp to silence).
    /// * [`Device::ParamEq`] — `0` = centre (80–16000 Hz), `1` =
    ///   bandwidth (1–36 semitones), `2` = gain (−15…+15 dB).
    /// * [`Device::Distortion`] — `0` = output gain (−60…0 dB), `1` =
    ///   edge (0–100 %), `2` = post-EQ centre, `3` = post-EQ bandwidth,
    ///   `4` = pre-low-pass cutoff (all Hz, 100–8000).
    /// * [`Device::Compressor`] — `0` = threshold (−60…0 dB), `1` =
    ///   ratio (1…100), `2` = attack (0.01…500 ms), `3` = release
    ///   (50…3000 ms), `4` = make-up gain (−60…+60 dB).
    /// * [`Device::Echo`] — `0` = wet/dry, `1` = feedback, `2` = left
    ///   delay (1…2000 ms), `3` = right delay (1…2000 ms), `4` = pan-delay
    ///   toggle.
    /// * [`Device::ModDelay`] — `0` = wet/dry, `1` = depth, `2` = feedback
    ///   (signed), `3` = LFO rate (0…10 Hz), `4` = base delay (0…20 ms).
    /// * [`Device::Reverb`] — `0` = room size, `1` = damping, `2` = width,
    ///   `3` = wet level, `4` = dry level, `5` = freeze (≥ 0.5 = on).
    /// * [`Device::I3DL2Reverb`] — `0` = room, `1` = decay time, `2` =
    ///   decay-HF ratio, `3` = reflections level, `4` = reflections delay,
    ///   `5` = reverb level, `6` = reverb delay.
    /// * [`Device::Gargle`] — `0` = rate (1–1000 Hz), `1` = waveform
    ///   (triangle / square).
    ///
    /// The normalized value is mapped linearly onto each range; the
    /// device recomputes its coefficients in place.
    ///
    /// [`AutomationValue::Normalized`]: crate::core::daw::automation::AutomationValue::Normalized
    #[inline]
    pub fn set_param(&mut self, param: u16, value: Q15) {
        match self {
            Device::Gain(g) => {
                if param == 0 {
                    *g = Volume::from_q15(value.max(Q15::ZERO));
                }
            }
            Device::LowPass { cutoff, .. } => {
                if param == 0 {
                    *cutoff = value.max(Q15::ZERO);
                }
            }
            Device::ParamEq {
                center_hz,
                bandwidth_semitones,
                gain_milli_db,
                rate_hz,
                eq,
            } => {
                match param {
                    0 => *center_hz = lerp_param(value, 80, 16_000) as u16,
                    1 => *bandwidth_semitones = lerp_param(value, 1, 36) as u8,
                    2 => *gain_milli_db = lerp_param(value, -15_000, 15_000) as i16,
                    _ => return,
                }
                recompute_param_eq(
                    eq,
                    *rate_hz,
                    *center_hz,
                    *bandwidth_semitones,
                    *gain_milli_db,
                );
            }
            Device::Distortion {
                gain_milli_db,
                edge,
                post_eq_center_hz,
                post_eq_bandwidth_hz,
                pre_lowpass_hz,
                rate_hz,
                pre_lp,
                post_eq,
                out_gain_q15,
            } => {
                match param {
                    0 => *gain_milli_db = lerp_param(value, -60_000, 0),
                    1 => *edge = value.max(Q15::ZERO),
                    2 => *post_eq_center_hz = lerp_param(value, 100, 8_000) as u16,
                    3 => *post_eq_bandwidth_hz = lerp_param(value, 100, 8_000) as u16,
                    4 => *pre_lowpass_hz = lerp_param(value, 100, 8_000) as u16,
                    _ => return,
                }
                recompute_distortion(
                    pre_lp,
                    post_eq,
                    out_gain_q15,
                    *rate_hz,
                    *gain_milli_db,
                    *post_eq_center_hz,
                    *post_eq_bandwidth_hz,
                    *pre_lowpass_hz,
                );
            }
            Device::Compressor {
                threshold_milli_db,
                ratio_q8_8,
                attack_us,
                release_us,
                makeup_milli_db,
                rate_hz,
                env,
                thresh_oct_q16_16,
                slope_q16_16,
                makeup_oct_q16_16,
            } => {
                match param {
                    0 => *threshold_milli_db = lerp_param(value, -60_000, 0),
                    1 => *ratio_q8_8 = lerp_param(value, 256, 25_600) as u16,
                    2 => *attack_us = lerp_param(value, 10, 500_000) as u32,
                    3 => *release_us = lerp_param(value, 50_000, 3_000_000) as u32,
                    4 => *makeup_milli_db = lerp_param(value, -60_000, 60_000),
                    _ => return,
                }
                recompute_compressor(
                    env,
                    thresh_oct_q16_16,
                    slope_q16_16,
                    makeup_oct_q16_16,
                    *rate_hz,
                    *threshold_milli_db,
                    *ratio_q8_8,
                    *attack_us,
                    *release_us,
                    *makeup_milli_db,
                );
            }
            Device::Echo {
                wet_dry,
                feedback,
                left_delay_ms,
                right_delay_ms,
                pan_delay,
                ..
            } => match param {
                0 => *wet_dry = value.max(Q15::ZERO),
                1 => *feedback = value.clamp(Q15::ZERO, FEEDBACK_MAX_Q15),
                2 => *left_delay_ms = lerp_param(value, 1, 2_000) as u16,
                3 => *right_delay_ms = lerp_param(value, 1, 2_000) as u16,
                4 => *pan_delay = value.raw() > 0,
                _ => {}
            },
            Device::ModDelay {
                wet_dry,
                depth,
                feedback,
                frequency_milli_hz,
                base_delay_ms_q8_8,
                rate_hz,
                lfo_inc,
                ..
            } => {
                match param {
                    0 => *wet_dry = value.max(Q15::ZERO),
                    1 => *depth = value.max(Q15::ZERO),
                    // Feedback is signed (centre = no feedback).
                    2 => *feedback = value.clamp(FEEDBACK_MIN_Q15, FEEDBACK_MAX_Q15),
                    3 => {
                        *frequency_milli_hz = lerp_param(value, 0, 10_000) as u32;
                        *lfo_inc = dsp::lfo_increment(*frequency_milli_hz, *rate_hz);
                    }
                    4 => *base_delay_ms_q8_8 = lerp_param(value, 0, 20 * 256) as u16,
                    _ => {}
                }
            }
            Device::Reverb {
                mode,
                room_size,
                damping,
                width,
                wet,
                dry,
                freeze,
                combs_l,
                combs_r,
                ..
            } => {
                match param {
                    0 => *room_size = value.max(Q15::ZERO),
                    1 => *damping = value.max(Q15::ZERO),
                    2 => *width = value.max(Q15::ZERO),
                    3 => *wet = value.max(Q15::ZERO),
                    4 => *dry = value.max(Q15::ZERO),
                    5 => *freeze = value.raw() >= Q15::HALF.raw(),
                    _ => return,
                }
                recompute_reverb(*mode, *room_size, *damping, *freeze, combs_l, combs_r);
            }
            Device::I3DL2Reverb {
                room_milli_db,
                decay_time_ms,
                decay_hf_milli,
                reflections_milli_db,
                reflections_delay_ms,
                reverb_milli_db,
                reverb_delay_ms,
                combs_l,
                combs_r,
                early_gain_q15,
                reverb_gain_q15,
                room_gain_q15,
                ..
            } => {
                match param {
                    0 => *room_milli_db = lerp_param(value, -10_000, 0) as i16,
                    1 => *decay_time_ms = lerp_param(value, 100, 20_000) as u16,
                    2 => *decay_hf_milli = lerp_param(value, 100, 2_000) as u16,
                    3 => *reflections_milli_db = lerp_param(value, -10_000, 1_000) as i16,
                    4 => *reflections_delay_ms = lerp_param(value, 0, 300) as u16,
                    5 => *reverb_milli_db = lerp_param(value, -10_000, 2_000) as i16,
                    6 => *reverb_delay_ms = lerp_param(value, 0, 100) as u16,
                    _ => return,
                }
                recompute_i3dl2(
                    *decay_time_ms,
                    *decay_hf_milli,
                    *reflections_milli_db,
                    *reverb_milli_db,
                    *room_milli_db,
                    combs_l,
                    combs_r,
                    early_gain_q15,
                    reverb_gain_q15,
                    room_gain_q15,
                );
            }
            Device::Gargle {
                rate_hz,
                wave_square,
                sample_rate,
                period,
                ..
            } => {
                match param {
                    0 => *rate_hz = lerp_param(value, 1, 1_000) as u16,
                    1 => *wave_square = value.raw() > Q15::HALF.raw(),
                    _ => return,
                }
                if *sample_rate > 0 {
                    *period = (*sample_rate / (*rate_hz).max(1) as u32).max(2);
                }
            }
        }
    }

    /// Prepare the device for playback at `sample_rate` (Hz): size any
    /// time-based buffers (delay lines, reverb combs) and precompute
    /// rate-dependent coefficients (filter cutoffs, attack/release time
    /// constants, LFO increments).
    ///
    /// **Invariant.** Transient runtime state is `#[serde(skip)]`'d and
    /// therefore empty after deserialisation, so the runtime **must**
    /// call this (via [`DeviceChain::prepare`]) before the first
    /// [`Device::process`]. An unprepared device must still be a safe
    /// pass-through, never a panic. Re-call on a sample-rate change.
    /// Stateless devices ([`Device::Gain`]) ignore it.
    pub fn prepare(&mut self, sample_rate: u32) {
        let _ = sample_rate;
        match self {
            Device::Gain(_) => {}
            // One-pole `cutoff` is already a per-sample coefficient, not
            // a frequency, so nothing to precompute yet.
            Device::LowPass { .. } => {}
            Device::ParamEq {
                center_hz,
                bandwidth_semitones,
                gain_milli_db,
                rate_hz,
                eq,
            } => {
                *rate_hz = sample_rate;
                recompute_param_eq(
                    eq,
                    sample_rate,
                    *center_hz,
                    *bandwidth_semitones,
                    *gain_milli_db,
                );
            }
            Device::Distortion {
                gain_milli_db,
                post_eq_center_hz,
                post_eq_bandwidth_hz,
                pre_lowpass_hz,
                rate_hz,
                pre_lp,
                post_eq,
                out_gain_q15,
                ..
            } => {
                *rate_hz = sample_rate;
                recompute_distortion(
                    pre_lp,
                    post_eq,
                    out_gain_q15,
                    sample_rate,
                    *gain_milli_db,
                    *post_eq_center_hz,
                    *post_eq_bandwidth_hz,
                    *pre_lowpass_hz,
                );
            }
            Device::Compressor {
                threshold_milli_db,
                ratio_q8_8,
                attack_us,
                release_us,
                makeup_milli_db,
                rate_hz,
                env,
                thresh_oct_q16_16,
                slope_q16_16,
                makeup_oct_q16_16,
            } => {
                *rate_hz = sample_rate;
                recompute_compressor(
                    env,
                    thresh_oct_q16_16,
                    slope_q16_16,
                    makeup_oct_q16_16,
                    sample_rate,
                    *threshold_milli_db,
                    *ratio_q8_8,
                    *attack_us,
                    *release_us,
                    *makeup_milli_db,
                );
            }
            Device::Echo {
                rate_hz,
                dl_l,
                dl_r,
                ..
            } => {
                *rate_hz = sample_rate;
                let max_len = ((ECHO_MAX_DELAY_MS * sample_rate as u64) / 1000 + 1) as usize;
                dl_l.prepare(max_len);
                dl_r.prepare(max_len);
            }
            Device::ModDelay {
                frequency_milli_hz,
                rate_hz,
                dl_l,
                dl_r,
                lfo_inc,
                ..
            } => {
                *rate_hz = sample_rate;
                let max_len = ((MODDELAY_MAX_MS * sample_rate as u64) / 1000 + 1) as usize;
                dl_l.prepare(max_len);
                dl_r.prepare(max_len);
                *lfo_inc = dsp::lfo_increment(*frequency_milli_hz, sample_rate);
            }
            Device::Reverb {
                mode,
                room_size,
                damping,
                freeze,
                rate_hz,
                combs_l,
                combs_r,
                ap_l,
                ap_r,
                ..
            } => {
                *rate_hz = sample_rate;
                build_reverb_banks(sample_rate, combs_l, combs_r, ap_l, ap_r);
                recompute_reverb(*mode, *room_size, *damping, *freeze, combs_l, combs_r);
            }
            Device::I3DL2Reverb {
                room_milli_db,
                decay_time_ms,
                decay_hf_milli,
                reflections_milli_db,
                reverb_milli_db,
                rate_hz,
                early_l,
                early_r,
                combs_l,
                combs_r,
                ap_l,
                ap_r,
                early_gain_q15,
                reverb_gain_q15,
                room_gain_q15,
                ..
            } => {
                *rate_hz = sample_rate;
                let early_len = ((I3DL2_EARLY_MAX_MS * sample_rate as u64) / 1000 + 1) as usize;
                early_l.prepare(early_len);
                early_r.prepare(early_len);
                build_reverb_banks(sample_rate, combs_l, combs_r, ap_l, ap_r);
                recompute_i3dl2(
                    *decay_time_ms,
                    *decay_hf_milli,
                    *reflections_milli_db,
                    *reverb_milli_db,
                    *room_milli_db,
                    combs_l,
                    combs_r,
                    early_gain_q15,
                    reverb_gain_q15,
                    room_gain_q15,
                );
            }
            Device::Gargle {
                rate_hz,
                sample_rate: sr,
                period,
                counter,
                ..
            } => {
                *sr = sample_rate;
                *period = (sample_rate / (*rate_hz).max(1) as u32).max(2);
                *counter = 0;
            }
        }
    }

    /// Clear transient signal state (filter history, delay tails,
    /// envelope followers) **without** changing parameters or buffer
    /// sizes. Called on seek so a jump stays deterministic — mirrors the
    /// player's cursor-reset discipline (`reset_for_goto`). Stateless
    /// devices ignore it.
    pub fn reset(&mut self) {
        match self {
            Device::Gain(_) => {}
            Device::LowPass { state, .. } => *state = (0, 0),
            Device::ParamEq { eq, .. } => eq.reset_history(),
            Device::Distortion {
                pre_lp, post_eq, ..
            } => {
                pre_lp.reset_history();
                post_eq.reset_history();
            }
            Device::Compressor { env, .. } => env.reset(),
            Device::Echo { dl_l, dl_r, .. } => {
                dl_l.reset();
                dl_r.reset();
            }
            Device::ModDelay {
                dl_l,
                dl_r,
                lfo_phase,
                ..
            } => {
                dl_l.reset();
                dl_r.reset();
                *lfo_phase = 0;
            }
            Device::Reverb {
                combs_l,
                combs_r,
                ap_l,
                ap_r,
                ..
            } => {
                for c in combs_l.iter_mut().chain(combs_r.iter_mut()) {
                    c.reset();
                }
                for a in ap_l.iter_mut().chain(ap_r.iter_mut()) {
                    a.reset();
                }
            }
            Device::I3DL2Reverb {
                early_l,
                early_r,
                combs_l,
                combs_r,
                ap_l,
                ap_r,
                ..
            } => {
                early_l.reset();
                early_r.reset();
                for c in combs_l.iter_mut().chain(combs_r.iter_mut()) {
                    c.reset();
                }
                for a in ap_l.iter_mut().chain(ap_r.iter_mut()) {
                    a.reset();
                }
            }
            Device::Gargle { counter, .. } => *counter = 0,
        }
    }
}

/// An ordered insert chain on one mixer channel. Empty (the default,
/// and the only state any tracker import produces) ⇒ pass-through.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct DeviceChain {
    /// Inserts, processed front-to-back.
    pub devices: Vec<Device>,
    /// Master bypass for the whole chain (editor A/B compare).
    pub bypass: bool,
}

impl DeviceChain {
    /// `true` when the chain would actually change the signal — lets
    /// the mixer skip the per-sample call entirely on the (universal)
    /// empty / bypassed case.
    #[inline]
    pub fn is_active(&self) -> bool {
        !self.bypass && !self.devices.is_empty()
    }

    /// Run a stereo sample through every insert in order, advancing
    /// each device's state.
    #[inline]
    pub fn process(&mut self, mut sample: (Amp, Amp)) -> (Amp, Amp) {
        if self.bypass {
            return sample;
        }
        for d in &mut self.devices {
            sample = d.process(sample);
        }
        sample
    }

    /// Prepare every device for `sample_rate` (Hz). The runtime calls
    /// this when it materialises the chain (and on a rate change), before
    /// any [`DeviceChain::process`]. Empty for every tracker import.
    #[inline]
    pub fn prepare(&mut self, sample_rate: u32) {
        for d in &mut self.devices {
            d.prepare(sample_rate);
        }
    }

    /// Clear every device's transient state on seek (parameters kept).
    #[inline]
    pub fn reset(&mut self) {
        for d in &mut self.devices {
            d.reset();
        }
    }
}

/// RFC §3B — a **return/aux bus**: a named mix destination with its
/// own insert chain. Channels feed it via [`Send`]s; its processed
/// output is summed back into the master mix. Empty `buses` (every
/// tracker import) ⇒ no extra routing.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Bus {
    pub name: alloc::string::String,
    pub chain: DeviceChain,
}

/// RFC §3B — one channel→bus send: a copy of the channel's
/// post-insert signal, scaled by `level`, added to bus `bus`'s input.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct Send {
    /// Destination index into [`crate::core::module::Module::buses`].
    pub bus: u16,
    /// Send amount, Q1.15 `[0, 1]`.
    pub level: Volume,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_chain_is_passthrough() {
        let mut c = DeviceChain::default();
        assert!(!c.is_active());
        let s = (Amp::from_q15_i16(10_000), Amp::from_q15_i16(-8_000));
        assert_eq!(c.process(s), s);
    }

    #[test]
    fn bypass_is_passthrough() {
        let mut c = DeviceChain {
            devices: alloc::vec![Device::Gain(Volume::SILENT)],
            bypass: true,
        };
        assert!(!c.is_active());
        let s = (Amp::from_q15_i16(10_000), Amp::from_q15_i16(-8_000));
        assert_eq!(c.process(s), s);
    }

    #[test]
    fn gain_scales_both_channels() {
        let half = Volume::from_q15(Q15::from_ratio(1, 2)); // ~0.5
        let mut c = DeviceChain {
            devices: alloc::vec![Device::Gain(half)],
            bypass: false,
        };
        assert!(c.is_active());
        let s = (Amp::from_q15_i16(20_000), Amp::from_q15_i16(-20_000));
        let (l, r) = c.process(s);
        // ~half amplitude, both channels.
        assert!(l.as_q15_i16() > 9_000 && l.as_q15_i16() < 11_000);
        assert!(r.as_q15_i16() < -9_000 && r.as_q15_i16() > -11_000);
    }

    #[test]
    fn lowpass_unity_cutoff_is_passthrough() {
        // a = 1.0 ⇒ y[n] = x[n] (the previous-output term cancels).
        let mut d = Device::LowPass {
            cutoff: Q15::ONE,
            state: (0, 0),
        };
        for raw in [12_000_i16, -9_000, 4_000, 0, 30_000] {
            let s = (Amp::from_q15_i16(raw), Amp::from_q15_i16(raw));
            let (l, _) = d.process(s);
            // Within one LSB of the input (a = 32767/32768, not exactly 1).
            assert!((l.as_q15_i16() as i32 - raw as i32).abs() <= 1);
        }
    }

    #[test]
    fn prepared_empty_chain_is_passthrough() {
        // The runtime always `prepare`s a chain before processing;
        // preparing an empty (every tracker import) or unity-gain chain
        // must leave it a bit-exact pass-through.
        let mut empty = DeviceChain::default();
        empty.prepare(48_000);
        let s = (Amp::from_q15_i16(7_777), Amp::from_q15_i16(-5_555));
        assert_eq!(empty.process(s), s);

        let mut unity = DeviceChain {
            devices: alloc::vec![Device::Gain(Volume::FULL)],
            bypass: false,
        };
        unity.prepare(48_000);
        assert_eq!(unity.process(s), s);
    }

    #[test]
    fn reset_clears_stateful_history() {
        // After driving a low-cutoff filter to a non-zero state, `reset`
        // must restore it so the next input sees the same response as a
        // fresh device — deterministic seeks depend on this.
        let fresh = || Device::LowPass {
            cutoff: Q15::from_ratio(12, 100),
            state: (0, 0),
        };
        let step = (Amp::from_q15_i16(20_000), Amp::from_q15_i16(20_000));

        let mut a = fresh();
        a.prepare(48_000);
        let first = a.process(step);

        let mut b = fresh();
        b.prepare(48_000);
        for _ in 0..16 {
            b.process(step); // warm up to a non-zero history
        }
        b.reset();
        let after_reset = b.process(step);

        assert_eq!(after_reset, first, "reset did not restore initial state");
    }

    #[test]
    fn param_eq_unprepared_is_passthrough() {
        // Inserted but never prepared ⇒ identity biquad ⇒ bit-exact.
        let mut d = Device::param_eq(1_000, 12, 6_000);
        for raw in [0i16, 12_345, -9_876, 30_000] {
            let (l, r) = d.process((Amp::from_q15_i16(raw), Amp::from_q15_i16(-raw)));
            assert!((l.as_q15_i16() as i32 - raw as i32).abs() <= 1);
            assert!((r.as_q15_i16() as i32 + raw as i32).abs() <= 1);
        }
    }

    #[test]
    fn param_eq_boost_raises_centre_tone() {
        use crate::core::fixed::tables::sine;
        use crate::core::fixed::units::Phase;
        let centre = 3_000u32;
        let rate = 48_000u32;
        let peak = |gain_milli_db: i16| -> i32 {
            let mut d = Device::param_eq(centre as u16, 12, gain_milli_db);
            d.prepare(rate);
            let inc = (((centre as u64) << 16) / rate as u64) as u16;
            let mut ph = 0u16;
            let mut p = 0i32;
            for n in 0..800 {
                let s = (sine(Phase::from_raw(ph)).raw() as i32 * 4_000) >> 15;
                let (l, _) = d.process((Amp::from_q15_i16(s as i16), Amp::from_q15_i16(s as i16)));
                if n >= 400 {
                    p = p.max(l.as_q15_i16().unsigned_abs() as i32);
                }
                ph = ph.wrapping_add(inc);
            }
            p
        };
        assert!(peak(12_000) > peak(0) + 1_000, "boost did not raise centre");
        assert!(peak(-12_000) < peak(0) - 500, "cut did not lower centre");
    }

    #[test]
    fn distortion_unprepared_is_passthrough_and_chain_inert() {
        // Bare device un-prepared.
        let mut d = Device::distortion(0, Q15::from_ratio(61, 100), 2_000, 1_000, 6_000);
        let s = (Amp::from_q15_i16(15_000), Amp::from_q15_i16(-7_000));
        assert_eq!(d.process(s), s, "un-prepared distortion not pass-through");

        // A bypassed chain holding a prepared distortion is bit-exact.
        let mut chain = DeviceChain {
            devices: alloc::vec![Device::distortion(
                -3_000,
                Q15::from_ratio(49, 100),
                2_000,
                1_000,
                6_000
            )],
            bypass: true,
        };
        chain.prepare(48_000);
        assert_eq!(chain.process(s), s);
    }

    #[test]
    fn distortion_adds_harmonics_and_drives_signal() {
        // A driven sine should clip toward the rails (peak grows / sticks
        // high relative to a clean unity path), proving the waveshaper is
        // engaged after prepare.
        use crate::core::fixed::tables::sine;
        use crate::core::fixed::units::Phase;
        let mut d = Device::distortion(0, Q15::ONE, 8_000, 4_000, 8_000); // max edge
        d.prepare(48_000);
        let inc = (((2_000u64) << 16) / 48_000) as u16;
        let mut ph = 0u16;
        let mut energy_in = 0i64;
        let mut energy_out = 0i64;
        for _ in 0..400 {
            let s = (sine(Phase::from_raw(ph)).raw() as i32 * 3_000) >> 15;
            let (l, _) = d.process((Amp::from_q15_i16(s as i16), Amp::from_q15_i16(s as i16)));
            energy_in += (s as i64) * (s as i64);
            let o = l.as_q15_i16() as i64;
            energy_out += o * o;
            ph = ph.wrapping_add(inc);
        }
        // High drive lifts a small input substantially.
        assert!(
            energy_out > energy_in,
            "distortion did not add gain/harmonics"
        );
    }

    /// Steady output peak of a sine tone driven through a device long
    /// enough for the envelope to settle.
    fn steady_tone_peak(d: &mut Device, freq: u32, rate: u32, ampl: i32) -> i32 {
        use crate::core::fixed::tables::sine;
        use crate::core::fixed::units::Phase;
        let inc = (((freq as u64) << 16) / rate as u64) as u16;
        let mut ph = 0u16;
        let mut peak = 0i32;
        let total = 4_000usize; // ample settle time for slow release
        for n in 0..total {
            let s = (sine(Phase::from_raw(ph)).raw() as i32 * ampl) >> 15;
            let (l, _) = d.process((Amp::from_q15_i16(s as i16), Amp::from_q15_i16(s as i16)));
            if n >= total * 3 / 4 {
                peak = peak.max(l.as_q15_i16().unsigned_abs() as i32);
            }
            ph = ph.wrapping_add(inc);
        }
        peak
    }

    #[test]
    fn compressor_unprepared_is_passthrough() {
        let mut d = Device::compressor(-20_000, 1_024, 1_000, 100_000, 0);
        let s = (Amp::from_q15_i16(25_000), Amp::from_q15_i16(-12_000));
        assert_eq!(d.process(s), s);
    }

    #[test]
    fn compressor_below_threshold_is_unity() {
        // −24 dBFS tone, threshold −20 dB ⇒ untouched (make-up 0).
        let mut d = Device::compressor(-20_000, 1_024, 1_000, 100_000, 0);
        d.prepare(48_000);
        let ampl = 2_000; // ≈ −24 dBFS
        let peak = steady_tone_peak(&mut d, 1_000, 48_000, ampl);
        assert!(
            (peak - ampl).abs() < ampl / 8,
            "below-thresh not unity: {peak} vs {ampl}"
        );
    }

    #[test]
    fn compressor_reduces_above_threshold() {
        // ≈ −1 dBFS tone, threshold −20 dB, ratio 4:1 ⇒ ~−14 dB gain
        // reduction ⇒ output ≈ ×0.19. Check it is clearly reduced and in
        // the right ballpark.
        let mut comp = Device::compressor(-20_000, 1_024, 1_000, 100_000, 0);
        comp.prepare(48_000);
        let ampl = 29_000;
        let out = steady_tone_peak(&mut comp, 1_000, 48_000, ampl);
        assert!(out < ampl / 2, "not compressed: {out} vs in {ampl}");
        // Expected ≈ 0.19·29000 ≈ 5500; allow a wide tolerance band.
        assert!((3_000..=9_000).contains(&out), "gain reduction off: {out}");

        // Higher ratio ⇒ more reduction.
        let mut hard = Device::compressor(-20_000, 25_600, 1_000, 100_000, 0); // 100:1
        hard.prepare(48_000);
        let out_hard = steady_tone_peak(&mut hard, 1_000, 48_000, ampl);
        assert!(
            out_hard < out,
            "100:1 not harder than 4:1: {out_hard} vs {out}"
        );
    }

    #[test]
    fn compressor_makeup_lifts_quiet_signal() {
        // Below threshold, +12 dB make-up ⇒ ×~4 (clamped by headroom).
        let mut d = Device::compressor(-20_000, 1_024, 1_000, 100_000, 12_000);
        d.prepare(48_000);
        let ampl = 4_000;
        let peak = steady_tone_peak(&mut d, 1_000, 48_000, ampl);
        assert!(
            peak > ampl + ampl / 2,
            "make-up did not lift: {peak} vs {ampl}"
        );
    }

    /// Run an impulse (then silence) through a device and return the
    /// per-sample left-channel output magnitude.
    fn impulse_response(d: &mut Device, n: usize) -> alloc::vec::Vec<i32> {
        let mut out = alloc::vec::Vec::with_capacity(n);
        for i in 0..n {
            let x = if i == 0 { 32_767 } else { 0 };
            let (l, _) = d.process((Amp::from_q15_i16(x), Amp::from_q15_i16(x)));
            out.push(l.as_q15_i16() as i32);
        }
        out
    }

    fn argmax(v: &[i32]) -> usize {
        let mut bi = 0;
        let mut bv = i32::MIN;
        for (i, &x) in v.iter().enumerate() {
            if x.abs() > bv {
                bv = x.abs();
                bi = i;
            }
        }
        bi
    }

    #[test]
    fn echo_unprepared_is_passthrough() {
        let mut d = Device::echo(Q15::HALF, Q15::from_ratio(1, 2), 100, 120, false);
        let s = (Amp::from_q15_i16(20_000), Amp::from_q15_i16(-9_000));
        assert_eq!(d.process(s), s);
    }

    #[test]
    fn echo_taps_at_delay_time() {
        // 10 ms delay at 48 kHz = 480 samples; full wet, no feedback ⇒
        // a single echo of the impulse at exactly that offset.
        let mut d = Device::echo(Q15::ONE, Q15::ZERO, 10, 10, false);
        d.prepare(48_000);
        let resp = impulse_response(&mut d, 1_200);
        let peak = argmax(&resp);
        assert!(
            (peak as i64 - 480).abs() <= 1,
            "echo at {peak}, expected 480"
        );
        // Only one echo (feedback 0): nothing significant after it.
        assert!(resp[960].abs() < 100, "spurious second echo: {}", resp[960]);
    }

    #[test]
    fn echo_feedback_decays_monotonically() {
        // 5 ms delay = 240 samples; feedback 0.5 ⇒ echoes at 240, 480,
        // 720 … each smaller than the last, none blowing up.
        let mut d = Device::echo(Q15::ONE, Q15::from_ratio(1, 2), 5, 5, false);
        d.prepare(48_000);
        let resp = impulse_response(&mut d, 1_600);
        let e1 = resp[240].abs();
        let e2 = resp[480].abs();
        let e3 = resp[720].abs();
        assert!(e1 > e2 && e2 > e3, "not decaying: {e1} {e2} {e3}");
        assert!(e3 > 0, "tail died too early");
        // ~0.5 ratio between successive echoes (band [0.3, 0.7]).
        assert!(
            e2 * 10 > e1 * 3 && e2 * 10 < e1 * 7,
            "feedback ratio off: {e1}→{e2}"
        );
    }

    /// Drive a sine and return `(min, max)` of the per-window output peak
    /// across the run (first window skipped as transient). A modulating
    /// effect produces a wide spread; a static one a narrow one.
    fn windowed_peak_spread(
        d: &mut Device,
        freq: u32,
        rate: u32,
        ampl: i32,
        total: usize,
        win: usize,
    ) -> (i32, i32) {
        use crate::core::fixed::tables::sine;
        use crate::core::fixed::units::Phase;
        let inc = (((freq as u64) << 16) / rate as u64) as u16;
        let mut ph = 0u16;
        let mut lo = i32::MAX;
        let mut hi = 0i32;
        let mut wpeak = 0i32;
        for n in 0..total {
            let s = (sine(Phase::from_raw(ph)).raw() as i32 * ampl) >> 15;
            let (l, _) = d.process((Amp::from_q15_i16(s as i16), Amp::from_q15_i16(s as i16)));
            wpeak = wpeak.max(l.as_q15_i16().unsigned_abs() as i32);
            if n % win == win - 1 {
                if n >= win {
                    // skip the very first window (transient)
                    lo = lo.min(wpeak);
                    hi = hi.max(wpeak);
                }
                wpeak = 0;
            }
            ph = ph.wrapping_add(inc);
        }
        (lo, hi)
    }

    #[test]
    fn moddelay_unprepared_is_passthrough() {
        let mut d = Device::mod_delay(
            ModDelayMode::Chorus,
            Q15::HALF,
            Q15::HALF,
            Q15::ZERO,
            2_000,
            true,
            512,
            true,
        );
        let s = (Amp::from_q15_i16(18_000), Amp::from_q15_i16(-6_000));
        assert_eq!(d.process(s), s);
    }

    #[test]
    fn flanger_modulates_a_steady_tone() {
        // Flanger with depth + feedback ⇒ a swept comb ⇒ the output of a
        // steady tone rises and falls over the LFO cycle.
        let mut fl = Device::mod_delay(
            ModDelayMode::Flanger,
            Q15::HALF,
            Q15::from_ratio(85, 100),  // deep
            Q15::from_ratio(-61, 100), // negative feedback (flanger notch)
            8_000,                     // 8 Hz LFO (fast, for a short test)
            true,
            512, // 2 ms base delay (Q8.8)
            false,
        );
        fl.prepare(48_000);
        let (lo, hi) = windowed_peak_spread(&mut fl, 1_000, 48_000, 16_000, 12_000, 400);
        assert!(hi - lo > 2_000, "flanger did not modulate: lo {lo} hi {hi}");

        // depth = 0 ⇒ a static comb ⇒ steady output (narrow spread).
        let mut flat = Device::mod_delay(
            ModDelayMode::Flanger,
            Q15::HALF,
            Q15::ZERO,
            Q15::from_ratio(-61, 100),
            8_000,
            true,
            512,
            false,
        );
        flat.prepare(48_000);
        let (lo2, hi2) = windowed_peak_spread(&mut flat, 1_000, 48_000, 16_000, 12_000, 400);
        assert!(hi2 - lo2 < (hi - lo), "depth=0 should modulate less");
    }

    #[test]
    fn chorus_is_stable_and_audible() {
        // A chorus should pass a stable, non-silent signal (no runaway).
        let mut ch = Device::mod_delay(
            ModDelayMode::Chorus,
            Q15::HALF,
            Q15::from_ratio(61, 100),
            Q15::from_ratio(12, 100), // light feedback
            3_000,                    // 3 Hz
            true,
            2_560, // 10 ms base (Q8.8)
            true,
        );
        ch.prepare(48_000);
        let (lo, hi) = windowed_peak_spread(&mut ch, 800, 48_000, 14_000, 16_000, 500);
        assert!(lo > 1_000, "chorus went silent: lo {lo}");
        assert!(hi < 32_767, "chorus ran away to clipping: hi {hi}");
    }

    #[test]
    fn reverb_unprepared_is_passthrough() {
        let mut d = Device::reverb(
            ReverbMode::Freeverb,
            Q15::from_ratio(61, 100),
            Q15::from_ratio(49, 100),
            Q15::ONE,
            Q15::from_ratio(31, 100),
            Q15::from_ratio(31, 100),
        );
        let s = (Amp::from_q15_i16(15_000), Amp::from_q15_i16(-7_000));
        assert_eq!(d.process(s), s);
    }

    #[test]
    fn reverb_produces_decaying_bounded_tail() {
        let mut d = Device::reverb(
            ReverbMode::Freeverb,
            Q15::from_ratio(73, 100), // largish room
            Q15::from_ratio(31, 100),
            Q15::ONE,
            Q15::from_ratio(1, 3), // wet ≈ 1.0 (×scalewet)
            Q15::ZERO,             // no dry — measure the tail alone
        );
        d.prepare(48_000);
        let resp = impulse_response(&mut d, 24_000);
        // Energy in an early window vs a late window of the tail.
        let win_energy = |from: usize, to: usize| -> i64 {
            resp[from..to]
                .iter()
                .map(|&x| (x as i64) * (x as i64))
                .sum()
        };
        let early = win_energy(1_000, 3_000);
        let late = win_energy(20_000, 22_000);
        assert!(early > 0, "no reverb tail produced");
        assert!(
            late < early,
            "tail did not decay: early {early} late {late}"
        );
        // Never runs away: stays well inside full scale.
        let peak = resp.iter().map(|&x| x.unsigned_abs()).max().unwrap();
        assert!(peak < 32_767, "reverb clipped/ran away: peak {peak}");
    }

    #[test]
    fn reverb_dry_only_is_passthrough() {
        // wet = 0, dry = 0.5 ⇒ dry gain ×scaledry(2) = unity ⇒ exact
        // pass-through of the direct signal.
        let mut d = Device::reverb(
            ReverbMode::WavesReverb,
            Q15::from_ratio(61, 100),
            Q15::from_ratio(49, 100),
            Q15::ONE,
            Q15::ZERO,
            Q15::HALF,
        );
        d.prepare(48_000);
        for raw in [0i16, 12_345, -9_000, 25_000] {
            let (l, _) = d.process((Amp::from_q15_i16(raw), Amp::from_q15_i16(raw)));
            assert!(
                (l.as_q15_i16() as i32 - raw as i32).abs() <= 2,
                "dry mix off at {raw}"
            );
        }
    }

    #[test]
    fn reverb_scales_to_any_sample_rate() {
        // The comb / all-pass lengths are tuned at 44.1 kHz and rescaled
        // to the run rate (`len = tuning · rate / 44100`), so the reverb
        // must work — decaying, bounded tail, no panic — at any rate,
        // 48 kHz and beyond included (the player is never assumed 44.1).
        for &rate in &[22_050u32, 44_100, 48_000, 96_000] {
            let mut d = Device::reverb(
                ReverbMode::Freeverb,
                Q15::from_ratio(67, 100),
                Q15::from_ratio(24, 100),
                Q15::ONE,
                Q15::from_ratio(1, 3),
                Q15::ZERO,
            );
            d.prepare(rate);
            let n = (rate / 2) as usize; // 0.5 s
            let resp = impulse_response(&mut d, n);
            let mid: i64 = resp[n / 8..n / 4].iter().map(|&x| (x as i64).pow(2)).sum();
            let peak = resp.iter().map(|&x| x.unsigned_abs()).max().unwrap();
            assert!(mid > 0, "no reverb tail at {rate} Hz");
            assert!(peak < 32_767, "reverb clipped/ran away at {rate} Hz");
        }
    }

    #[test]
    fn reverb_wet_and_dry_are_independent() {
        // Dry at unity plus a wet send adds reverb energy ON TOP of the
        // dry signal (not a crossfade): the wet+dry render has strictly
        // more energy than dry-only.
        let render = |wet: Q15, dry: Q15| -> i64 {
            let mut d = Device::reverb(
                ReverbMode::Freeverb,
                Q15::from_ratio(67, 100),
                Q15::from_ratio(24, 100),
                Q15::ONE,
                wet,
                dry,
            );
            d.prepare(48_000);
            // A short tone burst then silence; sum output energy.
            let mut e = 0i64;
            for n in 0..6_000 {
                let x = if n < 500 {
                    (n as i16 % 64 - 32) * 400
                } else {
                    0
                };
                let (l, _) = d.process((Amp::from_q15_i16(x), Amp::from_q15_i16(x)));
                e += (l.as_q15_i16() as i64).pow(2);
            }
            e
        };
        let dry_only = render(Q15::ZERO, Q15::HALF);
        let wet_plus_dry = render(Q15::from_ratio(24, 100), Q15::HALF);
        assert!(
            wet_plus_dry > dry_only,
            "wet did not add on top of dry: {wet_plus_dry} vs {dry_only}"
        );
    }

    #[test]
    fn reverb_freeze_sustains_the_tail() {
        // Two identical reverbs are excited, then silence is fed. The
        // frozen one keeps its tail (unit feedback, muted input); the
        // free one decays. The frozen late tail must dwarf the free one.
        let late_tail = |freeze: bool| -> i64 {
            let mut d = Device::reverb(
                ReverbMode::Freeverb,
                Q15::from_ratio(61, 100),
                Q15::from_ratio(12, 100),
                Q15::ONE,
                Q15::from_ratio(1, 3),
                Q15::ZERO,
            );
            d.prepare(48_000);
            // Fill the network with a burst.
            for n in 0..400 {
                let x = (n as i16 % 50 - 25) * 600;
                d.process((Amp::from_q15_i16(x), Amp::from_q15_i16(x)));
            }
            if freeze {
                d.set_param(5, Q15::ONE); // engage freeze
            }
            // Feed silence; measure energy in a late window.
            let mut e = 0i64;
            for n in 0..40_000 {
                let (l, _) = d.process((Amp::SILENCE, Amp::SILENCE));
                if n >= 36_000 {
                    e += (l.as_q15_i16() as i64).pow(2);
                }
            }
            e
        };
        let frozen = late_tail(true);
        let free = late_tail(false);
        assert!(
            frozen > free * 8,
            "freeze did not sustain: frozen {frozen} free {free}"
        );
    }

    #[test]
    fn i3dl2_unprepared_is_passthrough() {
        let mut d = Device::i3dl2_reverb(0, 1_500, 1_000, -1_000, 20, -1_000, 40);
        let s = (Amp::from_q15_i16(15_000), Amp::from_q15_i16(-7_000));
        assert_eq!(d.process(s), s);
    }

    #[test]
    fn i3dl2_has_early_reflections_and_decaying_tail() {
        // Room 0 dB, audible reflections + reverb, 20 ms pre-delay.
        let mut d = Device::i3dl2_reverb(0, 2_000, 1_000, 0, 20, 0, 30);
        d.prepare(48_000);
        let resp = impulse_response(&mut d, 20_000);
        // An early reflection appears shortly after the pre-delay
        // (20 ms + first tap 7 ms ≈ 27 ms ≈ 1296 samples), before the
        // late tail builds.
        let early_peak = resp[1_000..2_500]
            .iter()
            .map(|&x| x.unsigned_abs())
            .max()
            .unwrap();
        assert!(early_peak > 50, "no early reflections: {early_peak}");
        // The late tail decays.
        let mid: i64 = resp[3_000..5_000].iter().map(|&x| (x as i64).pow(2)).sum();
        let late: i64 = resp[17_000..19_000]
            .iter()
            .map(|&x| (x as i64).pow(2))
            .sum();
        assert!(mid > 0 && late < mid, "tail did not decay: {mid} → {late}");
        // Past the direct impulse (n=0) the wet tail must stay bounded
        // (no instability); the dry impulse itself is full-scale.
        let tail_peak = resp[100..].iter().map(|&x| x.unsigned_abs()).max().unwrap();
        assert!(tail_peak < 32_767, "i3dl2 tail ran away: {tail_peak}");
    }

    #[test]
    fn i3dl2_longer_decay_extends_tail() {
        let tail_energy = |decay_ms: u16| -> i64 {
            let mut d = Device::i3dl2_reverb(0, decay_ms, 1_500, -20_000, 10, 0, 20);
            d.prepare(48_000);
            let resp = impulse_response(&mut d, 24_000);
            resp[18_000..22_000]
                .iter()
                .map(|&x| (x as i64).pow(2))
                .sum()
        };
        let short = tail_energy(300);
        let long = tail_energy(12_000);
        assert!(
            long > short,
            "longer DecayTime did not extend tail: {short} vs {long}"
        );
    }

    #[test]
    fn gargle_unprepared_is_passthrough() {
        let mut d = Device::gargle(50, false);
        let s = (Amp::from_q15_i16(20_000), Amp::from_q15_i16(-12_000));
        assert_eq!(d.process(s), s);
    }

    #[test]
    fn gargle_triangle_modulates_amplitude() {
        // A steady DC-ish input through a triangle gargle must trace a
        // 0→peak→0 amplitude envelope over one cycle.
        let mut d = Device::gargle(100, false); // 100 Hz
        d.prepare(48_000); // period = 480 frames
        let period = 480usize;
        let mut peaks = Vec::new();
        for _ in 0..period {
            let (l, _) = d.process((Amp::from_q15_i16(30_000), Amp::from_q15_i16(30_000)));
            peaks.push(l.as_q15_i16() as i32);
        }
        // Starts near silence, reaches near full near the middle, returns
        // near silence at the end.
        assert!(peaks[0] < 1_000, "start not silent: {}", peaks[0]);
        assert!(
            peaks[period / 2] > 25_000,
            "mid not loud: {}",
            peaks[period / 2]
        );
        assert!(
            peaks[period - 1] < 2_000,
            "end not silent: {}",
            peaks[period - 1]
        );
    }

    #[test]
    fn gargle_square_gates_audio() {
        // Square: full for the first half, silent for the second.
        let mut d = Device::gargle(100, true);
        d.prepare(48_000); // period = 480
        let mut first_half_loud = false;
        let mut second_half_silent = true;
        for n in 0..480 {
            let (l, _) = d.process((Amp::from_q15_i16(20_000), Amp::from_q15_i16(20_000)));
            let v = l.as_q15_i16().unsigned_abs() as i32;
            if n < 240 && v > 19_000 {
                first_half_loud = true;
            }
            if n >= 240 && v != 0 {
                second_half_silent = false;
            }
        }
        assert!(first_half_loud, "square first half not passing audio");
        assert!(second_half_silent, "square second half not silenced");
    }

    #[test]
    fn lowpass_is_stateful() {
        // A step into a low-cutoff filter rises gradually: each output
        // depends on the previous one (proves per-sample state).
        let mut d = Device::LowPass {
            cutoff: Q15::from_ratio(12, 100), // ~0.12
            state: (0, 0),
        };
        let step = Amp::from_q15_i16(20_000);
        let y1 = d.process((step, step)).0.as_q15_i16();
        let y2 = d.process((step, step)).0.as_q15_i16();
        let y3 = d.process((step, step)).0.as_q15_i16();
        assert!(
            0 < y1 && y1 < y2 && y2 < y3 && y3 < 20_000,
            "{y1} {y2} {y3}"
        );
    }
}
