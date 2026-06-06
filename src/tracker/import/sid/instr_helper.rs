//! SID → `Instrument` bridge: turns a parsed SID voice + its
//! Rob-Hubbard-style effect table into the `InstrumentType::Sid`
//! or `InstrumentType::RobSid` variant, plus the synthetic
//! sample-based fallback the sample engine will play if no
//! downstream SID emulator is registered.

use crate::core::fixed::fixed::Q8_8;
use crate::core::fixed::units::{ChannelVolume, EnvValue, Finetune, Panning, PitchDelta, Volume};
use crate::{prelude::*, tracker::instr_sid::SidVoice};
use alloc::format;
use alloc::string::String;
use alloc::{vec, vec::Vec};

pub struct InstrHelper;

impl InstrHelper {
    fn sid_to_sample(voice: &SidVoice) -> Sample {
        let mut name = String::from("Unknown Waveform");
        let mut data = SampleDataType::Mono8(vec![0; 128]);
        let loop_flag = LoopType::Forward;

        if voice.ctrl_noise {
            name = String::from("Noise");
            data = Self::generate_sid_noise_sample();
        } else if voice.ctrl_pulse {
            name = format!("Pulse Wave {}", voice.pw);
            data = Self::generate_pulse_sample(voice.pw);
        } else if voice.ctrl_sawtooth {
            name = String::from("Sawtooth Wave");
            data = Self::generate_sawtooth_sample();
        } else if voice.ctrl_triangle {
            name = String::from("Triangle Wave");
            data = Self::generate_triangle_sample();
        }

        Sample {
            name,
            relative_pitch: 0,
            finetune: Finetune::ZERO,
            volume: ChannelVolume::FULL,
            // Synthesized SID waveforms don't carry a meaningful
            // default-note-volume — always triggers at full.
            default_note_volume: Volume::FULL,
            panning: Panning::CENTER,
            loop_flag,
            loop_start: 0,
            loop_length: 128,
            sustain_loop_flag: LoopType::No,
            sustain_loop_start: 0,
            sustain_loop_length: 0,
            data: Some(data),
        }
    }

    fn generate_sid_noise_sample() -> SampleDataType {
        let mut lfsr: u16 = 0xACE1;
        let mut noise_data: Vec<i8> = vec![0; 128];

        for v in noise_data.iter_mut().take(128) {
            let new_bit = lfsr ^ (lfsr >> 1) & 1;
            lfsr = (lfsr >> 1) | (new_bit << 15);
            let value_u8 = (lfsr & 0xFF) as u8;
            *v = value_u8.wrapping_sub(128) as i8;
        }

        SampleDataType::Mono8(noise_data)
    }

    fn generate_pulse_sample(pw: u16) -> SampleDataType {
        let mut pulse_data: Vec<i8> = vec![0; 128];
        // PWM duty cycle: `pw / 4095 × 128`, exact in `u32` for
        // any `pw ∈ [0, 4095]` (max product `524 160` fits u32,
        // max result `128`). Replaces the f32 path
        // `(pw as f32 / 4095.0 * 128.0) as usize`.
        let duty_cycle = ((pw as u32) * 128 / 4095) as usize;
        for (i, v) in pulse_data.iter_mut().enumerate().take(128) {
            *v = if i < duty_cycle { 127 } else { -127 };
        }
        SampleDataType::Mono8(pulse_data)
    }

    fn generate_sawtooth_sample() -> SampleDataType {
        let sawtooth_data: Vec<i8> = (0..128)
            .map(|x| (2 * x as u8).wrapping_sub(128) as i8)
            .collect();
        SampleDataType::Mono8(sawtooth_data)
    }

    fn generate_triangle_sample() -> SampleDataType {
        let mut triangle_data: Vec<i8> = vec![0; 128];

        for i in 0..128 {
            let value = if i < 64 {
                ((4 * i) as u8).wrapping_sub(128) as i8
            } else {
                ((4 * (127 - i)) as u8).wrapping_sub(128) as i8
            };

            triangle_data[i as usize] = value;
        }

        SampleDataType::Mono8(triangle_data)
    }

    // fn gen_adr_frames(bpm: u16, speed: u8) -> [u16; 16] {
    //     let tick_duration_ms = (2500.0 / bpm as f32) * speed as f32;

    //     let adr_durations_ms: [u32; 16] = [
    //         2, 8, 16, 24, 38, 56, 68, 80, 100, 250, 500, 800, 1100, 1500, 2400, 3900,
    //     ];

    //     let mut frames_for_adr: [u16; 16] = [0; 16];
    //     for (i, &duration_ms) in adr_durations_ms.iter().enumerate() {
    //         frames_for_adr[i] = (duration_ms as f32 / tick_duration_ms).round() as u16;
    //     }

