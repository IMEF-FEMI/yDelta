# yDelta — mainnet deployment & markets bootstrapping runbook

yDelta deploys to **mainnet only** — it's a layer on marginfi v0.1.8, whose banks
live on mainnet, so there is no other target. This runbook sequences the existing
`scripts/*.sh` and `ts/scripts/*.ts` tooling for a (re)deploy/upgrade and the first
markets. It invents no new tooling.

## Current mainnet deployment

| | |
|---|---|
| Program ID | `HfYCWUgFuUbuzZTeQAAGzVkXw2uFM51QRoDfhFV8vyCj` |
| ProgramData | `8N66P2MSDcWUtLpaWMVvBhgEkDziNbKEcZQ7882C6PvJ` |
| Upgrade authority | `EokNERAAaWorYsqMqgNhVHbyNmQoGDzEuVpbbEsCtK3a` (upgradeable) |
| Verifiable hash | `10c746b0cef697fbe15a7359be5597e86448d5e25d0e56a8ac913c92178ae7e9` |
| IDL metadata PDA | `71duU5FLye26WuuAr7FvBvhmzycyUXAkjfaVXd9qCt6w` |
| Keypair | `target/deploy/ydelta-keypair.json` (retired keys under `target/deploy/closed/`) |

---

## 1. Prerequisites

**`.env`** (copy from `.env.example`):
- `YDELTA_RPC_URL` — your **mainnet** RPC. Every script defaults to
  `https://api.mainnet-beta.solana.com` if unset; nothing reads the Solana CLI's
  configured cluster.
- `YDELTA_DEPLOYER_KEYPAIR_PATH` — deployer / **upgrade authority** (needs ~7 SOL for
  a fresh ~1 MB program deploy; upgrades cost only fees unless the binary grows).
- `YDELTA_UPGRADE_AUTHORITY_PUBKEY` — asserted to match the signer on upgrades.
- `YDELTA_DEPOSITOR_KEYPAIR_B58`, `YDELTA_BORROWER_KEYPAIR_B58` — LP / borrower actors.
- `YDELTA_SWITCHBOARD_CROSSBAR`, `YDELTA_SWITCHBOARD_GATEWAY` — to crank the
  Switchboard-Pull oracles (SOL, JitoSOL).

**Keypairs & roles** — decide whether these are one key or split, and fund them:

| Role | Used by | Notes |
|---|---|---|
| Deployer / upgrade authority | `YDELTA_DEPLOYER_KEYPAIR_PATH` | program rent + upgrades |
| Protocol admin | signer of `create-global-config` | gates Pool sub-vaults, fee config, global pause |
| Curator (`curator-main`) | signer of `create-curators` / `setup-curator-profiles` | sub-vault policy |
| LP depositor | `YDELTA_DEPOSITOR_KEYPAIR_B58` | seeds liquidity |
| Borrower | `YDELTA_BORROWER_KEYPAIR_B58` | smoke test |

> By default every bootstrap step is signed by the deployer key, collapsing all
> roles onto it. Decide any split **before** bootstrapping; consider a multisig for
> the upgrade authority after go-live.

**`.local/` inputs** (copy from `.local.example/`, gitignored):
- `mainnet-banks.json` — **verify every pubkey against `solana account <bank>`** before
  use (the file says so). Sole source of bank/oracle/vault/group pubkeys; no discovery API.
- `curators-input.json`, `vault-input.json`, `setup-curator-profiles-input.json`,
  `markets-input.json` — review the defaults (Section 3).

## 2. Deploy / upgrade

1. **Verifiable build:** `scripts/build-verifiable.sh` (deterministic Docker build →
   byte-stable `target/deploy/ydelta.so` + sha256). Always deploy this, never the
   host build, so the program can earn the Verified-Build badge.
2. **Fresh deploy:** `scripts/deploy-program.sh --skip-build` — reviews a deploy plan
   (cluster, program id, artifact bytes, signer), deploys the verifiable `.so`,
   writes `.local/protocol.json`. Idempotent: aborts if `protocol.json` exists,
   recovers if the program is already on-chain. (Default `max-len` = 1× program size;
   a future upgrade that grows the binary needs `solana program extend` first —
   `upgrade-program.sh` checks and tells you.)
