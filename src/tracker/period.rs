//! Pitch ↔ period ↔ frequency conversion for tracker formats.
//!
//! Tracker note pitch is described in two equivalent but
//! distinct systems:
//!
//! - **Linear**
//!   ([`FrequencyType::LinearFrequencies`]):
//!   a logarithmic note → period mapping pivoted on a chosen
//!   reference C-4 frequency. Hardware-independent; period
//!   is a unit on a pitch axis with 768 units per octave.
//!   Used by FT2's "linear frequencies" mode and natively
//!   by S3M / IT.
//!
//! - **Amiga**
//!   ([`FrequencyType::AmigaFrequencies`]):
//!   period directly drives the Paula chip via
//!   `freq = 7093789.2 / (period × 2)` (PAL clock). The
//!   note → period mapping is exponential
//!   (`period = K × 2⁻ⁿᐟ¹²`). Used by FT2's "Amiga
//!   frequencies" mode and natively by `.mod`.
//!
//! Both modes share the same triangle pitch ↔ period ↔ freq,
//! dispatched by [`PeriodHelper`].
//!
//! ## Q-format pitch chain
//!
//! All conversions use typed fixed-point values from
//! [`crate::core::fixed::units`]:
//!
//! - [`Pitch`] (Q8.8): semitones from
//!   C-0, fractional.
//! - [`PitchDelta`] (Q8.8): signed
//!   pitch offset (arpeggio, portamento, vibrato, finetune).
//! - [`Period`] (u16): tracker period
//!   (linear or Amiga).
//! - `Frequency` (Q16.16): Hz, output of the conversion.
//!
//! No `f32` enters the pitch chain — every conversion is an
//! exact-integer table lookup or short binary search. The
//! audio path is integer-only.

use serde::{Deserialize, Serialize};

use crate::core::fixed::tables::{
    amiga_frequency_to_period, amiga_period_from_pitch_base, amiga_period_to_frequency,
    amiga_period_to_pitch_base, linear_frequency_to_period, linear_period_to_frequency,
    linear_period_to_pitch, linear_pitch_to_period, AMIGA_C0_PERIOD, AMIGA_C0_PERIOD_LEGACY,
};
use crate::core::fixed::units::{Frequency, Period, Pitch, PitchDelta};

/// Frequency interpretation for the active module.
#[derive(Default, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
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

/// Top of FT2's arpeggio note table.
const FT2_ARPEGGIO_NOTE_MAX: i16 = 95;
/// Tracker note index of MIDI C-4.
const NOTE_C4: i16 = 48;
/// Highest valid tracker note (B-9).
const NOTE_B9: i16 = 119;

/// Pitch ↔ period ↔ frequency dispatcher.
///
/// One `PeriodHelper` per active player: it bundles the per-module
/// frequency mode (`Linear` vs. `Amiga`) and the FT2 arpeggio-clamp
/// quirk, and exposes the conversion entry points the runtime
/// audio path uses to drive each voice's pitch chain.
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

    /// When `true`, use `AMIGA_C0_PERIOD_LEGACY` (6848) for Amiga
    /// periods rather than `AMIGA_C0_PERIOD` (6779).
    legacy_amiga_period_base: bool,

    /// Single-entry memoization for [`Self::all_to_frequency_cached`].
    /// The dominant access pattern is a held note evaluating the
    /// same key on every tick of a row, so one slot captures
    /// ~95% of calls at zero book-keeping cost. Any change in
    /// the key just overwrites the slot.
    cache_key: Option<(Period, PitchDelta, PitchDelta, bool)>,
    cache_value: Frequency,
}

impl Default for PeriodHelper {
    fn default() -> Self {
        Self::new(FrequencyType::default(), false)
    }
}

impl PeriodHelper {
    pub fn new(freq_type: FrequencyType, legacy_arpeggio_clamp: bool) -> Self {
        Self::with_quirks(freq_type, legacy_arpeggio_clamp, false)
    }

    pub fn with_quirks(
        freq_type: FrequencyType,
        legacy_arpeggio_clamp: bool,
        legacy_amiga_period_base: bool,
    ) -> Self {
        Self {
            freq_type,
            legacy_arpeggio_clamp,
            legacy_amiga_period_base,
            cache_key: None,
            cache_value: Frequency::ZERO,
        }
    }

