//! Original XM Instrument.
//!
//! Layout (variable length, driven by `instrument_header_len`):
//!
//! ```text
//! u32  instrument_header_len     // 4 + this header on disk
//! ---- XmInstrumentHeader (25B) ----
//! [u8;22] name
//! u8      instr_type             // "random" per FT2 reference
//! u16     num_samples
//! ---- if num_samples > 0 ----
//! u32  sample_header_size
//! ---- XmInstrDefault (215B) ----
//! [u8;96]  sample_for_pitchs
//! [u8;48]  volume_envelope
//! [u8;48]  panning_envelope
//! u8       number_of_volume_points / panning_points / sustain & loop pts (×7)
//! u8       volume_flag, panning_flag
//! u8 × 4   vibrato type / sweep / depth / rate
//! u16      volume_fadeout
//! u8       midi_on, midi_channel
//! u16      midi_program, midi_bend
//! u8       midi_mute_computer
//! ---- num_samples × XmSampleHeader, then sample bodies ----
//! ```

use alloc::boxed::Box;
use alloc::string::String;
use alloc::{vec, vec::Vec};

use crate::core::envelope::{Envelope, EnvelopePoint};
use crate::core::fixed::fixed::Q8_8;
use crate::core::fixed::units::{EnvValue, PitchDelta, Volume};
use crate::core::instr_default::InstrDefault;
use crate::core::instrument::{Instrument, InstrumentType};
use crate::core::sample::Sample;
use crate::core::waveform::Waveform;
use crate::tracker::import::bin_reader::{bytes_to_trimmed_string, BinReader, ImportError};

use super::xmsample::{XmSample, XMSAMPLE_HEADER_SIZE};

#[derive(Debug)]
pub enum XmInstrumentType {
    Empty,
    Default(Box<XmInstrDefault>),
}

pub const XMINSTRDEFAULT_SIZE: usize = 96 + 4 * 12 + 4 * 12 + 14 + 2 + 2 + 2 + 2 + 1;

#[derive(Clone, Copy, Debug)]
pub struct XmInstrDefault {
    sample_for_pitchs: [u8; 96],

    volume_envelope: [u8; 4 * 12],
    panning_envelope: [u8; 4 * 12],
    number_of_volume_points: u8,
    number_of_panning_points: u8,
    volume_sustain_point: u8,
    volume_loop_start_point: u8,
    volume_loop_end_point: u8,
    panning_sustain_point: u8,
    panning_loop_start_point: u8,
    panning_loop_end_point: u8,
    volume_flag: u8,
    panning_flag: u8,

    vibrato_type: u8,
    vibrato_sweep: u8,
    vibrato_depth: u8,
    vibrato_rate: u8,

    volume_fadeout: u16,

    midi_on: u8,
    midi_channel: u8,
    midi_program: u16,
    midi_bend: u16,
    midi_mute_computer: u8,
}

impl Default for XmInstrDefault {
    fn default() -> Self {
        Self {
            sample_for_pitchs: [0; 96],
            volume_envelope: [0; 4 * 12],
            panning_envelope: [0; 4 * 12],
            number_of_volume_points: 0,
            number_of_panning_points: 0,
            volume_sustain_point: 0,
            volume_loop_start_point: 0,
            volume_loop_end_point: 0,
            panning_sustain_point: 0,
            panning_loop_start_point: 0,
            panning_loop_end_point: 0,
            volume_flag: 0,
            panning_flag: 0,

            vibrato_type: 0,
            vibrato_sweep: 0,
            vibrato_depth: 0,
            vibrato_rate: 0,

            volume_fadeout: 0,

            midi_on: 1,
            midi_channel: 0,
            midi_program: 0,
            midi_bend: 0,
            midi_mute_computer: 0,
        }
    }
}

