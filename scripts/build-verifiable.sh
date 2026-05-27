#!/usr/bin/env bash
# Build the ydelta program inside the deterministic Docker container
# `solana-verify` ships, so the resulting `target/deploy/ydelta.so` has a
# byte-for-byte stable hash that OtterSec / explorers can match against a
# public source rebuild.
#
# Use this for any upgrade you want stamped as a verified build.
# Regular dev iteration can stay on `scripts/build-program.sh` (faster,
# host toolchain).
#
# Outputs:
#   target/deploy/ydelta.so       — verifiable artifact
#   target/deploy/ydelta.so.sha256 — convenience hash to compare with on-chain
#
# Requires:
#   solana-verify >= 0.4 (`cargo install solana-verify`)
#   Docker daemon running
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if ! command -v solana-verify >/dev/null 2>&1; then
    echo "Error: solana-verify not on PATH." >&2
    echo "       Install with: cargo install solana-verify" >&2
    exit 1
fi

if ! docker info >/dev/null 2>&1; then
    echo "Error: Docker daemon is not running. solana-verify needs it for" >&2
    echo "       reproducible builds. Start Docker Desktop (or your daemon)" >&2
    echo "       and re-run." >&2
    exit 1
fi

echo "Running: solana-verify build --library-name ydelta"
solana-verify build --library-name ydelta

artifact="$ROOT/target/deploy/ydelta.so"
if [[ ! -f "$artifact" ]]; then
    echo "Error: $artifact missing after build" >&2
    exit 1
fi

hash=$(solana-verify get-executable-hash "$artifact")
echo "$hash" > "$artifact.sha256"

echo
echo "── Verifiable build complete ─────────────────────────────────"
echo "  artifact:        $artifact"
echo "  bytes:           $(wc -c < "$artifact" | tr -d ' ')"
echo "  executable hash: $hash"
echo "──────────────────────────────────────────────────────────────"
echo
echo "Next steps:"
echo "  1. Deploy / upgrade with scripts/upgrade-program.sh --skip-build"
echo "     (so the upgrade uses THIS verifiable .so)"
echo "  2. After deploy lands, run scripts/verify-program.sh to register"
echo "     the build on the OtterSec verification PDA."
