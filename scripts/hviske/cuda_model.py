"""Optional CUDA and model-library integration for the Hviske service."""

import json

from policy import (
    CHECKPOINTS, SAMPLE_RATE, V5, cached_snapshot, require_cuda_module, require_vram,
    select_cuda_device,
)


TRANSFORMERS_VERSION = "4.57.6"


def load_v5_processor(snapshot, device, tokenizer, load_class):
    from safetensors import safe_open

    extractor_class = load_class("processing_cohere_asr.CohereAsrFeatureExtractor")
    processor_class = load_class("processing_cohere_asr.CohereAsrProcessor")
    settings = json.loads((snapshot / "preprocessor_config.json").read_text(encoding="utf-8"))
    # AutoFeatureExtractor's from_dict ignores a device override absent from its JSON.
    settings["device"] = str(device)
    extractor = extractor_class.from_dict(settings)
    frontend = extractor.filterbank
    # Mirror the pinned extractor's optional checkpoint buffers without opening all weights.
    with safe_open(str(snapshot / "model.safetensors"), framework="pt", device=str(device)) as weights:
        keys = weights.keys()
        fb_key = "preprocessor.featurizer.fb"
        window_key = "preprocessor.featurizer.window"
        if fb_key in keys and window_key in keys:
            frontend.fb = weights.get_tensor(fb_key).to(dtype=frontend.fb.dtype)
            frontend.window = weights.get_tensor(window_key).to(dtype=frontend.window.dtype)
    require_cuda_module(frontend, device)
    return processor_class(feature_extractor=extractor, tokenizer=tokenizer)


def load_transcriber(model_id, offline, requested_index):
    import torch

    checkpoint = CHECKPOINTS[model_id]
    device = select_cuda_device(torch, checkpoint.min_vram_mib, requested_index)
    print(f"{device} fits {model_id} (native BF16); {'reading cache for' if offline else 'fetching'} "
          f"pinned revision {checkpoint.revision}.", flush=True)

    import numpy as np
    import transformers
    from huggingface_hub import snapshot_download
    from huggingface_hub.constants import HF_HOME
    from transformers import AutoModel, AutoModelForSpeechSeq2Seq, AutoTokenizer
    from transformers.dynamic_module_utils import get_class_from_dynamic_module

    if transformers.__version__ != TRANSFORMERS_VERSION:
        raise RuntimeError(f"Install transformers=={TRANSFORMERS_VERSION}")
    snapshot = cached_snapshot(model_id, offline, HF_HOME, snapshot_download)

    local = {"local_files_only": True}
    trusted_local = {**local, "trust_remote_code": True}

    def load_class(name):
        return get_class_from_dynamic_module(name, str(snapshot), **local)

    print(f"Loading {model_id} on {device} (no CPU or disk offload).", flush=True)
    require_vram(torch.cuda, device.index, checkpoint.min_vram_mib)
    loader = AutoModelForSpeechSeq2Seq if model_id == V5 else AutoModel
    model = loader.from_pretrained(
        str(snapshot), **trusted_local, dtype=torch.bfloat16,
        device_map={"": device}, use_safetensors=True,
    ).eval()
    require_cuda_module(model, device)
    if model.dtype != torch.bfloat16:
        raise RuntimeError("Model did not load in BF16")
    tokenizer = AutoTokenizer.from_pretrained(str(snapshot), **trusted_local)
    if model_id == V5:
        processor = load_v5_processor(snapshot, device, tokenizer, load_class)

        def transcribe_audio(audio):
            return model.transcribe(
                processor=processor, language="da", audio_arrays=[audio],
                sample_rates=[SAMPLE_RATE], batch_size=1, compile=False,
                pipeline_detokenization=False,
            )[0]
    else:
        # The upstream convenience loader does not pin the tokenizer and can choose CPU.
        asr_class = load_class("processing_whisper_qwen.HviskeASR")
        asr = asr_class(model, tokenizer, device=device, dtype=torch.bfloat16)
        require_cuda_module(asr.model, device)
        require_cuda_module(asr.mel, device)

        def transcribe_audio(audio):
            return asr.transcribe(audio, batch_size=1)

    torch.cuda.synchronize(device)

    def transcribe(pcm):
        audio = np.frombuffer(pcm, dtype=np.int16).astype(np.float32)
        audio *= 1.0 / 32768.0
        with torch.inference_mode():
            return transcribe_audio(audio)

    return transcribe
