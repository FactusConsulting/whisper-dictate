"""Tests for ``vp_provenance`` -- the engine / STT-implementation
provenance vocabulary and resolution.

WHAT THIS GUARDS
----------------
An utterance record used to carry only the CONFIGURED stack::

    {"compute_type":"int8_float16","real_time_factor":0.23,"compute_ms":351,
     "model":"large-v3-turbo","stt_backend":"whisper","device":"auto"}

Both engines emit every one of those fields, and ``stt_backend`` names the
SELECTED backend, so the row could not say whether Rust whisper.cpp on a
Vulkan GPU or the Python worker's faster-whisper on CUDA produced it --
nor whether either had silently fallen back to CPU.

The labels are also a cross-language wire contract: the same strings are
produced by ``src/rust/dictate/provenance.rs`` and asserted in
``src/rust/dictate/provenance_tests.rs``. A rename on one side must fail a
test rather than quietly forking the JSONL schema.
"""
from helpers import (
    types,
    unittest,
)

from whisper_dictate import vp_provenance
from whisper_dictate.vp_provenance import (
    ACCEL_UNKNOWN,
    ENGINE_PYTHON_WORKER,
    ENGINE_RUST_IN_PROCESS,
    STT_IMPL_CLOUD_GROQ,
    STT_IMPL_CLOUD_OPENAI,
    STT_IMPL_FASTER_WHISPER,
    STT_IMPL_UNKNOWN,
    STT_IMPL_WHISPER_CPP,
    cloud_stt_impl_for_base_url,
    describe_stt_stack,
    faster_whisper_accel,
    normalize_accel,
    startup_line,
)


def _fake_faster_whisper(device: str):
    """Duck-typed stand-in for ``faster_whisper.WhisperModel``.

    ``.model`` is the CTranslate2 model; its ``.device`` is the device the
    weights were REALLY loaded onto (never ``auto``).
    """
    return types.SimpleNamespace(model=types.SimpleNamespace(device=device))


class ProvenanceVocabularyTests(unittest.TestCase):
    def test_engine_labels_match_the_rust_side(self):
        self.assertEqual(ENGINE_PYTHON_WORKER, "python-worker")
        self.assertEqual(ENGINE_RUST_IN_PROCESS, "rust-in-process")

    def test_stt_impl_labels_match_the_rust_side(self):
        self.assertEqual(STT_IMPL_WHISPER_CPP, "whisper.cpp")
        self.assertEqual(STT_IMPL_FASTER_WHISPER, "faster-whisper")
        self.assertEqual(STT_IMPL_CLOUD_OPENAI, "cloud-openai")
        self.assertEqual(STT_IMPL_CLOUD_GROQ, "cloud-groq")

    def test_every_label_is_ascii_and_space_free(self):
        # They reach the console via the startup line and land in JSONL
        # rows; a space would also break `key=value` parsing of the line.
        for label in (
            ENGINE_PYTHON_WORKER,
            ENGINE_RUST_IN_PROCESS,
            STT_IMPL_WHISPER_CPP,
            STT_IMPL_FASTER_WHISPER,
            STT_IMPL_CLOUD_OPENAI,
            STT_IMPL_CLOUD_GROQ,
            STT_IMPL_UNKNOWN,
        ):
            self.assertTrue(label.isascii(), label)
            self.assertNotIn(" ", label)


class NormalizeAccelTests(unittest.TestCase):
    def test_known_accelerators_pass_through_lowercased(self):
        self.assertEqual(normalize_accel("CUDA"), "cuda")
        self.assertEqual(normalize_accel(" cpu "), "cpu")
        self.assertEqual(normalize_accel("vulkan"), "vulkan")

    def test_auto_is_not_an_outcome_and_becomes_unknown(self):
        # `auto` is the REQUEST. Letting it through would recreate the
        # exact ambiguity this field exists to remove.
        self.assertEqual(normalize_accel("auto"), ACCEL_UNKNOWN)

    def test_missing_or_unrecognised_values_become_unknown(self):
        for raw in (None, "", "   ", "rocm", "metal", 0):
            self.assertEqual(normalize_accel(raw), ACCEL_UNKNOWN, raw)


