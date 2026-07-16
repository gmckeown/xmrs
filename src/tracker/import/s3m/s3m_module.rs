//! ScreamTracker 3 (.S3M) file loader — entry point for the
//! `import_s3m` feature. Parses the `SCRM` header, the channel
//! settings (with optional Adlib mapping), the order table,
//! instrument and sample headers (PCM + Adlib opaque blocks),
//! and the pattern bytes. Produces a `Module` populated with
//! `crate::tracker::profiles::st3()` quirks plus the file's
//! `amigalimits` masterflag overlay.

use crate::core::fixed::units::{ChannelVolume, Finetune, Frequency, Panning, Period, Volume};
use crate::prelude::*;
use crate::tracker::codepage::Codepage;
use crate::tracker::import::bin_reader::{bytes_to_trimmed_string, BinReader, ImportError};
use crate::tracker::import::memory::ImportMemory;
use crate::tracker::import::memory::MemoryType;
use crate::tracker::import::orders_helper;
use crate::tracker::import::patternslot::PatternSlot;

use alloc::format;
use alloc::string::String;
use alloc::{vec, vec::Vec};

use super::s3m_effect::S3mEffect;
use crate::tracker::import::memory::S3mImportFlags;

/// S3M file header (96 bytes).
#[derive(Default, Debug)]
struct S3mHeader {
    title: String,
    /// 0x1A
    sig1: u8,
    song_type: u8,
    reserved1: u16,
    order_count: u16,
    instrument_count: u16,
    pattern_count: u16,
    flags: u16,
    version: u16,
    sample_type: u16,
    /// SCRM
    sig2: String,
    global_volume: u8,
    speed: u8,
    tempo: u8,
    master_volume: u8, // bit7: stereo, bits 6-0: volume
    ultra_click_removal: u8,
    pan: u8,
    reserved2: [u8; 8],
    ptr_special: u16, // parapointer to additional data ? see flags, bit7
    channel_settings: [u8; 32],
}

impl S3mHeader {
    fn read(r: &mut BinReader) -> Result<Self, ImportError> {
        let title_bytes: [u8; 28] = r.read_array()?;
        let sig1 = r.read_u8()?;
        let song_type = r.read_u8()?;
        let reserved1 = r.read_u16_le()?;
        let order_count = r.read_u16_le()?;
        let instrument_count = r.read_u16_le()?;
        let pattern_count = r.read_u16_le()?;
        let flags = r.read_u16_le()?;
        let version = r.read_u16_le()?;
        let sample_type = r.read_u16_le()?;
        let sig2_bytes: [u8; 4] = r.read_array()?;
        let global_volume = r.read_u8()?;
        let speed = r.read_u8()?;
        let tempo = r.read_u8()?;
        let master_volume = r.read_u8()?;
        let ultra_click_removal = r.read_u8()?;
        let pan = r.read_u8()?;
        let reserved2: [u8; 8] = r.read_array()?;
        let ptr_special = r.read_u16_le()?;
        let channel_settings: [u8; 32] = r.read_array()?;
        Ok(Self {
            title: bytes_to_trimmed_string(&title_bytes),
            sig1,
            song_type,
            reserved1,
            order_count,
            instrument_count,
            pattern_count,
            flags,
            version,
            sample_type,
            sig2: bytes_to_trimmed_string(&sig2_bytes),
            global_volume,
            speed,
            tempo,
            master_volume,
            ultra_click_removal,
            pan,
            reserved2,
            ptr_special,
            channel_settings,
        })
    }
}

/// S3M PCM sample instrument record (offset `+13` past the
/// discriminator + 12-byte filename prefix; total record size on
/// disk is 80 bytes).
#[derive(Debug, Default)]
struct S3mPcmInstr {
    /// offset = ((ptr_data_h << 16) | ptr_data_l) * 16
    ptr_data_h: u8,
    ptr_data_l: u16,

    len: u32,
    loop_start: u32,
    loop_end: u32,

    /// 0-63
    volume: u8,
    dsk: u8,
    /// 0=unpacked, 1=amiga (module memory marker), 2=DP30ADPCM
    /// — but no major decoder honours this byte. Schism's
    /// `fmt/s3m.c:279` skips it as "packing info (never used)"
    /// and st3play's `digdata.h:91` notes ADPCM is "not used in
    /// st3play". Treated identically here: every sample is read
    /// as raw PCM.
    pack: u8,
    // 1=loop on, 2=stereo (data left then data right), 4=s16le
    flags: u8,
    /// sample rate for middle-c note (C-4)
    c2spd: u32,
    internal: [u8; 12],
    title: String,
    /// SCRS
    sig: String,
}

impl S3mPcmInstr {
    fn read(r: &mut BinReader) -> Result<Self, ImportError> {
        let ptr_data_h = r.read_u8()?;
        let ptr_data_l = r.read_u16_le()?;
        let len = r.read_u32_le()?;
        let loop_start = r.read_u32_le()?;
        let loop_end = r.read_u32_le()?;
        let volume = r.read_u8()?;
        let dsk = r.read_u8()?;
        let pack = r.read_u8()?;
        let flags = r.read_u8()?;
        let c2spd = r.read_u32_le()?;
        let internal: [u8; 12] = r.read_array()?;
        let title_bytes: [u8; 28] = r.read_array()?;
        let sig_bytes: [u8; 4] = r.read_array()?;
        Ok(Self {
            ptr_data_h,
            ptr_data_l,
            len,
            loop_start,
            loop_end,
            volume,
            dsk,
            pack,
            flags,
            c2spd,
            internal,
            title: bytes_to_trimmed_string(&title_bytes),
            sig: bytes_to_trimmed_string(&sig_bytes),
        })
    }
}

impl S3mPcmInstr {
    fn is_loop(&self) -> bool {
        self.flags & 1 != 0
    }

    fn is_stereo(&self) -> bool {
        self.flags & 2 != 0
    }

    fn is_16bits(&self) -> bool {
        self.flags & 4 != 0
    }