impl XmInstrDefault {
    pub(super) fn read(r: &mut BinReader) -> Result<Self, ImportError> {
        let sample_for_pitchs: [u8; 96] = r.read_array()?;
        let volume_envelope: [u8; 48] = r.read_array()?;
        let panning_envelope: [u8; 48] = r.read_array()?;
        let number_of_volume_points = r.read_u8()?;
        let number_of_panning_points = r.read_u8()?;
        let volume_sustain_point = r.read_u8()?;
        let volume_loop_start_point = r.read_u8()?;
        let volume_loop_end_point = r.read_u8()?;
        let panning_sustain_point = r.read_u8()?;
        let panning_loop_start_point = r.read_u8()?;
        let panning_loop_end_point = r.read_u8()?;
        let volume_flag = r.read_u8()?;
        let panning_flag = r.read_u8()?;
        let vibrato_type = r.read_u8()?;
        let vibrato_sweep = r.read_u8()?;
        let vibrato_depth = r.read_u8()?;
        let vibrato_rate = r.read_u8()?;
        let volume_fadeout = r.read_u16_le()?;
        let midi_on = r.read_u8()?;
        let midi_channel = r.read_u8()?;
        let midi_program = r.read_u16_le()?;
        let midi_bend = r.read_u16_le()?;
        let midi_mute_computer = r.read_u8()?;
        Ok(Self {
            sample_for_pitchs,
            volume_envelope,
            panning_envelope,
            number_of_volume_points,
            number_of_panning_points,
            volume_sustain_point,
            volume_loop_start_point,
            volume_loop_end_point,
            panning_sustain_point,
            panning_loop_start_point,
            panning_loop_end_point,
            volume_flag,
            panning_flag,
            vibrato_type,
            vibrato_sweep,
            vibrato_depth,
            vibrato_rate,
            volume_fadeout,
            midi_on,
            midi_channel,
            midi_program,
            midi_bend,
            midi_mute_computer,
        })
    }
}

pub const XMINSTRUMENT_HEADER_SIZE: usize = 25;

#[derive(Debug, Default)]
pub struct XmInstrumentHeader {
    pub name: String,
    pub instr_type: u8, // FT2 writes a "random" byte here
    pub num_samples: u16,
}

impl XmInstrumentHeader {
    pub(super) fn read(r: &mut BinReader) -> Result<Self, ImportError> {
        let name_bytes: [u8; 22] = r.read_array()?;
        let instr_type = r.read_u8()?;
        let num_samples = r.read_u16_le()?;
        Ok(Self {
            name: bytes_to_trimmed_string(&name_bytes),
            instr_type,
            num_samples,
        })
    }
}

#[derive(Debug)]
pub struct XmInstrument {
    pub instrument_header_len: u32,
    pub header: XmInstrumentHeader,
    pub sample_header_size: u32,
    pub instr: XmInstrumentType,
    pub sample: Vec<XmSample>,
}

impl Default for XmInstrument {
    fn default() -> Self {
        Self {
            instrument_header_len: 4 + XMINSTRUMENT_HEADER_SIZE as u32,
            header: XmInstrumentHeader::default(),
            sample_header_size: XMSAMPLE_HEADER_SIZE as u32,
            instr: XmInstrumentType::Empty,
            sample: vec![],
        }
    }
}

impl XmInstrument {
    pub fn load(data: &[u8]) -> Result<(&[u8], XmInstrument), ImportError> {
        let mut sample: Vec<XmSample> = vec![];

        let mut r = BinReader::new(data);
        // instrument_header_len. Some files truncate before this
        // field — yield an empty record and don't advance.
        let Ok(xmih_len_raw) = r.read_u32_le() else {
            return Ok((data, XmInstrument::default()));
        };
        let xmih_len = xmih_len_raw as usize;

        if xmih_len == 4 {
            // no data: just the length tag
            return Ok((&data[4..], XmInstrument::default()));
        }

        // XmInstrumentHeader (immediately follows the length tag).
        let xmih = XmInstrumentHeader::read(&mut r)?;

        if xmih.num_samples == 0 {
            if xmih_len > data.len() {
                return Err(ImportError::OutOfRange(
                    "XmInstrument.instrument_header_len overshoots input",
                ));
            }
            let data = &data[xmih_len..];
            let xmi = XmInstrument {
                instrument_header_len: 4 + XMINSTRUMENT_HEADER_SIZE as u32,
                header: xmih,
                sample_header_size: XMSAMPLE_HEADER_SIZE as u32,
                instr: XmInstrumentType::Empty,
                sample: vec![],
            };
            return Ok((data, xmi));
        }

        // sample_header_size (discarded — we use XMSAMPLE_HEADER_SIZE)
        // and then XmInstrDefault.
        let _sample_header_size: u32 = r.read_u32_le()?;
        let xmid = Box::new(XmInstrDefault::read(&mut r)?);

        // After the (variable-length) instrument header, sample
        // headers and bodies are positioned at the offset declared
        // by `instrument_header_len`. The previous implementation
        // honoured that even when we had read fewer bytes than the
        // declared length.
        if xmih_len > data.len() {
            return Err(ImportError::OutOfRange(
                "XmInstrument.instrument_header_len overshoots input",
            ));
        }
        let mut d3 = &data[xmih_len..];
        for _ in 0..xmih.num_samples {
            let (d, s) = XmSample::load(d3)?;
            sample.push(s);
            d3 = d;
        }
        for s in &mut sample {
            let d = s.add_sample(d3)?;
            d3 = d;
        }

        let xmi = XmInstrument {
            instrument_header_len: 4 + XMINSTRUMENT_HEADER_SIZE as u32,
            header: xmih,
            sample_header_size: XMSAMPLE_HEADER_SIZE as u32,
            instr: XmInstrumentType::Default(xmid),
            sample,
        };
        Ok((d3, xmi))
    }

