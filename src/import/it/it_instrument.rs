use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;
use bincode::error::DecodeError;
use serde::Deserialize;
use serde_big_array::BigArray;

use crate::prelude::*;

use super::serde_helper::deserialize_string_12;
use super::serde_helper::deserialize_string_26;
use super::serde_helper::deserialize_string_4;

#[derive(Deserialize, Debug)]
#[repr(C)]
/// IT instrument header (pre-2.0).
pub struct ItInstrumentHeaderPre2 {
    /// Identifier ("IMPI").
    #[serde(deserialize_with = "deserialize_string_4")]
    pub id: String,

    /// DOS filename
    #[serde(deserialize_with = "deserialize_string_12")]
    pub dos_filename: String,

    /// Reserved for future use.
    pub reserved1: u8,

    /// Configuration flags (8 bits).
    /// - Bit 1: Use volume envelope if on.
    /// - Bit 2: Use loop envelope if on.
    /// - Bit 3: Use sustain loop envelope if on.
    pub flags: u8,

    /// Number of loop start node of volume envelope.
    pub loop_start: u8,

    /// Number of loop end node of volume envelope.
    pub loop_end: u8,

    /// Number of sustain loop start node of envelope.
    pub sustain_loop_start: u8,

    /// Number of sustain loop end node of envelope.
    pub sustain_loop_end: u8,

    /// Reserved for future use.
    pub reserved2: u16,

    /// Fadeout value (0-64, but counted by 512).
    pub fadeout: u16,

    /// New Note Action
    /// - 0: Note cut.
    /// - 1: Continue.
    /// - 2: Note off.
    /// - 3: Note fade.
    pub nna: u8,

    /// Disable Note Channel (DNC).
    /// - 0: Disable channel.
    /// - 1: Enable channel.
    pub dnc: u8,

    /// Tracker version (only used in instrument files).
    pub tracker_version: u16,

    /// Number of samples (only used in instrument files).
    pub number_of_samples: u8,

    /// Reserved for future use.
    pub reserved3: u8,

    /// Instrument Name
    #[serde(deserialize_with = "deserialize_string_26")]
    pub instrument_name: String,

    /// Reserved for future use.
    pub reserved4: [u8; 6],

    /// Note-to-sample mapping table
    /// - .0: Note.
    /// - .1: Sample.
    #[serde(with = "BigArray")]
    pub note_sample_keyboard_table: [(u8, u8); 120],
}

impl ItInstrumentHeaderPre2 {
    pub fn is_it_instrument(&self) -> bool {
        self.id == "IMPI"
    }
}

// Volume only
#[derive(Deserialize, Debug)]
#[repr(C)]
pub struct ItEnvelopePre2 {
    /// 0-64, 0xff=end of envelope
    #[serde(with = "BigArray")]
    pub envelope: [u8; 200],

    /// .0 = tick, .1 = magnitude
    pub node_points: [(u8, u8); 25],
}

impl ItEnvelopePre2 {
    /// Return the envelope points in chronological (by-frame) order,
    /// plus a remap table that rewrites file-order indices (loop_start
    /// etc.) into the new sorted order.
    fn to_envelope_points(&self) -> (Vec<EnvelopePoint>, [u8; 25]) {
        // Collect authored nodes up to the `(0, 0)` sentinel.
        let mut indexed: Vec<(usize, EnvelopePoint)> = Vec::new();
        for (orig_idx, (tick, magnitude)) in self.node_points.iter().enumerate() {
            if *tick == 0 && *magnitude == 0 && !indexed.is_empty() {
                break;
            }
            indexed.push((
                orig_idx,
                EnvelopePoint {
                    frame: *tick as usize,
                    value: (*magnitude as f32) / 64.0,
                },
            ));
        }
        indexed.sort_by_key(|(_, p)| p.frame);

        let mut remap = [0u8; 25];
        let points = indexed
            .into_iter()
            .enumerate()
            .map(|(new_idx, (orig_idx, point))| {
                remap[orig_idx] = new_idx as u8;
                point
            })
            .collect();
        (points, remap)
    }

