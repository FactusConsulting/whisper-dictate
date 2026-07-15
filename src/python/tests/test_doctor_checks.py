"""Unit tests for vp_doctor_checks (the cross-platform doctor readiness probes).

Each probe is exercised with its heavy/OS dependency stubbed, so the suite runs
without numpy/faster-whisper/a GPU/network.
"""
import sys
import urllib.error

from helpers import (
    _env,
    patch,
    Path,
    tempfile,
    types,
    unittest,
)

from whisper_dictate import vp_doctor_checks as dc


class _FakeResp:
    def __init__(self, status):
        self.status = status

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


class ConfiguredBackendTests(unittest.TestCase):
    def test_defaults_to_whisper(self):
        with _env(VOICEPI_STT_BACKEND=None):
            self.assertEqual(dc.configured_backend(), "whisper")

    def test_faster_whisper_alias_normalised(self):
        with _env(VOICEPI_STT_BACKEND="faster-whisper"):
            self.assertEqual(dc.configured_backend(), "whisper")

    def test_openai_passes_through(self):
        with _env(VOICEPI_STT_BACKEND="openai"):
            self.assertEqual(dc.configured_backend(), "openai")


class ConfigCheckTests(unittest.TestCase):
    def test_valid_json_passes(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "config.json"
            path.write_text('{"a": 1}', encoding="utf-8")
            with _env(VOICEPI_CONFIG=str(path)):
                check = dc.check_config()
        self.assertTrue(check.ok)
        self.assertIn("valid JSON", check.detail)

    def test_invalid_json_is_required_fail(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "config.json"
            path.write_text("{not json", encoding="utf-8")
            with _env(VOICEPI_CONFIG=str(path)):
                check = dc.check_config()
        self.assertFalse(check.ok)
        self.assertTrue(check.required)

    def test_absent_file_is_warn_not_fail(self):
        with _env(VOICEPI_CONFIG=str(Path(tempfile.gettempdir()) / "wd-nope.json")):
            check = dc.check_config()
        self.assertTrue(check.ok)
        self.assertFalse(check.required)


class RustHelperCheckTests(unittest.TestCase):
    def test_found(self):
        with patch("whisper_dictate.vp_rust._rust_helper", lambda: "whisper-dictate"):
            self.assertTrue(dc.check_rust_helper().ok)

    def test_missing_is_required_fail(self):
        with patch("whisper_dictate.vp_rust._rust_helper", lambda: ""):
            check = dc.check_rust_helper()
        self.assertFalse(check.ok)
        self.assertTrue(check.required)


class VersionCheckTests(unittest.TestCase):
    def test_known_version_ok(self):
        with patch("whisper_dictate.runtime.get_version", lambda: "1.7.0"):
            check = dc.check_version()
        self.assertTrue(check.ok)
        self.assertEqual(check.detail, "1.7.0")

    def test_unknown_version_is_warn(self):
        with patch("whisper_dictate.runtime.get_version", lambda: "unknown"):
            check = dc.check_version()
        self.assertFalse(check.ok)
        self.assertFalse(check.required)


class GpuCheckTests(unittest.TestCase):
    def test_cuda_present(self):
        fake = types.SimpleNamespace(get_cuda_device_count=lambda: 2)
        with patch.dict(sys.modules, {"ctranslate2": fake}), \
                patch.object(dc.shutil, "which", lambda name: None):
            cuda = next(c for c in dc.gpu_checks() if c.name == "cuda")
        self.assertTrue(cuda.ok)
        self.assertIn("2 CUDA", cuda.detail)

    def test_cpu_only_is_warn_never_fail(self):
        fake = types.SimpleNamespace(get_cuda_device_count=lambda: 0)
        with patch.dict(sys.modules, {"ctranslate2": fake}), \
                patch.object(dc.shutil, "which", lambda name: None):
            cuda = next(c for c in dc.gpu_checks() if c.name == "cuda")
        self.assertFalse(cuda.ok)
        self.assertFalse(cuda.required)


class CloudReachableTests(unittest.TestCase):
    def test_http_200_is_ok(self):
        with patch("urllib.request.urlopen", lambda *a, **k: _FakeResp(200)):
            check = dc._cloud_reachable("https://api.openai.com/v1", "key")
        self.assertTrue(check.ok)

    def test_http_403_is_warn(self):
        err = urllib.error.HTTPError("u", 403, "Forbidden", None, None)

        def raise_http(*a, **k):
            raise err

        with patch("urllib.request.urlopen", raise_http):
            check = dc._cloud_reachable("https://api.groq.com/openai/v1", "key")
        self.assertFalse(check.ok)
        self.assertFalse(check.required)
        self.assertIn("403", check.detail)

    def test_offline_is_warn(self):
        def raise_url(*a, **k):
            raise urllib.error.URLError("offline")

        with patch("urllib.request.urlopen", raise_url):
            check = dc._cloud_reachable("https://api.openai.com/v1", "key")
        self.assertFalse(check.ok)
        self.assertIn("unreachable", check.detail)


class BackendDispatchTests(unittest.TestCase):
    def test_whisper_probes_local_deps_and_gpu(self):
        with patch.object(dc.shutil, "which", lambda name: None):
            names = {c.name for c in dc.backend_checks("whisper", ping=False)}
        self.assertIn("faster_whisper", names)
        self.assertIn("ctranslate2", names)
        self.assertIn("cuda", names)
        self.assertIn("model cache", names)

    def test_openai_probes_cloud_not_local(self):
        with _env(VOICEPI_STT_API_KEY="k", VOICEPI_STT_BASE_URL="https://api.openai.com/v1"):
            checks = dc.backend_checks("openai", ping=False)
        names = {c.name for c in checks}
        self.assertIn("cloud api key", names)
        self.assertNotIn("faster_whisper", names)
        # ping=False must not perform a network request.
        self.assertNotIn("cloud reachable", names)

    def test_openai_missing_key_is_required_fail(self):
        with _env(
            VOICEPI_STT_API_KEY=None,
            OPENAI_API_KEY=None,
            GROQ_API_KEY=None,
        ):
            key = next(
                c for c in dc.backend_checks("openai", ping=False) if c.name == "cloud api key"
            )
        self.assertFalse(key.ok)
        self.assertTrue(key.required)

    def test_unknown_backend_probes_nothing(self):
        # An unknown backend must not fall through to whisper probes; the
        # invalid value is surfaced by check_backend instead.
        self.assertEqual(dc.backend_checks("bogus", ping=False), [])


class CheckBackendTests(unittest.TestCase):
    def test_valid_backend_is_ok(self):
        with _env(VOICEPI_STT_BACKEND="whisper"):
            self.assertTrue(dc.check_backend().ok)

    def test_invalid_backend_is_required_fail(self):
        with _env(VOICEPI_STT_BACKEND="bogus"):
            check = dc.check_backend()
        self.assertFalse(check.ok)
        self.assertTrue(check.required)


if __name__ == "__main__":
    unittest.main()
