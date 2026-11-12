//! v6: the dates made and opened, and the attributes, in less room, every value as it was (lossless).
//!
//! **Times** (`Coded`): most entries' date made is their date modified (52% on a PC of 9.2M entries), and their date opened
//! their date modified or made (36%). So each entry has a 2-bit code: its own value (kept in `values`, in entry order,
//! found by counting the entries before it that have one), the same as the first or the second other time (the caller
//! says which they are), or set aside (`spill`). Codes only change in place: an entry whose value no longer fits its
//! code (its file was written, so its date modified moved away from its date made) is set aside, not given a value of
//! its own (that would move every value after it). The next full write codes everything anew.
//!
//! **Attributes** (`Palette`): a PC has a few dozen different sets of attributes (52 on that one), so each entry has
//! a byte, the place of its set in a table of up to 255; past that, the rest are set aside.
//!
//! Both are columns of the mapped file (`store`), changed in place, with room to grow.

use crate::column::Column;
use std::collections::HashMap;

/// An entry's own value is in `values`.
pub const OWN: u64 = 0;
/// The same as the first other time (the date modified).
pub const FIRST: u64 = 1;
/// The same as the second other time (not known, for the date made; the date made, for the date opened).
pub const SECOND: u64 = 2;
/// Set aside, in `spill`.
pub const SPILLED: u64 = 3;

/// Every other bit of a word: where each 2-bit code's low bit is.
const LOW: u64 = 0x5555_5555_5555_5555;

/// One time per entry, as codes against two others (module doc).
#[derive(Default)]
pub struct Coded {
    /// 32 codes a word, entry `e` at bits `2 * (e % 32)`.
    pub codes: Column<u64>,
    /// How many entries before each word of codes have a value of their own: where the word's first one is.
    pub ranks: Column<u32>,
    /// The entries' own values, in entry order.
    pub values: Column<u32>,
    /// Entries set aside since the last full write, as (entry << 32 | value); an entry set aside again reuses its place.
    pub spill: Column<u64>,
    /// Entry → its place in `spill` (made from it when opened).
    spilled: HashMap<u32, u32>,
    len: usize,
}

impl Coded {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// From the columns of a file, checked against each other (`None`: they don't fit).
    pub fn from_columns(codes: Column<u64>, ranks: Column<u32>, values: Column<u32>, spill: Column<u64>, len: usize) -> Option<Coded> {
        if codes.len() != len.div_ceil(32) || ranks.len() != codes.len() {
            return None;
        }
        let mut coded = Coded { codes, ranks, values, spill, spilled: HashMap::new(), len };
        // The last word's codes past the end are 0 (own): they don't count.
        let owns = match coded.codes.len() {
            0 => 0,
            words => coded.ranks[words - 1] as usize + own_count(coded.codes[words - 1], coded.len - (words - 1) * 32),
        };
        if owns != coded.values.len() {
            return None;
        }
        for (place, &pair) in coded.spill.iter().enumerate() {
            let entry = (pair >> 32) as u32;
            if entry as usize >= len {
                return None;
            }
            coded.spilled.insert(entry, place as u32);
        }
        Some(coded)
    }

    /// Every value coded anew against the other two times (`first`, `second`: `None` when one can't stand for it).
    pub fn encode(len: usize, value: impl Fn(usize) -> (u32, Option<u32>, Option<u32>)) -> Coded {
        let mut codes = Vec::with_capacity(len.div_ceil(32));
        let mut ranks = Vec::with_capacity(len.div_ceil(32));
        let mut values = Vec::new();
        for word in 0..len.div_ceil(32) {
            ranks.push(values.len() as u32);
            let mut bits = 0u64;
            for e in word * 32..(word * 32 + 32).min(len) {
                let (own, first, second) = value(e);
                let code = if first == Some(own) {
                    FIRST
                } else if second == Some(own) {
                    SECOND
                } else {
                    values.push(own);
                    OWN
                };
                bits |= code << ((e % 32) * 2);
            }
            codes.push(bits);
        }
        Coded { codes: codes.into(), ranks: ranks.into(), values: values.into(), spill: Column::default(), spilled: HashMap::new(), len }
    }

