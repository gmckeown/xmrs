#[derive(Copy, Clone)]
pub struct CacheEntry {
    key: (f32, f32, f32, bool),
    value: f32,
    last_used: usize,
}

#[derive(Copy, Clone)]
pub struct PeriodHelperCache<const N: usize> {
    entries: [CacheEntry; N],
    access_counter: usize,
}

impl<const N: usize> PeriodHelperCache<N> {
    pub const fn new() -> Self {
        let default_entry = CacheEntry {
            key: (0.0, 0.0, 0.0, false),
            value: 0.0,
            last_used: 0,
        };
        PeriodHelperCache {
            entries: [default_entry; N],
            access_counter: 0,
        }
    }

    fn reset_counters(&mut self) {
        self.access_counter = 0;
        for entry in &mut self.entries {
            entry.last_used = 0;
        }
    }

    pub fn get(
        &mut self,
        period: f32,
        arp_pitch: f32,
        finetune: f32,
        semitone: bool,
    ) -> Option<f32> {
        if self.access_counter >= usize::MAX - 1 {
            self.reset_counters();
        }

        for entry in &mut self.entries {
            if entry.key == (period, arp_pitch, finetune, semitone) {
                self.access_counter += 1;
                entry.last_used = self.access_counter;
                return Some(entry.value);
            }
        }
        None
    }

    pub fn insert(
        &mut self,
        period: f32,
        arp_pitch: f32,
        finetune: f32,
        semitone: bool,
        frequency: f32,
    ) {
        if self.access_counter >= usize::MAX - 1 {
            self.reset_counters();
        }

        let mut lru_index = 0;
        let mut lru_time = self.entries[0].last_used;

        for i in 1..N {
            if self.entries[i].last_used < lru_time {
                lru_index = i;
                lru_time = self.entries[i].last_used;
            }
        }

        self.access_counter += 1;
        self.entries[lru_index] = CacheEntry {
            key: (period, arp_pitch, finetune, semitone),
            value: frequency,
            last_used: self.access_counter,
        };
    }
}
