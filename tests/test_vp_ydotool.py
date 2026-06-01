from __future__ import annotations

import subprocess
import unittest
from unittest.mock import patch

import vp_ydotool


class YdotoolReadinessTests(unittest.TestCase):
    def test_ready_accepts_ydotool_debug_even_when_socket_connect_fails(self):
        with patch("vp_ydotool.shutil.which", return_value="/usr/bin/ydotool"), \
             patch("vp_ydotool.subprocess.run") as run, \
             patch("vp_ydotool.unix_socket_connect_ready", return_value=False):
            run.return_value = subprocess.CompletedProcess(["ydotool", "debug"], 0)

            self.assertTrue(vp_ydotool.ydotoold_ready("/tmp/ydotool.sock", timeout=0.1))

            _, kwargs = run.call_args
            self.assertEqual(kwargs["env"]["YDOTOOL_SOCKET"], "/tmp/ydotool.sock")

    def test_ready_falls_back_to_socket_connect_without_ydotool_client(self):
        with patch("vp_ydotool.shutil.which", return_value=None), \
             patch("vp_ydotool.unix_socket_connect_ready", return_value=True):
            self.assertTrue(vp_ydotool.ydotoold_ready("/tmp/ydotool.sock", timeout=0.1))