    /// Whether [`Self::adjust_period`] applies FT2's
    /// arpeggio note clamp at note > 95. Read-only accessor
    /// (the field is set at construction).
    #[inline]
    pub fn legacy_arpeggio_clamp_enabled(&self) -> bool {
        self.legacy_arpeggio_clamp
    }

    /// Base period used for Amiga period calculations (6848 vs 6779).
    #[inline]
    pub fn amiga_base_period(&self) -> u32 {
        if self.legacy_amiga_period_base {
            AMIGA_C0_PERIOD_LEGACY
        } else {
            AMIGA_C0_PERIOD
        }
    }
}

// =====================================================================
// Generic dispatchers — typed Q-format triangle
// =====================================================================

impl PeriodHelper {
    /// Pitch (Q8.8 semitones) → period.
    ///
    /// Linear mode is an exact integer mapping
    /// (`period = 7680 - pitch · 64`). Amiga walks the LUTs
    /// directly from the Q8.8 pitch via
    /// [`amiga_period_from_pitch`], preserving the full 1/64-
    /// semitone resolution (no FT2-nibble quantization).
    pub fn note_to_period(&self, pitch: Pitch) -> Period {
        match self.freq_type {
            FrequencyType::LinearFrequencies => linear_pitch_to_period(pitch),
            FrequencyType::AmigaFrequencies => {
                amiga_period_from_pitch_base(pitch, self.amiga_base_period())
            }
        }
    }

    /// Period → pitch in semitones (Q8.8). Returns 0 (C-0) for
    /// invalid (zero) inputs.
    pub fn period_to_pitch(&self, period: Period) -> Pitch {
        match self.freq_type {
            FrequencyType::LinearFrequencies => linear_period_to_pitch(period),
            FrequencyType::AmigaFrequencies => {
                amiga_period_to_pitch_base(period, self.amiga_base_period())
            }
        }
    }

    /// Period → frequency (Q16.16 Hz).
    pub fn period_to_frequency(&self, period: Period) -> Frequency {
        match self.freq_type {
            FrequencyType::LinearFrequencies => linear_period_to_frequency(period),
            FrequencyType::AmigaFrequencies => amiga_period_to_frequency(period),
        }
    }

    /// Frequency (Q16.16) → period.
    pub fn frequency_to_period(&self, freq: Frequency) -> Period {
        match self.freq_type {
            FrequencyType::LinearFrequencies => linear_frequency_to_period(freq),
            FrequencyType::AmigaFrequencies => amiga_frequency_to_period(freq),
        }
    }
}

// =====================================================================
// Sample-tagging conversions (used at import / editor time)
// =====================================================================

impl PeriodHelper {
    /// Compute the C-4 frequency at which a sample with the
    /// given `relative_pitch` (semitones from C-4) and
    /// `finetune` (fractional semitone offset) plays. Returns
    /// `None` if the resulting absolute note falls outside
    /// `[0, 119]`.
    pub fn relative_pitch_to_c4freq(
        &self,
        relative_pitch: i8,
        finetune: PitchDelta,
    ) -> Option<Frequency> {
        // C-4 is the integer reference at semitone 48; add the
        // signed `relative_pitch` to land on the sample's
        // tagged note.
        let note_int = NOTE_C4 + relative_pitch as i16;
        if !(0..=NOTE_B9).contains(&note_int) {
            return None;
        }
        let pitch = Pitch::from_semitone(note_int).shift(finetune);
        let period = self.note_to_period(pitch);
        Some(self.period_to_frequency(period))
    }

    /// Convert a sample's natural C-4 frequency back to
    /// `(relative_pitch, finetune)`. Inverse of
    /// [`Self::relative_pitch_to_c4freq`].
    ///
    /// Uses round-to-nearest, so `finetune ∈ [-0.5, +0.5]
    /// semitones` — always the smallest correction within the
    /// surrounding semitone.
    pub fn c4freq_to_relative_pitch(&self, freq: Frequency) -> (i8, PitchDelta) {
        let period = self.frequency_to_period(freq);
        let pitch = self.period_to_pitch(period);
        // Round-to-nearest semitone.
        let nearest_int = pitch.semitone_round().clamp(0, NOTE_B9);
        let relative_pitch = (nearest_int - NOTE_C4) as i8;
        // finetune = pitch - nearest_semitone (signed Q8.8).
        let ft_raw = pitch.as_q8_8_i32() - ((nearest_int as i32) << 8);
        let finetune = PitchDelta::from_q8_8_i16(ft_raw as i16);
        (relative_pitch, finetune)
    }
}

