//! Bounds-checked little/big-endian binary reader for Unity data.

use super::UnityError;

#[derive(Clone)]
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    big_endian: bool,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8], big_endian: bool) -> Self {
        Self {
            data,
            pos: 0,
            big_endian,
        }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn seek(&mut self, pos: usize) {
        self.pos = pos;
    }

    pub fn set_big_endian(&mut self, big: bool) {
        self.big_endian = big;
    }

    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], UnityError> {
        let end = self.pos.checked_add(n).ok_or(UnityError::Truncated)?;
        let s = self.data.get(self.pos..end).ok_or(UnityError::Truncated)?;
        self.pos = end;
        Ok(s)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], UnityError> {
        let mut a: [u8; N] = self.bytes(N)?.try_into().expect("length checked");
        if self.big_endian {
            a.reverse();
        }
        Ok(a)
    }

    pub fn u8(&mut self) -> Result<u8, UnityError> {
        Ok(self.bytes(1)?[0])
    }
    pub fn bool(&mut self) -> Result<bool, UnityError> {
        Ok(self.u8()? != 0)
    }
    pub fn i16(&mut self) -> Result<i16, UnityError> {
        Ok(i16::from_le_bytes(self.array()?))
    }
    pub fn u32(&mut self) -> Result<u32, UnityError> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    pub fn i32(&mut self) -> Result<i32, UnityError> {
        Ok(i32::from_le_bytes(self.array()?))
    }
    pub fn u64(&mut self) -> Result<u64, UnityError> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    pub fn i64(&mut self) -> Result<i64, UnityError> {
        Ok(i64::from_le_bytes(self.array()?))
    }
    pub fn f32(&mut self) -> Result<f32, UnityError> {
        Ok(f32::from_le_bytes(self.array()?))
    }

    /// Advances to the next multiple of 4 (Unity aligns many fields).
    pub fn align4(&mut self) {
        self.pos = (self.pos + 3) & !3;
    }

    /// NUL-terminated string.
    pub fn cstring(&mut self) -> Result<String, UnityError> {
        let rest = self.data.get(self.pos..).ok_or(UnityError::Truncated)?;
        let len = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or(UnityError::Truncated)?;
        let s = String::from_utf8_lossy(&rest[..len]).into_owned();
        self.pos += len + 1;
        Ok(s)
    }

    /// Length-prefixed (i32) string followed by 4-byte alignment.
    pub fn aligned_string(&mut self) -> Result<String, UnityError> {
        let len = self.i32()?;
        if len < 0 || len as usize > self.remaining() {
            return Err(UnityError::Invalid(format!("string length {len}")));
        }
        let s = String::from_utf8_lossy(self.bytes(len as usize)?).into_owned();
        self.align4();
        Ok(s)
    }

    /// Length-prefixed (i32) byte array followed by 4-byte alignment.
    pub fn aligned_bytes(&mut self) -> Result<&'a [u8], UnityError> {
        let len = self.i32()?;
        if len < 0 || len as usize > self.remaining() {
            return Err(UnityError::Invalid(format!("array length {len}")));
        }
        let b = self.bytes(len as usize)?;
        self.align4();
        Ok(b)
    }

    /// Element count for a serialized array, sanity-checked against the
    /// remaining bytes (each element needs at least `min_elem` bytes).
    pub fn count(&mut self, min_elem: usize) -> Result<usize, UnityError> {
        let n = self.i32()?;
        if n < 0 || (n as usize).saturating_mul(min_elem.max(1)) > self.remaining() {
            return Err(UnityError::Invalid(format!("array count {n}")));
        }
        Ok(n as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_both_endians_and_aligns() {
        let data = [1, 0, 0, 0, 0, 0, 0, 2, b'h', b'i', 0, 9];
        let mut r = Reader::new(&data, false);
        assert_eq!(r.u32().unwrap(), 1);
        r.set_big_endian(true);
        assert_eq!(r.u32().unwrap(), 2);
        assert_eq!(r.cstring().unwrap(), "hi");
        assert_eq!(r.pos(), 11);
        r.align4();
        assert_eq!(r.pos(), 12);
        assert!(matches!(r.u8(), Err(UnityError::Truncated)));
    }

    #[test]
    fn aligned_string_and_bad_lengths() {
        let data = [3, 0, 0, 0, b'a', b'b', b'c', 0, 0xff, 0xff, 0xff, 0x7f];
        let mut r = Reader::new(&data, false);
        assert_eq!(r.aligned_string().unwrap(), "abc");
        assert_eq!(r.pos(), 8);
        assert!(r.aligned_string().is_err());
    }
}
