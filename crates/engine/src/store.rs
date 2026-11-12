//! The index: each distinct name once in one byte arena, and flat arrays beside it. Per entry that's 15 bytes (its
//! name's id and parent, 3 each while they fit (`ids`), size, date modified, flags and extension), its dates made and opened as codes against its date
//! modified (`coded`: about 2.4 and 3 bytes on a PC of 9.2M entries) and its attributes as a byte, plus about 4.7 for the
//! (folder, name) table; per distinct name, its UTF-8, a terminator and where it starts. No pointers, no per-entry allocation, no path strings (a path is its
//! parent chain).
//!
//! - **Each name once**: of 9.5M entries on one PC, 2.8M names are distinct (39.6% of the bytes), so the
//!   arena is 87 MB rather than 219, and a scan reads that much: it finds the names that match into a bitset, then
//!   one pass over the entries' name ids finds the entries.
//! - **Names in name order:** the distinct names are kept sorted (by their folded form, then their bytes), so two ids
//!   compare as their names do and no per-entry order is kept for sorting by name. Names that came since compare by
//!   their bytes, until the next sort (`sort_names`).
//! - **Sizes** take 3 bytes: 99.9% of files are under 16 MB (on that PC, the 10,800 larger go in a map), and
//!   folders have none (13.6% of entries). **Dates** take 4, as seconds since 1980 (up to 2116; the 3,998 older ones
//!   go in a map): 442,000 different dates among 9.2M entries, too many for a table of them to pay.
//! - **Each entry's extension** has a byte: one of the 254 most common, or "another" (checked by name), or none. So
//!   `ext:` scans a 1-byte column instead of the names.
//!
//! Entries only ever append. A delete marks an entry dead; a rename appends a new entry and the old one forwards to
//! it, so its children still find their folder. `compact` rebuilds without the dead entries (and names no one has).

use crate::coded::{Coded, Palette};
use crate::column::{Column, Mapping};
use crate::ids::Ids;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub const NONE: u32 = u32::MAX;

pub const DIR: u8 = 1;
pub const HIDDEN: u8 = 2;
pub const SYSTEM: u8 = 4;
pub const REPARSE: u8 = 8;
/// Gone: deleted, or renamed (then `parents` holds the entry that replaced it).
pub const DEAD: u8 = 16;
pub const FORWARD: u8 = 32;
/// The name has bytes beyond ASCII: it's searched in `folded`, not `names`.
pub const WIDE: u8 = 64;

/// Extension bytes: none, or "another" one than the table's (its name says which).
pub const EXT_NONE: u8 = 0;
pub const EXT_OTHER: u8 = 255;
const EXT_MAX: usize = 254;
/// Extensions longer than this are "another" (they're rare, and mostly not extensions at all).
const EXT_LONGEST: usize = 16;

const SIZE_UNKNOWN: u32 = 0xFF_FFFF;
const SIZE_BIG: u32 = 0xFF_FFFE;
const DATE_ODD: u32 = u32::MAX;
/// 1980-01-01 in seconds since 1970: dates are kept as seconds since then.
const DATE_BASE: i64 = 315_532_800;

const EMPTY: u32 = u32::MAX;
const TOMB: u32 = u32::MAX - 1;

#[derive(Default)]
pub struct Index {
    /// Each distinct name followed by a 0 byte; the arena starts with a 0 too, so "\0name\0" finds a whole name.
    pub names: Column<u8>,
    /// Where each distinct name starts in `names`, and one more: where the next one would.
    starts: Column<u32>,
    /// Each entry's name, as an index into `starts` (3 bytes each while all fit: `ids`).
    pub name_of: Ids,
    /// Names with ids below this are in name order (`name_order`): their ids compare as they do.
    pub sorted: u32,
    /// The ids of the names added since they were last sorted, by their exact bytes (open addressing; grown past 85%
    /// full). Sorted names need no table: they're found by halving (`find_sorted`). Not kept in the file (15 MB less
    /// on disk); made again on opening, from the few names since.
    name_slots: Column<u32>,
    /// Each entry's folder (`NONE` for a root), or for one renamed the entry that took its place (3 bytes, as above).
    pub parents: Ids,
    /// Three bytes each (`size24`): `SIZE_UNKNOWN`, `SIZE_BIG` (in `big`), or the size.
    sizes: Column<u8>,
    /// Seconds since 1980 (`DATE_ODD`: in `odd`).
    dates: Column<u32>,
    pub flags: Column<u8>,
    /// When each entry was made and last opened (seconds since 1980, 0 when not known), and all its attributes
    /// (Windows' FILE_ATTRIBUTE_* bits, the low 16): `dc:`, `da:` and `attrib:` answered from the index, where
    /// Everything (which doesn't keep them unless told to) reads every file's on the disk. v6: as codes (`coded`),
    /// the date made against the date modified (or not known), the date opened against the date modified or made.
    created: Coded,
    accessed: Coded,
    attrs: Palette,
    /// Opened from a v5 file, which is only read now: written as v6 at the next save, never saved as it is.
    from_v5: bool,
    /// Whether every entry's dates made and opened and attributes have been read (an index saved without them gets
    /// them at its next full listing).
    pub times_filled: bool,
    /// Each entry's extension: `EXT_NONE`, an id in `ext_names` (1 to 254), or `EXT_OTHER`.
    pub exts: Column<u8>,
    ext_names: Vec<Vec<u8>>,
    ext_ids: HashMap<Vec<u8>, u8>,
    /// Live entries with each extension byte.
    ext_counts: Vec<u32>,
    /// Entries without a parent: drives, or folders walked on their own.
    roots: Vec<u32>,
    /// Sizes of 4 GB and more, and dates before 1980 or from 2116: rare enough for maps.
    big: HashMap<u32, u64>,
    odd: HashMap<u32, i64>,
    /// File reference number (its MFT record, the low 48 bits) to entry, when the index came from the MFT.
    pub by_record: Column<u32>,
    pub dead: usize,
    /// Distinct names beyond ASCII once more, folded (see `fold`), each followed by a 0, so "cafe" finds "Café" and
    /// "CAFE\u{301}".
    pub folded: Vec<u8>,
    /// For each folded name: where it starts in `folded`, and its name's id (both ascending).
    pub folded_at: Vec<(u32, u32)>,
    /// Each entry's place in path order (folders before their contents, siblings by name); stale once a folder is
    /// renamed or moved (`path_stale`).
    pub path_rank: Vec<u32>,
    pub path_ranked: usize,
    pub path_stale: bool,
    /// (folder, name) → entry, open addressing, keyed by the folder's `origin`.
    slots: Column<u32>,
    slots_used: usize,
    slots_tomb: usize,
    /// A renamed folder's new entry → the entry it started as, so its children (which point at the old one) and new
    /// children (which point at the new one) hash alike.
    origins: HashMap<u32, u32>,
    /// Folders deleted since `settle` last marked their contents gone, and the lowest of them.
    pending_dirs: usize,
    pending_low: u32,
    /// The lowest entry whose folder (followed through renames) comes after it, once known (`None`: not worked out
    /// since loading). Entries are otherwise after their folders, so what's inside a deleted folder is all after it
    /// or after this, and `settle` looks only from there: a folder made and deleted a moment ago costs a few entries,
    /// not a pass over millions.
    low_water: Option<u32>,
    /// How often each byte comes in names (ASCII lowercased), sampled once: which word of a query is rarest.
    frequency: std::sync::OnceLock<Box<[u32; 256]>>,
    /// The saved file the arrays are mapped from (`open`), if they are; and whether it's as the file says (its
    /// "saved cleanly" mark is on: nothing has changed since `flush`).
    map: Option<Arc<Mapping>>,
    clean: AtomicBool,
    /// Changes under way (nested): while there are any, the mapped file's busy mark is on (`Busy`).
    busy: std::sync::atomic::AtomicUsize,
    /// What the small part held when it was last written to the mapped file (`publish`): its sizes, and how many times
    /// a size or date kept aside changed.
    published: std::sync::atomic::AtomicU64,
    kept_aside: u64,
    /// How many changes have been made (to tell whether any came while a copy was written).
    edits: std::sync::atomic::AtomicU64,
    /// When the small part may next be written to the mapped file (ms since 1970).
    small_written: std::sync::atomic::AtomicU64,
}

/// A change under way on a mapped index: its busy mark is on until the last one ends. A file whose clean mark is off
/// but whose busy mark is too was left between changes (Excavate ended without saving, Windows still running and
/// writing its pages): it can be trusted (`from_v5`).
struct Busy {
    mark: *mut u8,
    depth: *const std::sync::atomic::AtomicUsize,
    index: *const Index,
}

impl Drop for Busy {
    fn drop(&mut self) {
        if !self.mark.is_null() {
            // SAFETY: both point into the index (and its mapping) borrowed by the change, which outlives this.
            unsafe {
                if (*self.depth).fetch_sub(1, std::sync::atomic::Ordering::SeqCst) == 1 {
                    (*self.index).publish(false);
                    std::ptr::write_volatile(self.mark, 0);
                }
            }
        }
    }
}

/// When Windows last started, in seconds since 1970 (to within a second or two).
fn booted_at() -> u64 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    // SAFETY: no arguments.
    let up = unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() } / 1000;
    now.saturating_sub(up)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meta {
    pub size: Option<u64>,
    /// Seconds since 1970; negative before.
    pub modified: i64,
    pub flags: u8,
}

/// What else a folder listing says of an entry: kept for `dc:`, `da:` and `attrib:`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct More {
    /// Seconds since 1970 (0: not known).
    pub created: i64,
    pub accessed: i64,
    /// Windows' FILE_ATTRIBUTE_* bits.
    pub attributes: u32,
}

/// Attribute bits from the flags the index always kept (what an entry has before its listing says more).
pub fn attributes_of(flags: u8) -> u16 {
    let mut bits = 0u16;
    if flags & DIR != 0 { bits |= 0x10; }
    if flags & HIDDEN != 0 { bits |= 0x2; }
    if flags & SYSTEM != 0 { bits |= 0x4; }
    if flags & REPARSE != 0 { bits |= 0x400; }
    bits
}

/// A time kept in 4 bytes: seconds since 1980 (earlier ones as 1980, 0 as not known).
fn pack_time(seconds: i64) -> u32 {
    if seconds <= 0 { 0 } else { (seconds - DATE_BASE).clamp(1, u32::MAX as i64) as u32 }
}

/// The distinct names in name order, made from a shared borrow (`sorted_names`) and installed by `set_sorted`.
pub struct Sorted {
    names: Vec<u8>,
    starts: Vec<u32>,
    folded: Vec<u8>,
    folded_at: Vec<(u32, u32)>,
    name_slots: Vec<u32>,
    /// Each old id's new one.
    map: Vec<u32>,
    /// How many names there were when it was made (names added since are kept, after the sorted ones), and how many
    /// of them an entry had.
    count: usize,
    kept: usize,
}

impl Index {
    pub fn new() -> Index {
        Index { names: vec![0].into(), starts: vec![1].into(), folded: vec![0], ext_counts: vec![0; 256], times_filled: true, ..Default::default() }
    }

    pub fn len(&self) -> usize {
        self.name_of.len()
    }

    pub fn is_empty(&self) -> bool {
        self.name_of.is_empty()
    }

    pub fn live(&self) -> usize {
        self.len() - self.dead
    }

    /// How many distinct names there are.
    pub fn name_count(&self) -> usize {
        self.starts.len() - 1
    }

    /// Names that came since they were last sorted.
    pub fn unsorted(&self) -> usize {
        self.name_count() - self.sorted as usize
    }

    /// Adds an entry. The (folder, name) table is updated unless it hasn't been built yet (`build_table` builds it
    /// once, after a walk).
    pub fn push(&mut self, parent: u32, name: &str, meta: Meta) -> u32 {
        let index = self.push_raw(parent, name, meta);
        if !self.slots.is_empty() {
            self.slot_insert(index);
        }
        index
    }

