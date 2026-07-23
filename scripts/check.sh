#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python3 tests/check_manifest.py
python3 tests/check_architecture.py
python3 scripts/remagic-bundle.py --help >/dev/null
