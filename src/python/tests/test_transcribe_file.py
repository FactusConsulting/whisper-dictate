from helpers import (
    _capture_stdout,
    json,
    os,
    patch,
    Path,
    real_numpy,
    sys,
    tempfile,
    types,
    unittest,
    wave,
)

class TranscribeFileTests(unittest.TestCase):
    def _write_test_wav(self, path, *, rate=16000, seconds=0.8):
        import math
        import struct

        frames = int(rate * seconds)
        pcm = b"".join(
            struct.pack("<h", int(0.25 * 32767 * math.sin(2 * math.pi * 440 * i / rate)))
            for i in range(frames)
        )
        with wave.open(path, "wb") as wav:
            wav.setnchannels(1)
            wav.setsampwidth(2)
            wav.setframerate(rate)
            wav.writeframes(pcm)

    def test_parser_accepts_transcribe_file(self):
        sys.modules.pop("vp_cli", None)
        from whisper_dictate import vp_cli

        args = vp_cli.build_arg_parser().parse_args(
            ["--transcribe-file", "sample.wav"])
        self.assertEqual(args.transcribe_file, "sample.wav")

    def test_load_audio_file_decodes_wav_as_16khz_int16_mono(self):
        sys.modules["numpy"] = real_numpy()
        sys.modules.pop("whisper_dictate.runtime", None)
        from whisper_dictate import runtime

        with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as f:
            path = f.name
        try:
            self._write_test_wav(path, rate=8000)
            pcm = runtime.load_audio_file(path)
        finally:
            os.remove(path)

        self.assertEqual(pcm.dtype.name, "int16")
        self.assertEqual(pcm.ndim, 2)
        self.assertEqual(pcm.shape[1], 1)
        self.assertGreaterEqual(len(pcm), 12000)

    def test_transcribe_file_event_uses_dictionary_replacements(self):
        sys.modules["numpy"] = real_numpy()
        for name in ("vp_audio", "vp_transcribe", "whisper_dictate.runtime"):
            sys.modules.pop(name, None)
        from whisper_dictate import runtime
        from whisper_dictate import vp_transcribe

        class Segment:
            text = " lead death"
            start = 0.0
            end = 0.8

        class Info:
            language = "en"
            language_probability = 0.9

        class Model:
            def transcribe(self, *_args, **_kwargs):
                return [Segment()], Info()

        def dictionary_runtime(text="", base_prompt=None):
            if text:
                return vp_transcribe.DictionaryRuntimeResult(
                    text=text.replace("lead death", "lead dev"),
                    prompt=base_prompt,
                    terms=["lead dev"],
                    changes=[{"from": "lead death", "to": "lead dev", "count": 1}],
                    term_count=1,
                    replacement_count=1,
                )
            return vp_transcribe.DictionaryRuntimeResult(
                text=text,
                prompt=base_prompt,
                terms=["lead dev"],
                term_count=1,
                replacement_count=1,
            )

        old_dictionary_runtime = vp_transcribe._dictionary_runtime
        old_gate = vp_transcribe._looks_like_speech
        vp_transcribe._dictionary_runtime = dictionary_runtime
        vp_transcribe._looks_like_speech = lambda _audio: (True, "test gate")
        with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as f:
            path = f.name
        try:
            self._write_test_wav(path)
            event = runtime.transcribe_file_event(
                Model(), path, "en",
                model_name="fake", stt_backend="whisper",
                device="cpu", compute_type="int8",
            )
        finally:
            vp_transcribe._dictionary_runtime = old_dictionary_runtime
            vp_transcribe._looks_like_speech = old_gate
            os.remove(path)

        self.assertEqual(event["event"], "file_transcription")
        self.assertEqual(event["text"], "lead dev")
        self.assertEqual(event["raw_text"], "lead death")
        self.assertEqual(event["source_file"], path)
        self.assertEqual(event["dictionary_terms"], ["lead dev"])
        self.assertEqual(event["dictionary_replacements"][0]["from"], "lead death")

    def test_transcribe_file_json_output_is_single_json_object(self):
        from whisper_dictate import runtime

        event = {"event": "file_transcription", "text": "hello"}
        with _capture_stdout() as buf:
            runtime.print_transcribe_file_result(event, as_json=True)

        self.assertEqual(json.loads(buf.getvalue()), event)

