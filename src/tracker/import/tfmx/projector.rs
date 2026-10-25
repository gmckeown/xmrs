#![forbid(unsafe_code)]

//! TFMX to XM / Module projection engine.
//!
//! Projects simulated per-tick voice state frames into FastTracker II extended modules
//! and loads them into native [`crate::core::module::Module`] with bit-exact Amiga Paula accuracy.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cmp::{max, min};

use super::header::TfmxModule;
use super::runtime::{ActiveSampleEvent, TfmxSimulation, TfmxSimulator};
use crate::core::fixed::tables::{amiga_period_from_pitch_base, AMIGA_C0_PERIOD_LEGACY};
use crate::core::fixed::units::Pitch;
use crate::core::module::Module;
use crate::tracker::import::bin_reader::ImportError;

pub const DEFAULT_FINETUNE: i8 = 0;

/// Authentic Amiga stereo placement (0=full left, 128=center, 255=full right).
pub fn voice_panning(v_idx: usize, is_7v: bool) -> u8 {
    if is_7v {
        match v_idx {
            0 => 0x30, // Paula 0 (Left)
            1 => 0xD0, // Paula 1 (Right)
            2 => 0xC0, // Paula 2 (Right)
            4 => 0x40, // Paula 3A (Left)
            5 => 0x54, // Paula 3B (Left-Center)
            6 => 0x38, // Paula 3C (Left)
            7 => 0x60, // Paula 3D (Left-Center)
            _ => 0x80,
        }
    } else {
        match v_idx {
            0 => 0x38, // Paula 0 (Left)
            1 => 0xC8, // Paula 1 (Right)
            2 => 0xC8, // Paula 2 (Right)
            3 => 0x38, // Paula 3 (Left)
            _ => 0x80,
        }
    }
}

pub fn extract_safe_slice(smpl: &[u8], start: usize, len: usize) -> Vec<u8> {
    if start >= smpl.len() {
        return vec![0u8; len.max(4)];
    }
    let end = start + len;
    if end <= smpl.len() {
        smpl[start..end].to_vec()
    } else {
        let mut v = smpl[start..].to_vec();
        v.resize(len, 0);
        v
    }
}

pub fn encode_delta8(pcm: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; pcm.len()];
    let mut prev = 0u8;
    for (i, &b) in pcm.iter().enumerate() {
        out[i] = b.wrapping_sub(prev);
        prev = b;
    }
    out
}

pub fn pack_xm_slot(note: u8, instr: u8, vol: u8, eff_type: u8, eff_param: u8) -> Vec<u8> {
    let mut mask = 0x80u8;
    let mut payload = Vec::with_capacity(5);
    if note != 0 {
        mask |= 0x01;
        payload.push(note);
    }
    if instr != 0 {
        mask |= 0x02;
        payload.push(instr);
    }
    if vol != 0 {
        mask |= 0x04;
        payload.push(vol);
    }
    if eff_type != 0 {
        mask |= 0x08;
        payload.push(eff_type);
    }
    if eff_param != 0 {
        mask |= 0x10;
        payload.push(eff_param);
    }
    let mut res = Vec::with_capacity(1 + payload.len());
    res.push(mask);
    res.extend_from_slice(&payload);
    res
}

pub fn nearest_xm_note(target_period: u16) -> (u8, u16) {
    let mut best_note = 49u8;
    let mut best_diff = i32::MAX;
    let mut best_per = 428u16;
    for note in 1..=96 {
        let pitch = Pitch::from_semitone((note - 1) as i16);
        let p = amiga_period_from_pitch_base(pitch, AMIGA_C0_PERIOD_LEGACY).raw() as u16;
        let diff = (p as i32 - target_period as i32).abs();
        if diff < best_diff {
            best_diff = diff;
            best_note = note;
            best_per = p;
        }
    }
    (best_note, best_per)
}

#[derive(Debug, Clone)]
pub struct XmInstrumentSpec {
    pub name: String,
    pub pcm: Vec<u8>,
    pub loop_start: u32,
    pub loop_len: u32,
    pub loop_type: u8,
    pub panning: u8,
}

