"""Regression test: doctor's _in_group must honour the primary GID too.

os.getgroups() only lists *supplementary* groups; membership via the user's
primary group must not be reported as a false FAIL.
"""
from helpers import _capture_stdout, patch, types, unittest

from whisper_dictate import vp_doctor


class InGroupTests(unittest.TestCase):
    def _check(self, gid, groups, primary):
        fake_grp = types.SimpleNamespace(
            getgrnam=lambda name: types.SimpleNamespace(gr_gid=gid)
        )
        with patch.object(vp_doctor, "grp", fake_grp), \
                patch("os.getgroups", lambda: groups, create=True), \
                patch("os.getgid", lambda: primary, create=True):
            return vp_doctor._in_group("input")

    def test_member_via_supplementary_group(self):
        self.assertTrue(self._check(42, [10, 42], 1))

    def test_member_via_primary_gid(self):
        self.assertTrue(self._check(42, [10, 20], 42))

    def test_not_a_member(self):
        self.assertFalse(self._check(42, [10, 20], 1))

    def test_no_grp_module_returns_false(self):
        with patch.object(vp_doctor, "grp", None):
            self.assertFalse(vp_doctor._in_group("input"))


class PrintChecksTests(unittest.TestCase):
    def test_required_failure_marks_run_failed(self):
        checks = [
            vp_doctor.Check("alpha", True, "good"),
            vp_doctor.Check("beta", False, "broken"),
        ]
        with _capture_stdout() as buf:
            failed = vp_doctor._print_checks(checks)
        out = buf.getvalue()
        self.assertTrue(failed)
        self.assertIn("alpha: good", out)
        self.assertIn("beta: broken", out)
        self.assertIn("FAIL", out)

    def test_optional_failure_is_warn_not_failure(self):
        with _capture_stdout() as buf:
            failed = vp_doctor._print_checks(
                [vp_doctor.Check("gpu", False, "cpu only", required=False)]
            )
        self.assertFalse(failed)
        self.assertIn("WARN", buf.getvalue())


class BaseChecksTests(unittest.TestCase):
    def test_reports_session_and_python(self):
        checks = {c.name: c for c in vp_doctor._base_checks(on_linux=True, on_wayland=True)}
        self.assertEqual(checks["session"].detail, "Wayland detected")
        self.assertTrue(checks["python"].ok)  # the suite runs on a supported Python
        self.assertFalse(checks["platform"].required)

    def test_non_wayland_session_detail(self):
        checks = {c.name: c for c in vp_doctor._base_checks(on_linux=False, on_wayland=False)}
        self.assertEqual(checks["session"].detail, "not a Wayland session")


class RunDoctorTests(unittest.TestCase):
    def test_returns_zero_when_required_checks_pass(self):
        with patch.object(vp_doctor, "readiness_checks",
                          lambda: [vp_doctor.Check("x", True, "ok")]), \
                patch.object(vp_doctor, "_linux_checks", lambda: []):
            with _capture_stdout() as buf:
                rc = vp_doctor.run_doctor()
        self.assertEqual(rc, 0)
        self.assertIn("[doctor]", buf.getvalue())

    def test_returns_one_and_prints_fix_hints_on_required_failure(self):
        with patch.object(vp_doctor, "readiness_checks",
                          lambda: [vp_doctor.Check("x", False, "bad")]), \
                patch.object(vp_doctor, "_linux_checks", lambda: []):
            with _capture_stdout() as buf:
                rc = vp_doctor.run_doctor()
        out = buf.getvalue()
        self.assertEqual(rc, 1)
        self.assertIn("Fix hints", out)


if __name__ == "__main__":
    unittest.main()
