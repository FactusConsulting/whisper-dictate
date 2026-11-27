use std::path::PathBuf;

use super::{
    best_free_mib, hviske_cuda, whisper_gpus_for_model_load, whisper_required_mib, SpeechGpu,
};
use crate::cloud_api::hviske_local::LocalModel;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LocalGpuTarget {
    Whisper(u32),
    WhisperFile(PathBuf),
    Nemotron { model: String, device: String },
    Hviske(LocalModel),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LocalGpuCheck {
    Capacity {
        required_mib: u32,
        free_mib: Option<u32>,
    },
    HviskeServiceReady,
    HviskeServiceNotReady {
        required_mib: u32,
        free_mib: Option<u32>,
    },
    Unavailable,
}

impl LocalGpuTarget {
    pub(crate) fn check(&self) -> LocalGpuCheck {
        self.check_with(
            || cfg!(feature = "whisper-rs-vulkan").then(whisper_gpus_for_model_load),
            nemotron_memory,
            |model| crate::cloud_api::hviske_local::ensure_ready(model).is_ok(),
            hviske_cuda::free_mib,
        )
    }

    fn check_with(
        &self,
        whisper_gpus: impl FnOnce() -> Option<Vec<SpeechGpu>>,
        nemotron_memory: impl FnOnce(&str, &str) -> Option<(u32, Option<u32>)>,
        hviske_ready: impl FnOnce(LocalModel) -> bool,
        hviske_free: impl FnOnce(u32) -> Option<u32>,
    ) -> LocalGpuCheck {
        match self {
            Self::Whisper(required_mib) => whisper_capacity(*required_mib, whisper_gpus),
            Self::WhisperFile(path) => match path.metadata() {
                Ok(file) if file.is_file() => {
                    whisper_capacity(whisper_required_mib(file.len()), whisper_gpus)
                }
                _ => LocalGpuCheck::Unavailable,
            },
            Self::Nemotron { model, device } => match nemotron_memory(model, device) {
                Some((required_mib, free_mib)) => LocalGpuCheck::Capacity {
                    required_mib,
                    free_mib,
                },
                None => LocalGpuCheck::Unavailable,
            },
            Self::Hviske(model) => hviske_capacity(*model, hviske_ready, hviske_free),
        }
    }
}

fn whisper_capacity(
    required_mib: u32,
    probe: impl FnOnce() -> Option<Vec<SpeechGpu>>,
) -> LocalGpuCheck {
    let Some(gpus) = probe() else {
        return LocalGpuCheck::Unavailable;
    };
    let free_mib = best_free_mib(
        gpus.into_iter().map(|g| (g.total_mib, g.free_mib)),
        required_mib,
    );
    LocalGpuCheck::Capacity {
        required_mib,
        free_mib,
    }
}

fn nemotron_memory(_model: &str, _device: &str) -> Option<(u32, Option<u32>)> {
    #[cfg(feature = "nemotron-local")]
    {
        crate::dictate::backends::nemotron_local::gpu_memory(_model, _device).ok()
    }
    #[cfg(not(feature = "nemotron-local"))]
    {
        None
    }
}

fn hviske_capacity(
    model: LocalModel,
    service_ready: impl FnOnce(LocalModel) -> bool,
    free_memory: impl FnOnce(u32) -> Option<u32>,
) -> LocalGpuCheck {
    if service_ready(model) {
        return LocalGpuCheck::HviskeServiceReady;
    }
    let required_mib = model.required_mib();
    LocalGpuCheck::HviskeServiceNotReady {
        required_mib,
        free_mib: free_memory(required_mib),
    }
}

#[cfg(test)]
#[path = "start_tests.rs"]
mod tests;
