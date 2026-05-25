//! Variable-width bit reader used by the IT sample decompressor
//! (IT2.14 / IT2.15 compressed sample payloads). The bit-stream
//! shape is unique to IT — XM/S3M/MOD samples are raw PCM and use
//! the byte-level `BinReader` directly.

pub struct BitReader<'a> {
    data: &'a [u8],
    data_index: usize,
    databit: u32,
    databit_index: u32,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            databit: 0,
            databit_index: 0,
            data,
            data_index: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.data_index >= self.data.len() && self.databit_index == 0
    }

    pub fn read_bits(&mut self, n: u8) -> Option<u32> {
        if n == 0 || n > 32 || self.is_empty() {
            return None;
        }

        // println!("{}@{}", self.databit_index, self.data_index);

        let mut retval: u32 = 0;

        for _ in 0..n {
            if self.databit_index == 0 {
                if self.data_index >= self.data.len() {
                    return None;
                }
                self.databit = self.data[self.data_index] as u32;
                self.data_index += 1;
                self.databit_index = 8;
            }

            retval = (retval >> 1) | ((self.databit & 1) << 31);

            self.databit >>= 1;
            self.databit_index -= 1;
        }

        Some(retval >> (32 - n))
    }
}
