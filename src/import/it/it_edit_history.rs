use core::time::Duration;

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;
use bincode::error::DecodeError;
use bincode::Decode;
use core::fmt;
use serde::Deserialize;

// Float math backend (only needed when `std` is disabled).
// Priority: std > libm > micromath.
#[cfg(all(not(feature = "std"), not(feature = "libm"), feature = "micromath"))]
#[allow(unused_imports)]
use micromath::F32Ext;
#[cfg(all(not(feature = "std"), feature = "libm"))]
#[allow(unused_imports)]
use num_traits::float::Float;

#[derive(Deserialize, Default, Decode)]
#[repr(C)]
pub struct ItEditHistoryEntry {
    // 2 bytes (Microsoft FAT date format)
    fat_date: u16,

    // 2 bytes (Microsoft FAT time format)
    fat_time: u16,

    // 4 bytes (Runtime in MS-DOS ticks, 1/18.2 second)
    run_time: u32,
}

impl fmt::Debug for ItEditHistoryEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let date = self.date();
        let time = self.time();
        f.debug_struct("ItEditHistoryEntry")
            .field(
                "datetime",
                &format!(
                    "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                    date.0, date.1, date.2, time.0, time.1, time.2
                ),
            )
            .field("duration", &format!("{:?}", self.duration()))
            .finish()
    }
}

impl ItEditHistoryEntry {
    /// Return (year, month, day)
    pub fn date(&self) -> (u16, u8, u8) {
        let year = (self.fat_date >> 9) + 1980;
        let month = ((self.fat_date >> 5) & 0x0F) as u8;
        let day = (self.fat_date & 0x1F) as u8;
        (year, month, day)
    }

    /// Return (hour, minute, second)
    pub fn time(&self) -> (u8, u8, u8) {
        let hour = ((self.fat_time >> 11) & 0x1F) as u8;
        let minute = ((self.fat_time >> 5) & 0x3F) as u8;
        let second = ((self.fat_time & 0x1F) * 2) as u8;
        (hour, minute, second)
    }

    /// Return Duration
    pub fn duration(&self) -> Duration {
        let ticks = self.run_time as f32 / 18.2;
        let seconds = ticks as u64;
        let nanoseconds = (ticks.fract() * 1e9) as u32;
        Duration::new(seconds, nanoseconds)
    }
}

#[derive(Deserialize, Debug, Default)]
#[repr(C)]
pub struct ItEditHistory;

impl ItEditHistory {
    pub fn load(source: &[u8]) -> Result<(Option<Vec<ItEditHistoryEntry>>, usize), DecodeError> {
        let data = source;

        if data.len() < 2 {
            return Err(DecodeError::LimitExceeded);
        }

        let edit_history_number: u16 = u16::from_le_bytes(data[0..2].try_into().unwrap());
        if edit_history_number == 0 {
            return Ok((None, 2));
        }

        let struct_size = core::mem::size_of::<ItEditHistoryEntry>();
        let total_size = struct_size * edit_history_number as usize;
        let data = &data[2..2 + total_size];

        if data.len() < total_size {
            return Err(DecodeError::LimitExceeded);
        }

        let mut edit_histories: Vec<ItEditHistoryEntry> = vec![];
        for i in 0..edit_history_number {
            let start = struct_size * i as usize;
            let end = start + struct_size;
            let src = &data[start..end];
            let edit_history_entry: (ItEditHistoryEntry, usize) =
                bincode::decode_from_slice::<ItEditHistoryEntry, _>(
                    src,
                    bincode::config::legacy(),
                )?;
            edit_histories.push(edit_history_entry.0);
        }

        Ok((Some(edit_histories), 2 + total_size))
    }
}
