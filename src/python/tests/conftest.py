from __future__ import annotations

import sys
from pathlib import Path


TESTS = Path(__file__).resolve().parent
PYTHON_SRC = Path(__file__).resolve().parents[1]

for path in (TESTS, PYTHON_SRC):
    if str(path) not in sys.path:
        sys.path.insert(0, str(path))
