"""Guard: every subprocess call that decodes text must pin UTF-8.

``subprocess.run(..., text=True)`` (or ``universal_newlines=True``) without an
explicit ``encoding=`` decodes/encodes via the platform locale — cp1252 on
Danish Windows — so the Rust helper's UTF-8 JSON, and any non-ASCII
transcription, mojibakes or raises. This AST scan keeps the whole worker package
on ``encoding="utf-8"`` instead of relying on each call site being remembered.
"""
import ast
import unittest
from pathlib import Path

PACKAGE = Path("src/python/whisper_dictate")
_SUBPROCESS_FUNCS = {"run", "Popen", "call", "check_call", "check_output"}


def _is_subprocess_call(node: ast.Call) -> bool:
    func = node.func
    return (
        isinstance(func, ast.Attribute)
        and func.attr in _SUBPROCESS_FUNCS
        and isinstance(func.value, ast.Name)
        and func.value.id == "subprocess"
    )


def _decodes_text(node: ast.Call) -> bool:
    kwargs = {kw.arg for kw in node.keywords if kw.arg}
    return "text" in kwargs or "universal_newlines" in kwargs


def _pins_utf8(node: ast.Call) -> bool:
    # Require encoding="utf-8" specifically — a present-but-wrong encoding
    # (e.g. cp1252) would still mangle the helper's UTF-8 output.
    for kw in node.keywords:
        if kw.arg == "encoding":
            return isinstance(kw.value, ast.Constant) and kw.value.value == "utf-8"
    return False


class SubprocessEncodingTests(unittest.TestCase):
    def test_text_mode_subprocess_calls_pin_utf8(self):
        violations = []
        for path in sorted(PACKAGE.glob("*.py")):
            tree = ast.parse(path.read_text(encoding="utf-8"))
            for node in ast.walk(tree):
                if isinstance(node, ast.Call) and _is_subprocess_call(node):
                    if _decodes_text(node) and not _pins_utf8(node):
                        violations.append(f"{path.name}:{node.lineno}")
        self.assertEqual(
            violations,
            [],
            'subprocess text-mode calls must set encoding="utf-8" — locale '
            f"decoding breaks UTF-8 on Windows (cp1252): {violations}",
        )


if __name__ == "__main__":
    unittest.main()
