#!/usr/bin/env bash
# The gates every commit passes on its final tree, in order, stopping at the first failure
# (CLAUDE.md §4). CI runs the same script on all six targets.
set -euo pipefail
cd "$(dirname "$0")/.."

python3 scripts/check-contracts.py
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