    fn push_raw(&mut self, parent: u32, name: &str, meta: Meta) -> u32 {
        let _busy = self.changing();
        // Grow by a thirty-second, not by doubling: a live index at 9M entries would otherwise jump by hundreds of MB
        // the moment one entry too many arrives (an eighth held 45 MB more after one change; this, 11 MB, and a copy
        // of tens of milliseconds every ~300,000 new entries).
        if self.name_of.len() == self.name_of.capacity() {
            let more = (self.name_of.len() / 32).max(4096);
            self.name_of.reserve_exact(more);
            self.parents.reserve_exact(more);
            self.sizes.reserve_exact(more * 3);
            self.dates.reserve_exact(more);
            self.created.reserve(more);
            self.accessed.reserve(more);
            self.attrs.codes.reserve_exact(more);
            self.flags.reserve_exact(more);
            self.exts.reserve_exact(more);
        }
        let index = self.name_of.len() as u32;
        let id = self.intern(name);
        self.name_of.push(id);
        let mut flags = meta.flags & !(WIDE | DEAD | FORWARD);
        if !name.is_ascii() {
            flags |= WIDE;
        }
        self.parents.push(parent);
        let size = pack_size(meta.size, index, &mut self.big);
        self.sizes.extend_from_slice(&size.to_le_bytes()[..3]);
        let date = pack_date(meta.modified, index, &mut self.odd);
        self.dates.push(date);
        self.created.push(0);
        self.accessed.push(0);
        self.attrs.push(attributes_of(flags));
        self.flags.push(flags);
        let ext = self.ext_of(name.as_bytes(), true);
        self.exts.push(ext);
        self.ext_counts[ext as usize] += 1;
        if parent == NONE {
            self.roots.push(index);
        }
        index
    }

    /// The id of `name`, added to the arena (and, beyond ASCII, to `folded`) when it's new.
    fn intern(&mut self, name: &str) -> u32 {
        let bytes = name.as_bytes();
        if let Some(id) = self.find_sorted(bytes) {
            return id;
        }
        if (self.unsorted() + 1) * 100 > self.name_slots.len() * 85 {
            self.name_slots = slots_for(&self.names, &self.starts, self.sorted as usize, (self.unsorted() + 1) * 100 / 60).into();
        }
        let mut at = slot_of(name_hash(bytes), self.name_slots.len());
        loop {
            let v = self.name_slots[at];
            if v == EMPTY {
                break;
            }
            if self.id_bytes(v) == bytes {
                return v;
            }
            at += 1;
            if at == self.name_slots.len() {
                at = 0;
            }
        }
        if self.names.capacity() - self.names.len() < bytes.len() + 1 {
            self.names.reserve_exact((self.names.len() / 32).max(bytes.len() + 1 + 65536));
        }
        if self.starts.len() == self.starts.capacity() {
            self.starts.reserve_exact((self.starts.len() / 32).max(4096));
        }
        let id = self.name_count() as u32;
        self.names.extend_from_slice(bytes);
        self.names.push(0);
        self.starts.push(self.names.len() as u32);
        if !name.is_ascii() {
            self.folded_at.push((self.folded.len() as u32, id));
            self.folded.extend_from_slice(fold(name).as_bytes());
            self.folded.push(0);
        }
        self.name_slots[at] = id;
        id
    }

    /// The extension byte for a name; `assign` gives a new extension the next free id.
    fn ext_of(&mut self, name: &[u8], assign: bool) -> u8 {
        let Some(ext) = extension(name) else { return EXT_NONE };
        if ext.len() > EXT_LONGEST {
            return EXT_OTHER;
        }
        let key = ext_key(ext);
        if let Some(&id) = self.ext_ids.get(&key) {
            return id;
        }
        if !assign || self.ext_names.len() >= EXT_MAX {
            return EXT_OTHER;
        }
        self.ext_names.push(key.clone());
        let id = self.ext_names.len() as u8;
        self.ext_ids.insert(key, id);
        id
    }

    /// Live entries whose extension byte is `id`.
    pub fn ext_count(&self, id: u8) -> u32 {
        self.ext_counts[id as usize]
    }

    /// The entries without a parent: drives, or folders walked on their own.
    pub fn roots(&self) -> &[u32] {
        &self.roots
    }

    /// Entries added without their folder and given it afterwards (the MFT's, joined once all are in) are roots no
    /// more. (Left in, an index read from the MFT would keep every entry as a root, 36 MB more in its file.)
    pub fn forget_joined_roots(&mut self) {
        let parents = &self.parents;
        self.roots.retain(|&r| parents.get(r as usize) == NONE);
    }

    fn count_exts(&mut self) {
        use rayon::prelude::*;
        let counts = (0..self.len()).into_par_iter().fold(|| vec![0u32; 256], |mut counts, e| {
            if self.flags[e] & DEAD == 0 {
                counts[self.exts[e] as usize] += 1;
            }
            counts
        }).reduce(|| vec![0u32; 256], |mut a, b| {
            for (x, y) in a.iter_mut().zip(b) {
                *x += y;
            }
            a
        });
        self.ext_counts = counts;
    }

    /// The extensions with an id, in id order (id 1 first).
    pub fn ext_names(&self) -> &[Vec<u8>] {
        &self.ext_names
    }

    /// The id of an extension (as typed after `ext:`), if it has one: otherwise names with it are `EXT_OTHER`.
    pub fn ext_id(&self, ext: &str) -> Option<u8> {
        self.ext_ids.get(&ext_key(ext.as_bytes())).copied()
    }

    /// Gives the 254 most common extensions the ids, by one pass over the names. After a walk (ids then went first
    /// come, first served) and in `compact`.
    pub fn rank_exts(&mut self) {
        let _busy = self.changing();
        use rayon::prelude::*;
        let n = self.len();
        // Each distinct name's extension once, then counted by the entries that have it.
        let keys: Vec<Option<Vec<u8>>> = (0..self.name_count() as u32).into_par_iter()
            .map(|id| extension(self.id_bytes(id)).filter(|ext| ext.len() <= EXT_LONGEST).map(ext_key))
            .collect();
        // One pass (a counter per name for each of rayon's pieces would be millions each, thousands of times).
        let mut uses = vec![0u32; keys.len()];
        for e in 0..n {
            if self.flags[e] & DEAD == 0 {
                uses[self.name_of.get(e) as usize] += 1;
            }
        }
        let mut counts: HashMap<&[u8], usize> = HashMap::new();
        for (key, &used) in keys.iter().zip(&uses) {
            if let Some(key) = key.as_deref().filter(|_| used > 0) {
                *counts.entry(key).or_default() += used as usize;
            }
        }
        let mut ranked: Vec<(&[u8], usize)> = counts.into_iter().collect();
        ranked.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        ranked.truncate(EXT_MAX);
        self.ext_names = ranked.into_iter().map(|(ext, _)| ext.to_vec()).collect();
        self.ext_ids = self.ext_names.iter().enumerate().map(|(n, ext)| (ext.clone(), n as u8 + 1)).collect();
        let by_name: Vec<u8> = keys.par_iter().zip(0..keys.len() as u32).map(|(key, id)| match key {
            Some(key) => self.ext_ids.get(key).copied().unwrap_or(EXT_OTHER),
            None if extension(self.id_bytes(id)).is_some() => EXT_OTHER,
            None => EXT_NONE,
        }).collect();
        self.exts = self.name_of.par_iter().map(|id| by_name[id as usize]).collect::<Vec<u8>>().into();
        self.count_exts();
    }

    /// Where name `id` starts in `names`.
    pub fn id_start(&self, id: u32) -> usize {
        self.starts[id as usize] as usize
    }

    /// Name `id`'s length in bytes.
    pub fn id_len(&self, id: u32) -> usize {
        (self.starts[id as usize + 1] - self.starts[id as usize] - 1) as usize
    }

    /// Name `id`'s bytes.
    pub fn id_bytes(&self, id: u32) -> &[u8] {
        &self.names[self.starts[id as usize] as usize..self.starts[id as usize + 1] as usize - 1]
    }

    /// The name whose bytes (or terminator) hold `offset` of the arena.
    pub fn id_at(&self, offset: usize) -> u32 {
        (self.starts.partition_point(|&start| start as usize <= offset).max(1) - 1) as u32
    }

    /// The id of a name exactly as given, if any entry has it.
    pub fn id_of(&self, name: &[u8]) -> Option<u32> {
        if let Some(id) = self.find_sorted(name) {
            return Some(id);
        }
        if self.name_slots.is_empty() {
            return None;
        }
        let mut at = slot_of(name_hash(name), self.name_slots.len());
        loop {
            let v = self.name_slots[at];
            if v == EMPTY {
                return None;
            }
            if self.id_bytes(v) == name {
                return Some(v);
            }
            at += 1;
            if at == self.name_slots.len() {
                at = 0;
            }
        }
    }

    /// The id of a sorted name exactly as given: by halving over the sorted names, which are in order of their folded
    /// form (lowercase, or `fold` beyond ASCII), then their bytes (`sorted_names`). Some 23 steps for 9M names.
    fn find_sorted(&self, name: &[u8]) -> Option<u32> {
        if self.sorted == 0 {
            return None;
        }
        let wanted: std::borrow::Cow<[u8]> = if name.is_ascii() {
            name.to_ascii_lowercase().into()
        } else {
            fold(std::str::from_utf8(name).ok()?).into_bytes().into()
        };
        let order = |id: u32| -> Ordering {
            let bytes = self.id_bytes(id);
            let key: std::borrow::Cow<[u8]> = if !bytes.is_ascii() {
                self.id_folded(id).into()
            } else if bytes.iter().any(u8::is_ascii_uppercase) {
                bytes.to_ascii_lowercase().into()
            } else {
                bytes.into()
            };
            key.as_ref().cmp(wanted.as_ref()).then_with(|| bytes.cmp(name))
        };
        let (mut low, mut high) = (0u32, self.sorted);
        while low < high {
            let middle = low + (high - low) / 2;
            match order(middle) {
                Ordering::Less => low = middle + 1,
                Ordering::Greater => high = middle,
                Ordering::Equal => return Some(middle),
            }
        }
        None
    }

    /// Name `id` folded, for one beyond ASCII.
    pub fn id_folded(&self, id: u32) -> &[u8] {
        let n = self.folded_at.binary_search_by_key(&id, |&(_, name)| name).expect("a name beyond ASCII");
        let start = self.folded_at[n].0 as usize;
        let end = match self.folded_at.get(n + 1) {
            Some(&(next, _)) => next as usize - 1,
            None => self.folded.len() - 1,
        };
        &self.folded[start..end]
    }

    /// The length of entry `e`'s name in bytes.
    pub fn name_len(&self, e: u32) -> usize {
        self.id_len(self.name_of.get(e as usize))
    }

    /// Where entry `e`'s name starts in `names`.
    pub fn start(&self, e: u32) -> usize {
        self.id_start(self.name_of.get(e as usize))
    }

    pub fn name(&self, index: u32) -> &str {
        let bytes = self.name_bytes(index);
        // Names went in as &str.
        std::str::from_utf8(bytes).unwrap_or("")
    }

    pub fn name_bytes(&self, index: u32) -> &[u8] {
        self.id_bytes(self.name_of.get(index as usize))
    }

    /// The name's bytes when its start is already known (`usize::MAX`: not looked up yet).
    pub fn name_at(&self, index: u32, start: usize) -> &[u8] {
        if start == usize::MAX {
            return self.name_bytes(index);
        }
        &self.names[start..start + self.name_len(index)]
    }

    /// The folded name of a `WIDE` entry.
    pub fn folded_name(&self, index: u32) -> &[u8] {
        self.id_folded(self.name_of.get(index as usize))
    }

    /// New size and date for an entry that changed in place.
    pub fn set_facts(&mut self, index: u32, size: Option<u64>, modified: i64) {
        let _busy = self.changing();
        if self.big.contains_key(&index) || self.odd.contains_key(&index) {
            self.kept_aside += 1;
        }
        self.big.remove(&index);
        self.odd.remove(&index);
        let size = pack_size(size, index, &mut self.big);
        set24(&mut self.sizes, index as usize, size);
        let e = index as usize;
        let date = pack_date(modified, index, &mut self.odd);
        if date != self.dates[e] {
            // The dates made and opened stay as they were, whatever their codes stood for.
            let (made, opened) = (self.created_raw(e), self.accessed_raw(e));
            self.dates[e] = date;
            let modified = self.modified_raw(e);
            self.created.set(e, made, modified, Some(0));
            self.accessed.set(e, opened, modified, Some(made));
        }
    }

    /// The date modified as kept (seconds since 1980), when it can stand for another time (not one kept aside).
    fn modified_raw(&self, e: usize) -> Option<u32> {
        Some(self.dates[e]).filter(|&date| date != DATE_ODD)
    }

    /// The date made as kept (seconds since 1980, 0: not known).
    #[inline]
    fn created_raw(&self, e: usize) -> u32 {
        self.created.get(e, || self.dates[e], || 0)
    }

    #[inline]
    fn accessed_raw(&self, e: usize) -> u32 {
        self.accessed.get(e, || self.dates[e], || self.created_raw(e))
    }

