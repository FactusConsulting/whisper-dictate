"""Pinned checkpoints, snapshot cache, and CUDA placement policy."""

from dataclasses import dataclass
from itertools import chain
from pathlib import Path


SAMPLE_RATE = 16_000
MIB = 1024 * 1024


@dataclass(frozen=True)
class Checkpoint:
    revision: str
    min_vram_mib: int
    files: tuple[str, ...]


V5 = "syvai/hviske-v5.3"
V6 = "syvai/hviske-v6"
COMMON_FILES = (
    "config.json", "model.safetensors", "special_tokens_map.json", "tokenizer.json",
    "tokenizer_config.json",
)
CHECKPOINTS = {
    V5: Checkpoint(
        "5d1a09822018702dc51d763e3a867b62d26b3501",
        6144,
        COMMON_FILES + (
            "configuration_cohere_asr.py", "generation_config.json", "modeling_cohere_asr.py",
            "preprocessor_config.json", "processing_cohere_asr.py", "tokenization_cohere_asr.py",
            "tokenizer.model",
        ),
    ),
    V6: Checkpoint(
        "323b5ffdf54ef22196340a0b37cb11e668fefc5a",
        3584,
        COMMON_FILES + (
            "added_tokens.json", "configuration_whisper_qwen.py", "merges.txt",
            "modeling_whisper_qwen.py", "processing_whisper_qwen.py", "vocab.json",
        ),
    ),
}


def require_vram(cuda, index, min_vram_mib):
    free, total = cuda.mem_get_info(index)
    if min(free, total) < min_vram_mib * MIB:
        raise RuntimeError(
            f"requires {min_vram_mib} MiB total and free VRAM; "
            f"found {total // MIB} MiB total and {free // MIB} MiB free"
        )


def select_cuda_device(torch, min_vram_mib, requested_index=None):
    cuda = torch.cuda
    if torch.version.cuda is None or not cuda.is_available():
        raise RuntimeError("NVIDIA CUDA is unavailable; install a CUDA PyTorch wheel and driver")
    count = cuda.device_count()
    if requested_index is not None and not 0 <= requested_index < count:
        raise RuntimeError(f"--cuda-device {requested_index} is not a visible CUDA device")

    problems = []
    indices = range(count) if requested_index is None else (requested_index,)
    for index in indices:
        try:
            device = torch.device(f"cuda:{index}")
            with cuda.device(device):
                if (cuda.get_device_capability(index) < (8, 0)
                        or not cuda.is_bf16_supported(including_emulation=False)):
                    raise RuntimeError("requires compute capability 8.0+ with native BF16")
                # A real kernel also catches wheels that cannot run on this architecture.
                value = torch.ones((16, 16), dtype=torch.bfloat16, device=device)
                value @ value
                cuda.synchronize(device)
                del value
                require_vram(cuda, index, min_vram_mib)
            return device
        except RuntimeError as error:
            problems.append(f"cuda:{index}: {error}")
    raise RuntimeError("No suitable NVIDIA CUDA device: " + "; ".join(problems))


def require_cuda_module(module, device):
    if any(tensor.device != device for tensor in chain(module.parameters(), module.buffers())):
        raise RuntimeError(f"Model or audio frontend is not entirely on {device}")


def cached_snapshot(model_id, offline, hf_home, download_snapshot):
    checkpoint = CHECKPOINTS[model_id]
    # Hub local_dir uses ordinary files, so Windows does not need symlink privileges.
    snapshot = Path(hf_home) / "whisper-dictate" / "hviske" / model_id / checkpoint.revision
    if not offline:
        download_snapshot(
            repo_id=model_id, revision=checkpoint.revision, allow_patterns=checkpoint.files,
            token=False, endpoint="https://huggingface.co", local_dir=snapshot,
        )
    missing = [name for name in checkpoint.files if not (snapshot / name).is_file()]
    if missing:
        raise RuntimeError(f"Incomplete pinned snapshot at {snapshot}; missing files: {', '.join(missing)}")
    return snapshot
