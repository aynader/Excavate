//! The way in with admin rights, as Everything's service does it.
//!
//! - **Building:** read the volume's `$MFT` directly (`raw`): the boot sector says where it is, record 0 says where
//!   its pieces are, then every 1 KB file record in order, in big reads. Each record gives the file's names (every
//!   hard link's, the DOS short names left out), its parents, its size (the unnamed `$DATA`), its date and attributes
//!   (`$STANDARD_INFORMATION`). No call per file and no folder opened.
//! - **Following:** the change journal, without admin rights (`journal`).
//!
//! Both need a volume handle opened for reading, which Windows gives only to an elevated process (or a service). The
//! record parsing works on any `Read + Seek`, so it's tested on a volume image made in memory.

use crate::store::{Index, Meta, DIR, HIDDEN, NONE, REPARSE, SYSTEM};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};

/// NTFS's root folder is always record 5; records below 16 are NTFS's own, and 11 (`$Extend`) holds more of them.
pub(crate) const ROOT_RECORD: u64 = 5;
const EXTEND_RECORD: u64 = 11;
const FIRST_USER_RECORD: u64 = 16;

const ATTR_STANDARD_INFORMATION: u32 = 0x10;
const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_DATA: u32 = 0x80;
const ATTR_END: u32 = 0xFFFF_FFFF;

/// Namespaces of a `$FILE_NAME`: the DOS one is the 8.3 short name of a name that has a long one.
const NAMESPACE_DOS: u8 = 2;

const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// What the boot sector says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Boot {
    pub bytes_per_sector: u64,
    pub cluster: u64,
    pub mft_lcn: u64,
    pub record: u64,
}

pub fn parse_boot(sector: &[u8]) -> Option<Boot> {
    if sector.get(3..11)? != b"NTFS    " {
        return None;
    }
    let bytes_per_sector = u16_at(sector, 0x0B)? as u64;
    let cluster = bytes_per_sector * *sector.get(0x0D)? as u64;
    let mft_lcn = u64_at(sector, 0x30)?;
    let per_record = *sector.get(0x40)? as i8;
    let record = if per_record < 0 { 1u64 << (-per_record as u32) } else { per_record as u64 * cluster };
    (bytes_per_sector >= 256 && cluster > 0 && record >= 256).then_some(Boot { bytes_per_sector, cluster, mft_lcn, record })
}

/// A non-resident attribute's data runs: (first cluster on the volume, or none for a sparse run; length in clusters).
pub fn data_runs(mut runs: &[u8]) -> Option<Vec<(Option<u64>, u64)>> {
    let mut out = Vec::new();
    let mut lcn: i64 = 0;
    while let Some((&header, rest)) = runs.split_first() {
        if header == 0 {
            break;
        }
        let (len_size, off_size) = ((header & 0x0F) as usize, (header >> 4) as usize);
        if len_size == 0 || len_size > 8 || off_size > 8 || rest.len() < len_size + off_size {
            return None;
        }
        let mut length = 0u64;
        for (i, &b) in rest[..len_size].iter().enumerate() {
            length |= (b as u64) << (8 * i);
        }
        let start = if off_size == 0 {
            None
        } else {
            let mut delta = 0i64;
            for (i, &b) in rest[len_size..len_size + off_size].iter().enumerate() {
                delta |= (b as i64) << (8 * i);
            }
            // Sign-extend.
            let shift = 64 - 8 * off_size as u32;
            delta = (delta << shift) >> shift;
            lcn += delta;
            Some(lcn as u64)
        };
        out.push((start, length));
        runs = &rest[len_size + off_size..];
    }
    Some(out)
}

/// Undoes the update sequence: the last two bytes of each sector were swapped for a check value when written.
/// False when a sector doesn't check (torn write): the record isn't trusted.
pub fn fixup(record: &mut [u8], sector: usize) -> bool {
    let (Some(offset), Some(count)) = (u16_at(record, 4), u16_at(record, 6)) else { return false };
    let (offset, count) = (offset as usize, count as usize);
    if count == 0 || offset + count * 2 > record.len() || (count - 1) * sector > record.len() {
        return false;
    }
    let check = [record[offset], record[offset + 1]];
    for i in 1..count {
        let end = i * sector - 2;
        if record[end..end + 2] != check {
            return false;
        }
        record[end] = record[offset + 2 * i];
        record[end + 1] = record[offset + 2 * i + 1];
    }
    true
}

