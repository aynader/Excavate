//! Excavate Bedrock, the service: each NTFS drive's file table read with the rights only a service has, and sent to
//! Excavate over a pipe as names and facts. That's the ~10 s build and every folder (the ones a person's own rights
//! can't list too), without asking Windows for administrator rights each time.
//!
//! - **What it sends:** for each record of a drive's `$MFT` in use, its number, its names with their folders' record
//!   numbers, its size, dates and attributes (`mft::Record`). Never a handle, never a file's contents, nothing but
//!   what a listing shows.
//! - **Other people's files:** the folders of the PC's other accounts (`C:\Users\<someone else>`) are left out by the
//!   service itself before sending (`Filter`), so asking the pipe directly shows a person no more than Excavate does.
//! - **The pipe:** `\\.\pipe\excavate-bedrock`, local only, any signed-in user may connect (each is told only about
//!   their own files and the shared ones). One request per connection: `Hello` (the version, the drives it reads) or
//!   `List` (some drives).
//! - **Running:** started when Excavate asks (`connect`), ended after a minute with nothing asked (`serve`), so it
//!   holds no memory at rest.
//!
//! The wire format is little-endian, in frames: a kind byte, the payload's length (u32), the payload. A request is
//! `BDRK`, the protocol version (u16), then the command. Every decoder takes any bytes without panicking (`tests`).

use crate::mft::{self, Record};
use std::io::{Read, Write};
use std::time::Duration;

/// The pipe the service answers on.
pub const PIPE: &str = r"\\.\pipe\excavate-bedrock";
/// The service's name with Windows, and the name people see.
pub const SERVICE: &str = "ExcavateBedrock";
pub const DISPLAY_NAME: &str = "Excavate Bedrock";
/// Raised whenever a frame or request changes meaning.
pub const VERSION: u16 = 1;
const MAGIC: &[u8; 4] = b"BDRK";
/// No frame is larger (a batch of records is cut well before).
const MOST: usize = 8 << 20;
/// A batch of records is sent once it's this large.
const BATCH: usize = 256 << 10;
/// The service ends after this long without a request.
pub const IDLE: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Hello,
    /// The drives to read ("C:"), in order.
    List(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    /// The service's version and the drives it can read (NTFS, fixed).
    Hello { version: u16, drives: Vec<String> },
    /// A drive's records begin: how many the MFT holds (room for them is made at once).
    Drive { drive: String, records: u64 },
    Records(Vec<(u64, Record)>),
    /// A drive's records end; `error` says why when it couldn't be read (then it's listed folder by folder).
    DriveDone { drive: String, error: String },
    /// Nothing more for this request.
    Done,
}

// ---- Encoding ----

fn put_str(out: &mut Vec<u8>, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    put_wide(out, &wide);
}

fn put_wide(out: &mut Vec<u8>, wide: &[u16]) {
    out.extend_from_slice(&(wide.len().min(u16::MAX as usize) as u16).to_le_bytes());
    for c in wide.iter().take(u16::MAX as usize) {
        out.extend_from_slice(&c.to_le_bytes());
    }
}

pub fn encode_request(request: &Request) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&VERSION.to_le_bytes());
    match request {
        Request::Hello => out.push(1),
        Request::List(drives) => {
            out.push(2);
            out.extend_from_slice(&(drives.len() as u16).to_le_bytes());
            for drive in drives {
                put_str(&mut out, drive);
            }
        }
    }
    out
}

fn put_record(out: &mut Vec<u8>, number: u64, record: &Record) {
    out.extend_from_slice(&number.to_le_bytes());
    out.extend_from_slice(&record.base.to_le_bytes());
    let flags = record.is_dir as u8
        | (record.size.is_some() as u8) << 1
        | (record.written.is_some() as u8) << 2
        | (record.created.is_some() as u8) << 3
        | (record.accessed.is_some() as u8) << 4;
    out.push(flags);
    if let Some(size) = record.size { out.extend_from_slice(&size.to_le_bytes()); }
    for time in [record.written, record.created, record.accessed].into_iter().flatten() {
        out.extend_from_slice(&time.to_le_bytes());
    }
    out.extend_from_slice(&record.attributes.to_le_bytes());
    out.extend_from_slice(&(record.names.len().min(u16::MAX as usize) as u16).to_le_bytes());
    for (parent, name) in record.names.iter().take(u16::MAX as usize) {
        out.extend_from_slice(&parent.to_le_bytes());
        put_wide(out, name);
    }
}

fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 5);
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

