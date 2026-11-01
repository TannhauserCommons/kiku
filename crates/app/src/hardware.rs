//! Finds the PC's processors, memory and NVIDIA GPU, through DXGI and NVIDIA's driver, and how much
//! GPU memory is free before a model is loaded there.

use kiku_core::hardware::{Gpu, Hardware};
use windows::{
    core::{s, w, Interface},
    Win32::{
        Foundation::FreeLibrary,
        Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1},
        System::{
            LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32},
            SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX},
        },
    },
};

const NVIDIA: u32 = 0x10DE;

pub fn detect() -> Hardware {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut memory = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    let ram_mb = unsafe { GlobalMemoryStatusEx(&mut memory) }.map_or(0, |_| memory.ullTotalPhys / (1 << 20));
    let gpu = nvidia_adapter().map(|adapter| describe(&adapter));
    Hardware { threads, ram_mb, gpu }
}

/// Free memory on the first NVIDIA GPU in MB, from the driver's NVML in System32. DXGI only knows
/// this process's budget, which ignores what other apps hold.
pub fn free_vram_mb() -> Option<u32> {
    #[repr(C)]
    struct Memory {
        total: u64,
        free: u64,
        used: u64,
    }
    unsafe {
        let library = LoadLibraryExW(w!("nvml.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
        let function = |name| GetProcAddress(library, name);
        let free = (|| {
            let init: extern "C" fn() -> i32 = std::mem::transmute(function(s!("nvmlInit_v2"))?);
            let device: extern "C" fn(u32, *mut usize) -> i32 = std::mem::transmute(function(s!("nvmlDeviceGetHandleByIndex_v2"))?);
            let memory: extern "C" fn(usize, *mut Memory) -> i32 = std::mem::transmute(function(s!("nvmlDeviceGetMemoryInfo"))?);
            let shutdown: extern "C" fn() -> i32 = std::mem::transmute(function(s!("nvmlShutdown"))?);
            if init() != 0 {
                return None;
            }
            let (mut handle, mut info) = (0, Memory { total: 0, free: 0, used: 0 });
            let read = device(0, &mut handle) == 0 && memory(handle, &mut info) == 0;
            shutdown();
            read.then_some((info.free >> 20) as u32)
        })();
        let _ = FreeLibrary(library);
        free
    }
}

/// The NVIDIA adapter with the most memory, which CUDA's device 0 almost always is.
fn nvidia_adapter() -> Option<IDXGIAdapter1> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
    (0..)
        .map_while(|index| unsafe { factory.EnumAdapters1(index) }.ok())
        .filter_map(|adapter| Some((unsafe { adapter.GetDesc1() }.ok()?, adapter)))
        .filter(|(desc, _)| desc.VendorId == NVIDIA)
        .max_by_key(|(desc, _)| desc.DedicatedVideoMemory)
        .map(|(_, adapter)| adapter)
}

fn describe(adapter: &IDXGIAdapter1) -> Gpu {
    let desc = unsafe { adapter.GetDesc1() }.unwrap_or_default();
    let name_len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
    let name = String::from_utf16_lossy(&desc.Description[..name_len]).replace("NVIDIA ", "");
    let driver = unsafe { adapter.CheckInterfaceSupport(&IDXGIDevice::IID) }.map_or_else(|_| "unknown".to_owned(), nvidia_version);
    Gpu { name, vram_mb: (desc.DedicatedVideoMemory >> 20) as u32, driver, cuda_driver: cuda_driver_version() }
}

/// NVIDIA's name for a driver version as Windows numbers it: 32.0.15.6094 is 560.94, from its last five digits.
fn nvidia_version(windows: i64) -> String {
    let nvidia = ((windows >> 16) & 0xFFFF) % 10 * 10_000 + (windows & 0xFFFF);
    format!("{}.{:02}", nvidia / 100, nvidia % 100)
}

/// The newest CUDA the installed driver supports, asked of the driver's own nvcuda.dll in System32.
/// Asking needs no GPU context, so it costs nothing on the GPU.
fn cuda_driver_version() -> Option<u32> {
    unsafe {
        let library = LoadLibraryExW(w!("nvcuda.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
        let version = GetProcAddress(library, s!("cuDriverGetVersion")).and_then(|function| {
            let get_version: extern "system" fn(*mut i32) -> i32 = std::mem::transmute(function);
            let mut version = 0;
            (get_version(&mut version) == 0).then_some(version as u32)
        });
        let _ = FreeLibrary(library);
        version
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvidia_driver_versions_come_from_windows_numbers() {
        let windows = |parts: [i64; 4]| parts[0] << 48 | parts[1] << 32 | parts[2] << 16 | parts[3];
        assert_eq!(nvidia_version(windows([32, 0, 15, 6094])), "560.94");
        assert_eq!(nvidia_version(windows([32, 0, 16, 1062])), "610.62");
        assert_eq!(nvidia_version(windows([31, 0, 15, 2741])), "527.41");
    }
}
