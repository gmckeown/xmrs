#![forbid(unsafe_code)]

//! TFMX sound macro interpreter, pattern sequencer, and per-frame state machine.

use alloc::vec::Vec;
use core::cmp::Ordering;

use super::header::TfmxModule;
use super::tables::NOTEVALS;

#[inline]
fn s8(v: u8) -> i8 {
    v as i8
}

#[inline]
fn s16(v: u16) -> i16 {
    v as i16
}

/// Advances a TFMX loop counter (`None` = inactive, `Some(rem)` = remaining iterations).
/// Returns `true` if the loop branch should be taken.
#[inline]
fn step_loop(slot: &mut Option<u16>, count: u16) -> bool {
    match (count, *slot) {
        (0, _) => true,
        (c, None) => {
            *slot = Some(c - 1);
            true
        }
        (_, Some(n)) if n > 0 => {
            *slot = Some(n - 1);
            true
        }
        (_, Some(_)) => {
            *slot = None;
            false
        }
    }
}

/// An active DMA sample playback event, tracking its dynamic modulation evolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveSampleEvent {
    pub id: usize,
    pub macro_num: u8,
    pub init_beg: u32,
    pub init_len: u32,
    pub init_period: u16,
    pub loop_offset: Option<u32>,
    pub one_shot: bool,
    pub has_sid: bool,
    pub has_add_begin_or_len: bool,
    pub has_wait_dma: bool,
    pub sid_frames: Vec<Vec<u8>>,
    pub tick_slices: Vec<(u32, u32, u16)>,
}

/// Snapshot of an individual voice at a given tick frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceTickSnapshot {
    pub triggered: bool,
    pub event_id: Option<usize>,
    pub macro_num: u8,
    pub period: u16,
    pub volume: u8,
    pub dma_on: bool,
    pub beg_addr: u32,
    pub len_bytes: u32,
}

/// Pattern sequencer track state (up to 8 tracks).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TrackState {
    pub enabled: bool,
    pub pat_num: u8,
    pub transpose: i8,
    pub step: usize,
    pub wait: u8,
    pub loop_cnt: Option<u16>,
    pub saved_pat: u8,
    pub saved_step: usize,
}

/// Sound macro interpreter voice state.
#[derive(Debug, Clone)]
pub struct VoiceState {
    pub idx: usize,
    pub macro_running: bool,
    pub macro_num: u8,
    pub macro_step: usize,
    pub macro_wait: usize,
    pub macro_loop: Option<u16>,
    pub saved_macro: u8,
    pub saved_step: usize,
    pub new_style_macro: bool,
    pub key_up: bool,
    pub curr_note: u8,
    pub prev_note: u8,
    pub rel_vol: u8,
    pub finetune: i8,
    pub cur_vol: u8,
    pub env_rate: u8,
    pub env_time: u8,
    pub env_reset: u8,
    pub env_end_vol: u8,
    pub vib_reset: u8,
    pub vib_time: u8,
    pub vib_width: i8,
    pub vib_offset: i16,
    pub porta_reset: u8,
    pub porta_time: u8,
    pub porta_rate: u16,
    pub porta_period: u16,
    pub base_period: u16,
    pub dest_period: u16,
    pub curr_period: u16,
    pub dma_on: bool,
    pub beg_addr: u32,
    pub cur_len_words: u16,
    pub add_begin_time: u8,
    pub add_begin_reset: u8,
    pub add_begin_delta: i16,
    pub triggered_this_tick: bool,
    pub active_event_id: Option<usize>,
    pub sid_mode: bool,
    pub sid_src: u32,
    pub sid_src_mask: u16,
    pub sid_dst_len: u8,
    pub sid_op1: u32,
    pub sid_op1_pos: u32,
    pub sid_op2_reset: u8,
    pub sid_op2_cnt: i16,
    pub sid_op2_delta: i16,
    pub sid_op3: u32,
    pub sid_op3_pos: u32,
    pub sid_op4_reset: u8,
    pub sid_op4_cnt: i16,
    pub sid_op4_delta: i16,
    pub sid_last_val: i8,
}

impl VoiceState {
    pub fn new(idx: usize) -> Self {
        Self {
            idx,
            macro_running: false,
            macro_num: 0,
            macro_step: 0,
            macro_wait: 0,
            macro_loop: None,
            saved_macro: 0,
            saved_step: 0,
            new_style_macro: false,
            key_up: true,
            curr_note: 0,
            prev_note: 0,
            rel_vol: 0,
            finetune: 0,
            cur_vol: 0,
            env_rate: 0,
            env_time: 0,
            env_reset: 0,
            env_end_vol: 0,
            vib_reset: 0,
            vib_time: 0,
            vib_width: 0,
            vib_offset: 0,
            porta_reset: 0,
            porta_time: 0,
            porta_rate: 0,
            porta_period: 428,
            base_period: 428,
            dest_period: 428,
            curr_period: 428,
            dma_on: false,
            beg_addr: 0,
            cur_len_words: 0,
            add_begin_time: 0,
            add_begin_reset: 0,
            add_begin_delta: 0,
            triggered_this_tick: false,
            active_event_id: None,
            sid_mode: false,
            sid_src: 0,
            sid_src_mask: 0,
            sid_dst_len: 0x40,
            sid_op1: 0,
            sid_op1_pos: 0,
            sid_op2_reset: 0,
            sid_op2_cnt: 0,
            sid_op2_delta: 0,
            sid_op3: 0,
            sid_op3_pos: 0,
            sid_op4_reset: 0,
            sid_op4_cnt: 0,
            sid_op4_delta: 0,
            sid_last_val: 0,
        }
    }
}