    /// New attributes (hidden, system…) for an entry; what the index itself keeps (dead, wide…) stays.
    pub fn set_flags(&mut self, index: u32, flags: u8) {
        let _busy = self.changing();
        let keep = self.flags[index as usize] & (WIDE | DEAD | FORWARD);
        self.flags[index as usize] = keep | (flags & !(WIDE | DEAD | FORWARD));
    }

    /// What the folder listing said of an entry beyond its size, date and flags.
    pub fn set_more(&mut self, index: u32, more: More) {
        let _busy = self.changing();
        self.put_more(index, more);
    }

    fn put_more(&mut self, index: u32, more: More) {
        let e = index as usize;
        let (made, opened) = (pack_time(more.created), pack_time(more.accessed));
        let modified = self.modified_raw(e);
        self.created.set(e, made, modified, Some(0));
        self.accessed.set(e, opened, modified, Some(made));
        self.attrs.set(e, more.attributes as u16);
    }

    pub fn more(&self, index: u32) -> More {
        More { created: self.created(index), accessed: self.accessed(index), attributes: self.attributes(index) }
    }

    /// When it was made (seconds since 1970; 0 when not known).
    #[inline]
    pub fn created(&self, index: u32) -> i64 {
        match self.created_raw(index as usize) { 0 => 0, t => t as i64 + DATE_BASE }
    }

    /// When it was last opened (seconds since 1970; 0 when not known).
    #[inline]
    pub fn accessed(&self, index: u32) -> i64 {
        match self.accessed_raw(index as usize) { 0 => 0, t => t as i64 + DATE_BASE }
    }

    /// Its attributes (Windows' FILE_ATTRIBUTE_* bits, the low 16).
    #[inline]
    pub fn attributes(&self, index: u32) -> u32 {
        self.attrs.get(index as usize) as u32
    }

    /// Which entries were made (or, `opened`, last opened) in `from..=to` (seconds since 1970; one not known never
    /// is), a bit each, 64 to a word: the codes read in order, a word at a time (`coded::Cursor`), in parallel.
    pub fn times_in(&self, opened: bool, from: i64, to: i64) -> Vec<u64> {
        use rayon::prelude::*;
        let n = self.len();
        let (low, high) = (from - DATE_BASE, to - DATE_BASE);
        let mut out = vec![0u64; n.div_ceil(64)];
        const WORDS: usize = 256;
        const ZEROS: [u32; 32] = [0; 32];
        out.par_chunks_mut(WORDS).enumerate().for_each(|(chunk, words)| {
            let (mut made, mut seen) = ([0u32; 32], [0u32; 32]);
            for (w, word) in words.iter_mut().enumerate() {
                let mut bits = 0u64;
                // Two words of codes to a word of bits.
                for half in 0..2 {
                    let base = (chunk * WORDS + w) * 64 + half * 32;
                    if base >= n {
                        break;
                    }
                    let count = 32.min(n - base);
                    let modified = &self.dates[base..base + count];
                    self.created.fill(base / 32, modified, &ZEROS[..count], &mut made[..count]);
                    let raw: &[u32] = if opened {
                        self.accessed.fill(base / 32, modified, &made[..count], &mut seen[..count]);
                        &seen[..count]
                    } else {
                        &made[..count]
                    };
                    for (k, &raw) in raw.iter().enumerate() {
                        let raw = raw as i64;
                        bits |= ((raw != 0 && low <= raw && raw <= high) as u64) << (half * 32 + k);
                    }
                }
                *word = bits;
            }
        });
        out
    }

    /// Which entries have every one of `bits` among their attributes, a bit each, 64 to a word: each set in the table
    /// of attributes checked once, then the column of their places read as `ext:` reads extensions.
    pub fn attributes_with(&self, bits: u32) -> Vec<u64> {
        use rayon::prelude::*;
        let mut fits = [false; 256];
        for (code, &value) in self.attrs.values.iter().enumerate() {
            fits[code] = value as u32 & bits == bits;
        }
        let n = self.len();
        let mut out = vec![0u64; n.div_ceil(64)];
        out.par_iter_mut().enumerate().for_each(|(w, word)| {
            let base = w * 64;
            let mut found = 0u64;
            for (k, &code) in self.attrs.codes[base..(base + 64).min(n)].iter().enumerate() {
                let fit = if code == crate::coded::RARE { self.attrs.get(base + k) as u32 & bits == bits } else { fits[code as usize] };
                found |= (fit as u64) << k;
            }
            *word = found;
        });
        out
    }

    /// How many entries' dates made and opened are set aside since the file was last written (`coded`).
    pub fn times_set_aside(&self) -> (usize, usize) {
        (self.created.spilled(), self.accessed.spilled())
    }

    /// Every entry's dates made and opened and attributes have now been read.
    pub fn set_times_filled(&mut self) {
        let _busy = self.changing();
        self.times_filled = true;
    }


    /// Seconds since 1970.
    pub fn modified(&self, index: u32) -> i64 {
        match self.dates[index as usize] {
            DATE_ODD => self.odd.get(&index).copied().unwrap_or(0),
            seconds => seconds as i64 + DATE_BASE,
        }
    }

    pub fn size(&self, index: u32) -> Option<u64> {
        match get24(&self.sizes, index as usize) {
            SIZE_UNKNOWN => None,
            SIZE_BIG => self.big.get(&index).copied(),
            size => Some(size as u64),
        }
    }

    pub fn meta(&self, index: u32) -> Meta {
        Meta { size: self.size(index), modified: self.modified(index), flags: self.flags[index as usize] }
    }

    /// The entry that holds `index`'s place now: itself, or what it was renamed to.
    pub fn current(&self, mut index: u32) -> u32 {
        while self.flags[index as usize] & FORWARD != 0 {
            index = self.parents.get(index as usize);
        }
        index
    }

    pub fn parent(&self, index: u32) -> u32 {
        let parent = self.parents.get(index as usize);
        if parent == NONE { NONE } else { self.current(parent) }
    }

    fn origin(&self, index: u32) -> u32 {
        if index == NONE { NONE } else { self.origins.get(&index).copied().unwrap_or(index) }
    }

    pub fn path(&self, index: u32) -> String {
        let mut chain = vec![index];
        let mut at = self.parent(index);
        while at != NONE {
            chain.push(at);
            at = self.parent(at);
        }
        let mut path = String::new();
        for (n, &entry) in chain.iter().rev().enumerate() {
            if n > 0 && !path.ends_with('\\') {
                path.push('\\');
            }
            path.push_str(self.name(entry));
        }
        path
    }

    pub fn remove(&mut self, index: u32) {
        let _busy = self.changing();
        if self.flags[index as usize] & DEAD == 0 {
            if !self.slots.is_empty() {
                self.slot_remove(index);
            }
            if self.flags[index as usize] & DIR != 0 {
                self.pending_low = if self.pending_dirs == 0 { index } else { self.pending_low.min(index) };
                self.pending_dirs += 1;
            }
            self.flags[index as usize] |= DEAD;
            self.dead += 1;
            self.ext_counts[self.exts[index as usize] as usize] -= 1;
        }
    }

    /// Renamed or moved: a new entry with the same facts, the old one forwarding to it.
    pub fn rename(&mut self, index: u32, parent: u32, name: &str) -> u32 {
        let _busy = self.changing();
        let meta = self.meta(index);
        let is_dir = meta.flags & DIR != 0;
        let origin = self.origin(index);
        let more = self.more(index);
        let new = self.push_raw(parent, name, meta);
        self.put_more(new, more);
        if is_dir {
            self.origins.insert(new, origin);
            // The path order is wrong for what's inside: it's let go, and made again when a sort by path asks.
            self.path_stale = true;
            self.path_rank = Vec::new();
            self.path_ranked = 0;
            // What's inside it (after `origin`) now has a folder that comes after it.
            self.low_water = self.low_water.map(|low| low.min(origin));
        }
        // Out of the table while its name can still be hashed, then gone, then the new entry in.
        let table = !self.slots.is_empty();
        if table {
            self.slot_remove(index);
        }
        self.flags[index as usize] |= DEAD | FORWARD;
        self.dead += 1;
        self.ext_counts[self.exts[index as usize] as usize] -= 1;
        self.parents.set(index as usize, new);
        if table {
            self.slot_insert(new);
        }
        new
    }

    /// Whether `index` is gone, or inside something that is. (After `settle`, the DEAD flag alone answers this.)
    pub fn gone(&self, index: u32) -> bool {
        let mut at = index;
        loop {
            let now = self.current(at);
            if self.flags[now as usize] & DEAD != 0 {
                return true;
            }
            let parent = self.parents.get(now as usize);
            if parent == NONE {
                return false;
            }
            at = parent;
        }
    }

    /// Marks everything inside a deleted folder as gone, so a search can trust the DEAD flag alone. One pass over the
    /// index, and only after a folder was deleted.
    pub fn settle(&mut self) {
        if self.pending_dirs == 0 {
            return;
        }
        let _busy = self.changing();
        let low = match self.low_water {
            Some(low) => low,
            None => {
                let low = self.lowest_before_folder();
                self.low_water = Some(low);
                low
            }
        };
        let from = self.pending_low.min(low);
        self.pending_dirs = 0;
        let doomed = self.inside_from(from, |index, e| index.flags[e as usize] & DEAD != 0 && index.flags[e as usize] & FORWARD == 0);
        for e in doomed {
            self.remove(e);
        }
        self.pending_dirs = 0;
    }

    /// The lowest entry whose folder, followed through renames, comes after it (`len` when none does).
    fn lowest_before_folder(&self) -> u32 {
        (0..self.len() as u32).find(|&e| {
            let parent = self.parents.get(e as usize);
            self.flags[e as usize] & FORWARD == 0 && parent != NONE && self.current(parent) > e
        }).unwrap_or(self.len() as u32)
    }

    /// Every live entry with an ancestor (or itself, when it's live) for which `marked` holds, by one pass with
    /// each chain's verdict shared along it. (Forwarding entries are followed, never marked.)
    pub fn inside(&self, marked: impl Fn(&Index, u32) -> bool) -> Vec<u32> {
        self.inside_from(0, marked)
    }

    /// `inside`, looking only at entries from `from` on (and their folders).
    fn inside_from(&self, from: u32, marked: impl Fn(&Index, u32) -> bool) -> Vec<u32> {
        // 0 unknown, 1 outside, 2 inside.
        let mut state = vec![0u8; self.len()];
        let mut chain = Vec::new();
        let mut found = Vec::new();
        for e in from..self.len() as u32 {
            if state[e as usize] != 0 {
                continue;
            }
            chain.clear();
            let mut at = e;
            let verdict = loop {
                let s = state[at as usize];
                if s != 0 {
                    break s;
                }
                chain.push(at);
                let flags = self.flags[at as usize];
                if flags & FORWARD != 0 {
                    at = self.parents.get(at as usize);
                    continue;
                }
                if marked(self, at) {
                    break 2;
                }
                let parent = self.parents.get(at as usize);
                if parent == NONE {
                    break 1;
                }
                at = parent;
            };
            for &c in &chain {
                state[c as usize] = verdict;
                if verdict == 2 && self.flags[c as usize] & DEAD == 0 {
                    found.push(c);
                }
            }
        }
        found
    }

    /// The live child of `parent` called `name` (NTFS keeps a name's case, and two names in one folder never differ
    /// by case alone, so an exact match is tried first, then one ignoring case).
    pub fn child(&self, parent: u32, name: &str) -> Option<u32> {
        if self.slots.is_empty() {
            return self.child_by_scan(parent, name);
        }
        let origin = self.origin(parent);
        let mut at = slot_of(key_hash(origin, name.as_bytes()), self.slots.len());
        let mut loose = None;
        loop {
            let v = self.slots[at];
            if v == EMPTY {
                return loose;
            }
            if v != TOMB && self.origin(self.parents.get(v as usize)) == origin {
                let have = self.name_bytes(v);
                if have == name.as_bytes() {
                    return Some(v);
                }
                if loose.is_none() && same_ignoring_case(have, name.as_bytes()) {
                    loose = Some(v);
                }
            }
            at += 1;
            if at == self.slots.len() {
                at = 0;
            }
        }
    }

    /// Without the table: the entries with that name (ignoring case, as `child` does), one pass.
    pub fn child_by_scan(&self, parent: u32, name: &str) -> Option<u32> {
        let mut loose = None;
        for e in 0..self.len() as u32 {
            if self.flags[e as usize] & DEAD != 0 || self.parent(e) != parent {
                continue;
            }
            let have = self.name_bytes(e);
            if have == name.as_bytes() {
                return Some(e);
            }
            if loose.is_none() && same_ignoring_case(have, name.as_bytes()) {
                loose = Some(e);
            }
        }
        loose
    }

