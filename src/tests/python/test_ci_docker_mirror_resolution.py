"""Regression coverage for CI Docker image resolution paths."""

from dataclasses import dataclass

from helpers import Path, os, subprocess, tempfile, unittest


_ACTIONLINT_RESOLVER = """#!/usr/bin/env bash
set -eu
upstream_image="rhysd/actionlint:1.7.12"
mirror_image="${NEXUS_DOCKER_MIRROR:+$NEXUS_DOCKER_MIRROR/$upstream_image}"
if [ -n "$mirror_image" ] && docker pull "$mirror_image"; then
  echo "image=$mirror_image" >> "$GITHUB_OUTPUT"
else
  if [ -n "$mirror_image" ]; then
    echo "::notice::Nexus mirror miss for ${mirror_image}; falling back to ${upstream_image}"
  fi
  if docker pull "$upstream_image"; then
    echo "image=$upstream_image" >> "$GITHUB_OUTPUT"
  else
    echo "::error::Failed to pull ${upstream_image} from Docker Hub."
    exit 1
  fi
fi
"""


_BUILD_BASE_RESOLVER = """#!/usr/bin/env bash
set -eu
upstream_base="ubuntu:26.04"
base_image="$upstream_base"
mirror_base="${NEXUS_DOCKER_MIRROR:+$NEXUS_DOCKER_MIRROR/library/ubuntu:26.04}"
if [ -n "$mirror_base" ] && docker pull "$mirror_base"; then
  base_image="$mirror_base"
else
  if [ -n "$mirror_base" ]; then
    echo "::notice::Nexus mirror miss for ${mirror_base}; using ${upstream_base}"
  fi
fi
echo "image=$base_image" >> "$GITHUB_OUTPUT"
"""

@dataclass
class _ResolverResult:
    image: str
    calls: list[str]
    status_log: list[str]


