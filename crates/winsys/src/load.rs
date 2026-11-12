//! How busy the PC is with everything but Excavate: time since the last input, the CPU, the best graphics card's busiest
//! engine and its free memory. All cheap, for the indexer to ask every few seconds while heavy models have work.
//!
//! GPU use comes from the `GPU Engine` and `GPU Adapter Memory` performance counters, the ones Task Manager shows: per
//! process, so Excavate's own model can be left out of what it's judged by.

use std::collections::HashMap;
use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, FILETIME};
use windows::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA, PdhAddEnglishCounterW, PdhCloseQuery,
    PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Threading::{GetProcessTimes, GetSystemTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::core::w;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Load {
    /// Since the person last touched the keyboard or the mouse.
    pub idle: Duration,
    /// Everything but Excavate: its share of all the CPU's cores, 0 to 1.
    pub cpu: f32,
    /// Everything but Excavate: its use of the best graphics card's busiest engine, 0 to 1.
    pub gpu: f32,
    /// That card's memory nobody is using, in megabytes (None without such a card); with `gpu_own_counted`, what
    /// Excavate's own processes hold on it is counted as free too: it's room Excavate has.
    pub gpu_free_mb: Option<u64>,
    /// Whether `gpu_free_mb` includes Excavate's own (Windows' per-process counter was there to say).
    pub gpu_own_counted: bool,
    /// The process using that engine most, other than Excavate's.
    pub gpu_top: Option<u32>,
}

/// The program using the CPU most, other than Excavate: its processes counted together (four python.exe are one
/// Python), with one of them to name it by, and their share of all the cores.
#[derive(Clone, Debug, PartialEq)]
pub struct Top {
    pub file: String,
    pub process: u32,
    pub share: f32,
}

/// Rates need two looks: each `sample` covers the time since the one before.
pub struct Sampler {
    cpu: Option<(u64, u64, u64)>,
    gpu: Option<GpuCounters>,
    /// For `cpu_top`: each process's CPU time at the last look, and when that was.
    processes: HashMap<u32, u64>,
    looked: Option<std::time::Instant>,
}

impl Default for Sampler {
    fn default() -> Sampler {
        Sampler::new()
    }
}

impl Sampler {
    pub fn new() -> Sampler {
        Sampler { cpu: None, gpu: GpuCounters::open(), processes: HashMap::new(), looked: None }
    }

    /// The program using the CPU most since the last call (None the first time, or when nothing is). Opens every
    /// process to ask for its CPU time: for when the CPU is busy, to say with what.
    pub fn cpu_top(&mut self, own: &[u32]) -> Option<Top> {
        let now = std::time::Instant::now();
        let cores = std::thread::available_parallelism().map_or(1, |cores| cores.get()) as f64;
        let mut times = HashMap::new();
        let mut by_file: HashMap<String, (u64, u32, u64)> = HashMap::new();
        for (process, file) in crate::process::all() {
            // (The idle process's time isn't anyone's use.)
            if process == 0 || own.contains(&process) {
                continue;
            }
            let used = process_ticks(process);
            if used == 0 {
                continue;
            }
            times.insert(process, used);
            let Some(&before) = self.processes.get(&process) else { continue };
            let delta = used.saturating_sub(before);
            let entry = by_file.entry(file.to_lowercase()).or_insert((0, process, 0));
            entry.0 += delta;
            // Named by its busiest process.
            if delta > entry.2 {
                (entry.1, entry.2) = (process, delta);
            }
        }
        let since = self.looked.replace(now).map(|looked| now.duration_since(looked));
        self.processes = times;
        let since = since?;
        let total = since.as_secs_f64() * 10_000_000.0 * cores;
        let (file, (used, process, _)) = by_file.into_iter().max_by_key(|(_, (used, ..))| *used)?;
        (used > 0).then(|| Top { file, process, share: (used as f64 / total).clamp(0.0, 1.0) as f32 })
    }

    /// How busy the PC has been since the last call. `own`: the processes to leave out.
    pub fn sample(&mut self, own: &[u32]) -> Load {
        let idle = idle();
        let cpu = self.cpu_share(own);
        let (gpu, gpu_free_mb, gpu_top, gpu_own_counted) = self.gpu.as_mut().map_or((0.0, None, None, false), |counters| counters.sample(own));
        Load { idle, cpu, gpu, gpu_free_mb, gpu_top, gpu_own_counted }
    }

    fn cpu_share(&mut self, own: &[u32]) -> f32 {
        let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
        // SAFETY: filled by the call.
        if unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.is_err() {
            return 0.0;
        }
        // (Kernel time includes idle time.)
        let total = ticks(kernel) + ticks(user);
        let busy = total.saturating_sub(ticks(idle));
        let ours: u64 = own.iter().map(|&process| process_ticks(process)).sum();
        let now = (total, busy, ours);
        let share = self.cpu.map_or(0.0, |(total_before, busy_before, ours_before)| {
            let total = now.0.saturating_sub(total_before).max(1);
            let busy = now.1.saturating_sub(busy_before);
            let ours = now.2.saturating_sub(ours_before);
            busy.saturating_sub(ours) as f32 / total as f32
        });
        self.cpu = Some(now);
        share.clamp(0.0, 1.0)
    }
}

fn ticks(time: FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}

/// What a process has used of the CPU so far (100 ns units).
fn process_ticks(process: u32) -> u64 {
    // SAFETY: opening a process to query it, and closing the handle; the times are filled by the call.
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process) else { return 0 };
        let (mut created, mut exited, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let used = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user).map_or(0, |_| ticks(kernel) + ticks(user));
        let _ = CloseHandle(handle);
        used
    }
}