    fn get_sample_offset(&self) -> usize {
        (((self.ptr_data_h as usize) << 16) | (self.ptr_data_l as usize)) << 4
    }

    fn get_sample_data(&self, data: &[u8]) -> Result<SampleDataType, ImportError> {
        // The `pack` byte is essentially never honoured by real
        // S3M decoders. ScreamTracker 3 itself documented it as
        // `0=normal(disk), 1=amiga(module), 2=ADPCM`, but
        // st3play's `digdata.h:91` explicitly notes that ADPCM
        // is "not used in st3play", and schism's `fmt/s3m.c:279`
        // skips the byte with the comment `"packing info (never
        // used)"`. We mirror that: just decode every sample as
        // raw PCM regardless of the packing nibble. Real-world
        // S3M files with non-zero pack are vanishingly rare, and
        // the cost of being stricter than the reference players
        // is rejecting files they would happily play.

        let offset = self.get_sample_offset();

        if offset > data.len() {
            return Err(ImportError::Other("S3M sample offset out of bounds"));
        }
        let available = data.len() - offset;

        // `self.len` is a count of sample *values* on disk (not frames, not
        // raw bytes). For 16-bit samples each value takes 2 bytes, hence
        // `bytes_per_value`. Stereo is handled downstream: `convert_*_sample`
        // splits the buffer in half (L then R), so stereo gets half as many
        // final frames as `self.len`, which matches the original behaviour.
        let bytes_per_value: usize = if self.is_16bits() { 2 } else { 1 };
        let wanted_bytes = (self.len as usize).saturating_mul(bytes_per_value);

        // Truncate to what is actually available — fixes "miracle man.s3m"
        // and other broken S3Ms where the sample header over-claims. We
        // must keep whole values (2 bytes for 16-bit), otherwise the
        // u8→u16 conversion would fail on an odd tail.
        let usable_bytes = wanted_bytes.min(available);
        let usable_bytes = usable_bytes - (usable_bytes % bytes_per_value);
        let len = usable_bytes / bytes_per_value; // count in sample *values*

        let dst = if self.is_16bits() {
            let src = Self::convert_u8_to_u16_vec(&data[offset..offset + len * 2])?;
            if self.is_stereo() {
                SampleDataType::Stereo16(self.convert_16bit_sample(src.as_slice()).into())
            } else {
                SampleDataType::Mono16(self.convert_16bit_sample(src.as_slice()).into())
            }
        } else {
            let src = &data[offset..offset + len];
            if self.is_stereo() {
                SampleDataType::Stereo8(self.convert_8bit_sample(src).into())
            } else {
                SampleDataType::Mono8(self.convert_8bit_sample(src).into())
            }
        };

        Ok(dst)
    }

    fn convert_u8_to_u16_vec(input: &[u8]) -> Result<Vec<u16>, ImportError> {
        if !input.len().is_multiple_of(2) {
            return Err(ImportError::Other("input is odd!"));
        }

        let mut output = Vec::with_capacity(input.len() / 2);

        for chunk in input.chunks_exact(2) {
            let value = u16::from_le_bytes([chunk[0], chunk[1]]);
            output.push(value);
        }
        Ok(output)
    }

    fn convert_8bit_sample(&self, p: &[u8]) -> Vec<i8> {
        let length = p.len();
        let mut dst: Vec<i8> = vec![];

        if self.is_stereo() {
            let half_length = length / 2;
            let (left_channel, right_channel) = p.split_at(half_length);

            for i in 0..half_length {
                dst.push(u8_pcm_to_i8(left_channel[i]));
                dst.push(u8_pcm_to_i8(right_channel[i]));
            }
        } else {
            for &v in p.iter().take(length) {
                dst.push(u8_pcm_to_i8(v))
            }
        }
        dst
    }

    fn convert_16bit_sample(&self, p: &[u16]) -> Vec<i16> {
        let length = p.len();
        let mut dst: Vec<i16> = vec![];

        if self.is_stereo() {
            let half_length = length / 2;
            let (left_channel, right_channel) = p.split_at(half_length);

            for i in 0..half_length {
                dst.push(u16_pcm_to_i16(left_channel[i]));
                dst.push(u16_pcm_to_i16(right_channel[i]));
            }
        } else {
            for &v in p.iter().take(length) {
                dst.push(u16_pcm_to_i16(v))
            }
        }
        dst
    }
}

/// Convert an unsigned-PCM byte to signed-PCM by flipping the
/// midpoint bit. S3M stores 8-bit PCM samples as unsigned
/// (`0..=255`, silence at `128`); the audio path expects signed
/// (`-128..=127`, silence at `0`). XOR with the MSB inverts the
/// high bit, mapping `0 ↔ -128`, `128 ↔ 0`, `255 ↔ +127`.
#[inline]
const fn u8_pcm_to_i8(u: u8) -> i8 {
    (u ^ 0x80) as i8
}

/// 16-bit equivalent of [`u8_pcm_to_i8`].
#[inline]
const fn u16_pcm_to_i16(u: u16) -> i16 {
    (u ^ 0x8000) as i16
}

/// S3M AdLib (OPL) instrument record (offset `+13` past the
/// discriminator + 12-byte filename prefix).
#[derive(Debug)]
struct S3mOplInstr {
    reserved1: [u8; 3],

    mod0: u8,
    car1: u8,
    mod2: u8,
    car3: u8,
    mod4: u8,
    car5: u8,
    mod6: u8,
    car7: u8,
    mod8: u8,
    car9: u8,
    mod10: u8,
    unused11: u8,

    volume: u8,
    dsk: u8,
    reserved2: u16,
    c2spd: u32,
    internal: [u8; 12],
    pub title: String,
    /// SCRI
    sig: String,
}

