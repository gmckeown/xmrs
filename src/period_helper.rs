//! Pitch ↔ period ↔ frequency conversion for tracker formats.
//!
//! Tracker note pitch is described in two equivalent but
//! distinct systems:
//!
//! - **Linear** ([`FrequencyType::LinearFrequencies`](crate::period_helper::FrequencyType::LinearFrequencies)): a
//!   logarithmic note → period mapping pivoted on a chosen
//!   reference C-4 frequency. Hardware-independent; period
//!   is a unit on a pitch axis with 768 units per octave.
//!   Used by FT2's "linear frequencies" mode and natively
//!   by S3M / IT.
//!
//! - **Amiga** ([`FrequencyType::AmigaFrequencies`](crate::period_helper::FrequencyType::AmigaFrequencies)): period
//!   directly drives the Paula chip via
//!   `freq = 7093789.2 / (period × 2)` (PAL clock). The
//!   note → period mapping is exponential
//!   (`period = K × 2⁻ⁿᐟ¹²`). Used by FT2's "Amiga
//!   frequencies" mode and natively by `.mod`.
//!
//! Both modes share the same triangle pitch ↔ period ↔ freq,
//! dispatched by [`PeriodHelper`](crate::period_helper::PeriodHelper).
//!
//! ## Tuning
//!
//! Linear ([`PeriodHelper::C4_FREQ`](crate::period_helper::PeriodHelper::C4_FREQ)) and Amiga pivots are
//! chosen so that A-4 = 440 Hz exactly — modules play at
//! concert pitch and mix cleanly with any 440-Hz-aligned
//! source. This sits ≈ 1.9 cents above the historical FT2 /
//! OpenMPT tuning ([`PeriodHelper::C4_FREQ_LEGACY`](crate::period_helper::PeriodHelper::C4_FREQ_LEGACY) = 8363,
//! [`PeriodHelper::AMIGA_C0_PERIOD_LEGACY`](crate::period_helper::PeriodHelper::AMIGA_C0_PERIOD_LEGACY) = 6848); the
//! difference is imperceptible in isolated playback but
//! audible as a 0.9 Hz beat against MIDI sources, vs 1.5 Hz
//! at the legacy tuning.

use serde::{Deserialize, Serialize};

// `no_std` float-method backend. Pulls inherent methods like
// `.exp()`, `.log2()`, `.powf()` into scope when std isn't
// available. Priority: std (default) > libm > micromath.
#[cfg(all(not(feature = "std"), not(feature = "libm"), feature = "micromath"))]
#[allow(unused_imports)]
use micromath::F32Ext;
#[cfg(all(not(feature = "std"), feature = "libm"))]
#[allow(unused_imports)]
use num_traits::float::Float;

/// Named magic numbers — kept private to avoid polluting the
/// public API but visible to all `impl` blocks below.
mod consts {
    /// Twelve semitones per octave.
    pub const NOTES_PER_OCTAVE: u32 = 12;
    /// Ten octaves of tracker note range.
    pub const OCTAVES: u32 = 10;
    /// Total notes in tracker range: `[0, 120)`.
    pub const NOTES_TOTAL: u32 = OCTAVES * NOTES_PER_OCTAVE;

    /// Linear-mode period grid: 16 finetune steps per semitone.
    pub const FINETUNE_STEPS: u32 = 16;
    /// Four fine units per finetune step.
    pub const FINE_RESOLUTION: u32 = 4;
    /// Period units per semitone: `16 × 4 = 64`.
    pub const PERIOD_PER_SEMITONE: u32 = FINETUNE_STEPS * FINE_RESOLUTION;
    /// Period units per octave: `12 × 64 = 768`.
    pub const PERIOD_PER_OCTAVE: u32 = NOTES_PER_OCTAVE * PERIOD_PER_SEMITONE;
    /// Maximum linear period: `10 × 768 = 7680` (= note 0 = C-0).
    pub const PERIOD_MAX: u32 = OCTAVES * PERIOD_PER_OCTAVE;

