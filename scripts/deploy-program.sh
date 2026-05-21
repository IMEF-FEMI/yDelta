#!/usr/bin/env bash
# First-time deploy of the ydelta program. Writes `.local/protocol.json`
# on success so the TS bootstrap chain can pick up from there.
#
# Idempotent semantics:
#   - If `.local/protocol.json` already exists → abort. You're either
#     already done, or you want `upgrade-program.sh`.
#   - If the program is already on-chain (matching the local keypair
#     pubkey) → SKIP the actual `solana program deploy`, just write
#     `.local/protocol.json` from the existing on-chain state. This
#     covers the recovery case where a previous run deployed but lost
#     the local file before writing it.
#   - Otherwise → build (unless `--skip-build`) and deploy, then write.
#
# Usage:
#   scripts/deploy-program.sh                # build + deploy + write protocol.json
#   scripts/deploy-program.sh --skip-build   # reuse target/deploy/ydelta.so
#   scripts/deploy-program.sh --yes          # skip the interactive confirm
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Load .env so YDELTA_RPC_URL and friends are visible without the user
# having to `source .env` each shell session. `set -a` exports every
# variable defined in the sourced file; `set +a` restores.
if [[ -f "$ROOT/.env" ]]; then
    set -a
    # shellcheck disable=SC1091
    source "$ROOT/.env"
    set +a
fi

skip_build=false
auto_confirm=false

while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --skip-build) skip_build=true; shift ;;
        --yes|-y)     auto_confirm=true; shift ;;
        *) echo "Unknown arg: $1" >&2; exit 2 ;;
    esac
done

LOCAL_DIR="$ROOT/.local"
PROTOCOL_JSON="$LOCAL_DIR/protocol.json"

if [[ -f "$PROTOCOL_JSON" ]]; then
    echo "Error: $PROTOCOL_JSON already exists." >&2
    echo "       If you want to upgrade the deployed program, use" >&2
    echo "       \`scripts/upgrade-program.sh\` instead. If you want to" >&2
    echo "       re-record the deploy in protocol.json, delete the file" >&2
    echo "       first." >&2
    exit 1
fi

artifact="$ROOT/target/deploy/ydelta.so"
keypair="$ROOT/target/deploy/ydelta-keypair.json"

if [[ ! -f "$keypair" ]]; then
    echo "Error: program keypair missing at $keypair" >&2
    echo "       (This is the source of the program ID baked into declare_id!.)" >&2
    exit 1
fi
program_id=$(solana-keygen pubkey "$keypair")

# Resolve cluster + signer. YDELTA_RPC_URL takes precedence over the
# Solana CLI's `solana config get` URL so the shell scripts agree with
# the TS scripts about which cluster they're targeting. Signer path
# is taken from env first so deploy shells and TS scripts use the same
# authority without relying on global Solana CLI state.
config_out=$(solana config get)
cli_cluster=$(echo "$config_out" | awk -F': ' '/RPC URL/ {print $2}' | xargs)
cluster="${YDELTA_RPC_URL:-$cli_cluster}"
cli_signer_path=$(echo "$config_out" | awk -F': ' '/Keypair Path/ {print $2}' | xargs)
signer_path="${YDELTA_DEPLOYER_KEYPAIR_PATH:-${YDELTA_KEYPAIR_PATH:-$cli_signer_path}}"
signer_pk=$(solana-keygen pubkey "$signer_path")
# Common `--url` flag passed to every solana subcommand below.
SOL_URL_FLAGS=(--url "$cluster")
SOL_SIGNER_FLAGS=(--keypair "$signer_path")
now_unix=$(date +%s)

mkdir -p "$LOCAL_DIR"

# ────────────────────── recovery path: already on-chain ──────────────────────

program_show=$(solana "${SOL_URL_FLAGS[@]}" program show "$program_id" 2>/dev/null || true)
if [[ -n "$program_show" ]]; then
    on_chain_auth=$(echo "$program_show" | awk -F': ' '/Authority/ {print $2}' | xargs)
    program_data_pda=$(echo "$program_show" | awk -F': ' '/ProgramData Address/ {print $2}' | xargs)
    echo "Program $program_id is already deployed on $cluster."
    echo "  authority         = ${on_chain_auth:-<unknown>}"
    echo "  program data pda  = $program_data_pda"
    echo "Skipping deploy; writing $PROTOCOL_JSON to record existing state."
    cat > "$PROTOCOL_JSON" <<EOF
{
  "programId": "$program_id",
  "programDataPda": "$program_data_pda",
  "deployer": "${on_chain_auth:-$signer_pk}",
  "deployedAtUnix": $now_unix,
  "recoveredFromOnChain": true
}
EOF
    echo "wrote $PROTOCOL_JSON"
    exit 0
fi

# ────────────────────── fresh deploy ──────────────────────

if [[ "$skip_build" == "false" ]]; then
    bash "$ROOT/scripts/build-program.sh"
elif [[ ! -f "$artifact" ]]; then
    echo "Error: --skip-build set but $artifact does not exist" >&2
    exit 1
fi

if [[ ! -f "$artifact" ]]; then
    echo "Error: expected $artifact to exist after build" >&2
    exit 1
fi
artifact_bytes=$(wc -c < "$artifact" | tr -d ' ')

echo
echo "── Deploy plan ──────────────────────────────────────────────"
echo "  cluster:     $cluster"
echo "  program id:  $program_id"
echo "  artifact:    $artifact ($artifact_bytes bytes)"
echo "  signer:      $signer_pk  ($signer_path)"
echo "─────────────────────────────────────────────────────────────"

if [[ "$auto_confirm" == "false" ]]; then
    read -r -p "Proceed with deploy? [y/N] " reply
    [[ "$reply" =~ ^[Yy]$ ]] || { echo "aborted"; exit 1; }
fi

# Capture the `solana program deploy` output so we can extract the
# ProgramData address it prints. The CLI writes "Program Id: <pk>" on
# success but not the ProgramData address — for that we follow up with
# `solana program show` after the deploy lands.
echo "Running: solana ${SOL_URL_FLAGS[*]} program deploy --program-id $keypair $artifact"
solana "${SOL_URL_FLAGS[@]}" "${SOL_SIGNER_FLAGS[@]}" program deploy --program-id "$keypair" "$artifact"

# `solana program show` is the authoritative source for the ProgramData
# PDA. Retry briefly in case of RPC propagation lag.
program_data_pda=""
for i in 1 2 3 4 5; do
    show_out=$(solana "${SOL_URL_FLAGS[@]}" program show "$program_id" 2>/dev/null || true)
    program_data_pda=$(echo "$show_out" | awk -F': ' '/ProgramData Address/ {print $2}' | xargs)
    if [[ -n "$program_data_pda" ]]; then break; fi
    sleep 1
done

if [[ -z "$program_data_pda" ]]; then
    echo "Error: deploy succeeded but couldn't read ProgramData address." >&2
    echo "       Try \`solana program show $program_id\` manually and" >&2
    echo "       write $PROTOCOL_JSON by hand." >&2
    exit 1
fi

cat > "$PROTOCOL_JSON" <<EOF
{
  "programId": "$program_id",
  "programDataPda": "$program_data_pda",
  "deployer": "$signer_pk",
  "deployedAtUnix": $now_unix
}
EOF

echo
echo "── Deploy complete ──────────────────────────────────────────"
echo "  program id:        $program_id"
echo "  program data pda:  $program_data_pda"
echo "  wrote:             $PROTOCOL_JSON"
echo "─────────────────────────────────────────────────────────────"
echo "Next: yarn tsx ts/scripts/bootstrap.ts"