pub fn build_instruments(
    simulation: &TfmxSimulation,
    smpl_bytes: &[u8],
) -> (Vec<XmInstrumentSpec>, BTreeMap<(usize, usize), usize>) {
    let paula_clocks_per_tick = 3546895.0 / simulation.tick_rate_hz;

    // Collect all (v_idx, event_id) pairs
    let mut seen_pairs = BTreeSet::new();
    let mut events = Vec::new();
    for f in &simulation.frames {
        for (v_idx, v) in f.voices.iter().enumerate() {
            if let Some(ev_id) = v.event_id {
                if ev_id < simulation.events.len() && seen_pairs.insert((v_idx, ev_id)) {
                    events.push((v_idx, &simulation.events[ev_id]));
                }
            }
        }
    }

    // Identify longest dynamic event for SID or sweep macros
    let mut longest_dyn: BTreeMap<(u8, u32, u32), &ActiveSampleEvent> = BTreeMap::new();
    for (_v_idx, ev) in &events {
        if ev.has_sid || ev.has_add_begin_or_len {
            let key = (ev.macro_num, ev.init_beg, ev.init_len);
            if let Some(existing) = longest_dyn.get(&key) {
                if ev.tick_slices.len() > existing.tick_slices.len() {
                    longest_dyn.insert(key, ev);
                }
            } else {
                longest_dyn.insert(key, ev);
            }
        }
    }

    let mut instruments = Vec::new();
    let mut key_to_instr_idx = BTreeMap::new();
    let mut ev_to_instr_idx = BTreeMap::new();

    for (v_idx, ev) in events {
        let pan = voice_panning(v_idx, simulation.is_7voice);

        if ev.has_sid {
            let base_key = (ev.macro_num, ev.init_beg, ev.init_len);
            let full_key = (0u8, base_key.0, base_key.1, base_key.2, pan, 0u32, false);
            if !key_to_instr_idx.contains_key(&full_key) {
                let rep_ev = longest_dyn.get(&base_key).copied().unwrap_or(ev);
                let ref_per = rep_ev.init_period.clamp(113, 856) as f64;
                let bytes_per_tick = (paula_clocks_per_tick / ref_per).round().max(32.0) as usize;

                let mut pcm = Vec::new();
                for sframe in rep_ev.sid_frames.iter().take(120) {
                    let slen = sframe.len().max(4);
                    let reps = ((bytes_per_tick as f64) / (slen as f64)).round().max(1.0) as usize;
                    for _ in 0..reps {
                        pcm.extend_from_slice(sframe);
                    }
                }
                if pcm.is_empty() {
                    pcm.resize(64, 0);
                }

                let (l_start, l_len, l_type) = if rep_ev.one_shot || pcm.len() < 128 {
                    (0, 0, 0)
                } else {
                    let start = (pcm.len() / 2) as u32;
                    let len = (pcm.len() as u32) - start;
                    (start, len, 1)
                };

                instruments.push(XmInstrumentSpec {
                    name: format!("SID_M{:02}_V{}", ev.macro_num, v_idx),
                    pcm,
                    loop_start: l_start,
                    loop_len: l_len,
                    loop_type: l_type,
                    panning: pan,
                });
                key_to_instr_idx.insert(full_key, instruments.len());
            }
            let idx = key_to_instr_idx[&full_key];
            ev_to_instr_idx.insert((v_idx, ev.id), idx);
        } else if ev.has_add_begin_or_len {
            let base_key = (ev.macro_num, ev.init_beg, ev.init_len);
            let full_key = (1u8, base_key.0, base_key.1, base_key.2, pan, 0u32, false);
            if !key_to_instr_idx.contains_key(&full_key) {
                let rep_ev = longest_dyn.get(&base_key).copied().unwrap_or(ev);
                let ref_per = rep_ev.init_period.clamp(113, 856) as f64;
                let bytes_per_tick = (paula_clocks_per_tick / ref_per).round().max(32.0) as usize;

                let mut pcm = Vec::new();
                let mut attack_len = 0;
                let slices = &rep_ev.tick_slices;
                let mut start_tick = 0;

                if rep_ev.has_wait_dma {
                    let att = extract_safe_slice(smpl_bytes, rep_ev.init_beg as usize, rep_ev.init_len as usize);
                    attack_len = att.len();
                    pcm.extend_from_slice(&att);
                    let wait_ticks = ((rep_ev.init_len as f64 * ref_per) / paula_clocks_per_tick)
                        .round()
                        .max(1.0) as usize;
                    start_tick = min(slices.len().saturating_sub(1), wait_ticks);
                }

                let end_tick = min(slices.len(), start_tick + 80);
                let sweep_slices = if start_tick < end_tick {
                    &slices[start_tick..end_tick]
                } else {
                    &[][..]
                };

                let mut tick_byte_offsets = Vec::new();
                let mut cur_bytes = attack_len;

                if sweep_slices.is_empty() {
                    let wave = extract_safe_slice(smpl_bytes, rep_ev.init_beg as usize, rep_ev.init_len as usize);
                    pcm.extend_from_slice(&wave);
                    tick_byte_offsets.push(cur_bytes);
                } else {
                    for &(b_addr, b_len, _per) in sweep_slices {
                        tick_byte_offsets.push(cur_bytes);
                        let wave = extract_safe_slice(smpl_bytes, b_addr as usize, b_len as usize);
                        let wlen = wave.len().max(4);
                        if wlen >= bytes_per_tick {
                            let chunk = &wave[..max(bytes_per_tick, wlen).min(wave.len())];
                            pcm.extend_from_slice(chunk);
                            cur_bytes += chunk.len();
                        } else {
                            let reps = ((bytes_per_tick as f64) / (wlen as f64)).round().max(1.0) as usize;
                            for _ in 0..reps {
                                pcm.extend_from_slice(&wave);
                            }
                            cur_bytes += reps * wave.len();
                        }
                    }
                }

                let (l_start, l_len, l_type) = if rep_ev.one_shot || tick_byte_offsets.is_empty() {
                    (0, 0, 0)
                } else {
                    let split_idx = min(tick_byte_offsets.len() - 1, max(1, tick_byte_offsets.len() / 3));
                    let start = tick_byte_offsets[split_idx] as u32;
                    let len = (pcm.len() as u32).saturating_sub(start);
                    (start, len, 1)
                };

                instruments.push(XmInstrumentSpec {
                    name: format!("WT_M{:02}_{:05x}", ev.macro_num, ev.init_beg),
                    pcm,
                    loop_start: l_start,
                    loop_len: l_len,
                    loop_type: l_type,
                    panning: pan,
                });
                key_to_instr_idx.insert(full_key, instruments.len());
            }
            let idx = key_to_instr_idx[&full_key];
            ev_to_instr_idx.insert((v_idx, ev.id), idx);
        } else {
            let loop_off = ev.loop_offset.unwrap_or(0);
            let full_key = (2u8, 0u8, ev.init_beg, ev.init_len, pan, loop_off, ev.one_shot);
            if !key_to_instr_idx.contains_key(&full_key) {
                let pcm = extract_safe_slice(smpl_bytes, ev.init_beg as usize, ev.init_len as usize);
                let (l_start, l_len, l_type) = if ev.one_shot {
                    (0, 0, 0)
                } else if let Some(off) = ev.loop_offset {
                    let start = min(pcm.len().saturating_sub(2), off as usize) as u32;
                    let len = max(2, pcm.len().saturating_sub(start as usize)) as u32;
                    (start, len, 1)
                } else {
                    (0, pcm.len() as u32, 1)
                };

                instruments.push(XmInstrumentSpec {
                    name: format!("PCM_M{:02}_{:05x}", ev.macro_num, ev.init_beg),
                    pcm,
                    loop_start: l_start,
                    loop_len: l_len,
                    loop_type: l_type,
                    panning: pan,
                });
                key_to_instr_idx.insert(full_key, instruments.len());
            }
            let idx = key_to_instr_idx[&full_key];
            ev_to_instr_idx.insert((v_idx, ev.id), idx);
        }
    }

    (instruments, ev_to_instr_idx)
}