    /// Linear pivot octave: C-4 sits 6 octaves above C-0.
    pub const PIVOT_OCTAVE: u32 = 6;
    /// Linear period at the pivot: `6 × 768 = 4608`.
    pub const PIVOT_PERIOD: u32 = PIVOT_OCTAVE * PERIOD_PER_OCTAVE;

    /// Paula PAL clock divided by 2 (per-sample tick rate).
    pub const PAULA_PAL_CLOCK_HALF: f32 = 7_093_789.2 / 2.0;

    /// Tracker note index of MIDI C-4 (= 4 × 12 = 48).
    pub const NOTE_C4: f32 = 4.0 * NOTES_PER_OCTAVE as f32;
    /// Highest valid tracker note (B-9 = 119).
    pub const NOTE_B9: f32 = NOTES_TOTAL as f32 - 1.0;

    /// Arpeggio safety bound: FT2's lookup table walks 0..95.
    pub const FT2_ARPEGGIO_NOTE_MAX: f32 = 95.0;
}

use consts::*;

/// Frequency interpretation for the active module.
#[derive(Default, Serialize, Deserialize, Clone, Copy, Debug)]
pub enum FrequencyType {
    /// Amiga period model: `freq = K / period`. Drives Paula
    /// directly on `.mod` (the historical case), and is used as
    /// a pure arithmetic convention by S3M and by FT2's "Amiga
    /// mode" — both run on PC but inherit the period semantics.
    AmigaFrequencies,
    /// Linear: logarithmic note → period, hardware-independent.
    /// Used by FT2 (linear mode) and IT.
    #[default]
    LinearFrequencies,
}

#[derive(Clone)]
pub struct PeriodHelper {
    /// Frequency mode of the loaded module.
    pub freq_type: FrequencyType,

    /// FT2-specific arpeggio quirk: when `true`,
    /// [`Self::adjust_period`] clamps the base note to ≤ 95
    /// under active arpeggio so the period lookup stays inside
    /// FT2's 0–95 note range (matches `ft2_replayer.c`
    /// arpeggio's `period2NotePeriod`). Off by default; driven
    /// from a module-level quirk flag at construction.
    legacy_arpeggio_clamp: bool,

    /// Single-entry memoization for [`Self::all_to_frequency_cached`].
    /// The dominant access pattern is a held note evaluating the
    /// same key on every tick of a row, so one slot captures
    /// ~95% of calls at zero book-keeping cost. Any change in
    /// the key just overwrites the slot.
    cache_key: Option<(f32, f32, f32, bool)>,
    cache_value: f32,
}

// =====================================================================
// Constants
// =====================================================================

impl PeriodHelper {
    /// Reference C-4 frequency for linear mode (= A-4 × 2⁻⁹ᐟ¹² × 32).
    ///
    /// Aligned with modern concert pitch: A-4 = 440 Hz exact,
    /// C-4 = 261.6256 Hz, and the canonical "× 32" octave
    /// factor maps tracker C-4 to its sample-rate equivalent
    /// (8372.018 Hz). See the module-level documentation for
    /// the rationale and trade-offs.
    pub const C4_FREQ: f32 = 8372.01809;

    /// Soundtracker / ProTracker / FT2 reference C-4. Originates
    /// from Karsten Obarski's Ultimate Soundtracker (1987),
    /// where the period table was derived from NTSC Amiga
    /// programming examples: `3_579_545 / 428 ≈ 8363.4`,
    /// rounded to 8363. Inherited unchanged by NoiseTracker,
    /// ProTracker and FT2, then frozen into the XM format and
    /// adopted by the rest of the ecosystem (OpenMPT,
    /// MilkyTracker, libxmp, ft2-clone, schism). ≈ 1.9 cents
    /// below modern concert pitch. Use [`Self::C4_FREQ`] for
    /// the MIDI-aligned reference; this constant is exported
    /// for bit-numerical comparison with legacy players only.
    pub const C4_FREQ_LEGACY: f32 = 8363.0;

