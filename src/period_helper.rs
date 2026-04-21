use serde::{Deserialize, Serialize};

#[cfg(feature = "micromath")]
#[allow(unused_imports)]
use micromath::F32Ext;
#[cfg(feature = "libm")]
#[allow(unused_imports)]
use num_traits::float::Float;

/// Historical Frequencies to load old data. Default is Linear.
#[derive(Default, Serialize, Deserialize, Clone, Copy, Debug)]
pub enum FrequencyType {
    AmigaFrequencies,
    #[default]
    LinearFrequencies,
}

/// Single-entry memoization of the most recent
/// `all_to_frequency` call. The dominant access pattern is a held
/// note producing the same key across every tick of a row; one slot
/// captures that case at 100% hit rate, with zero book-keeping. Any
/// change in period, arpeggio offset, finetune (including vibrato
/// modulation), or glissando mode just overwrites the entry.
#[derive(Clone, Copy)]
struct LastCall {
    key: (f32, f32, f32, bool),
    value: f32,
}

#[derive(Clone)]
pub struct PeriodHelper {
    pub freq_type: FrequencyType,
    /// Behaviour knob: when `true`, `adjust_period` clamps the base
    /// note to ≤ 95 under active arpeggio so the period lookup stays
    /// inside FT2's 0–95 note range (matches `ft2_replayer.c`
    /// arpeggio's `period2NotePeriod`). Off by default — driven from
    /// `module.quirks.ft2_arpeggio_note_clamp` at construction.
    ft2_arpeggio_note_clamp: bool,
    last_call: Option<LastCall>,
}

impl Default for PeriodHelper {
    fn default() -> Self {
        Self {
            freq_type: FrequencyType::LinearFrequencies,
            ft2_arpeggio_note_clamp: false,
            last_call: None,
        }
    }
}

impl PeriodHelper {
    /// historical amiga module sample frequency (Paula chipset related)
    pub const C4_FREQ: f32 = 8363.0;

    pub fn new(freq_type: FrequencyType, ft2_arpeggio_note_clamp: bool) -> Self {
        Self {
            freq_type,
            ft2_arpeggio_note_clamp,
            last_call: None,
        }
    }

    // ==== Linear

    /// return period
    #[inline(always)]
    fn linear_pitch_to_period(note: f32) -> f32 {
        // 10.0: number of octaves
        // 12.0: halftones
        // 16.0: number of finetune steps
        //  4.0: finetune resolution
        10.0 * 12.0 * 16.0 * 4.0 - note * 16.0 * 4.0
    }

    /// return note
    #[inline(always)]
    fn linear_period_to_pitch(period: f32) -> f32 {
        (10.0 * 12.0 * 16.0 * 4.0 - period) / (16.0 * 4.0)
    }

    /// return frequency
    #[inline(always)]
    fn linear_period_to_frequency(period: f32) -> f32 {
        // 8363.0 is historical amiga module sample frequency (Paula chipset related)
        //  6: octave center
        // 12: halftones
        // 64: period resolution (16.0 * 4.0)
        //     16.0: number of finetune steps
        //      4.0: finetune step resolution
        Self::C4_FREQ * (2.0f32).powf((6.0 * 12.0 * 16.0 * 4.0 - period) / (12.0 * 16.0 * 4.0))
    }

    /// return period
    #[inline(always)]
    fn linear_frequency_to_period(freq: f32) -> f32 {
        (6.0 * 12.0 * 16.0 * 4.0) - (12.0 * 16.0 * 4.0) * (freq / Self::C4_FREQ).log2()
    }

    // ==== Amiga

    /// return period
    #[inline(always)]
    fn amiga_pitch_to_period(note: f32) -> f32 {
        /* found using scipy.optimize.curve_fit */
        6848.0 * (-0.0578 * note).exp() + 0.2782
    }

    /// return note
    #[inline(always)]
    fn amiga_period_to_pitch(period: f32) -> f32 {
        -f32::ln((period - 0.2782) / 6848.0) / 0.0578
    }

    /// return frequency
    #[inline(always)]
    fn amiga_period_to_frequency(period: f32) -> f32 {
        if period == 0.0 {
            0.0
        } else {
            // 7159090.5 / (period * 2.0) // NTSC
            7093789.2 / (period * 2.0) // PAL
        }
    }