    pub fn code(&self, e: usize) -> u64 {
        (self.codes[e / 32] >> ((e % 32) * 2)) & 3
    }

    /// Where an entry with a value of its own has it in `values`.
    fn rank(&self, e: usize) -> usize {
        self.ranks[e / 32] as usize + own_count(self.codes[e / 32], e % 32)
    }

    /// Entry `e`'s value; `first` and `second` give the other two times, asked only when its code says so.
    #[inline]
    pub fn get(&self, e: usize, first: impl FnOnce() -> u32, second: impl FnOnce() -> u32) -> u32 {
        match self.code(e) {
            OWN => self.values[self.rank(e)],
            FIRST => first(),
            SECOND => second(),
            _ => self.spill[self.spilled[&(e as u32)] as usize] as u32,
        }
    }

    /// One more entry, with a value of its own (the next full write codes it).
    pub fn push(&mut self, value: u32) {
        if self.len % 32 == 0 {
            self.ranks.push(self.values.len() as u32);
            self.codes.push(0);
        }
        self.values.push(value);
        self.len += 1;
    }

    pub fn reserve(&mut self, more: usize) {
        self.codes.reserve_exact(more.div_ceil(32) + 1);
        self.ranks.reserve_exact(more.div_ceil(32) + 1);
        self.values.reserve_exact(more);
    }

    /// Entry `e`'s value is now `value`, the other two times now `first` and `second` (`None`: they can't stand for it).
    pub fn set(&mut self, e: usize, value: u32, first: Option<u32>, second: Option<u32>) {
        match self.code(e) {
            OWN => {
                let at = self.rank(e);
                self.values[at] = value;
            }
            _ if first == Some(value) => self.set_code(e, FIRST),
            _ if second == Some(value) => self.set_code(e, SECOND),
            _ => {
                let pair = (e as u64) << 32 | value as u64;
                match self.spilled.get(&(e as u32)) {
                    Some(&place) => self.spill[place as usize] = pair,
                    None => {
                        self.spilled.insert(e as u32, self.spill.len() as u32);
                        self.spill.push(pair);
                    }
                }
                self.set_code(e, SPILLED);
            }
        }
    }

    fn set_code(&mut self, e: usize, code: u64) {
        debug_assert!(code != OWN, "a value of its own is given only by a full write");
        let shift = (e % 32) * 2;
        let word = &mut self.codes[e / 32];
        *word = (*word & !(3 << shift)) | code << shift;
    }

    /// Only the entries `keep` says, in the same order (their codes still stand: what they're the same as moves with
    /// them). In place.
    pub fn compact(&mut self, keep: impl Fn(usize) -> bool) {
        let old = self.len;
        let spilled: Vec<(usize, u32)> = (0..old).filter(|&e| self.code(e) == SPILLED && keep(e))
            .map(|e| (e, self.spill[self.spilled[&(e as u32)] as usize] as u32)).collect();
        let (mut to, mut own_from, mut own_to) = (0usize, 0usize, 0usize);
        let mut word = 0u64;
        let mut renumbered = Vec::with_capacity(spilled.len());
        let mut next_spilled = spilled.iter().peekable();
        for e in 0..old {
            let code = self.code(e);
            let own = code == OWN;
            if keep(e) {
                if to % 32 == 0 {
                    if to > 0 {
                        self.codes[to / 32 - 1] = word;
                    }
                    self.ranks[to / 32] = own_to as u32;
                    word = 0;
                }
                word |= code << ((to % 32) * 2);
                if own {
                    self.values[own_to] = self.values[own_from];
                    own_to += 1;
                }
                if code == SPILLED && next_spilled.peek().is_some_and(|&&(at, _)| at == e) {
                    renumbered.push((to, next_spilled.next().unwrap().1));
                }
                to += 1;
            }
            if own {
                own_from += 1;
            }
        }
        if to > 0 {
            self.codes[(to - 1) / 32] = word;
        }
        self.codes.truncate(to.div_ceil(32));
        self.ranks.truncate(to.div_ceil(32));
        self.values.truncate(own_to);
        self.spill.clear();
        self.spilled.clear();
        for (e, value) in renumbered {
            self.spilled.insert(e as u32, self.spill.len() as u32);
            self.spill.push((e as u64) << 32 | value as u64);
        }
        self.len = to;
        for column in [&mut self.codes, &mut self.spill] {
            column.shrink_to_fit();
        }
        self.ranks.shrink_to_fit();
        self.values.shrink_to_fit();
    }