    /// Reference C-0 period for Amiga mode, MIDI-aligned.
    ///
    /// Derived from C-5 MIDI = 523.2511306 Hz under the Paula
    /// PAL clock: `7_093_789.2 / (523.2511306 × 2) ≈ 6778.57`.
    /// Internal use only — cross-cuts the same MIDI alignment
    /// as [`Self::C4_FREQ`] so the two modes agree on absolute
    /// pitch.
    const AMIGA_C0_PERIOD: f32 = 7_093_789.2 / (523.2511306 * 2.0);

    /// Historical ProTracker / FT2 C-0 period. Drives Paula at
    /// ≈ 518 Hz for "C-0 tracker", ≈ 17 cents above the modern
    /// MIDI alignment. Use only for bit-numerical comparison
    /// with legacy players.
    pub const AMIGA_C0_PERIOD_LEGACY: u32 = 6848;
}

// =====================================================================
// Construction
// =====================================================================

impl Default for PeriodHelper {
    fn default() -> Self {
        Self::new(FrequencyType::default(), false)
    }
}

impl PeriodHelper {
    pub fn new(freq_type: FrequencyType, legacy_arpeggio_clamp: bool) -> Self {
        Self {
            freq_type,
            legacy_arpeggio_clamp,
            cache_key: None,
            cache_value: 0.0,
        }
    }
}

// =====================================================================
// Linear mode (pure functions)
// =====================================================================

impl PeriodHelper {
    /// Linear: integer note → period. `period = 7680 - note × 64`.
    #[inline(always)]
    fn linear_pitch_to_period(note: f32) -> f32 {
        PERIOD_MAX as f32 - note * PERIOD_PER_SEMITONE as f32
    }

    /// Linear: period → note in semitones (possibly fractional).
    #[inline(always)]
    fn linear_period_to_pitch(period: f32) -> f32 {
        (PERIOD_MAX as f32 - period) / PERIOD_PER_SEMITONE as f32
    }

    /// Linear: period → frequency. Pivots on [`Self::C4_FREQ`]
    /// at `period = PIVOT_PERIOD = 4608`.
    #[inline(always)]
    fn linear_period_to_frequency(period: f32) -> f32 {
        let exponent = (PIVOT_PERIOD as f32 - period) / PERIOD_PER_OCTAVE as f32;
        Self::C4_FREQ * (2.0_f32).powf(exponent)
    }

    /// Linear: frequency → period. Inverse of
    /// [`Self::linear_period_to_frequency`].
    #[inline(always)]
    fn linear_frequency_to_period(freq: f32) -> f32 {
        PIVOT_PERIOD as f32 - PERIOD_PER_OCTAVE as f32 * (freq / Self::C4_FREQ).log2()
    }
}

// =====================================================================
// Amiga mode (pure functions)
// =====================================================================

impl PeriodHelper {
    /// Amiga: note → period. `period = AMIGA_C0_PERIOD × 2⁻ⁿᐟ¹²`.
    /// Pure equal-temperament, no empirical correction term —
    /// Paula-perfect.
    #[inline(always)]
    fn amiga_pitch_to_period(note: f32) -> f32 {
        Self::AMIGA_C0_PERIOD * (-note * core::f32::consts::LN_2 / 12.0).exp()
    }

    /// Amiga: period → note. Returns 0 for non-positive input.
    #[inline(always)]
    fn amiga_period_to_pitch(period: f32) -> f32 {
        if period <= 0.0 {
            return 0.0;
        }
        -(period / Self::AMIGA_C0_PERIOD).log2() * 12.0
    }

    /// Amiga: period → frequency via the Paula PAL chip formula.
    #[inline(always)]
    fn amiga_period_to_frequency(period: f32) -> f32 {
        if period == 0.0 {
            0.0
        } else {
            PAULA_PAL_CLOCK_HALF / period
        }
    }