    fn entry_hash(&self, entry: u32) -> u64 {
        key_hash(self.origin(self.parents.get(entry as usize)), self.name_bytes(entry))
    }

    fn slot_insert(&mut self, entry: u32) {
        if (self.slots_used + self.slots_tomb + 1) * 100 > self.slots.len() * 92 {
            self.build_table();
            return; // the rebuild included it
        }
        let mut at = slot_of(self.entry_hash(entry), self.slots.len());
        loop {
            let v = self.slots[at];
            if v == EMPTY || v == TOMB {
                if v == TOMB {
                    self.slots_tomb -= 1;
                }
                self.slots[at] = entry;
                self.slots_used += 1;
                return;
            }
            at += 1;
            if at == self.slots.len() {
                at = 0;
            }
        }
    }

    fn slot_remove(&mut self, entry: u32) {
        let mut at = slot_of(self.entry_hash(entry), self.slots.len());
        loop {
            let v = self.slots[at];
            if v == EMPTY {
                return;
            }
            if v == entry {
                self.slots[at] = TOMB;
                self.slots_used -= 1;
                self.slots_tomb += 1;
                return;
            }
            at += 1;
            if at == self.slots.len() {
                at = 0;
            }
        }
    }

    pub fn has_table(&self) -> bool {
        !self.slots.is_empty()
    }

    /// (Re)builds the (folder, name) table for every live entry, 85% full (it's rebuilt past 92%): a lookup takes a
    /// few probes, and lookups only come with changes.
    pub fn build_table(&mut self) {
        let _busy = self.changing();
        self.slots = self.table().into();
        self.slots_used = (0..self.len()).filter(|&e| self.flags[e] & DEAD == 0).count();
        self.slots_tomb = 0;
    }

    /// The table `build_table` installs, built from a shared borrow (so it can be built while searches go on).
    pub fn table(&self) -> Vec<u32> {
        use rayon::prelude::*;
        let live: Vec<u32> = (0..self.len() as u32).filter(|&e| self.flags[e as usize] & DEAD == 0).collect();
        let cap = (live.len() * 100 / 85).max(1024);
        let homes: Vec<usize> = live.par_iter().map(|&e| slot_of(self.entry_hash(e), cap)).collect();
        let mut slots = vec![EMPTY; cap];
        for (&e, &home) in live.iter().zip(&homes) {
            let mut at = home;
            while slots[at] != EMPTY {
                at += 1;
                if at == cap {
                    at = 0;
                }
            }
            slots[at] = e;
        }
        slots
    }

    /// Installs a table `table` built (for an index of this length, with nothing removed since).
    pub fn set_table(&mut self, slots: Vec<u32>) {
        let _busy = self.changing();
        self.slots = slots.into();
        self.slots_used = (0..self.len()).filter(|&e| self.flags[e] & DEAD == 0).count();
        self.slots_tomb = 0;
    }

    /// How common byte `b` (ignoring ASCII case) is in names, from a sample.
    pub fn frequency(&self, b: u8) -> u32 {
        let table = self.frequency.get_or_init(|| {
            let mut f = Box::new([0u32; 256]);
            for &c in self.names.iter().step_by(61).chain(self.folded.iter().step_by(61)) {
                f[c.to_ascii_lowercase() as usize] += 1;
            }
            f
        });
        table[b.to_ascii_lowercase() as usize]
    }

    /// Exact bytes the structures take (capacity, not just length).
    pub fn bytes(&self) -> usize {
        self.breakdown().iter().map(|(_, _, capacity)| capacity).sum()
    }

    /// What each structure takes, in bytes (capacity), for measuring.
    pub fn breakdown(&self) -> Vec<(&'static str, usize, usize)> {
        let maps = (self.big.capacity() + self.odd.capacity()) * 16 + self.origins.capacity() * 12;
        vec![
            ("names", self.names.len(), self.names.capacity()),
            ("starts", self.starts.len() * 4, self.starts.capacity() * 4),
            ("name_slots", self.name_slots.len() * 4, self.name_slots.capacity() * 4),
            ("folded", self.folded.len(), self.folded.capacity()),
            ("folded_at", self.folded_at.len() * 8, self.folded_at.capacity() * 8),
            ("name_of", self.name_of.bytes().len(), self.name_of.capacity() * self.name_of.width()),
            ("parents", self.parents.bytes().len(), self.parents.capacity() * self.parents.width()),
            ("sizes", self.sizes.len(), self.sizes.capacity()),
            ("dates", self.dates.len() * 4, self.dates.capacity() * 4),
            ("created", self.created.bytes().0, self.created.bytes().1),
            ("accessed", self.accessed.bytes().0, self.accessed.bytes().1),
            ("attrs", self.attrs.codes.len(), self.attrs.codes.capacity()),
            ("flags", self.flags.len(), self.flags.capacity()),
            ("exts", self.exts.len(), self.exts.capacity()),
            ("by_record", self.by_record.len() * 4, self.by_record.capacity() * 4),
            ("path_rank", self.path_rank.len() * 4, self.path_rank.capacity() * 4),
            ("slots", self.slots.len() * 4, self.slots.capacity() * 4),
            ("maps", maps, maps),
        ]
    }

    pub fn table_bytes(&self) -> usize {
        self.slots.capacity() * 4
    }

    /// Compares two entries by name (folded: ignoring case and accents, then by the bytes themselves), then by entry.
    pub fn name_order(&self, a: u32, b: u32) -> Ordering {
        let (x, y) = (self.name_of.get(a as usize), self.name_of.get(b as usize));
        if x == y {
            return a.cmp(&b);
        }
        // Two sorted names: their ids are in that order (and differ, so the names do).
        if x < self.sorted && y < self.sorted {
            return x.cmp(&y);
        }
        self.id_order(x, y).then(a.cmp(&b))
    }

    /// For each name, the first of the names that compare the same ignoring case and accents (they're next to each
    /// other in name order; names added since the last sort are each their own).
    pub fn name_groups(&self) -> Vec<u32> {
        let count = self.name_count();
        let mut groups: Vec<u32> = (0..count as u32).collect();
        let key = |id: u32| -> std::borrow::Cow<[u8]> {
            let name = self.id_bytes(id);
            if name.is_ascii() { name.to_ascii_lowercase().into() } else { self.id_folded(id).into() }
        };
        for id in 1..self.sorted {
            if key(id) == key(id - 1) {
                groups[id as usize] = groups[id as usize - 1];
            }
        }
        // Names added since: their group among the sorted ones (found by halving), or among each other.
        let mut since: HashMap<Vec<u8>, u32> = HashMap::new();
        for id in self.sorted..count as u32 {
            let wanted = key(id);
            let (mut low, mut high) = (0u32, self.sorted);
            while low < high {
                let middle = low + (high - low) / 2;
                if key(middle) < wanted { low = middle + 1 } else { high = middle }
            }
            let at = low;
            groups[id as usize] = if at < self.sorted && key(at) == wanted {
                groups[at as usize]
            } else {
                *since.entry(wanted.into_owned()).or_insert(id)
            };
        }
        groups
    }

    /// Two names by their folded form, then their bytes.
    fn id_order(&self, x: u32, y: u32) -> Ordering {
        let (wx, wy) = (!self.id_bytes(x).is_ascii(), !self.id_bytes(y).is_ascii());
        let order = if !wx && !wy {
            let (p, q) = (self.id_bytes(x), self.id_bytes(y));
            p.iter().map(u8::to_ascii_lowercase).cmp(q.iter().map(u8::to_ascii_lowercase))
        } else {
            let p: std::borrow::Cow<[u8]> = if wx { self.id_folded(x).into() } else { self.id_bytes(x).to_ascii_lowercase().into() };
            let q: std::borrow::Cow<[u8]> = if wy { self.id_folded(y).into() } else { self.id_bytes(y).to_ascii_lowercase().into() };
            p.cmp(&q)
        };
        order.then_with(|| self.id_bytes(x).cmp(self.id_bytes(y)))
    }