pub fn encode_frame(item: &Frame) -> Vec<u8> {
    let mut p = Vec::new();
    match item {
        Frame::Hello { version, drives } => {
            p.extend_from_slice(&version.to_le_bytes());
            p.extend_from_slice(&(drives.len() as u16).to_le_bytes());
            for drive in drives { put_str(&mut p, drive); }
            frame(1, &p)
        }
        Frame::Drive { drive, records } => {
            put_str(&mut p, drive);
            p.extend_from_slice(&records.to_le_bytes());
            frame(2, &p)
        }
        Frame::Records(records) => {
            p.extend_from_slice(&(records.len() as u32).to_le_bytes());
            for (number, record) in records { put_record(&mut p, *number, record); }
            frame(3, &p)
        }
        Frame::DriveDone { drive, error } => {
            put_str(&mut p, drive);
            put_str(&mut p, error);
            frame(4, &p)
        }
        Frame::Done => frame(5, &p),
    }
}

// ---- Decoding: any bytes, never a panic ----

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let out = self.bytes.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(out)
    }
    fn u8(&mut self) -> Option<u8> { Some(self.take(1)?[0]) }
    fn u16(&mut self) -> Option<u16> { Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?)) }
    fn u32(&mut self) -> Option<u32> { Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?)) }
    fn u64(&mut self) -> Option<u64> { Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?)) }
    fn wide(&mut self) -> Option<Vec<u16>> {
        let len = self.u16()? as usize;
        Some(self.take(len * 2)?.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect())
    }
    fn text(&mut self) -> Option<String> { String::from_utf16(&self.wide()?).ok() }
    fn done(&self) -> bool { self.at == self.bytes.len() }
}

pub fn decode_request(bytes: &[u8]) -> Option<Request> {
    let mut c = Cursor { bytes, at: 0 };
    if c.take(4)? != MAGIC || c.u16()? != VERSION {
        return None;
    }
    let request = match c.u8()? {
        1 => Request::Hello,
        2 => {
            let n = c.u16()? as usize;
            let mut drives = Vec::with_capacity(n.min(26));
            for _ in 0..n {
                drives.push(c.text()?);
            }
            Request::List(drives)
        }
        _ => return None,
    };
    c.done().then_some(request)
}

fn decode_record(c: &mut Cursor) -> Option<(u64, Record)> {
    let number = c.u64()?;
    let base = c.u64()?;
    let flags = c.u8()?;
    let size = if flags & 2 != 0 { Some(c.u64()?) } else { None };
    let mut time = |bit: u8| -> Option<Option<i64>> { Ok::<_, ()>(if flags & bit != 0 { Some(c.u64()? as i64) } else { None }).ok() };
    let written = time(4)?;
    let created = time(8)?;
    let accessed = time(16)?;
    let attributes = c.u32()?;
    let count = c.u16()? as usize;
    let mut names = Vec::with_capacity(count.min(64));
    for _ in 0..count {
        let parent = c.u64()?;
        names.push((parent, c.wide()?));
    }
    Some((number, Record { is_dir: flags & 1 != 0, base, names, size, written, created, accessed, attributes }))
}

pub fn decode_frame(kind: u8, payload: &[u8]) -> Option<Frame> {
    let mut c = Cursor { bytes: payload, at: 0 };
    let item = match kind {
        1 => {
            let version = c.u16()?;
            let n = c.u16()? as usize;
            let mut drives = Vec::with_capacity(n.min(26));
            for _ in 0..n { drives.push(c.text()?); }
            Frame::Hello { version, drives }
        }
        2 => Frame::Drive { drive: c.text()?, records: c.u64()? },
        3 => {
            let n = c.u32()? as usize;
            // Each record takes at least 31 bytes: a count that couldn't fit is a bad frame, not a huge allocation.
            if n > payload.len() / 31 + 1 {
                return None;
            }
            let mut records = Vec::with_capacity(n);
            for _ in 0..n { records.push(decode_record(&mut c)?); }
            Frame::Records(records)
        }
        4 => Frame::DriveDone { drive: c.text()?, error: c.text()? },
        5 => Frame::Done,
        _ => return None,
    };
    c.done().then_some(item)
}

fn invalid(what: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, what.to_string())
}

/// The next frame, or None at the end of the stream.
pub fn read_frame(from: &mut impl Read) -> std::io::Result<Option<Frame>> {
    let mut head = [0u8; 5];
    match from.read_exact(&mut head) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > MOST {
        return Err(invalid("a frame too large"));
    }
    let mut payload = vec![0u8; len];
    from.read_exact(&mut payload)?;
    decode_frame(head[0], &payload).map(Some).ok_or_else(|| invalid("a frame that doesn't read"))
}

/// A request as sent: its length (u32), then the request.
pub fn write_request(to: &mut impl Write, request: &Request) -> std::io::Result<()> {
    let bytes = encode_request(request);
    to.write_all(&(bytes.len() as u32).to_le_bytes())?;
    to.write_all(&bytes)?;
    to.flush()
}

pub fn read_request(from: &mut impl Read) -> std::io::Result<Request> {
    let mut len = [0u8; 4];
    from.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > 4096 {
        return Err(invalid("a request too large"));
    }
    let mut bytes = vec![0u8; len];
    from.read_exact(&mut bytes)?;
    decode_request(&bytes).ok_or_else(|| invalid("a request that doesn't read"))
}