/// One file record, as far as an index needs it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    pub is_dir: bool,
    /// The base record of an extension record (0 for a base record itself).
    pub base: u64,
    /// Each name and the record of its folder (hard links have several), DOS short names left out.
    pub names: Vec<(u64, Vec<u16>)>,
    pub size: Option<u64>,
    /// FILETIME of the last write, from `$STANDARD_INFORMATION`.
    pub written: Option<i64>,
    /// And of its making and last opening (`dc:`, `da:`).
    pub created: Option<i64>,
    pub accessed: Option<i64>,
    pub attributes: u32,
}

/// Parses a record that's been fixed up. None when it isn't a record in use.
pub fn parse_record(rec: &[u8]) -> Option<Record> {
    if rec.get(0..4)? != b"FILE" {
        return None;
    }
    let flags = u16_at(rec, 22)?;
    if flags & 1 == 0 {
        return None;
    }
    let mut out = Record { is_dir: flags & 2 != 0, base: u64_at(rec, 32)? & 0x0000_FFFF_FFFF_FFFF, ..Record::default() };
    let used = (u32_at(rec, 24)? as usize).min(rec.len());
    let mut at = u16_at(rec, 20)? as usize;
    while at + 16 <= used {
        let kind = u32_at(rec, at)?;
        if kind == ATTR_END {
            break;
        }
        let length = u32_at(rec, at + 4)? as usize;
        if length < 16 || at + length > used {
            break;
        }
        let attribute = &rec[at..at + length];
        let non_resident = attribute[8] != 0;
        let name_length = attribute[9];
        let value = || -> Option<&[u8]> {
            let size = u32_at(attribute, 16)? as usize;
            let offset = u16_at(attribute, 20)? as usize;
            attribute.get(offset..offset + size)
        };
        match kind {
            ATTR_STANDARD_INFORMATION if !non_resident => {
                let v = value()?;
                out.created = Some(u64_at(v, 0)? as i64);
                out.written = Some(u64_at(v, 8)? as i64);
                out.accessed = Some(u64_at(v, 24)? as i64);
                out.attributes = u32_at(v, 32)?;
            }
            ATTR_FILE_NAME if !non_resident => {
                let v = value()?;
                let parent = u64_at(v, 0)? & 0x0000_FFFF_FFFF_FFFF;
                let chars = *v.get(64)? as usize;
                let namespace = *v.get(65)?;
                if namespace != NAMESPACE_DOS {
                    let name: Vec<u16> = v.get(66..66 + chars * 2)?.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                    out.names.push((parent, name));
                }
            }
            // The file's own data (named streams aside); for a non-resident one, the piece that starts at cluster 0
            // holds the size.
            ATTR_DATA if name_length == 0 => {
                if non_resident {
                    if u64_at(attribute, 16)? == 0 {
                        out.size = Some(u64_at(attribute, 48)?);
                    }
                } else {
                    out.size = Some(u32_at(attribute, 16)? as u64);
                }
            }
            _ => {}
        }
        at += length;
    }
    Some(out)
}

/// Where the MFT's own pieces are: record 0's unnamed `$DATA` runs, as (byte offset on the volume, bytes).
fn mft_extents(boot: &Boot, record0: &[u8]) -> Option<Vec<(u64, u64)>> {
    let used = (u32_at(record0, 24)? as usize).min(record0.len());
    let mut at = u16_at(record0, 20)? as usize;
    while at + 16 <= used {
        let kind = u32_at(record0, at)?;
        let length = u32_at(record0, at + 4)? as usize;
        if kind == ATTR_END || length < 16 || at + length > used {
            break;
        }
        let attribute = &record0[at..at + length];
        if kind == ATTR_DATA && attribute[9] == 0 && attribute[8] != 0 {
            let runs_at = u16_at(attribute, 32)? as usize;
            let data_size = u64_at(attribute, 48)?;
            let mut extents = Vec::new();
            let mut left = data_size;
            for (start, clusters) in data_runs(attribute.get(runs_at..)?)? {
                let bytes = (clusters * boot.cluster).min(left);
                extents.push((start? * boot.cluster, bytes));
                left -= bytes;
            }
            return Some(extents);
        }
        at += length;
    }
    None
}