    /// Legacy accessor kept for any caller outside the crate.
    pub fn to_envelope(&self) -> Vec<EnvelopePoint> {
        self.to_envelope_points().0
    }

    fn remap_index(remap: &[u8; 25], raw: u8) -> usize {
        let idx = raw as usize;
        if idx < 25 {
            remap[idx] as usize
        } else {
            0
        }
    }

    /// Build the volume envelope, given the parent instrument's flags
    /// and node indices (which live on the header, not on the envelope
    /// payload in the pre-2.0 format).
    pub fn to_envelope_struct(
        &self,
        flags: u8,
        loop_start: u8,
        loop_end: u8,
        sustain_start: u8,
        sustain_end: u8,
    ) -> Envelope {
        let (points, remap) = self.to_envelope_points();
        Envelope {
            enabled: flags & 0b0000_0001 != 0,
            sustain_enabled: flags & 0b0000_0100 != 0,
            sustain_start_point: Self::remap_index(&remap, sustain_start),
            sustain_end_point: Self::remap_index(&remap, sustain_end),
            loop_enabled: flags & 0b0000_0010 != 0,
            loop_start_point: Self::remap_index(&remap, loop_start),
            loop_end_point: Self::remap_index(&remap, loop_end),
            point: points,
        }
    }
}

// --------------------------------------------------------------------------

#[derive(Deserialize, Debug)]
#[repr(C)]
pub struct ItInstrumentHeaderPost2 {
    /// Instrument identifier - must be "IMPI"
    #[serde(deserialize_with = "deserialize_string_4")]
    pub id: String,

    /// DOS filename
    #[serde(deserialize_with = "deserialize_string_12")]
    pub dos_filename: String,

    /// Reserved
    pub reserved1: u8,

    /// Action to take when a new note is played
    /// 0: Cut the note
    /// 1: Continue the note
    /// 2: Stop the note
    /// 3: Fade out the note
    pub nna: u8,

    /// Duplicate check type
    /// 0: Off
    /// 1: Note
    /// 2: Sample
    /// 3: Instrument
    pub duplicate_check_type: u8,

    /// Action to take when a duplicate is found
    /// 0: Cut the note
    /// 1: Stop the note
    /// 2: Fade out the note
    pub duplicate_check_action: u8,

    /// Fade-out time (0-128, but the actual value is 1024 times larger)
    pub fadeout: i16,

    /// Pitch and pan separation (-32 to 32)
    pub pitch_pan_separation: i8,

    /// Center note for panning (0-119)
    pub pitch_pan_center: u8,

    /// Global volume (0-128)
    pub global_volume: u8,

    /// Default pan (0-64, bit 128 to ignore)
    pub default_pan: u8,

    /// Random volume variation (0-100)
    pub random_volume_variation: u8,

    /// Random pan variation (0-100)
    pub random_pan_variation: u8,

    /// Tracker version used to save the instrument (only used in instrument files)
    pub tracker_version: u16,

    /// Number of samples used by this instrument (only used in instrument files)
    pub num_samples: u8,

    /// Reserved
    pub reserved2: u8,

    /// Instrument name
    #[serde(deserialize_with = "deserialize_string_26")]
    pub instrument_name: String,

    /// Initial filter cutoff frequency (0-127)
    /// The formula used is 110*2^(0.25+ce/fe), where ce is the cutoff frequency * (256 + 256) and fe is 24*512 or 20*512 if using OpenMPT's extended filter range.
    pub initial_filter_cutoff: u8,

    /// Initial filter resonance (0-127)
    /// The formula used is 10^((-resonance*24.0)/(128.0f*20.0f)), but it's generally better to use a precalculated table.
    pub initial_filter_resonance: u8,

