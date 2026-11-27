import contextlib
from http.client import HTTPConnection
import io
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
from tempfile import TemporaryDirectory
import threading
from types import SimpleNamespace
import unittest
from unittest import mock
import wave

import policy
import service


PCM = b"\x00\x00\x00\x80\xff\x7f"


def make_wav(pcm=PCM, *, rate=service.SAMPLE_RATE, channels=1, width=2):
    result = io.BytesIO()
    with wave.open(result, "wb") as audio:
        audio.setparams((channels, width, rate, 0, "NONE", "not compressed"))
        audio.writeframes(pcm)
    return result.getvalue()


class WavTests(unittest.TestCase):
    def test_pcm_round_trip(self):
        self.assertEqual(service.decode_wav(make_wav()), PCM)

    def test_invalid_audio(self):
        bad = (
            make_wav(rate=8000), make_wav(channels=2), make_wav(width=1),
            make_wav(b""), make_wav()[:-2], make_wav(b"\x00\x00\x00"),
            b"RIFF" + b"\x00" * 60, b"not a WAV file",
        )
        for body in bad:
            with self.subTest(body=body), self.assertRaises(ValueError):
                service.decode_wav(body)

    def test_audio_duration_limit_is_independent_of_riff_overhead(self):
        body = make_wav(b"\x00\x00" * 4)
        with mock.patch.object(service, "MAX_SAMPLES", 3), self.assertRaisesRegex(ValueError, "seconds"):
            service.decode_wav(body)


class SnapshotTests(unittest.TestCase):
    def test_online_and_offline_use_the_same_pinned_local_directory(self):
        checkpoint = policy.CHECKPOINTS[policy.V6]

        def write_files(*, local_dir, **_):
            local_dir.mkdir(parents=True)
            for name in checkpoint.files:
                (local_dir / name).touch()

        download = mock.Mock(side_effect=write_files)
        with TemporaryDirectory() as home:
            expected = Path(home) / "whisper-dictate" / "hviske" / policy.V6 / checkpoint.revision
            self.assertEqual(policy.cached_snapshot(policy.V6, False, home, download), expected)
            download.assert_called_once_with(
                repo_id=policy.V6, revision=checkpoint.revision, allow_patterns=checkpoint.files,
                token=False, endpoint="https://huggingface.co", local_dir=expected,
            )
            download.reset_mock()
            self.assertEqual(policy.cached_snapshot(policy.V6, True, home, download), expected)
            (expected / "model.safetensors").unlink()
            with self.assertRaisesRegex(RuntimeError, "missing files: model.safetensors"):
                policy.cached_snapshot(policy.V6, True, home, download)
            download.assert_not_called()