/// What reading the MFT found.
#[derive(Debug, Default)]
pub struct MftRead {
    pub records: u64,
    pub in_use: u64,
    pub names: u64,
    pub torn: u64,
    pub bytes: u64,
}

/// Every file and folder on a volume, from its MFT (`volume` reads the raw volume: a handle, or an image), into
/// `index` under a root named `drive` ("C:"). By record number, `index.by_record` gives each record's first entry.
pub fn raw(index: &mut Index, volume: &mut (impl Read + Seek + Send), drive: &str) -> std::io::Result<MftRead> {
    let bad = |what: &str| std::io::Error::other(what.to_string());
    let mut sector = vec![0u8; 4096];
    volume.seek(SeekFrom::Start(0))?;
    volume.read_exact(&mut sector[..512])?;
    let boot = parse_boot(&sector).ok_or_else(|| bad("not an NTFS volume"))?;
    let record_size = boot.record as usize;
    let sector_size = boot.bytes_per_sector as usize;
    // Record 0 is the MFT itself: where its pieces are.
    let mut record0 = vec![0u8; record_size.max(boot.bytes_per_sector as usize)];
    volume.seek(SeekFrom::Start(boot.mft_lcn * boot.cluster))?;
    volume.read_exact(&mut record0)?;
    if !fixup(&mut record0[..record_size], sector_size) {
        return Err(bad("the MFT's first record doesn't check"));
    }
    let extents = mft_extents(&boot, &record0[..record_size]).ok_or_else(|| bad("the MFT's pieces aren't readable"))?;
    let root = index.push(NONE, drive.trim_end_matches('\\'), Meta { size: None, modified: 0, flags: DIR });
    let first = index.len();
    // Each entry pushed here: the record of its folder, and its own record.
    let mut parents: Vec<u64> = Vec::new();
    let mut owners: Vec<u32> = Vec::new();
    let mut found = MftRead::default();
    // Names and sizes in extension records, for their base records (rare: files with very many links or pieces).
    let mut extra: HashMap<u64, Vec<Record>> = HashMap::new();
    let mut number = 0u64;
    // Pieces of 8 MB (whole records), read ahead on a thread of their own while this one parses each record and adds
    // its entries, in record order. (Parsing the pieces' records on every core instead took 7 times the
    // processor time, 67 s against 9.5, for the same 9.75M records: the cores waited on each other and on the one
    // adding entries. Reading ahead keeps the disk busy while parsing, which was the point.)
    let chunk = (8 << 20) / record_size * record_size;
    let mut pieces: Vec<(u64, usize)> = Vec::new();
    for (offset, bytes) in extents {
        let mut done = 0u64;
        while done < bytes {
            let want = ((bytes - done) as usize).min(chunk);
            pieces.push((offset + done, want));
            done += want as u64;
        }
    }
    // One slot per record, made at once (the MFT says how many it holds).
    let total = pieces.iter().map(|&(_, bytes)| bytes / record_size).sum::<usize>();
    if index.by_record.len() < total {
        index.by_record.resize(total, NONE);
    }
    let volume: &mut (dyn ReadSeek + Send) = volume;
    std::thread::scope(|scope| -> std::io::Result<()> {
        // Two pieces read ahead at most; buffers come back to be read into again.
        let (full_tx, full_rx) = std::sync::mpsc::sync_channel::<std::io::Result<Vec<u8>>>(2);
        let (spare_tx, spare_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let pieces = &pieces;
        scope.spawn(move || {
            for &(offset, want) in pieces {
                let mut buffer = spare_rx.try_recv().unwrap_or_else(|_| Vec::with_capacity(chunk));
                buffer.resize(want, 0);
                let read = volume.seek(SeekFrom::Start(offset)).and_then(|_| volume.read_exact(&mut buffer)).map(|()| buffer);
                let failed = read.is_err();
                if full_tx.send(read).is_err() || failed {
                    return;
                }
            }
        });
        for _ in 0..pieces.len() {
            let mut buffer = full_rx.recv().map_err(|_| bad("the volume's reader stopped"))??;
            found.bytes += buffer.len() as u64;
            for rec in buffer.chunks_exact_mut(record_size) {
                let n = number;
                number += 1;
                found.records += 1;
                if rec[0..4] != *b"FILE" {
                    continue;
                }
                if !fixup(rec, sector_size) {
                    found.torn += 1;
                    continue;
                }
                let Some(record) = parse_record(rec) else { continue };
                found.in_use += 1;
                if record.base != 0 {
                    extra.entry(record.base).or_default().push(record);
                    continue;
                }
                // NTFS's own files ($MFT, $LogFile…) and the root itself, which is the drive's entry.
                if n < FIRST_USER_RECORD {
                    if n == ROOT_RECORD {
                        set_record(index, n, root);
                    }
                    continue;
                }
                add_names(index, &mut parents, &mut owners, &mut found, n, &record, &record);
            }
            let _ = spare_tx.send(buffer);
        }
        Ok(())
    })?;
    // What extension records add: more names (with their base's facts), or the size when the base hadn't it.
    for (base, records) in extra {
        let Some(&entry) = index.by_record.get(base as usize).filter(|&&e| e != NONE) else { continue };
        let facts = index.meta(entry);
        for record in records {
            if let Some(size) = record.size.filter(|_| facts.flags & DIR == 0) {
                // Every entry of the base record: its names went in one after the other.
                let mut e = entry as usize;
                while e < index.len() && owners.get(e - first) == Some(&(base as u32)) {
                    index.set_facts(e as u32, Some(size), facts.modified);
                    e += 1;
                }
            }
            let start = index.len();
            let names = Record { names: record.names.clone(), ..Record::default() };
            add_names(index, &mut parents, &mut owners, &mut found, base, &Record::default(), &names);
            let size = index.size(entry);
            for e in start..index.len() {
                index.set_facts(e as u32, size, facts.modified);
                index.set_flags(e as u32, facts.flags);
            }
        }
    }
    // Parents can come after their children, so they're joined once everything is in. A name whose folder isn't
    // there ($Extend's files, or a folder that was being deleted) goes, with whatever is inside it.
    for (n, parent) in parents.into_iter().enumerate() {
        let entry = (first + n) as u32;
        let at = parent as usize;
        let folder = if parent == ROOT_RECORD { root } else { index.by_record.get(at).copied().unwrap_or(NONE) };
        index.parents.set(entry as usize, folder);
        if folder == NONE || parent == EXTEND_RECORD {
            index.parents.set(entry as usize, root);
            index.remove(entry);
        }
    }
    index.forget_joined_roots();
    index.settle();
    Ok(found)
}

pub(crate) fn set_record(index: &mut Index, record: u64, entry: u32) {
    let at = record as usize;
    if index.by_record.len() <= at {
        index.by_record.resize(at + 1, NONE);
    }
    if index.by_record[at] == NONE {
        index.by_record[at] = entry;
    }
}

fn add_names(index: &mut Index, parents: &mut Vec<u64>, owners: &mut Vec<u32>, found: &mut MftRead, number: u64, facts: &Record,
             names: &Record) {
    let mut flags = if facts.is_dir { DIR } else { 0 };
    if facts.attributes & FILE_ATTRIBUTE_HIDDEN != 0 { flags |= HIDDEN; }
    if facts.attributes & FILE_ATTRIBUTE_SYSTEM != 0 { flags |= SYSTEM; }
    if facts.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 { flags |= REPARSE; }
    let meta = Meta {
        size: if facts.is_dir { None } else { Some(facts.size.unwrap_or(0)) },
        modified: facts.written.map_or(0, crate::store::seconds),
        flags,
    };
    let more = crate::store::More {
        created: facts.created.map_or(0, crate::store::seconds),
        accessed: facts.accessed.map_or(0, crate::store::seconds),
        attributes: facts.attributes,
    };
    for (parent, name) in &names.names {
        // Parents are joined at the end: for now, none.
        let entry = index.push(NONE, &crate::walk::from_wide(name), meta);
        index.set_more(entry, more);
        parents.push(*parent);
        owners.push(number as u32);
        set_record(index, number, entry);
        found.names += 1;
    }
}

/// Read and seek, as one object (for reading the volume on another thread).
pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

/// A volume opened for raw reading: reads go through Windows in whole sectors.
pub struct RawVolume {
    handle: HANDLE,
    at: u64,
}

// SAFETY: a file handle can be used from any thread; one thread at a time reads through it.
unsafe impl Send for RawVolume {}

impl RawVolume {
    /// "C:", or the Windows error (5, access denied, when not elevated).
    pub fn open(drive: &str) -> Result<RawVolume, u32> {
        let path: Vec<u16> = format!(r"\\.\{}", drive.trim_end_matches('\\')).encode_utf16().chain(Some(0)).collect();
        let handle = unsafe {
            CreateFileW(path.as_ptr(), GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(), OPEN_EXISTING,
                        0, std::ptr::null_mut())
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(unsafe { GetLastError() });
        }
        Ok(RawVolume { handle, at: 0 })
    }
}

impl Drop for RawVolume {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

impl Read for RawVolume {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use windows_sys::Win32::Storage::FileSystem::{ReadFile, SetFilePointerEx, FILE_BEGIN};
        // Raw reads must start and end on sector boundaries: read whole sectors into an aligned buffer.
        let start = self.at / 4096 * 4096;
        let skip = (self.at - start) as usize;
        let len = (skip + buf.len()).div_ceil(4096) * 4096;
        let mut aligned = vec![0u64; len / 8];
        let mut got = 0u32;
        let ok = unsafe {
            SetFilePointerEx(self.handle, start as i64, std::ptr::null_mut(), FILE_BEGIN) != 0
                && ReadFile(self.handle, aligned.as_mut_ptr() as *mut u8, len as u32, &mut got, std::ptr::null_mut()) != 0
        };
        if !ok {
            return Err(std::io::Error::last_os_error());
        }
        let bytes = unsafe { std::slice::from_raw_parts(aligned.as_ptr() as *const u8, got as usize) };
        let n = bytes.len().saturating_sub(skip).min(buf.len());
        buf[..n].copy_from_slice(&bytes[skip..skip + n]);
        self.at += n as u64;
        Ok(n)
    }
}

impl Seek for RawVolume {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        match to {
            SeekFrom::Start(at) => self.at = at,
            SeekFrom::Current(by) => self.at = (self.at as i64 + by) as u64,
            SeekFrom::End(_) => return Err(std::io::Error::other("a volume has no end to seek from")),
        }
        Ok(self.at)
    }
}