    /// MIDI channel (0-16)
    pub midi_channel: u8,

    /// MIDI program (1-128)
    pub midi_program: u8,

    /// MIDI bank (0-16384)
    pub midi_bank: u16,

    /// Note-sample-keyboard table (120 entries)
    /// .0: Note
    /// .1: Sample
    #[serde(with = "BigArray")]
    pub note_sample_keyboard_table: [(u8, u8); 120],
}

impl ItInstrumentHeaderPost2 {
    pub fn is_it_instrument(&self) -> bool {
        self.id == "IMPI"
    }
}

#[derive(Deserialize, Debug, Default)]
#[repr(C)]
pub struct ItEnvelopePost2 {
    /// Envelope flags
    /// - Bit 0: Enable/disable envelope
    /// - Bit 1: Enable/disable loop
    /// - Bit 2: Enable/disable sustain loop
    /// - Bit 3: Reserved (used as envelope carry in OpenMPT)
    /// - Bits 4-6: Reserved
    /// - Bit 7: Use pitch envelope as filter (only applies to pitch envelope)
    pub flags: u8,

    /// Number of valid nodes in the file
    pub node_count: u8,

    /// Starting node of the loop
    pub loop_start: u8,

    /// Ending node of the loop
    pub loop_end: u8,

    /// Starting node of the sustain loop
    pub sustain_loop_start: u8,

    /// Ending node of the sustain loop
    pub sustain_loop_end: u8,

    /// Node points table
    /// - .0: Node value (0-64 for volume and filter, -32 to 32 for pan and pitch)
    /// - .1: Node position in ticks (0-9999)
    pub node_points: [(u8, u16); 25],
    // trailing_bytes: [u8; 7], // 7 bytes if version 2.0 to 2.14, 4 bytes if 2.14p1 or above
}

impl ItEnvelopePost2 {
    /// Convert raw nodes assuming **unsigned** magnitudes in 0..=64
    /// (volume / filter envelopes).
    ///
    /// Also returns the re-mapping table that rewrites loop/sustain
    /// node indices from file order to sorted order — IT stores nodes
    /// in authored order but points must be chronological for
    /// interpolation, so we sort by frame. Any index into the raw
    /// node array (`loop_start`, etc.) must go through this remap to
    /// stay pointing at the same logical node after the sort.
    fn to_envelope_points_unsigned(&self) -> (Vec<EnvelopePoint>, [u8; 25]) {
        let count = (self.node_count as usize).min(25);
        let mut indexed: Vec<(usize, EnvelopePoint)> = self.node_points[..count]
            .iter()
            .enumerate()
            .map(|(orig_idx, (value, tick))| {
                (
                    orig_idx,
                    EnvelopePoint {
                        frame: *tick as usize,
                        value: (*value as f32) / 64.0,
                    },
                )
            })
            .collect();
        indexed.sort_by_key(|(_, p)| p.frame);

        let mut remap = [0u8; 25];
        let points = indexed
            .into_iter()
            .enumerate()
            .map(|(new_idx, (orig_idx, point))| {
                remap[orig_idx] = new_idx as u8;
                point
            })
            .collect();
        (points, remap)
    }

