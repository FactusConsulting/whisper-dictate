"""Regression tests for the audio-ducking active-ducker registry.

A config reload builds a fresh AudioDucker that is value-equal to the previous
one; de-duping by identity (not dataclass equality) ensures every active ducker
is registered for the atexit volume restore.
"""
from helpers import unittest

from whisper_dictate import vp_audio_ducking


class RegisterActiveDuckerTests(unittest.TestCase):
    def setUp(self):
        self._saved = list(vp_audio_ducking._ACTIVE_DUCKERS)
        vp_audio_ducking._ACTIVE_DUCKERS.clear()

        def _restore():
            vp_audio_ducking._ACTIVE_DUCKERS.clear()
            vp_audio_ducking._ACTIVE_DUCKERS.extend(self._saved)

        self.addCleanup(_restore)

    def test_equal_but_distinct_duckers_both_register(self):
        a = vp_audio_ducking.AudioDucker(enabled=True, target_volume=0.25)
        b = vp_audio_ducking.AudioDucker(enabled=True, target_volume=0.25)
        self.assertEqual(a, b)        # value-equal (dataclass)
        self.assertIsNot(a, b)        # but distinct instances
        vp_audio_ducking.register_active_ducker(a)
        vp_audio_ducking.register_active_ducker(b)
        self.assertEqual(len(vp_audio_ducking._ACTIVE_DUCKERS), 2)

    def test_same_instance_registers_once(self):
        a = vp_audio_ducking.AudioDucker(enabled=True, target_volume=0.25)
        vp_audio_ducking.register_active_ducker(a)
        vp_audio_ducking.register_active_ducker(a)
        self.assertEqual(len(vp_audio_ducking._ACTIVE_DUCKERS), 1)


if __name__ == "__main__":
    unittest.main()
