# `.local.example/` → `.local/`

Templates for every JSON input the `ts/scripts/*.ts` action scripts read.

Copy what you need into `.local/` (gitignored) and fill in the bracketed
placeholders. Each script reads its `*-input.json` from `.local/` and
writes its result back as a sibling JSON file:

| script                  | input                              | output                |
| ----------------------- | ---------------------------------- | --------------------- |
| `yarn deploy`           | (none)                             | `protocol.json`       |
| `yarn create-global-config` | `protocol.json`                | `global-config.json`  |
| `yarn create-curators`  | `curators-input.json`              | `curators.json`       |
| `yarn create-vault`     | `vault-input.json`                 | `vaults.json`         |
| `yarn setup-curator-sub-vaults` | `setup-curator-sub-vaults-input.json` | `curator-setup.json` + `sub-vaults.json` |
| `yarn init-market`      | `markets-input.json`               | `markets.json`        |
| `yarn bootstrap`        | (chains the above)                 | `tx-log.json`         |
| `yarn vault:place-ask`  | `vault-place-ask-input.json`       | (sig logged)          |
| `yarn vault:cancel-order` | `vault-cancel-order-input.json`  | (sig logged)          |
| `yarn vault:deposit`    | `vault-deposit-input.json`         | (sig logged)          |
| `yarn vault:withdraw`   | `vault-withdraw-input.json`        | (sig logged)          |
| `yarn seat:deposit`     | `seat-deposit-input.json`          | (sig logged)          |
| `yarn seat:withdraw`    | `seat-withdraw-input.json`         | (sig logged)          |
| `yarn borrower:place-bid` | `borrower-place-bid-input.json`  | (sig logged)          |
| `yarn borrower:repay`   | `borrower-repay-input.json`        | (sig logged)          |
| `yarn crank-matched-loan` | `crank-matched-loan-input.json`  | (sig logged)          |
| `yarn claim-repayment`  | `claim-repayment-input.json`       | (sig logged)          |
| `yarn claim-curator-fee` | `claim-curator-fee-input.json`    | (sig logged)          |
| `yarn settle`           | `settle-input.json`                | (sig logged)          |
| `yarn liquidate`        | `liquidate-input.json`             | (sig logged)          |
| `yarn set-fee-config`   | `set-fee-config-input.json`        | (sig logged)          |
| `yarn set-market-pause` | `set-market-pause-input.json`     | (sig logged)          |

Secrets (RPC URL, keypair path, Hermes URL) live in `.env`, not here.

## Bank registry (`mainnet-banks.json`)

The single source of truth for marginfi bank pubkeys, oracle keys,
liquidity vaults, and Pyth feed ids. **No script ever hits a runtime
discovery API.** Edit this file once at install time; every action
script (`create-vault`, `init-market`, the oracle crank path, etc.)
reads from it.

Verify every entry against `solana account <bank>` before mainnet use —
the scripts re-check the registry against on-chain state at runtime
(`verifyBankMatchesRegistry`) and abort if anything diverges, so
copying stale or wrong values surfaces immediately rather than during
a signed transaction.

For your current intended setup, keep exactly one curator in
`curators-input.json` and point all three sub-vaults at that label via the
top-level `curatorLabel` in `setup-curator-sub-vaults-input.json`.

## Bootstrap order

```
yarn deploy            # writes .local/protocol.json (idempotent — skips if already deployed)
yarn bootstrap         # runs every step below, idempotent on each:
#                       create-global-config → create-curators → create-vault
#                       → setup-curator-sub-vaults → init-market
```

`tx-log.json` accumulates every signed tx (with oracle-crank sigs nested
under each entry) across all scripts so you have a single replay log.

## Oracle cranking

Every script that touches a marginfi health check (vault/seat withdraw,
place-bid, settle, liquidate, claim-repayment, claim-curator-fee,
init-market) re-reads the bank from chain, detects Pyth-Push staleness
via `bankNeedsOracleRefresh`, and prepends Pyth crank ixs sourced from
Hermes. The hex `pythFeedIdHex` per bank is pinned in
`markets.json` (USDC bank is Pyth-Push and lives there as
`debtPythFeedIdHex`) or pasted explicitly into the withdraw / fee-claim
input files. Switchboard-pull banks are left alone — Switchboard's own
publish cadence is fast enough that the on-chain age check passes
during normal operation.
