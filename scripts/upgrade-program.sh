#!/usr/bin/env bash
# Upgrade the deployed ydelta program with the latest local build.
# Builds via `scripts/build-program.sh`, then runs
# `solana program deploy --program-id <keypair> <so>` against whichever
# cluster `solana config get` is currently pointed at.
#
# Usage:
#   scripts/upgrade-program.sh                # build + deploy to current cluster
#   scripts/upgrade-program.sh --skip-build   # reuse target/deploy/ydelta.so
#   scripts/upgrade-program.sh --buffer <pk>  # resume a previously-failed deploy
#   scripts/upgrade-program.sh --yes          # skip the interactive confirm
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Load .env so YDELTA_RPC_URL drives the solana subcommands below.
if [[ -f "$ROOT/.env" ]]; then
    set -a
    # shellcheck disable=SC1091
    source "$ROOT/.env"
    set +a
fi

skip_build=false
auto_confirm=false
buffer=""

while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --skip-build) skip_build=true; shift ;;
        --yes|-y)     auto_confirm=true; shift ;;
        --buffer)     buffer="${2:?--buffer requires a pubkey}"; shift 2 ;;
        *) echo "Unknown arg: $1" >&2; exit 2 ;;
    esac
done

artifact="$ROOT/target/deploy/ydelta.so"
keypair="$ROOT/target/deploy/ydelta-keypair.json"

if [[ "$skip_build" == "false" ]]; then
    bash "$ROOT/scripts/build-program.sh"
elif [[ ! -f "$artifact" ]]; then
    echo "Error: --skip-build set but $artifact does not exist" >&2
    exit 1
fi

if [[ ! -f "$keypair" ]]; then
    echo "Error: program keypair missing at $keypair" >&2
    exit 1
fi

# `solana config get` output has trailing whitespace on each line, so we
# pipe through `xargs` to trim — otherwise the path/url is rejected by
# downstream subcommands as an "unrecognized signer source".
program_id=$(solana-keygen pubkey "$keypair")
config_out=$(solana config get)
cli_cluster=$(echo "$config_out" | awk -F': ' '/RPC URL/ {print $2}' | xargs)
# YDELTA_RPC_URL takes precedence so the upgrade and the TS scripts
# target the same cluster.
cluster="${YDELTA_RPC_URL:-$cli_cluster}"
SOL_URL_FLAGS=(--url "$cluster")
cli_signer=$(echo "$config_out" | awk -F': ' '/Keypair Path/ {print $2}' | xargs)
signer="${YDELTA_DEPLOYER_KEYPAIR_PATH:-${YDELTA_KEYPAIR_PATH:-$cli_signer}}"
SOL_SIGNER_FLAGS=(--keypair "$signer")
signer_pk=$(solana-keygen pubkey "$signer")
expected_upgrade_auth="${YDELTA_UPGRADE_AUTHORITY_PUBKEY:-}"
artifact_bytes=$(wc -c < "$artifact" | tr -d ' ')

# Pull the on-chain authority + current data length so we can sanity-check
# before paying for a tx that will obviously fail.
program_show=$(solana "${SOL_URL_FLAGS[@]}" program show "$program_id" 2>/dev/null || true)
on_chain_auth=$(echo "$program_show" | awk -F': ' '/Authority/ {print $2}' | xargs)
on_chain_len=$(echo "$program_show" | awk -F': ' '/Data Length/ {print $2}' | awk '{print $1}')

echo
echo "── Upgrade plan ──────────────────────────────────────────────"
echo "  cluster:           $cluster"
echo "  program id:        $program_id"
echo "  artifact:          $artifact ($artifact_bytes bytes)"
echo "  signer:            $signer_pk  ($signer)"
echo "  on-chain auth:     ${on_chain_auth:-<unknown>}"
echo "  on-chain data len: ${on_chain_len:-<unknown>} bytes"
[[ -n "$buffer" ]] && echo "  resume buffer:     $buffer"
echo "──────────────────────────────────────────────────────────────"

if [[ -n "$on_chain_auth" && "$on_chain_auth" != "$signer_pk" ]]; then
    echo "Error: current signer ($signer_pk) is not the program's upgrade authority ($on_chain_auth)." >&2
    echo "       Either switch keypairs (\`solana config set --keypair <path>\`) or pass" >&2
    echo "       \`--upgrade-authority <path>\` to \`solana program deploy\` directly." >&2
    exit 1
fi

if [[ -n "$expected_upgrade_auth" && "$signer_pk" != "$expected_upgrade_auth" ]]; then
    echo "Error: current signer ($signer_pk) does not match YDELTA_UPGRADE_AUTHORITY_PUBKEY ($expected_upgrade_auth)." >&2
    exit 1
fi

if [[ -n "$expected_upgrade_auth" && -n "$on_chain_auth" && "$on_chain_auth" != "$expected_upgrade_auth" ]]; then
    echo "Error: on-chain upgrade authority ($on_chain_auth) does not match YDELTA_UPGRADE_AUTHORITY_PUBKEY ($expected_upgrade_auth)." >&2
    exit 1
fi

if [[ -n "$on_chain_len" && "$artifact_bytes" -gt "$on_chain_len" ]]; then
    extra=$((artifact_bytes - on_chain_len))
    echo "Error: new .so ($artifact_bytes b) exceeds on-chain data length ($on_chain_len b)." >&2
    echo "       Run first:  solana program extend $program_id $extra" >&2
    exit 1
fi

if [[ "$auto_confirm" == "false" ]]; then
    read -r -p "Proceed with deploy? [y/N] " reply
    [[ "$reply" =~ ^[Yy]$ ]] || { echo "aborted"; exit 1; }
fi

cmd=(solana "${SOL_URL_FLAGS[@]}" "${SOL_SIGNER_FLAGS[@]}" program deploy --program-id "$keypair")
[[ -n "$buffer" ]] && cmd+=(--buffer "$buffer")
cmd+=("$artifact")

echo "Running: ${cmd[*]}"
"${cmd[@]}"
