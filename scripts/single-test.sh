#!/usr/bin/env bash
# Run one test by name with full debug logs. Defaults to SBPF mode since
# single-test runs are usually a debugging session — switch with `--native`.
#
# Usage:
#   scripts/single-test.sh <test_name>
#   scripts/single-test.sh --native <test_name>
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

mode=sbf
if [[ "${1-}" == "--native" ]]; then
    mode=native
    shift
fi

if [[ "$#" -lt 1 ]]; then
    echo "Usage: $0 [--native] <test_name>" >&2
    exit 1
fi

test_name="$1"

if [[ "$mode" == "sbf" ]]; then
    exec "$ROOT/scripts/test.sh" --sbf --verbose "$test_name"
else
    exec "$ROOT/scripts/test.sh" --verbose "$test_name"
fi
