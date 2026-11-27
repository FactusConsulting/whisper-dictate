use super::{best_free_mib, nvidia_smi};

/// Report the largest individual NVIDIA GPU with native BF16 support.
/// The local service independently checks CUDA and free VRAM before loading.
pub(crate) fn hviske_cuda_total_mib() -> Option<u32> {
    let output = nvidia_smi("--query-gpu=memory.total,compute_cap")?;
    parse_hviske_cuda_total_mib(std::str::from_utf8(&output).ok()?)
}

pub(super) fn free_mib(required_mib: u32) -> Option<u32> {
    let output = nvidia_smi("--query-gpu=memory.total,memory.free,compute_cap")?;
    parse_free_mib(std::str::from_utf8(&output).ok()?, required_mib)
}

fn parse_free_mib(output: &str, required_mib: u32) -> Option<u32> {
    let capacities = output.lines().filter_map(|row| {
        let (total, rest) = row.split_once(',')?;
        let (free, capability) = rest.split_once(',')?;
        supports_bf16(capability).then_some(())?;
        Some((total.trim().parse().ok()?, free.trim().parse().ok()?))
    });
    best_free_mib(capacities, required_mib)
}

fn supports_bf16(capability: &str) -> bool {
    capability
        .trim()
        .split_once('.')
        .is_some_and(|(major, minor)| {
            major.parse::<u32>().is_ok_and(|major| major >= 8) && minor.parse::<u32>().is_ok()
        })
}

fn parse_hviske_cuda_total_mib(output: &str) -> Option<u32> {
    output
        .lines()
        .filter_map(|row| {
            let (total, capability) = row.split_once(',')?;
            supports_bf16(capability).then_some(())?;
            total.trim().parse::<u32>().ok()
        })
        .max()
}

#[cfg(test)]
mod tests {
    use super::{parse_free_mib, parse_hviske_cuda_total_mib};

    #[test]
    fn hviske_probe_excludes_pre_bf16_and_unreadable_gpus_without_pooling_memory() {
        assert_eq!(
            parse_hviske_cuda_total_mib(
                "missing separator\n24576, 7.5\n8192, [N/A]\n4, 8.unknown\n[N/A], 8.6"
            ),
            None
        );
        assert_eq!(
            parse_hviske_cuda_total_mib("24576, 7.5\n4096, 8.0\n2048, 8.9\n5120, 12.0"),
            Some(5120)
        );
    }

    #[test]
    fn hviske_free_memory_uses_one_gpu_that_meets_the_model_requirement() {
        let gpus = "24576, 20000, 7.5\n4096, 4000, 8.0\n8192, 1000, 8.6\n8192, 2000, 8.9";
        assert_eq!(parse_free_mib(gpus, 3584), Some(4000));
        assert_eq!(parse_free_mib(gpus, 6144), Some(2000));
        assert_eq!(parse_free_mib(gpus, 16384), None);
    }
}