impl S3mOplInstr {
    fn read(r: &mut BinReader) -> Result<Self, ImportError> {
        let reserved1: [u8; 3] = r.read_array()?;
        let mod0 = r.read_u8()?;
        let car1 = r.read_u8()?;
        let mod2 = r.read_u8()?;
        let car3 = r.read_u8()?;
        let mod4 = r.read_u8()?;
        let car5 = r.read_u8()?;
        let mod6 = r.read_u8()?;
        let car7 = r.read_u8()?;
        let mod8 = r.read_u8()?;
        let car9 = r.read_u8()?;
        let mod10 = r.read_u8()?;
        let unused11 = r.read_u8()?;
        let volume = r.read_u8()?;
        let dsk = r.read_u8()?;
        let reserved2 = r.read_u16_le()?;
        let c2spd = r.read_u32_le()?;
        let internal: [u8; 12] = r.read_array()?;
        let title_bytes: [u8; 28] = r.read_array()?;
        let sig_bytes: [u8; 4] = r.read_array()?;
        Ok(Self {
            reserved1,
            mod0,
            car1,
            mod2,
            car3,
            mod4,
            car5,
            mod6,
            car7,
            mod8,
            car9,
            mod10,
            unused11,
            volume,
            dsk,
            reserved2,
            c2spd,
            internal,
            title: bytes_to_trimmed_string(&title_bytes),
            sig: bytes_to_trimmed_string(&sig_bytes),
        })
    }
}

impl S3mOplInstr {
    pub fn to_instr_opl(&self) -> InstrOpl {
        // Match the PCM path — S3M is Amiga-period natively.
        let ph = PeriodHelper::new(FrequencyType::AmigaFrequencies, false);
        let mut i_opl = InstrOpl::default();
        i_opl.element.modulator = MdiOpl {
            // KSL is the full 2-bit field (OPL reg 0x40 bits 7-6), indexing
            // the documented {off, 3, 1.5, 6} dB/oct key-scale ladder directly
            // (`v>>6` selects the ladder entry, no bit-swap). The old `(bit6)|(bit7)`
            // collapsed regs 1/2/3 all to 1, so KSL=2 lost ~2 dB and KSL=3
            // ~4 dB of slope — silently over-driving FM modulators that use
            // strong key-scaling (e.g. `in_the_face_of_danger.s3m` #3).
            ksl: self.mod2 >> 6,
            multiple: self.mod0 & 0b0000_1111,
            feedback: self.mod10 >> 1,
            attack: self.mod4 >> 4,
            sustain: self.mod6 >> 4,
            eg: (self.mod0 & 0b0010_0000) != 0, // OPL reg 0x20 bit 5: EG type (sustain)
            decay: self.mod4 & 0b0000_1111,
            release: self.mod6 & 0b0000_1111,
            total_level: self.mod2 & 0b0011_1111,
            am: (self.mod0 & 0b1000_0000) != 0, // OPL reg 0x20 bit 7: AM (tremolo)
            vib: (self.mod0 & 0b0100_0000) != 0,
            ksr: (self.mod0 & 0b0001_0000) != 0,
            con: (self.mod10 & 0b0000_0001) != 0,
        };
        i_opl.element.carrier = MdiOpl {
            ksl: self.car3 >> 6, // full 2-bit KSL field (see modulator above)
            multiple: self.car1 & 0b0000_1111,
            feedback: self.mod10 >> 1, // no way for carrier
            attack: self.car5 >> 4,
            sustain: self.car7 >> 4,
            eg: (self.car1 & 0b0010_0000) != 0, // OPL reg 0x20 bit 5: EG type (sustain)
            decay: self.car5 & 0b0000_1111,
            release: self.car7 & 0b0000_1111,
            total_level: self.car3 & 0b0011_1111,
            am: (self.car1 & 0b1000_0000) != 0, // OPL reg 0x20 bit 7: AM (tremolo)
            vib: (self.car1 & 0b0100_0000) != 0,
            ksr: (self.car1 & 0b0001_0000) != 0,
            con: (self.mod10 & 0b0000_0001) != 0, // no way for carrier
        };
        i_opl.element.modulator_wave_select = self.mod8;
        i_opl.element.carrier_wave_select = self.car9;
        i_opl.volume = self.volume;
        // C2-speed → (relative_pitch, finetune) using the Q-format
        // pitch tables. `c4freq_to_relative_pitch` returns
        // `(i8 semitones, PitchDelta Q8.8 fractional)`. Map the
        // fractional `PitchDelta` raw into a `Finetune` (Q1.15)
        // by `<< 7` (256→32768 = 2^7 widening). The fractional
        // part of `PitchDelta` here is bounded to ±0.5 semitone
        // by the splitting convention, well inside Q1.15 range.
        let f = Frequency::from_hz(self.c2spd);
        let rn = ph.c4freq_to_relative_pitch(f);
        i_opl.finetune =
            crate::core::fixed::units::Finetune::from_q15_i32_sat((rn.1.as_q8_8_i32()) << 7);
        i_opl.relative_pitch = rn.0;

        i_opl
    }
}

#[derive(Debug)]
enum S3mInstrument {
    PcmInstrument(S3mPcmInstr),
    OplInstrument(S3mOplInstr),
}

#[derive(Debug)]
struct S3mMetaInstrument {
    discriminator: u8,
    filename: String,
    value: S3mInstrument,
    sample: Option<SampleDataType>,
}

impl S3mMetaInstrument {
    /// Placeholder for an empty instrument slot (parapointer == 0).
    /// Needed to preserve 1:1 indexing between file slots and `self.instruments`,
    /// since S3M pattern slots reference instruments by absolute index.
    fn empty() -> Self {
        Self {
            discriminator: 0,
            filename: String::new(),
            value: S3mInstrument::PcmInstrument(S3mPcmInstr::default()),
            sample: None,
        }
    }