    pub fn shrink_to_fit(&mut self) {
        self.codes.shrink_to_fit();
        self.ranks.shrink_to_fit();
        self.values.shrink_to_fit();
        self.spill.shrink_to_fit();
    }

    pub fn is_mapped(&self) -> bool {
        self.codes.is_mapped() && self.ranks.is_mapped() && self.values.is_mapped() && self.spill.is_mapped()
    }

    /// Bytes in use, and bytes there's room for.
    pub fn bytes(&self) -> (usize, usize) {
        (self.codes.len() * 8 + self.ranks.len() * 4 + self.values.len() * 4 + self.spill.len() * 8,
         self.codes.capacity() * 8 + self.ranks.capacity() * 4 + self.values.capacity() * 4 + self.spill.capacity() * 8)
    }

    /// Excavate's own memory it takes (the mapped columns take none; the map of what's set aside does).
    pub fn heap_bytes(&self) -> usize {
        self.codes.heap_bytes() + self.ranks.heap_bytes() + self.values.heap_bytes() + self.spill.heap_bytes() + self.spilled.capacity() * 9
    }

    pub fn spilled(&self) -> usize {
        self.spilled.len()
    }

    /// The values of the 32 entries of word `w` (fewer at the end) into `out`, given their other two times: a scan's
    /// way, with locals the compiler keeps in registers and no branch on the codes.
    #[inline]
    pub fn fill(&self, w: usize, first: &[u32], second: &[u32], out: &mut [u32]) {
        let mut word = self.codes[w];
        let mut rank = self.ranks[w] as usize;
        let values: &[u32] = &self.values;
        for k in 0..out.len() {
            let code = (word & 3) as usize;
            word >>= 2;
            let own = if rank < values.len() { values[rank] } else { 0 };
            let mut value = [own, first[k], second[k], 0][code];
            if code == SPILLED as usize {
                value = self.spill[self.spilled[&((w * 32 + k) as u32)] as usize] as u32;
            }
            rank += (code == OWN as usize) as usize;
            out[k] = value;
        }
    }

    /// Reads the values in entry order from `from` (a multiple of 32), a word of codes at a time: what a scan of every
    /// entry uses, with no count of the entries before each and no branch on its code.
    pub fn cursor(&self, from: usize) -> Cursor<'_> {
        debug_assert!(from % 32 == 0);
        Cursor { coded: self, at: from, word: 0, rank: self.ranks.get(from / 32).map_or(self.values.len(), |&rank| rank as usize) }
    }
}

/// `Coded::cursor`.
pub struct Cursor<'a> {
    coded: &'a Coded,
    at: usize,
    word: u64,
    rank: usize,
}

impl Cursor<'_> {
    /// The next entry's value, given its two other times.
    #[inline]
    pub fn next(&mut self, first: u32, second: u32) -> u32 {
        if self.at % 32 == 0 {
            self.word = self.coded.codes[self.at / 32];
        }
        let code = (self.word & 3) as usize;
        self.word >>= 2;
        let own = self.coded.values.get(self.rank).copied().unwrap_or(0);
        let mut value = [own, first, second, 0][code];
        if code == SPILLED as usize {
            value = self.coded.spill[self.coded.spilled[&(self.at as u32)] as usize] as u32;
        }
        self.rank += (code == OWN as usize) as usize;
        self.at += 1;
        value
    }
}

/// How many of a word's first `count` codes are `OWN` (00).
#[inline]
fn own_count(word: u64, count: usize) -> usize {
    let own = !(word | (word >> 1)) & LOW;
    let below = if count >= 32 { u64::MAX } else { (1u64 << (count * 2)) - 1 };
    (own & below).count_ones() as usize
}

