//! Physical Vulkan adapter capacity shared by the local STT backends.

use std::ffi::CStr;

use ash::vk;

use super::{GpuInfo, SpeechGpu, MIB};

mod native;

pub(crate) struct Adapters(Vec<Adapter>);

struct Adapter {
    description: Box<CStr>,
    pci_bus: Option<[u32; 4]>,
    capacity: Option<(u32, u32)>,
}

impl Adapters {
    pub(crate) fn probe_for_model_load<T: AsRef<[GpuInfo]>>(query: impl FnOnce() -> T) -> Self {
        Self::probe().with_nvidia_free_limit(query)
    }

    pub(crate) fn with_nvidia_free_limit<T: AsRef<[GpuInfo]>>(
        mut self,
        query: impl FnOnce() -> T,
    ) -> Self {
        if self.0.iter().any(|adapter| {
            adapter.capacity.is_some() && adapter.description.to_bytes().starts_with(b"NVIDIA ")
        }) {
            self.limit_nvidia_free_mib(query().as_ref());
        }
        self
    }

    #[cfg(all(test, feature = "nemotron-local", any(windows, target_os = "linux")))]
    pub(crate) fn with_test_capacity(description: &CStr, total_mib: u32, free_mib: u32) -> Self {
        Self(vec![Adapter {
            description: description.into(),
            pci_bus: None,
            capacity: Some((total_mib, free_mib)),
        }])
    }

    /// Vulkan's per-process budget can exceed currently unused physical VRAM.
    /// When both reports identify the adapter, use the smaller value so starting
    /// a model does not rely on paging out another workload.
    pub(crate) fn limit_nvidia_free_mib(&mut self, gpus: &[GpuInfo]) {
        for adapter in &mut self.0 {
            if let Some((_, free)) = &mut adapter.capacity {
                for gpu in gpus
                    .iter()
                    .filter(|gpu| gpu.name.as_bytes() == adapter.description.to_bytes())
                {
                    // Duplicate descriptions are ambiguous; use the lowest free
                    // memory instead of matching unrelated driver ordinals.
                    *free = (*free).min(gpu.free_mb);
                }
            }
        }
    }

    pub(crate) fn max_total_mib(&self) -> u32 {
        self.0
            .iter()
            .filter_map(|adapter| adapter.capacity.map(|(total, _)| total))
            .max()
            .unwrap_or(0)
    }

    #[cfg(feature = "nemotron-local")]
    pub(crate) fn best_free_mib(&self, required_mib: u32) -> Option<u32> {
        super::best_free_mib(
            self.0.iter().filter_map(|adapter| adapter.capacity),
            required_mib,
        )
    }

    /// Resolve a ggml descriptor against independently queried physical
    /// devices. Without PCI information a duplicate name is safe only
    /// if every matching device is supported, with the lowest capacities.
    pub(crate) fn capacity(
        &self,
        description: &CStr,
        pci_bus_id: Option<&CStr>,
    ) -> Option<(u32, u32)> {
        let pci_bus = match pci_bus_id {
            Some(id) => Some(parse_pci_bus(id)?),
            None => None,
        };
        let mut matching = self.0.iter().filter(|adapter| {
            adapter.description.as_ref() == description
                && (pci_bus.is_none() || adapter.pci_bus == pci_bus)
        });
        let (mut total, mut free) = matching.next()?.capacity?;
        for adapter in matching {
            let (adapter_total, adapter_free) = adapter.capacity?;
            total = total.min(adapter_total);
            free = free.min(adapter_free);
        }
        Some((total, free))
    }

    pub(crate) fn whisper_gpus(&self) -> Vec<SpeechGpu> {
        #[cfg(feature = "whisper-rs-vulkan")]
        {
            self.registered_whisper_gpus()
        }
        #[cfg(not(feature = "whisper-rs-vulkan"))]
        {
            Vec::new()
        }
    }