    /// Amiga: frequency → period. Inverse of the chip formula.
    #[inline(always)]
    fn amiga_frequency_to_period(freq: f32) -> f32 {
        if freq == 0.0 {
            0.0
        } else {
            PAULA_PAL_CLOCK_HALF / freq
        }
    }
}

// =====================================================================
// Generic dispatchers
// =====================================================================

impl PeriodHelper {
    pub fn note_to_period(&self, note: f32) -> f32 {
        match self.freq_type {
            FrequencyType::LinearFrequencies => Self::linear_pitch_to_period(note),
            FrequencyType::AmigaFrequencies => Self::amiga_pitch_to_period(note),
        }
    }

    /// Period → note in semitones. Negative results and NaN are
    /// clamped to 0 (`.max(0.0)` returns the rhs for NaN inputs
    /// per IEEE 754).
    pub fn period_to_pitch(&self, period: f32) -> f32 {
        let note = match self.freq_type {
            FrequencyType::LinearFrequencies => Self::linear_period_to_pitch(period),
            FrequencyType::AmigaFrequencies => Self::amiga_period_to_pitch(period),
        };
        note.max(0.0)
    }

    pub fn period_to_frequency(&self, period: f32) -> f32 {
        match self.freq_type {
            FrequencyType::LinearFrequencies => Self::linear_period_to_frequency(period),
            FrequencyType::AmigaFrequencies => Self::amiga_period_to_frequency(period),
        }
    }

    pub fn frequency_to_period(&self, freq: f32) -> f32 {
        match self.freq_type {
            FrequencyType::LinearFrequencies => Self::linear_frequency_to_period(freq),
            FrequencyType::AmigaFrequencies => Self::amiga_frequency_to_period(freq),
        }
    }
}

// =====================================================================
// Sample-tagging conversions (used at import / editor time)
// =====================================================================

impl PeriodHelper {
    /// Compute the C-4 frequency at which a sample with the
    /// given `relative_pitch` (semitones from C-4) and
    /// `finetune` (fraction of a semitone) plays. Returns
    /// `None` if the resulting absolute note falls outside
    /// `[0, 119]`.
    pub fn relative_pitch_to_c4freq(&self, relative_pitch: f32, finetune: f32) -> Option<f32> {
        let note = NOTE_C4 + relative_pitch;
        if !(0.0..=NOTE_B9).contains(&note) {
            return None;
        }
        let period = self.note_to_period(note + finetune);
        Some(self.period_to_frequency(period))
    }

    /// Convert a sample's natural C-4 frequency back to
    /// `(relative_pitch, finetune)`. Inverse of
    /// [`Self::relative_pitch_to_c4freq`].
    ///
    /// Uses round-to-nearest, so `finetune ∈ [-0.5, +0.5]` —
    /// always the smallest correction within the surrounding
    /// semitone. The previous `ceil()`-based version had a
    /// discontinuity at integer note boundaries: a frequency
    /// 1 cent above C-4 returned `(1, -0.99)` instead of
    /// `(0, +0.01)`, breaking imports of any sample tagged
    /// just above a semitone.
    ///
    /// `relative_pitch` saturates to the `i8` range (sufficient
    /// for the `[-48, +71]` valid-note range).
    pub fn c4freq_to_relative_pitch(&self, freq: f32) -> (i8, f32) {
        let period = self.frequency_to_period(freq);
        let note = self.period_to_pitch(period);
        let nearest = note.round();
        let relative_pitch = (nearest - NOTE_C4) as i8;
        let finetune = note - nearest;
        (relative_pitch, finetune)
    }
}

// =====================================================================
// Per-tick adjustment + cached frequency lookup
// =====================================================================

