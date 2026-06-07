"""Regression test: doctor's _in_group must honour the primary GID too.

os.getgroups() only lists *supplementary* groups; membership via the user's
primary group must not be reported as a false FAIL.
"""
from helpers import patch, types, unittest

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


if __name__ == "__main__":
    unittest.main()
