#!/usr/bin/env bash
# Compile ydelta as SBPF bytecode. Outputs `target/deploy/ydelta.so`.
# Useful as a standalone step; for tests you usually want
# `scripts/test.sh --sbf`, which calls `cargo test-sbf` and handles the
# build + `SBF_OUT_DIR` in one shot.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

cmd=(cargo build-sbf --manifest-path "$ROOT/programs/ydelta/Cargo.toml")
echo "Running: ${cmd[*]}"
"${cmd[@]}"

artifact="$ROOT/target/deploy/ydelta.so"
if [[ ! -f "$artifact" ]]; then
    echo "Error: expected $artifact to exist after build" >&2
    exit 1
fi
echo "Built: $artifact"