def _run_resolver_shell(
    script: str,
    env: dict[str, str],
) -> _ResolverResult:
    with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as raw_tmp:
        temp_dir = Path(raw_tmp)
        output_file = temp_dir / "GITHUB_OUTPUT"
        calls_file = temp_dir / "calls"
        output_file.write_text("", encoding="utf-8")
        calls_file.write_text("", encoding="utf-8")

        bin_dir = temp_dir / "bin"
        bin_dir.mkdir()

        docker = bin_dir / "docker"
        docker.write_text(
            """#!/usr/bin/env bash
set -eu
printf "%s %s\\n" "$1" "$2" >> "$MOCK_DOCKER_CALLS"
if [[ "$1" != "pull" ]]; then
  exit 0
fi
case "$2" in
  "$MIRROR_IMAGE")
    exit "$MIRROR_EXIT"
    ;;
  "$UPSTREAM_IMAGE")
    exit "$UPSTREAM_EXIT"
    ;;
    *)
    exit 0
    ;;
esac
""",
            encoding="utf-8",
            newline="\n",
        )
        docker.chmod(0o700)

        resolver = temp_dir / "resolver.sh"
        resolver.write_text(script, encoding="utf-8", newline="\n")
        resolver.chmod(0o700)

        shell_env = dict(os.environ)
        shell_env.update(env)

        if os.name == "nt":
            # Prefer WSL bash on Windows; avoid relying on the Windows Bash app
            # launcher (System32\\bash.exe), which can fail in headless runs.
            def _shell_q(value: str) -> str:
                return "'" + value.replace("'", "'\"'\"'") + "'"

            def _wsl_path(value: str) -> str:
                converted = subprocess.run(
                    ["wsl", "wslpath", "-a", "-u", value.replace("\\", "/")],
                    check=False,
                    capture_output=True,
                    text=True,
                )
                if converted.returncode != 0:
                    raise unittest.SkipTest(
                        "WSL is required on Windows for shell-based resolver tests"
                    )
                path = converted.stdout.strip()
                if not path:
                    raise unittest.SkipTest(
                        "Could not resolve WSL path for resolver execution"
                    )
                return path

            runner = temp_dir / "run-resolver.sh"
            runner_path = _wsl_path(str(runner))
            bin_path = _wsl_path(str(bin_dir))
            runner.write_text(
                "\n".join(
                    [
                        "#!/usr/bin/env bash",
                        "set -eu",
                        f"export PATH={_shell_q(bin_path + ':/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin')}",
                        f"export MOCK_DOCKER_CALLS={_shell_q(_wsl_path(str(calls_file)))}",
                        f"export GITHUB_OUTPUT={_shell_q(_wsl_path(str(output_file)))}",
                        f"export NEXUS_DOCKER_MIRROR={_shell_q(env.get('NEXUS_DOCKER_MIRROR', ''))}",
                        f"export MIRROR_IMAGE={_shell_q(env.get('MIRROR_IMAGE', ''))}",
                        f"export UPSTREAM_IMAGE={_shell_q(env.get('UPSTREAM_IMAGE', ''))}",
                        f"export MIRROR_EXIT={_shell_q(env.get('MIRROR_EXIT', '1'))}",
                        f"export UPSTREAM_EXIT={_shell_q(env.get('UPSTREAM_EXIT', '1'))}",
                        f"bash {_shell_q(_wsl_path(str(resolver)))}",
                    ]
                ),
                encoding="utf-8",
                newline="\n",
            )
            runner.chmod(0o700)
            shell_env = dict(os.environ)
            command = ["wsl", "bash", "-lc", f"bash {_shell_q(runner_path)}"]
        else:
            shell_env["PATH"] = str(bin_dir) + os.pathsep + shell_env.get("PATH", "")
            shell_env["MOCK_DOCKER_CALLS"] = str(calls_file)
            shell_env["GITHUB_OUTPUT"] = str(output_file)
            command = ["bash", "-eu", str(resolver)]

        completed = subprocess.run(
            command,
            cwd=str(temp_dir),
            env=shell_env,
            capture_output=True,
            text=True,
            check=False,
        )
        if completed.returncode != 0:
            raise AssertionError(
                "resolver shell failed\n"
                f"rc={completed.returncode}\nstdout={completed.stdout}\nstderr={completed.stderr}"
            )

        calls = calls_file.read_text(encoding="utf-8").splitlines()
        status_log = []
        for line in completed.stdout.splitlines():
            if line.startswith("::notice::"):
                status_log.append("notice: " + line.removeprefix("::notice::"))
            elif line.startswith("::error::"):
                status_log.append("error: " + line.removeprefix("::error::"))
            else:
                status_log.append(line)

        output_lines = output_file.read_text(encoding="utf-8").splitlines()
        image_line = next((line for line in output_lines if line.startswith("image=")), None)
        if image_line is None:
            raise AssertionError(
                f"resolver output did not set image\nstdout={completed.stdout}\nstderr={completed.stderr}"
            )
        return _ResolverResult(image_line.split("=", 1)[1], calls, status_log)


class CiDockerMirrorResolutionTests(unittest.TestCase):
    """Verify Nexus mirror env + fallback behavior is covered in workflow scripts."""

    def _resolve_actionlint_flow(
        self,
        *,
        mirror_host: str,
        mirror_exit: int,
        upstream_exit: int,
    ) -> _ResolverResult:
        return _run_resolver_shell(
            _ACTIONLINT_RESOLVER,
            {
                "NEXUS_DOCKER_MIRROR": mirror_host,
                "MIRROR_IMAGE": f"{mirror_host}/rhysd/actionlint:1.7.12" if mirror_host else "",
                "UPSTREAM_IMAGE": "rhysd/actionlint:1.7.12",
                "MIRROR_EXIT": str(mirror_exit),
                "UPSTREAM_EXIT": str(upstream_exit),
            },
        )

    def _resolve_build_ci_base(
        self,
        *,
        mirror_host: str,
        mirror_exit: int,
    ) -> _ResolverResult:
        return _run_resolver_shell(
            _BUILD_BASE_RESOLVER,
            {
                "NEXUS_DOCKER_MIRROR": mirror_host,
                "MIRROR_IMAGE": f"{mirror_host}/library/ubuntu:26.04" if mirror_host else "",
                "UPSTREAM_IMAGE": "ubuntu:26.04",
                "MIRROR_EXIT": str(mirror_exit),
                "UPSTREAM_EXIT": "0",
            },
        )

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