impl PeriodHelper {
    /// Resolve the post-effect period for one tick: take the
    /// current period, decompose to a note (optionally quantised
    /// to a semitone for glissando), apply arpeggio and finetune
    /// offsets, re-encode to a period.
    pub fn adjust_period(&self, period: f32, arp_pitch: f32, finetune: f32, semitone: bool) -> f32 {
        let raw_note = self.period_to_pitch(period);
        let mut note = if semitone { raw_note.round() } else { raw_note };

        if self.legacy_arpeggio_clamp && arp_pitch != 0.0 && note.ceil() >= FT2_ARPEGGIO_NOTE_MAX {
            // FT2's arpeggio period lookup walks the 0..95 note
            // range; any base note that would land at 96+ snaps
            // to 95 to avoid a negative-index nonsense period.
            note = FT2_ARPEGGIO_NOTE_MAX;
        }

        self.note_to_period(note + arp_pitch + finetune)
    }

    /// One-pass `period → adjusted period → frequency`.
    pub fn all_to_frequency(
        &self,
        period: f32,
        arp_pitch: f32,
        finetune: f32,
        semitone: bool,
    ) -> f32 {
        let adjusted = self.adjust_period(period, arp_pitch, finetune, semitone);
        self.period_to_frequency(adjusted)
    }

