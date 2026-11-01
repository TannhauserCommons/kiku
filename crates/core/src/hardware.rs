//! What the PC can run, and the model to recommend for it. Detection is per OS (the app's
//! `hardware` module); the rules here are not.

use crate::{
    catalog::{self, Model, Variant},
    engine::Device,
};

/// CUDA 12 and cuDNN 9 need a driver that supports CUDA 12.0: NVIDIA's R525 or later.
pub const MIN_CUDA_DRIVER: u32 = 12_000;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hardware {
    /// Logical processors.
    pub threads: usize,
    pub ram_mb: u64,
    /// The NVIDIA GPU with the most memory, if there is one.
    pub gpu: Option<Gpu>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Gpu {
    pub name: String,
    pub vram_mb: u32,
    /// As NVIDIA writes it, e.g. "581.57".
    pub driver: String,
    /// The newest CUDA the driver supports, e.g. 12080 for 12.8; `None` if it couldn't be asked.
    pub cuda_driver: Option<u32>,
}

/// A model and where to run it, with a sentence on why.
#[derive(Clone, Debug, PartialEq)]
pub struct Recommendation {
    pub model: &'static Model,
    pub variant: &'static Variant,
    pub why: String,
}

impl Hardware {
    /// Why models can't run on the GPU here, or `None` if they can.
    pub fn gpu_problem(&self) -> Option<String> {
        let Some(gpu) = &self.gpu else { return Some("no NVIDIA graphics card found".into()) };
        match gpu.cuda_driver {
            Some(version) if version >= MIN_CUDA_DRIVER => None,
            Some(_) => Some(format!("the NVIDIA driver ({}) is too old for CUDA 12: update it to 527 or later", gpu.driver)),
            None => Some(format!("the NVIDIA driver ({}) doesn't offer CUDA", gpu.driver)),
        }
    }

    /// Parakeet Unified, the most accurate model: its fp32 build on an NVIDIA GPU with a CUDA 12 driver
    /// and room for it (8 GB), the GPU pack downloading with it; otherwise its int8 build on the CPU.
    pub fn recommend(&self) -> Recommendation {
        let unified = catalog::by_id_or_default(catalog::DEFAULT_ID);
        let most_precise = unified.variants.iter().find(|v| v.device == Device::Cuda);
        if let (None, Some(gpu), Some(variant)) = (self.gpu_problem(), &self.gpu, most_precise)
            && variant.fits(gpu.vram_mb)
        {
            let why = format!("Runs on your {} with almost no CPU", gpu.name);
            return Recommendation { model: unified, variant, why };
        }
        let why = match (self.gpu_problem(), &self.gpu, most_precise) {
            (Some(problem), Some(_), _) => format!("On the CPU: {problem}"),
            (None, Some(gpu), Some(variant)) => format!("Your {} has {} GB; its GPU build needs {} GB", gpu.name, gb(gpu.vram_mb), gb(variant.gpu_memory_needed_mb())),
            _ => "The most accurate model, in its build for the CPU".into(),
        };
        Recommendation { model: unified, variant: unified.variant_for(Device::Cpu, 0), why }
    }
}

/// Whole gigabytes, rounded up: what a card's memory is called.
pub fn gb(mb: u32) -> u32 {
    mb.div_ceil(1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pc(threads: usize, ram_gb: u64, gpu: Option<(u32, u32)>) -> Hardware {
        let gpu = gpu.map(|(vram_gb, cuda)| Gpu { name: "RTX".into(), vram_mb: vram_gb * 1024, driver: "560.94".into(), cuda_driver: Some(cuda) });
        Hardware { threads, ram_mb: ram_gb * 1024, gpu }
    }

    #[test]
    fn recommendations_follow_the_hardware() {
        let pick = |hardware: Hardware| {
            let r = hardware.recommend();
            (r.model.id.clone(), r.variant.device, r.variant.precision.clone())
        };
        let cpu = ("parakeet-unified".to_owned(), Device::Cpu, "int8".to_owned());
        assert_eq!(pick(pc(24, 32, Some((24, 12_080)))), ("parakeet-unified".into(), Device::Cuda, "fp32".into()));
        assert_eq!(pick(pc(16, 16, Some((8, 12_080)))).1, Device::Cuda);
        // A card short of room for fp32, or a driver too old for CUDA 12, leaves the CPU.
        assert_eq!(pick(pc(8, 16, Some((6, 12_080)))), cpu);
        assert_eq!(pick(pc(16, 16, Some((8, 11_080)))), cpu);
        assert_eq!(pick(pc(4, 8, None)), cpu);
        // Why says so up front.
        assert!(pc(8, 16, Some((6, 12_080))).recommend().why.ends_with("has 6 GB; its GPU build needs 8 GB"));
        assert!(pc(8, 16, Some((8, 11_080))).recommend().why.contains("too old for CUDA 12"));
    }
}
