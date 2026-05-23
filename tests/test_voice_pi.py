from __future__ import annotations

import importlib
import io
import os
import sys
import tempfile
import types
import unittest
from contextlib import redirect_stderr, contextmanager
from unittest.mock import patch


def load_voice_pi(cuda_devices: int = 0):
    for name in ("voice_pi", "vp_keymap", "vp_device", "vp_audio", "vp_inject",
                 "vp_cli", "vp_transcribe",
                 "ctranslate2", "faster_whisper", "numpy",
                 "sounddevice", "pynput", "pynput.keyboard"):
        sys.modules.pop(name, None)

    ctranslate2 = types.ModuleType("ctranslate2")
    ctranslate2.get_cuda_device_count = lambda: cuda_devices
    sys.modules["ctranslate2"] = ctranslate2

    faster_whisper = types.ModuleType("faster_whisper")
    faster_whisper.WhisperModel = object
    sys.modules["faster_whisper"] = faster_whisper

    sys.modules["numpy"] = types.ModuleType("numpy")
    sys.modules["sounddevice"] = types.ModuleType("sounddevice")

    pynput = types.ModuleType("pynput")
    keyboard = types.ModuleType("keyboard")
    keyboard.Controller = object
    keyboard.Key = types.SimpleNamespace(
        ctrl_l=object(), ctrl_r=object(),
        shift_l=object(), shift_r=object(),
        alt_l=object(), alt_r=object(),
        esc=object(),
    )
    keyboard.Listener = object
    pynput.keyboard = keyboard
    sys.modules["pynput"] = pynput
    sys.modules["pynput.keyboard"] = keyboard

    return importlib.import_module("voice_pi")


def load_voice_pi_realnp():
    """Import voice_pi with the REAL numpy (for audio-DSP tests) but the
    heavy/uninstalled deps stubbed. CI installs numpy (see tests workflow)."""
    for name in ("voice_pi", "vp_keymap", "vp_device", "vp_audio", "vp_inject",
                 "vp_cli", "vp_transcribe",
                 "ctranslate2", "faster_whisper",
                 "sounddevice", "pynput", "pynput.keyboard"):
        sys.modules.pop(name, None)
    np_mod = sys.modules.get("numpy")
    if np_mod is not None and not hasattr(np_mod, "ndarray"):
        # a fake numpy left by another test — drop it so the real one loads
        for n in [m for m in list(sys.modules)
                  if m == "numpy" or m.startswith("numpy.")]:
            sys.modules.pop(n, None)
    import numpy  # noqa: F401 — real numpy must import (CI pip-installs it)

    ct = types.ModuleType("ctranslate2")
    ct.get_cuda_device_count = lambda: 0
    sys.modules["ctranslate2"] = ct
    fw = types.ModuleType("faster_whisper")
    fw.WhisperModel = object
    sys.modules["faster_whisper"] = fw
    sys.modules["sounddevice"] = types.ModuleType("sounddevice")
    pynput = types.ModuleType("pynput")
    kb = types.ModuleType("keyboard")
    kb.Controller = object
    kb.Key = types.SimpleNamespace(
        ctrl_l=object(), ctrl_r=object(), shift_l=object(),
        shift_r=object(), alt_l=object(), alt_r=object(), esc=object())
    kb.Listener = object
    pynput.keyboard = kb
    sys.modules["pynput"] = pynput
    sys.modules["pynput.keyboard"] = kb
    return importlib.import_module("voice_pi")


@contextmanager
def _capture_stdout():
    import contextlib
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        yield buf


