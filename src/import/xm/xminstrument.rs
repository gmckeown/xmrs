/// Original XM Instrument
use bincode::error::{DecodeError, EncodeError};
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::{vec, vec::Vec};

use crate::envelope::{Envelope, EnvelopePoint};
use crate::fixed::fixed::Q8_8;
use crate::fixed::units::{EnvValue, PitchDelta, Volume};
use crate::instr_default::InstrDefault;
use crate::instrument::{Instrument, InstrumentType};
use crate::module::Module;
use crate::sample::Sample;
use crate::waveform::Waveform;

use super::serde_helper::{deserialize_string_22, serialize_string_22};
use super::xmsample::{XmSample, XMSAMPLE_HEADER_SIZE};

#[derive(Serialize, Deserialize, Debug)]
pub enum XmInstrumentType {
    Empty,
    Default(Box<XmInstrDefault>),
}

impl XmInstrumentType {
    pub fn save(&self) -> Result<Vec<u8>, EncodeError> {
        match self {
            XmInstrumentType::Default(xmid) => {
                bincode::serde::encode_to_vec(xmid, bincode::config::legacy())
            }
            _ => Ok(vec![]),
        }
    }
}

pub const XMINSTRDEFAULT_SIZE: usize = 96 + 4 * 12 + 4 * 12 + 14 + 2 + 2 + 2 + 2 + 1;

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct XmInstrDefault {
    #[serde(with = "BigArray")]
    sample_for_pitchs: [u8; 96],

    #[serde(with = "BigArray")]
    volume_envelope: [u8; 4 * 12],
    #[serde(with = "BigArray")]
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
    fn from_envelope(e: &Envelope) -> [u8; 48] {
        let mut dst: [u8; 48] = [0; 48];
        let mut i = 0;
        for ep in &e.point {
            let f = ep.frame.to_le_bytes();
            // Inverse of `EnvValue::from_byte_64`: get back the
            // tracker byte in `0..=64`. Round-trip is exact for
            // every byte that came in through the importer; see
            // [`EnvValue::to_byte_64`] for the rounding rule.
            let v = (ep.value.to_byte_64() as u16).to_le_bytes();
            dst[i] = f[0];
            dst[i + 1] = f[1];
            dst[i + 2] = v[0];
            dst[i + 3] = v[1];
            i += 4;
        }
        dst
    }

    pub fn from_instr(i: &Instrument) -> XmInstrumentType {
        let mut xmid: Box<Self> = Box::default();
        match &i.instr_type {
            InstrumentType::Default(id) => {
                for i in 0..96 {
                    xmid.sample_for_pitchs[i] = match id.keyboard.sample_for_pitch[i] {
                        Some(sample) => sample as u8,
                        None => 0,
                    };
                }
                xmid.volume_envelope = Self::from_envelope(&id.voice.volume_envelope);
                xmid.number_of_volume_points = id.voice.volume_envelope.point.len() as u8;
                xmid.volume_sustain_point = id.voice.volume_envelope.sustain_start_point as u8;
                xmid.volume_loop_start_point = id.voice.volume_envelope.loop_start_point as u8;
                xmid.volume_loop_end_point = id.voice.volume_envelope.loop_end_point as u8;
                if id.voice.volume_envelope.enabled {
                    xmid.volume_flag |= 0b0001;
                }
                if id.voice.volume_envelope.sustain_enabled {
                    xmid.volume_flag |= 0b0010;
                }
                if id.voice.volume_envelope.loop_enabled {
                    xmid.volume_flag |= 0b0100;
                }

                xmid.panning_envelope = Self::from_envelope(&id.voice.pan_envelope);
                xmid.number_of_panning_points = id.voice.pan_envelope.point.len() as u8;
                xmid.panning_sustain_point = id.voice.pan_envelope.sustain_start_point as u8;
                xmid.panning_loop_start_point = id.voice.pan_envelope.loop_start_point as u8;
                xmid.panning_loop_end_point = id.voice.pan_envelope.loop_end_point as u8;
                if id.voice.pan_envelope.enabled {
                    xmid.panning_flag |= 0b0001;
                }
                if id.voice.pan_envelope.sustain_enabled {
                    xmid.panning_flag |= 0b0010;
                }
                if id.voice.pan_envelope.loop_enabled {
                    xmid.panning_flag |= 0b0100;
                }

                xmid.vibrato_type = match id.voice.vibrato.waveform {
                    Waveform::TranslatedSquare => 1,
                    Waveform::TranslatedRampUp => 2,
                    Waveform::TranslatedRampDown => 3,
                    _ => 0,
                };
                // XM stores each vibrato parameter as a u8 byte
                // representing `value × scale`. Scales come from
                // the FT2 reference: sweep × 255, depth × 30,
                // rate × 252. `to_ratio_byte` does the multiply
                // with proper round-to-nearest and clamps to u8.
                xmid.vibrato_sweep = id.voice.vibrato.sweep.to_ratio_byte(255);
                xmid.vibrato_depth = id.voice.vibrato.depth.to_ratio_byte(15 * 2);
                xmid.vibrato_rate = id.voice.vibrato.speed.to_ratio_byte(63 * 4);

                // XM stores volume_fadeout as `value × 32760`
                // in u16. The previous `+ 0.5` rounding bias is
                // baked into `to_ratio_u16`'s round-to-nearest.
                xmid.volume_fadeout = id.voice.volume_fadeout.to_ratio_u16(4095 * 4 * 2);

                xmid.midi_on = if id.midi.muted { 0 } else { 1 };
                xmid.midi_channel = id.midi.channel;
                xmid.midi_program = id.midi.program;
                xmid.midi_bend = id.midi.bend;
                xmid.midi_mute_computer = if id.midi_mute_computer { 1 } else { 0 };

                XmInstrumentType::Default(xmid)
            }
            _ => XmInstrumentType::Empty,
        }
    }
}

