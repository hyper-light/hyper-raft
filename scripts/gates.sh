#!/usr/bin/env bash
# The gates every commit passes on its final tree, in order, stopping at the first failure
# (CLAUDE.md §4). CI runs the same script on all six targets.
set -euo pipefail
cd "$(dirname "$0")/.."

python3 scripts/check-contracts.py
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
# Each test's end and each test binary's start, with the machine's memory, go to a file and to the
# job's summary as they happen (scripts/gate-progress.py): a runner lost mid-run leaves a record of
# what ran last.
cargo test --workspace --all-features --locked 2>&1 | python3 scripts/gate-progress.py