// ---- Leaving other people's folders out ----

const UNKNOWN: u8 = 0;
const KEEP: u8 = 1;
const HIDE: u8 = 2;
const NO_PARENT: u32 = u32::MAX;

/// Decides, record by record in the order the MFT gives them, which names to send: a name is left out when it's
/// inside a hidden folder (another account's profile), at any depth. A record whose folders haven't come yet waits
/// until the end (`rest`). Memory: 5 bytes a record while a drive is read (~50 MB for 10M records), then let go.
pub struct Filter {
    verdict: Vec<u8>,
    dir_parent: Vec<u32>,
    waiting: Vec<(u64, Record)>,
    chain: Vec<u32>,
}

impl Filter {
    /// `records`: how many the MFT holds; `hidden`: the record numbers of the folders whose insides are left out.
    pub fn new(records: usize, hidden: &[u64]) -> Filter {
        let mut verdict = vec![UNKNOWN; records];
        let dir_parent = vec![NO_PARENT; records];
        for (n, v) in verdict.iter_mut().enumerate().take(16) {
            // NTFS's own records and the root: shown.
            let _ = n;
            *v = KEEP;
        }
        for &h in hidden {
            if let Some(v) = verdict.get_mut(h as usize) {
                *v = HIDE;
            }
        }
        Filter { verdict, dir_parent, waiting: Vec::new(), chain: Vec::new() }
    }

    /// Whether what's inside folder `at` is shown: KEEP, HIDE, or UNKNOWN when a folder on the way hasn't come yet.
    fn inside(&mut self, at: u64) -> u8 {
        let Some(&first) = self.verdict.get(at as usize) else { return HIDE };
        if first != UNKNOWN {
            return first;
        }
        self.chain.clear();
        let mut x = at as u32;
        let found = loop {
            let v = self.verdict[x as usize];
            if v != UNKNOWN {
                break v;
            }
            self.chain.push(x);
            // A loop (a damaged table) is never followed for long, and what's in it isn't sent.
            if self.chain.len() > 4096 {
                break HIDE;
            }
            let parent = self.dir_parent[x as usize];
            if parent == NO_PARENT || parent as usize >= self.verdict.len() {
                return UNKNOWN;
            }
            x = parent;
        };
        for &c in &self.chain {
            self.verdict[c as usize] = found;
        }
        found
    }

    /// What of this record to send now: its names outside hidden folders (None: none, or it waits).
    pub fn offer(&mut self, number: u64, mut record: Record) -> Option<(u64, Record)> {
        if record.base == 0 && record.is_dir {
            if let (Some(slot), Some(&(parent, _))) = (self.dir_parent.get_mut(number as usize), record.names.first()) {
                if number >= 16 {
                    *slot = parent.min(NO_PARENT as u64 - 1) as u32;
                }
            }
        }
        if record.names.is_empty() {
            return Some((number, record));
        }
        let verdicts: Vec<u8> = record.names.iter().map(|&(parent, _)| self.inside(parent)).collect();
        if verdicts.contains(&UNKNOWN) {
            self.waiting.push((number, record));
            return None;
        }
        let mut v = verdicts.into_iter();
        record.names.retain(|_| v.next() == Some(KEEP));
        (!record.names.is_empty()).then_some((number, record))
    }

    /// The records that waited, decided now that every folder has come (those whose folders never came go: the
    /// index would leave them out anyway).
    pub fn rest(mut self) -> Vec<(u64, Record)> {
        let waiting = std::mem::take(&mut self.waiting);
        let mut out = Vec::new();
        for (number, mut record) in waiting {
            let verdicts: Vec<u8> = record.names.iter().map(|&(parent, _)| self.inside(parent)).collect();
            let mut v = verdicts.into_iter();
            record.names.retain(|_| v.next() == Some(KEEP));
            if !record.names.is_empty() {
                out.push((number, record));
            }
        }
        out
    }
}

// ---- Serving ----

/// What the server needs from the PC (the real one, or a test's): drives, their volumes, and who may see what.
pub trait Source: Sync {
    /// The drives that can be read ("C:").
    fn drives(&self) -> Vec<String>;
    /// The drive's volume, opened for raw reading, or the Windows error.
    fn open(&self, drive: &str) -> Result<Box<dyn mft::ReadSeek + Send>, u32>;
    /// The record numbers of the folders on `drive` whose insides `user` (a SID, "S-1-5-21-…") isn't shown.
    fn hidden(&self, user: &str, drive: &str) -> Vec<u64>;
}

