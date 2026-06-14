# yDelta Deployment And Market Bootstrap Runbook

This runbook covers program deployment, upgrades, initial protocol setup, market
creation, and operational handoff using the scripts in this repository.

The supplied configuration targets marginfi mainnet banks. Deployment and action
scripts default to `https://api.mainnet-beta.solana.com` when
`YDELTA_RPC_URL` is unset. Always verify the selected RPC, signer, program
identity, bank accounts, and transaction plan before broadcasting.

## Repository-Pinned Program Identity

The repository currently pins this program ID:

```text
HfYCWUgFuUbuzZTeQAAGzVkXw2uFM51QRoDfhFV8vyCj
```

It is declared in:

- `programs/ydelta/src/lib.rs`
- `programs/ydelta/Cargo.toml`
- `ts/src/constants.ts`
- `Anchor.toml`

ProgramData, upgrade authority, executable hash, IDL metadata, and verification
status are live on-chain properties. Check them immediately before a deployment
or upgrade instead of relying on values recorded in documentation:

```bash
solana program show \
  --url "$YDELTA_RPC_URL" \
  HfYCWUgFuUbuzZTeQAAGzVkXw2uFM51QRoDfhFV8vyCj

scripts/verify-program.sh
```

For reference, the program's current verified build hash is below — always
reconfirm it on-chain with the commands above rather than trusting this value:

```text
9d68f6c9232fc7ee0d558164f8b681a2f3cc4167c7ff2b79c2240d5f9d9820a7
```

The program keypair expected by the deployment scripts is:

```text
target/deploy/ydelta-keypair.json
```

Its public key must match the program ID pinned throughout the repository.

## 1. Prepare The Operator Environment

Install the build and deployment prerequisites:

- Rust and Cargo
- Solana/Agave CLI with `cargo build-sbf`
- Node.js and Yarn `4.14.1`
- Docker and `solana-verify` for verifiable builds

Copy and review the environment template:

```bash
cp .env.example .env
```

Important environment variables:

| Variable | Purpose |
| --- | --- |
| `YDELTA_RPC_URL` | RPC used by deployment and TypeScript action scripts. Defaults to public mainnet-beta. |
| `YDELTA_DEPLOYER_KEYPAIR_PATH` | Primary signer used for deployment, upgrades, bootstrap administration, fees, and keeper scripts. |
| `YDELTA_UPGRADE_AUTHORITY_PUBKEY` | Optional assertion checked by the upgrade script. |
| `YDELTA_DEPOSITOR_KEYPAIR_B58` | Depositor signer used by vault deposit and withdrawal scripts. |
| `YDELTA_BORROWER_KEYPAIR_B58` | Borrower signer used by borrower and market-seat scripts. |
| `YDELTA_CONFIRM` | Required by action scripts when their RPC endpoint string is recognized as mainnet. |
| `YDELTA_HERMES_URL` | Optional Pyth Hermes endpoint override. |
| `YDELTA_SWITCHBOARD_CROSSBAR` | Optional Switchboard Crossbar override. |
| `YDELTA_SWITCHBOARD_GATEWAY` | Optional pinned Switchboard gateway. |
| `YDELTA_LOCAL_DIR` | Optional replacement for the default `.local/` state directory. |

The mainnet confirmation guard checks whether the configured RPC URL contains
`mainnet`. A custom mainnet RPC URL may not satisfy that string check. Treat
`YDELTA_RPC_URL` as the authoritative cluster selection and verify it manually
before every production operation.

### Roles

The program distinguishes several authorities:

| Role | Responsibility |
| --- | --- |
| Upgrade authority | Deploys program upgrades and uploads IDL metadata. |
| Protocol admin | Creates markets and Pool sub-vaults and controls global administration. |
| Market admin | Controls a market's fee configuration, pause state, and admin transfer. |
| Global Vault admin | Controls vault administration and vault-level pause or lifecycle actions. |
| Curator | Manages a Pool or Private sub-vault's strategy and asks. |
| Depositor | Supplies or withdraws capital from a sub-vault. |
| Borrower | Places bids, manages resting bids, repays, and converts P2Pool debt. |
| Keeper | Performs permissionless matching, promotion, claims, settlement, and liquidation operations. |

The first `CreateGlobalConfig` signer must equal the deployed program's upgrade
authority and becomes the initial protocol admin. During the supplied bootstrap,
the primary signer also becomes the initial admin of each Global Vault and
market it creates. Those roles can later be transferred through their respective
two-step admin-transfer instructions.

