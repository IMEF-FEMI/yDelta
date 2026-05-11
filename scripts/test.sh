#!/usr/bin/env bash
# Run ydelta tests. Defaults to native (`processor!`) execution for speed;
# pass `--sbf` to compile the .so via `cargo build-sbf` and run through the
# real SBPF VM. Filters and `--verbose` are forwarded to cargo test.
#
# Usage:
#   scripts/test.sh                 # native, all tests
#   scripts/test.sh --sbf           # SBPF, all tests
#   scripts/test.sh some_test       # native, filter
#   scripts/test.sh --sbf some_test
#   scripts/test.sh --verbose       # debug log + --nocapture
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

sbf=false
verbose=false
filters=()

while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --sbf)     sbf=true; shift ;;
        --verbose) verbose=true; shift ;;
        --) shift; filters+=("$@"); break ;;
        *) filters+=("$1"); shift ;;
    esac
done

if [[ "$verbose" == "true" ]]; then
    export RUST_LOG="solana_runtime::message_processor::stable_log=debug,solana_program_test=info"
else
    export RUST_LOG="${RUST_LOG:-solana_runtime::message_processor::stable_log=warn}"
fi

# Ensure transitive deps that require `edition2024` (rustc >= 1.85) are
# pinned to versions parsable by the SBF toolchain's rustc 1.79. Re-applies
# if the lockfile was regenerated. Idempotent.
ensure_lockfile_pins() {
    cargo update -p blake3 --precise 1.6.1 >/dev/null 2>&1 || true
    cargo update -p 'proc-macro-crate@3.5.0' --precise 3.2.0 >/dev/null 2>&1 || true
    cargo update -p indexmap --precise 2.9.0 >/dev/null 2>&1 || true
    cargo update -p darling --precise 0.21.3 >/dev/null 2>&1 || true
    cargo update -p time --precise 0.3.41 >/dev/null 2>&1 || true
    cargo update -p serde_with --precise 3.14.0 >/dev/null 2>&1 || true
    cargo update -p ruint --precise 1.12.4 >/dev/null 2>&1 || true
}

if [[ "$sbf" == "true" ]]; then
    # `cargo test-sbf` builds the .so, points SBF_OUT_DIR at it, then runs
    # cargo test against the host targets. Pass `--features test-sbf` so the
    # fixture switches to ProgramTest::new(..., None) → SBPF load.
    if ! command -v cargo-test-sbf >/dev/null 2>&1; then
        echo "Error: cargo-test-sbf not on PATH. Install via:" >&2
        echo "  sh -c \"\$(curl -sSfL https://release.anza.xyz/stable/install)\"" >&2
        exit 1
    fi
    # The SBF toolchain ships its own rustc; modern solana-program-test
    # transitives need rustc >= 1.85 (edition2024). solana-cli 2.2+ ships
    # platform-tools v1.45 with rustc 1.84+; older releases (e.g. 2.1.20)
    # ship rustc 1.79 and will fail on edition2024 deps. Surface a clear
    # error early instead of letting cargo dump a transitive parse failure.
    sbf_rustc_version=$(cargo build-sbf --version 2>/dev/null | awk '/rustc/ {print $2}' | head -1)
    if [[ -n "$sbf_rustc_version" ]]; then
        major=$(echo "$sbf_rustc_version" | cut -d. -f1)
        minor=$(echo "$sbf_rustc_version" | cut -d. -f2)
        if [[ "$major" -lt 1 ]] || { [[ "$major" -eq 1 ]] && [[ "$minor" -lt 84 ]]; }; then
            echo "Warning: SBF toolchain ships rustc $sbf_rustc_version; modern" >&2
            echo "         solana-program-test deps require >= 1.85. Upgrade with:" >&2
            echo "  agave-install init 2.2.0" >&2
            echo "         (or run with --native if you only need the host build.)" >&2
        fi
    fi
    ensure_lockfile_pins
    # cargo test-sbf -p ydelta only builds ydelta.so. Adapter integration
    # tests also need ydelta_test_harness.so built into the same SBF_OUT_DIR.
    # Build it explicitly here so target/deploy has both .so files before
    # tests start.
    echo "Building ydelta-test-harness.so..."
    cargo build-sbf --manifest-path "$ROOT/programs/ydelta-test-harness/Cargo.toml" >/dev/null
    cmd=(cargo test-sbf -p ydelta --features test-sbf --test cases)
else
    cmd=(cargo test -p ydelta --test cases)
fi

if [[ ${#filters[@]} -gt 0 || "$verbose" == "true" ]]; then
    cmd+=(--)
    if [[ "$verbose" == "true" ]]; then
        cmd+=(--nocapture)
    fi
    if [[ ${#filters[@]} -gt 0 ]]; then
        cmd+=("${filters[@]}")
    fi
fi

echo "Running: ${cmd[*]}"
exec "${cmd[@]}"
