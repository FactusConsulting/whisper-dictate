use std::process::Command;

/// Report the largest individual NVIDIA GPU with native BF16 support.
/// The local service independently checks CUDA and free VRAM before loading.
pub(crate) fn hviske_cuda_total_mib() -> Option<u32> {
    #[cfg(windows)]
    use std::os::windows::process::CommandExt as _;

    let mut command = Command::new("nvidia-smi");
    command.args([
        "--query-gpu=memory.total,compute_cap",
        "--format=csv,noheader,nounits",
    ]);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    crate::runtime::settings_snapshot::scrub_credentials_from_child(&mut command);
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse_hviske_cuda_total_mib(std::str::from_utf8(&output.stdout).ok()?)
}

fn parse_hviske_cuda_total_mib(output: &str) -> Option<u32> {
    output
        .lines()
        .filter_map(|row| {
            let (total, capability) = row.split_once(',')?;
            let (major, minor) = capability.trim().split_once('.')?;
            if major.parse::<u32>().ok()? < 8 || minor.parse::<u32>().is_err() {
                return None;
            }
            total.trim().parse::<u32>().ok()
        })
        .max()
}

#[cfg(test)]
mod tests {
    use super::parse_hviske_cuda_total_mib;

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
}