class CloudProviderTests(unittest.TestCase):
    def test_groq_base_url_resolves_to_the_groq_impl(self):
        # `stt_backend` is `openai` for Groq too -- the base URL is the
        # only signal separating the two services.
        self.assertEqual(
            cloud_stt_impl_for_base_url("https://api.groq.com/openai/v1"),
            STT_IMPL_CLOUD_GROQ,
        )
        self.assertEqual(
            cloud_stt_impl_for_base_url("HTTPS://API.GROQ.COM/openai/v1"),
            STT_IMPL_CLOUD_GROQ,
        )

    def test_classification_is_by_host_not_substring(self):
        # Codex P2 #687: a substring test mislabels both directions, and
        # either way the record names a service that never saw the audio.
        for url in (
            "https://groq.com.attacker.example/v1",
            "https://example.test/proxy/groq.com",
            "https://api.groq.com@custom.example/v1",
        ):
            self.assertEqual(
                cloud_stt_impl_for_base_url(url), STT_IMPL_CLOUD_OPENAI, url,
            )
        # A trailing DNS root dot and an explicit port are the same host.
        for url in (
            "https://api.groq.com./openai/v1",
            "https://api.groq.com:443/openai/v1",
            "https://groq.com/openai/v1",
        ):
            self.assertEqual(
                cloud_stt_impl_for_base_url(url), STT_IMPL_CLOUD_GROQ, url,
            )

    def test_openai_and_blank_base_urls_resolve_to_openai(self):
        self.assertEqual(
            cloud_stt_impl_for_base_url("https://api.openai.com/v1"),
            STT_IMPL_CLOUD_OPENAI,
        )
        self.assertEqual(cloud_stt_impl_for_base_url(""), STT_IMPL_CLOUD_OPENAI)


class DescribeSttStackTests(unittest.TestCase):
    def test_faster_whisper_reports_the_device_ctranslate2_really_used(self):
        # THE headline case: `VOICEPI_DEVICE=auto` resolves to cuda or cpu
        # at load time and the setting never records which. The CTranslate2
        # model does.
        self.assertEqual(
            describe_stt_stack(_fake_faster_whisper("cuda")),
            (STT_IMPL_FASTER_WHISPER, "cuda"),
        )
        self.assertEqual(
            describe_stt_stack(_fake_faster_whisper("cpu")),
            (STT_IMPL_FASTER_WHISPER, "cpu"),
        )

    def test_faster_whisper_accel_helper_reads_the_inner_model(self):
        self.assertEqual(faster_whisper_accel(_fake_faster_whisper("cuda")), "cuda")
        self.assertEqual(faster_whisper_accel(object()), ACCEL_UNKNOWN)

    def test_a_reporting_wrapper_wins_over_the_faster_whisper_probe(self):
        # The Rust whisper.cpp helper and the cloud adapter know their own
        # provenance; ask them rather than guessing from shape.
        model = types.SimpleNamespace(
            stt_provenance=lambda: (STT_IMPL_WHISPER_CPP, "vulkan"),
            model=types.SimpleNamespace(device="cuda"),
        )
        self.assertEqual(
            describe_stt_stack(model), (STT_IMPL_WHISPER_CPP, "vulkan")
        )

    def test_reported_accelerator_is_normalised(self):
        model = types.SimpleNamespace(
            stt_provenance=lambda: (STT_IMPL_WHISPER_CPP, "AUTO"),
        )
        self.assertEqual(
            describe_stt_stack(model), (STT_IMPL_WHISPER_CPP, ACCEL_UNKNOWN)
        )

    def test_unknown_model_shape_reports_unknown_rather_than_guessing(self):
        self.assertEqual(
            describe_stt_stack(object()), (STT_IMPL_UNKNOWN, ACCEL_UNKNOWN)
        )

    def test_a_raising_reporter_degrades_instead_of_losing_the_utterance(self):
        # This runs on the per-utterance event path; a diagnostics probe
        # must never be able to drop a user's dictation.
        def boom():
            raise RuntimeError("helper is wedged")

        model = types.SimpleNamespace(stt_provenance=boom)
        self.assertEqual(
            describe_stt_stack(model), (STT_IMPL_UNKNOWN, ACCEL_UNKNOWN)
        )


class StartupLineTests(unittest.TestCase):
    def test_matches_the_documented_shape(self):
        self.assertEqual(
            startup_line(
                ENGINE_PYTHON_WORKER, STT_IMPL_FASTER_WHISPER, "cuda",
                "large-v3-turbo",
            ),
            "[runtime] transcribe backend resolved: engine=python-worker "
            "impl=faster-whisper accel=cuda model=large-v3-turbo",
        )

    def test_blank_model_is_omitted_rather_than_emitted_empty(self):
        line = startup_line(
            ENGINE_PYTHON_WORKER, STT_IMPL_CLOUD_GROQ, ACCEL_UNKNOWN, "  ",
        )
        self.assertNotIn("model=", line)
        self.assertTrue(line.endswith("accel=unknown"), line)

    def test_line_is_ascii(self):
        line = startup_line(
            ENGINE_PYTHON_WORKER, STT_IMPL_WHISPER_CPP, "cpu", "large-v3-turbo",
        )
        self.assertTrue(line.isascii(), line)

    def test_module_exposes_the_startup_line_helper(self):
        # Referenced by name so the regression-test discipline scanner can
        # see the new public symbol exercised.
        self.assertTrue(callable(vp_provenance.startup_line))


if __name__ == "__main__":
    unittest.main()
