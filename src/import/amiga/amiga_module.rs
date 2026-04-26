use super::amiga_sample::AmigaSample;
use super::patternslot::PatternSlot;
use bincode::error::DecodeError;

use crate::import::import_memory::ImportMemory;
use crate::import::import_memory::MemoryType;
use crate::prelude::*;

use alloc::string::String;
use alloc::string::ToString;
use alloc::{vec, vec::Vec};

#[derive(Default, Debug)]
pub struct AmigaModule {
    title: String,
    samples: Vec<AmigaSample>, // 15 or 31
    song_length: u8,
    restart_position: u8,
    positions: Vec<u8>, // 128
    tag: String,
    patterns: Vec<Vec<Vec<PatternSlot>>>, // pattern, row, element
    audio: Vec<Vec<i8>>,
}

impl AmigaModule {
    fn get_number_of_tracks(&self) -> Option<u8> {
        match self.tag.as_str() {
            "TDZ1" => Some(1),
            "2CHN" | "TDZ2" => Some(2),
            "TDZ3" => Some(3),
            "M.K." | "M!K!" | "FLT4" | "NSMS" | "LARD" | "PATT" | "EXO4" | "N.T." | "M&K!"
            | "FEST" | "CD61" => Some(4),
            "5CHN" => Some(5),
            "6CHN" => Some(6),
            "7CHN" => Some(7),
            "8CHN" | "CD81" | "OKTA" | "OCTA" | "FLT8" | "EXO8" => Some(8),
            "9CHN" => Some(9),
            tag if tag.ends_with("CH") || tag.ends_with("CN") => {
                match &tag[..tag.len() - 2].parse::<u8>().unwrap_or(0) {
                    0 => None,
                    v => Some(*v),
                }
            }
            _ => None,
        }
    }

    fn get_number_of_samples(&self) -> usize {
        match self.get_number_of_tracks() {
            None => 15,
            _ => 31,
        }
    }

    fn get_number_of_patterns(&self) -> usize {
        1 + *self.positions.iter().max().unwrap_or(&0) as usize
    }

    pub fn load(ser_amiga_module: &[u8]) -> Result<AmigaModule, DecodeError> {
        let mut amiga = AmigaModule {
            ..Default::default()
        };

        amiga.title = String::from_utf8_lossy(&ser_amiga_module[0..22]).to_string();
        amiga.title = amiga
            .title
            .split('\0')
            .next()
            .unwrap_or("")
            .trim()
            .to_string();

        // get tag if any?
        amiga.tag = String::from_utf8_lossy(&ser_amiga_module[0x438..0x438 + 4]).to_string();

        let mut data = &ser_amiga_module[0x14..];

        // samples struct
        for _i in 0..amiga.get_number_of_samples() {
            let (d2, sample) = AmigaSample::load(data)?;
            data = d2;
            amiga.samples.push(sample);
        }

        amiga.song_length = data[0];
        amiga.restart_position = data[1];
        data = &data[2..];

        // positions
        amiga.positions.extend_from_slice(&data[..128]);
        data = &data[128..];

        // tag?
        if amiga.get_number_of_samples() != 15 {
            data = &data[4..];
        }

        // patterns
        let number_of_tracks = match amiga.get_number_of_tracks() {
            Some(n) => n as usize,
            None => 4, // default is 4...return Result::Err(DecodeError::Other("Not an amiga module?")),
        };

        let number_of_patterns = amiga.get_number_of_patterns();
        for _p in 0..number_of_patterns {
            let mut pattern: Vec<Vec<PatternSlot>> = vec![];
            for _row in 0..64 {
                let mut row: Vec<PatternSlot> = vec![];
                for _elt in 0..number_of_tracks {
                    let e = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                    let element = PatternSlot::deserialize(e);
                    row.push(element);
                    data = &data[4..];
                }
                pattern.push(row);
            }
            amiga.patterns.push(pattern);
        }

        // audio
        for i_spl in 0..amiga.samples.len() {
            // small hack to force COUNTRY.MOD loading
            let l = if 2 * amiga.samples[i_spl].length_div2 as usize <= data.len() {
                2 * amiga.samples[i_spl].length_div2 as usize
            } else {
                data.len()
            };
            let s = &data[0..l];
            let vec_i8: Vec<i8> = s.iter().map(|&x| x as i8).collect();
            amiga.audio.push(vec_i8);
            data = &data[l..];
        }

        Result::Ok(amiga)
    }

    fn to_instr(&self, sample_index: usize) -> Instrument {
        let mut instr: Instrument = Instrument::default();

        let mut sample: Sample = self.samples[sample_index].to_sample();
        sample.data = Some(SampleDataType::Mono8(self.audio[sample_index].clone()));

        instr.name = sample.name.clone();

        let mut idef = InstrDefault::default();
        idef.sample.push(Some(sample));

        idef.keyboard.sample_for_pitch = [Some(0); 120];

        instr.instr_type = InstrumentType::Default(idef);

        instr
    }

    pub fn to_module(&self) -> Module {
        let mut module = Module::default();

        module.name = self.title.clone();
        module.comment = "XmRs reader".to_string();
        module.profile = CompatibilityProfile::pt();
        module.frequency_type = FrequencyType::AmigaFrequencies;
        // MOD has no mix-volume byte. Schism initialises the mixer
        // chain at `mixing_volume = 48` (`csndfile.c:55`,
        // `fmt/xm.c:885`) — same default as XM. We mirror that so
        // every format feeds the player's mixer with the same
        // pre-attenuated level.
        module.mix_volume = 48.0 / 128.0;
        module.default_tempo = 6;
        module.default_bpm = 125;
        module.pattern_order = vec![self.positions[..usize::from(self.song_length)]
            .to_vec()
            .iter()
            .map(|&x| x as usize)
            .collect()];
        let mut im = ImportMemory::default();
        module.pattern = im.unpack_patterns(
            FrequencyType::AmigaFrequencies,
            MemoryType::Mod,
            &module.pattern_order,
            &self.patterns,
        );

        for i in 0..self.samples.len() {
            let instr = self.to_instr(i);
            module.instrument.push(instr);
        }

        module
    }
}
