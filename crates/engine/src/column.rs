//! The index's arrays, either in Excavate's own memory (a `Vec`) or straight in the saved file, mapped. Mapped, the file *is* the array: Windows reads its pages in as they're touched and writes changed ones back
//! by itself, so the index costs Excavate no private memory (what Task Manager calls its memory) and a save writes only
//! what changed. Each array has room to grow in the file; one that outgrows it moves to Excavate's memory until the
//! file is written again with more room (`Index::save`).
//!
//! The file is opened for reading and writing, shared for reading only (no second Excavate can change it), and
//! mapped once, whole. Arrays are plain old data (u8, u32): any bytes are a valid value, so a mapped array can be
//! read as a slice of them.

use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FlushFileBuffers, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE, FILE_SHARE_READ, OPEN_EXISTING,
};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, FlushViewOfFile, MapViewOfFile, UnmapViewOfFile, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};

/// The saved file, mapped whole, read and written in place.
pub struct Mapping {
    file: HANDLE,
    mapping: HANDLE,
    view: *mut u8,
    len: usize,
}

// SAFETY: the view is plain memory; the index that holds it is behind a lock.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Mapping {
    pub fn open(path: &std::path::Path) -> std::io::Result<Arc<Mapping>> {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: plain Win32 calls; every handle is closed on failure or by Drop.
        unsafe {
            // (Shared for deleting too: the file is renamed into place while it's mapped, `Index::save`.)
            let file = CreateFileW(wide.as_ptr(), GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_DELETE, std::ptr::null(),
                                   OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, std::ptr::null_mut());
            if file == INVALID_HANDLE_VALUE {
                return Err(std::io::Error::last_os_error());
            }
            let len = std::fs::metadata(path).map(|m| m.len() as usize).unwrap_or(0);
            if len == 0 {
                CloseHandle(file);
                return Err(std::io::Error::other("an empty file"));
            }
            let mapping = CreateFileMappingW(file, std::ptr::null(), PAGE_READWRITE, 0, 0, std::ptr::null());
            if mapping.is_null() {
                let error = std::io::Error::last_os_error();
                CloseHandle(file);
                return Err(error);
            }
            let view = MapViewOfFile(mapping, FILE_MAP_WRITE, 0, 0, 0).Value as *mut u8;
            if view.is_null() {
                let error = std::io::Error::last_os_error();
                CloseHandle(mapping);
                CloseHandle(file);
                return Err(error);
            }
            Ok(Arc::new(Mapping { file, mapping, view, len }))
        }
    }

    pub fn bytes(&self) -> &[u8] {
        // SAFETY: the view is `len` bytes, mapped for as long as self lives.
        unsafe { std::slice::from_raw_parts(self.view, self.len) }
    }

    /// The bytes at `offset`, to write in place (the header). The caller makes sure nothing reads them meanwhile.
    #[allow(clippy::mut_from_ref)]
    pub fn bytes_mut(&self, offset: usize, len: usize) -> &mut [u8] {
        assert!(offset + len <= self.len);
        // SAFETY: inside the view; the index's lock keeps other users away.
        unsafe { std::slice::from_raw_parts_mut(self.view.add(offset), len) }
    }

    /// Writes the changed pages in `offset..offset + len` to the disk, and waits until they're there.
    pub fn flush(&self, offset: usize, len: usize) -> std::io::Result<()> {
        // SAFETY: a range inside the view.
        unsafe {
            if FlushViewOfFile(self.view.add(offset) as *const _, len.min(self.len - offset)) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            if FlushFileBuffers(self.file) == 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn at(&self, offset: usize) -> *mut u8 {
        // SAFETY: callers stay inside the view.
        unsafe { self.view.add(offset) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: what `open` made, released once.
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.view as *mut _ });
            CloseHandle(self.mapping);
            CloseHandle(self.file);
        }
    }
}

/// An array of plain values, in Excavate's memory or mapped from the file (with room for `cap` of them there).
pub enum Column<T: Copy> {
    Heap(Vec<T>),
    Mapped { map: Arc<Mapping>, at: *mut T, len: usize, cap: usize },
}

// SAFETY: T is plain data, and a mapped column only points into its mapping, which it keeps alive.
unsafe impl<T: Copy + Send> Send for Column<T> {}
unsafe impl<T: Copy + Sync> Sync for Column<T> {}

impl<T: Copy> Default for Column<T> {
    fn default() -> Self {
        Column::Heap(Vec::new())
    }
}

impl<T: Copy> From<Vec<T>> for Column<T> {
    fn from(vec: Vec<T>) -> Self {
        Column::Heap(vec)
    }
}

impl<T: Copy> Deref for Column<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        match self {
            Column::Heap(vec) => vec,
            // SAFETY: `len` values written at `at`, inside the mapping held.
            Column::Mapped { at, len, .. } => unsafe { std::slice::from_raw_parts(*at, *len) },
        }
    }
}

