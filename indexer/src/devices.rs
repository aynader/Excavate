//! Where each model runs fastest on this PC, measured the first time it runs (`config::devices`). Only when the
//! person hasn't chosen on its card, there's a graphics card able to, and the PC is quiet (a busy device would make
//! its time say nothing about the model).

use crate::work::Context;
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::time::Instant;

/// How many items are timed on each device, after one to warm it up.
pub const ITEMS: usize = 10;

/// What this PC's graphics card is, for measurements: its name and driver ("" without one able to).
pub fn adapter() -> String {
    winsys::gpu::capable().map(|gpu| gpu.identity()).unwrap_or_default()
}

/// Whether to measure `key` now: not measured with this card, nor chosen on its card (`chosen`), and a card able to.
pub fn wanted(measured: &config::devices::Measured, key: &str, chosen: bool, card: bool) -> bool {
    card && !chosen && measured.fastest(key).is_none()
}

/// Whether the PC is quiet enough to measure: nothing else busy on either device.
pub fn quiet(context: &Context) -> bool {
    let shared = &context.shared;
    ![&shared.cpu_busy, &shared.gpu_busy, &shared.cpu_slow, &shared.gpu_slow].iter().any(|flag| flag.load(Ordering::Relaxed))
}

/// Times the same items on each device, in the order given (the one expected faster first). `open` makes a model on a
/// device, `run` does one item with it. The first item warms it up and isn't counted. A device that has taken half
/// as long again as the fastest so far is given up: it's slower, and by how much doesn't matter. Each device's time
/// per item (ms); a device that couldn't open or run is left out.
pub fn race<M, T>(devices: &[&str], items: &[T], mut open: impl FnMut(&str) -> Result<M, String>, mut run: impl FnMut(&mut M, &T) -> Result<(), String>,
                  stopping: &dyn Fn() -> bool) -> BTreeMap<String, f64> {
    let mut times = BTreeMap::new();
    let Some((warm, timed)) = items.split_first() else { return times };
    if timed.is_empty() {
        return times;
    }
    let mut best: Option<f64> = None;
    for &device in devices {
        if stopping() {
            break;
        }
        let mut model = match open(device) {
            Ok(model) => model,
            Err(error) => {
                config::log(&format!("measuring: {device} didn't open: {error}"));
                continue;
            }
        };
        if let Err(error) = run(&mut model, warm) {
            config::log(&format!("measuring: {device} failed: {error}"));
            continue;
        }
        let started = Instant::now();
        let mut done = 0;
        let mut failed = false;
        for item in timed {
            if stopping() {
                return times;
            }
            if let Err(error) = run(&mut model, item) {
                config::log(&format!("measuring: {device} failed: {error}"));
                failed = true;
                break;
            }
            done += 1;
            if best.is_some_and(|best| started.elapsed().as_secs_f64() * 1000.0 > best * timed.len() as f64 * 1.5) {
                break;
            }
        }
        if failed || done == 0 {
            continue;
        }
        let each = started.elapsed().as_secs_f64() * 1000.0 / done as f64;
        best = Some(best.map_or(each, |best| best.min(each)));
        times.insert(device.to_string(), each);
    }
    times
}

/// Keeps a measurement (with the card it was made with) and says which was fastest.
pub fn keep(paths: &config::Paths, key: &str, times: BTreeMap<String, f64>, unit: &str) -> Option<String> {
    let mut measured = config::devices::Measured::load(paths);
    measured.follow_adapter(&adapter());
    let line = times.iter().map(|(device, ms)| format!("{device} {ms:.0} ms")).collect::<Vec<_>>().join(", ");
    let fastest = measured.record(key, times, crate::work::now())?;
    measured.save(paths);
    config::log(&format!("measured {key}: {line} {unit}; fastest on this PC: {fastest}"));
    Some(fastest)
}