/// What this process has used of the CPU so far, all threads.
pub fn own_cpu() -> Duration {
    process_cpu(std::process::id())
}

/// What a process has used of the CPU so far, all threads (zero when it can't be asked).
pub fn process_cpu(process: u32) -> Duration {
    Duration::from_nanos(process_ticks(process) * 100)
}

/// Since the last keyboard or mouse input in this session.
pub fn idle() -> Duration {
    let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    // SAFETY: filled by the call; both are ticks of the same clock, wrapping together.
    unsafe {
        if !GetLastInputInfo(&mut info).as_bool() {
            return Duration::ZERO;
        }
        Duration::from_millis(u64::from(GetTickCount().wrapping_sub(info.dwTime)))
    }
}

struct GpuCounters {
    query: PDH_HQUERY,
    engines: PDH_HCOUNTER,
    memory: PDH_HCOUNTER,
    /// Each process's own memory on each card, when Windows has the counter.
    processes: Option<PDH_HCOUNTER>,
    /// "luid_0x00000000_0x0000C3D2": the best card, as the counters name it.
    card: String,
    total_mb: u64,
}

// SAFETY: a PDH query can be used from any thread, one at a time (`&mut self`).
unsafe impl Send for GpuCounters {}

impl GpuCounters {
    fn open() -> Option<GpuCounters> {
        let (card, total_mb) = best_card()?;
        // SAFETY: a query and its counters, owned by the returned value and closed when it's dropped.
        unsafe {
            let mut query = PDH_HQUERY::default();
            if PdhOpenQueryW(None, 0, &mut query) != 0 {
                return None;
            }
            let (mut engines, mut memory) = (PDH_HCOUNTER::default(), PDH_HCOUNTER::default());
            let added = PdhAddEnglishCounterW(query, w!("\\GPU Engine(*)\\Utilization Percentage"), 0, &mut engines) == 0
                && PdhAddEnglishCounterW(query, w!("\\GPU Adapter Memory(*)\\Dedicated Usage"), 0, &mut memory) == 0;
            if !added {
                let _ = PdhCloseQuery(query);
                return None;
            }
            let mut processes = PDH_HCOUNTER::default();
            let processes = (PdhAddEnglishCounterW(query, w!("\\GPU Process Memory(*)\\Dedicated Usage"), 0, &mut processes) == 0).then_some(processes);
            let _ = PdhCollectQueryData(query);
            Some(GpuCounters { query, engines, memory, processes, card, total_mb })
        }
    }

    fn sample(&mut self, own: &[u32]) -> (f32, Option<u64>, Option<u32>, bool) {
        // SAFETY: our own query.
        if unsafe { PdhCollectQueryData(self.query) } != 0 {
            return (0.0, None, None, false);
        }
        // Each engine's use, and each process's on it.
        let mut engines: HashMap<String, (f64, HashMap<u32, f64>)> = HashMap::new();
        for (name, value) in values(self.engines) {
            if !name.contains(&self.card) {
                continue;
            }
            let (process, engine) = engine_of(&name);
            if own.contains(&process) {
                continue;
            }
            let entry = engines.entry(engine.to_string()).or_default();
            entry.0 += value;
            *entry.1.entry(process).or_default() += value;
        }
        let busiest = engines.values().max_by(|a, b| a.0.total_cmp(&b.0));
        let load = busiest.map_or(0.0, |(total, _)| *total) / 100.0;
        let top = busiest.and_then(|(_, processes)| processes.iter().filter(|(_, used)| **used > 0.0).max_by(|a, b| a.1.total_cmp(b.1)).map(|(process, _)| *process));
        let used: f64 = values(self.memory).into_iter().filter(|(name, _)| name.contains(&self.card)).map(|(_, bytes)| bytes).sum();
        // What Excavate's own processes hold on this card ("pid_1234_luid_…_phys_0"): room it has.
        let ours: Option<f64> = self.processes.map(|processes| values(processes).into_iter()
            .filter(|(name, _)| name.contains(&self.card) && own.contains(&engine_of(name).0)).map(|(_, bytes)| bytes).sum());
        let free = self.total_mb.saturating_sub(((used - ours.unwrap_or(0.0)).max(0.0) / 1_048_576.0) as u64);
        (load.clamp(0.0, 1.0) as f32, Some(free), top, ours.is_some())
    }
}