/// Simulated tick frame with state across all 8 potential virtual voices.
#[derive(Debug, Clone)]
pub struct TfmxTickFrame {
    pub tick: usize,
    pub voices: [VoiceTickSnapshot; 8],
}

/// Complete simulation result for a subsong.
#[derive(Debug, Clone)]
pub struct TfmxSimulation {
    pub subsong_index: usize,
    pub is_7voice: bool,
    pub tick_rate_hz: f64,
    pub loop_tick: usize,
    pub frames: Vec<TfmxTickFrame>,
    pub events: Vec<ActiveSampleEvent>,
}

/// State simulator for TFMX subsongs.
pub struct TfmxSimulator<'a> {
    module: &'a TfmxModule,
    subsong_idx: usize,
    tracks: [TrackState; 8],
    voices: [VoiceState; 8],
    pub events: Vec<ActiveSampleEvent>,
    pub step_first_tick: alloc::collections::BTreeMap<u16, usize>,
    curr_pos: u16,
    last_pos: u16,
    prescale: u16,
    ts_loop_count: Option<u16>,
    song_ended: bool,
    loop_target_pos: Option<u16>,
    is_7v: bool,
    speed_finetune: i8,
    master_vol: u8,
    fade_target: u8,
    fade_slope: u8,
    fade_timer: u8,
    fade_dir: i8,
    pstep_timer: u16,
    silent_ticks: usize,
}

impl<'a> TfmxSimulator<'a> {
    pub fn new(module: &'a TfmxModule, subsong_idx: usize) -> Option<Self> {
        let subsong = module.subsongs.iter().find(|s| s.index == subsong_idx)?;
        let voices = core::array::from_fn(VoiceState::new);

        let mut sim = Self {
            module,
            subsong_idx,
            tracks: [TrackState::default(); 8],
            voices,
            events: Vec::new(),
            step_first_tick: alloc::collections::BTreeMap::new(),
            curr_pos: subsong.start_pos,
            last_pos: subsong.end_pos,
            prescale: subsong.tempo,
            ts_loop_count: None,
            song_ended: false,
            loop_target_pos: None,
            is_7v: module.is_7voice,
            speed_finetune: 0,
            master_vol: 0x40,
            fade_target: 0x40,
            fade_slope: 0,
            fade_timer: 0,
            fade_dir: 0,
            pstep_timer: 0,
            silent_ticks: 0,
        };
        sim.load_trackstep();
        Some(sim)
    }

    #[inline]
    fn init_fade(&mut self, slope: u8, target: u8) {
        let targ = target.min(0x40);
        self.fade_target = targ;
        if slope == 0 {
            self.master_vol = targ;
            self.fade_dir = 0;
        } else {
            self.fade_slope = slope;
            self.fade_timer = slope;
            self.fade_dir = match targ.cmp(&self.master_vol) {
                Ordering::Greater => 1,
                Ordering::Less => -1,
                Ordering::Equal => 0,
            };
        }
    }

    #[inline]
    pub fn tick_rate_hz(&self) -> f64 {
        if self.is_7v {
            // In 7-voice TFMX, Paula Channel 3 streams the software-mixed audio buffer.
            // AUD3PER is fixed at 179. The buffer length in 16-bit words is:
            // len_words = 200 + 2 * speed_finetune (clamped >= -32).
            // Each word is 2 bytes/samples, each sample lasts 179 Paula PAL color clocks.
            // cycles_per_tick = len_words * 2 * 179 = len_words * 358.
            let x = (self.speed_finetune as i32).max(-32);
            let len_words = 200 + 2 * x;
            3546895.0 / (len_words as f64 * 358.0)
        } else {
            let e_clocks = (14318 + ((self.speed_finetune as i32) << 7)).max(4096);
            50.0 * 14318.0 / (e_clocks as f64)
        }
    }

    fn start_dma_event(&mut self, v_idx: usize) {
        let v = &mut self.voices[v_idx];
        let ev_id = self.events.len();
        let ev = ActiveSampleEvent {
            id: ev_id,
            macro_num: v.macro_num,
            init_beg: v.beg_addr,
            init_len: ((v.cur_len_words as u32) * 2).max(4),
            init_period: v.curr_period,
            loop_offset: None,
            one_shot: false,
            has_sid: v.sid_mode,
            has_add_begin_or_len: false,
            has_wait_dma: false,
            sid_frames: Vec::new(),
            tick_slices: Vec::new(),
        };
        self.events.push(ev);
        v.active_event_id = Some(ev_id);
        v.triggered_this_tick = true;
    }

