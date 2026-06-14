# yDelta — deployment & markets bootstrapping runbook

End-to-end plan for shipping the program and standing up the first markets,
built around the existing `scripts/*.sh` and `ts/scripts/*.ts` tooling. Nothing
here invents new tooling — it sequences what's already in the repo.

---

## 0. What we're shipping

- **Program:** `ydelta`, new id **`HfYCWUgFuUbuzZTeQAAGzVkXw2uFM51QRoDfhFV8vyCj`**.
- **Keypair / upgrade-authority source:** `target/deploy/ydelta-keypair.json` (the
  old `Ar38…` key is retired under `target/deploy/closed/`).
- **State going in:** branch `v1-followups`, tests green — **150 lib + 158 SBF
  e2e + 145 TS**.

## 1. The one decision that shapes everything: cluster + the marginfi dependency

yDelta is a layer **on top of marginfi v0.1.8 banks**. The bank registry the
bootstrap reads (`.local.example/mainnet-banks.json`) and the default markets
(`markets-input.json`) are **mainnet**: group `4qp6Fx6tnZkY5Wropq9wUYgtFxXKwE6viZxFHg3rdAG8`,
marginfi program `MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA`, banks for USDC /
SOL / JitoSOL. **marginfi does not run a devnet deployment we can use**, so there
is no plain-devnet path — markets can only be bootstrapped against:

- **(A) a mainnet-fork localnet** that clones marginfi's program + group + banks +
  vaults + oracles + mints — full dress rehearsal, $0, zero mainnet risk; or
- **(B) mainnet** — the real thing.

**Recommended: do A, then B.** Validate the new program id + the entire bootstrap +
a full loan lifecycle on a fork first, then deploy to mainnet.

## 2. Prerequisites (once)

**Keypairs & roles** — decide whether these are one key or split, and fund them:

| Role | Used by | Notes |
|---|---|---|
| Deployer / **upgrade authority** | `YDELTA_DEPLOYER_KEYPAIR_PATH` | needs ~4–5 SOL on mainnet for program rent |
| **Protocol admin** | the signer of `create-global-config` | gates Pool sub-vault creation, fee config, global pause |
| **Curator** (`curator-main`) | the signer of `create-curators` / `setup-curator-profiles` | owns sub-vault policy (spread, LTV, term caps) |
| LP depositor | `YDELTA_DEPOSITOR_KEYPAIR_B58` | seeds liquidity / smoke test |
| Borrower | `YDELTA_BORROWER_KEYPAIR_B58` | smoke test |

> By default every bootstrap step is signed by the `.env` keypair, so all the
> admin/curator roles collapse onto the deployer unless you split them. Decide
> this **before** Phase B — moving the protocol admin / curator later is extra
> work. For a launch: keep them on a key you control; consider a multisig for the
> upgrade authority after go-live.

**`.env`** (copy from `.env.example`):
- `YDELTA_RPC_URL` — the target cluster (fork → `http://127.0.0.1:8899`; mainnet → your RPC).
- `YDELTA_DEPLOYER_KEYPAIR_PATH` — deployer/upgrade authority.
- `YDELTA_DEPOSITOR_KEYPAIR_B58`, `YDELTA_BORROWER_KEYPAIR_B58` — smoke-test actors.
- `YDELTA_SWITCHBOARD_CROSSBAR`, `YDELTA_SWITCHBOARD_GATEWAY` — needed to crank the
  Switchboard-Pull oracles (SOL, JitoSOL).

**`.local/` inputs** (copy from `.local.example/`, gitignored):
- `mainnet-banks.json` — **verify every pubkey against `solana account <bank>`
  before mainnet use** (the file itself says so). This is the only source of bank/
  oracle/vault/group pubkeys; no script hits a discovery API.
- `curators-input.json`, `vault-input.json`, `setup-curator-profiles-input.json`,
  `markets-input.json` — review the defaults (Section 5).

## 3. Phase A — mainnet-fork localnet dress rehearsal

Start a validator that clones the live marginfi state these markets depend on:

