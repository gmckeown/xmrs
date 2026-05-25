//! IT edit history block.
//!
//! Each entry is 8 bytes: `fat_date:u16 fat_time:u16 run_time:u32`.

use core::time::Duration;

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use crate::tracker::import::bin_reader::{BinReader, ImportError};

const IT_EDIT_HISTORY_ENTRY_SIZE: usize = 8;

#[derive(Default)]
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
    fn read(r: &mut BinReader) -> Result<Self, ImportError> {
        let fat_date = r.read_u16_le()?;
        let fat_time = r.read_u16_le()?;
        let run_time = r.read_u32_le()?;
        Ok(Self {
            fat_date,
            fat_time,
            run_time,
        })
    }

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
        // FAT timestamps run at 18.2 Hz (the IBM PC system
        // timer). `18.2 = 91/5`, so `seconds = run_time × 5 / 91`
        // is exact integer math; the remainder gives the
        // fractional tick which converts to nanoseconds via
        // `remainder × 1e9 / 91` (max `90 × 1e9 = 9 × 10¹⁰`,
        // fits `u64`). Replaces `(run_time as f32 / 18.2)` and
        // `.fract() * 1e9`.
        let scaled = (self.run_time as u64) * 5;
        let seconds = scaled / 91;
        let remainder = scaled % 91; // ∈ [0, 90]
        let nanoseconds = (remainder * 1_000_000_000) / 91;
        Duration::new(seconds, nanoseconds as u32)
    }
}

#[derive(Debug, Default)]
pub struct ItEditHistory;

impl ItEditHistory {
    pub fn load(source: &[u8]) -> Result<(Option<Vec<ItEditHistoryEntry>>, usize), ImportError> {
        if source.len() < 2 {
            return Err(ImportError::UnexpectedEof);
        }

        let edit_history_number = u16::from_le_bytes(source[0..2].try_into().unwrap()) as usize;
        if edit_history_number == 0 {
            return Ok((None, 2));
        }

        let total_size = IT_EDIT_HISTORY_ENTRY_SIZE * edit_history_number;
        if source.len() < 2 + total_size {
            return Err(ImportError::UnexpectedEof);
        }

        let mut r = BinReader::new(&source[2..2 + total_size]);
        let mut edit_histories: Vec<ItEditHistoryEntry> = vec![];
        for _ in 0..edit_history_number {
            edit_histories.push(ItEditHistoryEntry::read(&mut r)?);
        }

        Ok((Some(edit_histories), 2 + total_size))
    }
}
