//! [`InstrSid`](crate::core::instr_sid::InstrSid) — Commodore 64 MOS6581
//! SID voice (one of three).
//!
//! Opaque to the xmrs sample engine: the player skips voices whose
//! `instr_type` is `InstrumentType::Sid`. Models the SID's
//! waveform/pulse-width/ADSR controls plus the filter routing and
//! the sync / ring-modulation links to its neighbours. Populated by
//! the SID importer.
//!
//! **Always compiled**, in every feature configuration — a data type with no
//! emulator dependency; only its render engine (`generators::sid`)
//! gates on `synth_sid`. See [`crate::core::instrument`] for why the file
//! format depends on that.

use serde::{Deserialize, Serialize};

/// MOS6581 SID Voice
///
/// Synchronize & ring modulation:
/// - voice0: from voice2
/// - voice1: from voice0
/// - voice2: from voice1
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug)]
pub struct SidVoice {
    /// Oscillator frequency, raw 16-bit SID register (0..65535).
    /// Hz ≈ `freq * Fclk / 16777216` (Fclk ≈ 985248 PAL / 1022727 NTSC).
    pub freq: u16,
    /// Pulse duty cycle, 12-bit (0..4095); 2048 = 50 % square.
    /// Only audible when `ctrl_pulse` is set.
    pub pw: u16,
    pub ctrl_noise: bool,    // 7
    pub ctrl_pulse: bool,    // 6
    pub ctrl_sawtooth: bool, // 5
    pub ctrl_triangle: bool, // 4
    pub ctrl_test: bool,     // 3
    /// ring modulation (2->0, 0->1, 1->2)
    pub ctrl_rm: bool, // 2
    /// synchronize with (2->0, 0->1, 1->2)
    pub ctrl_sync: bool, // 1
    pub ctrl_gate: bool,     // 0
    /// Packed envelope register `$D405`: attack = high nibble (`ad >> 4`,
    /// 0..15), decay = low nibble (`ad & 0x0F`, 0..15). A GUI must split
    /// the two nibbles into separate sliders.
    pub ad: u8,
    /// Packed envelope register `$D406`: sustain = high nibble (`sr >> 4`,
    /// 0..15 level), release = low nibble (`sr & 0x0F`, 0..15). A GUI must
    /// split the two nibbles into separate sliders.
    pub sr: u8,
}

impl SidVoice {
    pub fn update_from_ctrl_register(&mut self, ctrl: u8) {
        // 7 noise
        if ctrl & 0b1000_0000 != 0 {
            self.ctrl_noise = true;
        }
        // 6 pulse
        if ctrl & 0b0100_0000 != 0 {
            self.ctrl_pulse = true;
        }
        // 5 sawtooth
        if ctrl & 0b0010_0000 != 0 {
            self.ctrl_sawtooth = true;
        }
        // 4 triangle
        if ctrl & 0b0001_0000 != 0 {
            self.ctrl_triangle = true;
        }
        // 3 test
        if ctrl & 0b0000_1000 != 0 {
            self.ctrl_test = true;
        }
        // 2 ring modulation with voice X
        if ctrl & 0b0000_0100 != 0 {
            self.ctrl_rm = true;
        }
        // 1 synchronize with voice X
        if ctrl & 0b0000_0010 != 0 {
            self.ctrl_sync = true;
        }
        // 0 gate
        if ctrl & 0b0000_0001 != 0 {
            self.ctrl_gate = true;
        }
    }

    pub fn generate_ctrl_register(&self) -> u8 {
        let mut ctrl = 0;

        // 7 noise
        if self.ctrl_noise {
            ctrl |= 0b1000_0000;
        }
        // 6 pulse
        if self.ctrl_pulse {
            ctrl |= 0b0100_0000;
        }
        // 5 sawtooth
        if self.ctrl_sawtooth {
            ctrl |= 0b0010_0000;
        }
        // 4 triangle
        if self.ctrl_triangle {
            ctrl |= 0b0001_0000;
        }
        // 3 test
        if self.ctrl_test {
            ctrl |= 0b0000_1000;
        }
        // 2 ring modulation with voice X
        if self.ctrl_rm {
            ctrl |= 0b0000_0100;
        }
        // 1 synchronize with voice X
        if self.ctrl_sync {
            ctrl |= 0b0000_0010;
        }
        // 0 gate
        if self.ctrl_gate {
            ctrl |= 0b0000_0001;
        }

        ctrl
    }
}

/// MOS6581 SID Instrument
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct InstrSid {
    /// The three SID oscillator voices (sync/ring links are fixed: see above).
    pub voice: [SidVoice; 3],
    pub fc: u16, // Filter cutoff frequency (0..2047) - Used by (Low, Band, High)Pass
    pub filter_resonance: u8, // u4 (0..15) - Amount of feedback a filter receives from itself
    pub filter_gate: [bool; 4], // 0 for voice0, ..., 3 for external input
    pub low_pass: bool,
    pub band_pass: bool,
    pub high_pass: bool,
    pub mute_voice3: bool,
    pub main_volume: u8, // u4 (0..15)
}

impl Default for InstrSid {
    fn default() -> Self {
        Self {
            voice: [SidVoice::default(); 3],
            fc: 0,
            filter_resonance: 0,
            filter_gate: [false; 4],
            low_pass: false,
            band_pass: false,
            high_pass: false,
            mute_voice3: false,
            main_volume: 15,
        }
    }
}
