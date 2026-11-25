//! Deterministic registry data verifies native GPU numbering and capacity selection.

mod registry {
    use std::ffi::{c_char, CStr};
    use std::marker::PhantomData;
    use std::ptr;

    use super::super::{Accelerator, Dev, GpuRegistry};

    struct Device {
        kind: i32,
        registry: Option<Option<&'static CStr>>,
        description: Option<&'static CStr>,
    }

    const fn device(
        kind: i32,
        backend: &'static CStr,
        description: &'static CStr,
    ) -> Option<Device> {
        Some(Device {
            kind,
            registry: Some(Some(backend)),
            description: Some(description),
        })
    }

    static DEVICES: [Option<Device>; 12] = [
        device(0, c"CPU", c"CPU"),
        None,
        device(1, c"CUDA", c"wrong backend"),
        Some(Device {
            kind: 1,
            registry: None,
            description: Some(c"no registry"),
        }),
        Some(Device {
            kind: 1,
            registry: Some(None),
            description: Some(c"unnamed registry"),
        }),
        Some(Device {
            kind: 1,
            registry: Some(Some(c"Vulkan")),
            description: None,
        }),
        device(1, c"Vulkan", c"unknown"),
        device(1, c"Vulkan", c"too small"),
        device(1, c"Vulkan", c"too busy"),
        device(1, c"Vulkan", c"adequate"),
        device(2, c"VuLkAn", c"best"),
        device(1, c"Vulkan", c"tied"),
    ];

    unsafe extern "C" fn count() -> usize {
        DEVICES.len()
    }

    unsafe extern "C" fn get(index: usize) -> Dev {
        DEVICES[index].as_ref().map_or(ptr::null_mut(), |device| {
            ptr::from_ref(device).cast_mut().cast()
        })
    }

    unsafe extern "C" fn kind(device: Dev) -> i32 {
        unsafe { (*device.cast::<Device>()).kind }
    }

    unsafe extern "C" fn device_registry(device: Dev) -> Dev {
        if unsafe { (*device.cast::<Device>()).registry.is_some() } {
            device
        } else {
            ptr::null_mut()
        }
    }

    unsafe extern "C" fn registry_name(device: Dev) -> *const c_char {
        unsafe { (*device.cast::<Device>()).registry }
            .flatten()
            .map_or(ptr::null(), CStr::as_ptr)
    }

    unsafe extern "C" fn description(device: Dev) -> *const c_char {
        unsafe { (*device.cast::<Device>()).description }.map_or(ptr::null(), CStr::as_ptr)
    }

    #[test]
    fn gpu_selection_uses_native_ordinals_and_most_free_identifiable_vram() {
        let registry = GpuRegistry {
            count,
            get,
            kind,
            description,
            device_registry,
            registry_name,
            _libraries: PhantomData,
        };
        let capacity = |name: &CStr| match name.to_bytes() {
            b"unknown" => None,
            b"too small" => Some((3072, 3072)),
            b"too busy" => Some((16384, 1024)),
            b"adequate" => Some((8192, 4096)),
            b"best" | b"tied" => Some((8192, 6144)),
            _ => panic!("unusable device reached the capacity probe: {name:?}"),
        };
        // Only native GPUs count towards the ordinal, including those on
        // another accelerator or with unknown capacity.
        assert_eq!(
            registry
                .select(Accelerator::Vulkan, 4096, capacity)
                .unwrap(),
            8
        );
        let error = registry
            .select(Accelerator::Vulkan, 7000, capacity)
            .expect_err("total capacity alone must not select a GPU");
        assert!(error
            .to_string()
            .contains("no identifiable GPU with 7000 MiB"));
    }
}