    fn step_sid(&mut self, voice_idx: usize) -> Vec<u8> {
        let v = &mut self.voices[voice_idx];
        v.sid_op1 = v.sid_op1.wrapping_add(((v.sid_op2_delta as i32) << 4) as u32);
        if v.sid_op2_reset > 0 {
            v.sid_op2_cnt -= 1;
            if v.sid_op2_cnt <= 0 {
                v.sid_op2_cnt = v.sid_op2_reset as i16;
                v.sid_op2_delta = -v.sid_op2_delta;
            }
        }
        v.sid_op3 = v.sid_op3.wrapping_add(((v.sid_op4_delta as i32) << 4) as u32);
        if v.sid_op4_reset > 0 {
            v.sid_op4_cnt -= 1;
            if v.sid_op4_cnt <= 0 {
                v.sid_op4_cnt = v.sid_op4_reset as i16;
                v.sid_op4_delta = -v.sid_op4_delta;
            }
        }

        let length = (v.sid_dst_len as usize).clamp(4, 256);
        let mut val = v.sid_last_val;
        let mut buf = alloc::vec![0u8; length];
        let smpl_len = self.module.sample_data.len();

        if smpl_len > 0 {
            for slot in &mut buf {
                v.sid_op1_pos = v.sid_op1_pos.wrapping_add(v.sid_op1);
                let idx = ((v.sid_op1_pos >> 16) as u16 & v.sid_src_mask) as usize;
                let src_pos = ((v.sid_src as usize) + idx) % smpl_len;
                let sample_b = self.module.sample_data[src_pos] as i8;

                v.sid_op3_pos = v.sid_op3_pos.wrapping_add(v.sid_op3);
                let delta = ((v.sid_op3_pos >> 16) & 0xFF) as u8;
                match sample_b.cmp(&val) {
                    Ordering::Greater => {
                        val = if delta < 128 {
                            (val as i16 + delta as i8 as i16).min(127) as i8
                        } else {
                            sample_b
                        };
                    }
                    Ordering::Less => {
                        val = if delta < 128 {
                            (val as i16 - delta as i8 as i16).max(-128) as i8
                        } else {
                            sample_b
                        };
                    }
                    Ordering::Equal => {}
                }
                *slot = val as u8;
            }
        }
        v.sid_last_val = val;
        buf
    }

    fn load_trackstep(&mut self) {
        while !self.song_ended {
            if self.curr_pos > self.last_pos && self.loop_target_pos.is_none() {
                self.song_ended = true;
                return;
            }
            let step_idx = self.curr_pos as usize;
            if step_idx >= self.module.tracksteps.len() {
                self.song_ended = true;
                return;
            }
            let step = self.module.tracksteps[step_idx];
            if step.is_command() {
                let cmd = step.command_type();
                match cmd {
                    0 => {
                        self.song_ended = true;
                        return;
                    }
                    1 => {
                        let dest = step.words[2];
                        let cnt = step.words[3];
                        if cnt >= 0x8000 || cnt == 0 {
                            self.loop_target_pos = Some(dest);
                            self.song_ended = true;
                            return;
                        } else if step_loop(&mut self.ts_loop_count, cnt) {
                            self.curr_pos = dest;
                        } else {
                            self.curr_pos += 1;
                        }
                    }
                    2 => {
                        self.prescale = step.words[2] & 0xFF;
                        self.curr_pos += 1;
                    }
                    3 => {
                        self.is_7v = true;
                        self.speed_finetune = s8((step.words[3] & 0xFF) as u8);
                        self.curr_pos += 1;
                    }
                    4 => {
                        let slope = (step.words[2] & 0xFF) as u8;
                        let targ = (step.words[3] & 0xFF) as u8;
                        self.init_fade(slope, targ);
                        self.curr_pos += 1;
                    }
                    _ => {
                        self.curr_pos += 1;
                    }
                }
            } else {
                for t in 0..8 {
                    let w = step.words[t];
                    let hi = (w >> 8) as u8;
                    let lo = (w & 0xFF) as u8;
                    match hi {
                        0x00..=0x7F => {
                            self.tracks[t].enabled = true;
                            self.tracks[t].pat_num = hi;
                            self.tracks[t].transpose = s8(lo);
                            self.tracks[t].step = 0;
                            self.tracks[t].wait = 0;
                            self.tracks[t].loop_cnt = None;
                        }
                        0x80 => {
                            // Keep current track pattern playing
                        }
                        0xFE => {
                            self.tracks[t].enabled = false;
                            let v = (lo & 0x07) as usize;
                            self.voices[v].macro_running = false;
                            self.voices[v].dma_on = false;
                            self.voices[v].cur_vol = 0;
                            self.voices[v].active_event_id = None;
                        }
                        _ => {
                            self.tracks[t].enabled = false;
                        }
                    }
                }
                return;
            }
        }
    }

