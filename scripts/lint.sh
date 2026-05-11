#!/usr/bin/env bash
# Format + clippy. Mirrors marginfi-v2's lint.sh.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

cargo fmt --all -- --check
cargo clippy -p ydelta --all-targets -- -D warnings