// =====================================================================
// Per-tick adjustment + cached frequency lookup
// =====================================================================

impl PeriodHelper {
    /// Resolve the post-effect period for one tick: take the
    /// current period, decompose to a pitch (optionally
    /// quantised to a semitone for glissando), apply arpeggio
    /// and finetune offsets, re-encode to a period.
    pub fn adjust_period(
        &self,
        period: Period,
        arp_pitch: PitchDelta,
        finetune: PitchDelta,
        semitone: bool,
    ) -> Period {
        let raw_pitch = self.period_to_pitch(period);
        let mut pitch = if semitone {
            raw_pitch.quantized()
        } else {
            raw_pitch
        };

        if self.legacy_arpeggio_clamp && arp_pitch.as_q8_8_i16() != 0 {
            // FT2's arpeggio period lookup walks the 0..95 note
            // range; any base note that would land at 96+ snaps
            // to 95 to avoid a negative-index nonsense period.
            // Ceil semantics: any fractional pitch ≥ 95 should
            // saturate, so we compare the rounded-up pitch.
            if pitch.semitone_ceil() >= FT2_ARPEGGIO_NOTE_MAX {
                pitch = Pitch::from_semitone(FT2_ARPEGGIO_NOTE_MAX);
            }
        }

        let pitch = pitch.shift(arp_pitch).shift(finetune);
        self.note_to_period(pitch)
    }

    /// One-pass `period → adjusted period → frequency`.
    pub fn all_to_frequency(
        &self,
        period: Period,
        arp_pitch: PitchDelta,
        finetune: PitchDelta,
        semitone: bool,
    ) -> Frequency {
        let adjusted = self.adjust_period(period, arp_pitch, finetune, semitone);
        self.period_to_frequency(adjusted)
    }

    /// Same as [`Self::all_to_frequency`] with single-entry
    /// memoization. The dominant call pattern is a held note
    /// re-evaluating the same key every tick of a row, so one
    /// slot captures ~95% of calls at zero book-keeping cost.
    pub fn all_to_frequency_cached(
        &mut self,
        period: Period,
        arp_pitch: PitchDelta,
        finetune: PitchDelta,
        semitone: bool,
    ) -> Frequency {
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

    fn helper_linear() -> PeriodHelper {
        PeriodHelper::new(FrequencyType::LinearFrequencies, false)
    }

    fn helper_amiga() -> PeriodHelper {
        PeriodHelper::new(FrequencyType::AmigaFrequencies, false)
    }

    #[test]
    fn linear_c4_period_round_trips() {
        let ph = helper_linear();
        let p_c4 = ph.note_to_period(Pitch::C4);
        // Linear C-4 lives at period 4608 by construction.
        assert_eq!(p_c4.raw(), 4608);
        let back = ph.period_to_pitch(p_c4);
        assert_eq!(back, Pitch::C4);
    }

    #[test]
    fn amiga_a4_round_trips() {
        let ph = helper_amiga();
        let p_a4 = ph.note_to_period(Pitch::A4);
        // Amiga A-4 should round-trip back to A-4 (within
        // grid resolution of 1/8 semitone = 32 in Q8.8).
        let back = ph.period_to_pitch(p_a4);
        let diff = (back.as_q8_8_i32() - Pitch::A4.as_q8_8_i32()).abs();
        assert!(diff <= 32, "diff = {} Q8.8 units", diff);
    }

    #[test]
    fn c4freq_round_trip_at_reference() {
        let ph = helper_linear();
        let f_c4 = ph.period_to_frequency(ph.note_to_period(Pitch::C4));
        let (rp, ft) = ph.c4freq_to_relative_pitch(f_c4);
        assert_eq!(rp, 0);
        // Finetune should be near zero (round-trip fidelity).
        assert!(ft.as_q8_8_i16().abs() <= 1);
    }
}
