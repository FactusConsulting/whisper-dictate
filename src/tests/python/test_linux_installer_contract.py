from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]


def test_linux_installer_checks_xdotool_before_prebuilt_or_source_routes() -> None:
    script = (ROOT / "scripts/linux/install-rust-ui.sh").read_text(encoding="utf-8")

    check = script.index("require_focus_probe_prerequisite\n")
    route = script.index('if [[ -x "${HERE}/wd" ]]')

    assert check < route
    assert '[[ -z "${DISPLAY:-}" ]]' in script
    assert "command -v xdotool" in script
    assert "sudo apt install xdotool" in script


def test_linux_build_prerequisites_install_xdotool() -> None:
    building = (ROOT / "docs/dev/BUILDING.md").read_text(encoding="utf-8")

    assert "xclip xdotool" in building