impl<T: Copy> DerefMut for Column<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        match self {
            Column::Heap(vec) => vec,
            // SAFETY: as above; &mut self means no one else looks.
            Column::Mapped { at, len, .. } => unsafe { std::slice::from_raw_parts_mut(*at, *len) },
        }
    }
}

impl<T: Copy> Clone for Column<T> {
    /// A copy in memory (a mapped column's copy isn't mapped).
    fn clone(&self) -> Self {
        Column::Heap(self.to_vec())
    }
}

impl<T: Copy> Column<T> {
    /// `len` values at `offset` in the mapping, with room for `cap` (the caller checked they're inside it and aligned).
    pub fn mapped(map: &Arc<Mapping>, offset: usize, len: usize, cap: usize) -> Column<T> {
        assert!(offset % std::mem::align_of::<T>() == 0 && offset + cap * std::mem::size_of::<T>() <= map.len() && len <= cap);
        Column::Mapped { map: map.clone(), at: map.at(offset) as *mut T, len, cap }
    }

    pub fn is_mapped(&self) -> bool {
        matches!(self, Column::Mapped { .. })
    }

    pub fn capacity(&self) -> usize {
        match self {
            Column::Heap(vec) => vec.capacity(),
            Column::Mapped { cap, .. } => *cap,
        }
    }

    /// The column in Excavate's memory, with room for `more` (it outgrew its room in the file).
    fn to_heap(&mut self, more: usize) -> &mut Vec<T> {
        if let Column::Mapped { len, .. } = self {
            let mut vec = Vec::with_capacity(*len + more);
            vec.extend_from_slice(self);
            *self = Column::Heap(vec);
        }
        match self {
            Column::Heap(vec) => vec,
            Column::Mapped { .. } => unreachable!(),
        }
    }

    /// Room for `more` without moving (mapped: in its room in the file, else it moves to memory).
    pub fn reserve_exact(&mut self, more: usize) {
        match self {
            Column::Heap(vec) => vec.reserve_exact(more),
            Column::Mapped { len, cap, .. } if *len + more <= *cap => {}
            Column::Mapped { .. } => {
                self.to_heap(more);
            }
        }
    }

    pub fn reserve(&mut self, more: usize) {
        match self {
            Column::Heap(vec) => vec.reserve(more),
            Column::Mapped { len, cap, .. } if *len + more <= *cap => {}
            Column::Mapped { len, .. } => {
                let more = more.max(*len / 32).max(4096);
                self.to_heap(more);
            }
        }
    }

    pub fn push(&mut self, value: T) {
        match self {
            Column::Heap(vec) => vec.push(value),
            Column::Mapped { at, len, cap, .. } if *len < *cap => {
                // SAFETY: below `cap`, inside the mapping.
                unsafe { at.add(*len).write(value) };
                *len += 1;
            }
            Column::Mapped { len, .. } => {
                let more = (*len / 32).max(4096);
                self.to_heap(more).push(value);
            }
        }
    }

    pub fn extend_from_slice(&mut self, values: &[T]) {
        match self {
            Column::Heap(vec) => vec.extend_from_slice(values),
            Column::Mapped { at, len, cap, .. } if *len + values.len() <= *cap => {
                // SAFETY: the values fit below `cap`.
                unsafe { std::ptr::copy_nonoverlapping(values.as_ptr(), at.add(*len), values.len()) };
                *len += values.len();
            }
            Column::Mapped { len, .. } => {
                let more = (*len / 32).max(values.len() + 4096);
                self.to_heap(more).extend_from_slice(values);
            }
        }
    }

    pub fn extend(&mut self, values: impl IntoIterator<Item = T>) {
        for value in values {
            self.push(value);
        }
    }

    pub fn truncate(&mut self, to: usize) {
        match self {
            Column::Heap(vec) => vec.truncate(to),
            Column::Mapped { len, .. } => *len = (*len).min(to),
        }
    }

    pub fn clear(&mut self) {
        self.truncate(0);
    }

    /// Grows the way `Vec::resize` does: room for more than asked, so growing one at a time (the MFT's records, one
    /// by one) isn't a copy of everything each time. (`reserve_exact` here made reading an MFT copy the table at every
    /// one of its 9.75M records: 45 s instead of 13.)
    pub fn resize(&mut self, to: usize, value: T) {
        if to <= self.len() {
            self.truncate(to);
        } else {
            let more = to - self.len();
            self.reserve(more);
            for _ in 0..more {
                self.push(value);
            }
        }
    }

    /// Memory held beyond what's used goes back (a mapped column holds none of Excavate's).
    pub fn shrink_to_fit(&mut self) {
        if let Column::Heap(vec) = self {
            vec.shrink_to_fit();
        }
    }

    /// Bytes of Excavate's own memory this takes (0 mapped).
    pub fn heap_bytes(&self) -> usize {
        match self {
            Column::Heap(vec) => vec.capacity() * std::mem::size_of::<T>(),
            Column::Mapped { .. } => 0,
        }
    }
}

impl<T: Copy + PartialEq> PartialEq for Column<T> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl<T: Copy + std::fmt::Debug> std::fmt::Debug for Column<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        (**self).fmt(f)
    }
}