    /// Convert raw nodes assuming **signed** magnitudes in -32..=32
    /// (panning / pitch envelopes — see the IT file format docs on
    /// `ItEnvelopePost2::node_points` for the signed interpretation).
    ///
    /// The magnitude byte is re-interpreted as `i8` and normalised to
    /// the 0..=1 convention used by `Envelope`: 0.0 = fully left /
    /// pitched-down by one octave, 0.5 = centre / no pitch change,
    /// 1.0 = fully right / pitched-up by one octave.
    fn to_envelope_points_signed(&self) -> (Vec<EnvelopePoint>, [u8; 25]) {
        let count = (self.node_count as usize).min(25);
        let mut indexed: Vec<(usize, EnvelopePoint)> = self.node_points[..count]
            .iter()
            .enumerate()
            .map(|(orig_idx, (value, tick))| {
                let signed = *value as i8; // -32..=32 in the format
                let normalised = (signed as f32 + 32.0) / 64.0; // → 0.0..=1.0
                (
                    orig_idx,
                    EnvelopePoint {
                        frame: *tick as usize,
                        value: normalised,
                    },
                )
            })
            .collect();
        indexed.sort_by_key(|(_, p)| p.frame);

        let mut remap = [0u8; 25];
        let points = indexed
            .into_iter()
            .enumerate()
            .map(|(new_idx, (orig_idx, point))| {
                remap[orig_idx] = new_idx as u8;
                point
            })
            .collect();
        (points, remap)
    }

    /// Legacy unsigned conversion, kept for any callers outside the
    /// crate that relied on the original `to_envelope()` API. New code
    /// should use `to_envelope_struct` / `to_envelope_struct_signed`.
    pub fn to_envelope(&self) -> Vec<EnvelopePoint> {
        self.to_envelope_points_unsigned().0
    }

    fn remap_index(remap: &[u8; 25], raw: u8) -> usize {
        let idx = raw as usize;
        if idx < 25 {
            remap[idx] as usize
        } else {
            0
        }
    }

    /// Build an envelope using the **unsigned** magnitude interpretation.
    /// Use for volume and filter-cutoff envelopes.
    pub fn to_envelope_struct(&self) -> Envelope {
        let (points, remap) = self.to_envelope_points_unsigned();
        Envelope {
            enabled: self.flags & 0b0000_0001 != 0,
            sustain_enabled: self.flags & 0b0000_0100 != 0,
            sustain_start_point: Self::remap_index(&remap, self.sustain_loop_start),
            sustain_end_point: Self::remap_index(&remap, self.sustain_loop_end),
            loop_enabled: self.flags & 0b0000_0010 != 0,
            loop_start_point: Self::remap_index(&remap, self.loop_start),
            loop_end_point: Self::remap_index(&remap, self.loop_end),
            point: points,
        }
    }

    /// Build an envelope using the **signed** magnitude interpretation.
    /// Use for panning and pitch envelopes.
    pub fn to_envelope_struct_signed(&self) -> Envelope {
        let (points, remap) = self.to_envelope_points_signed();
        Envelope {
            enabled: self.flags & 0b0000_0001 != 0,
            sustain_enabled: self.flags & 0b0000_0100 != 0,
            sustain_start_point: Self::remap_index(&remap, self.sustain_loop_start),
            sustain_end_point: Self::remap_index(&remap, self.sustain_loop_end),
            loop_enabled: self.flags & 0b0000_0010 != 0,
            loop_start_point: Self::remap_index(&remap, self.loop_start),
            loop_end_point: Self::remap_index(&remap, self.loop_end),
            point: points,
        }
    }
}

// --------------------------------------------------------------------------

#[derive(Deserialize, Debug)]
pub struct ItInstrumentPre2 {
    pub instr: ItInstrumentHeaderPre2,
    pub volume_envelope: ItEnvelopePre2,
}

impl ItInstrumentPre2 {
    pub fn is_it_instrument(&self) -> bool {
        self.instr.is_it_instrument()
    }
}

#[derive(Deserialize, Debug)]
pub struct ItInstrumentPost2 {
    pub instr: ItInstrumentHeaderPost2,
    pub volume_envelope: ItEnvelopePost2,
    pub panning_envelope: ItEnvelopePost2,
    pub pitch_envelope: ItEnvelopePost2,
}

impl ItInstrumentPost2 {
    pub fn is_it_instrument(&self) -> bool {
        self.instr.is_it_instrument()
    }
}

#[derive(Deserialize, Debug)]
pub enum ItInstrument {
    Pre2(ItInstrumentPre2),
    Post2(ItInstrumentPost2),
}

