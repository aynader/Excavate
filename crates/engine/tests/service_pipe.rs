//! Excavate Bedrock end to end over a real named pipe: the server on a pipe of the test's own, reading a volume image,
//! and the client building an index from what comes through. (The service itself reads real volumes as SYSTEM; that's
//! tried on the VM.)

use engine::mft::image::{volume, File};
use engine::service::{self, Listed, Source};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

struct Image(Vec<u8>);

impl Source for Image {
    fn drives(&self) -> Vec<String> {
        vec!["X:".into()]
    }
    fn open(&self, _drive: &str) -> Result<Box<dyn engine::mft::ReadSeek + Send>, u32> {
        Ok(Box::new(std::io::Cursor::new(self.0.clone())))
    }
    fn hidden(&self, _user: &str, _drive: &str) -> Vec<u64> {
        vec![33]
    }
}

#[test]
fn listed_over_a_pipe() {
    let file = |number, dir, names: Vec<(u64, &'static str, u8)>| File {
        number, dir, names, size: (!dir).then_some(10), resident: true, written: 133_000_000_000_000_000, attributes: 0, base: 0 };
    let mut files = vec![
        file(5, true, vec![(5, ".", 3)]),
        file(30, true, vec![(5, "Users", 3)]),
        file(31, true, vec![(30, "me", 3)]),
        file(33, true, vec![(30, "them", 3)]),
        file(35, false, vec![(33, "theirs.txt", 3)]),
    ];
    // Enough files for several batches.
    let names: Vec<&'static str> = (0..3000).map(|i| &*Box::leak(format!("file {i:04} ü.txt").into_boxed_str())).collect();
    for (i, name) in names.iter().enumerate() {
        files.push(file(40 + i as u64, false, vec![(31, name, 1)]));
    }
    let image = Image(volume(&files, 3100));
    let pipe = format!(r"\\.\pipe\excavate-bedrock-test-{}", std::process::id());
    let stop = Arc::new(AtomicBool::new(false));
    let server = {
        let (pipe, stop) = (pipe.clone(), stop.clone());
        std::thread::spawn(move || service::serve(Arc::new(image), &pipe, Duration::from_secs(2), stop))
    };
    let mut ready = false;
    for _ in 0..50 {
        if let Ok((version, drives)) = service::hello_through(&pipe) {
            assert_eq!((version, drives), (service::VERSION, vec!["X:".to_string()]));
            ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if !ready {
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        panic!("no answer; server: {:?}", server.join());
    }

    let mut index = engine::store::Index::new();
    let mut builder = None;
    let mut batches = 0;
    service::list_through(&pipe, &["X:".to_string()], |part| match part {
        Listed::Drive { drive, records } => {
            let b = engine::mft::Builder::new(&mut index, &drive);
            b.expect(&mut index, records as usize);
            builder = Some(b);
        }
        Listed::Records(records) => {
            batches += 1;
            for (n, r) in records {
                builder.as_mut().unwrap().add(&mut index, n, r);
            }
        }
        Listed::DriveDone { error, .. } => {
            assert_eq!(error, "");
            builder.take().unwrap().finish(&mut index);
        }
    }).unwrap();
    assert!(batches >= 1);
    index.build_table();
    let live: Vec<String> = (0..index.len() as u32).filter(|&e| index.flags[e as usize] & engine::store::DEAD == 0).map(|e| index.path(e)).collect();
    assert_eq!(live.len(), 5 + 3000 - 1, "the drive, Users, me, them, and me's files; not theirs.txt");
    assert!(live.iter().any(|p| p == r"X:\Users\me\file 2999 ü.txt"));
    assert!(!live.iter().any(|p| p.contains("theirs")));

    // Idle for its time, it ends by itself.
    let ended = server.join().unwrap();
    assert!(ended.is_ok(), "{ended:?}");
}