class AudioDspTests(unittest.TestCase):
    """Characterisation tests for the audio DSP with REAL numpy. These pin
    current behaviour so the upcoming vp_audio.py extraction is provably
    behaviour-preserving (same asserts, only the import path changes)."""

    @classmethod
    def setUpClass(cls):
        try:
            cls.vp = load_voice_pi_realnp()
        except ImportError as e:
            raise unittest.SkipTest(f"real numpy unavailable: {e}")
        import numpy as np
        cls.np = np

    # --- _noise_snr ---
    def test_noise_snr_too_few_frames(self):
        a = self.np.zeros(1000, dtype=self.np.float32)
        self.assertEqual(self.vp._noise_snr(a), (-90.0, 0.0))

    def test_noise_snr_constant_signal(self):
        a = self.np.full(480 * 8, 0.5, dtype=self.np.float32)
        noise, snr = self.vp._noise_snr(a)
        self.assertAlmostEqual(noise, -6.0206, places=2)
        self.assertAlmostEqual(snr, 0.0, places=6)

    def test_noise_snr_contrast_has_high_snr(self):
        np = self.np
        a = np.concatenate([
            np.full(480, 1.0 if i % 2 == 0 else 0.001, dtype=np.float32)
            for i in range(10)])
        noise, snr = self.vp._noise_snr(a)
        self.assertGreater(snr, 40.0)
        self.assertLess(noise, -40.0)

    # --- _boost_quiet ---
    def test_boost_quiet_normalises_toward_target(self):
        np = self.np
        a = np.full(1920, 0.01, dtype=np.float32)
        with _capture_stdout():
            out = self.vp._boost_quiet(a)
        self.assertEqual(out.dtype, np.float32)
        rms = float(np.sqrt(np.mean(out ** 2)))
        self.assertAlmostEqual(20 * np.log10(rms), self.vp.TARGET_DBFS,
                               places=1)

    def test_boost_quiet_never_clips(self):
        np = self.np
        a = np.zeros(1920, dtype=np.float32)
        a[:10] = 0.9
        with _capture_stdout():
            out = self.vp._boost_quiet(a)
        self.assertLessEqual(float(np.max(np.abs(out))), 0.99 + 1e-6)

    # --- _looks_like_speech ---
    def test_looks_like_speech_rejects_too_quiet(self):
        a = self.np.full(1920, 1e-4, dtype=self.np.float32)
        ok, msg = self.vp._looks_like_speech(a)
        self.assertFalse(ok)
        self.assertIn("too quiet", msg)

    def test_looks_like_speech_rejects_flat_signal(self):
        a = self.np.full(1920, 0.1, dtype=self.np.float32)
        ok, msg = self.vp._looks_like_speech(a)
        self.assertFalse(ok)
        self.assertIn("no speech contrast", msg)

    def test_looks_like_speech_accepts_contrasted_speech(self):
        np = self.np
        a = np.concatenate([
            np.full(480, 0.8 if i % 2 == 0 else 0.05, dtype=np.float32)
            for i in range(10)])
        ok, _ = self.vp._looks_like_speech(a)
        self.assertTrue(ok)


class DeviceResolutionTests(unittest.TestCase):
    def setUp(self):
        self._old_compute = os.environ.pop("VOICEPI_COMPUTE_TYPE", None)

    def tearDown(self):
        os.environ.pop("VOICEPI_COMPUTE_TYPE", None)
        if self._old_compute is not None:
            os.environ["VOICEPI_COMPUTE_TYPE"] = self._old_compute

    def test_auto_uses_cuda_when_available(self):
        voice_pi = load_voice_pi(cuda_devices=1)

        self.assertEqual(
            voice_pi._resolve_device("auto"),
            ("cuda", "int8_float16"),
        )

    def test_auto_falls_back_to_cpu_without_cuda(self):
        voice_pi = load_voice_pi(cuda_devices=0)

        self.assertEqual(voice_pi._resolve_device("auto"), ("cpu", "int8"))

    def test_explicit_cpu_and_cuda(self):
        voice_pi = load_voice_pi()

        self.assertEqual(voice_pi._resolve_device("cpu"), ("cpu", "int8"))
        self.assertEqual(
            voice_pi._resolve_device("cuda"),
            ("cuda", "int8_float16"),
        )

    def test_invalid_device_is_rejected(self):
        voice_pi = load_voice_pi()

        with self.assertRaises(ValueError):
            voice_pi._resolve_device("cdua")