```bash
solana-test-validator --reset \
  --url <MAINNET_RPC> \
  --clone-upgradeable-program MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA  `# marginfi program` \
  --clone 4qp6Fx6tnZkY5Wropq9wUYgtFxXKwE6viZxFHg3rdAG8   `# marginfi group` \
  --clone 2s37akK2eyBbp8DZgCm7RtsaEz8eJP3Nxd4urLHQv7yB   `# USDC bank` \
  --clone 7jaiZR5Sk8hdYN9MxTpczTcwbWpb5WEoxSANuUwveuat   `# USDC liquidity vault` \
  --clone Dpw1EAVrSB1ibxiDQyTAW6Zip3J4Btk2x4SgApQCeFbX   `# USDC Pyth-push oracle` \
  --clone CCKtUs6Cgwo4aaQUmBPmyoApH2gUDErxNZCAntD6LYGh   `# SOL bank` \
  --clone 2eicbpitfJXDwqCuFAmPgDP7t2oUotnAzbGzRKLMgSLe   `# SOL liquidity vault` \
  --clone 4Hmd6PdjVA9auCoScE12iaBogfwS4ZXQ6VZoBeqanwWW   `# SOL Switchboard oracle` \
  --clone Bohoc1ikHLD7xKJuzTyiTyCwzaL5N7ggJQu75A8mKYM8   `# JitoSOL bank` \
  --clone 38VGtXd2pDPq9FMh1z6AVjcHCoHgvWyMhdNyamDTeeks   `# JitoSOL liquidity vault` \
  --clone 5htZ4vPKPjAEg8EJv6JHcaCetMM4XehZo8znQvrp6Ur3   `# JitoSOL Switchboard oracle` \
  --clone EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v   `# USDC mint` \
  --clone So11111111111111111111111111111111111111112    `# wSOL mint` \
  --clone J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn    `# JitoSOL mint`
```

> If a later step fails with an "account not found", clone the account it names and
> restart — marginfi banks reference a few extra accounts (group admin, fee state)
> that may need adding. Set `.env` `YDELTA_RPC_URL=http://127.0.0.1:8899`.

Then run the real pipeline against the fork:

1. `scripts/deploy-program.sh` → builds + deploys → writes `.local/protocol.json`.
2. `yarn bootstrap` → the 5-step chain (Section 5).
3. `yarn create-market-lut` per market (tx-size lookup tables; see Section 5).
4. **Smoke the full lifecycle** (airdrop SOL locally, mint/clone token balances):
   LP `vault:deposit` → curator `vault:place-ask` → borrower `seat:deposit`
   (collateral) → `borrower:place-bid` → `crank-matched-loan` → `borrower:repay` →
   `claim-repayment`. Confirm a `LoanFixed` opens, accrues, and closes.

Pass = the new program id and the whole bootstrap are validated end-to-end. Tear down.

## 4. Phase B — mainnet deploy

1. **Verifiable build:** `scripts/build-verifiable.sh` (deterministic Docker build
   → byte-stable `ydelta.so` OtterSec can match).
2. **Fund the deployer** (~5 SOL for program rent + tx).
3. **Deploy:** `scripts/deploy-program.sh` — review the printed *Deploy plan*
   (cluster, program id `HfYC…`, artifact bytes, signer), confirm, deploy. Writes
   `.local/protocol.json` (idempotent: re-records if already on-chain).
4. **Upload the IDL on-chain:** `scripts/upload-idl.sh` (so Explorer/Solscan decode
   instruction names + args). Signer must be the upgrade authority.
5. **Register the verified build:** `scripts/verify-program.sh` (flips the program
   to "Verified Build" on Solscan/Explorer/Phantom/Jupiter).

## 5. Markets bootstrapping plan

`yarn bootstrap` runs five **idempotent** steps (each skips when its `.local/*.json`
output already exists), then prints a summary. With the default inputs:

| # | Step | Output | Creates |
|---|---|---|---|
| 1 | `create-global-config` | `global-config.json` | protocol global config (admin = signer) |
| 2 | `create-curators` | `curators.json` | `curator-main` |
| 3 | `create-vault` | `vaults.json` | **USDC vault** on bank `2s37…` |
| 4 | `setup-curator-profiles` | `risk-profiles.json` | **3 sub-vaults** in the USDC vault |
| 5 | `init-market` | `markets.json` | **USDC/SOL** + **USDC/JitoSOL** markets |

