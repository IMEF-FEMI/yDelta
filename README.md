# yDelta

**Optimized fixed-rate lending on Solana.**

> This README is published on npm as the docs for [`@ydelta/sdk`](https://www.npmjs.com/package/@ydelta/sdk). The protocol explainer is below; the install + SDK-usage quickstart is here.

## `@ydelta/sdk` — install

```bash
yarn add @ydelta/sdk
# or
npm install @ydelta/sdk
```

Works with `@solana/web3.js ^1.95`.

## Quickstart

```ts
import { Connection, PublicKey } from '@solana/web3.js';
import {
  YDELTA_PROGRAM_ID,
  fetchMarket,
  fetchVault,
  placeOrderInstruction,
} from '@ydelta/sdk';

const conn = new Connection('https://api.mainnet-beta.solana.com', 'confirmed');
const market = await fetchMarket(conn, new PublicKey('9mSq5qvdKPJdNE8T8UMALrkuLxisw9eed87z33sWCUcv'));
// Vaults are keyed by the marginfi BANK (the market's debt-side lending pool),
// not by mint.
const vault  = await fetchVault(conn, market.header.debtLendingPool);
```

The SDK ships:

- **Instruction builders** for every yDelta instruction (`placeOrderInstruction`, `repayInstruction`, `convertP2poolToFixedInstruction`, …).
- **Account decoders** (`decodeMarket`, `decodeGlobalVault`, `decodeLoanFixed`, …) over the raw `getAccountInfo` bytes.
- **Helpers** for LTV math, marginfi share/atom conversion, oracle price reads.
- **The IDL** (`ts/idl/ydelta.json`) for off-chain tooling.

See the on-chain program under `programs/ydelta/` and the source under `ts/src/` in the [GitHub repo](https://github.com/IMEF-FEMI/yDelta).

---

yDelta runs a **two-sided** fixed-rate orderbook. The ask side holds
lender quotes, and those quotes come only from vault **sub-vaults** —
curator-run (Pool) or owner-run (Private). The bid side holds borrower
orders: a borrow request crosses resting asks immediately, and any
unfilled remainder can fall through to a marginfi backstop, rest on the
book as a standing bid, or drop.

yDelta is fixed-rate, fixed-term lending built on four ideas:

1. **Two-sided orderbook** — sub-vaults quote asks, borrowers post bids; but every ask is still backed by vault capital, never a bare wallet maker
2. **Yield-alive capital** — no atom sits idle; everything earns through marginfi
3. **Strategy sub-vaults** — one vault per bank runs many curator strategies
4. **`p0` backstop** — marginfi as the always-available fallback when book liquidity is thin

Sub-vault curators post asks priced as a **spread over the live bank
rate** (`bank lending APR + spread`, term-capped, unbounded size).
Borrowers place bids (`rate × term × principal × collateral`) that fill
immediately and optionally rest their residual. A matching engine crosses
them into discrete loans with the terms baked in. Everything in between —
idle vault capital waiting to match, collateral backing an active loan or
a resting bid, principal waiting to be claimed — sits in marginfi banks
earning supply yield.

---

## Table of contents

- [yDelta](#ydelta)
  - [Table of contents](#table-of-contents)
  - [1. What yDelta does differently](#1-what-ydelta-does-differently)
  - [2. Two-sided credit](#2-two-sided-credit)
  - [3. Yield-alive capital](#3-yield-alive-capital)
  - [4. Strategy sub-vaults: the lending optimizer](#4-strategy-sub-vaults-the-lending-optimizer)
  - [5. `p0` as backstop](#5-p0-as-backstop)
  - [6. 0-CPI fast-path](#6-0-cpi-fast-path)
  - [7. Partial liquidations](#7-partial-liquidations)
  - [8. Anatomy of a loan](#8-anatomy-of-a-loan)
  - [9. Architecture](#9-architecture)
    - [Two marginfi accounts per market](#two-marginfi-accounts-per-market)
    - [The seat-share invariant](#the-seat-share-invariant)
  - [10. Under the hood](#10-under-the-hood)
    - [Yield decomposition](#yield-decomposition)
    - [Unbounded sub-vault asks + match-time atomicity](#unbounded-sub-vault-asks--match-time-atomicity)
    - [Two-step admin transfers + market/global pause](#two-step-admin-transfers--marketglobal-pause)
    - [Oracle integration](#oracle-integration)
  - [11. Build \& test](#11-build--test)

---

## 1. What yDelta does differently

yDelta is fixed-rate, fixed-term lending built around two ideas that are hard to combine: a real orderbook for price discovery, and a yield rail underneath so capital is never idle. Most fixed-rate protocols ship a subset of what's below. yDelta is designed so all of it is **structural** — not curator-toggles, not optional features, not bolt-ons.

1. **Capital is yield-alive for the entire lifecycle.** Every deposit immediately routes into marginfi at the program level. Resting asks, encumbered collateral, idle vault liquidity, and even borrowed principal earn supply APY by default — until the user actively withdraws to a wallet. Even a borrower's bid that merely *rests* on the book keeps its collateral earning. There is no "atoms in escrow earning nothing" state anywhere in the protocol.

2. **The orderbook has a built-in variable-rate backstop, and the variable portion is upgradeable.** When fixed-rate liquidity doesn't fill a bid, the residual can fall through to `marginfi.borrow` so the borrower walks away with full requested principal — no partial-fill cliff. Later, `convert_p2pool_to_fixed` lets the borrower walk the asks tree and migrate the variable portion back to fixed-rate when better terms appear. The fallback is a backstop, not a one-way commitment.

3. **Vaults run multiple curator strategies on one capital pool per bank.** A single `GlobalVault` per marginfi **bank** hosts many `SubVault` entries, each with its own operator, spread, LTV ceiling, liquidation threshold and term cap. Sub-vaults come in two kinds — **Pool** (admin-created, curator-run, pooled deposits) and **Private** (permissionless, single-owner). A depositor can hold seats in multiple sub-vaults inside the same vault; a sub-vault can quote on any market whose debt side is that bank, with no fixed cap on how many.

4. **The book is two-sided, but asks are vault-only.** Ask-side quotes come only from vault sub-vaults — there are no wallet makers and no market-direct asks. A borrower posts a bid that crosses resting asks immediately; its unfilled residual chooses one of three fates — fall through to the variable-rate backstop, **rest** on the bid side of the book, or drop. A resting bid is later crossed by a fresh or repriced sub-vault ask (which *takes* on placement) or by anyone running the permissionless `MatchCrank`. Self-trading is blocked at the **owner** level: a wallet's bid never fills against a sub-vault that same wallet curates — that pair is skipped, not aborted.

5. **Rates are a spread over the live bank rate.** A sub-vault stores a `spread_bps`; at placement the program reads the debt bank's live marginfi lending APR and stores `lending_APR + spread` in the order. Repricing a market is a parameterless re-sync. A **fill-time floor** enforces the same idea on the way out: the engine skips any resting ask whose stored rate has fallen below the *current* bank lending APR, so a stale quote never fills below market.

6. **LTV is the curator's, decoupled from marginfi.** A fixed loan's collateral is an *asset* on marginfi (it carries no marginfi liability), so marginfi's risk weights were only ever a self-imposed reference. Origination gates on the sub-vault's `max_ltv_bps` alone — which may sit **above** marginfi's implied LTV, extending more borrowing power than marginfi itself would. Liquidation triggers on a per-sub-vault `liquidation_ltv_bps`, and both LTVs are **stamped onto the loan at match** so curator updates never move thresholds on open loans. Marginfi weights still gate one path — the variable-rate fallback, which opens a real marginfi borrow.

7. **Fixed terms genuinely run to maturity.** A loan opens at the locked rate, accrues for the full term, and resolves on borrower repay or keeper settlement after grace. There is no auto-rolling into shorter terms, and no prepayment fee — early repay is free. Repricing is opt-in via `convert_p2pool_to_fixed` (and only for borrowers on the variable-rate fallback), never imposed by the protocol.

8. **Curators reprice for free.** `place_order_for_sub_vault`, `cancel_order_for_sub_vault`, and `update_order_for_sub_vault` fire **zero external CPIs** — a vault ask is a pure-memory bookkeeping entry on the market account; it carries no fixed principal and takes no seat encumbrance. Even when a placement *takes* a crossable resting bid, the fill is recorded as a queued node and the atom movement is deferred to a permissionless cranker. Curators can reprice continuously without paying compute tax for each adjustment.

Taken together: yDelta prices credit on a two-sided orderbook, backstops it with marginfi, keeps every atom productive in every state, and lets curators run real strategies — all built on the same set of mechanisms rather than separate paths bolted together.

---

## 2. Two-sided credit

Most lending protocols pool deposits and let the protocol set one risk model for everyone. yDelta prices credit on an orderbook instead — two-sided, but with asks backed only by vault capital.

**Only vault sub-vaults quote the ask side.** Every ask belongs to a `SubVault` inside a `GlobalVault`. Each sub-vault carries a curator-set `spread_bps`, a `max_ltv_bps` origination ceiling, a `liquidation_ltv_bps` trigger, and a `max_term_seconds` cap. The matching engine reads these **live** from the sub-vault at match time, so a curator's policy change takes effect immediately with no per-seat re-sync.

**Spread-over-bank rates.** A sub-vault does not post an absolute rate. At placement the program reads the debt bank's live marginfi lending APR and stores `bank_APR + spread_bps` in the order — the sort tree still orders by the stored rate. A **fill-time floor** then skips, on the way out, any resting ask whose stored rate has fallen below the *current* live bank lending APR, so a stale quote never fills below market. The fix is a parameterless `update_order_for_sub_vault` re-sync that re-reads the bank.

**Borrowers post bids that can rest.** A borrow request crosses resting asks immediately; whatever it doesn't fill is handled by the borrower's chosen `residual_mode` — **P2PoolFallback** (open a variable-rate marginfi borrow), **Rest** (leave a standing bid whose collateral keeps earning), or **Drop**. A resting bid is crossed by a later sub-vault ask placement/re-sync (which *takes*) or by the permissionless `MatchCrank`; the borrower can `CancelOrder` or `UpdateOrder` it. A book can sit **crossed at rest** — that is legal; the invariant is that no *fillable* cross survives a taking instruction.

**Rate matching.** A bid crosses any ask whose `ask_rate ≤ bid_rate` and whose `max_term ≥ bid_term`. On a cross the loan is stamped:

```
lender_rate   = ask_rate
borrower_rate = max(bid_rate, ask_rate + protocol_fee_bps_floor)
```

The bid rate is a *ceiling on the lender rate*; the protocol fee floor is always guaranteed on top. So with `protocol_fee_bps_floor = 50` and a 500 bps bid: an ask at 480 yields `lender_rate = 480`, `borrower_rate = max(500, 530) = 530`; an ask at 400 yields `lender_rate = 400`, `borrower_rate = max(500, 450) = 500`. The borrower can pay up to the floor (50 bps) above their stated bid — they accept the protocol floor as a fee added on top. An ask at 510 does not cross a 500 bid.

**The match checks the sub-vault's LTV.** Every cross verifies `actual_ltv ≤ sub_vault.max_ltv_bps` at oracle prices — and nothing else (marginfi weights are not consulted for fixed fills). A bid whose collateral doesn't satisfy a sub-vault's cap simply skips that ask and walks on to one whose cap it does satisfy.

---

## 3. Yield-alive capital

Fixed-rate orderbooks have a structural problem: lender capital has dead time. Idle vault liquidity waits for a bid to cross (could be hours, could be days). The borrower's collateral sits in escrow until the loan matures. After repay, the lender's principal sits in escrow until it is claimed. Existing fixed-term protocols accept this as the cost of doing business.

yDelta routes **every atom** through marginfi for the entire lifecycle. Vault lenders, borrowers, idle vault liquidity, post-repay claims — all of it sits in marginfi banks earning supply APY by default. Even a borrower's collateral behind a *resting bid* keeps earning while the bid waits to be crossed.

```
   ┌──────────────────┐
   │ Vault depositor  │
   └────────┬─────────┘
            │ deposit USDC into a SubVault
            ▼
   ┌──────────────────────┐
   │  marginfi USDC bank  │  ◄── atoms earn supply APY:
   │  (vault integration) │     while asks rest, while
   └──────────┬───────────┘     loans run, while waiting
              │ shares           for claim
              ▼
   ┌──────────────────────┐
   │  SubVault pool       │
   │  idle_principal      │  ◄── a resting ask takes NO
   │  deployed_principal  │     encumbrance; the idle pool
   └──────────────────────┘     caps each cross at match time
              │
              │ match: idle → deployed; loan PDA records who owes what
              ▼
        Loan accrues
   ┌──────────────────────┐
   │ borrower repays      │  ◄── atoms re-enter marginfi →
   │ atoms route through  │     claim moves them to the
   │ marginfi             │     vault → withdraw
   └──────────────────────┘
```

**Place / cancel / re-sync a vault ask:** nothing moves. A vault ask carries no fixed principal and takes no seat encumbrance — it is a pure bookkeeping entry. The sub-vault's `idle_principal` pool is the backing, read at match time. Yield keeps accruing.

**Match a bid against a vault ask:** the sub-vault's `idle_principal` decrements and `deployed_principal` increments; the loan PDA records who owes what. The borrower's withdrawn principal also lands on marginfi (credited to the borrower's seat), so even **borrowed atoms earn supply yield until the borrower pulls them to their wallet**.

There is no "atoms in escrow earning nothing" state in the entire protocol.

The bookkeeping invariant that makes it work:

```
Per-trader balance = marginfi shares on seat
                   = withdrawable_shares + encumbered_shares

Atoms held         = shares × bank.asset_share_value
Yield accrual      = bank.asset_share_value growth
```

Share counts are conserved through `deposit / withdraw / match / repay / claim`. marginfi handles the yield arithmetic by share-price growth, which applies uniformly regardless of which seat-bucket the shares live in.

---

## 4. Strategy sub-vaults: the lending optimizer

Most depositors don't want to operate like an orderbook market maker. They don't want to decide what spread to quote, which markets to quote in, when to cancel and reprice, or how to balance idle versus deployed capital. They want to deposit, pick a risk style, and earn.

**Strategy sub-vaults are the only way liquidity reaches the book.** Because the ask side is vault-only, every lender ask comes from a curator-managed sub-vault. Depositors fund the sub-vaults; curators do the quoting.

A `GlobalVault` is keyed to one marginfi **bank** and holds many curator-managed **sub-vaults**, where each sub-vault is a distinct lending strategy. What's distinctive: **a single deposit can back multiple strategies at once, and each sub-vault lends across many markets simultaneously.**

```
                          GlobalVault (USDC bank)
                          ┌──────────────────┐
                          │ idle: $10,000,000│
                          ├──────────────────┤
                          │ SubVault A       │  Pool, 60% max_ltv,
                          │ (conservative)   │  30-day max term
                          ├──────────────────┤
                          │ SubVault B       │  Private, 75% max_ltv,
                          │ (owner-run)      │  90-day max term
                          └────────┬─────────┘
                                   │
                  ┌────────────────┼────────────────┐
                  │                │                │
                  ▼                ▼                ▼
       ┌──────────────┐  ┌──────────────┐  ┌──────────────┐
       │ USDC/SOL mkt │  │ USDC/JTO mkt │  │USDC/JUP mkt  │
       │  Ask (idle)  │  │  Ask (idle)  │  │  Ask (idle)  │
       └──────────────┘  └──────────────┘  └──────────────┘
```

A sub-vault may quote any market whose debt side is the vault's bank — there is no fixed cap on the count. Each ask is **unbounded** ("quote all idle"): it carries no principal, and every cross against it is sized at match time by the sub-vault's live `idle_principal`. The matching engine reads + mutates `GlobalVault` state at match time atomically, so a sub-vault can never accidentally over-commit even with concurrent matches against different markets — the match gate is `sub_vault_idle ≥ matched_principal`.

The vault-owned `ClaimedSeat` a sub-vault uses in a market is **auto-created** on the first `place_order_for_sub_vault` in that market — there is no separate claim-seat step.

For the depositor, the experience is what they expect from any lending protocol: deposit, choose a sub-vault, earn, withdraw.

**Two yield streams, accrued continuously.** A sub-vault earns from two sources at once:

- **Supply yield on idle capital** — funds waiting for matches sit on marginfi and earn supply APY through share-price growth
- **Lender-rate yield on deployed capital** — matched loans earn the locked-in fixed rate

Both stream into the same share-price, so depositors don't have to choose between "idle but liquid" and "earning but locked." Both states are productive. Loan yield is credited **net of `curator_fee_bps`** so the curator's management fee is not double-counted into the share price. The curator fee is a sub-vault property — set by the admin at Pool creation, zero for Private, and immutable thereafter.

**O(1) per-depositor accounting.** A running aggregate `total_weighted_rate_bps = Σ(P_i × R_i)` across all open loans, plus Aave-style cumulative yield indices, makes deposit and withdraw O(1) regardless of how many loans the sub-vault has open. A sub-vault with 10,000 active loans accrues yield in the same number of CPU cycles as one with 10. No iteration, ever.

**Vault-wide aggregates.** `GlobalVaultFixed` carries vault-wide aggregates maintained in lockstep with the per-sub-vault fields. Withdraws are gated per-sub-vault (`total_principal − deployed − encumbered ≥ atoms_out`) and against the vault-wide marginfi balance. Each sub-vault also tracks `open_orders_count` and `open_loans_count`, and can only be removed once both are zero.

A user holds seats in multiple sub-vaults inside the same vault — different strategies, different risk-return tradeoffs, all under one deposit.

---

## 5. `p0` as backstop

Strategy sub-vaults give passive lenders a productive home. `p0` — marginfi — gives the *protocol* a productive home, in two ways.

**As a yield rail.** This is the substrate beneath yield-alive capital (Section 3). Every atom not actively committed to a counterparty sits on marginfi. The orderbook is the optimised path; marginfi is the always-available path *for capital*.

**As a liquidity backstop.** When a borrower's bid exceeds available orderbook liquidity at their rate, the borrower's `residual_mode` decides the fate of the unfilled remainder:

```
borrow request (bid crosses resting asks)
    │
    ├── fully filled                   →  fixed loan(s)
    │
    └── residual remains:
          ├── P2PoolFallback  →  marginfi.borrow for the residual (variable rate)
          ├── Rest            →  a standing bid at the limit price
          └── Drop            →  cancel the residual, release its collateral
```

The **P2Pool fallback** is the only path that opens a *real* marginfi borrow, so it is the only path marginfi's own risk weights gate: the residual is pre-checked against marginfi's init-weight collateral requirement and rejected with a clear error rather than letting the borrow CPI fail opaquely. With **Rest**, the residual becomes a standing bid (prunable by a `last_valid_unix_ts` expiry); with **Drop**, it cancels and releases its collateral.

**Voluntary P2Pool repayment.** A borrower on the fallback can repay the variable-rate debt at any time via `repay`. The repayment is made against the borrower marginfi liability directly; full repay re-reads the live liability and retires it to exactly zero. The P2Pool loan PDA closes only once the post-CPI live liability is zero.

**Upgrade path: variable → fixed.** A borrower who took the fallback isn't stuck on the variable rate. `convert_p2pool_to_fixed` walks the asks tree and crosses every compatible vault sub-vault ask whose `rate_bps ≤ max_acceptable_rate_bps` AND `term_seconds ≥ remaining_term`, converting the variable-rate P2Pool debt into fresh fixed-rate `MatchedLoan` queue nodes. Each cross emits a match; the unfilled residual stays on the original P2Pool loan body. Full conversion closes the P2Pool PDA — again, only when the live liability is zero. So the fallback is genuinely a backstop, not a trap.

This is yDelta's strategic posture: **the orderbook is where credit gets priced; marginfi is where credit gets backstopped.** The two layers complement rather than compete.

---

## 6. 0-CPI fast-path

A curator repricing a vault ask does **zero external CPIs**. Most lending protocols fire 4-6 CPIs per order mutation (deposit, oracle reads, allocation, etc.). The orders-of-magnitude difference comes from the design: a vault ask is a pure-memory bookkeeping entry — it carries no principal and takes no encumbrance, so placing, cancelling and re-syncing it never touches a bank.

| Operation | CPIs | Notes |
|---|---|---|
| `place_order_for_sub_vault` | **0** | Pure-memory ask insert; auto-creates the vault seat; takes crossable resting bids |
| `cancel_order_for_sub_vault` / `update_order_for_sub_vault` | **0** | Pure bookkeeping (re-sync re-reads bank APR + spread) |
| `place_order` (borrower bid) crossing a vault ask | 0 (place); 3 (cranker) | Atom migration deferred to a permissionless cranker step |
| `place_order` with P2Pool fallback firing | 2 (`borrow + deposit`) | Residual borrows from marginfi and re-deposits |
| `cancel_order` / `update_order` (borrower bid) | **0** | Pure bookkeeping; releases / re-keys the resting bid |
| `match_crank` | **0** | Permissionless sweep of a crossed-at-rest book |
| `deposit` / `withdraw` | 1 each | + 1 SPL transfer |
| `repay` | 1 (`marginfi.deposit` or `marginfi.repay`) | + 1 SPL transfer |
| `claim_repayment_for_sub_vault` | 3 | Drains realised atoms back into the vault's marginfi account |
| `process_matched_loan` | 0–3 | Keeper mints the loan PDA (+ atom migration for fixed matches) |
| `settle_matured_loan` / `liquidate_loan` | 4 | Partial-by-default |

A market with thousands of active vault asks, where every cancel-and-reprice fires zero CPIs, has dramatically lower compute and rent costs than one where each operation roundtrips through the bank.

---

## 7. Partial liquidations

Both `liquidate_loan` and `settle_matured_loan` accept `repay_atoms_max`. A keeper can settle whatever liquidity they have; the loan stays `Active` until outstanding hits 0.

| Outcome | Liquidator seizes | Surplus to borrower | Bad-debt gap |
|---|---|---|---|
| Over-collateralized (typical) | `repay_value + bonus` | `collateral - seized` returned to borrower's seat | 0 |
| Exactly collateralized | `collateral` (full) | 0 | 0 |
| Under-collateralized | `collateral` (capped) | 0 | `(repay_value + bonus) - collateral` logged via `BadDebtLog` |

**The liquidation trigger is stamped, not live.** A fixed loan is liquidatable when its live oracle LTV breaches the `liquidation_ltv_bps` that was **stamped from the sub-vault at match time** — not marginfi maintenance weights, and not the sub-vault's current value. A curator who later changes the sub-vault's thresholds does not move the trigger on any open loan. Sub-vault create/update enforce `liquidation_ltv_bps ≥ max_ltv_bps + MIN_LIQ_GAP_BPS`, so no loan is ever born liquidatable. (P2Pool fallback positions are the exception — they hold a real marginfi liability, so their health is still measured against marginfi maintenance weights.)

Tiered keeper bonus (`bonus_atoms = debt_value_in_collateral × liquidation_keeper_bps / 10_000`) is admin-tunable per market. Keepers race on liquidations the same way they race on Solana arb opportunities — permissionless, performance-optimal. The liquidator's repaid atoms net of the liquidation protocol fee are deposited to the lender side so the fee accumulator stays fully backed.

Two read-only simulation gates — `check_ltv_liquidatable` and `check_maturity_liquidatable` — let keepers and UIs confirm a settlement would succeed before sending the real transaction.

---

## 8. Anatomy of a loan

Consider a 100 USDC loan at 8% APR for 30 days, collateralised with 0.5 SOL.

**1. A depositor funds a sub-vault.** `global_vault_deposit(100 USDC, sub_vault_id)`. Atoms hop into the vault's marginfi integration account; the sub-vault's `idle_principal` pool grows and the depositor is credited with vault shares. **From this moment, the atoms accrue marginfi supply APY.**

**2. The curator posts a vault ask.** `place_order_for_sub_vault(sub_vault_id)` — no rate or term parameter; the program computes `bank lending APR + sub_vault.spread_bps` and uses `sub_vault.max_term_seconds`. The vault-owned `ClaimedSeat` for this market is auto-created on first use, then an unbounded `RestingOrder` is inserted into the asks tree. **No external CPI fires, no principal is committed, no encumbrance is taken** — the sub-vault's idle pool is the backing. The placement also *takes* any crossable resting bids.

**3. Borrower deposits collateral.** `deposit(0.5 SOL, is_debt=false)`. Atoms hop into the market's borrower-side marginfi account; the borrower's seat is credited with collateral shares. **Collateral atoms also start earning marginfi supply APY.**

**4. Borrower places a bid.** `place_order(rate=8%, term=30d, principal=100, collateral=0.5, residual_mode)`. The matching engine walks the asks tree, finds the curator's ask (`ask_rate 8% ≤ bid_rate 8%`), checks `actual_ltv ≤ sub_vault.max_ltv_bps`, sizes the cross against the sub-vault's idle pool, debits `idle_principal`, credits `deployed_principal`, and inserts a `MatchedLoan` queue node stamping the LTV pair. Any unfilled residual follows the borrower's `residual_mode` (fallback / rest / drop).

**5. A keeper promotes the match.** Anyone can call `process_matched_loan(sequence)`. The keeper allocates a fresh `Loan` PDA, stamps the loan's terms (including `lender_rate`, `borrower_rate`, and the origination/liquidation LTV pair), and zeroes the queue node. The keeper pays the PDA rent and is reimbursed at loan close.

**6. The loan accrues.** Time passes. The loan's `outstanding_debt_atoms` and `lender_claimable_atoms` accrue at the fixed rate. **No on-chain interaction is required for accrual** — interest is computed on demand at any read or mutation:

```
elapsed_seconds = now - loan.last_accrued_unix
new_borrower_interest = principal × borrower_rate × elapsed / SECONDS_PER_YEAR / 10_000
new_lender_interest   = principal × lender_rate   × elapsed / SECONDS_PER_YEAR / 10_000
spread                = new_borrower_interest - new_lender_interest

loan.outstanding_debt_atoms      += new_borrower_interest
loan.lender_claimable_atoms      += new_lender_interest
loan.accumulated_protocol_fee    += spread
loan.last_accrued_unix            = now
```

Spread between borrower rate and lender rate accrues to a market-level protocol-fee bucket, drained periodically by the market admin; the curator fee accrues to the sub-vault.

**7. Borrower repays.** `repay(loan, atoms)`. Atoms route into the per-market lender marginfi account via `marginfi.deposit`; the loan's `outstanding_debt_atoms` decrements. On **full** repay the same instruction (a) credits the vault-owned seat's `debt_withdrawable_shares` with the repaid asset shares, (b) decrements the sub-vault's `deployed_principal`, weighted-rate accumulators, and curator-fee accumulator, (c) bumps `pending_claim_atoms` so a permissionless sweeper knows there's something to claim, (d) sweeps the per-loan protocol-fee bucket onto the market accumulator, and (e) closes the loan PDA — refunding rent to the keeper from step 5. The borrower's collateral encumbrance is released back to their seat.

**8. The vault sweeps the repaid atoms.** A permissionless cranker calls `claim_repayment_for_sub_vault(sub_vault_id)` on the relevant `(market, vault, sub_vault)` triple. This is a **stateless seat-to-vault sweeper** — it never reads the loan PDA, never re-accrues, and never touches sub-vault-level accounting. It just `marginfi.withdraw`s the vault seat's `debt_withdrawable_shares` from the per-market lender marginfi account into the vault's own `global_vault_integration_account` (so the atoms can back new asks), and decrements `pending_claim_atoms` by the swept amount.

**9. Or, if the borrower stops responding…** A keeper invokes `settle_matured_loan` (after maturity + grace period) or `liquidate_loan` (if live oracle LTV breaches the loan's stamped `liquidation_ltv_bps`). Both accept `repay_atoms_max` for partial settlements; both seize collateral, pay off the lender's vault, and return any surplus to the borrower's seat. On full close they run the same per-loan close-out as step 7 — the cranker's sweeper in step 8 then realises the atoms.

---

## 9. Architecture

```
                 ┌──────────────────┐                 ┌──────────────┐
                 │  Vault depositor │                 │   Borrower   │
                 │  + curator       │                 │   wallet     │
                 └────────┬─────────┘                 └──────┬───────┘
                          │ deposit USDC into a              │ deposit SOL
                          │ SubVault; curator quotes         │
                          ▼                                  ▼
       ┌─────────────────────────┐          ┌─────────────────────────┐
       │  marginfi (lender side) │          │ marginfi (borrower side)│
       │  — earns supply APY     │          │  — earns supply APY     │
       └────────────┬────────────┘          └────────────┬────────────┘
                    │ shares                             │ shares
                    ▼                                    ▼
       ┌──────────────────────────────────────────────────────────────┐
       │                          Market PDA                           │
       │                                                                │
       │  ┌──────────────┐ ┌──────────────┐ ┌──────────────────────┐  │
       │  │   Bids tree  │ │   Asks tree  │ │    Seats tree        │  │
       │  │ (borrower    │ │ (vault sub-  │ │  (per-trader balance │  │
       │  │  RestingOrder│ │  vault       │ │   bookkeeping)       │  │
       │  │  nodes)      │ │  RestingOrder│ └──────────────────────┘  │
       │  └──────┬───────┘ │  nodes)      │                            │
       │         │         └──────┬───────┘                            │
       │   matching engine ◄──────┘  (bid take · ask take · crank)     │
       │                       │                                        │
       │            ┌──────────▼──────────┐                             │
       │            │   MatchedLoan queue │                             │
       │            └──────────┬──────────┘                             │
       └───────────────────────┼────────────────────────────────────────┘
                               │ keeper promotes
                               ▼
                     ┌─────────────────────┐
                     │     Loan PDA        │
                     │  (fixed-rate,       │
                     │   fixed-term)       │
                     └─────────────────────┘
```

The book has **two trees of resting orders** — the asks tree (vault sub-vault quotes) and the bids tree (borrower residuals that chose to rest). A borrower bid crosses the asks tree on placement; its unfilled residual may rest in the bids tree, where a later ask placement or the `MatchCrank` crosses it.

### Two marginfi accounts per market

A market wraps **two** marginfi-account PDAs, not one:

- **Lender side** holds the debt-mint asset (USDC).
- **Borrower side** holds the collateral asset (SOL) and any P2Pool debt liability.

The split sidesteps a marginfi v0.1.8 invariant: a single marginfi account can't simultaneously hold an asset position and a liability position on the same bank. By keeping the lender's USDC asset and the borrower's USDC liability on separate accounts, both flows become unconditional. A `market_signer` PDA at `[b"market_signer", market]` is the authority on both, and on the market's per-mint debt/collateral staging vaults.

### The seat-share invariant

```
Per-trader balance = marginfi shares on ClaimedSeat
   {debt, collateral} × {withdrawable, encumbered}
```

- **Borrower deposits collateral** → `withdrawable + X` on the collateral side
- **Bid rests** → collateral shares move `withdrawable → encumbered` and keep earning while the bid waits
- **Match** → on the borrower's seat, collateral stays encumbered against the loan; the loan PDA records who owes what (atoms unchanged on chain)
- **Repay** → atoms flow back from the borrower's wallet via `marginfi.deposit`; the vault lender's claim realises onto the vault-owned seat via `claim_repayment_for_sub_vault`

A vault ask takes **no seat encumbrance** — it is backed by the sub-vault's idle pool, sized at match time. Share counts are conserved through the loan lifecycle. Share-price appreciation of the underlying marginfi position accrues continuously to the share holder regardless of bucket.

---

## 10. Under the hood

### Yield decomposition

The protocol decomposes lender yield into two streams so a UI can show them separately:

**Stream 1 — Supply yield (variable, marginfi-driven).** Earned on any atoms sitting on a marginfi account. Yield accrues uniformly by share-price growth on the underlying bank, regardless of whether shares are `withdrawable` or `encumbered`. A depositor whose 100 USDC sits idle while the USDC bank earns 4% supply APY for a month now holds shares worth ≈ 100.33 USDC.

**Stream 2 — Lender-rate yield (fixed, loan-driven).** Earned on the loan's `lender_rate_bps` while the loan is open, credited net of `curator_fee_bps`. Computed lazily at every read/mutation. Because each ask is stored at `bank_APR + spread` and the fill-time floor blocks anything below the live bank lending APR, a fixed loan is always struck at or above the bank's lending rate *at origination*.

**The single rule:** atoms can only earn one stream at a time. Idle on marginfi → supply yield. Committed to a loan → lender rate. Borrowers earn supply yield on borrowed atoms while they're parked on the lender side, naturally hedging their fixed liability against the variable supply rate.

### Unbounded sub-vault asks + match-time atomicity

A vault sub-vault ask rests on the market with **no fixed principal and no per-seat encumbrance** — it is the "quote all idle" model. The sub-vault's `idle_principal` pool is the backing, and there is at most one live ask per (sub-vault, market).

When a borrower's bid crosses a vault ask, the matching engine reads the `GlobalVault` state, checks the gate `sub_vault_idle ≥ matched_principal`, verifies `actual_ltv ≤ sub_vault.max_ltv_bps` at oracle prices, and atomically moves `idle_principal → deployed_principal` — all in the same transaction. **Concurrent matches from different markets see the locked pool** because the match-time read-modify-write is single-tx atomic. After the match, atoms migrate `vault.integration → market.lender_integration` via a 3-CPI cranker step.

### Two-step admin transfers + market/global pause

Every admin role (market admin, vault admin, sub-vault curator, protocol-wide admin) has a two-step transfer pattern: an `initiate` ix sets `pending_admin`, an `accept` ix (signed by the new admin) commits. Prevents accidental transfer to a non-controlled key.

Market and protocol-wide pause flags gate every state-mutating ix at the loader level. **Markets are live at creation** — `CreateMarketParams` carries the full fee config, so there is no unconfigured setup window and no paused-by-default handshake. The pause switches exist for *emergencies* — a market that loses oracle freshness, a vault accounting anomaly, a marginfi-side issue — and can be contained at the affected scope without redeploying code. Sub-vaults additionally support sunset/resume for orderly wind-down.

### Oracle integration

- **Pyth-Push** (`OracleSetup::PythPushOracle`) — single oracle account, decoded via offset reads on the `PriceUpdateV2` layout. Enforces `MIN_PYTH_PUSH_VERIFICATION_LEVEL = Full`. Partial-verified updates are rejected outright.
- **Switchboard-Pull** (`OracleSetup::SwitchboardPull`) — single oracle account, decoded from `PullFeedAccountData.result.value`.
- **StakedWithPythPush** — three oracle accounts (Pyth feed + LST mint + stake state). The Pyth SOL price is adjusted by `(sol_pool_balance - 1 SOL) / lst_supply` to derive the LST exchange rate.

Confidence-interval rejection on both Pyth (`2.12σ`) and Switchboard (`1.96σ`). Threshold = `bank.config.oracle_max_confidence × price` (default 10%). A bounded future-skew gate rejects readings stamped too far ahead of the clock. A volatile, unconfident, or skewed oracle reading rejects the LTV check — both the origination gate (sub-vault `max_ltv_bps`) and the stamped liquidation gate — rather than producing a bogus number.

---

## 11. Build & test

```bash
# Native unit + integration tests (fast)
./scripts/test.sh

# SBPF tests (requires solana-cli >= 2.2)
./scripts/test.sh --sbf

# Filter
./scripts/test.sh user_account
./scripts/test.sh --sbf place_order

# Build the program .so
./scripts/build-program.sh
```

Workspace layout:

```
.
├── README.md
├── programs/
│   ├── ydelta/                       # The ydelta on-chain program
│   ├── ydelta-test-harness/          # SBF test harness program
│   └── marginfi-mocks/               # marginfi v0.1.8 type mocks
├── lib/                              # Shared libs (hypertree)
├── ts/
│   ├── src/                          # `@ydelta/sdk` TypeScript source
│   ├── idl/ydelta.json               # IDL shipped with the npm package
│   ├── scripts/                      # Operator scripts (bootstrap, cranks, debug)
│   └── tests/                        # SDK + integration tests
├── docs/
│   ├── v1-spec.md                    # The v1 decision log (D1–D17) and contract
│   └── protocol-design.md            # Engineering companion to this README
├── dist/                             # Built SDK output (npm publish target)
└── scripts/
    ├── build-program.sh
    ├── deploy-program.sh
    ├── upgrade-program.sh
    └── test.sh
```

---