pub const XMINSTRUMENT_HEADER_SIZE: usize = 25;

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct XmInstrumentHeader {
    #[serde(
        deserialize_with = "deserialize_string_22",
        serialize_with = "serialize_string_22"
    )]
    pub name: String,
    pub instr_type: u8, // must be 0, be random...
    pub num_samples: u16,
}

impl XmInstrumentHeader {
    pub fn save(&self) -> Result<Vec<u8>, EncodeError> {
        bincode::serde::encode_to_vec(self, bincode::config::legacy())
    }

    pub fn from_instr(i: &Instrument) -> Self {
        XmInstrumentHeader {
            name: i.name.clone(),
            num_samples: match &i.instr_type {
                InstrumentType::Default(it) => it.sample.len() as u16,
                _ => 0,
            },
            ..Default::default()
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
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
    pub fn load(data: &[u8]) -> Result<(&[u8], XmInstrument), DecodeError> {
        let mut sample: Vec<XmSample> = vec![];

        // length
        let xmih_len = if let Ok(len) =
            bincode::serde::decode_from_slice::<u32, _>(data, bincode::config::legacy())
        {
            len.0 as usize
        } else {
            return Ok((data, XmInstrument::default()));
        };

        if xmih_len == 4 {
            // no data
            return Ok((&data[4..], XmInstrument::default()));
        }

        // xmih
        let xmih = bincode::serde::decode_from_slice::<XmInstrumentHeader, _>(
            &data[4..],
            bincode::config::legacy(),
        )?
        .0;

        if xmih.num_samples == 0 {
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

        // samples header
        let d2 = &data[4 + XMINSTRUMENT_HEADER_SIZE..];
        let _sample_header_size: u32 =
            bincode::serde::decode_from_slice::<u32, _>(d2, bincode::config::legacy())?.0;
        let d2 = &d2[4..];
        let xmid = Box::new(
            bincode::serde::decode_from_slice::<XmInstrDefault, _>(d2, bincode::config::legacy())?
                .0,
        );

        // all samples headers, then data...

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
        let data = d3;
        Ok((data, xmi))
    }

    pub fn save(&mut self) -> Result<Vec<u8>, EncodeError> {
        let mut i = self.instr.save()?;
        let mut vs: Vec<u8> = vec![];

        // all headers
        for s in &mut self.sample {
            let mut b = s.save()?;
            vs.append(&mut b);
        }

        // then samples
        for s in &mut self.sample {
            let mut b = s.save_sample()?;
            vs.append(&mut b);
        }

        self.instrument_header_len = 4 + XMINSTRUMENT_HEADER_SIZE as u32 + 4 + i.len() as u32;
        let mut instrument_header_len_v = bincode::serde::encode_to_vec::<u32, _>(
            self.instrument_header_len,
            bincode::config::legacy(),
        )?;

        self.header.num_samples = self.sample.len() as u16;
        let mut h = self.header.save()?;

        let sample_header_size = XMSAMPLE_HEADER_SIZE as u32;

        let mut sample_header_size_v =
            bincode::serde::encode_to_vec::<u32, _>(sample_header_size, bincode::config::legacy())?;

        let mut all: Vec<u8> = vec![];
        all.append(&mut instrument_header_len_v);
        all.append(&mut h);
        all.append(&mut sample_header_size_v);
        all.append(&mut i);
        all.append(&mut vs);
        Ok(all)
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
                id.voice.volume_fadeout = Volume::from_ratio(xmi.volume_fadeout as i32, 4095 * 4 * 2);
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
                    v.waveform = match xmi.vibrato_type & 3 {
                        1 => Waveform::TranslatedSquare,
                        2 => Waveform::TranslatedRampUp,
                        3 => Waveform::TranslatedRampDown,
                        _ => Waveform::TranslatedSine,
                    };
                    v.speed = Q8_8::from_ratio(xmi.vibrato_rate as i16, 63 * 4);
                    v.depth = PitchDelta::from_ratio(xmi.vibrato_depth as i16, 15 * 2);
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

    // All instr
    pub fn from_module(module: &Module) -> Vec<Self> {
        let mut all: Vec<XmInstrument> = vec![];
        for i in &module.instrument {
            all.push(XmInstrument {
                instrument_header_len: 4 + XMINSTRUMENT_HEADER_SIZE as u32,
                header: XmInstrumentHeader::from_instr(i),
                sample_header_size: XMSAMPLE_HEADER_SIZE as u32,
                instr: XmInstrDefault::from_instr(i),
                sample: XmSample::from_instr(i),
            });
        }
        all
    }
}
