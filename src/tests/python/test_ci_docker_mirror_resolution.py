"""Regression coverage for CI Docker image resolution paths."""

from helpers import Path, unittest


class CiDockerMirrorResolutionTests(unittest.TestCase):
    """Verify Nexus mirror env + fallback behavior is covered in workflow scripts."""

    def test_build_ci_images_uses_org_variable_not_hardcoded_mirror(self):
        workflow = Path(".github/workflows/build-ci-images.yml").read_text(
            encoding="utf-8"
        )

        self.assertIn("NEXUS_DOCKER_MIRROR: ${{ vars.NEXUS_DOCKER_MIRROR }}", workflow)
        self.assertNotIn("nexusdockerproxy.factus.dk", workflow)

    def test_build_ci_images_falls_back_to_upstream_when_mirror_unset_or_bad(self):
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
        self.assertIn('base_image="$upstream_base"', workflow)
        # If a mirror host is configured but pull fails, we should still fallback.
        self.assertIn(
            'if [ -n "$mirror_base" ]; then\n              echo "::notice::Nexus mirror miss for ${mirror_base}; using ${upstream_base}"',
            workflow,
        )

    def test_test_workflow_uses_org_variable_and_mirror_fallback(self):
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