The `create-curators` script only generates curator keypairs and stores them in
`.local/curators.json`; it does not create an on-chain curator account.

Decide authority ownership and transfer plans before funding production
accounts. Use appropriate multisig or operational controls for privileged keys.

### Local State And Secrets

Copy the required input templates:

```bash
mkdir -p .local
cp .local.example/mainnet-banks.json .local/
cp .local.example/curators-input.json .local/
cp .local.example/vault-input.json .local/
cp .local.example/setup-curator-sub-vaults-input.json .local/
cp .local.example/markets-input.json .local/
```

Review [`.local.example/README.md`](../.local.example/README.md) for all action
script inputs.

The `.local/` directory can contain sensitive material, including generated
curator secret keys and market account secret keys. It is gitignored, but it
must still be backed up and protected as secret operator state.

The supplied `mainnet-banks.json`, `vault-input.json`, and `markets-input.json`
contain operator-selected marginfi addresses. The scripts read relevant bank
accounts on-chain and validate important fields, but operators must still verify
the selected banks, group, mints, oracle configuration, and liquidity vaults
before signing.

## 2. Pre-Deployment Verification

Run the repository checks before producing the deployment artifact:

```bash
./scripts/test.sh
./scripts/test.sh --sbf
yarn test
yarn typecheck
yarn lint
```

Confirm the program keypair matches the pinned program ID:

```bash
solana-keygen pubkey target/deploy/ydelta-keypair.json
```

Confirm the intended cluster and signer:

```bash
solana config get
solana-keygen pubkey "$YDELTA_DEPLOYER_KEYPAIR_PATH"
```

## 3. Build And Deploy

### Verifiable Build

Create the artifact intended for deployment:

```bash
scripts/build-verifiable.sh
```

This uses `solana-verify` and Docker, writes
`target/deploy/ydelta.so`, and records its executable hash in
`target/deploy/ydelta.so.sha256`.

### First Deployment

```bash
scripts/deploy-program.sh --skip-build
```

The script:

- derives the program ID from `target/deploy/ydelta-keypair.json`
- displays the cluster, artifact, program ID, and signer
- asks for confirmation unless `--yes` is supplied
- deploys the program when it is not already present
- writes `.local/protocol.json`

If `.local/protocol.json` already exists, the script aborts. If the program is
already deployed but the local file is absent, it records the observed
ProgramData address and authority without redeploying.

### Upgrade

```bash
scripts/upgrade-program.sh --skip-build
```

The upgrade script checks the current on-chain upgrade authority when it can read
it and rejects artifacts larger than the current program data allocation. If the
artifact is too large, follow the script's `solana program extend` instruction
before retrying.

Use `--buffer <PUBKEY>` only to resume an existing deployment buffer flow.

### Verify And Publish Metadata

Compare the local verifiable build with the deployed executable:

```bash
scripts/verify-program.sh
```

Submit remote verification only after the matching source commit is publicly
available:

```bash
scripts/verify-program.sh --remote
```

Regenerate and upload the Anchor-style IDL metadata:

```bash
scripts/upload-idl.sh
```

The IDL upload signer must be authorized for the deployed program. Verify the
result using the fetch command printed by the script.

## 4. Bootstrap Protocol State

Bootstrap assumes `.local/protocol.json` already exists:

```bash
yarn bootstrap
```

For the first bootstrap, the primary signer must still be the program upgrade
authority so it can create the singleton Global Config. After creation, protocol
administration is governed by the protocol-admin role stored in that account.

It runs these steps:

| Step | Script | Result |
| --- | --- | --- |
| 1 | `create-global-config` | Creates or records the singleton Global Config. |
| 2 | `create-curators` | Generates local curator keypairs. No on-chain account is created. |
| 3 | `create-vault` | Creates or records the configured bank-keyed Global Vault. |
| 4 | `setup-curator-sub-vaults` | Creates configured Pool sub-vaults. |
| 5 | `init-market` | Allocates and creates configured markets. |

The scripts use `.local/*.json` files as resumable operator state. Some steps
also recover or validate on-chain state, while others skip entries already
recorded locally. Local output files are therefore not a substitute for an
independent on-chain verification after bootstrap.

With the current example inputs, bootstrap targets:

- one USDC Global Vault
- three Pool sub-vaults
- USDC/SOL and USDC/JitoSOL markets

The example Pool sub-vault parameters are:

