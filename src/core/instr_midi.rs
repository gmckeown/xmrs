//! [`InstrMidi`] — instrument routed to an external MIDI synth
//! rather than rendered natively by the sample engine.
//!
//! The xmrs sample engine doesn't render MIDI itself: when a Track
//! triggers an `InstrumentType::Midi`, the player only forwards the
//! event to a registered MIDI observer (channel, program, bank,
//! pitch-bend depth). Downstream code is responsible for the actual
//! synthesis. Editor-authored modules can use this to drive a
//! hardware or software synth from the tracker.

use serde::{Deserialize, Serialize};

/// Instrument routed to an external MIDI synth.
///
/// Carries the MIDI channel, program, bank-MSB / bank-LSB and the
/// pitch-bend depth a downstream MIDI observer should configure
/// before forwarding `NoteOn` / `NoteOff` events. The xmrs sample
/// engine doesn't render this variant — voices keyed to it just
/// surface the events through the player's observer chain.
#[derive(Clone, Default, Serialize, Deserialize, Debug)]
pub struct InstrMidi {
    /// Can be used when `InstrMidi` duplicates `InstrDefault` otherwise use `muted` in `Instrument`
    pub muted: bool,
    /// MIDI channel (0-15)
    pub channel: u8,
    /// MIDI program (1-128)
    pub program: u16,
    /// MIDI bank (0-16384)
    pub bank: u16,
    pub bend: u16,
}
