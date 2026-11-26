"""Opt-in loopback service for the two pinned Danish Hviske CUDA models."""

import argparse
from dataclasses import dataclass
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, HTTPServer
import io
from itertools import chain
import json
import os
from pathlib import Path
import socket
import struct
import sys
import wave


BIND_ADDRESS = ("127.0.0.1", 18765)
HEALTH_PATH = "/wd-hviske/v1/health"
TRANSCRIBE_PATH = "/wd-hviske/v1/transcribe"
MODEL_HEADER = "X-WD-Hviske-Model"
SAMPLE_RATE = 16_000
MAX_AUDIO_SECONDS = 300
MAX_SAMPLES = SAMPLE_RATE * MAX_AUDIO_SECONDS
MAX_WAV_BYTES = MAX_SAMPLES * 2 + 4096
MIB = 1024 * 1024
TRANSFORMERS_VERSION = "4.57.6"


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


def decode_wav(body):
    if (not 44 <= len(body) <= MAX_WAV_BYTES or body[:4] != b"RIFF"
            or struct.unpack_from("<I", body, 4)[0] != len(body) - 8):
        raise ValueError("Invalid RIFF/WAV length")
    try:
        with wave.open(io.BytesIO(body), "rb") as audio:
            if (audio.getnchannels(), audio.getsampwidth(), audio.getframerate(),
                    audio.getcomptype()) != (1, 2, SAMPLE_RATE, "NONE"):
                raise ValueError("Expected mono 16 kHz signed 16-bit PCM WAV")
            frames = audio.getnframes()
            if not 0 < frames <= MAX_SAMPLES:
                raise ValueError(f"WAV must contain audio up to {MAX_AUDIO_SECONDS} seconds")
            pcm = audio.readframes(frames + 1)
            if len(pcm) != frames * 2:
                raise ValueError("Invalid PCM length")
            return pcm
    except (EOFError, wave.Error, struct.error) as error:
        raise ValueError("Invalid or unsupported WAV") from error


class HviskeHandler(BaseHTTPRequestHandler):
    def setup(self):
        super().setup()
        self.connection.settimeout(10)

    def log_message(self, format, *args):
        # Do not log request-controlled text, audio, or transcripts.
        pass

    def reply(self, status, **fields):
        data = json.dumps(fields).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def request_matches(self, path):
        if self.headers.get_all("Host") != [f"{BIND_ADDRESS[0]}:{self.server.server_port}"]:
            self.reply(HTTPStatus.BAD_REQUEST, error="Expected literal loopback Host")
            return False
        if self.path != path:
            self.reply(HTTPStatus.NOT_FOUND, error="Unknown endpoint")
            return False
        return True

    def do_GET(self):
        if self.request_matches(HEALTH_PATH):
            self.reply(HTTPStatus.OK, model=self.server.model_id, device="cuda")

    def do_POST(self):
        if not self.request_matches(TRANSCRIBE_PATH):
            return
        if self.headers.get_all(MODEL_HEADER) != [self.server.model_id]:
            self.reply(HTTPStatus.CONFLICT, error="Requested model is not loaded")
            return
        if self.headers.get_all("Content-Type") != ["audio/wav"] or "Content-Encoding" in self.headers:
            self.reply(HTTPStatus.UNSUPPORTED_MEDIA_TYPE, error="Expected raw audio/wav")
            return
        lengths = self.headers.get_all("Content-Length", [])
        if ("Transfer-Encoding" in self.headers or len(lengths) != 1
                or not lengths[0].isascii() or not lengths[0].isdecimal()):
            self.reply(HTTPStatus.BAD_REQUEST, error="Expected one decimal Content-Length")
            return
        length = lengths[0].lstrip("0") or "0"
        if len(length) > len(str(MAX_WAV_BYTES)) or int(length) > MAX_WAV_BYTES:
            self.reply(HTTPStatus.REQUEST_ENTITY_TOO_LARGE, error="WAV exceeds the request limit")
            return
        size = int(length)
        if size < 44:
            self.reply(HTTPStatus.BAD_REQUEST, error="Invalid WAV length")
            return
        try:
            body = self.rfile.read(size)
        except TimeoutError:
            self.reply(HTTPStatus.REQUEST_TIMEOUT, error="Timed out reading WAV")
            return
        try:
            if len(body) != size:
                raise ValueError("Incomplete WAV body")
            pcm = decode_wav(body)
        except ValueError as error:
            self.reply(HTTPStatus.BAD_REQUEST, error=str(error))
            return
        try:
            text = self.server.transcribe(pcm)
        except Exception as error:
            print(f"CUDA transcription failed ({type(error).__name__}).", file=sys.stderr, flush=True)
            self.reply(HTTPStatus.INTERNAL_SERVER_ERROR, error="CUDA transcription failed")
            return
        self.reply(HTTPStatus.OK, text=text, model=self.server.model_id, device="cuda")


class HviskeServer(HTTPServer):
    # One transcription at a time, so simultaneous requests cannot multiply VRAM usage.
    allow_reuse_address = not hasattr(socket, "SO_EXCLUSIVEADDRUSE")

    def __init__(self, model_id, transcribe, port=BIND_ADDRESS[1]):
        self.model_id = model_id
        self.transcribe = transcribe
        super().__init__((BIND_ADDRESS[0], port), HviskeHandler)

    def server_bind(self):
        if hasattr(socket, "SO_EXCLUSIVEADDRUSE"):
            self.socket.setsockopt(socket.SOL_SOCKET, socket.SO_EXCLUSIVEADDRUSE, 1)
        super().server_bind()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", choices=CHECKPOINTS, required=True)
    parser.add_argument("--accept-noncommercial", action="store_true", required=True,
                        help="accept the selected checkpoint's CC BY-NC 4.0 terms")
    parser.add_argument("--offline", action="store_true", help="use only the pinned cache")
    parser.add_argument("--cuda-device", type=int, metavar="N", help="visible CUDA index (default: first that fits)")
    args = parser.parse_args()
    if args.offline:
        os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["HF_HUB_DISABLE_TELEMETRY"] = "1"

    try:
        transcribe = load_transcriber(args.model, args.offline, args.cuda_device)
        with HviskeServer(args.model, transcribe) as server:
            host, port = BIND_ADDRESS
            print(f"{args.model} ready on CUDA at http://{host}:{port}{HEALTH_PATH}", flush=True)
            server.serve_forever()
    except KeyboardInterrupt:
        return 0
    except Exception as error:
        print(f"Hviske startup failed ({type(error).__name__}): {ascii(str(error))}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
