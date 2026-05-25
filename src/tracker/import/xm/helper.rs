//! Tiny byte-twiddling helpers used by the XM loader — little-
//! endian `[u8]` → `[u16]` and `[u8]` → `[i8]` conversions over
//! the raw envelope / sample data the XM format stores byte-packed.

use alloc::{vec, vec::Vec};

pub fn u8_slice_to_vec_u16(u8_slice: &[u8]) -> Vec<u16> {
    // u8 slice must have an even number of elements: do the better w/o error
    let src = if !u8_slice.len().is_multiple_of(2) {
        &u8_slice[0..u8_slice.len() - 1]
    } else {
        u8_slice
    };
    let u16_vec: Vec<u16> = src
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    u16_vec
}

pub fn vec_u16_to_u8_slice(u16_vec: Vec<u16>) -> Vec<u8> {
    // allocate memory for the resulting slice
    let byte_len = u16_vec.len() * 2;
    let mut byte_vec = vec![0u8; byte_len];

    // convert each u16 to a little-endian byte array and copy into byte_vec
    for (i, u16_val) in u16_vec.iter().enumerate() {
        let bytes = u16_val.to_le_bytes();
        byte_vec[i * 2] = bytes[0];
        byte_vec[i * 2 + 1] = bytes[1];
    }
    byte_vec
}

pub fn delta8_to_sample(delta: Vec<u8>) -> Vec<i8> {
    let mut sample: Vec<i8> = vec![];
    if !delta.is_empty() {
        let mut old = 0;
        for item in &delta {
            let new = item.overflowing_add(old).0;
            sample.push(new as i8);
            old = new;
        }
    }
    sample
}

pub fn delta16_to_sample(delta: Vec<u16>) -> Vec<i16> {
    if delta.is_empty() {
        return vec![];
    }
    let mut sample: Vec<i16> = vec![];
    let mut old = 0;
    for item in &delta {
        let new = item.overflowing_add(old).0;
        sample.push(new as i16);
        old = new;
    }
    sample
}

pub fn sample8_to_delta(sample: &[i8]) -> Vec<u8> {
    let mut delta: Vec<u8> = vec![];
    let mut new: i8 = 0;
    for item in sample {
        let d = item.overflowing_sub(new).0;
        delta.push(d as u8);
        new = *item;
    }
    delta
}

pub fn sample16_to_delta(sample: &[i16]) -> Vec<u16> {
    let mut delta: Vec<u16> = vec![];
    let mut new: i16 = 0;
    for item in sample {
        let d = item.overflowing_sub(new).0;
        delta.push(d as u16);
        new = *item;
    }
    delta
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta8_roundtrip() {
        let original: Vec<i8> = vec![0, 10, 20, -5, -128, 127, 0];
        let delta = sample8_to_delta(&original);
        let recovered = delta8_to_sample(delta);
        assert_eq!(original, recovered);
    }

    #[test]
    fn delta16_roundtrip() {
        let original: Vec<i16> = vec![0, 1000, -1000, 32767, -32768, 0];
        let delta = sample16_to_delta(&original);
        let recovered = delta16_to_sample(delta);
        assert_eq!(original, recovered);
    }

    #[test]
    fn delta8_empty() {
        assert!(delta8_to_sample(vec![]).is_empty());
        assert!(sample8_to_delta(&[]).is_empty());
    }

    #[test]
    fn delta16_empty() {
        assert!(delta16_to_sample(vec![]).is_empty());
        assert!(sample16_to_delta(&[]).is_empty());
    }

    #[test]
    fn delta8_single_element() {
        let original: Vec<i8> = vec![42];
        let delta = sample8_to_delta(&original);
        let recovered = delta8_to_sample(delta);
        assert_eq!(original, recovered);
    }

    #[test]
    fn delta16_single_element() {
        let original: Vec<i16> = vec![-12345];
        let delta = sample16_to_delta(&original);
        let recovered = delta16_to_sample(delta);
        assert_eq!(original, recovered);
    }

    #[test]
    fn u8_u16_roundtrip() {
        let original: Vec<u16> = vec![0x0102, 0xFFFE, 0x0000, 0x8000];
        let bytes = vec_u16_to_u8_slice(original.clone());
        let recovered = u8_slice_to_vec_u16(&bytes);
        assert_eq!(original, recovered);
    }

    #[test]
    fn u8_u16_odd_length() {
        // Odd-length slice should drop the last byte
        let bytes = vec![1, 2, 3];
        let result = u8_slice_to_vec_u16(&bytes);
        assert_eq!(result.len(), 1);
    }
}