    fn new(data: &[u8], offset: usize) -> Result<Self, ImportError> {
        if offset + 13 > data.len() {
            return Err(ImportError::OutOfRange(
                "S3M instrument offset out of range",
            ));
        }
        let discriminator = data[offset];
        let filename_bytes: [u8; 12] = {
            let mut b = [0u8; 12];
            b.copy_from_slice(&data[offset + 1..offset + 13]);
            b
        };
        let filename = bytes_to_trimmed_string(&filename_bytes);
        let mut sample = None;
        let mut r = BinReader::new(&data[offset + 13..]);
        let value = match discriminator {
            0 => {
                // Empty Instrument, we use PcmInstrument not to forget informations
                S3mInstrument::PcmInstrument(S3mPcmInstr::read(&mut r)?)
            }
            1 => {
                let i = S3mPcmInstr::read(&mut r)?;
                if i.sig != "SCRS" {
                    return Err(ImportError::InvalidMagic("Not a SCRS instr?"));
                }
                sample = Some(i.get_sample_data(data)?);
                S3mInstrument::PcmInstrument(i)
            }
            2..=7 => {
                let i = S3mOplInstr::read(&mut r)?;
                if i.sig != "SCRI" {
                    return Err(ImportError::InvalidMagic("Not a SCRI instr?"));
                }
                S3mInstrument::OplInstrument(i)
            }
            _ => S3mInstrument::PcmInstrument(S3mPcmInstr::default()),
        };
        Ok(Self {
            discriminator,
            filename,
            value,
            sample,
        })
    }
}

#[derive(Default, Debug)]
pub struct S3mModule {
    header: S3mHeader,
    positions: Vec<u8>,
    instruments: Vec<S3mMetaInstrument>,
    patterns: Vec<Vec<Vec<PatternSlot>>>,
    /// Song message (free text), when the file has flag bit 0x80
    /// set and `ptr_special` points at usable text. The S3M format
    /// uses CR (0x0D) as line separator; we normalise to LF.
    /// Empty when no readable message was found.
    message: String,
}

