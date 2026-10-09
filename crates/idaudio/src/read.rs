//! Bounds-checked little-endian reader.

use anyhow::{Context, Result};

pub struct Cursor<'a> {
    pub bytes: &'a [u8],
    pub pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let s = self
            .bytes
            .get(self.pos..self.pos + n)
            .with_context(|| format!("read of {n} bytes at {:#x} past end ({:#x})", self.pos, self.bytes.len()))?;
        self.pos += n;
        Ok(s)
    }
    pub fn sub(&mut self, n: usize) -> Result<&'a [u8]> {
        self.bytes(n)
    }
    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.bytes(n).map(|_| ())
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.bytes(2)?.try_into()?))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into()?))
    }
    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.bytes(4)?.try_into()?))
    }
    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.bytes(4)?.try_into()?))
    }
    pub fn be_u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.bytes(4)?.try_into()?))
    }
    pub fn be_f32(&mut self) -> Result<f32> {
        Ok(f32::from_be_bytes(self.bytes(4)?.try_into()?))
    }
}
