#!/usr/bin/env bash
# Confirm the deployed program hash matches a verifiable local build, then
# (optionally) post a verification request to OtterSec's runner so Solscan
# / Solana Explorer / Phantom / Jupiter flip the program to "Verified Build".
#
# Workflow:
#   scripts/build-verifiable.sh        # build the verifiable .so
#   scripts/upgrade-program.sh \
#     --yes --skip-build                # upgrade with that .so
#   scripts/verify-program.sh          # this script — compare + register
#
# Flags:
#   --remote   submit a remote verification job to OtterSec (writes the
#              verification PDA + costs ~0.005 SOL). Without this flag the
#              script just checks that the hashes match and exits.
#   --yes      skip the interactive confirm before paying for the remote job
#
# Reads `[package.metadata.solana].program-id` from
# `programs/ydelta/Cargo.toml`, so you don't have to pass the id manually.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [[ -f "$ROOT/.env" ]]; then
    set -a
    # shellcheck disable=SC1091
    source "$ROOT/.env"
    set +a
fi

remote=false
auto_confirm=false
while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --remote) remote=true; shift ;;
        --yes|-y) auto_confirm=true; shift ;;
        *) echo "Unknown arg: $1" >&2; exit 2 ;;
    esac
done

if ! command -v solana-verify >/dev/null 2>&1; then
    echo "Error: solana-verify not on PATH. Install with: cargo install solana-verify" >&2
    exit 1
fi

artifact="$ROOT/target/deploy/ydelta.so"
if [[ ! -f "$artifact" ]]; then
    echo "Error: $artifact missing. Run scripts/build-verifiable.sh first." >&2
    exit 1
fi

# Pull the program id out of programs/ydelta/Cargo.toml.
program_id=$(awk '
    /^\[package\.metadata\.solana\]/ {in_section=1; next}
    /^\[/ {in_section=0}
    in_section && /^program-id[[:space:]]*=/ {
        # Extract the first "..."-quoted token. gsub on .*" is greedy and
        # eats the whole string, so we use match + substr instead.
        if (match($0, /"[^"]*"/)) {
            print substr($0, RSTART + 1, RLENGTH - 2)
            exit
        }
    }
' "$ROOT/programs/ydelta/Cargo.toml")
if [[ -z "$program_id" ]]; then
    echo "Error: could not read [package.metadata.solana].program-id from" >&2
    echo "       programs/ydelta/Cargo.toml. Add it or pass --program-id." >&2
    exit 1
fi

# Pin OtterSec's remote rebuild to the same toolchain image as
# scripts/build-verifiable.sh (from [package.metadata.solana].solana-version).
# 2.2.x ships cargo 1.84, which can't parse edition-2024 deps; pin >= 2.3.0.
solana_version=$(awk '
    /^\[package\.metadata\.solana\]/ {in_section=1; next}
    /^\[/ {in_section=0}
    in_section && /^solana-version[[:space:]]*=/ {
        if (match($0, /"[^"]*"/)) { print substr($0, RSTART + 1, RLENGTH - 2); exit }
    }
' "$ROOT/programs/ydelta/Cargo.toml")
base_image_args=()
if [[ -n "$solana_version" ]]; then
    base_image_args=(--base-image "solanafoundation/solana-verifiable-build:$solana_version")
fi

cluster="${YDELTA_RPC_URL:-https://api.mainnet-beta.solana.com}"

local_hash=$(solana-verify get-executable-hash "$artifact")
on_chain_hash=$(solana-verify get-program-hash --url "$cluster" "$program_id" 2>&1 | tr -d ' ')

echo
echo "── Verifiable build comparison ───────────────────────────────"
echo "  program id:      $program_id"
echo "  cluster:         $cluster"
echo "  local hash:      $local_hash"
echo "  on-chain hash:   $on_chain_hash"
echo "──────────────────────────────────────────────────────────────"

if [[ "$local_hash" != "$on_chain_hash" ]]; then
    echo
    echo "Error: hashes do not match. The on-chain program was not deployed" >&2
    echo "       from this verifiable build. Either:" >&2
    echo "         - rebuild with scripts/build-verifiable.sh" >&2
    echo "         - upgrade with scripts/upgrade-program.sh --skip-build" >&2
    echo "         - then re-run this script." >&2
    exit 1
fi

echo "✓ hashes match — on-chain program matches the local verifiable build."

if [[ "$remote" == "false" ]]; then
    echo
    echo "Skipping remote registration (no --remote flag). Pass --remote to"
    echo "post the verification job to OtterSec's runner (writes the PDA,"
    echo "costs ~0.005 SOL)."
    exit 0
fi

git_remote=$(git -C "$ROOT" remote get-url origin 2>/dev/null || true)
if [[ -z "$git_remote" ]]; then
    echo "Error: this repo has no 'origin' remote. solana-verify needs a" >&2
    echo "       public git URL the OtterSec runner can clone." >&2
    exit 1
fi
# Normalise SSH URL → HTTPS so the runner doesn't need a deploy key.
case "$git_remote" in
    git@github.com:*) git_remote="https://github.com/${git_remote#git@github.com:}"; git_remote="${git_remote%.git}" ;;
    *.git) git_remote="${git_remote%.git}" ;;
esac

commit=$(git -C "$ROOT" rev-parse HEAD)
branch=$(git -C "$ROOT" rev-parse --abbrev-ref HEAD)

echo
echo "── Remote verification plan ──────────────────────────────────"
echo "  repository:      $git_remote"
echo "  branch:          $branch"
echo "  commit:          $commit"
echo "  library name:    ydelta"
echo "  mount path:      <workspace root> (Cargo.lock lives at top of repo)"
echo "──────────────────────────────────────────────────────────────"

if [[ "$auto_confirm" == "false" ]]; then
    read -r -p "Proceed with remote verification? [y/N] " reply
    [[ "$reply" =~ ^[Yy]$ ]] || { echo "aborted"; exit 1; }
fi

echo
echo "Submitting verification job (this writes the PDA + pays ~0.005 SOL fee)…"
solana-verify verify-from-repo \
    --remote \
    --skip-prompt \
    --url "$cluster" \
    --library-name ydelta \
    "${base_image_args[@]}" \
    --program-id "$program_id" \
    --commit-hash "$commit" \
    "$git_remote"

echo
echo "── Submitted ─────────────────────────────────────────────────"
echo "OtterSec will rebuild from the public commit in their own Docker,"
echo "check the hash, and stamp the verification PDA. Usually 5–15 min."
echo "Once green, Solscan + Solana Explorer flip the program to ✓ Verified."