impl S3mModule {
    pub fn load(ser_s3m_module: &[u8]) -> Result<S3mModule, ImportError> {
        let mut s3m = S3mModule {
            ..Default::default()
        };

        // === load header

        let s = 96;
        if ser_s3m_module.len() < s {
            return Result::Err(ImportError::Other("Not an S3M module?"));
        }
        let mut r = BinReader::new(&ser_s3m_module[0..s]);
        s3m.header = S3mHeader::read(&mut r)?;
        let data = &ser_s3m_module[s..];

        // The authoritative signature is the `"SCRM"` magic at offset
        // 0x2C plus the type byte (0x10 = ScreamTracker 3 module) at
        // 0x1D. The byte at 0x1C (`sig1`) is only the DOS `type`-command
        // EOF marker (0x1A) — purely cosmetic, and some real files
        // (e.g. `strshine.s3m`) carry 0x00 there. Reference loaders
        // (libopenmpt) key off `"SCRM"`, not the EOF byte, so we don't
        // reject on `sig1`.
        if s3m.header.song_type != 0x10 || s3m.header.sig2 != "SCRM" {
            return Result::Err(ImportError::InvalidMagic("Not an S3M module?"));
        }

        // === positions offsets

        let s = s3m.header.order_count as usize;
        if data.len() < s {
            return Result::Err(ImportError::Other("Not an S3M module?"));
        }
        s3m.positions = data[0..s].to_vec();
        let data = &data[s..];

        // remove pattern separators (254)
        s3m.positions.retain(|&x| x != 254);
        // cut on first eop (255)
        s3m.positions = s3m
            .positions
            .iter()
            .take_while(|&x| *x != 255)
            .cloned()
            .collect();

        // === sample offsets

        let s = 2 * s3m.header.instrument_count as usize;
        if data.len() < s {
            return Result::Err(ImportError::Other("Not an S3M module?"));
        }
        let sample_offsets: Vec<u32> = data[0..s]
            .chunks(2)
            .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], 0, 0]) << 4)
            .collect();
        let data = &data[s..];

        // === pattern offsets

        let s = 2 * s3m.header.pattern_count as usize;
        if data.len() < s {
            return Result::Err(ImportError::Other("Not an S3M module?"));
        }
        let pattern_offsets: Vec<u32> = data[0..s]
            .chunks(2)
            .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], 0, 0]) << 4)
            .collect();

        // === Samples
        //
        // S3M pattern slots reference instruments by absolute 1-based index,
        // so empty slots (parapointer == 0) MUST be kept as placeholders
        // to preserve the mapping.

        for &offset in &sample_offsets {
            if offset == 0 {
                s3m.instruments.push(S3mMetaInstrument::empty());
                continue;
            }
            s3m.instruments
                .push(S3mMetaInstrument::new(ser_s3m_module, offset as usize)?);
        }

        // === Patterns
        //
        // Order list entries reference patterns by absolute index, so empty
        // slots (parapointer == 0) MUST be kept as placeholders (mirroring the
        // IT loader convention) instead of being skipped.

        for offset in pattern_offsets {
            if offset == 0 {
                s3m.patterns.push(vec![vec![]]);
                continue;
            }
            let start = offset as usize;
            if start + 2 > ser_s3m_module.len() {
                return Err(ImportError::UnexpectedEof);
            }
            let data = &ser_s3m_module[start..];
            let len: usize = (data[0] as usize) | ((data[1] as usize) << 8);
            // The 2-byte length field is the size of the packed body that
            // *follows* it — it does NOT count the 2 length bytes (Schism
            // `fmt/s3m.c`: `end = paraptr + len + 2`). The body is therefore
            // `data[2..2 + len]`. The previous `data[2..len]` treated `len`
            // as total-including-header, lopping the last 2 body bytes off
            // every pattern: harmless when the tail was padding / trailing
            // EOLs, but on a tightly-packed pattern (e.g. `classical
            // favorites II` pattern 35) it truncated the final slot's effect
            // parameter — an `A30` (set speed 48) read back as `A00`,
            // dropping the speed change and de-syncing the whole song.
            // The declared end may legitimately run past EOF on files whose
            // last pattern is tail-truncated (e.g. ModArchive's `adlib.s3m`,
            // 2 bytes short). Schism does NOT reject these: its decode loop
            // bounds on `row < 64 && slurp_tell(fp) < end` *and* breaks with
            // a "file truncated" warning the moment `slurp_getc` returns EOF
            // (`fmt/s3m.c`), so it reads up to the lesser of `end` and EOF.
            // We mirror that by clamping the body to the available bytes; the
            // row decoder (`decode_pattern_slot`) already tolerates an event
            // cut mid-record via its `k < packed_data.len()` guards.
            let body_end = 2usize.saturating_add(len).min(data.len());
            let data = &data[2..body_end];
            let mut d2 = data;
            let mut pattern: Vec<Vec<PatternSlot>> = vec![];
            // Cap at 64 rows like Schism (`while (row < 64 && ...)`): the
            // packed body is exactly 64 EOL-terminated rows, but bounding the
            // decode keeps a malformed / over-long body from synthesising
            // phantom rows past the S3M-fixed 64.
            while !d2.is_empty() && pattern.len() < 64 {
                let (pss, next) = Self::process_pattern_row(d2)?;
                pattern.push(pss);
                d2 = next;
            }
            S3mEffect::resolve_pattern_memory(&mut pattern);
            s3m.patterns.push(pattern);
        }

        // === codepage detection + re-decode of every text field
        //
        // S3M has no encoding declaration. Although ScreamTracker
        // 3 was a DOS-only tool and authors used CP437 in
        // practice — including for ASCII-art sample names — the
        // format is also written today by cross-platform editors
        // (OpenMPT, Schism) running on systems that default to
        // UTF-8 / Latin-1. The detector reads the byte
        // distribution and decides per-file, so all paths are
        // covered without hard-coding CP437.
        //
        // The byte offsets used below match the on-disk layout
        // of `S3mHeader`, `S3mPcmInstr`, and `S3mOplInstr`. In
        // every PCM/OPL instrument record the title is 28 bytes
        // at offset `+48` past the parapointer (`13` for the
        // record's discriminator + filename + `35` of fixed
        // fields before the title), filename is 12 bytes at
        // offset `+1`.
        const HEADER_TITLE_LEN: usize = 28;
        const FILENAME_OFFSET: usize = 1;
        const FILENAME_LEN: usize = 12;
        const INSTR_TITLE_OFFSET: usize = 48;
        const INSTR_TITLE_LEN: usize = 28;

        // Raw bytes of the optional song message — extracted now
        // so they can participate in codepage detection along
        // with every other text field. `None` when there's no
        // valid message; we still pool nothing in that case.
        let message_bytes: Option<Vec<u8>> =
            if s3m.header.flags & 0x80 != 0 && s3m.header.ptr_special != 0 {
                Self::extract_song_message_bytes(ser_s3m_module, s3m.header.ptr_special)
            } else {
                None
            };

        // Pool every text field. References borrow from
        // `ser_s3m_module` / `message_bytes` and live only until
        // the codepage verdict is reached.
        //
        // Filename bytes are always part of the instrument
        // record regardless of discriminator. Title bytes are
        // only meaningful when the discriminator declares a PCM
        // (`0` placeholder / `1`) or AdLib (`2..=7`) instrument;
        // anything past that, the existing loader returns a
        // default-empty instrument and the bytes at
        // `offset + 48` are not a title — including them would
        // pollute the codepage histogram with random binary.
        let mut text_fields: Vec<&[u8]> = Vec::new();
        text_fields.push(&ser_s3m_module[0..HEADER_TITLE_LEN]);
        for (i, &offset) in sample_offsets.iter().enumerate() {
            if offset == 0 {
                continue;
            }
            let off = offset as usize;
            if off + FILENAME_OFFSET + FILENAME_LEN <= ser_s3m_module.len() {
                text_fields.push(
                    &ser_s3m_module[off + FILENAME_OFFSET..off + FILENAME_OFFSET + FILENAME_LEN],
                );
            }
            let disc = s3m.instruments[i].discriminator;
            if disc <= 7 && off + INSTR_TITLE_OFFSET + INSTR_TITLE_LEN <= ser_s3m_module.len() {
                text_fields.push(
                    &ser_s3m_module
                        [off + INSTR_TITLE_OFFSET..off + INSTR_TITLE_OFFSET + INSTR_TITLE_LEN],
                );
            }
        }
        if let Some(ref msg) = message_bytes {
            text_fields.push(msg);
        }
        let codepage = Codepage::detect_from_fields(&text_fields);

        // Overwrite every text field with a codepage-aware
        // decode of the same byte range. Same discriminator gate
        // as above for the title field.
        s3m.header.title = codepage.decode_name(&ser_s3m_module[0..HEADER_TITLE_LEN]);
        for (i, &offset) in sample_offsets.iter().enumerate() {
            if offset == 0 {
                continue;
            }
            let off = offset as usize;
            if off + FILENAME_OFFSET + FILENAME_LEN <= ser_s3m_module.len() {
                s3m.instruments[i].filename = codepage.decode_name(
                    &ser_s3m_module[off + FILENAME_OFFSET..off + FILENAME_OFFSET + FILENAME_LEN],
                );
            }
            let disc = s3m.instruments[i].discriminator;
            if disc <= 7 && off + INSTR_TITLE_OFFSET + INSTR_TITLE_LEN <= ser_s3m_module.len() {
                let title = codepage.decode_name(
                    &ser_s3m_module
                        [off + INSTR_TITLE_OFFSET..off + INSTR_TITLE_OFFSET + INSTR_TITLE_LEN],
                );
                match &mut s3m.instruments[i].value {
                    S3mInstrument::PcmInstrument(p) => p.title = title,
                    S3mInstrument::OplInstrument(o) => o.title = title,
                }
            }
        }
        if let Some(msg_bytes) = message_bytes {
            // Decode the cleaned message bytes (CR already
            // normalised to LF by `extract_song_message_bytes`)
            // through the detected codepage and trim trailing
            // whitespace.
            s3m.message = codepage.decode(&msg_bytes).trim().into();
        }

        Ok(s3m)
    }

    /// Read up to 1024 bytes of NUL-terminated text at the
    /// parapointer offset, returning the raw bytes (CR
    /// normalised to LF) without committing to any encoding.
    /// Encoding is selected later by the file-wide codepage
    /// detector.
    ///
    /// Returns `None` when the data doesn't look like text
    /// (less than ~75 % "plausibly-text" bytes), so we silently
    /// fall back to "no message" instead of polluting
    /// `Module.comment` with binary noise from a tracker that
    /// repurposed `ptr_special` for something else.
    ///
    /// The "plausibly-text" gate now accepts every byte
    /// `≥ 0x20` plus CR / LF / TAB. The previous strict
    /// `0x20..=0x7E` rule rejected legitimate CP437/Latin-1
    /// messages outright.
    fn extract_song_message_bytes(data: &[u8], ptr_special: u16) -> Option<Vec<u8>> {
        let offset = (ptr_special as usize).checked_mul(16)?;
        if offset >= data.len() {
            return None;
        }
        let raw = &data[offset..];
        let scan_len = raw.len().min(1024);
        let scanned = &raw[..scan_len];
        let end = scanned.iter().position(|&b| b == 0).unwrap_or(scan_len);
        let text_bytes = &scanned[..end];
        if text_bytes.is_empty() {
            return None;
        }
        // Sanity gate: at least 75 % of bytes must be plausibly
        // text — ASCII printable / high-bit / CR / LF / TAB.
        // Pure binary fails this and is rejected outright.
        let plausible = text_bytes
            .iter()
            .filter(|&&b| b == b'\r' || b == b'\n' || b == b'\t' || b >= 0x20)
            .count();
        if plausible * 4 < text_bytes.len() * 3 {
            return None;
        }
        // Normalise: CR → LF (S3M's native line separator is CR;
        // editors / terminals expect LF). The actual
        // byte → char decoding is deferred to the codepage pass.
        Some(
            text_bytes
                .iter()
                .map(|&b| if b == b'\r' { b'\n' } else { b })
                .collect(),
        )
    }

    // load one pattern row
    fn process_pattern_row(data: &[u8]) -> Result<(Vec<PatternSlot>, &[u8]), ImportError> {
        let mut d2 = data;
        // IMPORTANT: 255 is the sentinel for "no volume command". Using 0
        // here would be interpreted by `s3m_walk_volume` as an explicit
        // Volume(0.0), which silences every channel on every row that has
        // no volume column — i.e. the vast majority of S3M rows.
        let mut pss: Vec<PatternSlot> = vec![
            PatternSlot {
                volume: 255,
                ..PatternSlot::default()
            };
            32
        ];
        while !d2.is_empty() {
            match Self::decode_pattern_slot(d2) {
                Some((channel, ps, next)) => {
                    pss[channel] = ps;
                    d2 = next;
                }
                None => {
                    d2 = &d2[1..];
                    break;
                }
            }
        }
        Ok((pss, d2))
    }

    // return channel, PatternSlot, next data
    fn decode_pattern_slot(packed_data: &[u8]) -> Option<(usize, PatternSlot, &[u8])> {
        let mut k = 0;

        let what = packed_data[k];
        k += 1;

        if what == 0 {
            return None; // EOL
        }

        let channel = (what & 0x1F) as usize;

        // 255 is the "no volume command" sentinel — same reason as in
        // `process_pattern_row`. Must be set BEFORE the volume-column
        // branch so that when the slot has no volume column (bit 0x40 of
        // `what` clear), the slot doesn't accidentally carry volume 0.
        let mut slot: PatternSlot = PatternSlot {
            volume: 255,
            ..PatternSlot::default()
        };

        // Note+Instr?
        if what & 0x20 != 0 {
            if k < packed_data.len() {
                let note = packed_data[k];
                slot.note = match note {
                    254 => CellNote::KeyOff,
                    255 => CellNote::Empty,
                    _ => {
                        let tmp_pitch = (note & 0xF) + (note >> 4) * 12;
                        if tmp_pitch > 96 {
                            CellNote::Empty
                        } else {
                            match Pitch::try_from(tmp_pitch) {
                                Ok(p) => CellNote::Play(p),
                                Err(_) => CellNote::Empty,
                            }
                        }
                    }
                };
                k += 1;
            }

            if k < packed_data.len() {
                slot.instrument = if packed_data[k] != 0 {
                    Some(packed_data[k] as usize - 1)
                } else {
                    None
                };
                k += 1;
            }
        }

        // Volume?
        //
        // Valid ranges:
        //   0..=64       → set volume
        //   0x80..=0xC0  → MPT-style "set panning" (left=0x80, right=0xC0)
        //   anything else → invalid → map to 255 (the "no command" sentinel)
        if what & 0x40 != 0 && k < packed_data.len() {
            let v = packed_data[k];
            slot.volume = if v <= 64 || (0x80..=0xC0).contains(&v) {
                v
            } else {
                255
            };
            k += 1;
        }

        // effect and data?
        if what & 0x80 != 0 {
            if k < packed_data.len() {
                slot.effect_type = packed_data[k];
                k += 1;
            }

            if k < packed_data.len() {
                slot.effect_parameter = packed_data[k];
                k += 1;
            }
        }

        Some((channel, slot, &packed_data[k..]))
    }

    /// Inject S3M header defaults (global volume) as effects on
    /// row 0 of the first pattern that playback will start with.
    /// This avoids renumbering patterns/positions — which would
    /// break any `goto(position, …)` call made by the player.
    ///
    /// Global volume is written as `Vxx` (effect 22) on any channel
    /// that hasn't already been used on row 0. If every slot on
    /// that row already carries an effect, the default is silently
    /// dropped — better to lose a default than overwrite explicit
    /// user data. Mirrors real tracker behaviour: state is
    /// established at song-start; a `goto` mid-song forfeits that
    /// state, which is correct.
    ///
    /// Per-channel panning used to ride the same path
    /// (`Xxx` on row 0), but it now lives on
    /// [`Module::channel_defaults`] — a clean header-side
    /// data path that doesn't depend on row 0 having a free slot.
    fn inject_header_defaults(&self, patterns: &mut [Vec<Vec<PatternSlot>>]) {
        let first_pat = match self.positions.first() {
            Some(&p) => p as usize,
            None => return,
        };
        let row = match patterns.get_mut(first_pat).and_then(|p| p.first_mut()) {
            Some(r) if !r.is_empty() => r,
            _ => return,
        };

        // Global volume (Vxx = effect 22). Any channel can carry a global
        // effect; pick the first free slot.
        let gv = self.header.global_volume.min(64);
        if gv != 64 {
            if let Some(slot) = row
                .iter_mut()
                .find(|s| s.effect_type == 0 && s.effect_parameter == 0)
            {
                slot.effect_type = 22;
                slot.effect_parameter = gv;
            }
        }
    }

    /// Build the per-channel default state from S3M's
    /// `channel_settings` block. Returns a vector aligned on the
    /// channel slots declared by the file. Each entry's pan and
    /// mute flag come straight from the channel byte.
    ///
    /// The S3M channel-settings byte is a documented format field;
    /// its meaning (an ST3 playback fact, not borrowed code):
    /// - bit 0x80 → channel muted (high bit is a mute flag; strip it
    ///   before classifying the type)
    /// - type 0x00..=0x07 → L1-L8 (soft left)
    /// - type 0x08..=0x0F → R1-R8 (soft right)
    /// - type 0x10..=0x18 → Adlib A1-A9 (centre, no PCM pan)
    /// - type 0x19..=0x7F (incl. the disabled sentinel 0xFF, whose
    ///   stripped type is 0x7F) → disabled / "broken" → **muted**.
    ///   ST3 shows these as `(--)` and never plays them; the pattern
    ///   data they carry is leftover and must stay silent.
    ///
    /// The earlier code special-cased `0xFF` *before* testing the
    /// mute bit and returned an unmuted default, so disabled slots
    /// played their residual notes (e.g. PANIC.S3M ch25/ch31 — a
    /// stray bass + clicks on an out-of-range instrument).
    fn header_channel_defaults(&self) -> Vec<ChannelDefault> {
        self.header
            .channel_settings
            .iter()
            .map(|&s| {
                let muted_flag = s & 0x80 != 0;
                let c = s & 0x7F;
                let (panning, type_muted) = if c < 0x08 {
                    (Some(Panning::from_byte_255(0x40)), false) // L1-L8 soft left
                } else if c < 0x10 {
                    (Some(Panning::from_byte_255(0xC0)), false) // R1-R8 soft right
                } else if c < 0x19 {
                    (None, false) // Adlib A1-A9 — centre, no PCM pan
                } else {
                    (None, true) // Disabled (0xFF/0x7F) or broken type
                };
                ChannelDefault {
                    muted: muted_flag || type_muted,
                    panning,
                    ..ChannelDefault::default()
                }
            })
            .collect()
    }

    pub fn to_module(&self) -> Module {
        // S3M is natively an Amiga-period format — Scream Tracker 3
        // inherited the Amiga clock-divider period math, and every
        // portamento/slide parameter in the wild was authored against
        // that model (including its natural period-saturation at high
        // pitches). Playing S3M data through Linear period math makes
        // extreme Fxy/Exy values (F80 in PANIC.S3M, etc.) sweep far
        // beyond where ST3 would saturate, producing aliased "shriek"
        // frequencies that don't exist in the original playback.
        //
        // Unlike IT, S3M has no file flag to select Linear mode — the
        // format predates that option. All S3M files should be treated
        // as Amiga.
        let ph = PeriodHelper::new(FrequencyType::AmigaFrequencies, false);
        let mut module = Module::default();

        // ST3 masterflags (see dig.c:setmasterflags). Bits we care
        // about:
        //   * bit 0x01 — `oldstvib`: ST2-compatibility vibrato
        //       (depth doubled). Absorbed at import.
        //   * bit 0x10 — `amigalimits`: period clamped to Amiga
        //       hardware range at runtime. Flipped into
        //       `module.quirks.period_clamp`.
        //   * bit 0x40 — `fastvolslide`: Dxy / Nxy apply on every
        //       tick. Absorbed at import via double-emission.
        // Other flags (bit 0x08 "vol0 opt", bit 0x20 "stereo") are
        // either performance hints or have no replay impact.
        let s3m_mflags = self.header.flags;
        let import_flags = S3mImportFlags {
            fastvolslide: s3m_mflags & 0x40 != 0,
            oldstvib: s3m_mflags & 0x01 != 0,
        };
        let amigalimits = s3m_mflags & 0x10 != 0;

        module.name = self.header.title.clone();
        // ST3's `version` byte pair (high nibble = tracker ID, low
        // 12 bits = version) — propagated to the comment so editors
        // can surface it. Mirrors what the XM importer does with its
        // header `tracker_name` + `version_number`. We render it as
        // a single hex word because the per-tracker layout of the
        // upper nibble is non-uniform (ST3 = 0x1, IT-export = 0x2,
        // Schism = 0x4, OpenMPT = 0x5, …) and a fixed major.minor
        // split would lie for half the corpus.
        //
        // Optional song message (when the file's flag 0x80 +
        // ptr_special pointed at usable text) is appended below
        // the version line — same pattern as the IT importer
        // appending `self.message` to its own comment.
        module.comment = if self.message.is_empty() {
            format!("S3M version 0x{:04X}", self.header.version)
        } else {
            format!(
                "S3M version 0x{:04X}\n\n{}",
                self.header.version, self.message
            )
        };
        // S3M's master-volume byte is encoded with bit 7 = stereo
        // flag and bits 6..0 = mixing volume in 0..127 range. Schism
        // (`fmt/s3m.c:166`) reads the same byte and stores it as
        // `mixing_volume`. We mirror that: strip bit 7 and normalise
        // by 128 to land in the same [0..1] domain the IT importer
        // uses, so the player's mix chain treats S3M and IT
        // uniformly. (We deliberately skip schism's optional ST3-era
        // rescaling at `fmt/s3m.c:233` — it only applies to a
        // legacy variant and would silently change levels for
        // mainstream S3Ms.)
        module.mix_volume = Volume::from_ratio((self.header.master_volume & 0x7F) as i32, 128);
        // Canonical ST3 replay profile. The amigalimits masterflag
        // is per-file, so we overlay it on top of the base profile.
        module.origin = Some(crate::tracker::format::ModuleFormat::S3m);
        module.quirks = {
            let mut q = crate::tracker::profiles::st3();
            if amigalimits {
                // ST3 `aspd` range `[453, 3424]` → xmrs Amiga units
                // `[113.25, 856]` (xmrs periods are 1/4 of ST3's).
                //
                // Truncated to integer `Period` after the migration
                // (was `(113.25, 856.0)`). The lost 0.25 sub-period
                // precision is well below an audible cent of pitch
                // and matches the typed `Period` storage.
                q.period_clamp = Some((Period::from_raw(113), Period::from_raw(856)));
            }
            q
        };
        module.frequency_type = FrequencyType::AmigaFrequencies;
        module.default_tempo = self.header.speed as usize;
        module.default_bpm = self.header.tempo as usize;

        // Inject header defaults (channel panning, global volume) into
        // row 0 of the first-played pattern. This preserves the original
        // position/pattern indexing — any `goto(position, …)` call keeps
        // referencing the same patterns it did before these defaults
        // were materialised.
        let mut patterns_for_unpack = self.patterns.clone();
        self.inject_header_defaults(&mut patterns_for_unpack);

        module.channel_defaults = self.header_channel_defaults();

        let pattern_order = orders_helper::parse_orders(&self.positions);

        let mut im = ImportMemory::default();
        im.s3m_flags = import_flags;
        let pattern = im.unpack_patterns(
            module.frequency_type,
            MemoryType::S3m,
            &pattern_order,
            &patterns_for_unpack,
        );

        for s3m_meta_instr in &self.instruments {
            match &s3m_meta_instr.value {
                S3mInstrument::PcmInstrument(pcm) => {
                    // Prepare sample
                    let data = s3m_meta_instr.sample.clone();
                    // Guard against degenerate c2spd values (0, or absurdly
                    // small) producing NaN/inf in `c4freq_to_relative_pitch`.
                    // 8372 Hz is the modern MIDI-aligned C-4 reference.
                    let c2spd_hz = if pcm.c2spd == 0 {
                        crate::core::fixed::tables::C4_FREQ_HZ
                    } else {
                        pcm.c2spd
                    };
                    let f = Frequency::from_hz(c2spd_hz);
                    let rn = ph.c4freq_to_relative_pitch(f);
                    // `rn.1` is `PitchDelta` (Q8.8 semitones, in
                    // `[-0.5, +0.5]` after round-to-nearest);
                    // `Sample.finetune` is `Finetune` (Q1.15
                    // where ±1 = ±1 semitone). Conversion is a
                    // simple `<< 7` (256 → 32768), with
                    // saturation guarding the corner cases.
                    let finetune = {
                        // PitchDelta is Q8.8 in `[-0.5, +0.5]`,
                        // Finetune is Q1.15 where `±1 = ±1
                        // semitone`. Width promotion is `<< 7`
                        // (256 → 32768), saturated.
                        let pd_raw = rn.1.as_q8_8_i32();
                        Finetune::from_q15_i32_sat(pd_raw << 7)
                    };
                    let sample = Sample {
                        name: s3m_meta_instr.filename.clone(),
                        relative_pitch: rn.0,
                        finetune,
                        volume: ChannelVolume::from_byte_64(pcm.volume),
                        // S3M has no per-sample default note volume.
                        default_note_volume: Volume::FULL,
                        panning: Panning::CENTER,
                        loop_flag: if pcm.is_loop() {
                            LoopType::Forward
                        } else {
                            LoopType::No
                        },
                        loop_start: pcm.loop_start,
                        loop_length: pcm.loop_end.saturating_sub(pcm.loop_start),
                        sustain_loop_flag: LoopType::No,
                        sustain_loop_start: 0,
                        sustain_loop_length: 0,
                        data,
                    };

                    // Create InstrDefault
                    let mut instr_def = InstrDefault::default();
                    instr_def.sample.push(Some(sample));
                    instr_def.keyboard.sample_for_pitch = [Some(0); 120];

                    // Create Instrument
                    let mut instr = Instrument::default();
                    instr.name = pcm.title.clone();
                    instr.instr_type = InstrumentType::Default(instr_def);

                    // Add Instrument to module
                    module.instrument.push(instr);
                }
                S3mInstrument::OplInstrument(opl) => {
                    // Create Instrument
                    let mut instr = Instrument::default();
                    instr.name = opl.title.clone();
                    let mut i_opl = opl.to_instr_opl();
                    // Adlib instrument type byte (discriminator): 2 = melody, 3 = bass drum,
                    // 4 = snare, 5 = tom-tom, 6 = cymbal, 7 = hi-hat (ScreamTracker 3 rhythm mode).
                    i_opl.rhythm = match s3m_meta_instr.discriminator {
                        3 => crate::tracker::instr_opl::OplRhythm::BassDrum,
                        4 => crate::tracker::instr_opl::OplRhythm::Snare,
                        5 => crate::tracker::instr_opl::OplRhythm::TomTom,
                        6 => crate::tracker::instr_opl::OplRhythm::Cymbal,
                        7 => crate::tracker::instr_opl::OplRhythm::HiHat,
                        _ => crate::tracker::instr_opl::OplRhythm::Melodic,
                    };
                    instr.instr_type = InstrumentType::Opl(i_opl);

                    // Add Instrument to module
                    module.instrument.push(instr);
                }
            }
        }

        crate::tracker::import::build::build_timeline_layer(&mut module, &pattern_order, &pattern);

        module
    }
}