Default sub-vault profiles (`setup-curator-profiles-input.json`) — review before running:

| Profile | max LTV | max term | liquidation LTV |
|---|---|---|---|
| conservative | 65% | 7 days | must be ≥ max_ltv + 2% (set per policy) |
| balanced | 75% | 30 days | ≥ 77% |
| aggressive | 85% | 90 days | ≥ 87% |

> The input only lists `maxLtvBps`/`maxTermSeconds`; confirm `setup-curator-profiles.ts`
> sets a `liquidation_ltv_bps` ≥ `max_ltv + MIN_LIQ_GAP_BPS` (200) and a `spread_bps`,
> or the create will reject. Adjust the input to taste before bootstrapping.

**After bootstrap — lookup tables:** `yarn create-market-lut` for each market.
Order/match/liquidate txns touch many accounts (two banks, oracles, two marginfi
accounts, vaults); the LUT keeps them under the tx-size limit. Do this before any
real order flow.

**Markets are live at creation** — there's no paused-by-default handshake. A market
starts trading the instant a curator posts an ask into a funded sub-vault.

Verify: every `.local/*.json` output is present and the bootstrap summary shows
`global-config: OK`, `vaults: 1`, `profiles: 3`, `markets: 2`.

## 6. Liquidity & go-live

1. **Seed liquidity:** LP `yarn vault:deposit` into a sub-vault (the deposit lands
   on marginfi and starts earning immediately).
2. **Quote:** curator `yarn vault:place-ask` per market — priced as
   `live bank lending APR + spread`.
3. **Stand up the crankers** (separate `../crankers` repo — the keeper service):
   re-pin its `ydelta` git dep to a `v1-followups`/`main` commit so it picks up the
   new program id via `ydelta::id()`, set its `.env` (RPC, keeper keypair; program
   id is auto unless overridden), and enable the loops:
   - `oracle-crank` / Switchboard pulls (SOL, JitoSOL) — **required**, those feeds
     must be fresh or the LTV gate rejects.
   - `match-crank` (crossed-at-rest books), `process-matched-loan` (promote matches
     to `LoanFixed`), `claim-repayment`, `claim-curator-fee`, `settle`, `liquidate`.
4. **Smoke a real borrow** with small size; watch one full lifecycle; then open up.

## 7. Operations & safety

- **Keepers run continuously** — the protocol needs the crankers for matching,
  promotion, claims, settlement, liquidation, and oracle freshness.
- **Pause switches:** `yarn set-market-pause` (per market) and the global pause for
  emergencies (oracle staleness, marginfi-side issue, accounting anomaly).
- **Oracles:** USDC is Pyth-push (cranked by Pyth; our crank is a backstop); SOL &
  JitoSOL are Switchboard-pull (we **must** crank them).
- **Upgrades:** `build-verifiable.sh` → `upgrade-program.sh` (checks signer ==
  upgrade authority, checks new `.so` ≤ on-chain data length, supports `--buffer`
  resume) → `verify-program.sh` → `upload-idl.sh` if the IDL changed.
- **Rollback:** pause the affected scope; ship a fix via the upgrade authority. The
  retired `Ar38…` keypair stays in `closed/` for reference only.

## 8. Pre-flight checklist

- [ ] Cluster target chosen; `.env` `YDELTA_RPC_URL` set
- [ ] Deployer funded; upgrade-authority / admin / curator key split decided
- [ ] `.local/mainnet-banks.json` pubkeys verified against `solana account …`
- [ ] Sub-vault profiles reviewed (LTV / term / liquidation gap / spread)
- [ ] Phase A fork rehearsal passed (deploy + bootstrap + full lifecycle)
- [ ] Verifiable build produced
- [ ] Mainnet deploy + `protocol.json` written
- [ ] IDL uploaded + verified build registered
- [ ] Market LUTs created
- [ ] Liquidity seeded; curator quoting
- [ ] Crankers re-pinned, configured, and running (oracles first)
- [ ] Small-size smoke borrow completed end-to-end
