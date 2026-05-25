//! [`Sample`] — a PCM payload owned by an [`InstrDefault`].
//!
//! Carries the raw audio data (8-bit or 16-bit signed mono, stored
//! as i16 in our model), the natural-pitch reference (C-5 by
//! convention), per-sample finetune and volume, and an optional
//! loop or sustain-loop window. The runtime resamples between any
//! source rate and the mixer rate via the fixed-point pitch chain
//! in [`crate::core::fixed`].
//!
//! [`InstrDefault`]: crate::core::instr_default::InstrDefault

use serde::{Deserialize, Serialize};

use alloc::string::String;
use alloc::vec::Vec;

use crate::core::fixed::units::{Amp, ChannelVolume, Finetune, Panning, Volume};

/// How to play sample
#[derive(Default, Serialize, Deserialize, Copy, Clone, Debug)]
#[repr(u8)]
pub enum LoopType {
    #[default]
    No = 0,
    Forward = 1,
    PingPong = 2,
}

impl From<LoopType> for u8 {
    #[inline]
    fn from(v: LoopType) -> u8 {
        v as u8
    }
}

impl TryFrom<u8> for LoopType {
    type Error = u8;
    #[inline]
    fn try_from(b: u8) -> Result<Self, u8> {
        match b {
            0 => Ok(LoopType::No),
            1 => Ok(LoopType::Forward),
            2 => Ok(LoopType::PingPong),
            other => Err(other),
        }
    }
}

// Hand-rolled `PartialEq` / `Eq` so the comparison inlines into the
// hot path. The derive emits a non-inline `<LoopType as PartialEq>::eq`
// (visible at ~1% of total runtime when profiling real-world IT
// modules) because rustc treats it as a generic-trait dispatch even
// with LTO on. With `#[inline]` on a manual impl that uses the
// `#[repr(u8)]` representation, the comparison reduces to a single
// byte compare at every call site (`StateSample::tick`'s loop checks,
// `is_looping`, the seek branch dispatch).
impl PartialEq for LoopType {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        (*self as u8) == (*other as u8)
    }
}

impl Eq for LoopType {}

/// is sample recorded with 8, 16 bits depth, or native f32
///
/// `StereoFloat` is the input format for callers that produce
/// audio data as `f32` directly (typical: WAV imports, audio
/// engines bridged in from a desktop host). It is converted
/// to Q1.15 [`Amp`] at read time via the saturating cast in
/// [`Sample::at`]. The audio runtime itself never carries
/// `f32` after this conversion — the variant is purely an
/// input-format option.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum SampleDataType {
    Mono8(Vec<i8>),
    Mono16(Vec<i16>),
    Stereo8(Vec<i8>),
    Stereo16(Vec<i16>),
    StereoFloat(Vec<f32>),
}