    /// Same as [`Self::all_to_frequency`] with single-entry
    /// memoization. The dominant call pattern is a held note
    /// re-evaluating the same key every tick of a row, so one
    /// slot captures ~95% of calls at zero book-keeping cost.
    /// NaN keys never match (NaN ≠ NaN), so they always
    /// recompute — correct fallback behaviour.
    pub fn all_to_frequency_cached(
        &mut self,
        period: f32,
        arp_pitch: f32,
        finetune: f32,
        semitone: bool,
    ) -> f32 {
        let key = (period, arp_pitch, finetune, semitone);
        if self.cache_key == Some(key) {
            return self.cache_value;
        }
        let value = self.all_to_frequency(period, arp_pitch, finetune, semitone);
        self.cache_key = Some(key);
        self.cache_value = value;
        value
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// `C4_FREQ` must match the MIDI-aligned formula. Catches
    /// edits to the literal that drift from the canonical value.
    #[test]
    fn c4_freq_matches_midi_alignment() {
        let expected = 440.0_f32 * 2.0_f32.powf(-9.0 / 12.0) * 32.0;
        assert!(
            (PeriodHelper::C4_FREQ - expected).abs() < 0.01,
            "C4_FREQ drifted: got {}, expected {}",
            PeriodHelper::C4_FREQ,
            expected
        );
    }

    /// `AMIGA_C0_PERIOD` must drive Paula at MIDI C-5 = 523.2511306 Hz.
    #[test]
    fn amiga_c0_period_matches_midi_c5() {
        let expected = 7_093_789.2_f32 / (523.2511306 * 2.0);
        assert!((PeriodHelper::AMIGA_C0_PERIOD - expected).abs() < 0.001);
    }

    /// At `C4_FREQ` exactly, the round-trip yields `(0, 0.0)`
    /// in both modes.
    #[test]
    fn c4freq_roundtrip_at_reference() {
        for mode in [
            FrequencyType::LinearFrequencies,
            FrequencyType::AmigaFrequencies,
        ] {
            let ph = PeriodHelper::new(mode, false);
            let (rp, ft) = ph.c4freq_to_relative_pitch(PeriodHelper::C4_FREQ);
            assert_eq!(rp, 0, "mode {:?}", mode);
            assert!(ft.abs() < 0.001, "mode {:?}: ft = {}", mode, ft);
        }
    }

    /// Bug regression: a frequency slightly above C-4 must
    /// give `(0, +small)`, not `(1, -large)` as the old
    /// `ceil()` implementation did.
    #[test]
    fn c4freq_slightly_high_gives_small_positive_finetune() {
        let ph = PeriodHelper::default();
        // +10 cents above C-4 (= 0.1 semitone)
        let slightly_high = PeriodHelper::C4_FREQ * 2.0_f32.powf(0.1 / 12.0);
        let (rp, ft) = ph.c4freq_to_relative_pitch(slightly_high);
        assert_eq!(rp, 0, "should not overflow into next semitone");
        assert!(ft > 0.0 && ft < 0.5, "ft = {}", ft);
    }

    /// A frequency closer to the next semitone rounds up cleanly.
    #[test]
    fn c4freq_near_next_semitone_rounds_up() {
        let ph = PeriodHelper::default();
        // +90 cents above C-4 — should round up to C#-4 (rp = 1)
        let nearly_csharp = PeriodHelper::C4_FREQ * 2.0_f32.powf(0.9 / 12.0);
        let (rp, ft) = ph.c4freq_to_relative_pitch(nearly_csharp);
        assert_eq!(rp, 1);
        assert!(ft.abs() < 0.5);
    }

    /// `relative_pitch_to_c4freq ∘ c4freq_to_relative_pitch ≈ id`
    /// across realistic sample frequencies.
    #[test]
    fn c4freq_roundtrip_general() {
        let ph = PeriodHelper::default();
        for freq in [4000.0, 8000.0, 8372.0, 10000.0, 16000.0, 22050.0, 44100.0] {
            let (rp, ft) = ph.c4freq_to_relative_pitch(freq);
            let back = ph.relative_pitch_to_c4freq(rp as f32, ft).unwrap();
            let ratio = back / freq;
            assert!(
                (ratio - 1.0).abs() < 1e-3,
                "freq {} → ({}, {}) → {} (ratio {})",
                freq,
                rp,
                ft,
                back,
                ratio
            );
        }
    }

    /// Cache hit returns the same value as a fresh compute.
    #[test]
    fn cache_hit_matches_uncached() {
        let mut ph = PeriodHelper::default();
        let cached = ph.all_to_frequency_cached(4608.0, 0.0, 0.0, false);
        let cached_again = ph.all_to_frequency_cached(4608.0, 0.0, 0.0, false);
        let uncached = ph.all_to_frequency(4608.0, 0.0, 0.0, false);
        assert_eq!(cached, cached_again);
        assert_eq!(cached, uncached);
    }

    /// Cache invalidates on any key change.
    #[test]
    fn cache_invalidates_on_key_change() {
        let mut ph = PeriodHelper::default();
        let f1 = ph.all_to_frequency_cached(4608.0, 0.0, 0.0, false);
        let f2 = ph.all_to_frequency_cached(4608.0, 1.0, 0.0, false);
        assert_ne!(f1, f2);
    }

    /// One octave up doubles the frequency (linear mode).
    #[test]
    fn linear_octave_up_doubles_freq() {
        let ph = PeriodHelper::new(FrequencyType::LinearFrequencies, false);
        let f_c4 = ph.period_to_frequency(ph.note_to_period(48.0));
        let f_c5 = ph.period_to_frequency(ph.note_to_period(60.0));
        assert!((f_c5 / f_c4 - 2.0).abs() < 1e-3);
    }

    /// Same in Amiga mode.
    #[test]
    fn amiga_octave_up_doubles_freq() {
        let ph = PeriodHelper::new(FrequencyType::AmigaFrequencies, false);
        let f_c4 = ph.period_to_frequency(ph.note_to_period(48.0));
        let f_c5 = ph.period_to_frequency(ph.note_to_period(60.0));
        assert!((f_c5 / f_c4 - 2.0).abs() < 1e-3);
    }

    /// MIDI alignment: A-4 plays at 440 × 32 = 14080 Hz in both modes.
    #[test]
    fn a4_plays_at_concert_pitch() {
        for mode in [
            FrequencyType::LinearFrequencies,
            FrequencyType::AmigaFrequencies,
        ] {
            let ph = PeriodHelper::new(mode, false);
            let f = ph.period_to_frequency(ph.note_to_period(57.0));
            assert!((f - 14080.0).abs() < 5.0, "mode {:?}: A-4 = {} Hz", mode, f);
        }
    }
}
