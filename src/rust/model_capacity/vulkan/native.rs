//! Vulkan loader and physical-device queries. No logical devices are opened.

use ash::{vk, Entry, Instance};

use super::{
    device_local_capacity, supports_api_version, supports_capacity_query, supports_compute,
    supports_extension, Adapter, Adapters,
};

impl Adapters {
    /// Query only physical devices, without opening a logical device or
    /// allocating GPU memory. A missing loader or driver yields no devices.
    pub(crate) fn probe() -> Self {
        // SAFETY: `with_loader` destroys its Vulkan instance before it drops
        // the owning entry; no driver handles leave the probe.
        Self::with_loader(unsafe { Entry::load() }.ok())
    }

    fn with_loader(entry: Option<Entry>) -> Self {
        let Some(entry) = entry else {
            return Self(Vec::new());
        };
        // SAFETY: Keep the Vulkan loader alive until the instance has
        // been destroyed. The probe does not return any Vulkan handles.
        unsafe {
            let Ok(Some(api_version)) = entry.try_enumerate_instance_version() else {
                return Self(Vec::new());
            };
            if !supports_api_version(api_version) {
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
}

impl Adapter {
    /// SAFETY: `device` must have been enumerated from `instance`.
    unsafe fn probe(instance: &Instance, device: vk::PhysicalDevice) -> Option<Self> {
        // SAFETY: Query a live physical device using the owning instance.
        unsafe {
            let properties = instance.get_physical_device_properties(device);
            let description = Box::from(properties.device_name_as_c_str().ok()?);
            let extensions = instance
                .enumerate_device_extension_properties(device)
                .unwrap_or_default();
            let pci_bus =
                supports_extension(&extensions, ash::ext::pci_bus_info::NAME).then(|| {
                    let mut pci = vk::PhysicalDevicePCIBusInfoPropertiesEXT::default();
                    let mut properties =
                        vk::PhysicalDeviceProperties2::default().push_next(&mut pci);
                    instance.get_physical_device_properties2(device, &mut properties);
                    [
                        pci.pci_domain,
                        pci.pci_bus,
                        pci.pci_device,
                        pci.pci_function,
                    ]
                });
            let capacity = if supports_capacity_query(&properties, &extensions) {
                let mut vk11 = vk::PhysicalDeviceVulkan11Features::default();
                let mut features = vk::PhysicalDeviceFeatures2::default().push_next(&mut vk11);
                instance.get_physical_device_features2(device, &mut features);
                let can_compute = supports_compute(
                    vk11.storage_buffer16_bit_access,
                    &instance.get_physical_device_queue_family_properties(device),
                );
                if can_compute {
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

#[cfg(test)]
mod tests {
    use ash::Entry;

    use super::Adapters;

    #[test]
    fn missing_vulkan_loader_reports_no_usable_adapter() {
        let directory = tempfile::tempdir().unwrap();
        // SAFETY: The file does not exist; no foreign library is loaded.
        let entry = unsafe { Entry::load_from(directory.path().join("missing-loader")) }.ok();
        assert_eq!(Adapters::with_loader(entry).max_total_mib(), 0);
    }
}