class ComputeTypeOverrideTests(unittest.TestCase):
    """VOICEPI_COMPUTE_TYPE overrides the auto-picked compute_type for
    cuda / cpu / auto-on-gpu / auto-on-cpu — and an unset/empty env leaves
    the int8_float16-on-GPU / int8-on-CPU defaults untouched."""

    def setUp(self):
        self._old = os.environ.pop("VOICEPI_COMPUTE_TYPE", None)

    def tearDown(self):
        os.environ.pop("VOICEPI_COMPUTE_TYPE", None)
        if self._old is not None:
            os.environ["VOICEPI_COMPUTE_TYPE"] = self._old

    def test_override_applies_to_explicit_cuda(self):
        os.environ["VOICEPI_COMPUTE_TYPE"] = "float16"
        voice_pi = load_voice_pi(cuda_devices=1)
        self.assertEqual(
            voice_pi._resolve_device("cuda"), ("cuda", "float16"))

    def test_override_applies_to_explicit_cpu(self):
        os.environ["VOICEPI_COMPUTE_TYPE"] = "float32"
        voice_pi = load_voice_pi()
        self.assertEqual(
            voice_pi._resolve_device("cpu"), ("cpu", "float32"))

    def test_override_applies_to_auto_on_gpu(self):
        os.environ["VOICEPI_COMPUTE_TYPE"] = "bfloat16"
        voice_pi = load_voice_pi(cuda_devices=1)
        self.assertEqual(
            voice_pi._resolve_device("auto"), ("cuda", "bfloat16"))

    def test_override_applies_to_auto_on_cpu(self):
        os.environ["VOICEPI_COMPUTE_TYPE"] = "float32"
        voice_pi = load_voice_pi(cuda_devices=0)
        self.assertEqual(
            voice_pi._resolve_device("auto"), ("cpu", "float32"))

    def test_empty_env_leaves_defaults_untouched(self):
        os.environ["VOICEPI_COMPUTE_TYPE"] = "   "  # whitespace only
        voice_pi = load_voice_pi(cuda_devices=1)
        self.assertEqual(
            voice_pi._resolve_device("cuda"), ("cuda", "int8_float16"))
        self.assertEqual(
            voice_pi._resolve_device("cpu"), ("cpu", "int8"))

    def test_default_unchanged_when_env_unset(self):
        voice_pi = load_voice_pi(cuda_devices=1)
        self.assertEqual(
            voice_pi._resolve_device("cuda"), ("cuda", "int8_float16"))
        self.assertEqual(
            voice_pi._resolve_device("cpu"), ("cpu", "int8"))