    /// Advance sequencer by exactly one tick and produce snapshot.
    pub fn step_tick(&mut self, tick_idx: usize) -> Option<TfmxTickFrame> {
        if self.song_ended {
            return None;
        }

        self.step_first_tick.entry(self.curr_pos).or_insert(tick_idx);

        for v in &mut self.voices {
            v.triggered_this_tick = false;
        }

        // 1. Sequencer / Pattern step
        if self.pstep_timer == 0 {
            self.pstep_timer = self.prescale;
            loop {
                let mut advanced_ts = false;
                for t in 0..8 {
                    if !self.tracks[t].enabled {
                        continue;
                    }
                    if self.tracks[t].wait > 0 {
                        self.tracks[t].wait -= 1;
                        continue;
                    }

                    let mut guard = 0;
                    while guard < 128 {
                        guard += 1;
                        let pat_num = self.tracks[t].pat_num as usize;
                        let p_step = self.tracks[t].step;
                        let [b0, b1, b2, b3] = match self.module.read_pattern_step(pat_num, p_step) {
                            Some(bytes) => bytes,
                            None => break,
                        };
                        self.tracks[t].step += 1;

                        match b0 {
                            0x00..=0xBF => {
                                let note = ((b0 as i16 + self.tracks[t].transpose as i16) & 0x3F) as u8;
                                let mac = b1;
                                let vol = (b2 >> 4) & 0x0F;
                                let v_idx = (b2 & 0x07) as usize;
                                let v = &mut self.voices[v_idx];
                                v.prev_note = v.curr_note;
                                v.curr_note = note;
                                v.rel_vol = vol;
                                v.finetune = if b0 < 0x80 { s8(b3) } else { 0 };
                                v.key_up = false;
                                v.macro_num = mac;
                                v.macro_step = 0;
                                v.macro_wait = 0;
                                v.macro_loop = None;
                                v.macro_running = true;
                                v.new_style_macro = false;
                                if b0 >= 0x80 {
                                    self.tracks[t].wait = b3;
                                    break;
                                }
                            }
                            0xC0..=0xEF => {
                                let note = ((b0 as i16 + self.tracks[t].transpose as i16) & 0x3F) as u8;
                                let v_idx = (b2 & 0x07) as usize;
                                let v = &mut self.voices[v_idx];
                                v.porta_reset = b1;
                                v.porta_time = 1;
                                if v.porta_rate == 0 {
                                    v.porta_period = v.base_period;
                                }
                                v.porta_rate = b3 as u16;
                                v.dest_period = NOTEVALS[(note & 0x3F) as usize];
                            }
                            0xF0 => {
                                self.curr_pos += 1;
                                if self.curr_pos > self.last_pos && self.loop_target_pos.is_none() {
                                    self.song_ended = true;
                                } else {
                                    self.load_trackstep();
                                }
                                advanced_ts = true;
                                break;
                            }
                            0xF1 => {
                                let dest = ((b2 as usize) << 8) | (b3 as usize);
                                if step_loop(&mut self.tracks[t].loop_cnt, b1 as u16) {
                                    self.tracks[t].step = dest;
                                }
                            }
                            0xF2 => {
                                self.tracks[t].pat_num = b1;
                                self.tracks[t].step = ((b2 as usize) << 8) | (b3 as usize);
                            }
                            0xF3 => {
                                self.tracks[t].wait = b1;
                                break;
                            }
                            0xF4 => {
                                self.tracks[t].enabled = false;
                                break;
                            }
                            0xF5 => {
                                let v_idx = (b2 & 0x07) as usize;
                                self.voices[v_idx].key_up = true;
                            }
                            0xF6 => {
                                let v_idx = (b2 & 0x07) as usize;
                                let v = &mut self.voices[v_idx];
                                v.vib_reset = b1 & 0xFE;
                                v.vib_time = v.vib_reset >> 1;
                                v.vib_width = s8(b3);
                                v.vib_offset = 0;
                            }
                            0xF7 => {
                                let v_idx = (b2 & 0x07) as usize;
                                let v = &mut self.voices[v_idx];
                                v.env_rate = b1;
                                v.env_reset = ((b2 >> 4) & 0x0F) + 1;
                                v.env_time = v.env_reset;
                                v.env_end_vol = b3.min(0x40);
                            }
                            0xF8 => {
                                self.tracks[t].saved_pat = self.tracks[t].pat_num;
                                self.tracks[t].saved_step = self.tracks[t].step;
                                self.tracks[t].pat_num = b1;
                                self.tracks[t].step = ((b2 as usize) << 8) | (b3 as usize);
                            }
                            0xF9 => {
                                self.tracks[t].pat_num = self.tracks[t].saved_pat;
                                self.tracks[t].step = self.tracks[t].saved_step;
                            }
                            0xFA => {
                                self.init_fade(b1, b3);
                            }
                            0xFB => {
                                let target_t = (b2 & 0x07) as usize;
                                self.tracks[target_t].enabled = true;
                                self.tracks[target_t].pat_num = b1;
                                self.tracks[target_t].transpose = s8(b3);
                                self.tracks[target_t].step = 0;
                                self.tracks[target_t].wait = 0;
                                self.tracks[target_t].loop_cnt = None;
                            }
                            _ => {}
                        }
                    }
                    if advanced_ts || self.song_ended {
                        break;
                    }
                }
                if !advanced_ts || self.song_ended {
                    break;
                }
            }
        } else {
            self.pstep_timer -= 1;
        }

        if self.song_ended {
            return None;
        }

        let paula_clocks_per_tick = 3546895.0 / self.tick_rate_hz();

        // 2. Run sound macros & per-tick modulations
        for v_idx in 0..8 {
            if self.voices[v_idx].macro_running {
                if self.voices[v_idx].macro_wait > 0 {
                    self.voices[v_idx].macro_wait -= 1;
                } else {
                    let mut mguard = 0;
                    while self.voices[v_idx].macro_running && mguard < 64 {
                        mguard += 1;
                        let m_idx = self.voices[v_idx].macro_num as usize;
                        let [b0, b1, b2, b3] = match self.module.read_macro_step(m_idx, self.voices[v_idx].macro_step) {
                            Some(bytes) => bytes,
                            None => {
                                self.voices[v_idx].macro_running = false;
                                break;
                            }
                        };
                        let op = b0 & 0x3F;
                        self.voices[v_idx].macro_step += 1;

                        match op {
                            0x00 => {
                                // DMAoff + Reset
                                let v = &mut self.voices[v_idx];
                                v.dma_on = false;
                                v.new_style_macro = true;
                                v.env_reset = 0;
                                v.env_time = 0;
                                v.env_rate = 0;
                                v.vib_reset = 0;
                                v.vib_time = 0;
                                v.vib_offset = 0;
                                v.porta_rate = 0;
                                v.porta_reset = 0;
                                v.porta_time = 0;
                                v.add_begin_time = 0;
                                v.add_begin_reset = 0;
                                v.add_begin_delta = 0;
                                if b2 != 0 || b3 != 0 {
                                    v.cur_vol = b3.min(0x40);
                                }
                            }
                            0x01 => {
                                // DMAon
                                self.voices[v_idx].dma_on = true;
                                self.voices[v_idx].new_style_macro = true;
                                self.start_dma_event(v_idx);
                            }
                            0x02 => {
                                // SetBegin
                                let has_add = self.voices[v_idx]
                                    .active_event_id
                                    .map(|id| self.events[id].has_add_begin_or_len)
                                    .unwrap_or(false);
                                let v = &mut self.voices[v_idx];
                                v.add_begin_time = 0;
                                v.add_begin_reset = 0;
                                v.beg_addr = ((b1 as u32) << 16) | ((b2 as u32) << 8) | (b3 as u32);
                                if v.dma_on && !v.triggered_this_tick {
                                    if v.active_event_id.is_none() || !has_add {
                                        self.start_dma_event(v_idx);
                                    }
                                } else if v.triggered_this_tick {
                                    if let Some(id) = v.active_event_id {
                                        self.events[id].init_beg = v.beg_addr;
                                    }
                                }
                            }
                            0x03 => {
                                // SetLen
                                let v = &mut self.voices[v_idx];
                                v.cur_len_words = ((b2 as u16) << 8) | (b3 as u16);
                                if v.dma_on && v.triggered_this_tick {
                                    if let Some(id) = v.active_event_id {
                                        self.events[id].init_len = ((v.cur_len_words as u32) * 2).max(4);
                                    }
                                }
                            }
                            0x04 => {
                                // Wait
                                let v = &mut self.voices[v_idx];
                                if (b1 & 0x01) != 0 {
                                    match v.macro_loop {
                                        None => v.macro_loop = Some((b3 as u16).saturating_sub(1)),
                                        Some(n) if n > 0 => v.macro_loop = Some(n - 1),
                                        Some(_) => {
                                            v.macro_loop = None;
                                            continue;
                                        }
                                    }
                                }
                                v.macro_wait = ((b2 as usize) << 8) | (b3 as usize);
                                break;
                            }
                            0x05 => {
                                // Loop
                                let v = &mut self.voices[v_idx];
                                let dest = ((b2 as usize) << 8) | (b3 as usize);
                                if step_loop(&mut v.macro_loop, b1 as u16) {
                                    v.macro_step = dest;
                                }
                            }
                            0x06 => {
                                // Cont
                                let v = &mut self.voices[v_idx];
                                v.macro_num = b1;
                                v.macro_step = ((b2 as usize) << 8) | (b3 as usize);
                                v.macro_loop = None;
                            }
                            0x07 => {
                                // Stop
                                self.voices[v_idx].macro_running = false;
                                break;
                            }
                            0x08 | 0x09 | 0x1F => {
                                // AddNote, SetNote, LastAddNote
                                let v = &mut self.voices[v_idx];
                                let n = match op {
                                    0x08 => (v.curr_note as i16 + s8(b1) as i16) & 0x3F,
                                    0x09 => (b1 & 0x3F) as i16,
                                    _ => (v.prev_note as i16 + s8(b1) as i16) & 0x3F,
                                } as usize;
                                let mut p = NOTEVALS[n];
                                let ft = s16(((b2 as u16) << 8) | (b3 as u16))
                                    + (v.finetune as i16);
                                if ft != 0 {
                                    let delta = ((p as i32 * ft as i32) >> 8) as i16;
                                    p = ((p as i16 + delta).max(64)) as u16;
                                }
                                v.dest_period = p;
                                if v.porta_rate == 0 {
                                    v.base_period = p;
                                }
                                v.curr_period = v.base_period;
                                if !v.new_style_macro {
                                    v.dma_on = true;
                                    v.new_style_macro = true;
                                    self.start_dma_event(v_idx);
                                    break;
                                } else if v.triggered_this_tick {
                                    if let Some(id) = v.active_event_id {
                                        self.events[id].init_period = v.curr_period;
                                    }
                                }
                            }
                            0x0A => {
                                // Reset
                                let v = &mut self.voices[v_idx];
                                v.env_reset = 0;
                                v.env_time = 0;
                                v.env_rate = 0;
                                v.vib_reset = 0;
                                v.vib_time = 0;
                                v.vib_offset = 0;
                                v.porta_rate = 0;
                                v.porta_reset = 0;
                                v.porta_time = 0;
                                v.add_begin_time = 0;
                                v.add_begin_reset = 0;
                                v.add_begin_delta = 0;
                            }
                            0x0B => {
                                // Portamento
                                let v = &mut self.voices[v_idx];
                                v.porta_reset = b1;
                                v.porta_time = 1;
                                if v.porta_rate == 0 {
                                    v.porta_period = v.base_period;
                                }
                                v.porta_rate = ((b2 as u16) << 8) | (b3 as u16);
                                if v.porta_rate == 0 {
                                    v.base_period = v.dest_period;
                                }
                            }
                            0x0C => {
                                // Vibrato
                                let v = &mut self.voices[v_idx];
                                v.vib_reset = b1;
                                v.vib_time = v.vib_reset >> 1;
                                v.vib_width = s8(b3);
                                v.vib_offset = 0;
                                if v.vib_reset == 0 {
                                    v.curr_period = v.base_period;
                                }
                            }
                            0x0D | 0x0E => {
                                // AddVolume / SetVolume
                                let v = &mut self.voices[v_idx];
                                if b2 == 0xFE {
                                    let n = ((v.curr_note as i16 + s8(b1) as i16) & 0x3F)
                                        as usize;
                                    let mut p = NOTEVALS[n];
                                    if v.finetune != 0 {
                                        let delta =
                                            ((p as i32 * v.finetune as i32) >> 8) as i16;
                                        p = ((p as i16 + delta).max(64)) as u16;
                                    }
                                    v.dest_period = p;
                                    if v.porta_rate == 0 {
                                        v.base_period = p;
                                    }
                                }
                                if op == 0x0D {
                                    let vol = (v.rel_vol as i16 * 3 + s8(b3) as i16)
                                        .clamp(0, 0x40) as u8;
                                    v.cur_vol = vol;
                                } else {
                                    v.cur_vol = b3.min(0x40);
                                }
                            }
                            0x0F => {
                                // Envelope
                                let v = &mut self.voices[v_idx];
                                v.env_rate = b1;
                                v.env_reset = b2;
                                v.env_time = b2;
                                v.env_end_vol = b3.min(0x40);
                            }
                            0x10 => {
                                // LoopKeyUp
                                let v = &mut self.voices[v_idx];
                                if !v.key_up {
                                    let dest = ((b2 as usize) << 8) | (b3 as usize);
                                    if step_loop(&mut v.macro_loop, b1 as u16) {
                                        v.macro_step = dest;
                                    }
                                }
                            }
                            0x11 => {
                                // AddBegin
                                let v = &mut self.voices[v_idx];
                                v.add_begin_reset = b1;
                                v.add_begin_time = b1;
                                let delta = s16(((b2 as u16) << 8) | (b3 as u16));
                                v.add_begin_delta = delta;
                                v.beg_addr = ((v.beg_addr as i32 + delta as i32) & 0xFFFFFF) as u32;
                                if let Some(id) = v.active_event_id {
                                    self.events[id].has_add_begin_or_len = true;
                                }
                            }
                            0x12 => {
                                // AddLen
                                let v = &mut self.voices[v_idx];
                                let delta = s16(((b2 as u16) << 8) | (b3 as u16));
                                v.cur_len_words = ((v.cur_len_words as i32 + delta as i32).max(1))
                                    as u16;
                                if let Some(id) = v.active_event_id {
                                    self.events[id].has_add_begin_or_len = true;
                                }
                            }
                            0x13 => {
                                // DMAoff without reset
                                self.voices[v_idx].dma_on = false;
                            }
                            0x14 => {
                                // WaitKeyUp
                                let v = &mut self.voices[v_idx];
                                if !v.key_up {
                                    let w = ((b2 as u16) << 8) | (b3 as u16);
                                    if step_loop(&mut v.macro_loop, w) {
                                        v.macro_step -= 1;
                                        break;
                                    }
                                }
                            }
                            0x15 => {
                                // GoSub
                                let v = &mut self.voices[v_idx];
                                v.saved_macro = v.macro_num;
                                v.saved_step = v.macro_step;
                                v.macro_num = b1;
                                v.macro_step = ((b2 as usize) << 8) | (b3 as usize);
                            }
                            0x16 => {
                                // Return
                                let v = &mut self.voices[v_idx];
                                v.macro_num = v.saved_macro;
                                v.macro_step = v.saved_step;
                            }
                            0x17 => {
                                // SetPeriod
                                let v = &mut self.voices[v_idx];
                                let p = (((b2 as u16) << 8) | (b3 as u16)).max(64);
                                v.dest_period = p;
                                if v.porta_rate == 0 {
                                    v.base_period = p;
                                }
                                v.curr_period = v.base_period;
                                if !v.new_style_macro {
                                    v.dma_on = true;
                                    v.new_style_macro = true;
                                    self.start_dma_event(v_idx);
                                    break;
                                } else if v.triggered_this_tick {
                                    if let Some(id) = v.active_event_id {
                                        self.events[id].init_period = v.curr_period;
                                    }
                                }
                            }
                            0x18 => {
                                // SampleLoop
                                let offset = (((b1 as u32) << 16)
                                    | ((b2 as u32) << 8)
                                    | (b3 as u32))
                                    & 0xFFFFFE;
                                let v = &mut self.voices[v_idx];
                                if let Some(id) = v.active_event_id {
                                    if self.events[id].loop_offset.is_none() {
                                        self.events[id].loop_offset = Some(offset);
                                    }
                                }
                                v.beg_addr = ((v.beg_addr as i32 + offset as i32) & 0xFFFFFF) as u32;
                                v.cur_len_words = v.cur_len_words.saturating_sub((offset >> 1) as u16).max(1);
                            }
                            0x19 => {
                                // SetOneShot
                                let v = &mut self.voices[v_idx];
                                if let Some(id) = v.active_event_id {
                                    self.events[id].one_shot = true;
                                }
                                v.beg_addr = 0;
                                v.cur_len_words = 1;
                            }
                            0x1A => {
                                // WaitOnDMA
                                let cycles = (((b2 as u32) << 8) | (b3 as u32)) + 1;
                                let v = &mut self.voices[v_idx];
                                let wait_bytes = if let Some(id) = v.active_event_id {
                                    self.events[id].has_wait_dma = true;
                                    (self.events[id].init_len * cycles) as f64
                                } else {
                                    (v.cur_len_words as f64 * 2.0 * cycles as f64).max(4.0)
                                };
                                let ticks = (((wait_bytes * v.curr_period as f64)
                                    / paula_clocks_per_tick)
                                    + 0.5) as usize;
                                v.macro_wait = ticks.max(1).saturating_sub(1);
                                break;
                            }
                            0x1C => {
                                // Splitkey: if note > b1, jump to macro step (b2 << 8) | b3
                                let v = &mut self.voices[v_idx];
                                if v.curr_note > b1 {
                                    v.macro_step = ((b2 as usize) << 8) | (b3 as usize);
                                }
                            }
                            0x22 => {
                                let v = &mut self.voices[v_idx];
                                v.sid_src = ((b1 as u32) << 16) | ((b2 as u32) << 8) | (b3 as u32);
                                v.sid_op1_pos = 0;
                            }
                            0x23 => {
                                let v = &mut self.voices[v_idx];
                                v.sid_dst_len = if b1 > 0 { b1 } else { 0x40 };
                                v.sid_src_mask = ((b2 as u16) << 8) | (b3 as u16);
                            }
                            0x24 => {
                                self.voices[v_idx].sid_op1 = ((b1 as u32) << 16) | ((b2 as u32) << 8) | (b3 as u32);
                            }
                            0x25 => {
                                let v = &mut self.voices[v_idx];
                                v.sid_op2_reset = b1;
                                v.sid_op2_cnt = b1 as i16;
                                v.sid_op2_delta = s16(((b2 as u16) << 8) | (b3 as u16));
                            }
                            0x26 => {
                                self.voices[v_idx].sid_op3 = ((b1 as u32) << 16) | ((b2 as u32) << 8) | (b3 as u32);
                            }
                            0x27 => {
                                let v = &mut self.voices[v_idx];
                                v.sid_op4_reset = b1;
                                v.sid_op4_cnt = b1 as i16;
                                v.sid_op4_delta = s16(((b2 as u16) << 8) | (b3 as u16));
                            }
                            0x28 => {}
                            0x29 => {
                                let v = &mut self.voices[v_idx];
                                v.sid_mode = b1 != 0;
                                if v.sid_mode {
                                    if let Some(id) = v.active_event_id {
                                        self.events[id].has_sid = true;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }

            // Per-tick envelope update
            let v = &mut self.voices[v_idx];
            if v.env_reset > 0 && !v.triggered_this_tick {
                if v.env_time > 0 {
                    v.env_time -= 1;
                } else {
                    v.env_time = v.env_reset;
                    match v.cur_vol.cmp(&v.env_end_vol) {
                        Ordering::Less => v.cur_vol = (v.cur_vol + v.env_rate).min(v.env_end_vol),
                        Ordering::Greater => v.cur_vol = v.cur_vol.saturating_sub(v.env_rate).max(v.env_end_vol),
                        Ordering::Equal => {}
                    }
                    if v.cur_vol == v.env_end_vol {
                        v.env_reset = 0;
                    }
                }
            }

            // Per-tick portamento update
            if v.porta_rate != 0 {
                v.porta_time = v.porta_time.saturating_sub(1);
                if v.porta_time == 0 {
                    v.porta_time = v.porta_reset.max(1);
                    let delta = ((v.porta_period as u32 * v.porta_rate as u32) >> 8).max(1) as u16;
                    match v.porta_period.cmp(&v.dest_period) {
                        Ordering::Greater => v.porta_period = v.porta_period.saturating_sub(delta).max(v.dest_period),
                        Ordering::Less => v.porta_period = (v.porta_period + delta).min(v.dest_period),
                        Ordering::Equal => {}
                    }
                    if v.porta_period == v.dest_period {
                        v.porta_rate = 0;
                    }
                    v.base_period = v.porta_period;
                }
            }

            // Per-tick vibrato update
            if v.vib_reset > 0 {
                v.vib_offset += v.vib_width as i16;
                v.vib_time = v.vib_time.saturating_sub(1);
                if v.vib_time == 0 {
                    v.vib_time = v.vib_reset;
                    v.vib_width = -v.vib_width;
                }
                let delta = ((v.base_period as i32 * v.vib_offset as i32) >> 11) as i16;
                v.curr_period = ((v.base_period as i16 + delta).max(64)) as u16;
            } else {
                v.curr_period = v.base_period.max(64);
            }

            // Per-tick add-begin update
            if v.add_begin_reset > 0 && !v.triggered_this_tick {
                v.beg_addr = ((v.beg_addr as i32 + v.add_begin_delta as i32) & 0xFFFFFF) as u32;
                v.add_begin_time = v.add_begin_time.saturating_sub(1);
                if v.add_begin_time == 0 {
                    v.add_begin_time = v.add_begin_reset;
                    v.add_begin_delta = -v.add_begin_delta;
                }
            }

            if self.voices[v_idx].dma_on {
                if let Some(ev_id) = self.voices[v_idx].active_event_id {
                    let sid_frame = if self.voices[v_idx].sid_mode {
                        Some(self.step_sid(v_idx))
                    } else {
                        None
                    };
                    let v = &self.voices[v_idx];
                    let slice = (v.beg_addr, ((v.cur_len_words as u32) * 2).max(4), v.curr_period);
                    let ev = &mut self.events[ev_id];
                    if let Some(f) = sid_frame {
                        ev.sid_frames.push(f);
                    }
                    ev.tick_slices.push(slice);
                }
            }
        }

        // Master fade update
        if self.fade_dir != 0 {
            self.fade_timer = self.fade_timer.saturating_sub(1);
            if self.fade_timer == 0 {
                self.fade_timer = self.fade_slope;
                self.master_vol = (self.master_vol as i16 + self.fade_dir as i16).clamp(0, 0x40) as u8;
                if self.master_vol == self.fade_target {
                    self.fade_dir = 0;
                }
            }
        }

        // Emit frame snapshot
        let mut any_sound = false;
        let snapshots = core::array::from_fn(|i| {
            let v = &self.voices[i];
            let eff_vol = if v.dma_on {
                ((v.cur_vol as u16 * self.master_vol as u16) >> 6) as u8
            } else {
                0
            };
            if eff_vol > 0 {
                any_sound = true;
            }
            VoiceTickSnapshot {
                triggered: v.triggered_this_tick,
                event_id: v.active_event_id,
                macro_num: v.macro_num,
                period: v.curr_period,
                volume: eff_vol,
                dma_on: v.dma_on,
                beg_addr: v.beg_addr,
                len_bytes: (v.cur_len_words as u32) * 2,
            }
        });

        let is_any_track_enabled = self.tracks.iter().any(|t| t.enabled);
        if !is_any_track_enabled {
            self.silent_ticks += 1;
            if self.silent_ticks > 25 {
                self.song_ended = true;
            }
        }

        Some(TfmxTickFrame {
            tick: tick_idx,
            voices: snapshots,
        })
    }

    /// Simulate full subsong until completion or maximum tick cutoff.
    pub fn simulate_all(&mut self, max_ticks: usize) -> TfmxSimulation {
        let mut frames = Vec::with_capacity(max_ticks.min(30000));
        let tick_rate_hz = self.tick_rate_hz();

        for t in 0..max_ticks {
            if let Some(frame) = self.step_tick(t) {
                frames.push(frame);
            } else {
                break;
            }
        }

        let loop_tick = self.loop_target_pos
            .and_then(|p| self.step_first_tick.get(&p).copied())
            .unwrap_or(0);

        TfmxSimulation {
            subsong_index: self.subsong_idx,
            is_7voice: self.is_7v,
            tick_rate_hz,
            loop_tick,
            frames,
            events: self.events.clone(),
        }
    }
}
