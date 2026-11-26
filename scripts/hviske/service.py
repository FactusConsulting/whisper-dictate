"""Opt-in loopback service for the two pinned Danish Hviske CUDA models."""

import argparse
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, HTTPServer
import io
import json
import os
import socket
import struct
import sys
import wave

from cuda_model import load_transcriber
from policy import CHECKPOINTS, SAMPLE_RATE


BIND_ADDRESS = ("127.0.0.1", 18765)
HEALTH_PATH = "/wd-hviske/v1/health"
TRANSCRIBE_PATH = "/wd-hviske/v1/transcribe"
MODEL_HEADER = "X-WD-Hviske-Model"
MAX_AUDIO_SECONDS = 300
MAX_SAMPLES = SAMPLE_RATE * MAX_AUDIO_SECONDS
MAX_WAV_BYTES = MAX_SAMPLES * 2 + 4096


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
