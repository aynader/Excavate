//! Searching the content index from the popup, on a thread of its own so typing never waits: requests superseded by a
//! newer keystroke are dropped before they run. The index is opened when it first exists (the indexer creates it) and
//! refreshed at most once a second, so what was just read shows up.

use content::{Hit, Query, Reader, Stored};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

enum Request {
    Search { generation: u64, query: Query, limit: usize, reply: Box<dyn FnOnce(u64, Vec<Hit>, Duration) + Send> },
    Stored { path: String, reply: Box<dyn FnOnce(Option<Stored>) + Send> },
}

pub struct Contents {
    sender: Sender<Request>,
}

impl Contents {
    pub fn start(index: PathBuf) -> Contents {
        let (sender, receiver) = channel();
        std::thread::Builder::new()
            .name("contents".into())
            .spawn(move || serve(index, receiver))
            .expect("can't start the content search thread");
        Contents { sender }
    }

    pub fn search(&self, generation: u64, query: Query, limit: usize, reply: impl FnOnce(u64, Vec<Hit>, Duration) + Send + 'static) {
        let _ = self.sender.send(Request::Search { generation, query, limit, reply: Box::new(reply) });
    }

    pub fn stored(&self, path: String, reply: impl FnOnce(Option<Stored>) + Send + 'static) {
        let _ = self.sender.send(Request::Stored { path, reply: Box::new(reply) });
    }
}

fn serve(index: PathBuf, requests: Receiver<Request>) {
    let mut reader: Option<Reader> = None;
    let mut tried = Instant::now() - Duration::from_secs(60);
    while let Ok(first) = requests.recv() {
        // Only the newest search matters; previews are all answered.
        let mut search = None;
        let mut stored = Vec::new();
        for request in std::iter::once(first).chain(requests.try_iter()) {
            match request {
                Request::Search { .. } => search = Some(request),
                Request::Stored { .. } => stored.push(request),
            }
        }
        if reader.is_none() && tried.elapsed() >= Duration::from_secs(2) {
            tried = Instant::now();
            reader = Reader::open(&index).ok();
        }
        if let Some(reader) = reader.as_mut() {
            reader.refresh();
        }
        if let Some(Request::Search { generation, query, limit, reply }) = search {
            let started = Instant::now();
            let hits = reader.as_ref().map(|reader| reader.search(&query, limit)).unwrap_or_default();
            reply(generation, hits, started.elapsed());
        }
        for request in stored {
            if let Request::Stored { path, reply } = request {
                reply(reader.as_ref().and_then(|reader| reader.stored(&path)));
            }
        }
    }
}