/// A volume image for tests: a boot sector and an MFT of 1 KB records at cluster 4 (4 KB clusters).
pub mod image {
    use super::*;

    pub const CLUSTER: usize = 4096;
    pub const RECORD: usize = 1024;
    const MFT_LCN: usize = 4;

    pub struct File<'a> {
        pub number: u64,
        pub dir: bool,
        /// (parent record, name, namespace)
        pub names: Vec<(u64, &'a str, u8)>,
        pub size: Option<u64>,
        pub resident: bool,
        pub written: i64,
        pub attributes: u32,
        /// An extension record of this base record.
        pub base: u64,
    }

    fn attribute(kind: u32, body: Vec<u8>, non_resident: bool) -> Vec<u8> {
        let mut a = vec![0u8; 24];
        a[0..4].copy_from_slice(&kind.to_le_bytes());
        if non_resident {
            a.resize(64, 0);
            a.extend_from_slice(&body);
        } else {
            a[16..20].copy_from_slice(&(body.len() as u32).to_le_bytes());
            a[20..22].copy_from_slice(&24u16.to_le_bytes());
            a.extend_from_slice(&body);
        }
        while a.len() % 8 != 0 {
            a.push(0);
        }
        let len = a.len() as u32;
        a[4..8].copy_from_slice(&len.to_le_bytes());
        a[8] = non_resident as u8;
        a
    }