class DebugConfigTests(unittest.TestCase):
    """VOICEPI_DEBUG triggers a startup dump of every effective setting
    + the env-var source annotation — so users can verify their setx
    actually arrived in the running process."""

    def setUp(self):
        # Cache + clear env we mutate so the dump is deterministic
        self._cached = {k: os.environ.pop(k, None) for k in (
            "VOICEPI_COMPUTE_TYPE", "VOICEPI_INITIAL_PROMPT",
            "VOICEPI_BEAM_SIZE", "VOICEPI_QUIT_COUNT",
            "VOICEPI_XKB_LAYOUT", "XKB_DEFAULT_LAYOUT",
            "VOICEPI_LANG", "VOICEPI_MODEL", "VOICEPI_DEVICE",
            "VOICEPI_KEY", "VOICEPI_INJECT_MODE",
        )}

    def tearDown(self):
        for k, v in self._cached.items():
            os.environ.pop(k, None)
            if v is not None:
                os.environ[k] = v

    def _args(self, **over):
        defaults = dict(key="ctrl_r", model="large-v3", lang="da",
                        autodetect=False, device="cuda", mode="type")
        defaults.update(over)
        return types.SimpleNamespace(**defaults)

    def test_dump_includes_all_expected_sections(self):
        os.environ["VOICEPI_COMPUTE_TYPE"] = "float16"
        os.environ["VOICEPI_INITIAL_PROMPT"] = "foo,bar,baz,qux"
        os.environ["VOICEPI_BEAM_SIZE"] = "8"
        voice_pi = load_voice_pi(cuda_devices=1)
        with _capture_stdout() as buf:
            voice_pi._print_effective_config(self._args(), "cuda", "float16")
        out = buf.getvalue()

        # header + every row label appears
        self.assertIn("[debug] effective settings:", out)
        for label in ("--key", "--model", "--lang", "--device",
                      "compute_type", "beam_size", "initial_prompt",
                      "quit", "audio thresholds", "XKB (Wayland)",
                      "inject mode"):
            self.assertIn(label, out)

        # env-sourced values are surfaced + annotated with the env var name
        self.assertIn("VOICEPI_COMPUTE_TYPE=float16", out)
        self.assertIn("VOICEPI_BEAM_SIZE=8", out)
        self.assertIn("VOICEPI_KEY=(unset)", out)
        self.assertIn("VOICEPI_INJECT_MODE=(unset)", out)
        self.assertIn("large-v3", out)
        self.assertIn("float16", out)
        # prompt is shown with its length + a preview substring
        self.assertIn("15 chars", out)
        self.assertIn("foo,bar,baz,qux", out)

    def test_long_prompt_is_truncated(self):
        os.environ["VOICEPI_INITIAL_PROMPT"] = "x" * 200
        voice_pi = load_voice_pi(cuda_devices=1)
        with _capture_stdout() as buf:
            voice_pi._print_effective_config(self._args(), "cuda", "float16")
        out = buf.getvalue()
        self.assertIn("200 chars", out)
        self.assertIn("...", out)  # truncated marker
        # full 200-char string is NOT in the output
        self.assertNotIn("x" * 200, out)

    def test_unset_env_shows_unset(self):
        voice_pi = load_voice_pi(cuda_devices=1)
        with _capture_stdout() as buf:
            voice_pi._print_effective_config(self._args(), "cuda", "int8_float16"),
        out = buf.getvalue()
        self.assertIn("VOICEPI_COMPUTE_TYPE=(unset)", out)
        self.assertIn("VOICEPI_INITIAL_PROMPT", out)  # row exists
        self.assertIn("(unset)", out)  # prompt shows (unset) too

    def test_autodetect_flag_overrides_lang_in_display(self):
        os.environ["VOICEPI_LANG"] = "da"
        voice_pi = load_voice_pi(cuda_devices=1)
        with _capture_stdout() as buf:
            voice_pi._print_effective_config(
                self._args(lang="da", autodetect=True), "cuda", "float16")
        out = buf.getvalue()
        # final resolved lang is 'auto' even though VOICEPI_LANG=da
        # because --autodetect was passed
        self.assertRegex(out, r"--lang\s+auto\b")
        self.assertIn("--autodetect=True", out)


class ArgumentParserTests(unittest.TestCase):
    def test_parser_rejects_invalid_device(self):
        voice_pi = load_voice_pi()
        parser = voice_pi.build_arg_parser()

        with redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit):
                parser.parse_args(["--device", "cdua"])

    def test_parser_accepts_supported_devices(self):
        voice_pi = load_voice_pi()
        parser = voice_pi.build_arg_parser()

        for device in voice_pi.VALID_DEVICES:
            with self.subTest(device=device):
                self.assertEqual(
                    parser.parse_args(["--device", device]).device,
                    device,
                )

    def test_parser_uses_key_env_default(self):
        with _env(VOICEPI_KEY="ctrl_l+space"):
            voice_pi = load_voice_pi()
            parser = voice_pi.build_arg_parser()

            self.assertEqual(parser.parse_args([]).key, "ctrl_l+space")
            self.assertEqual(parser.parse_args(["--key", "f9"]).key, "f9")

    def test_parser_uses_inject_mode_env_default(self):
        with _env(VOICEPI_INJECT_MODE="paste"):
            voice_pi = load_voice_pi()
            parser = voice_pi.build_arg_parser()

            self.assertEqual(parser.parse_args([]).mode, "paste")
            self.assertEqual(parser.parse_args(["--no-type"]).mode, "print")
            self.assertEqual(parser.parse_args(["--paste"]).mode, "paste")


@contextmanager
def _env(**kwargs):
    old = {k: os.environ.get(k) for k in kwargs}
    os.environ.update(kwargs)
    try:
        yield
    finally:
        for k, v in old.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v


