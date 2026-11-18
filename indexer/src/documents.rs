//! Documents read in a process of their own, one per documents' worker, at every speed: the same program,
//! `--read-documents`, a path on each line in, its text and its secrets (or why not) as a line of JSON out.
//!
//! Why a process: (1) PDFium serializes every call within a process (pdfium-render's `thread_safe`), so several
//! workers in one process went only about 2.3 times as fast on PDFs; (2) a reader that hangs, overflows its stack or
//! aborts takes only its own process down, which `catch_unwind` can't promise; (3) a file can be let go part way: Skip,
//! or a time limit, ends the process, and the next file gets a new one. Finding the secrets happens there too: on one
//! long-lined file it took 14–22 s, which Skip can now end. The process ends with its worker (and, by a job object,
//! with the indexer).

use extract::Extracted;
use privacy::{Kind, Secret};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// The argument that makes this program a documents' reader (and the one it had when it read only PDFs).
pub const ARGUMENT: &str = "--read-documents";
pub const OLD_ARGUMENT: &str = "--read-pdfs";
/// With it, the reader works behind other programs (the speeds below Full).
const BACKGROUND: &str = "--background";

/// How long a file may take before its reader is ended: 5 minutes, and a minute more for each 20 MB.
pub fn time_limit(size: u64) -> Duration {
    Duration::from_secs(300 + 60 * (size / (20 << 20)))
}

/// A document's text and the secrets in it (byte ranges in that text, as `privacy::find` gives them).
pub struct Read {
    pub extracted: Extracted,
    pub secrets: Vec<Secret>,
}

/// Why a document wasn't read.
#[derive(Debug, PartialEq)]
pub enum Failed {
    /// The reader's own answer (damaged, password-protected, …): the file's.
    File(String),
    /// The reader process ended on it (or couldn't be asked): `config::why::READER_STOPPED`.
    Stopped,
    /// Past `time_limit`: the reader was ended.
    TooLong(Duration),
    /// `stop` said so (Skip, or reading stopping): the reader was ended.
    Asked,
}

/// The reader's side: reads each file named on its input until the input ends.
pub fn serve() {
    if std::env::args().any(|arg| arg == BACKGROUND) {
        winsys::process::run_in_background();
        winsys::process::background_thread_as(true);
    }
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let Ok(path) = line else { break };
        let path = path.trim().trim_start_matches('\u{feff}');
        let read = std::panic::catch_unwind(|| read_here(path)).unwrap_or_else(|_| Err(config::why::READER_STOPPED.into()));
        if writeln!(out, "{}", answer(read)).is_err() || out.flush().is_err() {
            break;
        }
    }
}

/// A document read here, in this process: its text by the reader its name calls for, then its secrets.
pub fn read_here(path: &str) -> Result<Read, String> {
    let name = Path::new(path).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let reader = extract::reader_for(&name).ok_or_else(|| format!("no reader for {name}"))?;
    let extracted = extract::extract(Path::new(path), reader)?;
    let secrets = privacy::find(&extracted.text);
    Ok(Read { extracted, secrets })
}

fn answer(read: Result<Read, String>) -> serde_json::Value {
    match read {
        Ok(Read { extracted, secrets }) => serde_json::json!({
            "text": extracted.text,
            "scanned": extracted.scanned_pages,
            "pages": extracted.pages,
            "secrets": secrets.iter().map(|secret| serde_json::json!([secret.kind.name(), secret.range.start, secret.range.end])).collect::<Vec<_>>(),
        }),
        Err(error) => serde_json::json!({ "error": error }),
    }
}

fn parse(line: &str) -> Result<Read, String> {
    let value: serde_json::Value = serde_json::from_str(line).map_err(|error| format!("the reader's answer: {error}"))?;
    if let Some(error) = value["error"].as_str() {
        return Err(error.to_string());
    }
    let text = value["text"].as_str().unwrap_or_default().to_string();
    let mut secrets = Vec::new();
    for secret in value["secrets"].as_array().into_iter().flatten() {
        let (Some(kind), Some(start), Some(end)) = (secret[0].as_str().and_then(Kind::named), secret[1].as_u64(), secret[2].as_u64()) else {
            return Err(format!("the reader's answer: a secret as {secret}"));
        };
        let range = start as usize..end as usize;
        if range.start > range.end || !text.is_char_boundary(range.start) || !text.is_char_boundary(range.end) {
            return Err(format!("the reader's answer: a secret at {range:?}"));
        }
        secrets.push(Secret { kind, range });
    }
    Ok(Read {
        extracted: Extracted {
            text,
            scanned_pages: value["scanned"].as_array().map(|pages| pages.iter().filter_map(|page| page.as_u64().map(|page| page as u16)).collect()).unwrap_or_default(),
            pages: value["pages"].as_u64().unwrap_or(0) as u16,
        },
        secrets,
    })
}