impl ItInstrument {
    pub fn is_it_instrument(&self) -> bool {
        match self {
            ItInstrument::Pre2(i) => i.is_it_instrument(),
            ItInstrument::Post2(i) => i.is_it_instrument(),
        }
    }

    pub fn load_pre2(source: &[u8]) -> Result<Self, DecodeError> {
        let mut data = source;

        let instr_h = bincode::serde::decode_from_slice::<ItInstrumentHeaderPre2, _>(
            data,
            bincode::config::legacy(),
        )?;

        if !instr_h.0.is_it_instrument() {
            return Err(DecodeError::OtherString(
                "Not an IT Instrument?".to_string(),
            ));
        }

        data = &data[instr_h.1..];
        let vol = bincode::serde::decode_from_slice::<ItEnvelopePre2, _>(
            data,
            bincode::config::legacy(),
        )?;
        let instr = ItInstrumentPre2 {
            instr: instr_h.0,
            volume_envelope: vol.0,
        };
        Ok(ItInstrument::Pre2(instr))
    }

    pub fn load_post2(source: &[u8]) -> Result<Self, DecodeError> {
        let mut data = source;

        let instr_h = bincode::serde::decode_from_slice::<ItInstrumentHeaderPost2, _>(
            data,
            bincode::config::legacy(),
        )?;

        if !instr_h.0.is_it_instrument() {
            return Err(DecodeError::OtherString(
                "Not an IT Instrument?".to_string(),
            ));
        }

        data = &data[instr_h.1..];
        let vol = bincode::serde::decode_from_slice::<ItEnvelopePost2, _>(
            data,
            bincode::config::legacy(),
        )?;
        data = &data[1 + vol.1..];
        let pan = bincode::serde::decode_from_slice::<ItEnvelopePost2, _>(
            data,
            bincode::config::legacy(),
        )?;
        data = &data[1 + pan.1..];
        let pitch = bincode::serde::decode_from_slice::<ItEnvelopePost2, _>(
            data,
            bincode::config::legacy(),
        )?;
        let instr = ItInstrumentPost2 {
            instr: instr_h.0,
            volume_envelope: vol.0,
            panning_envelope: pan.0,
            pitch_envelope: pitch.0,
        };
        Ok(ItInstrument::Post2(instr))
    }

