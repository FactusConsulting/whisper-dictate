"""no-egui-outside-ui boundary test.

Enforces that egui/eframe types are confined to src/rust/ui/ (and the
eframe::run_native call in src/rust/main.rs), making the UI a replaceable
facade. If any .rs file OUTSIDE those two paths imports or references egui
or eframe types, this test fails — making "core must not depend on egui" a
CI-enforced invariant.
"""

from helpers import (
    Path,
    unittest,
)
import re


# Patterns that indicate a Rust file depends on the egui/eframe ecosystem.
_EGUI_PATTERNS = re.compile(
    r"use egui\b"        # `use egui` / `use egui::…`
    r"|use eframe\b"     # `use eframe` / `use eframe::…`
    r"|egui::"           # inline type path, e.g. egui::Context
    r"|eframe::"         # inline type path, e.g. eframe::NativeOptions
)

# Files whose egui/eframe usage is intentional and explicitly allowed:
#   ui.rs   — module root, owns the UI `run()` fn, re-exports into crate::ui
#   ui/     — the entire UI submodule tree (this is the egui facade)
#   main.rs — calls eframe::run_native as the binary entry point
_ALLOWED_PREFIXES = (
    "src/rust/ui.rs",
    "src/rust/ui/",
    "src/rust/main.rs",
)


def _rust_files_under_src_rust():
    """All .rs files under src/rust/ (non-recursive under ui/ handled by prefix)."""
    return sorted(Path("src/rust").rglob("*.rs"))


class NoEguiOutsideUiBoundaryTest(unittest.TestCase):
    """Enforce that egui/eframe symbols never leak into core/domain modules."""

    def test_egui_confined_to_ui_module_and_main(self):
        violations = []
        for path in _rust_files_under_src_rust():
            posix = path.as_posix()
            # Skip files that are explicitly allowed to use egui/eframe.
            if any(posix.startswith(p) or posix == p for p in _ALLOWED_PREFIXES):
                continue
            text = path.read_text(encoding="utf-8")
            # Strip line comments so `// use egui` in disabled code won't fire.
            stripped = re.sub(r"//[^\n]*", "", text)
            if _EGUI_PATTERNS.search(stripped):
                matching_lines = [
                    f"  {path}:{i + 1}: {line.rstrip()}"
                    for i, line in enumerate(text.splitlines())
                    if _EGUI_PATTERNS.search(re.sub(r"//.*", "", line))
                ]
                violations.extend(matching_lines)

        self.assertEqual(
            [],
            violations,
            "egui/eframe references found outside src/rust/ui/ and src/rust/main.rs "
            "(the UI must remain a replaceable facade — move these into ui/):\n"
            + "\n".join(violations),
        )