/// The value set aside for an attributes palette that's full.
pub const RARE: u8 = 255;

/// One set of attributes per entry, as its place in a table of them (module doc).
#[derive(Default)]
pub struct Palette {
    pub codes: Column<u8>,
    /// The sets of attributes, at most 255.
    pub values: Vec<u16>,
    /// Each set's place in `values` (`RARE`: not there).
    places: Vec<u8>,
    /// Entries whose set didn't fit in a full table.
    pub rare: HashMap<u32, u16>,
}

impl Palette {
    pub fn from_parts(codes: Column<u8>, values: Vec<u16>, rare: HashMap<u32, u16>) -> Option<Palette> {
        if values.len() > RARE as usize {
            return None;
        }
        let mut palette = Palette { codes, values, places: vec![RARE; 1 << 16], rare };
        for (place, &value) in palette.values.iter().enumerate() {
            palette.places[value as usize] = place as u8;
        }
        let known = palette.values.len() as u8;
        palette.codes.iter().enumerate().all(|(e, &code)| code < known || (code == RARE && palette.rare.contains_key(&(e as u32))))
            .then_some(palette)
    }

    pub fn len(&self) -> usize {
        self.codes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.codes.is_empty()
    }

    #[inline]
    pub fn get(&self, e: usize) -> u16 {
        match self.codes[e] {
            RARE => self.rare.get(&(e as u32)).copied().unwrap_or(0),
            code => self.values[code as usize],
        }
    }

    fn code_of(&mut self, value: u16) -> u8 {
        if self.places.is_empty() {
            self.places = vec![RARE; 1 << 16];
        }
        match self.places[value as usize] {
            RARE if self.values.len() < RARE as usize => {
                self.places[value as usize] = self.values.len() as u8;
                self.values.push(value);
                self.values.len() as u8 - 1
            }
            code => code,
        }
    }

    pub fn push(&mut self, value: u16) {
        let code = self.code_of(value);
        if code == RARE {
            self.rare.insert(self.codes.len() as u32, value);
        }
        self.codes.push(code);
    }

    pub fn set(&mut self, e: usize, value: u16) {
        let code = self.code_of(value);
        if code == RARE {
            self.rare.insert(e as u32, value);
        } else if self.codes[e] == RARE {
            self.rare.remove(&(e as u32));
        }
        self.codes[e] = code;
    }

