//! [`InstrOpl`](crate::core::instr_opl::InstrOpl) — Yamaha OPL FM
//! instrument (AdLib / Sound Blaster).
//!
//! Opaque to the xmrs sample engine: the player skips voices whose
//! `instr_type` is `InstrumentType::Opl`. Carries the OPL register
//! layout (carrier + modulator operators, feedback, algorithm) that
//! a downstream OPL emulator can consume directly. Populated by the
//! S3M importer when the source file uses Adlib instruments.
//!
//! **Always compiled**, in every feature configuration — a data type with no
//! emulator dependency; only its render engine (`generators::opl`)
//! gates on `synth_opl`. See [`crate::core::instrument`] for why the file
//! format depends on that.

use serde::{Deserialize, Serialize};

use crate::core::fixed::units::Finetune;

/// MDI_OPLREGS structure
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug)]
pub struct MdiOpl {
    /// 2 bits, Key scaling level (OPL: 0x40, bits 6-7)
    pub ksl: u8,
    /// 4 bits, Frequency multiplier (OPL: 0x20, bits 0-3)
    pub multiple: u8,
    /// 3 bits, modulator self-feedback amount, 0=none..7=max
    /// [channel-level: op 0 only, op 1 ignored] (OPL: 0xC0, bits 1-3)
    pub feedback: u8,
    /// 4 bits (OPL: 0x60, bits 4-7)
    pub attack: u8,
    /// 4 bits (OPL: 0x80, bits 4-7)
    pub sustain: u8,
    /// EG type / sustaining envelope: true = level held at sustain
    /// while keyed, false = percussive (decays to 0) (OPL: 0x20, bit 5)
    pub eg: bool,
    /// 4 bits (OPL: 0x60, bits 0-3)
    pub decay: u8,
    /// 4 bits (OPL: 0x80, bits 0-3)
    pub release: u8,
    /// 6 bits [0=loudest, 63=silent] (OPL: 0x40, bits 0-5)
    pub total_level: u8,
    /// Amplitude modulation (Tremolo) (OPL: 0x20, bit 7)
    pub am: bool,
    /// Frequency vibrato (OPL: 0x20, bit 6)
    pub vib: bool,
    /// Key-scaling rate: true = envelope rates scale with pitch (OPL: 0x20, bit 4)
    pub ksr: bool,
    /// Connection / synthesis type: false = FM (modulator → carrier),
    /// true = additive (both operators sum to output).
    /// [channel-level: op 0 only, op 1 ignored] (OPL: 0xC0, bit 0)
    pub con: bool,
}

#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug)]
pub struct MdiInstr {
    /// Register values for the modulator operator (op 0)
    pub modulator: MdiOpl,
    /// Register values for the carrier operator (op 1)
    pub carrier: MdiOpl,
    /// Modulator waveform shape, 0..7 (OPL: 0xE0)
    pub modulator_wave_select: u8,
    /// Carrier waveform shape, 0..7 (OPL: 0xE0)
    pub carrier_wave_select: u8,
}

/// Yamaha OPL with module compatibility
/// OPL rhythm-mode role of an instrument. `Melodic` (default) is an ordinary two-operator FM voice
/// on a dynamically-allocated channel. The five percussion roles map to the fixed rhythm channels
/// (BD = channel 6, HH/SD = channel 7 modulator/carrier, TT/CY = channel 8 modulator/carrier) and
/// are keyed via the rhythm register (0xBD). In S3M these come from the Adlib instrument type byte
/// (2 = melody, 3 = bass drum, 4 = snare, 5 = tom-tom, 6 = cymbal, 7 = hi-hat).
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub enum OplRhythm {
    #[default]
    Melodic,
    BassDrum,
    Snare,
    TomTom,
    Cymbal,
    HiHat,
}

#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug)]
pub struct InstrOpl {
    /// The two-operator FM voice (modulator + carrier + wave selects)
    pub element: MdiInstr,
    /// Instrument output volume, 0..63 (0 = silent, 63 = loudest)
    pub volume: u8,
    /// [-96..95] with 0 <=> C-4
    pub relative_pitch: i8,
    /// Sub-semitone detune, Q1.15. In tracker convention,
    /// `±0x4000` raw ≈ `±0.5` semitone (one full step span);
    /// `Finetune::ZERO` = on the integer grid.
    pub finetune: Finetune,
    /// OPL rhythm-mode role (default `Melodic` — ordinary FM voice).
    pub rhythm: OplRhythm,
}