| Row | Spread | Maximum LTV | Liquidation LTV | Maximum Term | Curator Fee |
| --- | --- | --- | --- | --- | --- |
| 1 | 100 bps | 65% | 72% | 7 days | 1,000 bps |
| 2 | 175 bps | 75% | 82% | 30 days | 1,000 bps |
| 3 | 300 bps | 85% | 90% | 90 days | 1,000 bps |

These are example values, not recommended production risk parameters. Review
them before use. The program requires:

```text
liquidation_ltv_bps >= max_ltv_bps + MIN_LIQ_GAP_BPS
```

where the current minimum liquidation gap is 200 bps.

### Address Lookup Tables

Create a lookup table for each recorded market:

```bash
yarn create-market-lut
```

The script adds each table address to `.local/markets.json`. These tables are
used by scripts that bundle account-heavy health checks and Switchboard updates
into versioned transactions. A newly created lookup table becomes usable after
the activation delay reported by the script.

## 5. Seed Liquidity And Open Markets

Markets are created unpaused, but usable order flow still requires configured
sub-vaults, deposited liquidity, asks, borrowers, and fresh acceptable oracle
data.

For each intended market:

1. Fund the configured depositor and curator or fee-payer accounts.
2. Configure `.local/vault-deposit-input.json` and deposit into a sub-vault:

   ```bash
   yarn vault:deposit
   ```

3. Configure `.local/vault-place-ask-input.json` and place a sub-vault ask:

   ```bash
   yarn vault:place-ask
   ```

4. Confirm the ask, sub-vault balances, and market configuration on-chain.
5. Run a small borrower lifecycle before increasing exposure.

The ask rate is derived from the live marginfi lending APR plus the sub-vault's
configured spread. Ask capacity is read from the sub-vault's available balance
at match time.

## 6. Oracle And Keeper Operations

The repository includes one-shot action scripts, not a continuously supervised
keeper service. Production operators must schedule, monitor, and alert on the
required operations.

Relevant commands include:

```bash
yarn crank-oracle
yarn match-crank
yarn crank-matched-loan
yarn claim-repayment
yarn claim-curator-fee
yarn settle
yarn liquidate
```

Oracle handling depends on the bank configuration and action script:

- stale Pyth-Push feeds can be refreshed through Hermes when the required feed
  ID is available
- Switchboard-Pull updates may be sent separately or bundled into supported
  versioned transactions
- yDelta instructions that perform LTV or marginfi health checks reject
  unacceptable oracle readings

Do not assume every script refreshes every oracle in the same way. Test the
complete transaction path for each configured market and monitor oracle age,
confidence, and update failures.

## 7. Pause And Incident Response

The on-chain program supports global, market, and Global Vault pause scopes.
This repository currently provides a ready-to-run TypeScript action script only
for market pause:

```bash
yarn set-market-pause
```

The SDK exports instruction builders for global and vault pause operations, but
operators must provide their own approved transaction flow for those scopes.
Verify that emergency signers can execute every required pause transaction
before opening markets.

For an incident:

1. Pause the narrowest appropriate scope.
2. Confirm the pause on-chain.
3. Preserve logs and `.local/tx-log.json`.
4. Diagnose and test the remediation.
5. Build, deploy, and verify an upgrade if required.
6. Resume only after validating protocol and keeper state.

## 8. Deployment Checklist

- [ ] Program ID is consistent across Rust, Cargo metadata, TypeScript, and Anchor configuration.
- [ ] RPC endpoint and cluster were manually verified.
- [ ] Upgrade authority and operational role ownership were verified.
- [ ] `.local/` inputs were reviewed and secret-bearing outputs are protected.
- [ ] Selected marginfi banks, groups, mints, liquidity vaults, and oracle accounts were verified.
- [ ] Native, SBPF, TypeScript, typecheck, and lint checks passed.
- [ ] Verifiable artifact was built and its hash recorded.
- [ ] Program deployment or upgrade was confirmed on-chain.
- [ ] Executable hash was compared and remote verification submitted if required.
- [ ] IDL metadata was uploaded and fetched successfully.
- [ ] Bootstrap outputs were independently checked against on-chain accounts.
- [ ] Pool sub-vault risk parameters and curator fees were approved.
- [ ] Market lookup tables were created and activated.
- [ ] Deposits, asks, oracle updates, and a small borrower lifecycle were tested.
- [ ] Keeper scheduling, monitoring, and alerting are operational.
- [ ] Global, market, and vault pause procedures were tested.
