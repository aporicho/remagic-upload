#!/usr/bin/env python3
from pathlib import Path

limit = 520
violations = []
for path in Path("src").rglob("*.rs"):
    lines = path.read_text().count("\n") + 1
    if lines > limit:
        violations.append(f"{path}: {lines} lines (limit {limit})")
if violations:
    raise SystemExit("\n".join(violations))
print("architecture size budget ok")
