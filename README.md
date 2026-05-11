# yDelta

**Optimized fixed-rate lending on Solana.**

yDelta treats credit as a two-sided market built on four ideas:

1. **Own-term credit** — both lenders and borrowers define the deal, including LTV
2. **Yield-alive capital** — no atom sits idle; everything earns through marginfi
3. **Strategy vaults** — one deposit, multiple curator-managed strategies
4. **`p0` backstop** — marginfi as the always-available fallback when book liquidity is thin

Lenders post asks (`rate × term × max-LTV`). Borrowers post bids (`rate × term × principal × collateral × borrower-LTV`). A matching engine crosses them into discrete loans with the terms baked in. Everything in between — capital waiting to match, collateral backing an active loan, principal waiting to be claimed — sits in marginfi banks earning supply yield.

---

## Table of contents

- [yDelta](#ydelta)
  - [Table of contents](#table-of-contents)
  - [1. What yDelta does differently](#1-what-ydelta-does-differently)
  - [2. Own-term credit](#2-own-term-credit)
  - [3. Yield-alive capital](#3-yield-alive-capital)
  - [4. Strategy vaults: the lending optimizer](#4-strategy-vaults-the-lending-optimizer)
  - [5. `p0` as backstop](#5-p0-as-backstop)
  - [6. 0-CPI taker fast-path](#6-0-cpi-taker-fast-path)
  - [7. Transferable loans + partial liquidations](#7-transferable-loans--partial-liquidations)
  - [8. Anatomy of a loan](#8-anatomy-of-a-loan)
  - [9. Architecture](#9-architecture)
    - [Two marginfi accounts per market](#two-marginfi-accounts-per-market)
    - [The seat-share invariant](#the-seat-share-invariant)
  - [10. Under the hood](#10-under-the-hood)
    - [Yield decomposition](#yield-decomposition)
    - [Open-ended profile orders + match-time atomicity](#open-ended-profile-orders--match-time-atomicity)
    - [LTV linearity (why the secondary book is cheap)](#ltv-linearity-why-the-secondary-book-is-cheap)
    - [Two-step admin transfers + market/global pause](#two-step-admin-transfers--marketglobal-pause)
    - [Oracle integration](#oracle-integration)
  - [11. Build \& test](#11-build--test)

---

## 1. What yDelta does differently

yDelta is fixed-rate, fixed-term lending built around two ideas that are hard to combine: a real orderbook for price discovery, and a yield rail underneath so capital is never idle. Most fixed-rate protocols ship a subset of what's below. yDelta is designed so all of it is **structural** — not curator-toggles, not optional features, not bolt-ons.

1. **Capital is yield-alive for the entire lifecycle.** Every deposit immediately routes into marginfi at the program level. Resting orders, encumbered collateral, idle vault liquidity, and even borrowed principal earn supply APY by default — until the user actively withdraws to a wallet. There is no "atoms in escrow earning nothing" state anywhere in the protocol.

2. **The orderbook has a built-in variable-rate backstop, and the variable portion is upgradeable.** When fixed-rate liquidity doesn't fill a bid, the residual falls through to `marginfi.borrow` so the borrower walks away with full requested principal — no partial-fill cliff. Later, `convert_p2pool_to_fixed` lets the borrower walk the asks tree and migrate the variable portion back to fixed-rate when better terms appear. The fallback is a backstop, not a one-way commitment.

3. **Vaults run multiple curator strategies on one capital pool per asset.** A single `GlobalVault` per mint hosts many `RiskProfile` entries, each with its own curator, LTV ceiling, term cap, and per-market exposure cap. A depositor can hold seats in multiple profiles inside the same vault; a profile can quote across up to 8 markets simultaneously. Strategy diversity is layered on shared capital, not fragmented into separate vault accounts.

4. **Risk preferences are symmetric — both sides declare LTV.** Lenders set `max_ltv_bps` per profile; borrowers declare their own `borrower_ltv_bps` per bid. Strict transitivity (`actual ≤ borrower ≤ profile-cap ≤ marginfi-init`) lets the orderbook self-segment into explicit risk tiers — conservative bids only cross conservative asks, without an off-chain matchmaker.

5. **Fixed terms genuinely run to maturity.** A loan opens at the locked rate, accrues for the full term, and resolves on borrower repay or keeper settlement after grace. There is no auto-rolling into shorter terms. Repricing is opt-in via `convert_p2pool_to_fixed` (and only for borrowers on the variable-rate fallback), never imposed by the protocol.

6. **Lenders can exit before maturity through the same matching engine.** `SecondaryLoanSale` orders rest on the primary bids tree; the matching engine crosses them against fresh asks at par. The borrower's terms are unchanged because they're contractual. One LTV check at placement covers every full cross and every split — the secondary book reuses the same engine, math, and cranker flow as primary orders.

7. **Designed for orderbook speed.** `place_order`, `cancel_order`, and `update_order` against wallet-side makers fire **zero CPIs** — encumbrance is pure-memory bookkeeping on the market account. Atoms only move on `deposit` / `withdraw` / `repay` / `claim` and the P2Pool fallback. Makers can reprice continuously without paying compute tax for each adjustment.

Taken together: yDelta prices credit on the orderbook, backstops it with marginfi, keeps every atom productive in every state, and lets both sides shape the deal — all built on the same set of mechanisms rather than separate paths bolted together.

---

## 2. Own-term credit

Most lending protocols let lenders or the protocol set risk parameters and force borrowers to accept whatever cap is offered. yDelta inverts that: **borrowers express their own LTV preferences, and matches only fire when both sides agree**.

```
actual_ltv  ≤  borrower_ltv_bps  ≤  vault_profile.max_ltv_bps
```

Strict transitivity creates **risk tiers on the orderbook**. A conservative borrower (`borrower_ltv_bps = 5000`) only crosses conservative lenders. An aggressive borrower (`borrower_ltv_bps = 8000`) finds aggressive lenders. The book segments naturally, both sides self-select, no off-chain matchmaking, no opaque single-protocol risk model.

The default keeps existing UX unchanged — a bid with `borrower_ltv_bps = None` defaults to marginfi's init LTV, so default-bidding borrowers see the same matching behaviour they see today. Risk tiers only activate for borrowers who explicitly opt in.

**Why it matters:** if the oracle moves between bid placement and match, a borrower's collateral might still satisfy a lender's loose cap but breach the borrower's own risk tolerance. Borrower-set LTV lets the bid reject upstream rather than landing them in an unintended position. Pool-based protocols can't do this; even orderbook competitors don't structure the book this way. **It's a feature only a two-sided orderbook can offer.**

---

## 3. Yield-alive capital

Fixed-rate orderbooks have a structural problem: lender capital has dead time. The deposit waits in escrow until a bid crosses (could be hours, could be days). The borrower's collateral sits in escrow until the loan matures. After repay, the lender's principal sits in escrow until they claim. Existing fixed-term protocols accept this as the cost of doing business.

yDelta routes **every atom** through marginfi for the entire lifecycle. Lenders, borrowers, idle vault liquidity, post-repay claims — all of it sits in marginfi banks earning supply APY by default.

```
   ┌──────────────┐
   │ Lender wallet │
   └───────┬───────┘
           │ deposit USDC
           ▼
   ┌──────────────────────┐
   │  marginfi USDC bank  │  ◄── atoms earn supply APY:
   │  (lender side)       │     while orders rest, while
   └──────────┬───────────┘     loans run, while waiting
              │ shares           for claim
              ▼
   ┌──────────────────────┐
   │  Lender's seat       │
   │  withdrawable shares │  ◄── post order: shares move to
   │  encumbered shares   │     `encumbered`. Atoms don't move.
   └──────────────────────┘
              │
              │ match: lender's shares unbind on full repay
              ▼
        Loan accrues
   ┌──────────────────────┐
   │ borrower repays      │  ◄── atoms re-enter marginfi →
   │ atoms route through  │     claim moves them to seat →
   │ marginfi             │     withdraw
   └──────────────────────┘
```

**Cancel an order:** shares move `encumbered → withdrawable`. Atoms don't move. marginfi never knows. Yield keeps accruing.

**Match an order:** shares stay where they are; the loan PDA records who owes what. Atoms don't move. Yield keeps accruing on the lender side. The borrower's withdrawn principal also lands on marginfi (credited to the borrower's seat), so even **borrowed atoms earn supply yield until the borrower pulls them to their wallet**.

There is no "atoms in escrow earning nothing" state in the entire protocol.

The bookkeeping invariant that makes it work:

```
Per-trader balance = marginfi shares on seat
                   = withdrawable_shares + encumbered_shares

Atoms held         = shares × bank.asset_share_value
Yield accrual      = bank.asset_share_value growth
```

Share counts are conserved through `place / cancel / match / update_order`. Atoms only move on `deposit / withdraw / repay / claim`. marginfi handles the yield arithmetic by share-price growth, which applies uniformly regardless of which seat-bucket the shares live in.

---

## 4. Strategy vaults: the lending optimizer

Most depositors don't want to operate like an orderbook market maker. They don't want to decide what rate to quote, which markets to quote in, when to cancel and reprice, or how to balance idle versus deployed capital. They want to deposit, pick a risk style, and earn.

**Without strategy vaults, yDelta would ask passive lenders to behave like traders. With them, yDelta serves both audiences from one venue.**

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
       │  Ask $5M     │  │  Ask $3M     │  │  Ask $2M     │
       └──────────────┘  └──────────────┘  └──────────────┘
```

For the depositor, the experience is what they expect from any lending protocol: deposit, choose a risk profile, earn, withdraw. The matching engine reads + mutates GlobalVault state at match time atomically, so a profile can never accidentally over-commit even with concurrent matches against different markets.

**Two yield streams, accrued continuously.** A profile earns from two sources at once:

- **Supply yield on idle capital** — funds waiting for matches sit on marginfi and earn supply APY through share-price growth
- **Lender-rate yield on deployed capital** — matched loans earn the locked-in fixed rate

Both stream into the same share-price, so depositors don't have to choose between "idle but liquid" and "earning but locked." Both states are productive.

**O(1) per-depositor accounting.** A running aggregate `total_weighted_rate_bps = Σ(P_i × R_i)` across all open loans, plus Aave-style cumulative yield indices, makes `vault_deposit` and `vault_withdraw` O(1) regardless of how many loans the profile has open. A profile with 10,000 active loans accrues yield in the same number of CPU cycles as one with 10. No iteration, ever.

A user holds seats in multiple profiles inside the same vault — different strategies, different risk-return tradeoffs, all under one deposit.

---

## 5. `p0` as backstop

Strategy vaults give passive lenders a productive home. `p0` — marginfi — gives the *protocol* a productive home, in two ways.

**As a yield rail.** This is the substrate beneath yield-alive capital (Section 3). Every atom not actively committed to a counterparty sits on marginfi. The orderbook is the optimised path; marginfi is the always-available path *for capital*.

**As a liquidity backstop.** When a borrower's bid exceeds available orderbook liquidity at their rate, the **P2Pool fallback** path fires `marginfi.borrow` against the borrower's marginfi account for the residual. The borrower walks away with the full requested principal — orderbook-matched portion at the locked-in rate, residual at marginfi's variable rate. No partial-fill abandonment, no UX cliff.

```
borrow request
    │
    ├── fixed-rate book fills fully     →  fixed loan
    │
    ├── fixed-rate book fills partially →  fixed loan + fallback residual
    │
    └── fixed-rate book does not fill   →  fallback path or orderbook-only rest
```

The default behaviour is fallback-on. Borrowers who want strict orderbook semantics (`flags = OB_ONLY`) opt out — unfilled residual rests on the book or drops with `OrderFilledIocLog`.

**Upgrade path: variable → fixed.** A borrower who took the fallback isn't stuck on the variable rate. `convert_p2pool_to_fixed` walks the asks tree and crosses every compatible wallet ask whose `rate_bps ≤ max_acceptable_rate_bps` AND `term_seconds ≥ remaining_term`, converting the variable-rate P2Pool debt into fresh fixed-rate `MatchedLoan` queue nodes. Each cross emits a primary-style match; the unfilled residual stays on the original P2Pool loan body. Full conversion closes the P2Pool PDA. So the fallback is genuinely a backstop, not a trap — when fixed-rate liquidity appears, the borrower can flip their debt over without unwinding.

This is yDelta's strategic posture: **the orderbook is where credit gets priced; marginfi is where credit gets backstopped.** The two layers complement rather than compete.

---

## 6. 0-CPI taker fast-path

A typical taker tx — a borrower placing a bid that crosses a wallet lender's ask in the same market — does **zero CPIs**. Most lending protocols fire 4-6 CPIs per `place_order` (deposit, oracle reads, allocation, etc.). The orders-of-magnitude difference comes from the seat-share-bucket invariant: encumbrance is bookkeeping only, atoms don't move.

| Operation | CPIs | Notes |
|---|---|---|
| `place_order` against same-market wallet liquidity | **0** | Encumbrance is bookkeeping only |
| `place_order` matching a vault-owned maker | 0 (place); 3 (cranker) | Atom migration deferred to a permissionless cranker step |
| `place_order` with P2Pool fallback firing | 2 (`borrow + deposit`) | Residual borrows from marginfi and re-deposits |
| `cancel_order` / `update_order` | 0 | Pure bookkeeping |
| `deposit` / `withdraw` | 1 each | + 1 SPL transfer |
| `repay` (wallet lender) | 1 (`marginfi.deposit`) | + 1 SPL transfer |
| `claim_repayment` | 0 | Pure bookkeeping |
| `process_matched_loan` (wallet) | 0 | Keeper just mints the loan PDA |
| `settle_matured_loan` / `liquidate_loan` | 4 | Partial-by-default |

A market with 100,000 active orders, where every cancel-and-reprice fires zero CPIs, has dramatically lower compute and rent costs than one where each operation roundtrips through the bank.

---

## 7. Transferable loans + partial liquidations

Two distinct mechanisms keep the system safe and liquid without requiring active management.

**Secondary loan book.** A lender who wants to exit early posts a `SecondaryLoanSale` order; the matching engine crosses it against primary asks at par. The cross transfers the loan's `lender_seat_index` and resets the lender rate to the new ask's rate. **The borrower's terms are unchanged** — they're contractual.

The mechanism uses the same matching engine as primary orders (no separate codepath) and benefits from the same 0-CPI taker fast-path. A subtle but important property: secondary crosses are pre-checked for solvency at **bid placement time**, not finalize time. Because LTV is linear in debt, one placement-time check covers the parent loan, every full cross, and every split sub-loan.

**Partial liquidations.** Both `liquidate_loan` and `settle_matured_loan` accept `repay_atoms_max`. A keeper can settle whatever liquidity they have; the loan stays `Active` until outstanding hits 0.

| Outcome | Liquidator seizes | Surplus to borrower | Bad-debt gap |
|---|---|---|---|
| Over-collateralized (typical) | `repay_value + bonus` | `collateral - seized` returned to borrower's seat | 0 |
| Exactly collateralized | `collateral` (full) | 0 | 0 |
| Under-collateralized | `collateral` (capped) | 0 | `(repay_value + bonus) - collateral` logged via `BadDebtLog` |

Tiered keeper bonus (`bonus_atoms = debt_value_in_collateral × liquidation_keeper_bps / 10_000`) is admin-tunable per market. Keepers race on liquidations the same way they race on Solana arb opportunities — permissionless, performance-optimal.

---

## 8. Anatomy of a loan

Consider a 100 USDC loan at 8% APR for 30 days, collateralised with 0.5 SOL.

**1. Lender deposits.** `deposit(100 USDC, is_debt=true)`. Atoms hop into the market's lender-side marginfi account; the lender's `ClaimedSeat` is credited with marginfi shares. **From this moment, the atoms accrue marginfi supply APY.**

**2. Lender posts an ask.** `place_order(side=Ask, rate=8%, term=30d, principal=100)`. Their seat's shares move from `withdrawable` → `encumbered`. **No CPI fires** — encumbrance is bookkeeping only; atoms stay on marginfi earning supply APY.

**3. Borrower deposits collateral.** `deposit(0.5 SOL, is_debt=false)`. Atoms hop into the market's borrower-side marginfi account; the borrower's seat is credited with collateral shares. **Collateral atoms also start earning marginfi supply APY.**

**4. Borrower posts a bid.** `place_order(side=Bid, rate=8%, term=30d, principal=100, collateral=0.5)`. The seat's collateral shares move to `encumbered`. The matching engine sees the cross, removes the lender's ask from the book, and inserts a `MatchedLoan` queue node carrying the matched parameters.

**5. A keeper promotes the match.** Anyone can call `process_matched_loan(sequence)`. The keeper allocates a fresh `Loan` PDA, stamps the loan's terms, and zeroes the queue node. The keeper pays the PDA rent and is reimbursed at loan close.

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

**7. Borrower repays.** `repay(loan, atoms)`. Atoms flow into the market's lender-side marginfi account (or directly to the vault if the lender is a vault profile); the loan's `outstanding_debt_atoms` decrements. On full repay the borrower's collateral is released back to their seat.

**8. Lender claims.** `claim_repayment(loan)`. The lender's `lender_claimable_atoms` (principal + accrued interest) converts to withdrawable shares on their seat. The loan PDA closes; rent is returned to the keeper from step 5. The lender withdraws to a wallet whenever they want.

**9. Or, if the borrower stops responding…** A keeper invokes `settle_matured_loan` (after maturity + grace period) or `liquidate_loan` (if collateral falls below maintenance LTV at oracle prices). Both accept `repay_atoms_max` for partial settlements; both seize collateral, pay off the lender, and return any surplus to the borrower's seat.

---

## 9. Architecture

```
                 ┌──────────────┐                     ┌──────────────┐
                 │   Lender     │                     │   Borrower   │
                 │   wallet     │                     │   wallet     │
                 └──────┬───────┘                     └──────┬───────┘
                        │ deposit USDC                       │ deposit SOL
                        ▼                                    ▼
       ┌─────────────────────────┐          ┌─────────────────────────┐
       │  marginfi (lender side) │          │ marginfi (borrower side)│
       │  — earns supply APY     │          │  — earns supply APY     │
       └────────────┬────────────┘          └────────────┬────────────┘
                    │ shares                             │ shares
                    ▼                                    ▼
       ┌──────────────────────────────────────────────────────────────┐
       │                          Market PDA                           │
       │                                                                │
       │   ┌──────────────┐ ┌──────────────┐ ┌──────────────────────┐ │
       │   │   Bids tree  │ │   Asks tree  │ │    Seats tree        │ │
       │   │  (RestingOr- │ │  (RestingOr- │ │  (per-trader balance │ │
       │   │   der nodes) │ │   der nodes) │ │   bookkeeping)       │ │
       │   └──────┬───────┘ └──────┬───────┘ └──────────────────────┘ │
       │          └─── matching engine ────┘                            │
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

### Two marginfi accounts per market

A market wraps **two** marginfi-account PDAs, not one:

- **Lender side** holds the debt-mint asset (USDC). Lenders deposit here.
- **Borrower side** holds the collateral asset (SOL) and any P2Pool debt liability.

The split sidesteps a marginfi v0.1.8 invariant: a single marginfi account can't simultaneously hold an asset position and a liability position on the same bank. By keeping the lender's USDC asset and the borrower's USDC liability on separate accounts, both flows become unconditional. A `market_signer` PDA at `[b"market_signer", market]` is the authority on both, and on the market's per-mint debt/collateral staging vaults.

### The seat-share invariant

```
Per-trader balance = marginfi shares on ClaimedSeat
   {debt, collateral} × {withdrawable, encumbered}
```

- **Place an order** → `withdrawable - X, encumbered + X` (atoms unchanged on chain)
- **Cancel** → `encumbered - X, withdrawable + X` (atoms unchanged)
- **Match** → `encumbered - X` on lender's seat; corresponding credit on borrower's seat (atoms still unchanged; loan PDA records who owes what)
- **Repay** → atoms flow back from borrower's wallet via `marginfi.deposit`; lender's `claimable_atoms` accrue on the loan; lender claims onto seat

Share counts are conserved through the order lifecycle. Share-price appreciation of the underlying marginfi position accrues continuously to the share holder regardless of bucket.

---

## 10. Under the hood

### Yield decomposition

The protocol decomposes lender yield into two streams so a UI can show them separately:

**Stream 1 — Supply yield (variable, marginfi-driven).** Earned on any atoms sitting on a marginfi account. Yield accrues uniformly by share-price growth on the underlying bank, regardless of whether shares are `withdrawable` or `encumbered`. A lender who deposits 100 USDC and watches the USDC bank earn 4% supply APY for a month now holds shares worth ≈ 100.33 USDC.

**Stream 2 — Lender-rate yield (fixed, loan-driven).** Earned on the loan's `lender_rate_bps` while the loan is open. Computed lazily at every read/mutation.

**The single rule:** atoms can only earn one stream at a time. Idle on marginfi → supply yield. Committed to a loan → lender rate. Borrowers earn supply yield on borrowed atoms while they're parked on the lender side, naturally hedging their fixed liability against the variable supply rate.

### Open-ended profile orders + match-time atomicity

A vault profile order rests on the market with **no per-seat share-backing** — the profile's `idle_principal_atoms` pool is the backing. A single order can be backed by funds across multiple markets, subject to a per-(profile, market) `max_exposure_atoms` cap that lives on the market-side `ClaimedSeat`.

When a borrower's bid crosses a vault-owned ask, the matching engine reads the `GlobalVault` state, verifies gates (`idle ≥ matched && deployed + matched ≤ max_exposure`), and atomically debits the encumbrance — all in the same transaction. **Concurrent matches from different markets see the locked pool** because the match-time read-modify-write is single-tx atomic. After the match, atoms migrate `vault.integration → market.lender_integration` via a 3-CPI cranker step.

### LTV linearity (why the secondary book is cheap)

`required_collateral` is linear in `debt`:

```
required(α·debt) = α·required(debt)
```

So for a secondary cross — where a fraction α of an existing loan's principal transfers to a new lender — the post-cross sub-loan's LTV equals the parent loan's LTV. **One LTV check at bid placement covers the parent loan, every full cross, and every split sub-loan.** That's why `place_secondary_bid` runs the check exactly once and `process_matched_loan` is validation-free for secondary cleanup.

The validation is amortised over the bid's entire lifetime — no per-cross oracle reads, no per-cross weight reads, no per-cross LTV math.

### Two-step admin transfers + market/global pause

Every admin role (market admin, vault admin, profile curator, protocol-wide admin) has a two-step transfer pattern: an `initiate` ix sets `pending_admin`, an `accept` ix (signed by the new admin) commits. Prevents accidental transfer to a non-controlled key.

Market and protocol-wide pause flags gate every state-mutating ix at the loader level. Emergencies can be contained without redeploying code.

### Oracle integration

- **Pyth-Push** (`OracleSetup::PythPushOracle`) — single oracle account, decoded via offset reads on the `PriceUpdateV2` layout. Enforces `MIN_PYTH_PUSH_VERIFICATION_LEVEL = Full`. Partial-verified updates are rejected outright.
- **Switchboard-Pull** (`OracleSetup::SwitchboardPull`) — single oracle account, decoded from `PullFeedAccountData.result.value`.
- **StakedWithPythPush** — three oracle accounts (Pyth feed + LST mint + stake state). The Pyth SOL price is adjusted by `(sol_pool_balance - 1 SOL) / lst_supply` to derive the LST exchange rate.

Confidence-interval rejection on both Pyth (`2.12σ`) and Switchboard (`1.96σ`). Threshold = `bank.config.oracle_max_confidence × price` (default 10%). A volatile or unconfident oracle reading rejects the LTV check rather than producing a bogus number.

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