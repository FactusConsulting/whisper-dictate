from __future__ import annotations

import subprocess
import unittest
from unittest.mock import patch

import vp_doctor


class YdotooldDoctorTests(unittest.TestCase):
    def test_process_detail_rejects_process_with_unready_socket(self):
        completed = subprocess.CompletedProcess(["pgrep", "-x", "ydotoold"], 0, stdout="9132\n")

        with patch("vp_doctor.subprocess.run", return_value=completed):
            ok, detail = vp_doctor._ydotoold_process_detail(socket_ready=False)

        self.assertFalse(ok)
        self.assertIn("socket is not accepting connections", detail)
        self.assertIn("9132", detail)

    def test_process_detail_accepts_ready_socket(self):
        ok, detail = vp_doctor._ydotoold_process_detail(socket_ready=True)

        self.assertTrue(ok)
        self.assertEqual(detail, "accepting connections")


if __name__ == "__main__":
    unittest.main()