class CudaGuardTests(unittest.TestCase):
    def setUp(self):
        self.cuda = mock.Mock()
        self.cuda.is_available.return_value = True
        self.cuda.device_count.return_value = 2
        self.cuda.device.side_effect = lambda _: contextlib.nullcontext()
        self.cuda.get_device_capability.return_value = (8, 0)
        self.cuda.is_bf16_supported.return_value = True
        self.cuda.mem_get_info.return_value = (6144 * policy.MIB, 8192 * policy.MIB)
        self.torch = SimpleNamespace(
            version=SimpleNamespace(cuda="12.8"), cuda=self.cuda,
            device=lambda name: name, bfloat16=object(), ones=mock.MagicMock(),
        )

    def test_cpu_and_rocm_builds_never_query_devices(self):
        self.torch.version.cuda = None
        with self.assertRaisesRegex(RuntimeError, "CUDA is unavailable"):
            policy.select_cuda_device(self.torch, 6144)
        self.cuda.device_count.assert_not_called()
        self.torch.ones.assert_not_called()

    def test_no_cuda_driver(self):
        self.cuda.is_available.return_value = False
        with self.assertRaisesRegex(RuntimeError, "CUDA is unavailable"):
            policy.select_cuda_device(self.torch, 6144)
        self.cuda.device_count.assert_not_called()

    def test_requires_capability_and_native_bf16(self):
        self.cuda.get_device_capability.side_effect = [(7, 5), (8, 9)]
        self.cuda.is_bf16_supported.return_value = False
        with self.assertRaisesRegex(RuntimeError, "native BF16"):
            policy.select_cuda_device(self.torch, 6144)
        self.cuda.is_bf16_supported.assert_called_once_with(including_emulation=False)
        self.torch.ones.assert_not_called()

    def test_selects_first_device_that_really_fits(self):
        self.cuda.mem_get_info.side_effect = [
            (6144 * policy.MIB - 1, 8192 * policy.MIB),
            (6144 * policy.MIB, 8192 * policy.MIB),
        ]
        self.assertEqual(policy.select_cuda_device(self.torch, 6144), "cuda:1")
        self.cuda.synchronize.assert_has_calls([mock.call("cuda:0"), mock.call("cuda:1")])

    def test_explicit_index_never_falls_back(self):
        self.cuda.mem_get_info.return_value = (3072 * policy.MIB, 6144 * policy.MIB)
        with self.assertRaisesRegex(RuntimeError, "cuda:1: requires 3584 MiB total and free"):
            policy.select_cuda_device(self.torch, 3584, requested_index=1)
        self.cuda.mem_get_info.assert_called_once_with(1)
        with self.assertRaisesRegex(RuntimeError, "not a visible CUDA device"):
            policy.select_cuda_device(self.torch, 3584, requested_index=2)

    def test_matmul_failure_does_not_pass_the_guard(self):
        self.torch.ones.return_value.__matmul__.side_effect = RuntimeError("kernel unavailable")
        with self.assertRaisesRegex(RuntimeError, "kernel unavailable"):
            policy.select_cuda_device(self.torch, 3584, requested_index=0)
        self.cuda.mem_get_info.assert_not_called()

    def test_parameters_and_buffers_must_reside_on_the_selected_device(self):
        device = "cuda:1"
        parameter = SimpleNamespace(device=device)
        buffer = SimpleNamespace(device=device)
        module = SimpleNamespace(parameters=lambda: [parameter], buffers=lambda: [buffer])
        policy.require_cuda_module(module, device)
        for tensor in (parameter, buffer):
            with self.subTest(tensor=tensor):
                tensor.device = "cpu"
                with self.assertRaisesRegex(RuntimeError, "not entirely on cuda:1"):
                    policy.require_cuda_module(module, device)
                tensor.device = device


class StartupTests(unittest.TestCase):
    def test_service_refuses_to_bind_nonloopback_addresses(self):
        server = object.__new__(service.HviskeServer)
        service.HTTPServer.__init__(
            server, ("0.0.0.0", 0), service.HviskeHandler, bind_and_activate=False,
        )
        with server, self.assertRaisesRegex(ValueError, "IPv4 loopback"):
            server.server_bind()

    def test_service_script_help_needs_no_optional_dependencies(self):
        result = subprocess.run(
            [sys.executable, "-S", str(Path(service.__file__).resolve()), "--help"],
            capture_output=True, text=True, timeout=10, check=True,
        )
        self.assertIn(policy.V5, result.stdout)
        self.assertIn(policy.V6, result.stdout)
        self.assertIn("--accept-noncommercial", result.stdout)

    def test_noncommercial_acceptance_required_before_loading(self):
        with mock.patch.object(sys, "argv", ["service.py", "--model", policy.V6]), \
                mock.patch.object(service, "load_transcriber") as load, \
                contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
            service.main()
        self.assertEqual(error.exception.code, 2)
        load.assert_not_called()

    def test_offline_startup_configures_hub_before_loading_and_reports_failure(self):
        args = ["service.py", "--model", policy.V6, "--accept-noncommercial", "--offline", "--cuda-device", "1"]

        def fail_loading(model_id, offline, device):
            self.assertEqual((model_id, offline, device), (policy.V6, True, 1))
            self.assertEqual(os.environ.get("HF_HUB_OFFLINE"), "1")
            self.assertEqual(os.environ.get("HF_HUB_DISABLE_TELEMETRY"), "1")
            raise RuntimeError("CUDA unavailable\nstartup details")

        with mock.patch.object(sys, "argv", args), mock.patch.dict(os.environ, clear=True), \
                mock.patch.object(service, "load_transcriber", side_effect=fail_loading), \
                contextlib.redirect_stderr(io.StringIO()) as stderr:
            self.assertEqual(service.main(), 1)
        self.assertEqual(stderr.getvalue().splitlines(), [
            "Hviske startup failed (RuntimeError): 'CUDA unavailable\\nstartup details'",
        ])