    /// The distinct names in name order, made from a shared borrow (searches go on meanwhile); `set_sorted` installs
    /// it. Each name's sort key starts as its first 8 bytes (lowercased, or folded beyond ASCII) in one integer, so
    /// most comparisons are one integer comparison; ties compare the rest. Only the names an entry has are kept (a
    /// dead one's too, until `compact` takes it out).
    pub fn sorted_names(&self) -> Sorted {
        use rayon::prelude::*;
        let count = self.name_count();
        let used = {
            let used: Vec<std::sync::atomic::AtomicU64> = (0..count.div_ceil(64)).map(|_| std::sync::atomic::AtomicU64::new(0)).collect();
            self.name_of.par_iter().for_each(|id| {
                used[id as usize / 64].fetch_or(1 << (id % 64), std::sync::atomic::Ordering::Relaxed);
            });
            used.into_iter().map(std::sync::atomic::AtomicU64::into_inner).collect::<Vec<u64>>()
        };
        let in_use = |id: u32| used[id as usize / 64] >> (id % 64) & 1 != 0;
        let key = |id: u32| -> std::borrow::Cow<[u8]> {
            let name = self.id_bytes(id);
            if !name.is_ascii() {
                self.id_folded(id).into()
            } else if name.iter().any(u8::is_ascii_uppercase) {
                name.to_ascii_lowercase().into()
            } else {
                name.into()
            }
        };
        let mut keys: Vec<(u64, u32)> = (0..count as u32).into_par_iter().filter(|&id| in_use(id)).map(|id| {
            let k = key(id);
            let mut prefix = [0u8; 8];
            let take = k.len().min(8);
            prefix[..take].copy_from_slice(&k[..take]);
            (u64::from_be_bytes(prefix), id)
        }).collect();
        keys.par_sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| key(a.1).cmp(&key(b.1))).then_with(|| self.id_bytes(a.1).cmp(self.id_bytes(b.1))));
        let mut map = vec![NONE; count];
        let mut names = Vec::with_capacity(self.names.len());
        let mut starts = Vec::with_capacity(keys.len() + 1);
        let mut folded = vec![0u8];
        let mut folded_at = Vec::new();
        names.push(0);
        for (place, &(_, id)) in keys.iter().enumerate() {
            map[id as usize] = place as u32;
            starts.push(names.len() as u32);
            let name = self.id_bytes(id);
            names.extend_from_slice(name);
            names.push(0);
            if !name.is_ascii() {
                folded_at.push((folded.len() as u32, place as u32));
                folded.extend_from_slice(self.id_folded(id));
                folded.push(0);
            }
        }
        starts.push(names.len() as u32);
        // Every name is sorted: none needs the table.
        let name_slots = Vec::new();
        Sorted { names, starts, folded, folded_at, name_slots, map, count, kept: keys.len() }
    }

    /// Installs names sorted by `sorted_names` (from this index, with nothing but new entries since: names added since
    /// keep coming after the sorted ones).
    pub fn set_sorted(&mut self, mut sorted: Sorted) {
        let _busy = self.changing();
        // Names that came since it was made: after the sorted ones, as they were.
        for id in sorted.count as u32..self.name_count() as u32 {
            let place = (sorted.starts.len() - 1) as u32;
            sorted.map.push(place);
            let name = self.id_bytes(id);
            sorted.names.extend_from_slice(name);
            sorted.names.push(0);
            sorted.starts.push(sorted.names.len() as u32);
            if !name.is_ascii() {
                sorted.folded_at.push((sorted.folded.len() as u32, place));
                sorted.folded.extend_from_slice(self.id_folded(id));
                sorted.folded.push(0);
            }
        }
        if sorted.count < self.name_count() {
            let since = sorted.starts.len() - 1 - sorted.kept;
            sorted.name_slots = slots_for(&sorted.names, &sorted.starts, sorted.kept, since * 100 / 60);
        }
        let kept = sorted.kept;
        let map = &sorted.map;
        self.name_of.map_all(|id| map[id as usize]);
        self.names = sorted.names.into();
        self.starts = sorted.starts.into();
        self.folded = sorted.folded;
        self.folded_at = sorted.folded_at;
        self.name_slots = sorted.name_slots.into();
        self.sorted = kept as u32;
        self.frequency = std::sync::OnceLock::new();
    }

    /// Sorts the distinct names: every entry in name order, ignoring case and accents.
    pub fn sort_names(&mut self) {
        let sorted = self.sorted_names();
        self.set_sorted(sorted);
    }

    /// `path_rank`, computed from a shared borrow (entries since compare by name).
    pub fn path_ranks(&self) -> Vec<u32> {
        let n = self.len();
        // Children of each live entry, as one array (counting sort by parent), each group in name order.
        let mut count = vec![0u32; n + 1];
        let mut roots = Vec::new();
        let mut parent_of = vec![NONE; n];
        for e in 0..n as u32 {
            if self.flags[e as usize] & DEAD != 0 {
                continue;
            }
            let p = self.parent(e);
            parent_of[e as usize] = p;
            if p == NONE { roots.push(e) } else { count[p as usize + 1] += 1 }
        }
        for i in 1..=n {
            count[i] += count[i - 1];
        }
        let mut fill = count.clone();
        let mut kids = vec![0u32; count[n] as usize];
        // In name order, so each group comes out sorted.
        let mut by_name: Vec<u32> = (0..n as u32).filter(|&e| parent_of[e as usize] != NONE).collect();
        {
            use rayon::prelude::*;
            by_name.par_sort_unstable_by(|&a, &b| self.name_order(a, b));
            roots.sort_unstable_by(|&a, &b| self.name_order(a, b));
        }
        for e in by_name {
            let p = parent_of[e as usize] as usize;
            kids[fill[p] as usize] = e;
            fill[p] += 1;
        }
        let mut path_rank = vec![u32::MAX; n];
        let mut next = 0u32;
        let mut stack: Vec<u32> = roots.into_iter().rev().collect();
        while let Some(e) = stack.pop() {
            path_rank[e as usize] = next;
            next += 1;
            let (from, to) = (count[e as usize] as usize, count[e as usize + 1] as usize);
            stack.extend(kids[from..to].iter().rev());
        }
        path_rank
    }

    pub fn set_path_rank(&mut self, path_rank: Vec<u32>) {
        self.path_ranked = path_rank.len();
        self.path_rank = path_rank;
        self.path_stale = false;
    }

    /// Builds `path_rank`: a walk of the tree, each folder followed by its contents, siblings in name order.
    pub fn rank_paths(&mut self) {
        let path_rank = self.path_ranks();
        self.set_path_rank(path_rank);
    }

    /// Compares two entries by path; both must be live. Falls back to building paths for entries added since.
    pub fn path_order(&self, a: u32, b: u32) -> Ordering {
        if !self.path_stale && (a as usize) < self.path_ranked && (b as usize) < self.path_ranked {
            return self.path_rank[a as usize].cmp(&self.path_rank[b as usize]);
        }
        // Level by level from the top, each level by `name_order`.
        let chain = |e: u32| {
            let mut chain = vec![e];
            let mut at = self.parent(e);
            while at != NONE {
                chain.push(at);
                at = self.parent(at);
            }
            chain.reverse();
            chain
        };
        let (x, y) = (chain(a), chain(b));
        for (p, q) in x.iter().zip(&y) {
            if p != q {
                return self.name_order(*p, *q);
            }
        }
        x.len().cmp(&y.len())
    }

    /// Compares two entries by extension (ignoring case), then by name.
    pub fn ext_order(&self, a: u32, b: u32) -> Ordering {
        let ext = |e: u32| extension(self.name_bytes(e)).map(ext_key).unwrap_or_default();
        ext(a).cmp(&ext(b)).then_with(|| self.name_order(a, b))
    }

    pub fn shrink(&mut self) {
        self.names.shrink_to_fit();
        self.starts.shrink_to_fit();
        self.name_of.shrink_to_fit();
        self.parents.shrink_to_fit();
        self.sizes.shrink_to_fit();
        self.dates.shrink_to_fit();
        self.created.shrink_to_fit();
        self.accessed.shrink_to_fit();
        self.attrs.codes.shrink_to_fit();
        self.flags.shrink_to_fit();
        self.exts.shrink_to_fit();
        self.by_record.shrink_to_fit();
        self.folded.shrink_to_fit();
        self.folded_at.shrink_to_fit();
    }

    /// Takes the dead entries out, in place, the rest in the same order (no second copy of the index is made, so
    /// memory doesn't double while it runs), then the names no entry has any more, the names sorted again. Returns each
    /// old entry's new number (`NONE`: gone).
    pub fn compact_in_place(&mut self) -> Vec<u32> {
        let had_table = !self.slots.is_empty();
        let map = self.compact_entries();
        self.sort_names();
        if had_table {
            self.build_table();
        }
        map
    }

    /// `compact_in_place`'s first part, the one that needs the index to itself: the dead entries out, the extensions
    /// counted again. The names stay as they were (still sorted; those no entry has go at the next sort), and the
    /// (folder, name) table is let go: `table` makes it again from a shared borrow, while searches go on.
    pub fn compact_entries(&mut self) -> Vec<u32> {
        let _busy = self.changing();
        use rayon::prelude::*;
        let n = self.len();
        let mut map = vec![NONE; n];
        let mut next = 0u32;
        for e in 0..n {
            if self.flags[e] & DEAD == 0 && !self.gone(e as u32) {
                map[e] = next;
                next += 1;
            }
        }
        for e in 0..n {
            if self.flags[e] & FORWARD != 0 {
                map[e] = map[self.current(e as u32) as usize];
            }
        }
        // Each kept entry's folder, numbered anew, worked out before anything moves.
        let parents: Vec<u32> = (0..n as u32).into_par_iter().map(|e| {
            if map[e as usize] == NONE || self.flags[e as usize] & DEAD != 0 {
                return NONE;
            }
            let parent = self.parent(e);
            if parent == NONE { NONE } else { map[parent as usize] }
        }).collect();
        // The times and attributes first (in place, in order): which entries stay is read from the flags, which the
        // loop below overwrites.
        let kept: Vec<bool> = (0..n).map(|e| map[e] != NONE && self.flags[e] & DEAD == 0).collect();
        self.created.compact(|e| kept[e]);
        self.accessed.compact(|e| kept[e]);
        self.attrs.compact(|e| kept[e]);
        drop(kept);
        let mut to = 0usize;
        for e in 0..n {
            if map[e] == NONE || self.flags[e] & DEAD != 0 {
                continue;
            }
            self.name_of.set(to, self.name_of.get(e));
            self.parents.set(to, parents[e]);
            self.sizes.copy_within(e * 3..e * 3 + 3, to * 3);
            self.dates[to] = self.dates[e];
            self.flags[to] = self.flags[e];
            self.exts[to] = self.exts[e];
            to += 1;
        }
        drop(parents);
        for column in [&mut self.name_of, &mut self.parents] {
            column.truncate(to);
            column.shrink_to_fit();
        }
        self.dates.truncate(to);
        self.dates.shrink_to_fit();
        self.sizes.truncate(to * 3);
        self.sizes.shrink_to_fit();
        for column in [&mut self.flags, &mut self.exts] {
            column.truncate(to);
            column.shrink_to_fit();
        }
        let renumber = |map: &[u32], old: HashMap<u32, u64>| -> HashMap<u32, u64> {
            old.into_iter().filter_map(|(e, v)| Some((map.get(e as usize).copied().filter(|&m| m != NONE)?, v))).collect()
        };
        self.big = renumber(&map, std::mem::take(&mut self.big));
        self.odd = renumber(&map, std::mem::take(&mut self.odd).into_iter().map(|(k, v)| (k, v as u64)).collect())
            .into_iter().map(|(k, v)| (k, v as i64)).collect();
        // (Children point at their folder's entry now, not at what it started as.)
        self.origins.clear();
        self.roots = self.roots.iter().filter_map(|&e| Some(map[e as usize]).filter(|&m| m != NONE && self.parents.get(m as usize) == NONE)).collect();
        self.roots.dedup();
        for record in self.by_record.iter_mut() {
            if *record != NONE {
                *record = map.get(*record as usize).copied().unwrap_or(NONE);
            }
        }
        self.dead = 0;
        self.pending_dirs = 0;
        self.low_water = None;
        self.path_rank = Vec::new();
        self.path_ranked = 0;
        self.path_stale = false;
        self.rank_exts();
        self.slots = Column::default();
        self.slots_used = 0;
        self.slots_tomb = 0;
        map
    }

    /// A new index with only the live entries, in the same order (and only their names, sorted); returns it and each
    /// old entry's new number.
    pub fn compact(&self) -> (Index, Vec<u32>) {
        let mut map = vec![NONE; self.len()];
        let mut next = 0u32;
        for e in 0..self.len() {
            if self.flags[e] & DEAD == 0 && !self.gone(e as u32) {
                map[e] = next;
                next += 1;
            }
        }
        // Forwarded entries map to where they lead.
        for e in 0..self.len() {
            if self.flags[e] & FORWARD != 0 {
                map[e] = map[self.current(e as u32) as usize];
            }
        }
        let mut out = Index::new();
        out.names.reserve(self.names.len());
        for e in 0..self.len() as u32 {
            if map[e as usize] == NONE || self.flags[e as usize] & DEAD != 0 {
                continue;
            }
            let parent = self.parent(e);
            let p = if parent == NONE { NONE } else { map[parent as usize] };
            let new = out.push_raw(p, self.name(e), self.meta(e));
            out.put_more(new, self.more(e));
        }
        out.shrink();
        out.sort_names();
        out.rank_exts();
        if !self.slots.is_empty() {
            out.build_table();
        }
        (out, map)
    }

    /// Writes the index to `path` as it's mapped (`open`): each array at its own place in the file with room to grow,
    /// through a temporary file so a crash leaves the old one. An index mapped from `path` itself can't replace it
    /// (`write_new`, then `open` that, then rename it into place; `flush` when it only changed).
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        let temporary = self.write_new(path)?;
        std::fs::rename(&temporary, path)
    }

    /// Writes the index next to `path` (as "names.saving"), ready to be mapped and renamed into place; returns where.
    pub fn write_new(&self, path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
        let temporary = path.with_extension("saving");
        {
            let mut out = std::io::BufWriter::with_capacity(1 << 22, std::fs::File::create(&temporary)?);
            self.write_v6(&mut out)?;
            std::io::Write::flush(&mut out)?;
            out.get_ref().sync_all()?;
        }
        Ok(temporary)
    }

    /// The arrays as written (`write_v6`): each as its bytes, its width, and how many values there's room for in the
    /// file. The dates made and opened are coded anew (`coded`): nothing set aside, nothing given a value of its own
    /// that a code can stand for.
    fn v6_columns<'a>(&'a self, created: &'a Coded, accessed: &'a Coded, (name_of, parents): (&'a Ids, &'a Ids))
        -> [(&'a [u8], usize, usize); V6_COLUMNS] {
        let n = self.len();
        let room = (n / 32).max(65_536);
        let entries = n + room;
        let names = self.name_count();
        let distinct = names + (names / 32).max(16_384);
        let arena = self.names.len() + (self.names.len() / 32).max(1 << 20);
        // Set aside until the next full write: a file written to, whose date modified moved away from its date made.
        let spill = (n / 128).max(16_384);
        let words = entries.div_ceil(32) + 1;
        [
            (bytes_of(&self.names), 1, arena),
            (bytes_of(&self.starts), 4, distinct + 1),
            (name_of.bytes(), name_of.width(), entries),
            (parents.bytes(), parents.width(), entries),
            (bytes_of(&self.sizes), 1, entries * 3),
            (bytes_of(&self.dates), 4, entries),
            (bytes_of(&self.flags), 1, entries),
            (bytes_of(&self.exts), 1, entries),
            (bytes_of(&self.name_slots), 4, self.name_slots.len()),
            (bytes_of(&self.slots), 4, self.slots.len()),
            (bytes_of(&self.by_record), 4, self.by_record.len()),
            (bytes_of(&created.codes), 8, words),
            (bytes_of(&created.ranks), 4, words),
            (bytes_of(&created.values), 4, created.values.len() + room),
            (bytes_of(&created.spill), 8, spill),
            (bytes_of(&accessed.codes), 8, words),
            (bytes_of(&accessed.ranks), 4, words),
            (bytes_of(&accessed.values), 4, accessed.values.len() + room),
            (bytes_of(&accessed.spill), 8, spill),
            (bytes_of(&self.attrs.codes), 1, entries),
        ]
    }

    /// The names' ids and the parents as written: 3 bytes each when every one (and the room after them) fits, so an
    /// index read with 4 (v5's, or one that had more entries) is written with 3 again; `None` when they're kept so.
    fn ids_anew(&self) -> Option<(Ids, Ids)> {
        let room = (self.len() / 32).max(65_536);
        let fit = self.len() + room < 0xFF_FFFF && self.name_count() + (self.name_count() / 32).max(16_384) < 0xFF_FFFF;
        let wide = self.name_of.width() == 4 || self.parents.width() == 4;
        (fit && wide).then(|| (Ids::from_values(&self.name_of.iter().collect::<Vec<u32>>()), Ids::from_values(&self.parents.iter().collect::<Vec<u32>>())))
    }

    /// The dates made and opened coded anew, for writing.
    fn coded_anew(&self) -> (Coded, Coded) {
        let n = self.len();
        let created = Coded::encode(n, |e| (self.created_raw(e), self.modified_raw(e), Some(0)));
        let accessed = Coded::encode(n, |e| (self.accessed_raw(e), self.modified_raw(e), Some(self.created_raw(e))));
        (created, accessed)
    }

    /// What isn't an array: the extension table, the rare values, the roots, the table of attributes (in Excavate's
    /// memory once loaded).
    fn v6_small(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let put = |out: &mut Vec<u8>, v: u64| out.extend_from_slice(&v.to_le_bytes());
        put(&mut out, self.ext_names.len() as u64);
        for ext in &self.ext_names {
            out.push(ext.len() as u8);
            out.extend_from_slice(ext);
        }
        out.extend_from_slice(bytes_of(&self.ext_counts));
        put(&mut out, self.big.len() as u64);
        for (&k, &v) in &self.big {
            put(&mut out, k as u64);
            put(&mut out, v);
        }
        put(&mut out, self.odd.len() as u64);
        for (&k, &v) in &self.odd {
            put(&mut out, k as u64);
            put(&mut out, v as u64);
        }
        put(&mut out, self.origins.len() as u64);
        for (&k, &v) in &self.origins {
            put(&mut out, k as u64);
            put(&mut out, v as u64);
        }
        put(&mut out, self.roots.len() as u64);
        out.extend_from_slice(bytes_of(&self.roots));
        put(&mut out, self.times_filled as u64);
        put(&mut out, self.attrs.values.len() as u64);
        out.extend_from_slice(bytes_of(&self.attrs.values));
        put(&mut out, self.attrs.rare.len() as u64);
        for (&k, &v) in &self.attrs.rare {
            put(&mut out, k as u64);
            put(&mut out, v as u64);
        }
        out
    }

    /// The counts the header keeps (`V5_SCALARS` of them), after the magic and the clean mark.
    fn v5_scalars(&self) -> [u64; V5_SCALARS] {
        [self.len() as u64, self.dead as u64, self.sorted as u64, self.slots_used as u64, self.slots_tomb as u64,
         self.pending_dirs as u64, self.pending_low as u64, self.low_water.map_or(u64::MAX, |low| low as u64)]
    }

    /// The file `open` maps: a header page (magic, the clean mark, the counts, each array's place, length and room),
    /// then each array on a page boundary with its room after it, then the small part with room too.
    pub(crate) fn write_v6(&self, out: &mut impl std::io::Write) -> std::io::Result<()> {
        let (created, accessed) = self.coded_anew();
        let narrowed = self.ids_anew();
        let ids = narrowed.as_ref().map_or((&self.name_of, &self.parents), |(name_of, parents)| (name_of, parents));
        let columns = self.v6_columns(&created, &accessed, ids);
        let small = self.v6_small();
        let page = |at: usize| at.div_ceil(V5_PAGE) * V5_PAGE;
        let mut header = vec![0u8; V5_HEADER];
        header[..8].copy_from_slice(MAGIC_V6);
        header[V5_CLEAN] = 1;
        header[V6_IDS] = ids.0.width() as u8;
        header[V6_IDS + 1] = ids.1.width() as u8;
        header[V5_BOOT..V5_BOOT + 8].copy_from_slice(&booted_at().to_le_bytes());
        let mut at = V5_HEADER;
        let mut fields: Vec<u64> = self.v5_scalars().to_vec();
        let mut places = Vec::new();
        for &(bytes, width, cap) in &columns {
            places.push(at);
            fields.extend([at as u64, (bytes.len() / width) as u64, cap as u64]);
            at = page(at + cap * width);
        }
        let small_cap = small.len() * 2 + 65_536;
        fields.extend([at as u64, small.len() as u64, small_cap as u64]);
        for (i, field) in fields.iter().enumerate() {
            header[V5_FIELDS + i * 8..V5_FIELDS + 8 + i * 8].copy_from_slice(&field.to_le_bytes());
        }
        out.write_all(&header)?;
        let mut written = V5_HEADER;
        let zeros = vec![0u8; 1 << 20];
        let pad = |out: &mut dyn std::io::Write, written: &mut usize, to: usize| -> std::io::Result<()> {
            while *written < to {
                let take = (to - *written).min(zeros.len());
                out.write_all(&zeros[..take])?;
                *written += take;
            }
            Ok(())
        };
        for (&(bytes, _, _), &place) in columns.iter().zip(&places) {
            pad(out, &mut written, place)?;
            out.write_all(bytes)?;
            written += bytes.len();
        }
        pad(out, &mut written, at)?;
        out.write_all(&small)?;
        written += small.len();
        pad(out, &mut written, at + small_cap)?;
        Ok(())
    }

    /// The saved index at `path`, mapped (the names service's: no copy of it in Excavate's memory, and changes are
    /// written back by `flush`). A v5 file is read (its arrays mapped, the dates made and opened and the attributes
    /// coded in memory) and written as v6 at the next save; one not saved cleanly fails (what it holds may be
    /// half-changed).
    pub fn open(path: &std::path::Path) -> std::io::Result<Index> {
        let map = Mapping::open(path)?;
        let bytes = map.bytes();
        if bytes.len() >= 8 && &bytes[..8] == MAGIC_V5 {
            return Index::from_file(bytes, Some(&map), 5);
        }
        Index::from_file(bytes, Some(&map), 6)
    }

    /// A v6 file (or a v5 one, `version`): mapped (`map`), or copied into memory.
    fn from_file(bytes: &[u8], map: Option<&Arc<Mapping>>, version: u8) -> std::io::Result<Index> {
        let bad = || std::io::Error::other("not an index of this version");
        let (magic, count, mut widths): (&[u8], usize, Vec<usize>) =
            if version == 5 { (MAGIC_V5, V5_COLUMNS, V5_WIDTHS.to_vec()) } else { (MAGIC_V6, V6_COLUMNS, V6_WIDTHS.to_vec()) };
        if bytes.len() < V5_HEADER || &bytes[..8] != magic {
            return Err(bad());
        }
        // v6: the names' ids and the parents, 3 bytes each or 4 (`ids`), as its header says.
        if version == 6 {
            widths[2] = bytes[V6_IDS] as usize;
            widths[3] = bytes[V6_IDS + 1] as usize;
        }
        // Not saved cleanly: trusted only when it was left between two changes and Windows hasn't restarted since (it
        // wrote every changed page itself); after a restart it may have been a power cut, with only some written.
        if bytes[V5_CLEAN] != 1 {
            let boot = u64::from_le_bytes(bytes[V5_BOOT..V5_BOOT + 8].try_into().unwrap());
            if bytes[V5_BUSY] != 0 || bytes[V5_BEHIND] != 0 || boot.abs_diff(booted_at()) > 5 {
                return Err(std::io::Error::other("the index wasn't saved cleanly"));
            }
        }
        let field = |i: usize| u64::from_le_bytes(bytes[V5_FIELDS + i * 8..V5_FIELDS + 8 + i * 8].try_into().unwrap()) as usize;
        let place = |c: usize| -> std::io::Result<(usize, usize, usize)> {
            let (at, len, cap) = (field(V5_SCALARS + c * 3), field(V5_SCALARS + c * 3 + 1), field(V5_SCALARS + c * 3 + 2));
            let end = cap.checked_mul(widths[c]).and_then(|bytes| bytes.checked_add(at));
            if len > cap || at % V5_PAGE != 0 || end.is_none_or(|end| end > bytes.len()) {
                return Err(bad());
            }
            Ok((at, len, cap))
        };
        fn column<T: Copy + Default>(bytes: &[u8], map: Option<&Arc<Mapping>>, (at, len, cap): (usize, usize, usize)) -> Column<T> {
            match map {
                Some(map) => Column::mapped(map, at, len, cap),
                None => pod::<T>(&bytes[at..at + len * std::mem::size_of::<T>()]).into(),
            }
        }
        let mut index = Index::default();
        let [n, dead, sorted, slots_used, slots_tomb, pending_dirs, pending_low, low_water]: [usize; V5_SCALARS] = std::array::from_fn(field);
        index.names = column(bytes, map, place(0)?);
        index.starts = column(bytes, map, place(1)?);
        let ids = |c: usize| -> std::io::Result<Ids> {
            let (at, len, cap) = place(c)?;
            Ids::from_bytes(column(bytes, map, (at, len * widths[c], cap * widths[c])), widths[c]).ok_or_else(bad)
        };
        index.name_of = ids(2)?;
        index.parents = ids(3)?;
        index.sizes = column(bytes, map, place(4)?);
        index.dates = column(bytes, map, place(5)?);
        index.flags = column(bytes, map, place(6)?);
        index.exts = column(bytes, map, place(7)?);
        index.name_slots = column(bytes, map, place(8)?);
        index.slots = column(bytes, map, place(9)?);
        index.by_record = column(bytes, map, place(10)?);
        let lengths = [index.name_of.len(), index.parents.len(), index.dates.len(), index.flags.len(), index.exts.len(), index.sizes.len() / 3];
        if lengths != [n; 6] || index.starts.is_empty() || index.names.is_empty() || *index.starts.last().unwrap() as usize != index.names.len() {
            return Err(bad());
        }
        // v5's dates made and opened, 4 bytes each, and attributes, 2: coded here, in memory, once.
        let mut v5_attrs: Option<Column<u16>> = None;
        if version == 5 {
            let (created, accessed): (Column<u32>, Column<u32>) = (column(bytes, map, place(11)?), column(bytes, map, place(12)?));
            let attrs: Column<u16> = column(bytes, map, place(13)?);
            if created.len() != n || accessed.len() != n || attrs.len() != n {
                return Err(bad());
            }
            let modified = |e: usize| Some(index.dates[e]).filter(|&date| date != DATE_ODD);
            index.created = Coded::encode(n, |e| (created[e], modified(e), Some(0)));
            index.accessed = Coded::encode(n, |e| (accessed[e], modified(e), Some(created[e])));
            v5_attrs = Some(attrs);
        } else {
            let coded = |first: usize| -> std::io::Result<Coded> {
                Coded::from_columns(column(bytes, map, place(first)?), column(bytes, map, place(first + 1)?),
                                    column(bytes, map, place(first + 2)?), column(bytes, map, place(first + 3)?), n).ok_or_else(bad)
            };
            index.created = coded(11)?;
            index.accessed = coded(15)?;
        }
        (index.dead, index.sorted, index.slots_used, index.slots_tomb) = (dead, sorted as u32, slots_used, slots_tomb);
        if index.sorted as usize > index.name_count() {
            return Err(bad());
        }
        // Only the names since the last sort.
        index.name_slots = slots_for(&index.names, &index.starts, index.sorted as usize, index.unsorted() * 100 / 60).into();
        (index.pending_dirs, index.pending_low) = (pending_dirs, pending_low as u32);
        index.low_water = (low_water as u64 != u64::MAX).then_some(low_water as u32);
        let (small_at, small_len) = (field(V5_SCALARS + count * 3), field(V5_SCALARS + count * 3 + 1));
        let mut small = bytes.get(small_at..small_at + small_len).ok_or_else(bad)?;
        let mut take = |len: usize| -> std::io::Result<&[u8]> {
            let (head, rest) = small.split_at_checked(len).ok_or_else(bad)?;
            small = rest;
            Ok(head)
        };
        let mut read = |len: usize| -> std::io::Result<Vec<u8>> { take(len).map(<[u8]>::to_vec) };
        let next = |read: &mut dyn FnMut(usize) -> std::io::Result<Vec<u8>>| -> std::io::Result<u64> {
            Ok(u64::from_le_bytes(read(8)?.try_into().unwrap()))
        };
        let exts = next(&mut read)? as usize;
        let mut ext_names = Vec::with_capacity(exts);
        for _ in 0..exts {
            let len = read(1)?[0] as usize;
            ext_names.push(read(len)?);
        }
        index.ext_counts = pod(&read(256 * 4)?);
        for _ in 0..next(&mut read)? {
            let (k, v) = (next(&mut read)? as u32, next(&mut read)?);
            index.big.insert(k, v);
        }
        for _ in 0..next(&mut read)? {
            let (k, v) = (next(&mut read)? as u32, next(&mut read)? as i64);
            index.odd.insert(k, v);
        }
        for _ in 0..next(&mut read)? {
            let (k, v) = (next(&mut read)? as u32, next(&mut read)? as u32);
            index.origins.insert(k, v);
        }
        let roots = next(&mut read)? as usize;
        index.roots = pod(&read(roots * 4)?);
        index.times_filled = next(&mut read)? != 0;
        match v5_attrs {
            Some(attrs) => {
                for &value in attrs.iter() {
                    index.attrs.push(value);
                }
            }
            None => {
                let values = next(&mut read)? as usize;
                let values: Vec<u16> = pod(&read(values * 2)?);
                let mut rare = HashMap::new();
                for _ in 0..next(&mut read)? {
                    let (k, v) = (next(&mut read)? as u32, next(&mut read)? as u16);
                    rare.insert(k, v);
                }
                let codes: Column<u8> = column(bytes, map, place(19)?);
                if codes.len() != n {
                    return Err(bad());
                }
                index.attrs = Palette::from_parts(codes, values, rare).ok_or_else(bad)?;
            }
        }
        index.ext_ids = ext_names.iter().enumerate().map(|(i, ext)| (ext.clone(), i as u8 + 1)).collect();
        index.ext_names = ext_names;
        index.fold_names();
        // (Left between changes: the counts per extension are counted again, as they aren't kept after every change.)
        if bytes[V5_CLEAN] != 1 {
            index.count_exts();
        }
        index.published = std::sync::atomic::AtomicU64::new(index.small_print());
        if version == 5 {
            // Only read now: never trusted again unsaved (behind for good), so a change made to it before the v6 file
            // takes its place is never half-kept; written as v6 at the next save (`mapped` is false).
            if let Some(map) = map {
                map.bytes_mut(V5_CLEAN, 1)[0] = 0;
                map.bytes_mut(V5_BEHIND, 1)[0] = 1;
                let _ = map.flush(0, V5_HEADER);
            }
            index.from_v5 = true;
        } else {
            index.map = map.cloned();
            index.clean = AtomicBool::new(map.is_some() && bytes[V5_CLEAN] == 1);
        }
        Ok(index)
    }

    /// How many changes it has had (any change, mapped or not).
    pub fn edits(&self) -> u64 {
        self.edits.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Whether the arrays are mapped from a file (`open`), every one of them still in its room there.
    pub fn mapped(&self) -> bool {
        self.map.is_some()
            && [self.names.is_mapped(), self.starts.is_mapped(), self.name_of.is_mapped(), self.parents.is_mapped(), self.sizes.is_mapped(),
                self.dates.is_mapped(), self.flags.is_mapped(), self.exts.is_mapped(), self.slots.is_mapped(),
                self.by_record.is_mapped(), self.created.is_mapped(), self.accessed.is_mapped(), self.attrs.codes.is_mapped()]
                .iter().all(|&mapped| mapped)
    }

    /// Something is about to change: the mapped file's clean mark goes off first (and is on the disk before any
    /// change is), so a file left half-changed by a crash or a power cut is never trusted (`open`).
    fn changing(&mut self) -> Busy {
        self.edits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let Some(map) = &self.map else { return Busy { mark: std::ptr::null_mut(), depth: std::ptr::null(), index: std::ptr::null() } };
        if self.clean.swap(false, std::sync::atomic::Ordering::SeqCst) {
            map.bytes_mut(V5_BOOT, 8).copy_from_slice(&booted_at().to_le_bytes());
            map.bytes_mut(V5_CLEAN, 1)[0] = 0;
            let _ = map.flush(0, V5_HEADER);
        }
        let mark = map.bytes_mut(V5_BUSY, 1).as_mut_ptr();
        self.busy.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // SAFETY: inside the mapping, which the index holds.
        unsafe { std::ptr::write_volatile(mark, 1) };
        Busy { mark, depth: &self.busy, index: self }
    }

    /// What the small part holds, roughly: when this changes, it's written to the mapped file again.
    fn small_print(&self) -> u64 {
        let mut print = self.kept_aside.wrapping_mul(2).wrapping_add(self.times_filled as u64);
        for len in [self.big.len(), self.odd.len(), self.origins.len(), self.roots.len(), self.ext_names.len(), self.attrs.values.len(),
                    self.attrs.rare.len()] {
            print = print.wrapping_mul(1_000_003).wrapping_add(len as u64);
        }
        print
    }

    /// Puts the counts and each array's length in the mapped file's header, and the small part when it changed
    /// (after every change, in memory: Windows writes the pages). False when the small part outgrew its room.
    fn publish(&self, force: bool) -> bool {
        let Some(map) = &self.map else { return false };
        let field = |i: usize| u64::from_le_bytes(map.bytes()[V5_FIELDS + i * 8..V5_FIELDS + 8 + i * 8].try_into().unwrap()) as usize;
        let print = self.small_print();
        let mut fits = true;
        // The small part is written again when it changed, at most every 100 ms (a burst of changes to it would
        // otherwise write all of it each time); meanwhile the file says it's behind, and isn't trusted unsaved.
        if self.small_written.load(std::sync::atomic::Ordering::SeqCst) == u64::MAX {
            fits = false;
        } else if self.published.load(std::sync::atomic::Ordering::SeqCst) != print {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
            if force || now >= self.small_written.load(std::sync::atomic::Ordering::SeqCst) {
                let small = self.v6_small();
                let (small_at, small_cap) = (field(V5_SCALARS + V6_COLUMNS * 3), field(V5_SCALARS + V6_COLUMNS * 3 + 2));
                if small.len() <= small_cap {
                    map.bytes_mut(small_at, small.len()).copy_from_slice(&small);
                    let i = V5_SCALARS + V6_COLUMNS * 3 + 1;
                    map.bytes_mut(V5_FIELDS + i * 8, 8).copy_from_slice(&(small.len() as u64).to_le_bytes());
                    self.published.store(print, std::sync::atomic::Ordering::SeqCst);
                    // The next one not before 100 ms plus nine times what this one took (at most a tenth of the time).
                    let after = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
                    self.small_written.store(after + 100 + 9 * after.saturating_sub(now), std::sync::atomic::Ordering::SeqCst);
                    map.bytes_mut(V5_BEHIND, 1)[0] = 0;
                } else {
                    // Outgrew its room: behind until the file is written anew (`flush` says so); not tried again
                    // before then.
                    fits = false;
                    self.published.store(print, std::sync::atomic::Ordering::SeqCst);
                    self.small_written.store(u64::MAX, std::sync::atomic::Ordering::SeqCst);
                    map.bytes_mut(V5_BEHIND, 1)[0] = 1;
                }
            } else {
                map.bytes_mut(V5_BEHIND, 1)[0] = 1;
            }
        }
        let (c, a) = (&self.created, &self.accessed);
        let lengths = [self.names.len(), self.starts.len(), self.name_of.len(), self.parents.len(), self.sizes.len(), self.dates.len(),
                       self.flags.len(), self.exts.len(), self.name_slots.len(), self.slots.len(), self.by_record.len(),
                       c.codes.len(), c.ranks.len(), c.values.len(), c.spill.len(), a.codes.len(), a.ranks.len(), a.values.len(),
                       a.spill.len(), self.attrs.codes.len()];
        // (Only lengths within the room the file has: an array that moved to memory is written anew at the next save.)
        let caps: Vec<usize> = (0..V6_COLUMNS).map(|c| field(V5_SCALARS + c * 3 + 2)).collect();
        let header = map.bytes_mut(0, V5_HEADER);
        let mut set = |i: usize, value: u64| header[V5_FIELDS + i * 8..V5_FIELDS + 8 + i * 8].copy_from_slice(&value.to_le_bytes());
        for (i, value) in self.v5_scalars().iter().enumerate() {
            set(i, *value);
        }
        for (c, len) in lengths.iter().enumerate() {
            set(V5_SCALARS + c * 3 + 1, (*len).min(caps[c]) as u64);
        }
        fits
    }

    /// Makes the mapped file what the index is (only the changed pages are written), and marks it clean. Ok(false)
    /// when it can't: not mapped, or something outgrew its room (`write_new` then).
    pub fn flush(&self) -> std::io::Result<bool> {
        let Some(map) = self.map.as_ref().filter(|_| self.mapped()) else { return Ok(false) };
        if self.clean.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(true);
        }
        if !self.publish(true) {
            return Ok(false);
        }
        map.flush(0, map.len())?;
        map.bytes_mut(V5_CLEAN, 1)[0] = 1;
        map.flush(0, V5_HEADER)?;
        self.clean.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(true)
    }

    /// Bytes of Excavate's own memory the index takes (a mapped array takes none until it outgrows its room).
    pub fn heap_bytes(&self) -> usize {
        let columns = self.names.heap_bytes() + self.starts.heap_bytes() + self.name_of.heap_bytes() + self.parents.heap_bytes()
            + self.sizes.heap_bytes() + self.dates.heap_bytes() + self.flags.heap_bytes() + self.exts.heap_bytes()
            + self.name_slots.heap_bytes() + self.slots.heap_bytes() + self.by_record.heap_bytes()
            + self.created.heap_bytes() + self.accessed.heap_bytes() + self.attrs.codes.heap_bytes();
        let maps = (self.big.capacity() + self.odd.capacity()) * 16 + self.origins.capacity() * 12;
        columns + maps + self.folded.capacity() + self.folded_at.capacity() * 8 + self.path_rank.capacity() * 4
    }

    /// Reads a saved index: the file in parallel pieces, its blocks decoded in parallel straight into place.
    pub fn load(path: &std::path::Path) -> std::io::Result<Index> {
        use rayon::prelude::*;
        use std::os::windows::fs::FileExt;
        let file = std::fs::File::open(path)?;
        let len = file.metadata()?.len() as usize;
        let mut bytes = vec![0u8; len];
        let piece = (len / 16).max(1 << 20);
        bytes.par_chunks_mut(piece).enumerate().try_for_each(|(i, chunk)| -> std::io::Result<()> {
            let mut done = 0;
            while done < chunk.len() {
                let got = file.seek_read(&mut chunk[done..], (i * piece + done) as u64)?;
                if got == 0 {
                    return Err(std::io::ErrorKind::UnexpectedEof.into());
                }
                done += got;
            }
            Ok(())
        })?;
        Index::load_from(&bytes)
    }

    /// A saved index, from its bytes (a v6 file, or a v5 one).
    pub fn load_from(bytes: &[u8]) -> std::io::Result<Index> {
        if bytes.len() >= 8 && &bytes[..8] == MAGIC_V5 {
            return Index::from_file(bytes, None, 5);
        }
        Index::from_file(bytes, None, 6)
    }

    /// `folded` made from the names beyond ASCII (in parallel, then joined in id order).
    fn fold_names(&mut self) {
        use rayon::prelude::*;
        let wide: Vec<(u32, String)> = (0..self.name_count() as u32).into_par_iter()
            .filter_map(|id| {
                let name = self.id_bytes(id);
                (!name.is_ascii()).then(|| (id, fold(std::str::from_utf8(name).unwrap_or(""))))
            })
            .collect();
        self.folded = vec![0u8];
        self.folded_at = Vec::with_capacity(wide.len());
        for (id, folded) in wide {
            self.folded_at.push((self.folded.len() as u32, id));
            self.folded.extend_from_slice(folded.as_bytes());
            self.folded.push(0);
        }
    }
}

