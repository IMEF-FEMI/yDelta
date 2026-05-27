#!/usr/bin/env bash
# Regenerate the Anchor-style IDL from our Shank source-of-truth, then
# publish it on-chain via the solana-program-metadata PDA so explorers
# (Solana Explorer, Solscan, etc.) can decode instruction names + args
# for the deployed ydelta program.
#
# Run this any time `ts/idl/ydelta.json` changes (new instruction, arg
# rename, etc.). The signer must be the program's upgrade authority.
#
# Usage:
#   scripts/upload-idl.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [[ -f "$ROOT/.env" ]]; then
    set -a
    # shellcheck disable=SC1091
    source "$ROOT/.env"
    set +a
fi

program_id=$(awk '
    /^\[package\.metadata\.solana\]/ {in_section=1; next}
    /^\[/ {in_section=0}
    in_section && /^program-id[[:space:]]*=/ {
        if (match($0, /"[^"]*"/)) {
            print substr($0, RSTART + 1, RLENGTH - 2)
            exit
        }
    }
' "$ROOT/programs/ydelta/Cargo.toml")
if [[ -z "$program_id" ]]; then
    echo "Error: missing [package.metadata.solana].program-id in programs/ydelta/Cargo.toml" >&2
    exit 1
fi

# Use the keyed RPC if provided — the public mainnet-beta endpoint
# rate-limits the ~5-chunk buffer upload halfway through.
rpc="${YDELTA_RPC_URL:-https://api.mainnet-beta.solana.com}"
keypair="${YDELTA_DEPLOYER_KEYPAIR_PATH:-${YDELTA_KEYPAIR_PATH:-$HOME/.config/solana/id.json}}"
signer_pk=$(solana-keygen pubkey "$keypair")

echo
echo "── IDL upload plan ───────────────────────────────────────────"
echo "  program id:      $program_id"
echo "  cluster:         $rpc"
echo "  authority:       $signer_pk"
echo "──────────────────────────────────────────────────────────────"
echo "Step 1: regenerate Anchor IDL from Shank source"
yarn tsx ts/scripts/convert-idl-to-anchor.ts
echo
echo "Step 2: upload to the canonical program-metadata PDA"
echo "(the seed 'idl' + program addr derive the address Solana Explorer reads."
echo "Buffer write costs ~0.05 SOL the first time; subsequent updates reuse"
echo "the existing PDA so cost is just tx fees.)"
npx --yes @solana-program/program-metadata write idl "$program_id" \
    ts/idl/ydelta-anchor.json \
    --keypair "$keypair" \
    --rpc "$rpc"

echo
echo "── Done ──────────────────────────────────────────────────────"
echo "Confirm with:"
echo "  npx --yes @solana-program/program-metadata fetch idl $program_id \\"
echo "      --rpc '$rpc'"
echo
echo "Allow a few minutes for explorer caches to refresh:"
echo "  https://explorer.solana.com/address/$program_id"
echo "  https://solscan.io/account/$program_id"