impl Drop for GpuCounters {
    fn drop(&mut self) {
        // SAFETY: our own query.
        unsafe {
            let _ = PdhCloseQuery(self.query);
        }
    }
}

/// From "pid_1234_luid_0x00000000_0x0000C3D2_phys_0_eng_3_engtype_3D": the process, and which engine.
fn engine_of(name: &str) -> (u32, &str) {
    let process = name.strip_prefix("pid_").and_then(|rest| rest.split('_').next()).and_then(|pid| pid.parse().ok()).unwrap_or(0);
    let engine = name.find("eng_").map_or("", |at| name[at..].split("_engtype").next().unwrap_or(""));
    (process, engine)
}

/// A counter's instances and their values.
fn values(counter: PDH_HCOUNTER) -> Vec<(String, f64)> {
    let (mut size, mut count) = (0u32, 0u32);
    // SAFETY: asked for the size first; the buffer is u64-aligned and that size, and the names it points into live in it.
    unsafe {
        if PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, None) != PDH_MORE_DATA {
            return Vec::new();
        }
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        let items = buffer.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
        if PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, Some(items)) != 0 {
            return Vec::new();
        }
        std::slice::from_raw_parts(items, count as usize)
            .iter()
            .filter(|item| item.FmtValue.CStatus == 0)
            .filter_map(|item| Some((item.szName.to_string().ok()?, item.FmtValue.Anonymous.doubleValue)))
            .collect()
    }
}

/// The card Excavate runs on (`gpu::capable`, the one chosen in Settings or the best): how the counters name it, and its
/// memory.
fn best_card() -> Option<(String, u64)> {
    crate::gpu::capable().map(|gpu| (gpu.luid, gpu.dedicated_mb))
}

/// Whether something's quiet enough, with some patience so it doesn't flap: busy after two looks above `busy_above`, and
/// quiet again only after three below `quiet_below`.
#[derive(Clone, Copy, Debug)]
pub struct Quiet {
    quiet: bool,
    above: u32,
    below: u32,
}

impl Default for Quiet {
    fn default() -> Quiet {
        Quiet { quiet: true, above: 0, below: 0 }
    }
}

impl Quiet {
    pub fn update(&mut self, value: f32, busy_above: f32, quiet_below: f32) -> bool {
        if value > busy_above {
            self.above += 1;
            self.below = 0;
        } else if value < quiet_below {
            self.below += 1;
            self.above = 0;
        } else {
            self.above = 0;
            self.below = 0;
        }
        if self.quiet && self.above >= 2 {
            self.quiet = false;
        } else if !self.quiet && self.below >= 3 {
            self.quiet = true;
        }
        self.quiet
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_engine_names() {
        assert_eq!(engine_of("pid_1234_luid_0x00000000_0x0000C3D2_phys_0_eng_3_engtype_3D"), (1234, "eng_3"));
        assert_eq!(engine_of("nonsense"), (0, ""));
    }

    #[test]
    fn patience() {
        let mut quiet = Quiet::default();
        assert!(quiet.update(0.9, 0.35, 0.25), "one spike isn't busy");
        assert!(quiet.update(0.1, 0.35, 0.25));
        assert!(quiet.update(0.9, 0.35, 0.25));
        assert!(!quiet.update(0.9, 0.35, 0.25), "two in a row are");
        assert!(!quiet.update(0.3, 0.35, 0.25), "between the two: no change");
        assert!(!quiet.update(0.1, 0.35, 0.25));
        assert!(!quiet.update(0.1, 0.35, 0.25));
        assert!(quiet.update(0.1, 0.35, 0.25), "three quiet looks");
    }

    #[test]
    fn samples_this_pc() {
        let mut sampler = Sampler::new();
        let first = sampler.sample(&[std::process::id()]);
        assert_eq!(first.cpu, 0.0);
        std::thread::sleep(Duration::from_millis(300));
        let second = sampler.sample(&[std::process::id()]);
        assert!((0.0..=1.0).contains(&second.cpu) && (0.0..=1.0).contains(&second.gpu));
    }
}