const MAGIC_V5: &[u8] = b"XENG0005";
/// v6: the dates made and opened as codes, the attributes as a byte (`coded`); the names' ids and the
/// parents 3 bytes each (`ids`).
const MAGIC_V6: &[u8] = b"XENG0007";
/// v5: the header's size, where its clean mark is, the counts it keeps, the arrays (and each one's width), the page.
const V5_HEADER: usize = 4096;
const V5_CLEAN: usize = 8;
const V5_BUSY: usize = 9;
/// The small part is behind the arrays (written again at most every 100 ms).
const V5_BEHIND: usize = 10;
/// When Windows had started, as the file was marked changing.
const V5_BOOT: usize = 16;
/// v6: how many bytes each name's id and each parent takes (3 or 4), one byte each.
const V6_IDS: usize = 11;
const V5_FIELDS: usize = 32;
const V5_SCALARS: usize = 8;
const V5_COLUMNS: usize = 14;
const V5_WIDTHS: [usize; V5_COLUMNS] = [1, 4, 4, 4, 1, 4, 1, 1, 4, 4, 4, 4, 4, 2];
const V6_COLUMNS: usize = 20;
/// (The names' ids and the parents, the third and fourth, as the header says: `V6_IDS`.)
const V6_WIDTHS: [usize; V6_COLUMNS] = [1, 4, 3, 3, 1, 4, 1, 1, 4, 4, 4, 8, 4, 4, 8, 8, 4, 4, 8, 1];
const V5_PAGE: usize = 4096;
/// A name table of `cap` slots (at least a little more than the names) for the names in `names` at `starts` from id
/// `from` on (the sorted ones before it are found by halving).
fn slots_for(names: &[u8], starts: &[u32], from: usize, cap: usize) -> Vec<u32> {
    use rayon::prelude::*;
    let count = starts.len() - 1;
    let from = from.min(count);
    let cap = cap.max((count - from) + (count - from) / 8).max(1024);
    let homes: Vec<usize> = (from..count).into_par_iter()
        .map(|id| slot_of(name_hash(&names[starts[id] as usize..starts[id + 1] as usize - 1]), cap))
        .collect();
    let mut slots = vec![EMPTY; cap];
    for (id, &home) in (from..count).zip(&homes) {
        let mut at = home;
        while slots[at] != EMPTY {
            at += 1;
            if at == cap {
                at = 0;
            }
        }
        slots[at] = id as u32;
    }
    slots
}

