//! Read the pinned Nemotron runtime's device registry and select a GPU that fits.

use std::ffi::{c_char, c_void, CStr};

use anyhow::{anyhow, Result};
use libloading::Library;

use super::{symbol, Accelerator};

#[cfg(test)]
#[path = "nemotron_gpu_registry_tests.rs"]
mod tests;

type Dev = *mut c_void;
type DevCount = unsafe extern "C" fn() -> usize;
type DevGet = unsafe extern "C" fn(usize) -> Dev;
type DevType = unsafe extern "C" fn(Dev) -> std::ffi::c_int;
type DevString = unsafe extern "C" fn(Dev) -> *const c_char;
type DevReg = unsafe extern "C" fn(Dev) -> *mut c_void;

pub(super) struct GpuRegistry<'lib> {
    count: DevCount,
    get: DevGet,
    kind: DevType,
    description: DevString,
    device_registry: DevReg,
    registry_name: DevString,
    // The copied function pointers must not outlive either library.
    _libraries: std::marker::PhantomData<&'lib Library>,
}

impl<'lib> GpuRegistry<'lib> {
    pub(super) fn load(registry: &'lib Library, base: &'lib Library) -> Result<Self> {
        Ok(Self {
            count: unsafe { symbol(registry, b"ggml_backend_dev_count\0")? },
            get: unsafe { symbol(registry, b"ggml_backend_dev_get\0")? },
            kind: unsafe { symbol(base, b"ggml_backend_dev_type\0")? },
            description: unsafe { symbol(base, b"ggml_backend_dev_description\0")? },
            device_registry: unsafe { symbol(base, b"ggml_backend_dev_backend_reg\0")? },
            registry_name: unsafe { symbol(base, b"ggml_backend_reg_name\0")? },
            _libraries: std::marker::PhantomData,
        })
    }

    pub(super) fn select(
        &self,
        accelerator: Accelerator,
        required_mib: u32,
        mut capacity: impl FnMut(&CStr) -> Option<(u32, u32)>,
    ) -> Result<i32> {
        const GPU: std::ffi::c_int = 1;
        const IGPU: std::ffi::c_int = 2;

        let mut best: Option<(u32, i32)> = None;
        let mut gpu_index = 0_i32;
        for index in 0..unsafe { (self.count)() } {
            let device = unsafe { (self.get)(index) };
            if device.is_null() || !matches!(unsafe { (self.kind)(device) }, GPU | IGPU) {
                continue;
            }
            let index = gpu_index;
            gpu_index += 1;
            let reg = unsafe { (self.device_registry)(device) };
            if reg.is_null() {
                continue;
            }
            let backend = unsafe { (self.registry_name)(reg) };
            if backend.is_null()
                || !unsafe { CStr::from_ptr(backend) }
                    .to_bytes()
                    .eq_ignore_ascii_case(accelerator.label().as_bytes())
            {
                continue;
            }
            let description = unsafe { (self.description)(device) };
            if description.is_null() {
                continue;
            }
            let Some((total_mib, free_mib)) = capacity(unsafe { CStr::from_ptr(description) })
            else {
                continue;
            };
            if total_mib >= required_mib
                && free_mib >= required_mib
                && best.is_none_or(|(best_free, _)| free_mib > best_free)
            {
                best = Some((free_mib, index));
            }
        }
        best.map(|(_, index)| index).ok_or_else(|| {
            anyhow!(
                "the verified Nemotron {} runtime has no identifiable GPU with {required_mib} MiB total and free VRAM",
                accelerator.label()
            )
        })
    }
}
