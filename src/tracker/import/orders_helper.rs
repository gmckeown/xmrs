//! Order-list parsing helper shared by XM / S3M / IT importers.
//! Splits the raw `[u8]` order table on the `0xFE` sub-song
//! separator (and trims the `0xFF` end-of-list sentinel) into
//! sub-songs of pattern indices. MOD and SID importers build their
//! orders directly and don't use this helper.

use alloc::vec::Vec;

pub fn parse_orders(orders: &[u8]) -> Vec<Vec<usize>> {
    let mut result: Vec<Vec<usize>> = Vec::new();
    let mut current_group: Vec<usize> = Vec::new();

    for order in orders {
        match *order {
            254 | 255 => {
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
    fn split_on_254() {
        let orders = vec![0, 1, 254, 2, 3];
        let result = parse_orders(&orders);
        assert_eq!(result, vec![vec![0, 1], vec![2, 3]]);
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