    #[cfg(feature = "whisper-rs-vulkan")]
    fn registered_whisper_gpus(&self) -> Vec<SpeechGpu> {
        use whisper_rs::whisper_rs_sys as sys;

        const MAX_VULKAN_DEVICES: usize = 16; // GGML_VK_MAX_DEVICES in whisper-rs-sys 0.15
        if self.max_total_mib() == 0
            || std::env::var_os("GGML_VK_VISIBLE_DEVICES").is_some_and(|value| value.is_empty())
        {
            return Vec::new();
        }

        // SAFETY: Use Whisper's static ggml registry, whose getters only
        // enumerate devices and read cached descriptions. Bound the Vulkan
        // slots before use and the global indices with ggml_backend_dev_count.
        // Neither the upstream buffer-type getter nor list_devices is
        // suitable for probing: both can initialize a logical device.
        unsafe {
            let reg = sys::ggml_backend_reg_by_name(c"Vulkan".as_ptr());
            if reg.is_null() || sys::ggml_backend_reg_dev_count(reg) > MAX_VULKAN_DEVICES {
                return Vec::new();
            }
            let mut gpus = Vec::new();
            let mut ordinal = 0;
            for index in 0..sys::ggml_backend_dev_count() {
                let device = sys::ggml_backend_dev_get(index);
                if device.is_null() {
                    return Vec::new();
                }
                let device_type = sys::ggml_backend_dev_type(device);
                if !matches!(
                    device_type,
                    sys::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_GPU
                        | sys::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_IGPU
                ) {
                    continue;
                }
                // whisper_backend_init_gpu counts every GPU and IGPU,
                // even if this probe rejects one of them.
                let gpu_index = ordinal;
                ordinal += 1;
                if sys::ggml_backend_dev_backend_reg(device) != reg {
                    continue;
                }
                let description = sys::ggml_backend_dev_description(device);
                if description.is_null() {
                    continue;
                }
                // The description is a C string owned by this registered
                // device. The richer GGML properties getter re-enumerates
                // physical devices and reads unreliable memory fields;
                // resolve duplicate names conservatively instead.
                let description = CStr::from_ptr(description);
                if let Some((total_mib, free_mib)) = self.capacity(description, None) {
                    gpus.push(SpeechGpu {
                        index: gpu_index,
                        total_mib,
                        free_mib,
                    });
                }
            }
            gpus
        }
    }
}

fn supports_capacity_query(
    properties: &vk::PhysicalDeviceProperties,
    extensions: &[vk::ExtensionProperties],
) -> bool {
    is_hardware_gpu(properties.device_type)
        && supports_api_version(properties.api_version)
        && supports_extension(extensions, ash::ext::memory_budget::NAME)
}

fn supports_api_version(api_version: u32) -> bool {
    api_version >= vk::API_VERSION_1_2
}

fn supports_compute(
    storage_buffer16_bit_access: vk::Bool32,
    queues: &[vk::QueueFamilyProperties],
) -> bool {
    storage_buffer16_bit_access == vk::TRUE
        && queues.iter().any(|queue| {
            queue.queue_count > 0 && queue.queue_flags.contains(vk::QueueFlags::COMPUTE)
        })
}

fn is_hardware_gpu(device_type: vk::PhysicalDeviceType) -> bool {
    matches!(
        device_type,
        vk::PhysicalDeviceType::DISCRETE_GPU | vk::PhysicalDeviceType::INTEGRATED_GPU
    )
}

fn device_local_capacity(
    properties: &vk::PhysicalDeviceMemoryProperties,
    budget: &vk::PhysicalDeviceMemoryBudgetPropertiesEXT<'_>,
) -> Option<(u32, u32)> {
    let heaps = properties
        .memory_heaps
        .get(..properties.memory_heap_count as usize)?;
    let mut total = 0;
    let mut free = 0;
    for (index, heap) in heaps.iter().enumerate() {
        if heap.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL) && budget.heap_budget[index] > 0 {
            // Don't add capacities of different heaps, which may share
            // backing memory. A single heap must have enough headroom.
            total = total.max(heap.size);
            free = free.max(
                budget.heap_budget[index]
                    .min(heap.size)
                    .saturating_sub(budget.heap_usage[index]),
            );
        }
    }
    (total >= MIB).then(|| (to_mib(total), to_mib(free)))
}

fn to_mib(bytes: u64) -> u32 {
    u32::try_from(bytes / MIB).unwrap_or(u32::MAX)
}

fn supports_extension(extensions: &[vk::ExtensionProperties], name: &CStr) -> bool {
    extensions.iter().any(|ext| {
        ext.extension_name_as_c_str()
            .is_ok_and(|actual| actual == name)
    })
}