    fn envelope_from_slice(src: &[u8]) -> Option<Envelope> {
        let mut e = Envelope::default();
        let mut iter = src
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]));
        for _i in 0..src.len() / 4 {
            let ep = EnvelopePoint {
                frame: iter.next()? as usize,
                value: EnvValue::from_byte_64(iter.next()? as u8),
            };
            e.point.push(ep);
        }
        Some(e)
    }

    // Sanity check is delegated to `Envelope::is_valid` on the loaded
    // envelope — see `to_instrument`. Any envelope that fails it is
    // replaced by `Envelope::default()` (which silently disables it).

    pub fn to_instrument(&self) -> Instrument {
        let it: InstrumentType = match &self.instr {
            XmInstrumentType::Empty => InstrumentType::Empty,
            XmInstrumentType::Default(xmi) => {
                let mut sample: Vec<Option<Sample>> = vec![];
                for xms in &self.sample {
                    let s = xms.to_sample();
                    sample.push(Some(s));
                }

                let num_vol_pt = if xmi.number_of_volume_points as usize <= 12 {
                    4 * xmi.number_of_volume_points as usize
                } else {
                    0
                };
                let num_pan_pt = if xmi.number_of_panning_points as usize <= 12 {
                    4 * xmi.number_of_panning_points as usize
                } else {
                    0
                };

                let mut sample_for_pitch: [Option<usize>; 120] = [None; 120];
                for (i, &val) in xmi.sample_for_pitchs.iter().enumerate() {
                    sample_for_pitch[i] = Some(val as usize);
                }
                let mut id = InstrDefault::default();
                id.voice.volume_envelope =
                    Self::envelope_from_slice(&xmi.volume_envelope[0..num_vol_pt])
                        .unwrap_or_default();
                id.keyboard.sample_for_pitch = sample_for_pitch;
                id.voice.pan_envelope =
                    Self::envelope_from_slice(&xmi.panning_envelope[0..num_pan_pt])
                        .unwrap_or_default();
                id.voice.volume_fadeout =
                    Volume::from_ratio(xmi.volume_fadeout as i32, 4095 * 4 * 2);
                id.sample = sample;

                // copy volume envelope data
                {
                    let ve = &mut id.voice.volume_envelope;
                    ve.enabled = xmi.volume_flag & 0b0001 != 0;
                    ve.sustain_enabled = xmi.volume_flag & 0b0010 != 0;
                    ve.sustain_start_point = xmi.volume_sustain_point as usize;
                    ve.sustain_end_point = xmi.volume_sustain_point as usize;
                    ve.loop_enabled = xmi.volume_flag & 0b0100 != 0;
                    ve.loop_start_point = xmi.volume_loop_start_point as usize;
                    ve.loop_end_point = xmi.volume_loop_end_point as usize;

                    // Schism's `fmt/xm.c:703-712` trick — applied here
                    // for parity. FT2 starts the volume fadeout in
                    // parallel with the envelope's release section
                    // (key-off opens both the gate and the fade
                    // simultaneously). The replayer's rule for "fade
                    // immediately on key-off" is
                    //   `env.loop_enabled && volume_fadeout > 0`,
                    // which mirrors schism's `if penv->fadeout &&
                    // (penv->flags & ENV_VOLLOOP)` in
                    // `effects.c:166`. To opt XM into that branch,
                    // schism collapses any missing-or-degenerate loop
                    // to a one-point loop at the last node, then
                    // forces ENV_VOLLOOP on. We do the same: the
                    // synthetic loop holds the envelope at its tail
                    // (no audible difference vs. an envelope without
                    // a loop) and lets the player engage fadeout at
                    // key-off, the way FT2 does.
                    if ve.enabled && !ve.point.is_empty() {
                        let last = ve.point.len() - 1;
                        if !ve.loop_enabled || ve.loop_start_point == ve.loop_end_point {
                            ve.loop_start_point = last;
                            ve.loop_end_point = last;
                        }
                        ve.loop_enabled = true;
                        // Sustain may have come in disabled: schism
                        // pins it to the last node too so the
                        // sustain-vs-release branch in
                        // `_process_envelope` always has valid
                        // indices to read.
                        if !ve.sustain_enabled {
                            ve.sustain_start_point = last;
                            ve.sustain_end_point = last;
                        }
                        ve.sustain_enabled = true;
                    }
                }

                // copy panning envelope data
                {
                    let pe = &mut id.voice.pan_envelope;
                    pe.enabled = xmi.panning_flag & 0b0001 != 0;
                    pe.sustain_enabled = xmi.panning_flag & 0b0010 != 0;
                    pe.sustain_start_point = xmi.panning_sustain_point as usize;
                    pe.sustain_end_point = xmi.panning_sustain_point as usize;
                    pe.loop_enabled = xmi.panning_flag & 0b0100 != 0;
                    pe.loop_start_point = xmi.panning_loop_start_point as usize;
                    pe.loop_end_point = xmi.panning_loop_end_point as usize;
                }

                // cleanup bad envelope — a single-point or zero-point
                // envelope flagged "enabled" is the documented root cause
                // of the 0.10 "all sound panned to one side" regression.
                // `Envelope::is_valid()` rejects any degenerate case
                // (< 2 points, > 12 points, or out-of-range sustain/loop
                // indices) and we fall back to `Envelope::default()`
                // (enabled = false) in that case.
                if !id.voice.volume_envelope.is_valid() {
                    id.voice.volume_envelope = Envelope::default();
                }
                if !id.voice.pan_envelope.is_valid() {
                    id.voice.pan_envelope = Envelope::default();
                }

                // cleanup bad sample for notes
                let sample_qty = id.sample.len();
                for i in 0..id.keyboard.sample_for_pitch.len() {
                    if let Some(value) = id.keyboard.sample_for_pitch[i] {
                        if value >= sample_qty {
                            id.keyboard.sample_for_pitch[i] = None;
                        }
                    }
                }

                // vibrato
                {
                    let v = &mut id.voice.vibrato;
                    // FT2 (`ft2_replayer.c`) auto-vibrato shapes, in the
                    // pitch domain: type 1 square (rises first), type 2
                    // "ramp up" → pitch ramps down (mid-cycle
                    // discontinuity, `BipolarRampDown`), type 3 "ramp
                    // down" → pitch ramps up (`BipolarRampUp`), type 0
                    // sine (`+sin`, rises first).
                    v.waveform = match xmi.vibrato_type & 3 {
                        1 => Waveform::AutoVibSquare,
                        2 => Waveform::BipolarRampDown,
                        3 => Waveform::BipolarRampUp,
                        _ => Waveform::AutoVibSine,
                    };
                    v.speed = Q8_8::from_ratio(xmi.vibrato_rate as i16, 63 * 4);
                    // Peak pitch deviation = `depth_byte / 64` semitones:
                    // FT2's peak `autoVibVal` is `depth_byte` period units
                    // and one semitone is 64 period units in linear mode.
                    // (Was `/30`, calibrated for the old half-range
                    // `[0,1]` rendering; the bipolar shape makes `depth`
                    // the true peak.)
                    v.depth = PitchDelta::from_ratio(xmi.vibrato_depth as i16, 64);
                    v.sweep = Q8_8::from_ratio(xmi.vibrato_sweep as i16, 255);
                }

                id.midi.muted = xmi.midi_on == 0;
                id.midi.channel = xmi.midi_channel;
                id.midi.program = xmi.midi_program;
                id.midi.bend = xmi.midi_bend;
                id.midi_mute_computer = xmi.midi_mute_computer == 1;

                InstrumentType::Default(id)
            }
        };
        Instrument {
            name: self.header.name.clone(),
            instr_type: it,
            muted: false,
        }
    }
}