impl SampleDataType {
    #[inline(always)]
    pub fn len(&self) -> usize {
        match &self {
            SampleDataType::Mono8(v) => v.len(),
            SampleDataType::Mono16(v) => v.len(),
            SampleDataType::Stereo8(v) => v.len() / 2,
            SampleDataType::Stereo16(v) => v.len() / 2,
            SampleDataType::StereoFloat(v) => v.len() / 2,
        }
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A Real Data sample
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Sample {
    /// Name
    pub name: String,
    /// [-96..95] with 0 <=> C-4
    pub relative_pitch: i8,
    /// Sub-semitone detune, Q1.15 in `[-1, +1]`. Was `f32` —
    /// see [`Finetune`] for the conversion convention.
    pub finetune: Finetune,
    /// ALWAYS-applied sample scale (IT's GvL, `0..64`
    /// normalised), Q1.15 in `[0, 1]`. Independent of the
    /// note-column volume.
    ///
    /// Was `f32`. Field name kept for API stability even
    /// though semantically this is the channel-applied
    /// volume — see the GvL documentation in the IT spec.
    pub volume: ChannelVolume,
    /// Starting note volume used when a pattern cell triggers
    /// this sample without a volume-column override (IT's
    /// "Vol", `0..64` normalised), Q1.15 in `[0, 1]`. MOD /
    /// XM / S3M have no equivalent and leave it at
    /// [`Volume::FULL`].
    ///
    /// Layered on top of `volume` at note-trigger time:
    ///   `voice_start = sample.volume × sample.default_note_volume`
    /// while a V-column override replaces `default_note_volume`
    /// at trigger time, letting `volume` (GvL) keep scaling the
    /// sample regardless.
    pub default_note_volume: Volume,
    /// Pan position, Q1.15 in `[0, 1]` (`0` left, `0.5` centre,
    /// `1.0` right).
    pub panning: Panning,

    /// loop type
    pub loop_flag: LoopType,
    /// 0 <= loop_start < len()
    pub loop_start: u32,
    /// 1 <= loop_length <= len() - loop_start
    pub loop_length: u32,

    /// sustain loop type
    pub sustain_loop_flag: LoopType,
    /// 0 <= sustain_loop_start < len()
    pub sustain_loop_start: u32,
    /// 1 <= sustain_loop_length <= len() - sustain_loop_start
    pub sustain_loop_length: u32,

    /// wave data
    pub data: Option<SampleDataType>,
}

impl Sample {
    /// return sample length
    #[inline(always)]
    pub fn len(&self) -> usize {
        if let Some(d) = &self.data {
            d.len()
        } else {
            0
        }
    }

    /// `true` when the sample has no data or zero-length data.
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// return sample at seek as a Q1.15 stereo pair.
    ///
    /// Each variant maps directly to Q1.15 with no rounding:
    /// * `Mono8` / `Stereo8`: i8 is "Q1.7"; left-shift by 8
    ///   widens to Q1.15 — the low byte is zero, the upper
    ///   byte is the sign-extended sample.
    /// * `Mono16` / `Stereo16`: i16 IS Q1.15.
    /// * `StereoFloat`: input-side `f32` (e.g. WAV imports);
    ///   the saturating `× 32768.0 as i16` cast is the input
    ///   boundary — once converted to [`Amp`], the audio path
    ///   is integer-only.
    #[inline(always)]
    pub fn at(&self, seek: usize) -> (Amp, Amp) {
        match &self.data {
            Some(SampleDataType::Mono8(v)) => {
                let raw = (v[seek] as i16) << 8;
                let a = Amp::from_q15_i16(raw);
                (a, a)
            }
            Some(SampleDataType::Mono16(v)) => {
                let a = Amp::from_q15_i16(v[seek]);
                (a, a)
            }
            Some(SampleDataType::Stereo8(v)) => (
                Amp::from_q15_i16((v[seek * 2] as i16) << 8),
                Amp::from_q15_i16((v[seek * 2 + 1] as i16) << 8),
            ),
            Some(SampleDataType::Stereo16(v)) => (
                Amp::from_q15_i16(v[seek * 2]),
                Amp::from_q15_i16(v[seek * 2 + 1]),
            ),
            Some(SampleDataType::StereoFloat(v)) => {
                // Input-side `f32 → Q1.15` saturating cast. Per
                // the Rust reference (since 1.45), `f32 as i16`
                // is well-defined: NaN → 0, ±∞ → ±i16::MAX/MIN,
                // and out-of-range values saturate to the
                // bounds — which is exactly the semantic we
                // want at this audio input boundary.
                let to_amp = |x: f32| -> Amp { Amp::from_q15_i16((x * 32768.0) as i16) };
                (to_amp(v[seek * 2]), to_amp(v[seek * 2 + 1]))
            }
            None => (Amp::SILENCE, Amp::SILENCE),
        }
    }

    pub fn clamp(&mut self) {
        // `volume`, `panning`, and `finetune` were `f32` with
        // explicit `clamp(0.0, 1.0)` / `clamp(-1.0, +1.0)`
        // calls. Their fixed-point types are bounded by
        // construction (`Q15` is in `[-1, +1)` always; the
        // domain newtypes' `from_*` constructors saturate),
        // so the runtime clamp is no-op and has been removed.
        self.relative_pitch = self.relative_pitch.clamp(-95, 96);

        if self.sustain_loop_start as usize > self.len() {
            self.sustain_loop_start = 0;
        }
        if self.sustain_loop_start as usize + self.sustain_loop_length as usize > self.len() {
            self.sustain_loop_length = self.len() as u32 - self.sustain_loop_start;
        }

        if self.loop_start as usize > self.len() {
            self.loop_start = 0;
        }
        if self.loop_start as usize + self.loop_length as usize > self.len() {
            self.loop_length = self.len() as u32 - self.loop_start;
        }
    }

    #[inline(always)]
    fn calculate_loop(
        &self,
        pos: usize,
        start: usize,
        length: usize,
        loop_type: LoopType,
    ) -> usize {
        if self.is_empty() {
            return 0;
        }
        let end = start + length;
        match loop_type {
            LoopType::No => {
                if pos < self.len() {
                    pos
                } else {
                    self.len() - 1
                }
            }
            LoopType::Forward => {
                if length == 0 || pos < end {
                    pos.min(self.len() - 1)
                } else {
                    start + (pos - start) % length
                }
            }
            LoopType::PingPong => {
                if length == 0 || pos < end {
                    pos.min(self.len() - 1)
                } else {
                    let total_length = 2 * length;
                    let mod_pos = (pos - start) % total_length;
                    if mod_pos < length {
                        start + mod_pos
                    } else {
                        end - (mod_pos - length) - 1
                    }
                }
            }
        }
    }

    /// Returns the real position in the sample buffer for a virtual
    /// play-head position, or `None` when the play head has run past
    /// the end of a non-looping sample (or the sample is empty).
    ///
    /// This is the honest version of [`Sample::meta_seek`]: callers
    /// driving a voice should use `seek` and treat `None` as "the
    /// voice is done, disable it", rather than reading the last frame
    /// forever.
    #[inline(always)]
    pub fn seek(&self, pos: usize, sustain: bool) -> Option<usize> {
        if self.is_empty() {
            return None;
        }

        let (start, length, loop_type) = if sustain && self.sustain_loop_flag != LoopType::No {
            (
                self.sustain_loop_start as usize,
                self.sustain_loop_length as usize,
                self.sustain_loop_flag,
            )
        } else {
            (
                self.loop_start as usize,
                self.loop_length as usize,
                self.loop_flag,
            )
        };

        match loop_type {
            LoopType::No => {
                if pos < self.len() {
                    Some(pos)
                } else {
                    None
                }
            }
            // For looped samples `calculate_loop` already returns a
            // valid index for any `pos`, so just reuse it.
            LoopType::Forward | LoopType::PingPong => {
                Some(self.calculate_loop(pos, start, length, loop_type))
            }
        }
    }

    /// Returns the real position in the sample.
    /// The calling function must save the real position at sustain end to avoid problems
    ///
    /// **Note**: when the play head is past the end of a non-looping
    /// sample, this clamps to the last frame. That makes the voice hold
    /// its tail value forever, which is almost never what a player
    /// wants — prefer [`Sample::seek`] and treat `None` as "stop the
    /// voice". This wrapper is kept for backward compatibility.
    pub fn meta_seek(&self, pos: usize, sustain: bool) -> usize {
        if sustain && self.sustain_loop_flag != LoopType::No {
            self.calculate_loop(
                pos,
                self.sustain_loop_start as usize,
                self.sustain_loop_length as usize,
                self.sustain_loop_flag,
            )
        } else {
            self.calculate_loop(
                pos,
                self.loop_start as usize,
                self.loop_length as usize,
                self.loop_flag,
            )
        }
    }

    /// return sample size (8, 16 bits, or 32 for f32)
    pub fn bits(&self) -> u8 {
        match &self.data {
            Some(SampleDataType::Mono8(_)) => 8,
            Some(SampleDataType::Mono16(_)) => 16,
            Some(SampleDataType::Stereo8(_)) => 8,
            Some(SampleDataType::Stereo16(_)) => 16,
            Some(SampleDataType::StereoFloat(_)) => 32,
            None => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_sample(len: usize, loop_flag: LoopType, loop_start: u32, loop_length: u32) -> Sample {
        Sample {
            name: alloc::string::String::new(),
            relative_pitch: 0,
            finetune: Finetune::ZERO,
            volume: ChannelVolume::FULL,
            default_note_volume: Volume::FULL,
            panning: Panning::CENTER,
            loop_flag,
            loop_start,
            loop_length,
            sustain_loop_flag: LoopType::No,
            sustain_loop_start: 0,
            sustain_loop_length: 0,
            data: Some(SampleDataType::Mono8(alloc::vec![0i8; len])),
        }
    }

    #[test]
    fn empty_sample_no_panic() {
        let s = make_sample(0, LoopType::No, 0, 0);
        assert_eq!(s.meta_seek(0, false), 0);
        assert_eq!(s.meta_seek(100, false), 0);
    }

    #[test]
    fn empty_sample_forward_loop_no_panic() {
        let s = make_sample(0, LoopType::Forward, 0, 0);
        assert_eq!(s.meta_seek(0, false), 0);
    }

    #[test]
    fn empty_sample_pingpong_no_panic() {
        let s = make_sample(0, LoopType::PingPong, 0, 0);
        assert_eq!(s.meta_seek(0, false), 0);
    }

    #[test]
    fn no_loop_clamps_to_end() {
        let s = make_sample(10, LoopType::No, 0, 0);
        assert_eq!(s.meta_seek(5, false), 5);
        assert_eq!(s.meta_seek(9, false), 9);
        assert_eq!(s.meta_seek(10, false), 9);
        assert_eq!(s.meta_seek(100, false), 9);
    }

    #[test]
    fn forward_loop() {
        let s = make_sample(10, LoopType::Forward, 4, 4);
        assert_eq!(s.meta_seek(3, false), 3);
        assert_eq!(s.meta_seek(7, false), 7);
        assert_eq!(s.meta_seek(8, false), 4);
        assert_eq!(s.meta_seek(9, false), 5);
    }

    #[test]
    fn pingpong_loop() {
        let s = make_sample(10, LoopType::PingPong, 4, 4);
        assert_eq!(s.meta_seek(3, false), 3);
        assert_eq!(s.meta_seek(7, false), 7);
        assert_eq!(s.meta_seek(8, false), 7);
        assert_eq!(s.meta_seek(9, false), 6);
    }

    #[test]
    fn zero_loop_length_no_panic() {
        let s = make_sample(10, LoopType::Forward, 4, 0);
        let _ = s.meta_seek(5, false);
    }

    // --- seek() — honest version returning Option ---

    #[test]
    fn seek_empty_sample_is_none() {
        let s = make_sample(0, LoopType::No, 0, 0);
        assert_eq!(s.seek(0, false), None);
        assert_eq!(s.seek(100, false), None);
    }

    #[test]
    fn seek_no_loop_past_end_is_none() {
        // Past-end on a non-looping sample must report None, not
        // clamp to len-1 — letting it clamp would trap the playback
        // position on the last frame and the voice would never
        // self-terminate.
        let s = make_sample(10, LoopType::No, 0, 0);
        assert_eq!(s.seek(0, false), Some(0));
        assert_eq!(s.seek(9, false), Some(9));
        assert_eq!(s.seek(10, false), None);
        assert_eq!(s.seek(100, false), None);
    }

    #[test]
    fn seek_forward_loop_wraps() {
        let s = make_sample(10, LoopType::Forward, 4, 4);
        assert_eq!(s.seek(3, false), Some(3));
        assert_eq!(s.seek(7, false), Some(7));
        assert_eq!(s.seek(8, false), Some(4));
        assert_eq!(s.seek(9, false), Some(5));
        // Forward loop never ends.
        assert_eq!(s.seek(1000, false), Some(4));
    }

    #[test]
    fn seek_pingpong_loop_wraps() {
        let s = make_sample(10, LoopType::PingPong, 4, 4);
        assert_eq!(s.seek(3, false), Some(3));
        assert_eq!(s.seek(7, false), Some(7));
        assert_eq!(s.seek(8, false), Some(7));
        assert_eq!(s.seek(9, false), Some(6));
    }

    #[test]
    fn seek_sustain_loop_used_while_sustained() {
        // With sustain_loop != No, the sustain loop is used while
        // sustain == true; after key-off (sustain == false) the normal
        // loop takes over.
        let mut s = make_sample(20, LoopType::No, 0, 0);
        s.sustain_loop_flag = LoopType::Forward;
        s.sustain_loop_start = 4;
        s.sustain_loop_length = 4;
        // Sustained: sustain loop wraps within [4, 8).
        assert_eq!(s.seek(8, true), Some(4));
        assert_eq!(s.seek(100, true), Some(4));
        // Released: falls back to the (No) loop, so past-end → None.
        assert_eq!(s.seek(100, false), None);
    }
}
