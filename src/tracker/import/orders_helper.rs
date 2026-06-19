//! Order-list parsing helper shared by XM / S3M / IT importers.
//! Trims the markers in the raw `[u8]` order table into sub-songs of
//! pattern indices. MOD and SID importers build their orders directly
//! and don't use this helper.
//!
//! The two markers are **not** equivalent (IT/S3M spec; matches Schism
//! `fmt/it.c` / `fmt/s3m.c` and ST3):
//!
//! * `0xFE` ("+++") is a **skip** marker — skip *this* order position and
//!   continue the *same* song. It does NOT start a new sub-song. The old
//!   code split on it, which shredded any song that uses `0xFE` as an
//!   intra-song section separator (e.g. `mv-desk.it`, order
//!   `[5,6,254,1,1,254,…]`) into dozens of one-/two-pattern "sub-songs",
//!   so playback looped the first fragment forever instead of advancing
//!   through the 100-entry order.
//! * `0xFF` ("---") is the **end-of-song** sentinel; content past it is a
//!   separate packed song, so it is the genuine sub-song boundary.
//!   Sub-song 0 (everything up to the first `0xFF`) is what a tracker
//!   plays by default — the same span Schism renders.

use alloc::vec::Vec;

pub fn parse_orders(orders: &[u8]) -> Vec<Vec<usize>> {
    let mut result: Vec<Vec<usize>> = Vec::new();
    let mut current_group: Vec<usize> = Vec::new();

    for order in orders {
        match *order {
            // "+++" skip marker: omit this position, stay in the same song.
            254 => {}
            // "---" end-of-song: close the current sub-song.
            255 => {
                if !current_group.is_empty() {
                    result.push(current_group);
                    current_group = Vec::new();
                }
            }
            o => {
                current_group.push(o as usize);
            }
        }
    }

    if !current_group.is_empty() {
        result.push(current_group);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_order() {
        let orders = vec![0, 1, 2, 3];
        let result = parse_orders(&orders);
        assert_eq!(result, vec![vec![0, 1, 2, 3]]);
    }

    #[test]
    fn split_on_255() {
        let orders = vec![0, 1, 255, 2, 3];
        let result = parse_orders(&orders);
        assert_eq!(result, vec![vec![0, 1], vec![2, 3]]);
    }

    #[test]
    fn skip_on_254_does_not_split() {
        // 0xFE ("+++") is a skip marker, not a sub-song boundary: the
        // position is dropped and the song continues as one.
        let orders = vec![0, 1, 254, 2, 3];
        let result = parse_orders(&orders);
        assert_eq!(result, vec![vec![0, 1, 2, 3]]);
    }

    #[test]
    fn trailing_separator() {
        let orders = vec![0, 1, 255];
        let result = parse_orders(&orders);
        assert_eq!(result, vec![vec![0, 1]]);
    }

    #[test]
    fn empty_orders() {
        let result = parse_orders(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn consecutive_separators() {
        let orders = vec![0, 255, 255, 1];
        let result = parse_orders(&orders);
        assert_eq!(result, vec![vec![0], vec![1]]);
    }
}
