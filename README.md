# yDelta

**Optimized fixed-rate lending on Solana.**

yDelta runs a **quote-only** orderbook: the book holds only lender
quotes, and those quotes come only from vault risk-profile curators.
Borrowers do not rest orders — a borrow request is immediate-fill (IOC)
and any unfilled residual optionally routes to the marginfi fallback.

yDelta is fixed-rate, fixed-term lending built on three ideas:

1. **Quote-only orderbook** — only vault risk-profile curators post asks; borrowers fill them immediately
2. **Yield-alive capital** — no atom sits idle; everything earns through marginfi
3. **`p0` backstop** — marginfi as the always-available fallback when book liquidity is thin

Vault risk-profile curators post asks (`rate × term × max-LTV`, unbounded size). Borrowers place immediate-fill bids (`rate × term × principal × collateral`). A matching engine crosses them into discrete loans with the terms baked in. Everything in between — idle vault capital waiting to match, collateral backing an active loan, principal waiting to be claimed — sits in marginfi banks earning supply yield.

---

## Table of contents

- [yDelta](#ydelta)
  - [Table of contents](#table-of-contents)
  - [1. What yDelta does differently](#1-what-ydelta-does-differently)
  - [2. Quote-only credit](#2-quote-only-credit)
  - [3. Yield-alive capital](#3-yield-alive-capital)
  - [4. Strategy vaults: the lending optimizer](#4-strategy-vaults-the-lending-optimizer)
  - [5. `p0` as backstop](#5-p0-as-backstop)
  - [6. 0-CPI fast-path](#6-0-cpi-fast-path)
  - [7. Partial liquidations](#7-partial-liquidations)
  - [8. Anatomy of a loan](#8-anatomy-of-a-loan)
  - [9. Architecture](#9-architecture)
    - [Two marginfi accounts per market](#two-marginfi-accounts-per-market)
    - [The seat-share invariant](#the-seat-share-invariant)
  - [10. Under the hood](#10-under-the-hood)
    - [Yield decomposition](#yield-decomposition)
    - [Unbounded profile asks + match-time atomicity](#unbounded-profile-asks--match-time-atomicity)
    - [Two-step admin transfers + market/global pause](#two-step-admin-transfers--marketglobal-pause)
    - [Oracle integration](#oracle-integration)
  - [11. Build \& test](#11-build--test)

---

## 1. What yDelta does differently

yDelta is fixed-rate, fixed-term lending built around two ideas that are hard to combine: a real orderbook for price discovery, and a yield rail underneath so capital is never idle. Most fixed-rate protocols ship a subset of what's below. yDelta is designed so all of it is **structural** — not curator-toggles, not optional features, not bolt-ons.

1. **Capital is yield-alive for the entire lifecycle.** Every deposit immediately routes into marginfi at the program level. Resting asks, encumbered collateral, idle vault liquidity, and even borrowed principal earn supply APY by default — until the user actively withdraws to a wallet. There is no "atoms in escrow earning nothing" state anywhere in the protocol.

2. **The orderbook has a built-in variable-rate backstop, and the variable portion is upgradeable.** When fixed-rate liquidity doesn't fill a bid, the residual falls through to `marginfi.borrow` so the borrower walks away with full requested principal — no partial-fill cliff. Later, `convert_p2pool_to_fixed` lets the borrower walk the asks tree and migrate the variable portion back to fixed-rate when better terms appear. The fallback is a backstop, not a one-way commitment.

3. **Vaults run multiple curator strategies on one capital pool per asset.** A single `GlobalVault` per mint hosts many `RiskProfile` entries, each with its own curator, LTV ceiling and term cap. A depositor can hold seats in multiple profiles inside the same vault; a profile can quote on any market that shares the vault's mint, with no fixed cap on how many.

4. **The book is quote-only.** Lender quotes come only from vault risk-profile curators — there are no wallet makers and no market-direct quotes. A borrower never rests an order: a borrow request is an immediate-or-cancel (IOC) bid that crosses resting risk-profile asks in one transaction. This collapses the matching engine to a single direction (taker is always a borrower bid; makers are always vault asks) and removes an entire class of stale-order and self-cross edge cases.

5. **Fixed terms genuinely run to maturity.** A loan opens at the locked rate, accrues for the full term, and resolves on borrower repay or keeper settlement after grace. There is no auto-rolling into shorter terms. Repricing is opt-in via `convert_p2pool_to_fixed` (and only for borrowers on the variable-rate fallback), never imposed by the protocol.

6. **Curators reprice for free.** `place_order_for_risk_profile`, `cancel_order_for_risk_profile`, and `update_order_for_risk_profile` fire **zero CPIs** — a vault ask is a pure-memory bookkeeping entry on the market account; it carries no fixed principal and takes no seat encumbrance. Atoms only move on `deposit` / `withdraw` / `repay` / `claim` and the P2Pool fallback. Curators can reprice continuously without paying compute tax for each adjustment.

Taken together: yDelta prices credit on the orderbook, backstops it with marginfi, keeps every atom productive in every state, and lets curators run real strategies — all built on the same set of mechanisms rather than separate paths bolted together.

---

## 2. Quote-only credit

Most lending protocols pool deposits and let the protocol set one risk model for everyone. yDelta prices credit on an orderbook instead — but only one side rests.

**Only vault risk-profile curators quote.** The book holds nothing but lender asks, and every ask belongs to a `RiskProfile` inside a `GlobalVault`. Each profile carries a curator-set `max_ltv_bps` — the lender-side LTV ceiling — and a `max_term_seconds` cap. The matching engine reads `max_ltv_bps` **live** from the profile at match time, so a curator's policy change takes effect immediately with no per-seat re-sync.

**Borrowers fill, they don't rest.** A borrow request is an immediate-or-cancel (IOC) bid. It crosses resting risk-profile asks in the same transaction; whatever it doesn't fill either routes to the P2Pool fallback or drops. There is no resting borrower order, no bids tree, and no off-chain matchmaker.

**Rate matching.** A bid crosses any ask whose `ask_rate ≤ bid_rate`. On a cross the loan is stamped:

```
lender_rate   = ask_rate
borrower_rate = max(bid_rate, ask_rate + protocol_fee_bps_floor)
```

The bid rate is a *ceiling on the lender rate*; the protocol fee floor is always guaranteed on top. So with `protocol_fee_bps_floor = 50` and a 500 bps bid: an ask at 480 yields `lender_rate = 480`, `borrower_rate = max(500, 530) = 530`; an ask at 400 yields `lender_rate = 400`, `borrower_rate = max(500, 450) = 500`. The borrower can pay up to the floor (50 bps) above their stated bid — they accept the protocol floor as a fee added on top. An ask at 510 does not cross a 500 bid.

**The match still checks LTV.** Every cross verifies `actual_ltv ≤ profile.max_ltv_bps` at oracle prices. A bid whose collateral doesn't satisfy a profile's cap simply skips that ask and walks on.

---

## 3. Yield-alive capital

Fixed-rate orderbooks have a structural problem: lender capital has dead time. Idle vault liquidity waits for a bid to cross (could be hours, could be days). The borrower's collateral sits in escrow until the loan matures. After repay, the lender's principal sits in escrow until it is claimed. Existing fixed-term protocols accept this as the cost of doing business.

yDelta routes **every atom** through marginfi for the entire lifecycle. Vault lenders, borrowers, idle vault liquidity, post-repay claims — all of it sits in marginfi banks earning supply APY by default.

```
   ┌──────────────────┐
   │ Vault depositor  │
   └────────┬─────────┘
            │ deposit USDC into a RiskProfile
            ▼
   ┌──────────────────────┐
   │  marginfi USDC bank  │  ◄── atoms earn supply APY:
   │  (vault integration) │     while asks rest, while
   └──────────┬───────────┘     loans run, while waiting
              │ shares           for claim
              ▼
   ┌──────────────────────┐
   │  RiskProfile pool    │
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

**Place / cancel / update a vault ask:** nothing moves. A vault ask carries no fixed principal and takes no seat encumbrance — it is a pure bookkeeping entry. The profile's `idle_principal_atoms` pool is the backing, and it is read at match time. Yield keeps accruing.

**Match a bid against a vault ask:** the profile's `idle_principal_atoms` decrements and `deployed_principal_atoms` increments; the loan PDA records who owes what. The borrower's withdrawn principal also lands on marginfi (credited to the borrower's seat), so even **borrowed atoms earn supply yield until the borrower pulls them to their wallet**.

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

## 4. Strategy vaults: the lending optimizer

Most depositors don't want to operate like an orderbook market maker. They don't want to decide what rate to quote, which markets to quote in, when to cancel and reprice, or how to balance idle versus deployed capital. They want to deposit, pick a risk style, and earn.

**Strategy vaults are the only way liquidity reaches the book.** Because the orderbook is quote-only, every lender ask comes from a curator-managed risk profile. Depositors fund the profiles; curators do the quoting.

A `GlobalVault` is a pooled lender holding many curator-managed **risk profiles**, where each profile is a distinct lending strategy. What's distinctive: **a single deposit can back multiple strategies at once, and each profile lends across many markets simultaneously.**

```
                          GlobalVault (USDC)
                          ┌──────────────────┐
                          │ idle: $10,000,000│
                          ├──────────────────┤
                          │ Profile A        │  60% max_ltv,
                          │ (conservative)   │  30-day max term
                          ├──────────────────┤
                          │ Profile B        │  75% max_ltv,
                          │ (aggressive)     │  90-day max term
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

A profile may quote any market that shares the vault's mint — there is no fixed cap on the count. Each ask is **unbounded** ("quote all idle"): it carries no principal, and every cross against it is sized at match time by the profile's live `idle_principal_atoms`. The matching engine reads + mutates `GlobalVault` state at match time atomically, so a profile can never accidentally over-commit even with concurrent matches against different markets — the only match gate is `profile_idle ≥ matched_principal`.

The vault-owned `ClaimedSeat` a profile uses in a market is **auto-created** on the first `place_order_for_risk_profile` in that market — there is no separate claim-seat step.

For the depositor, the experience is what they expect from any lending protocol: deposit, choose a risk profile, earn, withdraw.

**Two yield streams, accrued continuously.** A profile earns from two sources at once:

- **Supply yield on idle capital** — funds waiting for matches sit on marginfi and earn supply APY through share-price growth
- **Lender-rate yield on deployed capital** — matched loans earn the locked-in fixed rate

Both stream into the same share-price, so depositors don't have to choose between "idle but liquid" and "earning but locked." Both states are productive. Loan yield is credited **net of `curator_fee_bps`** so the curator's management fee is not double-counted into the share price.

**O(1) per-depositor accounting.** A running aggregate `total_weighted_rate_bps = Σ(P_i × R_i)` across all open loans, plus Aave-style cumulative yield indices, makes `vault_deposit` and `vault_withdraw` O(1) regardless of how many loans the profile has open. A profile with 10,000 active loans accrues yield in the same number of CPU cycles as one with 10. No iteration, ever.

**Vault-wide aggregates.** `GlobalVaultFixed` carries `total_shares`, `total_assets_atoms` and `total_principal_atoms` aggregates, maintained in lockstep with the per-profile fields. Withdraws are gated per-profile (`profile.total_principal − deployed − encumbered ≥ atoms_out`) and against the vault-wide marginfi balance.

A user holds seats in multiple profiles inside the same vault — different strategies, different risk-return tradeoffs, all under one deposit.

---

## 5. `p0` as backstop

Strategy vaults give passive lenders a productive home. `p0` — marginfi — gives the *protocol* a productive home, in two ways.

**As a yield rail.** This is the substrate beneath yield-alive capital (Section 3). Every atom not actively committed to a counterparty sits on marginfi. The orderbook is the optimised path; marginfi is the always-available path *for capital*.

**As a liquidity backstop.** When a borrower's bid exceeds available orderbook liquidity at their rate, the **P2Pool fallback** path fires `marginfi.borrow` against the borrower's marginfi account for the residual. The borrower walks away with the full requested principal — orderbook-matched portion at the locked-in fixed rate, residual at marginfi's variable rate. No partial-fill abandonment, no UX cliff.

```
borrow request (IOC)
    │
    ├── fixed-rate book fills fully     →  fixed loan
    │
    ├── fixed-rate book fills partially →  fixed loan + fallback residual
    │
    └── fixed-rate book does not fill   →  fallback path, or drop
```

The default behaviour is fallback-on. Borrowers who want strict orderbook semantics (`flags = OB_ONLY`) opt out — an unfilled residual simply drops with `OrderFilledIocLog`.

**Voluntary P2Pool repayment.** A borrower on the fallback can repay the variable-rate debt at any time via `repay`. The repayment is made against the borrower marginfi liability directly; `full_repay` re-reads the live liability and retires it to exactly zero. The P2Pool loan PDA closes only once the post-CPI live liability is zero.

**Upgrade path: variable → fixed.** A borrower who took the fallback isn't stuck on the variable rate. `convert_p2pool_to_fixed` walks the asks tree and crosses every compatible vault risk-profile ask whose `rate_bps ≤ max_acceptable_rate_bps` AND `term_seconds ≥ remaining_term`, converting the variable-rate P2Pool debt into fresh fixed-rate `MatchedLoan` queue nodes. Each cross emits a match; the unfilled residual stays on the original P2Pool loan body. Full conversion closes the P2Pool PDA — again, only when the live liability is zero. So the fallback is genuinely a backstop, not a trap.

This is yDelta's strategic posture: **the orderbook is where credit gets priced; marginfi is where credit gets backstopped.** The two layers complement rather than compete.

---

## 6. 0-CPI fast-path

A curator repricing a vault ask does **zero CPIs**. Most lending protocols fire 4-6 CPIs per order mutation (deposit, oracle reads, allocation, etc.). The orders-of-magnitude difference comes from the design: a vault ask is a pure-memory bookkeeping entry — it carries no principal and takes no encumbrance, so placing, cancelling and updating it never touches a bank.

| Operation | CPIs | Notes |
|---|---|---|
| `place_order_for_risk_profile` | **0** | Pure-memory ask insert; auto-creates the vault seat |
| `cancel_order_for_risk_profile` / `update_order_for_risk_profile` | **0** | Pure bookkeeping |
| `place_order` (borrower IOC) crossing a vault ask | 0 (place); 3 (cranker) | Atom migration deferred to a permissionless cranker step |
| `place_order` with P2Pool fallback firing | 2 (`borrow + deposit`) | Residual borrows from marginfi and re-deposits |
| `deposit` / `withdraw` | 1 each | + 1 SPL transfer |
| `repay` | 1 (`marginfi.deposit` or `marginfi.repay`) | + 1 SPL transfer |
| `claim_repayment_for_risk_profile` | 3 | Drains realised atoms back into the vault's marginfi account |
| `process_matched_loan` | 0 | Keeper just mints the loan PDA |
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

Tiered keeper bonus (`bonus_atoms = debt_value_in_collateral × liquidation_keeper_bps / 10_000`) is admin-tunable per market. Keepers race on liquidations the same way they race on Solana arb opportunities — permissionless, performance-optimal. The liquidator's repaid atoms net of the liquidation protocol fee are deposited to the lender side so the fee accumulator stays fully backed.

Two read-only simulation gates — `check_ltv_liquidatable` and `check_maturity_liquidatable` — let keepers and UIs confirm a settlement would succeed before sending the real transaction.

---

## 8. Anatomy of a loan

Consider a 100 USDC loan at 8% APR for 30 days, collateralised with 0.5 SOL.

**1. A depositor funds a risk profile.** `global_vault_deposit(100 USDC, profile_id)`. Atoms hop into the vault's marginfi integration account; the profile's `idle_principal_atoms` pool grows and the depositor is credited with vault shares. **From this moment, the atoms accrue marginfi supply APY.**

**2. The curator posts a vault ask.** `place_order_for_risk_profile(rate=8%, term=30d)`. The vault-owned `ClaimedSeat` for this market is auto-created on first use, then an unbounded `RestingOrder` is inserted into the asks tree. **No CPI fires, no principal is committed, no encumbrance is taken** — the profile's idle pool is the backing.

**3. Borrower deposits collateral.** `deposit(0.5 SOL, is_debt=false)`. Atoms hop into the market's borrower-side marginfi account; the borrower's seat is credited with collateral shares. **Collateral atoms also start earning marginfi supply APY.**

**4. Borrower places an IOC bid.** `place_order(rate=8%, term=30d, principal=100, collateral=0.5)`. The matching engine walks the asks tree, finds the curator's ask (`ask_rate 8% ≤ bid_rate 8%`), checks `actual_ltv ≤ profile.max_ltv_bps`, sizes the cross against the profile's idle pool, debits `idle_principal`, credits `deployed_principal`, and inserts a `MatchedLoan` queue node. The bid does not rest — any unfilled residual routes to the P2Pool fallback or drops.

**5. A keeper promotes the match.** Anyone can call `process_matched_loan(sequence)`. The keeper allocates a fresh `Loan` PDA, stamps the loan's terms (including `lender_rate` and `borrower_rate`), and zeroes the queue node. The keeper pays the PDA rent and is reimbursed at loan close.

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

Spread between borrower rate and lender rate accrues to a market-level protocol-fee bucket, drained periodically by the market admin.

**7. Borrower repays.** `repay(loan, atoms)`. Atoms route to the lender's `GlobalVault`; the loan's `outstanding_debt_atoms` decrements. On full repay the borrower's collateral is released back to their seat.

**8. The vault lender realises the repayment.** A permissionless cranker calls `claim_repayment_for_risk_profile(loan)`. It shifts the lender's seat shares from encumbered → withdrawable, sweeps the loan-body protocol fee onto the market accumulator, drains the realised atoms back into the vault's marginfi account, updates `idle_principal_atoms` / `deployed_principal_atoms`, and closes the loan PDA. Rent is returned to the keeper from step 5.

**9. Or, if the borrower stops responding…** A keeper invokes `settle_matured_loan` (after maturity + grace period) or `liquidate_loan` (if collateral falls below maintenance LTV at oracle prices). Both accept `repay_atoms_max` for partial settlements; both seize collateral, pay off the lender's vault, and return any surplus to the borrower's seat.

---

## 9. Architecture

```
                 ┌──────────────────┐                 ┌──────────────┐
                 │  Vault depositor │                 │   Borrower   │
                 │  + curator       │                 │   wallet     │
                 └────────┬─────────┘                 └──────┬───────┘
                          │ deposit USDC into a              │ deposit SOL
                          │ RiskProfile; curator quotes      │
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
       │           ┌──────────────┐ ┌──────────────────────┐          │
       │           │   Asks tree  │ │    Seats tree        │          │
       │           │ (vault risk- │ │  (per-trader balance │          │
       │           │  profile     │ │   bookkeeping)       │          │
       │           │  RestingOrder│ └──────────────────────┘          │
       │           │  nodes)      │                                    │
       │           └──────┬───────┘                                    │
       │   borrower IOC bid ──► matching engine                         │
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

The book has **one tree of resting orders** — the asks tree, holding only vault risk-profile quotes. A borrower bid is a transient IOC taker that crosses the tree and never rests, so there is no bids tree.

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
- **Match** → on the borrower's seat, collateral shares move `withdrawable → encumbered`; the loan PDA records who owes what (atoms unchanged on chain)
- **Repay** → atoms flow back from the borrower's wallet via `marginfi.deposit`; the vault lender's claim realises onto the vault-owned seat via `claim_repayment_for_risk_profile`

A vault ask takes **no seat encumbrance** — it is backed by the profile's idle pool, sized at match time. Share counts are conserved through the loan lifecycle. Share-price appreciation of the underlying marginfi position accrues continuously to the share holder regardless of bucket.

---

## 10. Under the hood

### Yield decomposition

The protocol decomposes lender yield into two streams so a UI can show them separately:

**Stream 1 — Supply yield (variable, marginfi-driven).** Earned on any atoms sitting on a marginfi account. Yield accrues uniformly by share-price growth on the underlying bank, regardless of whether shares are `withdrawable` or `encumbered`. A depositor whose 100 USDC sits idle while the USDC bank earns 4% supply APY for a month now holds shares worth ≈ 100.33 USDC.

**Stream 2 — Lender-rate yield (fixed, loan-driven).** Earned on the loan's `lender_rate_bps` while the loan is open, credited net of `curator_fee_bps`. Computed lazily at every read/mutation.

**The single rule:** atoms can only earn one stream at a time. Idle on marginfi → supply yield. Committed to a loan → lender rate. Borrowers earn supply yield on borrowed atoms while they're parked on the lender side, naturally hedging their fixed liability against the variable supply rate.

### Unbounded profile asks + match-time atomicity

A vault profile ask rests on the market with **no fixed principal and no per-seat encumbrance** — it is the "quote all idle" model. The profile's `idle_principal_atoms` pool is the backing, and there is at most one live ask per (profile, market).

When a borrower's IOC bid crosses a vault ask, the matching engine reads the `GlobalVault` state, checks the single gate `profile_idle ≥ matched_principal`, verifies `actual_ltv ≤ profile.max_ltv_bps` at oracle prices, and atomically moves `idle_principal → deployed_principal` — all in the same transaction. **Concurrent matches from different markets see the locked pool** because the match-time read-modify-write is single-tx atomic. After the match, atoms migrate `vault.integration → market.lender_integration` via a 3-CPI cranker step.

### Two-step admin transfers + market/global pause

Every admin role (market admin, vault admin, profile curator, protocol-wide admin) has a two-step transfer pattern: an `initiate` ix sets `pending_admin`, an `accept` ix (signed by the new admin) commits. Prevents accidental transfer to a non-controlled key.

Market and protocol-wide pause flags gate every state-mutating ix at the loader level. Markets ship paused-by-default; the documented setup order is **configure the fee config, then unpause** (`set_fee_config` runs while paused — it is an admin-only header mutation). Emergencies can be contained without redeploying code.

### Oracle integration

- **Pyth-Push** (`OracleSetup::PythPushOracle`) — single oracle account, decoded via offset reads on the `PriceUpdateV2` layout. Enforces `MIN_PYTH_PUSH_VERIFICATION_LEVEL = Full`. Partial-verified updates are rejected outright.
- **Switchboard-Pull** (`OracleSetup::SwitchboardPull`) — single oracle account, decoded from `PullFeedAccountData.result.value`.
- **StakedWithPythPush** — three oracle accounts (Pyth feed + LST mint + stake state). The Pyth SOL price is adjusted by `(sol_pool_balance - 1 SOL) / lst_supply` to derive the LST exchange rate.

Confidence-interval rejection on both Pyth (`2.12σ`) and Switchboard (`1.96σ`). Threshold = `bank.config.oracle_max_confidence × price` (default 10%). A bounded future-skew gate rejects readings stamped too far ahead of the clock. A volatile, unconfident, or skewed oracle reading rejects the LTV check rather than producing a bogus number.

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
├── yDelta/
│   └── README.md
├── programs/
│   ├── ydelta/                       # The ydelta on-chain program
│   ├── ydelta-test-harness/          # Test harness program (retiring)
│   └── marginfi-mocks/               # marginfi v0.1.8 type mocks
├── lib/                              # Shared libs (hypertree)
└── scripts/
    ├── build-program.sh
    └── test.sh
```

---
