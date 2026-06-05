from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from whisper_dictate import vp_ydotool


class YdotoolReadinessTests(unittest.TestCase):
    def _socket_path(self) -> str:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        return str(Path(self._tmp.name) / "ydotool.sock")

    def test_ready_accepts_ydotool_debug_even_when_socket_connect_fails(self):
        socket_path = self._socket_path()
        with patch("whisper_dictate.vp_ydotool.shutil.which", return_value="/usr/bin/ydotool"), \
             patch("whisper_dictate.vp_ydotool.subprocess.run") as run, \
             patch("whisper_dictate.vp_ydotool.unix_socket_connect_ready", return_value=False):
            run.return_value = subprocess.CompletedProcess(["ydotool", "debug"], 0)

            self.assertTrue(vp_ydotool.ydotoold_ready(socket_path, timeout=0.1))

            _, kwargs = run.call_args
            self.assertEqual(kwargs["env"]["YDOTOOL_SOCKET"], socket_path)

    def test_ready_falls_back_to_socket_connect_without_ydotool_client(self):
        socket_path = self._socket_path()
        with patch("whisper_dictate.vp_ydotool.shutil.which", return_value=None), \
             patch("whisper_dictate.vp_ydotool.unix_socket_connect_ready", return_value=True):
            self.assertTrue(vp_ydotool.ydotoold_ready(socket_path, timeout=0.1))
