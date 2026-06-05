from __future__ import annotations

import subprocess
import unittest
from unittest.mock import patch

from whisper_dictate import runtime


class YdotooldDoctorTests(unittest.TestCase):
    def test_process_detail_rejects_process_with_unready_socket(self):
        completed = subprocess.CompletedProcess(["pgrep", "-x", "ydotoold"], 0, stdout="9132\n")

        with patch("whisper_dictate.runtime.subprocess.run", return_value=completed):
            ok, detail = runtime._ydotoold_process_detail(socket_ready=False)

        self.assertFalse(ok)
        self.assertIn("socket is not accepting connections", detail)
        self.assertIn("9132", detail)

    def test_process_detail_accepts_ready_socket(self):
        ok, detail = runtime._ydotoold_process_detail(socket_ready=True)

        self.assertTrue(ok)
        self.assertEqual(detail, "accepting connections")


if __name__ == "__main__":
    unittest.main()