    /// Only the entries `keep` says, in the same order, in place.
    pub fn compact(&mut self, keep: impl Fn(usize) -> bool) {
        let mut rare = HashMap::new();
        let mut to = 0;
        for e in 0..self.codes.len() {
            if keep(e) {
                let code = self.codes[e];
                if code == RARE {
                    rare.insert(to as u32, self.rare.get(&(e as u32)).copied().unwrap_or(0));
                }
                self.codes[to] = code;
                to += 1;
            }
        }
        self.codes.truncate(to);
        self.codes.shrink_to_fit();
        self.rare = rare;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A model of three times per entry, against what `Coded` gives back after every kind of change.
    #[test]
    fn times_as_codes_round_trip() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut random = move |below: u32| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % below as u64) as u32
        };
        let n = 1000;
        // (modified, created, accessed), raw; modified may be "odd" (None: can't stand for anything).
        let mut model: Vec<(Option<u32>, u32, u32)> = (0..n).map(|_| {
            let modified = random(5) + 1;
            let created = if random(2) == 0 { modified } else { random(5) };
            let accessed = match random(3) { 0 => modified, 1 => created, _ => random(7) };
            (if random(50) == 0 { None } else { Some(modified) }, created, accessed)
        }).collect();
        let encode = |model: &Vec<(Option<u32>, u32, u32)>| {
            let created = Coded::encode(model.len(), |e| (model[e].1, model[e].0, Some(0)));
            let accessed = Coded::encode(model.len(), |e| (model[e].2, model[e].0, Some(model[e].1)));
            (created, accessed)
        };
        let check = |model: &Vec<(Option<u32>, u32, u32)>, created: &Coded, accessed: &Coded| {
            for (e, &(modified, made, opened)) in model.iter().enumerate() {
                let c = created.get(e, || modified.unwrap(), || 0);
                assert_eq!(c, made, "made, entry {e}");
                assert_eq!(accessed.get(e, || modified.unwrap(), || c), opened, "opened, entry {e}");
            }
            // In order, from any word: the same.
            for from in [0, 32, 64 * 3] {
                let (mut made, mut seen) = (created.cursor(from), accessed.cursor(from));
                for (e, &(modified, c, a)) in model.iter().enumerate().skip(from) {
                    let first = modified.unwrap_or(u32::MAX);
                    assert_eq!(made.next(first, 0), c, "made in order, entry {e}");
                    assert_eq!(seen.next(first, c), a, "opened in order, entry {e}");
                }
            }
        };
        let (mut created, mut accessed) = encode(&model);
        check(&model, &created, &accessed);
        for round in 0..20_000 {
            match random(4) {
                // The file written: its date modified moves; the other two stay as they were.
                0 => {
                    let e = random(model.len() as u32) as usize;
                    let (old_c, old_a) = (model[e].1, model[e].2);
                    model[e].0 = if random(30) == 0 { None } else { Some(random(9) + 1) };
                    created.set(e, old_c, model[e].0, Some(0));
                    accessed.set(e, old_a, model[e].0, Some(old_c));
                }
                // New dates made and opened.
                1 => {
                    let e = random(model.len() as u32) as usize;
                    let modified = model[e].0;
                    let made = if random(2) == 0 { modified.unwrap_or(3) } else { random(9) };
                    let opened = match random(3) { 0 => modified.unwrap_or(4), 1 => made, _ => random(9) };
                    model[e].1 = made;
                    model[e].2 = opened;
                    created.set(e, made, modified, Some(0));
                    accessed.set(e, opened, modified, Some(made));
                }
                // A new entry.
                2 => {
                    let modified = Some(random(9) + 1);
                    let made = random(9);
                    model.push((modified, made, made));
                    created.push(made);
                    accessed.push(made);
                }
                // Entries taken out, now and then; or everything coded anew.
                _ if round % 997 == 0 => {
                    let gone: Vec<bool> = (0..model.len()).map(|_| random(4) == 0).collect();
                    created.compact(|e| !gone[e]);
                    accessed.compact(|e| !gone[e]);
                    model = model.iter().zip(&gone).filter(|(_, &g)| !g).map(|(m, _)| *m).collect();
                    assert_eq!(created.len(), model.len());
                }
                _ if round % 1999 == 0 => (created, accessed) = encode(&model),
                _ => {}
            }
        }
        check(&model, &created, &accessed);
        // Opened again from its columns, as from the file.
        let created = Coded::from_columns(created.codes.clone(), created.ranks.clone(), created.values.clone(), created.spill.clone(), created.len)
            .expect("fits");
        let accessed = Coded::from_columns(accessed.codes.clone(), accessed.ranks.clone(), accessed.values.clone(), accessed.spill.clone(), accessed.len)
            .expect("fits");
        check(&model, &created, &accessed);
    }

    #[test]
    fn attributes_as_a_palette() {
        let mut palette = Palette::default();
        for e in 0..600u32 {
            palette.push((e % 300) as u16);
        }
        assert_eq!(palette.values.len(), 255);
        assert_eq!(palette.rare.len(), 600 - 255 - 255, "a full table sets the rest aside");
        for e in 0..600 {
            assert_eq!(palette.get(e), (e % 300) as u16);
        }
        palette.set(299, 7);
        assert_eq!(palette.get(299), 7);
        assert!(!palette.rare.contains_key(&299));
        palette.compact(|e| e % 2 == 1);
        for e in 0..300 {
            let old = e * 2 + 1;
            assert_eq!(palette.get(e), if old == 299 { 7 } else { (old % 300) as u16 });
        }
        let again = Palette::from_parts(palette.codes.clone(), palette.values.clone(), palette.rare.clone()).expect("fits");
        assert!((0..300).all(|e| again.get(e) == palette.get(e)));
    }
}
