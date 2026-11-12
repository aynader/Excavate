//! The graphics cards, to recommend the GPU reading options only where they'll be fast, and the one card everything
//! of Excavate's runs on (`capable`): DirectML's device, llama.cpp's and the listener's Vulkan device (by name), video
//! decoding's Direct3D device, and the card the load check watches. Chosen in Settings (`choose`), else the one with
//! the most memory of its own: on a laptop, the NVIDIA or AMD card rather than the processor's own graphics.

use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIDevice, IDXGIFactory1};
use windows::core::Interface;

#[derive(Clone, Debug)]
pub struct Gpu {
    pub name: String,
    /// Its own memory (VRAM), in megabytes. Integrated GPUs have little or none.
    pub dedicated_mb: u64,
    /// Its place in DXGI's list: DirectML's device id for it.
    pub index: u32,
    /// Its driver's version, "32.0.15.6094" (empty when Windows doesn't say).
    pub driver: String,
    /// "luid_0x00000000_0x0000C3D2": how Windows' performance counters name it.
    pub luid: String,
}

impl Gpu {
    /// The card and its driver, in one line: what a measurement made with it is kept with (a new driver can change
    /// what's fastest).
    pub fn identity(&self) -> String {
        if self.driver.is_empty() { self.name.clone() } else { format!("{}, driver {}", self.name, self.driver) }
    }
}

/// The hardware adapters, biggest memory first.
pub fn adapters() -> Vec<Gpu> {
    let mut gpus = Vec::new();
    // SAFETY: a DXGI factory, enumerated and released here.
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else { return gpus };
        let mut index = 0;
        while let Ok(adapter) = factory.EnumAdapters1(index) {
            let place = index;
            index += 1;
            let Ok(description) = adapter.GetDesc1() else { continue };
            if description.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                continue;
            }
            let length = description.Description.iter().position(|&c| c == 0).unwrap_or(description.Description.len());
            // (The user-mode driver's version, as DXGI reports it for its own interface.)
            let driver = adapter.CheckInterfaceSupport(&IDXGIDevice::IID).map(|version| {
                let version = version as u64;
                format!("{}.{}.{}.{}", version >> 48, (version >> 32) & 0xFFFF, (version >> 16) & 0xFFFF, version & 0xFFFF)
            }).unwrap_or_default();
            let luid = description.AdapterLuid;
            gpus.push(Gpu {
                name: String::from_utf16_lossy(&description.Description[..length]),
                dedicated_mb: description.DedicatedVideoMemory as u64 / (1 << 20),
                index: place,
                driver,
                luid: format!("luid_0x{:08X}_0x{:08X}", luid.HighPart as u32, luid.LowPart),
            });
        }
    }
    gpus.sort_by(|a, b| b.dedicated_mb.cmp(&a.dedicated_mb));
    // For tests: only the card asked for, by part of its name ("Intel") or its place in DXGI's list ("1"), as if
    // it were the only one. With `EXCAVATE_GPU_MB` too, it's taken for one with that much memory of its own.
    if let Ok(wanted) = std::env::var("EXCAVATE_GPU_ADAPTER") {
        let wanted = wanted.trim().to_lowercase();
        gpus.retain(|gpu| wanted.parse::<u32>().is_ok_and(|index| index == gpu.index) || gpu.name.to_lowercase().contains(&wanted));
    }
    gpus
}

/// The card chosen in Settings: "" for the best one, "none" for the processor alone, or a card's name.
static CHOSEN: std::sync::RwLock<String> = std::sync::RwLock::new(String::new());

/// Settings' choice of graphics card (`Settings::graphics_card`), for everything in this process that asks `capable`.
pub fn choose(card: &str) {
    *CHOSEN.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = card.trim().to_string();
}

/// The card Excavate runs its models on: the one chosen in Settings when it's there (whatever its memory: chosen on
/// purpose), none when the processor alone was chosen; else the one with the most memory of its own, when that's
/// enough for the vision models (4 GB): a card of its own over a processor's graphics, always.
pub fn capable() -> Option<Gpu> {
    let chosen = CHOSEN.read().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
    if chosen.eq_ignore_ascii_case("none") {
        return None;
    }
    if !chosen.is_empty() {
        if let Some(gpu) = adapters().into_iter().find(|gpu| gpu.name == chosen) {
            return Some(gpu);
        }
    }
    automatic()
}

/// The card "Automatic" picks: the one with the most memory of its own, when that's enough for the vision models
/// (4 GB).
pub fn automatic() -> Option<Gpu> {
    let mut best = adapters().into_iter().next();
    // For tests: what a PC with a smaller graphics card sees.
    if let (Some(gpu), Some(mb)) = (best.as_mut(), std::env::var("EXCAVATE_GPU_MB").ok().and_then(|mb| mb.parse().ok())) {
        gpu.dedicated_mb = mb;
    }
    best.filter(|gpu| gpu.dedicated_mb >= 4000)
}

/// The card `capable` names, as Direct3D takes it (to make a device on it rather than on Windows' default adapter,
/// which on a laptop is the processor's graphics).
pub fn dxgi_adapter() -> Option<windows::Win32::Graphics::Dxgi::IDXGIAdapter1> {
    let gpu = capable()?;
    // SAFETY: a DXGI factory, asked for the adapter at the card's place and released here.
    unsafe {
        let factory = CreateDXGIFactory1::<IDXGIFactory1>().ok()?;
        let adapter = factory.EnumAdapters1(gpu.index).ok()?;
        let description = adapter.GetDesc1().ok()?;
        let length = description.Description.iter().position(|&c| c == 0).unwrap_or(description.Description.len());
        (String::from_utf16_lossy(&description.Description[..length]) == gpu.name).then_some(adapter)
    }
}