    pub fn prepare_instrument(&self) -> Instrument {
        #[allow(unused_assignments)]
        let mut name = String::new();
        let mut muted = false;
        let mut instr = InstrDefault::default();

        match self {
            ItInstrument::Pre2(source) => {
                name = if !source.instr.instrument_name.is_empty() {
                    source.instr.instrument_name.clone()
                } else {
                    source.instr.dos_filename.clone()
                };

                for (note, sample) in source.instr.note_sample_keyboard_table.iter() {
                    if *note < 120 && *sample != 0 {
                        instr.sample_for_pitch[*note as usize] = Some(*sample as usize - 1);
                    }
                }

                instr.volume_envelope = source.volume_envelope.to_envelope_struct(
                    source.instr.flags,
                    source.instr.loop_start,
                    source.instr.loop_end,
                    source.instr.sustain_loop_start,
                    source.instr.sustain_loop_end,
                );
                // Guard against degenerate envelopes (0 or 1 point, out-of-
                // range loop/sustain indices): fall back to disabled.
                if !instr.volume_envelope.is_valid() {
                    instr.volume_envelope = Envelope::default();
                }

                // IT fadeout: Schism / OpenMPT run a 16-bit counter
                // that starts at 65536 and decrements by the
                // `fadeout` value every tick. Time to fully fade =
                // 65536 / fadeout ticks. The player's
                // `volume_fadeout` register starts at 1.0 and is
                // decremented by `instr.volume_fadeout` per tick, so
                // the matching scale is `fadeout / 65536`. The
                // previous `/32768` formula faded notes twice as
                // fast as reference implementations — audible on any
                // IT module with NNA=Fade or a sustained tail.
                instr.volume_fadeout = source.instr.fadeout as f32 / 65536.0;

                let nna = match source.instr.nna {
                    1 => NewNoteAction::Continue,
                    2 => NewNoteAction::NoteOff,
                    3 => NewNoteAction::NoteFadeOut,
                    _ => NewNoteAction::NoteCut,
                };

                instr.duplicate_check = DuplicateCheckType::Off(nna);
                muted = source.instr.dnc == 0;
            }
            ItInstrument::Post2(source) => {
                name = if !source.instr.instrument_name.is_empty() {
                    source.instr.instrument_name.clone()
                } else {
                    source.instr.dos_filename.clone()
                };

                for (note, sample) in source.instr.note_sample_keyboard_table.iter() {
                    if *note < 120 && *sample != 0 {
                        instr.sample_for_pitch[*note as usize] = Some(*sample as usize - 1);
                    }
                }

                instr.volume_envelope = source.volume_envelope.to_envelope_struct();
                instr.pan_envelope = source.panning_envelope.to_envelope_struct_signed();
                instr.pitch_envelope = source.pitch_envelope.to_envelope_struct_signed();
                instr.pitch_envelope_as_low_pass_filter =
                    source.pitch_envelope.flags & 0b1000_0000 != 0;

                // Guard against degenerate envelopes (0 or 1 point, out-of-
                // range loop/sustain indices).
                if !instr.volume_envelope.is_valid() {
                    instr.volume_envelope = Envelope::default();
                }
                if !instr.pan_envelope.is_valid() {
                    instr.pan_envelope = Envelope::default();
                }
                if !instr.pitch_envelope.is_valid() {
                    instr.pitch_envelope = Envelope::default();
                }

                let nna = match source.instr.nna {
                    1 => NewNoteAction::Continue,
                    2 => NewNoteAction::NoteOff,
                    3 => NewNoteAction::NoteFadeOut,
                    _ => NewNoteAction::NoteCut,
                };

                let dca = match source.instr.duplicate_check_action {
                    1 => DuplicateCheckAction::NoteOff(nna),
                    2 => DuplicateCheckAction::NoteFadeOut(nna),
                    _ => DuplicateCheckAction::NoteCut(nna),
                };

                instr.duplicate_check = match source.instr.duplicate_check_type {
                    1 => DuplicateCheckType::Note(dca),
                    2 => DuplicateCheckType::Sample(dca),
                    3 => DuplicateCheckType::Instrument(dca),
                    _ => DuplicateCheckType::Off(nna),
                };

                // See note on fadeout scale at the earlier site.
                instr.volume_fadeout = source.instr.fadeout as f32 / 65536.0;
                instr.pitch_pan_center = source
                    .instr
                    .pitch_pan_center
                    .try_into()
                    .unwrap_or(Pitch::C4);
                instr.pitch_pan_separation = source.instr.pitch_pan_separation as f32 / 32.0;
                instr.global_volume = source.instr.global_volume as f32 / 128.0;
                instr.default_pan = if source.instr.default_pan & 0b1000_0000 == 0 {
                    source.instr.default_pan as f32 / 64.0
                } else {
                    0.5
                };
                instr.random_volume_variation = source.instr.random_volume_variation as f32 / 100.0;
                instr.random_pan_variation = source.instr.random_pan_variation as f32 / 100.0;

                instr.initial_filter_cutoff = source.instr.initial_filter_cutoff;
                instr.initial_filter_resonance = source.instr.initial_filter_resonance;

                instr.midi = InstrMidi {
                    muted: true,
                    channel: source.instr.midi_channel,
                    program: source.instr.midi_program as u16,
                    bank: source.instr.midi_bank,
                    bend: 0,
                };
            }
        }

        Instrument {
            name,
            instr_type: InstrumentType::Default(instr),
            muted,
        }
    }
}