class BuildYdotoolOpsTests(unittest.TestCase):
    """_build_ydotool_ops: tekst → liste af (subkommando, *args) tupler."""

    def setUp(self):
        self.vp = load_voice_pi()
        self.dk = self.vp._LAYOUT_KEYCODES['dk']

    def test_ascii_only_is_single_type_op(self):
        ops = self.vp._build_ydotool_ops("hello", {})
        self.assertEqual(ops, [('type', '--', 'hello')])

    def test_empty_string_gives_no_ops(self):
        ops = self.vp._build_ydotool_ops("", {})
        self.assertEqual(ops, [])

    def test_oe_splits_into_key_op(self):
        ops = self.vp._build_ydotool_ops("ø", self.dk)
        self.assertEqual(ops, [('key', '40:1', '40:0')])

    def test_mixed_flushes_ascii_buffer_before_special(self):
        # "høre" → type "h", key ø, type "re"
        ops = self.vp._build_ydotool_ops("høre", self.dk)
        self.assertEqual(ops, [
            ('type', '--', 'h'),
            ('key', '40:1', '40:0'),
            ('type', '--', 're'),
        ])

    def test_question_mark_uses_nordic_keycode(self):
        # '?' er shift+KEY_MINUS i nordiske layouts, ikke shift+KEY_SLASH
        ops = self.vp._build_ydotool_ops("hvad?", self.dk)
        self.assertEqual(ops, [
            ('type', '--', 'hvad'),
            ('key', '42:1', '12:1', '12:0', '42:0'),
        ])

    def test_consecutive_special_chars_each_get_key_op(self):
        ops = self.vp._build_ydotool_ops("æøå", self.dk)
        self.assertEqual(ops, [
            ('key', '39:1', '39:0'),  # æ
            ('key', '40:1', '40:0'),  # ø
            ('key', '26:1', '26:0'),  # å
        ])

    def test_uppercase_special_char(self):
        ops = self.vp._build_ydotool_ops("Ø", self.dk)
        self.assertEqual(ops, [('key', '42:1', '40:1', '40:0', '42:0')])

    def test_ascii_after_special_is_flushed(self):
        ops = self.vp._build_ydotool_ops("åben", self.dk)
        self.assertEqual(ops, [
            ('key', '26:1', '26:0'),  # å
            ('type', '--', 'ben'),
        ])

    def test_no_map_passthrough(self):
        # Uden keycode_map (f.eks. us-layout) → alt sendes som type
        ops = self.vp._build_ydotool_ops("høre", {})
        self.assertEqual(ops, [('type', '--', 'høre')])


class LayoutKeycodeMapTests(unittest.TestCase):
    """Mapningens indhold: hvert layout har de forventede specialtegn."""

    def setUp(self):
        self.vp = load_voice_pi()

    def _assert_has_chars(self, layout: str, chars: str):
        m = self.vp._LAYOUT_KEYCODES[layout]
        for ch in chars:
            with self.subTest(layout=layout, char=ch):
                self.assertIn(ch, m)

    def test_dk_has_ae_oe_aa(self):
        self._assert_has_chars('dk', 'æøåÆØÅ')

    def test_no_aliases_dk(self):
        self.assertIs(
            self.vp._LAYOUT_KEYCODES['no'],
            self.vp._LAYOUT_KEYCODES['dk'],
        )

    def test_se_has_ae_oe_aa(self):
        self._assert_has_chars('se', 'äöåÄÖÅ')

    def test_de_has_umlauts(self):
        self._assert_has_chars('de', 'äöüÄÖÜ')

    def test_fi_has_ae_oe(self):
        self._assert_has_chars('fi', 'äöÄÖ')

    def test_all_layouts_have_nordic_punct(self):
        punct = '?-_:;/"'
        for layout in ('dk', 'no', 'se', 'de', 'fi'):
            self._assert_has_chars(layout, punct)

    def test_es_has_n_tilde_and_accented_vowels(self):
        self._assert_has_chars('es', 'ñÑáéíóúÁÉÍÓÚüÜ')

    def test_pt_has_cedilla_and_accented_vowels(self):
        self._assert_has_chars('pt', 'çÇáéíóúÁÉÍÓÚàÀãõÃÕâêôÂÊÔ')

    def test_br_has_cedilla_tilde_circumflex(self):
        self._assert_has_chars('br', 'çÇãõÃÕâêôÂÊÔáéíóúÁÉÍÓÚ')

    def test_pl_has_polish_chars(self):
        self._assert_has_chars('pl', 'ąęóśźżćńłĄĘÓŚŹŻĆŃŁ')

    def test_ua_has_full_cyrillic_alphabet(self):
        self._assert_has_chars('ua', 'йцукенгшщзхїфівапролджєґячсмитьбюЙЦУКЕНГШЩЗХЇФІВАПРОЛДЖЄҐЯЧСМИТЬБЮ')

    def test_ru_not_in_lang_to_xkb(self):
        self.assertNotIn('ru', self.vp._LANG_TO_XKB)

    def test_uk_maps_to_ua(self):
        self.assertEqual(self.vp._LANG_TO_XKB.get('uk'), 'ua')

    def test_keycodes_are_balanced_per_key(self):
        # Stærkere end "lige antal codes": hvert keycode skal have lige
        # mange press (N:1) og release (N:0) i en sekvens — ellers hænger
        # fx Shift(42)/AltGr(100) og korrumperer efterfølgende input.
        import collections
        for layout, m in self.vp._LAYOUT_KEYCODES.items():
            for ch, codes in m.items():
                with self.subTest(layout=layout, char=ch):
                    bal: "collections.Counter[str]" = collections.Counter()
                    for tok in codes:
                        key, sep, state = tok.partition(":")
                        self.assertTrue(sep and state in ("0", "1"),
                                        f"Ugyldig token {tok!r} for '{ch}'")
                        bal[key] += 1 if state == "1" else -1
                    for key, net in bal.items():
                        self.assertEqual(
                            net, 0,
                            f"Keycode {key} ubalanceret for '{ch}' i "
                            f"layout '{layout}' (net={net} press-release)")