/// Answers `request` (read from the connection first: Windows says who sent it only after that), for `user`.
pub fn answer(source: &dyn Source, user: &str, request: Request, connection: &mut impl Write) -> std::io::Result<()> {
    let mut out = std::io::BufWriter::with_capacity(BATCH + (64 << 10), connection);
    match request {
        Request::Hello => {
            out.write_all(&encode_frame(&Frame::Hello { version: VERSION, drives: source.drives() }))?;
        }
        Request::List(drives) => {
            let readable = source.drives();
            for drive in drives {
                let drive = drive.trim_end_matches('\\').to_uppercase();
                if !readable.iter().any(|d| d.eq_ignore_ascii_case(&drive)) {
                    out.write_all(&encode_frame(&Frame::DriveDone { drive, error: "not a drive this service reads".into() }))?;
                    continue;
                }
                let error = list(source, user, &drive, &mut out).err().map(|e| e.to_string()).unwrap_or_default();
                out.write_all(&encode_frame(&Frame::DriveDone { drive, error }))?;
            }
        }
    }
    out.write_all(&encode_frame(&Frame::Done))?;
    out.flush()
}

/// One drive's listing on its way out: the filter, the batch being filled, and the first error writing.
struct Sending<'a, W: Write> {
    out: &'a mut W,
    filter: Option<Filter>,
    batch: Vec<(u64, Record)>,
    size: usize,
    failed: Option<std::io::Error>,
}

impl<W: Write> Sending<'_, W> {
    fn write(&mut self, item: &Frame) {
        if self.failed.is_none() {
            if let Err(error) = self.out.write_all(&encode_frame(item)) {
                self.failed = Some(error);
            }
        }
    }

    fn push(&mut self, item: (u64, Record)) {
        self.size += 31 + item.1.names.iter().map(|(_, n)| 10 + n.len() * 2).sum::<usize>();
        self.batch.push(item);
        if self.size >= BATCH {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if !self.batch.is_empty() {
            let batch = Frame::Records(std::mem::take(&mut self.batch));
            self.write(&batch);
        }
        self.size = 0;
    }
}

fn list(source: &dyn Source, user: &str, drive: &str, out: &mut impl Write) -> std::io::Result<()> {
    let mut volume = source.open(drive).map_err(|code| std::io::Error::other(format!("couldn't open it (Windows error {code})")))?;
    let hidden = source.hidden(user, drive);
    let sending = std::cell::RefCell::new(Sending { out, filter: None, batch: Vec::new(), size: 0, failed: None });
    let read = mft::read_records(&mut volume, |records| {
        let mut s = sending.borrow_mut();
        s.filter = Some(Filter::new(records, &hidden));
        s.write(&Frame::Drive { drive: drive.to_string(), records: records as u64 });
    }, |number, record| {
        let mut s = sending.borrow_mut();
        if s.failed.is_some() {
            return;
        }
        let kept = s.filter.as_mut().and_then(|filter| filter.offer(number, record));
        if let Some(item) = kept {
            s.push(item);
        }
    });
    let mut s = sending.into_inner();
    if let Some(error) = s.failed.take() {
        return Err(error);
    }
    read?;
    if let Some(filter) = s.filter.take() {
        for item in filter.rest() {
            s.push(item);
        }
    }
    s.flush();
    s.failed.map_or(Ok(()), Err)
}

// ---- The client ----

/// What a listing from the service gives, drive by drive.
pub enum Listed {
    Drive { drive: String, records: u64 },
    Records(Vec<(u64, Record)>),
    DriveDone { drive: String, error: String },
}

/// Lists `drives` through the pipe at `pipe`, giving each part to `each` as it comes.
pub fn list_through(pipe: &str, drives: &[String], mut each: impl FnMut(Listed)) -> std::io::Result<()> {
    let mut connection = std::fs::OpenOptions::new().read(true).write(true).open(pipe)?;
    write_request(&mut connection, &Request::List(drives.to_vec()))?;
    let mut from = std::io::BufReader::with_capacity(1 << 20, connection);
    loop {
        match read_frame(&mut from)? {
            Some(Frame::Drive { drive, records }) => each(Listed::Drive { drive, records }),
            Some(Frame::Records(records)) => each(Listed::Records(records)),
            Some(Frame::DriveDone { drive, error }) => each(Listed::DriveDone { drive, error }),
            Some(Frame::Done) => return Ok(()),
            Some(Frame::Hello { .. }) => return Err(invalid("an answer out of place")),
            None => return Err(invalid("the service stopped answering")),
        }
    }
}

/// The service's version and the drives it reads, through the pipe at `pipe`.
pub fn hello_through(pipe: &str) -> std::io::Result<(u16, Vec<String>)> {
    let mut connection = std::fs::OpenOptions::new().read(true).write(true).open(pipe)?;
    write_request(&mut connection, &Request::Hello)?;
    match read_frame(&mut connection)? {
        Some(Frame::Hello { version, drives }) => Ok((version, drives)),
        _ => Err(invalid("not a hello")),
    }
}