/// A worker's documents' reader, started when it's first needed.
pub struct DocumentReader {
    child: Child,
    input: ChildStdin,
    /// Its answers, a line each, from a thread that reads them (so waiting for one can end).
    answers: Receiver<String>,
    pub background: bool,
    _job: Option<winsys::process::Job>,
}

impl DocumentReader {
    pub fn start(background: bool) -> Result<DocumentReader, String> {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let exe = std::env::current_exe().map_err(|error| error.to_string())?;
        let mut command = Command::new(exe);
        command.arg(ARGUMENT);
        if background {
            command.arg(BACKGROUND);
        }
        let mut child = command.creation_flags(CREATE_NO_WINDOW).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
            .spawn().map_err(|error| format!("can't start a documents' reader: {error}"))?;
        let job = winsys::process::Job::new();
        if let Some(job) = &job {
            job.add(child.id());
        }
        let input = child.stdin.take().ok_or("no input")?;
        let output = BufReader::new(child.stdout.take().ok_or("no output")?);
        let (send, answers) = std::sync::mpsc::channel();
        std::thread::Builder::new().name("reader's answers".into()).spawn(move || {
            for line in output.lines() {
                let Ok(line) = line else { break };
                if send.send(line).is_err() {
                    break;
                }
            }
        }).map_err(|error| error.to_string())?;
        Ok(DocumentReader { child, input, answers, background, _job: job })
    }

    /// A document's text and secrets, waiting at most `limit`, and asking `stop` every quarter second. After any
    /// failure but `File` the process is gone: start a new one for the next file.
    pub fn read(&mut self, path: &str, limit: Duration, stop: &dyn Fn() -> bool) -> Result<Read, Failed> {
        if writeln!(self.input, "{path}").and_then(|()| self.input.flush()).is_err() {
            self.end();
            return Err(Failed::Stopped);
        }
        let started = Instant::now();
        loop {
            match self.answers.recv_timeout(Duration::from_millis(250)) {
                Ok(line) => return parse(&line).map_err(Failed::File),
                Err(RecvTimeoutError::Disconnected) => {
                    self.end();
                    return Err(Failed::Stopped);
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            if stop() {
                self.end();
                return Err(Failed::Asked);
            }
            if started.elapsed() >= limit {
                self.end();
                return Err(Failed::TooLong(limit));
            }
        }
    }

    fn end(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for DocumentReader {
    fn drop(&mut self) {
        self.end();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_go_both_ways() {
        let text = "pass: hunter2 ü \"quoted\"\nline";
        let read = Read {
            extracted: Extracted { text: text.into(), scanned_pages: vec![2, 5], pages: 7 },
            secrets: vec![Secret { kind: Kind::Password, range: 6..13 }],
        };
        let back = parse(&answer(Ok(read)).to_string()).unwrap();
        assert_eq!((back.extracted.text.as_str(), back.extracted.scanned_pages, back.extracted.pages), (text, vec![2, 5], 7));
        assert_eq!(back.secrets, [Secret { kind: Kind::Password, range: 6..13 }]);
        assert_eq!(parse(&answer(Err("PasswordError".into())).to_string()).err().unwrap(), "PasswordError");
        assert!(parse("not json").is_err());
        // A secret's place must be in the text, on a character's edge.
        assert!(parse(r#"{"text":"ü","secrets":[["password",0,1]]}"#).is_err());
        assert!(parse(r#"{"text":"ab","secrets":[["nothing",0,1]]}"#).is_err());
        for kind in Kind::ALL {
            assert_eq!(Kind::named(kind.name()), Some(kind));
        }
    }

    #[test]
    fn time_limits_grow_with_size() {
        assert_eq!(time_limit(1 << 20), Duration::from_secs(300));
        assert_eq!(time_limit(100 << 20), Duration::from_secs(600));
    }

    /// The reader as the indexer runs it: a real process (this test program can't be one, so a stand-in that answers
    /// one line and then hangs), ended by `stop` and by the time limit.
    #[test]
    fn a_reader_that_hangs_is_ended() {
        use std::os::windows::process::CommandExt;
        let mut child = Command::new("powershell").args(["-NoProfile", "-Command",
            r#"$null = [Console]::In.ReadLine(); [Console]::Out.WriteLine('{"text":"ok","secrets":[]}'); [Console]::Out.Flush(); Start-Sleep 600"#])
            .creation_flags(0x0800_0000).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let (send, answers) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in output.lines().map_while(Result::ok) {
                let _ = send.send(line);
            }
        });
        let mut reader = DocumentReader { child, input, answers, background: false, _job: None };
        assert_eq!(reader.read("first", Duration::from_secs(60), &|| false).unwrap().extracted.text, "ok");
        let asked = std::cell::Cell::new(0);
        let started = Instant::now();
        assert_eq!(reader.read("second", Duration::from_secs(60), &|| { asked.set(asked.get() + 1); asked.get() > 2 }).err(), Some(Failed::Asked));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(reader.child.try_wait().unwrap().is_some(), "ended");
    }
}