    /// A non-resident `$DATA` of `size` bytes in `runs`.
    pub fn data_attribute(size: u64, runs: &[u8]) -> Vec<u8> {
        let mut a = attribute(ATTR_DATA, runs.to_vec(), true);
        a[16..24].copy_from_slice(&0u64.to_le_bytes());
        a[32..34].copy_from_slice(&64u16.to_le_bytes());
        a[48..56].copy_from_slice(&size.to_le_bytes());
        a
    }

    /// A record of `RECORD` bytes with its update sequence applied (as NTFS writes it).
    pub fn record(file: &File, extra: Option<Vec<u8>>) -> Vec<u8> {
        let mut r = vec![0u8; RECORD];
        r[0..4].copy_from_slice(b"FILE");
        // Update sequence at 48: the check value, then one saved pair per 512-byte sector.
        r[4..6].copy_from_slice(&48u16.to_le_bytes());
        r[6..8].copy_from_slice(&3u16.to_le_bytes());
        r[20..22].copy_from_slice(&56u16.to_le_bytes());
        r[22..24].copy_from_slice(&(1u16 | if file.dir { 2 } else { 0 }).to_le_bytes());
        r[32..40].copy_from_slice(&file.base.to_le_bytes());
        let mut at = 56;
        let mut put = |a: Vec<u8>, r: &mut Vec<u8>| {
            r[at..at + a.len()].copy_from_slice(&a);
            at += a.len();
        };
        if file.base == 0 {
            let mut si = vec![0u8; 48];
            si[8..16].copy_from_slice(&file.written.to_le_bytes());
            si[32..36].copy_from_slice(&file.attributes.to_le_bytes());
            put(attribute(ATTR_STANDARD_INFORMATION, si, false), &mut r);
        }
        for &(parent, name, namespace) in &file.names {
            let wide: Vec<u16> = name.encode_utf16().collect();
            let mut fn_ = vec![0u8; 66];
            fn_[0..8].copy_from_slice(&(parent | 1 << 48).to_le_bytes());
            fn_[64] = wide.len() as u8;
            fn_[65] = namespace;
            for c in wide {
                fn_.extend_from_slice(&c.to_le_bytes());
            }
            put(attribute(ATTR_FILE_NAME, fn_, false), &mut r);
        }
        if let Some(size) = file.size {
            if file.resident {
                put(attribute(ATTR_DATA, vec![7u8; size as usize], false), &mut r);
            } else {
                put(data_attribute(size, &[0x11, 1, 40, 0]), &mut r);
            }
        }
        if let Some(extra) = extra {
            put(extra, &mut r);
        }
        put(ATTR_END.to_le_bytes().to_vec(), &mut r);
        let used = (at as u32).max(64);
        r[24..28].copy_from_slice(&used.to_le_bytes());
        r[28..32].copy_from_slice(&(RECORD as u32).to_le_bytes());
        // The update sequence: each sector's last two bytes saved, then replaced by the check value.
        let check = [0x34u8, 0x12];
        r[48..50].copy_from_slice(&check);
        for i in 1..=2 {
            let end = i * 512 - 2;
            let saved = [r[end], r[end + 1]];
            r[48 + 2 * i..50 + 2 * i].copy_from_slice(&saved);
            r[end..end + 2].copy_from_slice(&check);
        }
        r
    }