    /// return period
    #[inline(always)]
    fn amiga_frequency_to_period(freq: f32) -> f32 {
        if freq == 0.0 {
            0.0
        } else {
            // 7159090.5 / (freq * 2.0) // NTSC
            7093789.2 / (freq * 2.0) // PAL
        }
    }

    // ==== Generic

    pub fn note_to_period(&self, note: f32) -> f32 {
        match self.freq_type {
            FrequencyType::LinearFrequencies => Self::linear_pitch_to_period(note),
            FrequencyType::AmigaFrequencies => Self::amiga_pitch_to_period(note),
        }
    }

    pub fn period_to_pitch(&self, period: f32) -> f32 {
        match self.freq_type {
            FrequencyType::LinearFrequencies => Self::linear_period_to_pitch(period),
            FrequencyType::AmigaFrequencies => Self::amiga_period_to_pitch(period),
        }
        .max(0.0) // Remove < 0.0 and NaN numbers
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

    /// returns C-4 frequency from relative note and finetune
    pub fn relative_pitch_to_c4freq(&self, relative_pitch: f32, finetune: f32) -> Option<f32> {
        const NOTE_C4: f32 = 4.0 * 12.0;
        const NOTE_B9: f32 = 10.0 * 12.0 - 1.0;

        let note = NOTE_C4 + relative_pitch;
        if !(0.0..=NOTE_B9).contains(&note) {
            return None;
        }
        let c4_period = self.note_to_period(note + finetune);
        Some(self.period_to_frequency(c4_period))
    }

    /// return relative note and finetune
    pub fn c4freq_to_relative_pitch(&self, freq: f32) -> (i8, f32) {
        const NOTE_C4: f32 = 4.0 * 12.0;
        let period = self.frequency_to_period(freq);
        let note = self.period_to_pitch(period);
        let note_ceil = note.ceil();
        let relative_pitch = note_ceil - NOTE_C4;
        let finetune = note - note_ceil;
        (relative_pitch as i8, finetune)
    }

    //-----------------------------------------------------

    /// new adjust period to arpeggio and finetune delta
    pub fn adjust_period(&self, period: f32, arp_pitch: f32, finetune: f32, semitone: bool) -> f32 {
        let note_orig: f32 = self.period_to_pitch(period);

        let note = if semitone {
            note_orig.round()
        } else {
            note_orig
        };

        if self.ft2_arpeggio_note_clamp && arp_pitch != 0.0 {
            // FT2-canonical note clamp when arpeggio is active: FT2's
            // arpeggio period lookup walks a 0..95 note range
            // (`period2NotePeriod` in ft2_replayer.c), so any base note
            // whose perturbed index would land at 96 or above is snapped
            // back to 95. Without this, the arpeggio on very high notes
            // can compute a negative index and return a nonsense period.
            let mut note = note;
            if note.ceil() >= 95.0 {
                note = 95.0;
            }
            self.note_to_period(note + arp_pitch + finetune)
        } else {
            self.note_to_period(note + arp_pitch + finetune)
        }
    }

    /// Do all the work in one pass.
    pub fn all_to_frequency(
        &self,
        period: f32,
        arp_pitch: f32,
        finetune: f32,
        semitone: bool,
    ) -> f32 {
        let period_adjusted = self.adjust_period(period, arp_pitch, finetune, semitone);
        self.period_to_frequency(period_adjusted)
    }

    /// Same as [`Self::all_to_frequency`] with single-entry
    /// memoization. The dominant call pattern is a held note
    /// re-evaluating the same key every tick of a row, so a single
    /// slot captures ~95% of calls at zero book-keeping cost. Any
    /// change in the key (vibrato, slide, arpeggio, instrument
    /// swap) just overwrites the slot — no LRU, no scan.
    pub fn all_to_frequency_cached(
        &mut self,
        period: f32,
        arp_pitch: f32,
        finetune: f32,
        semitone: bool,
    ) -> f32 {
        let key = (period, arp_pitch, finetune, semitone);
        if let Some(last) = self.last_call {
            if last.key == key {
                return last.value;
            }
        }
        let value = self.all_to_frequency(period, arp_pitch, finetune, semitone);
        self.last_call = Some(LastCall { key, value });
        value
    }
}