    //     frames_for_adr
    // }

    fn sid_to_xi(sid: &SidVoice) -> InstrDefault {
        // let adr_frames = Self::gen_adr_frames(125, 2);
        let adr_frames: [usize; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

        // ADSR
        let attack = (sid.ad >> 4) & 0x0F;
        let decay = sid.ad & 0x0F;
        let sustain = (sid.sr >> 4) & 0x0F;
        let release = sid.sr & 0x0F;

        let mut point: Vec<EnvelopePoint> = vec![];
        let mut seek: usize = 0;
        let mut seek_point = 0;

        if attack != 0 {
            let attack_frame = adr_frames[attack as usize];
            point.push(EnvelopePoint {
                frame: 0,
                value: EnvValue::default(),
            });
            seek += attack_frame;
            point.push(EnvelopePoint {
                frame: attack_frame,
                value: EnvValue::VOLUME_DEFAULT,
            });
            seek_point = 1;
        } else {
            point.push(EnvelopePoint {
                frame: 0,
                value: EnvValue::VOLUME_DEFAULT,
            });
        }

        if decay != 0 {
            seek += adr_frames[decay as usize];
        } else {
            seek += 1;
        }
        point.push(EnvelopePoint {
            frame: seek,
            // SID sustain nibble (`0..=15`) scaled by 4 into the
            // standard `0..=64` envelope-byte range. So a max
            // sustain of 15 lands at `60 / 64 ≈ 0.9375`, matching
            // the previous `4.0 * sustain / 64.0`.
            value: EnvValue::from_byte_64(4 * sustain),
        });
        seek_point += 1;

        if release != 0 {
            seek += adr_frames[release as usize];
        } else {
            seek += 1;
        }
        point.push(EnvelopePoint {
            frame: seek,
            value: EnvValue::default(),
        });

        let volume_envelope = Envelope {
            enabled: true,
            point,
            sustain_enabled: sustain != 0,
            sustain_start_point: seek_point,
            sustain_end_point: seek_point,
            loop_enabled: false,
            loop_start_point: 0,
            loop_end_point: 0,
        };

        let mut instr = InstrDefault::default();
        instr.voice.volume_envelope = volume_envelope;
        instr.keyboard.sample_for_pitch = [Some(0); 120];

        let mut s: Sample = Self::sid_to_sample(sid);
        s.relative_pitch = 24;
        instr.sample = vec![Some(s)];

        instr
    }

    fn irs_to_instrument(irs: &InstrRobSid, original: bool) -> Instrument {
        let mut idst = Instrument::default();
        if irs.sid.voice[0].ctrl_noise {
            idst.name = format!(
                "Noise {:02X}{:02X}",
                irs.sid.voice[0].ad, irs.sid.voice[0].sr
            );
        } else if irs.sid.voice[0].ctrl_pulse {
            idst.name = format!(
                "Pulse {:02X}{:02X}",
                irs.sid.voice[0].ad, irs.sid.voice[0].sr
            );
        } else if irs.sid.voice[0].ctrl_sawtooth {
            idst.name = format!(
                "Sawtooth {:02X}{:02X}",
                irs.sid.voice[0].ad, irs.sid.voice[0].sr
            );
        } else if irs.sid.voice[0].ctrl_triangle {
            idst.name = format!(
                "Triangle {:02X}{:02X}",
                irs.sid.voice[0].ad, irs.sid.voice[0].sr
            );
        }
        if original {
            idst.instr_type = InstrumentType::RobSid(irs.clone());
        } else {
            let mut xi = Self::sid_to_xi(&irs.sid.voice[0]);
            if irs.fx[0].vibrato {
                // Explicit shape: a C64 vibrato is a centred (bipolar)
                // pitch wobble, so use the instrument auto-vibrato sine
                // rather than relying on `Waveform::default()`.
                xi.voice.vibrato.waveform = crate::core::waveform::Waveform::AutoVibSine;
                xi.voice.vibrato.depth = PitchDelta::from_ratio(irs.fx[0].vibrato_depth as i16, 15);
                xi.voice.vibrato.speed = Q8_8::from_ratio(1, irs.fx[0].vibrato_div as i16);
            }
            idst.instr_type = InstrumentType::Default(xi);
        }
        idst
    }

    pub fn irss_to_instruments(irss: &[InstrRobSid], original: bool) -> Vec<Instrument> {
        let mut idst: Vec<Instrument> = vec![];
        for isrc in irss {
            idst.push(Self::irs_to_instrument(isrc, original));
        }
        idst
    }
}