class HttpProtocolTests(unittest.TestCase):
    def setUp(self):
        self.transcribe = mock.Mock(return_value="Rødgrød med fløde.")
        server = service.HviskeServer(policy.V6, self.transcribe, port=0)
        self.addCleanup(server.server_close)
        self.assertEqual(server.server_address[0], "127.0.0.1")
        thread = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.01}, daemon=True)
        thread.start()
        self.addCleanup(thread.join, 2)
        self.addCleanup(server.shutdown)
        self.conn = HTTPConnection(*server.server_address, timeout=2)
        self.addCleanup(self.conn.close)
        self.headers = {service.MODEL_HEADER: policy.V6, "Content-Type": "audio/wav"}

    def response(self):
        with self.conn.getresponse() as response:
            self.assertEqual(response.getheader("Content-Type"), "application/json")
            return response.status, json.load(response)

    def test_health_and_transcription_use_canonical_cuda_identity(self):
        self.conn.request("GET", service.HEALTH_PATH)
        identity = {"model": policy.V6, "device": "cuda"}
        self.assertEqual(self.response(), (200, identity))
        self.transcribe.assert_not_called()

        self.conn.request("POST", service.TRANSCRIBE_PATH, body=make_wav(), headers=self.headers)
        self.assertEqual(self.response(), (200, {**identity, "text": self.transcribe.return_value}))
        self.transcribe.assert_called_once_with(PCM)

    def test_wrong_models_are_rejected_without_waiting_for_body(self):
        for model_id in (policy.V5, f"local:{policy.V6}", ""):
            with self.subTest(model_id=model_id):
                headers = {**self.headers, service.MODEL_HEADER: model_id, "Content-Length": "128"}
                self.conn.request("POST", service.TRANSCRIBE_PATH, headers=headers)
                self.assertEqual(self.response()[0], 409)
        self.transcribe.assert_not_called()

    def test_oversize_is_rejected_without_waiting_for_body(self):
        headers = {**self.headers, "Content-Length": str(service.MAX_WAV_BYTES + 1)}
        self.conn.request("POST", service.TRANSCRIBE_PATH, headers=headers)
        self.assertEqual(self.response()[0], 413)
        self.transcribe.assert_not_called()

    def test_short_body_is_rejected_without_waiting_for_audio(self):
        headers = {**self.headers, "Content-Length": "43"}
        self.conn.request("POST", service.TRANSCRIBE_PATH, headers=headers)
        self.assertEqual(self.response(), (400, {"error": "Invalid WAV length"}))
        self.transcribe.assert_not_called()

    def test_invalid_framing_and_media_type_do_not_reach_inference(self):
        body = make_wav()
        for headers, status in (
            ({"Content-Type": "application/octet-stream"}, 415),
            ({"Content-Encoding": "gzip"}, 415),
            ({"Transfer-Encoding": "chunked"}, 400),
            ({"Content-Length": "-1"}, 400),
        ):
            with self.subTest(headers=headers):
                self.conn.request("POST", service.TRANSCRIBE_PATH, body=body,
                                  headers={**self.headers, **headers})
                self.assertEqual(self.response()[0], status)
        self.transcribe.assert_not_called()

    def test_truncated_body_does_not_reach_inference(self):
        body = make_wav()
        headers = {**self.headers, "Content-Length": str(len(body))}
        self.conn.request("POST", service.TRANSCRIBE_PATH, body=body[:-2], headers=headers)
        self.conn.sock.shutdown(socket.SHUT_WR)
        self.assertEqual(self.response()[0], 400)
        self.transcribe.assert_not_called()

    def test_nonloopback_host_is_rejected(self):
        self.conn.request("GET", service.HEALTH_PATH, headers={"Host": "example.invalid"})
        self.assertEqual(self.response()[0], 400)
        self.transcribe.assert_not_called()

    def test_unknown_routes_do_not_reach_inference(self):
        for verb, path in (("GET", service.TRANSCRIBE_PATH), ("POST", service.HEALTH_PATH)):
            with self.subTest(verb=verb):
                self.conn.request(verb, path)
                self.assertEqual(self.response(), (404, {"error": "Unknown endpoint"}))
        self.transcribe.assert_not_called()

    def test_inference_error_returns_no_transcript_or_success_identity(self):
        self.transcribe.side_effect = RuntimeError("private transcript")
        with contextlib.redirect_stderr(io.StringIO()) as stderr:
            self.conn.request("POST", service.TRANSCRIBE_PATH, body=make_wav(), headers=self.headers)
            self.assertEqual(self.response(), (500, {"error": "CUDA transcription failed"}))
        self.assertNotIn("private transcript", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