class DetectXkbLayoutTests(unittest.TestCase):
    """_detect_xkb_layout: prioritetsrækkefølge og fallback."""

    def setUp(self):
        self.vp = load_voice_pi()
        # Ryd env-variabler der ellers forstyrrer
        self._patches = [
            patch.dict(os.environ, {}, clear=False),
        ]
        for p in self._patches:
            p.start()
        os.environ.pop('VOICEPI_XKB_LAYOUT', None)
        os.environ.pop('XKB_DEFAULT_LAYOUT', None)

    def tearDown(self):
        for p in self._patches:
            p.stop()

    def test_voicepi_env_var_takes_priority(self):
        with _env(VOICEPI_XKB_LAYOUT='se', XKB_DEFAULT_LAYOUT='de'):
            result = self.vp._detect_xkb_layout('da')
        self.assertEqual(result, 'se')

    def test_xkb_default_layout_beats_keyboard_file(self):
        with _env(XKB_DEFAULT_LAYOUT='de'):
            with patch('builtins.open', side_effect=FileNotFoundError):
                result = self.vp._detect_xkb_layout('da')
        self.assertEqual(result, 'de')

    def test_keyboard_file_parsed_correctly(self):
        content = 'XKBLAYOUT="dk"\nXKBVARIANT=""\n'
        with patch('builtins.open',
                   unittest.mock.mock_open(read_data=content)):
            result = self.vp._detect_xkb_layout(None)
        self.assertEqual(result, 'dk')

    def test_us_layout_in_keyboard_file_is_ignored(self):
        content = 'XKBLAYOUT="us"\n'
        with patch('builtins.open',
                   unittest.mock.mock_open(read_data=content)):
            result = self.vp._detect_xkb_layout('da')
        # Falder igennem til lang-hint: da → dk
        self.assertEqual(result, 'dk')

    def test_lang_hint_da_gives_dk(self):
        with patch('builtins.open', side_effect=FileNotFoundError):
            result = self.vp._detect_xkb_layout('da')
        self.assertEqual(result, 'dk')

    def test_lang_hint_nb_gives_no(self):
        with patch('builtins.open', side_effect=FileNotFoundError):
            result = self.vp._detect_xkb_layout('nb')
        self.assertEqual(result, 'no')

    def test_no_hints_returns_none(self):
        with patch('builtins.open', side_effect=FileNotFoundError):
            result = self.vp._detect_xkb_layout(None)
        self.assertIsNone(result)


if __name__ == "__main__":
    unittest.main()