3. **Upgrade (later):** `scripts/upgrade-program.sh --skip-build` — checks signer ==
   upgrade authority and new `.so` ≤ on-chain data length; `--buffer <pk>` resumes a
   failed deploy.
4. **IDL on-chain:** `scripts/upload-idl.sh` (so Explorer/Solscan decode instruction
   names + args). Signer must be the upgrade authority.
5. **Register verified build:** `scripts/verify-program.sh --remote --yes` (rebuilds
   from the pushed git commit on OtterSec's runner, stamps the verification PDA).

## 3. Markets bootstrapping

`yarn bootstrap` runs five **idempotent** steps (each skips when its `.local/*.json`
output exists), then prints a summary. With the default inputs:

| # | Step | Output | Creates |
|---|---|---|---|
| 1 | `create-global-config` | `global-config.json` | protocol global config (admin = signer) |
| 2 | `create-curators` | `curators.json` | `curator-main` |
| 3 | `create-vault` | `vaults.json` | **USDC vault** on bank `2s37…` |
| 4 | `setup-curator-profiles` | `risk-profiles.json` | **3 sub-vaults** in the USDC vault |
| 5 | `init-market` | `markets.json` | **USDC/SOL** + **USDC/JitoSOL** markets |

Default sub-vault profiles (`setup-curator-profiles-input.json`) — review first:

| Profile | max LTV | max term | liquidation LTV |
|---|---|---|---|
| conservative | 65% | 7 days | ≥ max_ltv + 2% |
| balanced | 75% | 30 days | ≥ 77% |
| aggressive | 85% | 90 days | ≥ 87% |

> Confirm the input sets `liquidation_ltv_bps` (≥ `max_ltv + MIN_LIQ_GAP_BPS` = 200)
> and a `spread_bps`, or the create rejects.

**After bootstrap — lookup tables:** `yarn create-market-lut` for each market.
Order/match/liquidate txns touch many accounts; the LUT keeps them under the tx-size
limit. Do this before any real order flow.

**Markets are live at creation** — no paused handshake. A market trades the instant a
curator posts an ask into a funded sub-vault.

Verify: the bootstrap summary shows `global-config: OK`, `vaults: 1`, `profiles: 3`,
`markets: 2`.

## 4. Liquidity & go-live

1. **Seed liquidity:** LP `yarn vault:deposit` into a sub-vault (earns marginfi yield
   immediately).
2. **Quote:** curator `yarn vault:place-ask` per market — priced `bank lending APR + spread`.
3. **Stand up the crankers** (the `../crankers` keeper service): re-pin its `ydelta`
   git dep to a `main` commit so it resolves the new program id via `ydelta::id()`,
   set its `.env`, and run the loops — **oracle cranks first** (Switchboard SOL/JitoSOL
   must be fresh or the LTV gate rejects), then `match-crank`, `process-matched-loan`,
   `claim-repayment`, `claim-curator-fee`, `settle`, `liquidate`.
4. **Smoke a real borrow** with small size; watch one full lifecycle; then open up.

## 5. Operations & safety

- **Keepers run continuously** — matching, promotion, claims, settlement, liquidation,
  oracle freshness all need the crankers.
- **Pause switches:** `yarn set-market-pause` (per market) and the global pause for
  emergencies.
- **Oracles:** USDC is Pyth-push (cranked by Pyth; our crank is a backstop); SOL &
  JitoSOL are Switchboard-pull (we **must** crank them).
- **Upgrades:** `build-verifiable.sh` → `upgrade-program.sh --skip-build` →
  `verify-program.sh` → `upload-idl.sh` if the IDL changed.
- **Rollback:** pause the affected scope; ship a fix via the upgrade authority.

## 6. Pre-flight checklist

- [ ] `.env` `YDELTA_RPC_URL` = your mainnet RPC; deployer funded
- [ ] Upgrade-authority / admin / curator key split decided
- [ ] `.local/mainnet-banks.json` pubkeys verified against `solana account …`
- [ ] Sub-vault profiles reviewed (LTV / term / liquidation gap / spread)
- [ ] Verifiable build produced and deployed
- [ ] `protocol.json` written; IDL uploaded; verified build registered
- [ ] Market LUTs created
- [ ] Liquidity seeded; curator quoting
- [ ] Crankers re-pinned, configured, and running (oracles first)
- [ ] Small-size smoke borrow completed end-to-end