    /// The volume: record 0 is the MFT, whose `$DATA` runs over `count` records from cluster 4.
    pub fn volume(files: &[File], count: usize) -> Vec<u8> {
        let mft_bytes = count * RECORD;
        let mut v = vec![0u8; MFT_LCN * CLUSTER + mft_bytes + CLUSTER];
        v[3..11].copy_from_slice(b"NTFS    ");
        v[0x0B..0x0D].copy_from_slice(&512u16.to_le_bytes());
        v[0x0D] = (CLUSTER / 512) as u8;
        v[0x30..0x38].copy_from_slice(&(MFT_LCN as u64).to_le_bytes());
        v[0x40] = (-10i8) as u8;
        let clusters = mft_bytes.div_ceil(CLUSTER);
        let c = (clusters as u32).to_le_bytes();
        let runs = [0x14, c[0], c[1], c[2], c[3], MFT_LCN as u8, 0];
        let mft = File { number: 0, dir: false, names: vec![(5, "$MFT", 3)], size: None, resident: false, written: 0, attributes: 6, base: 0 };
        let record0 = record(&mft, Some(data_attribute(mft_bytes as u64, &runs)));
        let at = MFT_LCN * CLUSTER;
        v[at..at + RECORD].copy_from_slice(&record0);
        for file in files {
            let r = record(file, None);
            let at = MFT_LCN * CLUSTER + file.number as usize * RECORD;
            v[at..at + RECORD].copy_from_slice(&r);
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::image::*;
    use super::*;
    use crate::store::DEAD;

    #[test]
    fn runs() {
        // 0x18 clusters at 0x5634, then 0x10 at 0x5634 - 0x10 (a negative jump), then a sparse run of 4.
        let runs = [0x21, 0x18, 0x34, 0x56, 0x11, 0x10, 0xF0, 0x01, 0x04, 0x00];
        assert_eq!(data_runs(&runs), Some(vec![(Some(0x5634), 0x18), (Some(0x5624), 0x10), (None, 4)]));
        assert_eq!(data_runs(&[0x00]), Some(vec![]));
        assert_eq!(data_runs(&[0x21, 0x18]), None);
    }

    #[test]
    fn a_volume_from_its_mft() {
        let written = 133_000_000_000_000_000i64; // 2022
        let file = |number, dir, names: Vec<(u64, &'static str, u8)>, size: Option<u64>| File {
            number, dir, names, size, resident: size.is_some_and(|s| s < 100), written, attributes: 0, base: 0 };
        let files = vec![
            file(5, true, vec![(5, ".", 3)], None),
            // $Extend and a file in it: NTFS's own, left out.
            file(11, true, vec![(5, "$Extend", 3)], None),
            file(24, false, vec![(11, "$UsnJrnl", 3)], Some(10)),
            file(30, true, vec![(5, "Users", 3)], None),
            file(31, false, vec![(30, "notes.txt", 1), (30, "NOTES~1.TXT", 2)], Some(42)),
            // A hard link: two names in two folders.
            file(32, false, vec![(30, "a.bin", 3), (33, "same.bin", 3)], Some(1 << 20)),
            file(33, true, vec![(30, "Other", 3)], None),
            File { attributes: FILE_ATTRIBUTE_HIDDEN, ..file(34, false, vec![(33, "hidden.ini", 3)], Some(5)) },
            // A name whose folder isn't there.
            file(35, false, vec![(999, "orphan.txt", 3)], Some(1)),
            // Its size and a second name in an extension record.
            file(40, false, vec![(30, "big.iso", 3)], None),
            File { base: 40, ..file(41, false, vec![(33, "link.iso", 3)], Some(5 << 30)) },
        ];
        let mut bytes = volume(&files, 64);
        let mut index = Index::new();
        let read = raw(&mut index, &mut std::io::Cursor::new(&mut bytes), "C:").unwrap();
        // Only the drive is a root (every entry went in without its folder, joined afterwards).
        assert_eq!(index.roots().len(), 1);
        index.build_table();
        let mut paths: Vec<String> = (0..index.len() as u32).filter(|&e| index.flags[e as usize] & DEAD == 0).map(|e| index.path(e)).collect();
        paths.sort();
        assert_eq!(paths, ["C:", r"C:\Users", r"C:\Users\Other", r"C:\Users\Other\hidden.ini", r"C:\Users\Other\link.iso",
                           r"C:\Users\Other\same.bin", r"C:\Users\a.bin", r"C:\Users\big.iso", r"C:\Users\notes.txt"]);
        for path in [r"C:\Users\big.iso", r"C:\Users\Other\link.iso"] {
            assert_eq!(index.size(crate::search::resolve(&index, path).unwrap()), Some(5 << 30), "{path}");
        }
        let notes = crate::search::resolve(&index, r"C:\Users\notes.txt").unwrap();
        assert_eq!(index.size(notes), Some(42));
        assert_eq!(index.modified(notes), crate::store::seconds(written));
        let same = crate::search::resolve(&index, r"C:\Users\Other\same.bin").unwrap();
        assert_eq!(index.size(same), Some(1 << 20));
        let hidden = crate::search::resolve(&index, r"C:\Users\Other\hidden.ini").unwrap();
        assert!(index.flags[hidden as usize] & HIDDEN != 0);
        assert!(read.in_use >= 9);
    }

    #[test]
    fn torn_records_are_skipped() {
        let f = File { number: 20, dir: false, names: vec![(5, "x.txt", 3)], size: Some(3), resident: true, written: 0, attributes: 0, base: 0 };
        let mut r = image::record(&f, None);
        assert!(fixup(&mut r.clone(), 512));
        r[510] ^= 0xFF;
        assert!(!fixup(&mut r, 512));
        let mut ok = image::record(&f, None);
        assert!(fixup(&mut ok, 512));
        let parsed = parse_record(&ok).unwrap();
        assert_eq!(parsed.names, vec![(5, "x.txt".encode_utf16().collect::<Vec<_>>())]);
        assert_eq!(parsed.size, Some(3));
    }
}