/// The `i`th of a column of 3-byte values.
fn get24(column: &[u8], i: usize) -> u32 {
    let at = i * 3;
    column[at] as u32 | (column[at + 1] as u32) << 8 | (column[at + 2] as u32) << 16
}

fn set24(column: &mut [u8], i: usize, value: u32) {
    column[i * 3..i * 3 + 3].copy_from_slice(&value.to_le_bytes()[..3]);
}

fn bytes_of<T>(v: &[T]) -> &[u8] {
    // SAFETY: the arrays hold plain integers and pairs of them.
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

/// Plain integers (or pairs of them) from their bytes, as `bytes_of` wrote them.
fn pod<T: Copy + Default>(bytes: &[u8]) -> Vec<T> {
    let n = bytes.len() / std::mem::size_of::<T>();
    let mut v = vec![T::default(); n];
    // SAFETY: plain old data of the same layout; the lengths match.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), v.as_mut_ptr() as *mut u8, n * std::mem::size_of::<T>()) };
    v
}

/// A name's extension, as Everything takes it: after the last dot, when the dot isn't first or last.
pub fn extension(name: &[u8]) -> Option<&[u8]> {
    let dot = memchr::memrchr(b'.', name)?;
    (dot > 0 && dot + 1 < name.len()).then(|| &name[dot + 1..])
}

/// An extension as it's compared: lowercased (folded beyond ASCII).
pub fn ext_key(ext: &[u8]) -> Vec<u8> {
    if ext.is_ascii() {
        ext.to_ascii_lowercase()
    } else {
        fold(std::str::from_utf8(ext).unwrap_or("")).into_bytes()
    }
}

/// A name's exact bytes, hashed: 8 bytes at a time.
fn name_hash(name: &[u8]) -> u64 {
    const K: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut h = (name.len() as u64).wrapping_mul(K) ^ 0x2545_F491_4F6C_DD1D;
    let mut chunks = name.chunks_exact(8);
    for chunk in &mut chunks {
        h = (h.rotate_left(5) ^ u64::from_le_bytes(chunk.try_into().unwrap())).wrapping_mul(K);
    }
    let rest = chunks.remainder();
    if !rest.is_empty() {
        let mut w = [0u8; 8];
        w[..rest.len()].copy_from_slice(rest);
        h = (h.rotate_left(5) ^ u64::from_le_bytes(w)).wrapping_mul(K);
    }
    h ^ (h >> 29)
}

/// Folder and name, the name without case (ASCII lowercased, the rest by `to_lowercase`).
fn key_hash(origin: u32, name: &[u8]) -> u64 {
    const K: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut h = (origin as u64).wrapping_mul(K) ^ 0x51_7CC1_B727_220A;
    if name.is_ascii() {
        for chunk in name.chunks(8) {
            let mut w = [0u8; 8];
            for (d, s) in w.iter_mut().zip(chunk) {
                *d = s.to_ascii_lowercase();
            }
            h = (h.rotate_left(5) ^ u64::from_le_bytes(w)).wrapping_mul(K);
        }
    } else {
        let lower = std::str::from_utf8(name).unwrap_or("").to_lowercase();
        for chunk in lower.as_bytes().chunks(8) {
            let mut w = [0u8; 8];
            w[..chunk.len()].copy_from_slice(chunk);
            h = (h.rotate_left(5) ^ u64::from_le_bytes(w)).wrapping_mul(K);
        }
    }
    h ^ (h >> 29)
}

fn slot_of(hash: u64, cap: usize) -> usize {
    ((hash as u128 * cap as u128) >> 64) as usize
}

fn same_ignoring_case(a: &[u8], b: &[u8]) -> bool {
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(b);
    }
    match (std::str::from_utf8(a), std::str::from_utf8(b)) {
        (Ok(x), Ok(y)) => x.to_lowercase() == y.to_lowercase(),
        _ => false,
    }
}

