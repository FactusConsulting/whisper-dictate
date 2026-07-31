"""Regression coverage for CI Docker image resolution paths."""

from dataclasses import dataclass
from helpers import Path, unittest


@dataclass
class _ResolverResult:
    image: str
    calls: list[str]
    status_log: list[str]


def _mock_docker_pull(
    image: str,
    exit_code: int,
    calls: list[str],
    status_log: list[str],
) -> bool:
    calls.append(f"pull {image}")
    status_log.append(f"docker pull {image} => {'ok' if exit_code == 0 else 'fail'}")
    return exit_code == 0


class CiDockerMirrorResolutionTests(unittest.TestCase):
    """Verify Nexus mirror env + fallback behavior is covered in workflow scripts."""

    def _resolve_actionlint_flow(
        self,
        *,
        mirror_host: str,
        mirror_exit: int,
        upstream_exit: int,
    ) -> _ResolverResult:
        upstream_image = "rhysd/actionlint:1.7.12"
        mirror_image = f"{mirror_host}/{upstream_image}" if mirror_host else ""
        calls: list[str] = []
        status_log: list[str] = []

        if mirror_image:
            if _mock_docker_pull(mirror_image, mirror_exit, calls, status_log):
                return _ResolverResult(mirror_image, calls, status_log)

            status_log.append(
                f"notice: Nexus mirror miss for {mirror_image}; falling back to {upstream_image}"
            )

        if _mock_docker_pull(upstream_image, upstream_exit, calls, status_log):
            return _ResolverResult(upstream_image, calls, status_log)

        raise AssertionError(f"Failed to pull {upstream_image} from Docker Hub.")

    def _resolve_build_ci_base(
        self,
        *,
        mirror_host: str,
        mirror_exit: int,
    ) -> _ResolverResult:
        upstream_base = "ubuntu:26.04"
        mirror_base = f"{mirror_host}/library/ubuntu:26.04" if mirror_host else ""
        calls: list[str] = []
        status_log: list[str] = []
        base_image = upstream_base

        if mirror_base:
            if _mock_docker_pull(mirror_base, mirror_exit, calls, status_log):
                base_image = mirror_base
            else:
                status_log.append(
                    f"notice: Nexus mirror miss for {mirror_base}; using {upstream_base}"
                )

        # The script only sets an upstream default; it does not pull upstream here.
        return _ResolverResult(base_image, calls, status_log)

    def test_build_ci_images_uses_org_variable_not_hardcoded_mirror(self):
        workflow = Path(".github/workflows/build-ci-images.yml").read_text(
            encoding="utf-8"
        )

        self.assertIn("NEXUS_DOCKER_MIRROR: ${{ vars.NEXUS_DOCKER_MIRROR }}", workflow)
        self.assertNotIn("nexusdockerproxy.factus.dk", workflow)

    def test_build_ci_images_chooses_mirror_or_fallback(self):
        workflow = Path(".github/workflows/build-ci-images.yml").read_text(
            encoding="utf-8"
        )

        self.assertIn(
            'mirror_base="${NEXUS_DOCKER_MIRROR:+$NEXUS_DOCKER_MIRROR/library/ubuntu:26.04}"',
            workflow,
        )
        self.assertIn(
            'if [ -n "$mirror_base" ] && docker pull "$mirror_base"; then',
            workflow,
        )
        self.assertIn("base_image=\"$upstream_base\"", workflow)
        self.assertIn(
            'if [ -n "$mirror_base" ]; then\n              echo "::notice::Nexus mirror miss for ${mirror_base}; using ${upstream_base}"',
            workflow,
        )

        mirror_ok = self._resolve_build_ci_base(
            mirror_host="mirror.example",
            mirror_exit=0,
        )
        self.assertEqual(mirror_ok.image, "mirror.example/library/ubuntu:26.04")
        self.assertEqual(mirror_ok.calls, ["pull mirror.example/library/ubuntu:26.04"])

        mirror_falls_back_to_base = self._resolve_build_ci_base(
            mirror_host="mirror.example",
            mirror_exit=1,
        )
        self.assertEqual(mirror_falls_back_to_base.image, "ubuntu:26.04")
        self.assertEqual(mirror_falls_back_to_base.calls, ["pull mirror.example/library/ubuntu:26.04"])
        self.assertIn(
            "notice: Nexus mirror miss for mirror.example/library/ubuntu:26.04; using ubuntu:26.04",
            mirror_falls_back_to_base.status_log,
        )

        mirror_unset = self._resolve_build_ci_base(
            mirror_host="",
            mirror_exit=1,
        )
        self.assertEqual(mirror_unset.image, "ubuntu:26.04")
        self.assertEqual(mirror_unset.calls, [])

    def test_actionlint_image_resolution_runs_expected_branches(self):
        workflow = Path(".github/workflows/test.yml").read_text(encoding="utf-8")

        self.assertIn("NEXUS_DOCKER_MIRROR: ${{ vars.NEXUS_DOCKER_MIRROR }}", workflow)
        self.assertNotIn("nexusdockerproxy.factus.dk", workflow)

        self.assertIn(
            'mirror_image="${NEXUS_DOCKER_MIRROR:+$NEXUS_DOCKER_MIRROR/$upstream_image}"',
            workflow,
        )
        self.assertIn(
            'if [ -n "$mirror_image" ] && docker pull "$mirror_image"; then',
            workflow,
        )
        self.assertIn(
            'if docker pull "$upstream_image"; then',
            workflow,
        )
        self.assertIn(
            'mirror_base="${NEXUS_DOCKER_MIRROR:+$NEXUS_DOCKER_MIRROR/library/ubuntu:26.04}"',
            workflow,
        )
        self.assertIn(
            'if [ -n "$mirror_base" ] && docker pull "$mirror_base"; then',
            workflow,
        )

        mirror_ok = self._resolve_actionlint_flow(
            mirror_host="mirror.example",
            mirror_exit=0,
            upstream_exit=0,
        )
        self.assertEqual(
            mirror_ok.image, "mirror.example/rhysd/actionlint:1.7.12"
        )
        self.assertEqual(
            mirror_ok.calls,
            ["pull mirror.example/rhysd/actionlint:1.7.12"],
        )

        mirror_fallback = self._resolve_actionlint_flow(
            mirror_host="mirror.example",
            mirror_exit=1,
            upstream_exit=0,
        )
        self.assertEqual(mirror_fallback.image, "rhysd/actionlint:1.7.12")
        self.assertEqual(
            mirror_fallback.calls,
            [
                "pull mirror.example/rhysd/actionlint:1.7.12",
                "pull rhysd/actionlint:1.7.12",
            ],
        )
        self.assertIn(
            "notice: Nexus mirror miss for mirror.example/rhysd/actionlint:1.7.12; falling back to rhysd/actionlint:1.7.12",
            mirror_fallback.status_log,
        )

        direct_pull = self._resolve_actionlint_flow(
            mirror_host="",
            mirror_exit=1,
            upstream_exit=0,
        )
        self.assertEqual(direct_pull.image, "rhysd/actionlint:1.7.12")
        self.assertEqual(
            direct_pull.calls,
            ["pull rhysd/actionlint:1.7.12"],
        )