fn parse_pci_bus(pci_bus: &CStr) -> Option<[u32; 4]> {
    let mut fields = pci_bus.to_str().ok()?.split([':', '.']);
    let mut pci = [0; 4];
    for field in &mut pci {
        *field = u32::from_str_radix(fields.next()?, 16).ok()?;
    }
    fields.next().is_none().then_some(pci)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_cpu_and_unverified_virtual_gpu_types() {
        assert!(!is_hardware_gpu(vk::PhysicalDeviceType::CPU));
        assert!(!is_hardware_gpu(vk::PhysicalDeviceType::VIRTUAL_GPU));
        assert!(!is_hardware_gpu(vk::PhysicalDeviceType::OTHER));
        assert!(is_hardware_gpu(vk::PhysicalDeviceType::DISCRETE_GPU));
        assert!(is_hardware_gpu(vk::PhysicalDeviceType::INTEGRATED_GPU));
    }

    #[test]
    fn capacity_query_requires_vulkan_1_2_hardware_and_memory_budget_extension() {
        let mut malformed = vk::ExtensionProperties::default();
        malformed.extension_name.fill(1); // No NUL terminator.
        let extensions = [
            malformed,
            vk::ExtensionProperties::default()
                .extension_name(ash::ext::pci_bus_info::NAME)
                .unwrap(),
            vk::ExtensionProperties::default()
                .extension_name(ash::ext::memory_budget::NAME)
                .unwrap(),
        ];
        let mut properties = vk::PhysicalDeviceProperties {
            device_type: vk::PhysicalDeviceType::INTEGRATED_GPU,
            api_version: vk::API_VERSION_1_2,
            ..Default::default()
        };
        assert!(supports_capacity_query(&properties, &extensions));
        assert!(!supports_capacity_query(&properties, &extensions[..2]));
        assert!(!supports_capacity_query(&properties, &[]));

        properties.api_version = vk::API_VERSION_1_1;
        assert!(!supports_capacity_query(&properties, &extensions));

        properties.api_version = vk::API_VERSION_1_2;
        properties.device_type = vk::PhysicalDeviceType::CPU;
        assert!(!supports_capacity_query(&properties, &extensions));
    }

    #[test]
    fn compute_requires_16_bit_storage_and_a_nonempty_compute_queue_family() {
        let queues = [
            vk::QueueFamilyProperties {
                queue_count: 1,
                queue_flags: vk::QueueFlags::GRAPHICS,
                ..Default::default()
            },
            vk::QueueFamilyProperties {
                queue_count: 0,
                queue_flags: vk::QueueFlags::COMPUTE,
                ..Default::default()
            },
            vk::QueueFamilyProperties {
                queue_count: 1,
                queue_flags: vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE,
                ..Default::default()
            },
        ];
        assert!(!supports_compute(vk::TRUE, &[]));
        assert!(!supports_compute(vk::TRUE, &queues[..2]));
        assert!(!supports_compute(vk::FALSE, &queues));
        assert!(supports_compute(vk::TRUE, &queues));
    }

    #[test]
    fn empty_inventory_exposes_no_whisper_gpus() {
        let adapters = Adapters(Vec::new());
        assert_eq!(adapters.max_total_mib(), 0);
        assert!(adapters.whisper_gpus().is_empty());
    }

    #[test]
    fn model_load_uses_available_physical_vram_when_vulkan_budget_is_higher() {
        let nvidia = c"NVIDIA RTX test";
        let other = c"Other adapter";
        let adapters = Adapters(vec![
            Adapter {
                description: nvidia.into(),
                pci_bus: None,
                capacity: Some((10240, 3817)),
            },
            Adapter {
                description: other.into(),
                pci_bus: None,
                capacity: Some((8192, 4096)),
            },
        ]);
        let reported = |index, name: &CStr, free_mb| GpuInfo {
            index,
            name: name.to_string_lossy().into_owned(),
            total_mb: 10240,
            free_mb,
            source: "nvidia-smi",
        };
        let adapters = adapters.with_nvidia_free_limit(|| {
            [
                reported(1, nvidia, 1134),
                reported(0, nvidia, 8000),
                reported(2, c"Unmatched", 0),
            ]
        });
        assert_eq!(adapters.capacity(nvidia, None), Some((10240, 1134)));
        assert_eq!(adapters.capacity(other, None), Some((8192, 4096)));
    }

    #[test]
    fn model_load_ignores_nvidia_diagnostics_without_a_supported_nvidia_adapter() {
        let adapters = Adapters(vec![
            Adapter {
                description: c"NVIDIA without a supported memory query".into(),
                pci_bus: None,
                capacity: None,
            },
            Adapter {
                description: c"Other adapter".into(),
                pci_bus: None,
                capacity: Some((8192, 4096)),
            },
        ])
        .with_nvidia_free_limit(|| -> Vec<GpuInfo> {
            panic!("no supported NVIDIA capacity to reconcile")
        });
        assert_eq!(
            adapters.capacity(c"Other adapter", None),
            Some((8192, 4096))
        );
    }

    #[cfg(feature = "nemotron-local")]
    #[test]
    fn capacity_uses_a_single_supported_adapter_without_pooling() {
        let mut adapters = Adapters(vec![
            Adapter {
                description: c"Unsupported".into(),
                pci_bus: None,
                capacity: None,
            },
            Adapter {
                description: c"Higher total".into(),
                pci_bus: None,
                capacity: Some((8192, 2048)),
            },
            Adapter {
                description: c"Higher free".into(),
                pci_bus: None,
                capacity: Some((4096, 4096)),
            },
        ]);
        assert_eq!(adapters.max_total_mib(), 8192);
        assert_eq!(adapters.best_free_mib(4096), Some(4096));
        assert_eq!(adapters.best_free_mib(4097), Some(2048));

        adapters.0.truncate(1);
        assert_eq!(adapters.max_total_mib(), 0);
        assert_eq!(adapters.best_free_mib(0), None);
    }

    #[test]
    fn matches_duplicate_descriptions_by_pci_or_conservative_capacity() {
        let description = c"Duplicated Vulkan description";
        let mut adapters = Adapters(vec![
            Adapter {
                description: description.into(),
                pci_bus: Some([0, 2, 0, 0]),
                capacity: Some((8192, 4096)),
            },
            Adapter {
                description: description.into(),
                pci_bus: Some([0, 10, 0, 0]),
                capacity: Some((4096, 2000)),
            },
        ]);
        assert_eq!(adapters.capacity(description, None), Some((4096, 2000)));
        assert_eq!(
            adapters.capacity(description, Some(c"0000:02:00.0")),
            Some((8192, 4096))
        );
        assert_eq!(
            adapters.capacity(description, Some(c"0000:0a:00.0")),
            Some((4096, 2000))
        );
        assert_eq!(adapters.capacity(description, Some(c"0000:03:00.0")), None);
        assert_eq!(adapters.capacity(c"Different description", None), None);
        for malformed in [
            c"0000:02:00",
            c"0000:02:00.0.1",
            c"0000:0g:00.0",
            c"0000:02:00.\xFF",
        ] {
            assert_eq!(adapters.capacity(description, Some(malformed)), None);
        }

        adapters.0.push(Adapter {
            description: description.into(),
            pci_bus: None,
            capacity: None,
        });
        assert_eq!(adapters.capacity(description, None), None);
        assert_eq!(
            adapters.capacity(description, Some(c"0000:02:00.0")),
            Some((8192, 4096))
        );
    }

    #[test]
    fn only_device_local_reported_budgets_count_without_pooling() {
        let mut properties = vk::PhysicalDeviceMemoryProperties {
            memory_heap_count: 3,
            ..Default::default()
        };
        properties.memory_heaps[0] = vk::MemoryHeap {
            size: 8192 * MIB,
            flags: vk::MemoryHeapFlags::DEVICE_LOCAL,
        };
        properties.memory_heaps[1] = vk::MemoryHeap {
            size: 4096 * MIB,
            flags: vk::MemoryHeapFlags::DEVICE_LOCAL,
        };
        properties.memory_heaps[2].size = 16_384 * MIB;
        let mut budget = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
        assert_eq!(device_local_capacity(&properties, &budget), None);

        budget.heap_budget[..3].copy_from_slice(&[4000 * MIB, 4096 * MIB, 16_384 * MIB]);
        budget.heap_usage[..2].copy_from_slice(&[3200 * MIB, 2048 * MIB]);
        assert_eq!(
            device_local_capacity(&properties, &budget),
            Some((8192, 2048))
        );
        budget.heap_usage[..2].copy_from_slice(&[4500 * MIB, 4500 * MIB]);
        assert_eq!(device_local_capacity(&properties, &budget), Some((8192, 0)));

        properties.memory_heap_count = properties.memory_heaps.len() as u32 + 1;
        assert_eq!(device_local_capacity(&properties, &budget), None);
    }

    #[test]
    fn device_local_budget_is_clamped_to_heap_size_before_usage_is_subtracted() {
        let mut properties = vk::PhysicalDeviceMemoryProperties {
            memory_heap_count: 1,
            ..Default::default()
        };
        properties.memory_heaps[0] = vk::MemoryHeap {
            size: 2 * MIB + MIB / 2,
            flags: vk::MemoryHeapFlags::DEVICE_LOCAL,
        };
        let mut budget = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
        budget.heap_budget[0] = 4 * MIB;
        budget.heap_usage[0] = MIB;
        assert_eq!(device_local_capacity(&properties, &budget), Some((2, 1)));

        properties.memory_heaps[0].size = MIB - 1;
        assert_eq!(device_local_capacity(&properties, &budget), None);

        properties.memory_heaps[0].size = u64::MAX;
        budget.heap_budget[0] = u64::MAX;
        assert_eq!(
            device_local_capacity(&properties, &budget),
            Some((u32::MAX, u32::MAX))
        );
    }
}