fn pack_size(size: Option<u64>, index: u32, big: &mut HashMap<u32, u64>) -> u32 {
    match size {
        None => SIZE_UNKNOWN,
        Some(size) if size >= SIZE_BIG as u64 => {
            big.insert(index, size);
            SIZE_BIG
        }
        Some(size) => size as u32,
    }
}

fn pack_date(seconds: i64, index: u32, odd: &mut HashMap<u32, i64>) -> u32 {
    let since = seconds - DATE_BASE;
    if (0..DATE_ODD as i64).contains(&since) {
        since as u32
    } else {
        odd.insert(index, seconds);
        DATE_ODD
    }
}

const FROM_1601: i64 = 11_644_473_600;

/// FILETIME (100 ns since 1601) to seconds since 1970.
pub fn seconds(filetime: i64) -> i64 {
    filetime.div_euclid(10_000_000) - FROM_1601
}

/// What a name is compared as: decomposed by compatibility (so "é" and "e\u{301}" are one, and full- and half-width
/// forms, ligatures and the like meet their plain forms: "ＡＢＣ" finds "abc", "ｶﾀｶﾅ" finds "カタカナ"), accents and
/// other combining marks dropped ("café" finds "cafe", as Everything does by default), lowercased, final sigma as plain
/// sigma, and Turkish dotless ı as i (as Everything does).
pub fn fold(text: &str) -> String {
    if text.is_ascii() {
        return text.to_ascii_lowercase();
    }
    use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};
    text.nfkd()
        .filter(|&c| !is_combining_mark(c))
        .flat_map(char::to_lowercase)
        .map(|c| match c { 'ς' => 'σ', 'ı' => 'i', c => c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(size: Option<u64>, modified: i64) -> Meta {
        Meta { size, modified, flags: 0 }
    }

    #[test]
    fn layout_round_trips() {
        let mut index = Index::new();
        let root = index.push(NONE, "C:", Meta { size: None, modified: 0, flags: DIR });
        let long = "x".repeat(300) + ".txt";
        let mut names = Vec::new();
        for n in 0..200 {
            let name = match n {
                7 => long.clone(),
                9 => "café.JPG".to_string(),
                // The same name twice: kept once.
                11 | 12 => "same.txt".to_string(),
                _ => format!("file {n}.{}", ["txt", "jpg", "rs", "longextensionnameover16"][n % 4]),
            };
            let (size, date) = match n {
                3 => (Some(5u64 << 32), 1_700_000_000),
                4 => (Some(1), -2_207_520_000),
                5 => (None, 7_253_280_000),
                _ => (Some(n as u64 * 1000), 1_600_000_000 + n as i64),
            };
            let e = index.push(root, &name, meta(size, date));
            names.push((e, name, size, date));
        }
        assert_eq!(index.name_count(), 200, "199 different names and C:");
        index.rank_exts();
        index.sort_names();
        index.rank_paths();
        let check = |index: &Index| {
            for (e, name, size, date) in &names {
                assert_eq!(index.name(*e), name);
                assert_eq!(index.size(*e), *size);
                assert_eq!(index.modified(*e), *date);
                assert_eq!(index.id_at(index.start(*e) + 1), index.name_of.get(*e as usize));
            }
            assert_eq!(index.name_of.get(names[11].0 as usize), index.name_of.get(names[12].0 as usize));
            assert_eq!(index.exts[names[9].0 as usize], index.ext_id("jpg").unwrap());
            assert_eq!(index.exts[names[3].0 as usize], EXT_OTHER);
            assert_eq!(index.folded_name(names[9].0), "cafe.jpg".as_bytes());
            assert_eq!(index.id_of(b"same.txt"), Some(index.name_of.get(names[11].0 as usize)));
        };
        check(&index);
        let mut file = Vec::new();
        index.write_v6(&mut file).unwrap();
        let loaded = Index::load_from(&file).unwrap();
        check(&loaded);
        assert_eq!(loaded.name_of, index.name_of);
        assert_eq!(loaded.sorted, index.sorted);
        assert!(loaded.path_rank.is_empty() && loaded.path_ranked == 0, "the path order is made when asked, not saved");
        assert_eq!(loaded.names, index.names);
        // Sorted: entries in name order by their ids alone.
        let mut by_id: Vec<u32> = names.iter().map(|(e, ..)| *e).collect();
        by_id.sort_by(|&a, &b| loaded.name_order(a, b));
        let mut by_name: Vec<u32> = names.iter().map(|(e, ..)| *e).collect();
        by_name.sort_by_key(|&e| (fold(loaded.name(e)), loaded.name(e).to_string(), e));
        assert_eq!(by_id, by_name);
    }

    #[test]
    fn compacting_in_place() {
        let mut index = Index::new();
        let root = index.push(NONE, "C:", Meta { size: None, modified: 0, flags: DIR });
        let folder = index.push(root, "folder", Meta { size: None, modified: 5, flags: DIR });
        let inside = index.push(folder, "inside.txt", meta(Some(5 << 32), -10));
        let gone = index.push(root, "gone only.txt", meta(Some(1), 1));
        let kept = index.push(root, "kept.txt", meta(Some(2), 2));
        index.build_table();
        let moved = index.rename(folder, root, "moved");
        index.remove(gone);
        index.settle();
        let names_before = index.name_count();
        let map = index.compact_in_place();
        assert_eq!(index.len(), 4);
        assert_eq!(index.dead, 0);
        assert_eq!(map[gone as usize], NONE);
        assert_eq!(map[folder as usize], map[moved as usize], "a forward leads where it led");
        let inside = map[inside as usize];
        assert_eq!(index.path(inside), r"C:\moved\inside.txt");
        assert_eq!(index.size(inside), Some(5 << 32));
        assert_eq!(index.modified(inside), -10);
        assert_eq!(index.child(map[kept as usize], "x"), None);
        assert_eq!(index.child(map[root as usize], "kept.txt"), Some(map[kept as usize]));
        assert_eq!(index.name_count(), names_before - 2, "the names only dead entries had are gone");
        assert_eq!(index.id_of(b"gone only.txt"), None);
    }

    /// Ids kept at 4 bytes (as a v5 file has them, or once one didn't fit) are written at 3 when they all fit, and
    /// read back the same.
    #[test]
    fn ids_written_narrow_again() {
        let mut index = Index::new();
        let root = index.push(NONE, "C:", Meta { size: None, modified: 0, flags: DIR });
        index.roots.push(root);
        let folder = index.push(root, "folder", Meta { size: None, modified: 0, flags: DIR });
        for i in 0..50u64 {
            index.push(folder, &format!("file {i}.txt"), meta(Some(i), 1_700_000_000));
        }
        let before: Vec<(String, u32)> = (0..index.len() as u32).map(|e| (index.path(e), index.parent(e))).collect();
        // Widened: a value past 3 bytes, then taken off again.
        for ids in [&mut index.name_of, &mut index.parents] {
            let len = ids.len();
            ids.push(0x0100_0000);
            ids.truncate(len);
            assert_eq!(ids.width(), 4);
        }
        let mut file = Vec::new();
        index.write_v6(&mut file).unwrap();
        assert_eq!(&file[V6_IDS..V6_IDS + 2], &[3, 3]);
        let loaded = Index::load_from(&file).unwrap();
        assert_eq!((loaded.name_of.width(), loaded.parents.width()), (3, 3));
        let after: Vec<(String, u32)> = (0..loaded.len() as u32).map(|e| (loaded.path(e), loaded.parent(e))).collect();
        assert_eq!(after, before);
        assert_eq!(loaded.parent(root), NONE);
    }
}
