//! LEB128 variable-length integers.

pub fn write_uleb(out: &mut Vec<u8>, mut v: u32) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

pub fn write_sleb(out: &mut Vec<u8>, mut v: i32) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        let done = (v == 0 && byte & 0x40 == 0) || (v == -1 && byte & 0x40 != 0);
        if done {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Returns `None` on truncation or overflow (more than 5 bytes / > u32).
pub fn read_uleb(buf: &[u8], pos: &mut usize) -> Option<u32> {
    let mut result: u64 = 0;
    for i in 0..5 {
        let byte = *buf.get(*pos)?;
        *pos += 1;
        result |= ((byte & 0x7f) as u64) << (7 * i);
        if byte & 0x80 == 0 {
            return u32::try_from(result).ok();
        }
    }
    None
}

pub fn read_sleb(buf: &[u8], pos: &mut usize) -> Option<i32> {
    let mut result: i64 = 0;
    let mut shift = 0;
    loop {
        let byte = *buf.get(*pos)?;
        *pos += 1;
        result |= ((byte & 0x7f) as i64) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            if shift < 64 && byte & 0x40 != 0 {
                result |= -1i64 << shift;
            }
            return i32::try_from(result).ok();
        }
        if shift >= 35 {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for v in [0u32, 1, 127, 128, 300, 16384, u32::MAX] {
            let mut b = Vec::new();
            write_uleb(&mut b, v);
            let mut p = 0;
            assert_eq!(read_uleb(&b, &mut p), Some(v));
            assert_eq!(p, b.len());
        }
        for v in [0i32, 1, -1, 63, -64, 64, -65, i32::MAX, i32::MIN] {
            let mut b = Vec::new();
            write_sleb(&mut b, v);
            let mut p = 0;
            assert_eq!(read_sleb(&b, &mut p), Some(v), "{v}");
            assert_eq!(p, b.len());
        }
        let mut p = 0;
        assert_eq!(read_uleb(&[0x80, 0x80], &mut p), None);
    }
}
