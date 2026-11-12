//! Ids of entries or names (each entry's name and parent), 3 bytes each while every one fits (a PC of 9.2M entries
//! has 2.8M names, against 16.7M that 3 bytes can number), 4 once one doesn't. In memory or mapped
//! from the file, like any column (`column`); `NONE` is kept as 3 bytes of ones.

use crate::column::Column;
use crate::store::NONE;

/// The most 3 bytes hold, kept for `NONE`: ids below it fit.
const NONE3: u32 = 0xFF_FFFF;

#[derive(Clone, PartialEq)]
pub struct Ids {
    bytes: Column<u8>,
    /// 3 or 4.
    width: usize,
}

impl Default for Ids {
    fn default() -> Ids {
        Ids { bytes: Column::default(), width: 3 }
    }
}

impl std::fmt::Debug for Ids {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl Ids {
    /// From a column of `width`-byte ids (as the file has them).
    pub fn from_bytes(bytes: Column<u8>, width: usize) -> Option<Ids> {
        ((width == 3 || width == 4) && bytes.len() % width == 0).then_some(Ids { bytes, width })
    }

    /// The ids at `width` bytes each (`write`): 3 when they all fit.
    pub fn from_values(values: &[u32]) -> Ids {
        let mut ids = Ids::default();
        ids.reserve_exact(values.len());
        for &value in values {
            ids.push(value);
        }
        ids
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn len(&self) -> usize {
        self.bytes.len() / self.width
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.bytes.capacity() / self.width
    }

    pub fn is_mapped(&self) -> bool {
        self.bytes.is_mapped()
    }

    pub fn heap_bytes(&self) -> usize {
        self.bytes.heap_bytes()
    }

    /// The bytes as kept.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[inline]
    pub fn get(&self, e: usize) -> u32 {
        let bytes: &[u8] = &self.bytes;
        if self.width == 4 {
            return u32::from_le_bytes(bytes[e * 4..e * 4 + 4].try_into().unwrap());
        }
        let i = e * 3;
        // One read of 4 bytes where there are 4 (all but the last id).
        let value = match bytes.get(i..i + 4) {
            Some(four) => u32::from_le_bytes(four.try_into().unwrap()) & NONE3,
            None => u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], 0]),
        };
        if value == NONE3 { NONE } else { value }
    }

    /// The ids from `from` on into `out` (as many as there are, up to its length): how many. What a scan over every
    /// entry reads, a block at a time (one id at a time, `get`, made name scans a fifth slower than 4-byte ids).
    #[inline]
    pub fn fill(&self, from: usize, out: &mut [u32]) -> usize {
        let n = out.len().min(self.len().saturating_sub(from));
        let bytes: &[u8] = &self.bytes;
        if self.width == 4 {
            for (value, four) in out[..n].iter_mut().zip(bytes[from * 4..(from + n) * 4].chunks_exact(4)) {
                *value = u32::from_le_bytes(four.try_into().unwrap());
            }
        } else {
            for (value, three) in out[..n].iter_mut().zip(bytes[from * 3..(from + n) * 3].chunks_exact(3)) {
                let id = three[0] as u32 | (three[1] as u32) << 8 | (three[2] as u32) << 16;
                *value = if id == NONE3 { NONE } else { id };
            }
        }
        n
    }

    /// Whether `value` fits as it's kept now.
    fn fits(&self, value: u32) -> bool {
        self.width == 4 || value < NONE3 || value == NONE
    }

    /// Every id at 4 bytes (one didn't fit): in Excavate's memory until the file is written again.
    fn widen(&mut self) {
        let values: Vec<u32> = self.iter().collect();
        let mut bytes = Vec::with_capacity(self.bytes.capacity().max(values.len() * 3) / 3 * 4);
        for value in values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        *self = Ids { bytes: bytes.into(), width: 4 };
    }

    fn put(&self, value: u32) -> [u8; 4] {
        if self.width == 3 && value == NONE { [0xFF, 0xFF, 0xFF, 0] } else { value.to_le_bytes() }
    }

    pub fn set(&mut self, e: usize, value: u32) {
        if !self.fits(value) {
            self.widen();
        }
        let width = self.width;
        let bytes = self.put(value);
        self.bytes[e * width..e * width + width].copy_from_slice(&bytes[..width]);
    }

    pub fn push(&mut self, value: u32) {
        if !self.fits(value) {
            self.widen();
        }
        let bytes = self.put(value);
        self.bytes.extend_from_slice(&bytes[..self.width]);
    }

    pub fn reserve_exact(&mut self, more: usize) {
        self.bytes.reserve_exact(more * self.width);
    }

    pub fn truncate(&mut self, to: usize) {
        self.bytes.truncate(to * self.width);
    }

    pub fn shrink_to_fit(&mut self) {
        self.bytes.shrink_to_fit();
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = u32> + '_ {
        (0..self.len()).map(|e| self.get(e))
    }

    /// Each id, in parallel.
    pub fn par_iter(&self) -> impl rayon::iter::IndexedParallelIterator<Item = u32> + '_ {
        use rayon::prelude::*;
        (0..self.len()).into_par_iter().map(|e| self.get(e))
    }

    /// Each id replaced by `f` of it, in parallel (`f` keeps them below what fits: names numbered anew).
    pub fn map_all(&mut self, f: impl Fn(u32) -> u32 + Sync) {
        use rayon::prelude::*;
        let width = self.width;
        let none3 = width == 3;
        self.bytes.par_chunks_exact_mut(width).for_each(|chunk| {
            let mut four = [0u8; 4];
            four[..width].copy_from_slice(chunk);
            let value = u32::from_le_bytes(four);
            let value = f(if none3 && value == NONE3 { NONE } else { value });
            assert!(!none3 || value < NONE3 || value == NONE, "an id past what 3 bytes hold");
            let out = if none3 && value == NONE { [0xFF, 0xFF, 0xFF, 0] } else { value.to_le_bytes() };
            chunk.copy_from_slice(&out[..width]);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_bytes_until_one_doesnt_fit() {
        let mut ids = Ids::default();
        for value in [0, 1, 0xAB_CDEF, NONE, 0xFF_FFFE] {
            ids.push(value);
        }
        assert_eq!(ids.width(), 3);
        assert_eq!(ids.bytes().len(), 15);
        assert_eq!(ids.iter().collect::<Vec<_>>(), [0, 1, 0xAB_CDEF, NONE, 0xFF_FFFE]);
        ids.set(1, NONE);
        assert_eq!(ids.get(1), NONE);
        ids.map_all(|id| if id == NONE { NONE } else { id / 2 });
        assert_eq!(ids.iter().collect::<Vec<_>>(), [0, NONE, 0x55_E6F7, NONE, 0x7F_FFFF]);
        // 16,777,215 and past: every id goes to 4 bytes, and keeps its value.
        ids.push(0xFF_FFFF);
        assert_eq!(ids.width(), 4);
        ids.set(0, 0x0123_4567);
        assert_eq!(ids.iter().collect::<Vec<_>>(), [0x0123_4567, NONE, 0x55_E6F7, NONE, 0x7F_FFFF, 0xFF_FFFF]);
        let mut out = [0u32; 4];
        assert_eq!(ids.fill(3, &mut out), 3);
        assert_eq!(out[..3], [NONE, 0x7F_FFFF, 0xFF_FFFF]);
        ids.truncate(2);
        assert_eq!(ids.len(), 2);
        assert_eq!(Ids::from_values(&[5, NONE]).width(), 3);
    }
}