#[cfg(windows)]
pub use self::windows::*;

#[cfg(windows)]
mod windows {
    use super::*;
    use std::os::windows::io::FromRawHandle;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Instant;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, LocalFree, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW};
    use windows_sys::Win32::Security::{GetTokenInformation, RevertToSelf, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::Storage::FileSystem::{PIPE_ACCESS_DUPLEX};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, ImpersonateNamedPipeClient, WaitNamedPipeW, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus, StartServiceW, SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS,
        SERVICE_RUNNING, SERVICE_START, SERVICE_STATUS,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

    /// A name as Windows takes it, ending in a nul (not a path: no `\\?\` in front, unlike `walk::wide`).
    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    /// Who is on the other end of a connected pipe: their SID ("S-1-5-21-…").
    fn client_of(pipe: HANDLE) -> Option<String> {
        unsafe {
            if ImpersonateNamedPipeClient(pipe) == 0 {
                return None;
            }
            let mut token: HANDLE = std::ptr::null_mut();
            let opened = OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) != 0;
            RevertToSelf();
            if !opened {
                return None;
            }
            let mut buffer = vec![0u64; 64];
            let mut needed = 0u32;
            let ok = GetTokenInformation(token, TokenUser, buffer.as_mut_ptr() as *mut _, (buffer.len() * 8) as u32, &mut needed) != 0;
            CloseHandle(token);
            if !ok {
                return None;
            }
            let user = &*(buffer.as_ptr() as *const TOKEN_USER);
            let mut text: *mut u16 = std::ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
                return None;
            }
            let len = (0..).take_while(|&i| *text.add(i) != 0).count();
            let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
            LocalFree(text as *mut _);
            Some(sid)
        }
    }

    /// The pipe's security: the service (SYSTEM) and administrators fully; any signed-in user may connect, read and
    /// write (each is shown only their own files and the shared ones).
    const PIPE_SECURITY: &str = "D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;AU)";

    fn new_instance(name: &str, first: bool) -> Option<HANDLE> {
        unsafe {
            let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(wide(PIPE_SECURITY).as_ptr(), 1, &mut descriptor, std::ptr::null_mut()) == 0 {
                return None;
            }
            let attributes = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: descriptor, bInheritHandle: 0 };
            // FILE_FLAG_FIRST_PIPE_INSTANCE: no one else may have made a pipe of this name first.
            let mode = PIPE_ACCESS_DUPLEX | if first { 0x0008_0000 } else { 0 };
            let handle = CreateNamedPipeW(wide(name).as_ptr(), mode, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                                          8, 1 << 20, 4096, 0, &attributes);
            LocalFree(descriptor as *mut _);
            (handle != INVALID_HANDLE_VALUE).then_some(handle)
        }
    }

    /// Serves requests on the pipe `name` until `stop` is set, or `idle` passes without one. Each connection is
    /// answered on a thread of its own. Err when the pipe couldn't be made (another has its name).
    pub fn serve(source: Arc<dyn Source + Send>, name: &str, idle: Duration, stop: Arc<AtomicBool>) -> Result<(), String> {
        let busy = Arc::new(AtomicUsize::new(0));
        let last = Arc::new(std::sync::Mutex::new(Instant::now()));
        let mut first = true;
        // Wakes the waiting connect when it's time to end (connecting to itself).
        {
            let (stop, busy, last, name) = (stop.clone(), busy.clone(), last.clone(), name.to_string());
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_millis(500));
                let quiet = busy.load(Ordering::SeqCst) == 0 && last.lock().unwrap().elapsed() >= idle;
                if quiet {
                    stop.store(true, Ordering::SeqCst);
                }
                if stop.load(Ordering::SeqCst) {
                    let _ = std::fs::OpenOptions::new().read(true).write(true).open(&name);
                    return;
                }
            });
        }
        loop {
            let Some(pipe) = new_instance(name, first) else {
                return Err(format!("couldn't make the pipe (Windows error {})", unsafe { GetLastError() }));
            };
            first = false;
            let connected = unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) != 0 || GetLastError() == ERROR_PIPE_CONNECTED };
            if stop.load(Ordering::SeqCst) {
                unsafe { CloseHandle(pipe) };
                return Ok(());
            }
            if !connected {
                unsafe { CloseHandle(pipe) };
                continue;
            }
            *last.lock().unwrap() = Instant::now();
            busy.fetch_add(1, Ordering::SeqCst);
            let (source, busy, last) = (source.clone(), busy.clone(), last.clone());
            // (A handle is a number any thread may use: passed as one.)
            let handle = pipe as usize;
            std::thread::spawn(move || {
                let pipe = handle as HANDLE;
                // The handle is the connection's: closed with it.
                let mut connection = unsafe { std::fs::File::from_raw_handle(pipe as _) };
                if let Ok(request) = read_request(&mut connection) {
                    if let Some(user) = client_of(pipe) {
                        let _ = answer(&*source, &user, request, &mut connection);
                        // Until the client has read it all (disconnecting throws away what it hasn't).
                        let _ = connection.sync_all();
                    }
                }
                unsafe { DisconnectNamedPipe(pipe) };
                *last.lock().unwrap() = Instant::now();
                busy.fetch_sub(1, Ordering::SeqCst);
            });
        }
    }

    /// The PC itself: its NTFS drives, read raw (only a service, or an administrator, may), and its accounts.
    pub struct Pc;

    impl Source for Pc {
        fn drives(&self) -> Vec<String> {
            crate::names::indexed_drives().into_iter().filter(|d| file_system(d) == "NTFS")
                .map(|d| d.trim_end_matches('\\').to_string()).collect()
        }

        fn open(&self, drive: &str) -> Result<Box<dyn mft::ReadSeek + Send>, u32> {
            Ok(Box::new(mft::RawVolume::open(drive)?))
        }

        fn hidden(&self, user: &str, drive: &str) -> Vec<u64> {
            let prefix = format!("{}\\", drive.trim_end_matches('\\')).to_lowercase();
            profiles().into_iter()
                .filter(|(sid, path)| !sid.eq_ignore_ascii_case(user) && path.to_lowercase().starts_with(&prefix))
                .filter_map(|(_, path)| record_of(&path))
                .collect()
        }
    }

    /// "NTFS", "FAT32"… for a drive's root ("C:\").
    fn file_system(root: &str) -> String {
        use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationW;
        let mut fs = [0u16; 32];
        let ok = unsafe {
            GetVolumeInformationW(wide(root).as_ptr(), std::ptr::null_mut(), 0, std::ptr::null_mut(), std::ptr::null_mut(),
                                  std::ptr::null_mut(), fs.as_mut_ptr(), fs.len() as u32)
        };
        if ok == 0 { String::new() } else { String::from_utf16_lossy(&fs[..fs.iter().position(|&c| c == 0).unwrap_or(0)]) }
    }

    /// The people's accounts on this PC and their profile folders: (SID, "C:\Users\name"). Only accounts of people
    /// (local, domain and Microsoft Entra ones), not Windows' own (SYSTEM, the services').
    pub fn profiles() -> Vec<(String, String)> {
        use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;
        use windows_sys::Win32::System::Registry::{RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RRF_NOEXPAND};
        let mut out = Vec::new();
        unsafe {
            let mut key: HKEY = std::ptr::null_mut();
            if RegOpenKeyExW(HKEY_LOCAL_MACHINE, wide(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList").as_ptr(), 0, KEY_READ, &mut key) != 0 {
                return out;
            }
            for i in 0.. {
                let mut name = [0u16; 256];
                let mut len = name.len() as u32;
                if RegEnumKeyExW(key, i, name.as_mut_ptr(), &mut len, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) != 0 {
                    break;
                }
                let sid = String::from_utf16_lossy(&name[..len as usize]);
                if !(sid.starts_with("S-1-5-21-") || sid.starts_with("S-1-12-1-")) {
                    continue;
                }
                let mut value = [0u16; 1024];
                let mut bytes = (value.len() * 2) as u32;
                if RegGetValueW(key, name.as_ptr(), wide("ProfileImagePath").as_ptr(), RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND,
                                std::ptr::null_mut(), value.as_mut_ptr() as *mut _, &mut bytes) != 0 {
                    continue;
                }
                let mut expanded = [0u16; 1024];
                let n = ExpandEnvironmentStringsW(value.as_ptr(), expanded.as_mut_ptr(), expanded.len() as u32) as usize;
                if n == 0 || n > expanded.len() {
                    continue;
                }
                let path = String::from_utf16_lossy(&expanded[..n.saturating_sub(1)]);
                if !path.is_empty() {
                    out.push((sid, path));
                }
            }
            RegCloseKey(key);
        }
        out
    }

    /// A folder's MFT record number (its file id's low 48 bits).
    fn record_of(path: &str) -> Option<u64> {
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        };
        unsafe {
            let handle = CreateFileW(wide(path).as_ptr(), 0, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, std::ptr::null(),
                                     OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, std::ptr::null_mut());
            if handle == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
            let ok = GetFileInformationByHandle(handle, &mut info) != 0;
            CloseHandle(handle);
            ok.then_some(((info.nFileIndexHigh as u64) << 32 | info.nFileIndexLow as u64) & 0x0000_FFFF_FFFF_FFFF)
        }
    }

    /// Whether Excavate Bedrock is installed (whether it runs or not).
    pub fn installed() -> bool {
        unsafe {
            let manager = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
            if manager.is_null() {
                return false;
            }
            let service = OpenServiceW(manager, wide(SERVICE).as_ptr(), SERVICE_QUERY_STATUS);
            let there = !service.is_null();
            if there {
                CloseServiceHandle(service);
            }
            CloseServiceHandle(manager);
            there
        }
    }

    /// Starts the service when it isn't running (it's installed so any user may start it) and waits for its pipe.
    /// The pipe's name, ready to open; Err says why not.
    pub fn connect() -> Result<&'static str, String> {
        if unsafe { WaitNamedPipeW(wide(PIPE).as_ptr(), 0) } != 0 {
            return Ok(PIPE);
        }
        unsafe {
            let manager = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
            if manager.is_null() {
                return Err(format!("couldn't ask Windows for its services (error {})", GetLastError()));
            }
            let service = OpenServiceW(manager, wide(SERVICE).as_ptr(), SERVICE_START | SERVICE_QUERY_STATUS);
            if service.is_null() {
                let code = GetLastError();
                CloseServiceHandle(manager);
                return Err(if code == 1060 { "it isn't installed".into() } else { format!("couldn't open it (error {code})") });
            }
            let mut status: SERVICE_STATUS = std::mem::zeroed();
            let running = QueryServiceStatus(service, &mut status) != 0 && status.dwCurrentState == SERVICE_RUNNING;
            // (Already starting or running: error 1056, fine.)
            if !running && StartServiceW(service, 0, std::ptr::null()) == 0 && GetLastError() != 1056 {
                let code = GetLastError();
                CloseServiceHandle(service);
                CloseServiceHandle(manager);
                return Err(format!("couldn't start it (error {code})"));
            }
            CloseServiceHandle(service);
            CloseServiceHandle(manager);
        }
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(15) {
            if unsafe { WaitNamedPipeW(wide(PIPE).as_ptr(), 500) } != 0 {
                return Ok(PIPE);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err("it started but didn't answer".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mft::image::{volume, File};

    fn record(dir: bool, names: &[(u64, &str)]) -> Record {
        Record {
            is_dir: dir,
            base: 0,
            names: names.iter().map(|&(p, n)| (p, n.encode_utf16().collect())).collect(),
            size: (!dir).then_some(12),
            written: Some(133_000_000_000_000_000),
            created: None,
            accessed: Some(5),
            attributes: 0x20,
        }
    }

    #[test]
    fn frames_go_and_come_back() {
        let frames = vec![
            Frame::Hello { version: VERSION, drives: vec!["C:".into(), "D:".into()] },
            Frame::Drive { drive: "C:".into(), records: 1234 },
            Frame::Records(vec![(20, record(false, &[(5, "a.txt"), (30, "ü.txt")])), (30, record(true, &[(5, "Users")]))]),
            Frame::DriveDone { drive: "C:".into(), error: "nope".into() },
            Frame::Done,
        ];
        let mut bytes = Vec::new();
        for f in &frames {
            bytes.extend(encode_frame(f));
        }
        let mut from = std::io::Cursor::new(bytes);
        for f in &frames {
            assert_eq!(read_frame(&mut from).unwrap().as_ref(), Some(f));
        }
        assert!(read_frame(&mut from).unwrap().is_none());
        for request in [Request::Hello, Request::List(vec!["C:".into(), "E:".into()])] {
            assert_eq!(decode_request(&encode_request(&request)), Some(request));
        }
    }

    #[test]
    fn any_bytes_are_safe() {
        // Every cut and every flipped byte of real frames, and random bytes: an error or a frame, never a panic.
        let good = encode_frame(&Frame::Records(vec![(20, record(false, &[(5, "a.txt")])), (21, record(true, &[(5, "b")]))]));
        for cut in 0..good.len() {
            let _ = decode_frame(good[0], &good[5..cut.max(5)]);
            let _ = read_frame(&mut std::io::Cursor::new(&good[..cut]));
        }
        for at in 0..good.len() {
            for flip in [0x01u8, 0x80, 0xFF] {
                let mut bad = good.clone();
                bad[at] ^= flip;
                let _ = read_frame(&mut std::io::Cursor::new(&bad));
            }
        }
        let mut seed = 0x2545F4914F6CDD1Du64;
        for _ in 0..20_000 {
            let len = (seed % 64) as usize;
            let bytes: Vec<u8> = (0..len).map(|_| { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; seed as u8 }).collect();
            let _ = decode_request(&bytes);
            for kind in 0..7 {
                let _ = decode_frame(kind, &bytes);
            }
        }
        // A count promising more records than the bytes could hold.
        let mut lie = vec![0u8; 8];
        lie[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_frame(3, &lie).is_none());
    }

    #[test]
    fn other_peoples_folders_are_left_out() {
        // 30 Users, 31 Users\me, 32 Users\them (hidden), 33 them\deep, 34 a file deep inside, 35 a hard link with one
        // name in "me" and one in "them". 36 comes before its folder 40 (inside "them"), and waits.
        let mut filter = Filter::new(64, &[32]);
        let mut sent = Vec::new();
        let mut offer = |filter: &mut Filter, n: u64, r: Record| {
            if let Some(kept) = filter.offer(n, r) { sent.push(kept); }
        };
        offer(&mut filter, 5, record(true, &[(5, ".")]));
        offer(&mut filter, 30, record(true, &[(5, "Users")]));
        offer(&mut filter, 31, record(true, &[(30, "me")]));
        offer(&mut filter, 32, record(true, &[(30, "them")]));
        offer(&mut filter, 33, record(true, &[(32, "deep")]));
        offer(&mut filter, 34, record(false, &[(33, "secret.txt")]));
        offer(&mut filter, 35, record(false, &[(31, "mine.txt"), (33, "theirs.txt")]));
        offer(&mut filter, 36, record(false, &[(40, "later.txt")]));
        offer(&mut filter, 37, record(false, &[(41, "mine-later.txt")]));
        offer(&mut filter, 40, record(true, &[(33, "inner")]));
        offer(&mut filter, 41, record(true, &[(31, "inner")]));
        sent.extend(filter.rest());
        let names: Vec<String> = sent.iter().flat_map(|(_, r)| r.names.iter().map(|(_, n)| String::from_utf16_lossy(n))).collect();
        assert_eq!(names, [".", "Users", "me", "them", "mine.txt", "inner", "mine-later.txt"]);
    }

    struct Image {
        bytes: Vec<u8>,
    }

    impl Source for Image {
        fn drives(&self) -> Vec<String> { vec!["X:".into()] }
        fn open(&self, drive: &str) -> Result<Box<dyn mft::ReadSeek + Send>, u32> {
            if drive == "X:" { Ok(Box::new(std::io::Cursor::new(self.bytes.clone()))) } else { Err(2) }
        }
        fn hidden(&self, user: &str, _drive: &str) -> Vec<u64> {
            if user == "me" { vec![33] } else { vec![] }
        }
    }

    fn image() -> Image {
        let file = |number, dir, names: Vec<(u64, &'static str, u8)>| File {
            number, dir, names, size: (!dir).then_some(10), resident: true, written: 133_000_000_000_000_000, attributes: 0, base: 0 };
        let files = vec![
            file(5, true, vec![(5, ".", 3)]),
            file(30, true, vec![(5, "Users", 3)]),
            file(31, true, vec![(30, "me", 3)]),
            file(33, true, vec![(30, "them", 3)]),
            file(34, false, vec![(31, "mine.txt", 3)]),
            file(35, false, vec![(33, "theirs.txt", 3)]),
        ];
        Image { bytes: volume(&files, 64) }
    }

    /// Builds an index from what `answer` sends, through memory (the pipe's own test is in the integration tests).
    fn listed(source: &Image, user: &str) -> Vec<String> {
        struct Both { input: std::io::Cursor<Vec<u8>>, output: Vec<u8> }
        impl Read for Both { fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> { self.input.read(b) } }
        impl Write for Both {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> { self.output.write(b) }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        let mut request = Vec::new();
        write_request(&mut request, &Request::List(vec!["x:".into(), "Q:".into()])).unwrap();
        let mut both = Both { input: std::io::Cursor::new(request), output: Vec::new() };
        let request = read_request(&mut both).unwrap();
        answer(source, user, request, &mut both).unwrap();
        let mut from = std::io::Cursor::new(both.output);
        let mut index = crate::store::Index::new();
        let mut builder = None;
        let mut errors = Vec::new();
        while let Some(f) = read_frame(&mut from).unwrap() {
            match f {
                Frame::Drive { drive, records } => {
                    let b = mft::Builder::new(&mut index, &drive);
                    b.expect(&mut index, records as usize);
                    builder = Some(b);
                }
                Frame::Records(records) => for (n, r) in records { builder.as_mut().unwrap().add(&mut index, n, r) },
                Frame::DriveDone { drive, error } => {
                    if let Some(b) = builder.take() { b.finish(&mut index); }
                    if !error.is_empty() { errors.push(drive); }
                }
                Frame::Done => break,
                Frame::Hello { .. } => panic!(),
            }
        }
        assert_eq!(errors, ["Q:"]);
        index.build_table();
        let mut paths: Vec<String> = (0..index.len() as u32).filter(|&e| index.flags[e as usize] & crate::store::DEAD == 0).map(|e| index.path(e)).collect();
        paths.sort();
        paths
    }

    #[test]
    fn a_drive_listed_through_the_protocol() {
        let source = image();
        assert_eq!(listed(&source, "me"), ["X:", r"X:\Users", r"X:\Users\me", r"X:\Users\me\mine.txt", r"X:\Users\them"]);
        assert_eq!(listed(&source, "admin"), ["X:", r"X:\Users", r"X:\Users\me", r"X:\Users\me\mine.txt", r"X:\Users\them",
                                               r"X:\Users\them\theirs.txt"]);
    }
}