pub fn compile_xm_bytes(
    title: &str,
    simulation: &TfmxSimulation,
    instruments: &[XmInstrumentSpec],
    ev_to_instr_idx: &BTreeMap<(usize, usize), usize>,
) -> Vec<u8> {
    let voice_order: &[usize] = if simulation.is_7voice {
        &[0, 1, 2, 4, 5, 6, 7]
    } else {
        &[0, 1, 2, 3]
    };
    let num_channels = voice_order.len();
    let num_frames = simulation.frames.len();

    let mut rows_per_pat = 128;
    while (num_frames + rows_per_pat - 1) / rows_per_pat > 255 {
        rows_per_pat *= 2;
    }

    let num_patterns = (num_frames + rows_per_pat - 1) / rows_per_pat;
    let restart_pos = min(num_patterns.saturating_sub(1), simulation.loop_tick / rows_per_pat) as u16;

    // Precalculate future audibility lookahead
    let mut future_audible = vec![vec![false; num_frames]; 8];
    for &v in voice_order {
        let mut aud = false;
        let mut prev_ev = None;
        for t in (0..num_frames).rev() {
            let snap = simulation.frames[t].voices[v];
            if !snap.dma_on || snap.event_id.is_none() {
                aud = false;
            } else if snap.volume > 0 {
                aud = true;
            }
            future_audible[v][t] = aud;
            if snap.triggered || (snap.event_id.is_some() && prev_ev.is_some() && snap.event_id != prev_ev) {
                aud = false;
            }
            prev_ev = snap.event_id;
        }
    }

    let mut cur_xm_per = vec![428u16; num_channels];
    let mut cur_xm_vol = vec![-1i16; num_channels];
    let mut cur_ev_id = vec![None; num_channels];
    let mut has_live_voice = vec![false; num_channels];
    let mut prev_target_per = vec![428u16; num_channels];

    let mut pattern_blobs = Vec::new();
    let mut frame_idx = 0;

    for _ in 0..num_patterns {
        let pat_rows = min(rows_per_pat, num_frames - frame_idx);
        let mut packed_cells = Vec::new();

        for _ in 0..pat_rows {
            let (fr, t_idx) = if frame_idx < num_frames {
                let idx = frame_idx;
                frame_idx += 1;
                (Some(&simulation.frames[idx]), idx)
            } else {
                (None, 0)
            };

            for ch in 0..num_channels {
                let v_idx = voice_order[ch];
                let snap = fr.map(|f| f.voices[v_idx]);
                let is_audible = if fr.is_some() { future_audible[v_idx][t_idx] } else { false };

                let mut note_byte = 0u8;
                let mut instr_byte = 0u8;
                let mut vol_byte = 0u8;
                let mut eff_type = 0u8;
                let mut eff_param = 0u8;

                if let Some(s) = snap {
                    if !s.dma_on || s.event_id.is_none() {
                        if cur_xm_vol[ch] != 0 {
                            vol_byte = 0x10;
                            cur_xm_vol[ch] = 0;
                        }
                    } else {
                        let ev_id = s.event_id.unwrap();
                        let instr_idx = ev_to_instr_idx.get(&(v_idx, ev_id)).copied().unwrap_or(1) as u8;
                        let mut need_trigger = s.triggered || (Some(ev_id) != cur_ev_id[ch]) || !has_live_voice[ch];

                        if !need_trigger
                            && (s.period as i32 - prev_target_per[ch] as i32).abs() >= 50
                            && (is_audible || cur_xm_vol[ch] > 0 || s.volume > 0)
                        {
                            need_trigger = true;
                        }
                        prev_target_per[ch] = s.period;

                        if need_trigger {
                            let (nb, init_per) = nearest_xm_note(s.period);
                            note_byte = nb;
                            instr_byte = instr_idx;
                            cur_xm_per[ch] = init_per;
                            cur_ev_id[ch] = Some(ev_id);
                            has_live_voice[ch] = true;
                            vol_byte = 0x10 + min(64, s.volume);
                            cur_xm_vol[ch] = s.volume as i16;
                        } else if (s.volume as i16) != cur_xm_vol[ch] {
                            vol_byte = 0x10 + min(64, s.volume);
                            cur_xm_vol[ch] = s.volume as i16;
                        }

                        if is_audible || cur_xm_vol[ch] > 0 || s.volume > 0 {
                            let diff = s.period as i32 - cur_xm_per[ch] as i32;
                            if diff < 0 {
                                let step_up = min(15, -diff) as u8;
                                eff_type = 0x0E;
                                eff_param = 0x10 | step_up;
                                cur_xm_per[ch] -= step_up as u16;
                            } else if diff > 0 {
                                let step_dn = min(15, diff) as u8;
                                eff_type = 0x0E;
                                eff_param = 0x20 | step_dn;
                                cur_xm_per[ch] += step_dn as u16;
                            }
                        }
                    }
                }

                packed_cells.extend_from_slice(&pack_xm_slot(note_byte, instr_byte, vol_byte, eff_type, eff_param));
            }
        }

        let mut pat_hdr = Vec::with_capacity(9);
        pat_hdr.extend_from_slice(&9u32.to_le_bytes());
        pat_hdr.push(0);
        pat_hdr.extend_from_slice(&(pat_rows as u16).to_le_bytes());
        pat_hdr.extend_from_slice(&(packed_cells.len() as u16).to_le_bytes());
        pat_hdr.extend_from_slice(&packed_cells);
        pattern_blobs.push(pat_hdr);
    }

    // Deduplicate pattern blobs into the XM Pattern Order Table
    let mut unique_patterns = Vec::new();
    let mut pattern_map = BTreeMap::new();
    let mut order_list = Vec::new();

    for blob in pattern_blobs {
        let idx = if let Some(&i) = pattern_map.get(&blob) {
            i
        } else {
            let i = unique_patterns.len();
            pattern_map.insert(blob.clone(), i);
            unique_patterns.push(blob);
            i
        };
        order_list.push(idx as u8);
    }

    let song_len = order_list.len() as u16;
    let mut order_table = order_list;
    order_table.resize(256, 0);

    // Build 336-byte XM Header
    let mut out = Vec::new();
    out.extend_from_slice(b"Extended Module: ");

    let mut title_bytes = [0u8; 20];
    let t_slice = title.as_bytes();
    let copy_len = min(20, t_slice.len());
    title_bytes[..copy_len].copy_from_slice(&t_slice[..copy_len]);
    out.extend_from_slice(&title_bytes);

    out.push(0x1a);
    out.extend_from_slice(b"TFMX2XM Antigravity "); // 20 bytes tracker name

    out.extend_from_slice(&0x0104u16.to_le_bytes()); // version
    out.extend_from_slice(&276u32.to_le_bytes());   // header_size
    out.extend_from_slice(&song_len.to_le_bytes());
    out.extend_from_slice(&restart_pos.to_le_bytes());
    out.extend_from_slice(&(num_channels as u16).to_le_bytes());
    out.extend_from_slice(&(unique_patterns.len() as u16).to_le_bytes());
    out.extend_from_slice(&(instruments.len() as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());      // flags: 0 = Amiga frequencies
    out.extend_from_slice(&1u16.to_le_bytes());      // default_tempo: 1 tick/row
    let bpm = ((simulation.tick_rate_hz * 2.5).round().clamp(32.0, 255.0)) as u16;
    out.extend_from_slice(&bpm.to_le_bytes());
    out.extend_from_slice(&order_table);

    // Append pattern data
    for pat in unique_patterns {
        out.extend_from_slice(&pat);
    }

    // Append instruments and sample payloads
    for ins in instruments {
        let mut ins_name = [0u8; 22];
        let name_bytes = ins.name.as_bytes();
        let copy_len = min(22, name_bytes.len());
        ins_name[..copy_len].copy_from_slice(&name_bytes[..copy_len]);

        out.extend_from_slice(&248u32.to_le_bytes()); // instr_hdr_len
        out.extend_from_slice(&ins_name);
        out.push(0);                                  // type
        out.extend_from_slice(&1u16.to_le_bytes());   // num_samples
        out.extend_from_slice(&40u32.to_le_bytes());  // sample_hdr_size
        out.extend_from_slice(&[0u8; 215]);           // xm_instr_default

        let delta_pcm = encode_delta8(&ins.pcm);
        out.extend_from_slice(&(delta_pcm.len() as u32).to_le_bytes());
        out.extend_from_slice(&ins.loop_start.to_le_bytes());
        out.extend_from_slice(&ins.loop_len.to_le_bytes());
        out.push(64);                                // volume
        out.push(DEFAULT_FINETUNE as u8);
        out.push(ins.loop_type & 0x03);
        out.push(ins.panning);
        out.push(0);                                 // rel_note
        out.push(0);                                 // reserved
        out.extend_from_slice(&ins_name);
        out.extend_from_slice(&delta_pcm);
    }

    out
}

impl TfmxModule {
    /// Compiles a subsong into a standard FastTracker II extended module binary byte vector.
    pub fn to_xm_bytes(&self, subsong_idx: usize) -> Result<Vec<u8>, ImportError> {
        let mut sim = TfmxSimulator::new(self, subsong_idx)
            .ok_or(ImportError::OutOfRange("subsong index"))?;
        let simulation = sim.simulate_all(40000);
        let (instruments, ev_to_instr) = build_instruments(&simulation, &self.sample_data);
        Ok(compile_xm_bytes(&self.title, &simulation, &instruments, &ev_to_instr))
    }

    /// Converts a subsong directly into an [`crate::core::module::Module`] with native Amiga quirks.
    pub fn to_module(&self, subsong_idx: usize) -> Result<Module, ImportError> {
        let xm_bytes = self.to_xm_bytes(subsong_idx)?;
        let mut module = Module::load_xm(&xm_bytes)?;
        module.quirks.legacy_amiga_period_base = true;
        module.quirks.pitch_slide_ticks_at_row_zero = true;
        Ok(module)
    }
}
