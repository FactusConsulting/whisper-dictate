//! Physical Vulkan adapter capacity shared by the local STT backends.

use std::ffi::CStr;

use ash::{vk, Entry, Instance};

use super::{SpeechGpu, MIB};

pub(crate) struct Adapters(Vec<Adapter>);

struct Adapter {
    description: Box<CStr>,
    pci_bus: Option<[u32; 4]>,
    capacity: Option<(u32, u32)>,
}

impl Adapters {
    /// Query only physical devices, without opening a logical device or
    /// allocating GPU memory. A missing loader or driver yields no devices.
    pub(crate) fn probe() -> Self {
        // SAFETY: Keep the Vulkan loader alive until the instance has
        // been destroyed. The probe does not return any Vulkan handles.
        unsafe {
            let Ok(entry) = Entry::load() else {
                return Self(Vec::new());
            };
            let Ok(Some(api_version)) = entry.try_enumerate_instance_version() else {
                return Self(Vec::new());
            };
            if api_version < vk::API_VERSION_1_2 {
                return Self(Vec::new());
            }
            let app = vk::ApplicationInfo::default()
                .application_name(c"ggml-vulkan")
                .application_version(1)
                .api_version(api_version);
            let info = vk::InstanceCreateInfo::default().application_info(&app);
            #[cfg(target_os = "macos")]
            let extensions = [ash::khr::portability_enumeration::NAME.as_ptr()];
            #[cfg(target_os = "macos")]
            let info = if entry
                .enumerate_instance_extension_properties(None)
                .is_ok_and(|exts| {
                    supports_extension(&exts, ash::khr::portability_enumeration::NAME)
                }) {
                info.enabled_extension_names(&extensions)
                    .flags(vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR)
            } else {
                info
            };
            let Ok(instance) = entry.create_instance(&info, None) else {
                return Self(Vec::new());
            };
            let adapters = instance.enumerate_physical_devices().map_or_else(
                |_| Vec::new(),
                |devices| {
                    devices
                        .into_iter()
                        .filter_map(|device| Adapter::probe(&instance, device))
                        .collect()
                },
            );
            instance.destroy_instance(None);
            Self(adapters)
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
    pub(crate) fn has_capacity(&self, required_mib: u32) -> bool {
        self.0.iter().any(|adapter| {
            adapter
                .capacity
                .is_some_and(|(total, free)| total >= required_mib && free >= required_mib)
        })
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

impl Adapter {
    /// SAFETY: `device` must have been enumerated from `instance`.
    unsafe fn probe(instance: &Instance, device: vk::PhysicalDevice) -> Option<Self> {
        // SAFETY: Query a live physical device using the owning instance.
        unsafe {
            let properties = instance.get_physical_device_properties(device);
            let description = Box::from(properties.device_name_as_c_str().ok()?);
            let extensions = instance.enumerate_device_extension_properties(device).ok();
            let supports = |name| {
                extensions
                    .as_ref()
                    .is_some_and(|exts| supports_extension(exts, name))
            };
            let pci_bus = supports(ash::ext::pci_bus_info::NAME).then(|| {
                let mut pci = vk::PhysicalDevicePCIBusInfoPropertiesEXT::default();
                let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut pci);
                instance.get_physical_device_properties2(device, &mut properties);
                [
                    pci.pci_domain,
                    pci.pci_bus,
                    pci.pci_device,
                    pci.pci_function,
                ]
            });
            let capacity = if is_hardware_gpu(properties.device_type)
                && properties.api_version >= vk::API_VERSION_1_2
                && supports(ash::ext::memory_budget::NAME)
            {
                let mut vk11 = vk::PhysicalDeviceVulkan11Features::default();
                let mut features = vk::PhysicalDeviceFeatures2::default().push_next(&mut vk11);
                instance.get_physical_device_features2(device, &mut features);
                let has_compute_queue = instance
                    .get_physical_device_queue_family_properties(device)
                    .iter()
                    .any(|queue| {
                        queue.queue_count > 0 && queue.queue_flags.contains(vk::QueueFlags::COMPUTE)
                    });
                if vk11.storage_buffer16_bit_access == vk::TRUE && has_compute_queue {
                    let mut budget = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
                    let mut memory =
                        vk::PhysicalDeviceMemoryProperties2::default().push_next(&mut budget);
                    instance.get_physical_device_memory_properties2(device, &mut memory);
                    let memory_properties = memory.memory_properties;
                    device_local_capacity(&memory_properties, &budget)
                } else {
                    None
                }
            } else {
                None
            };
            // Keep unsupported same-name devices in the inventory so
            // their descriptors cannot be mistaken for hardware.
            Some(Self {
                description,
                pci_bus,
                capacity,
            })
        }
    }
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
}
