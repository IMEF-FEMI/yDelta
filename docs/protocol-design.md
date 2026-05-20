# yDelta Protocol Design

yDelta runs a **quote-only** orderbook: the book holds only lender
quotes, and those quotes come only from vault risk-profile curators.
Borrowers do not rest orders — a borrow request is immediate-fill (IOC).

## Summary

yDelta is a fixed-rate, fixed-term lending protocol on Solana that prices credit on a quote-only orderbook rather than a shared utilization curve.

The protocol is built around three ideas:

1. The orderbook holds lender quotes only — vault risk-profile curators quote; borrowers fill immediately.
2. No capital should sit idle.
3. One vault should be able to run multiple strategies.

Plus a fourth, supporting idea: the orderbook should have a practical fallback when direct fixed-rate liquidity is not enough.

In short:

**Quote-Only Credit + Yield-Alive Capital + Strategy Vaults + `p0` Backstop**

This document is the deeper engineering companion to the README. The README sells the thesis; this doc closes the math.

---

## What yDelta does differently

A fixed-rate lending protocol can ship any subset of the properties below. yDelta is designed so that all of them are **structural** — not curator-toggles, not optional features, not bolt-ons that activate on configuration.

**Capital is yield-alive across the entire lifecycle.** Every atom under protocol control sits on marginfi. This applies to lender capital waiting for a match, borrower collateral securing an active loan, idle vault liquidity, and principal a borrower has drawn but not yet withdrawn to their wallet. The protocol has no notion of "in-escrow but not earning."

**The orderbook has a structural variable-rate backstop, and the variable portion is upgradeable.** Borrow intent that exceeds available fixed-rate liquidity falls through to marginfi at variable rate, so the borrower never faces a partial-fill cliff. When better fixed-rate terms appear later, the borrower can opportunistically convert the variable portion back to fixed-rate. The fallback is a backstop, not a one-way commitment.

**Vaults express multiple curator strategies on one capital pool per asset.** A single `GlobalVault` per mint hosts many independent `RiskProfile`s, each with its own curator, LTV ceiling and term cap. A depositor can hold positions in multiple profiles inside the same vault; a profile can quote on any market that shares the vault's mint, with no fixed cap on how many. Strategy diversity does not require fragmenting depositor capital into separate vault accounts.

**The book is quote-only.** Lender quotes come only from vault risk-profile curators — there are no wallet makers and no market-direct quotes. A borrower never rests an order: a borrow request is an immediate-or-cancel (IOC) bid that crosses resting risk-profile asks in one transaction. The matching engine therefore has a single direction — taker is always a borrower bid, makers are always vault asks — which removes an entire class of stale-order and self-cross edge cases.

**Fixed terms run to maturity.** A loan opens at the locked rate, accrues for the full term, and resolves on borrower repay or keeper settlement after grace. There is no auto-rolling into shorter terms. Repricing is opt-in (and only available to borrowers on the variable-rate fallback), never imposed by the protocol.

**Curator order placement is zero-CPI.** A vault ask is a pure-memory bookkeeping entry — it carries no fixed principal and takes no seat encumbrance. Placing, canceling, and updating a vault ask fires no CPIs — only deposit, withdraw, repay, claim, and the P2Pool fallback path touch external programs. Curators can reprice continuously without compute tax per adjustment.

The combination is the protocol's identity: credit is priced on a quote-only orderbook, backstopped by marginfi, kept productive in every state, and quoted by curator-run strategies — all built on one shared set of mechanisms.

---

## 1. Market model

At the highest level, yDelta matches:

- **lender supply:** vault risk-profile asks — fixed rate, fixed term, unbounded size
- **borrower demand:** an immediate-or-cancel bid — fixed rate, fixed term, principal, collateral

The match creates a discrete loan rather than adding both sides to a pooled balance sheet.

```text
Lender quote (resting):
  rate, term — placed by a vault risk-profile curator

Borrower intent (IOC, never rests):
  rate, term, principal, collateral

Match result:
  fixed loan with locked terms
```

Only one side rests. The orderbook holds lender quotes only, and every quote belongs to a `RiskProfile` inside a `GlobalVault`. A borrow request is not a resting order — it is an immediate-fill taker that crosses the asks tree in the same transaction. The borrower is not simply choosing "how much to borrow from a pool"; the borrower is choosing the exact credit shape they are willing to enter, and either fills it now or routes the residual to the fallback.

---

## 2. Quote-only credit and rate matching

The orderbook holds nothing but lender asks, and every ask belongs to a vault risk profile. The profile carries a curator-set `max_ltv_bps` — the **lender-side** LTV ceiling — and a `max_term_seconds` cap. The matching engine reads `max_ltv_bps` live from the profile at match time, so a curator's policy change takes effect immediately with no per-seat re-sync.

A borrower's IOC bid crosses an ask when:

```text
ask_rate ≤ bid_rate
```

On a cross the loan is stamped:

```text
lender_rate   = ask_rate
borrower_rate = max(bid_rate, ask_rate + protocol_fee_bps_floor)
```

The bid rate is a **ceiling on the lender rate**; the protocol fee floor is always guaranteed on top. With `protocol_fee_bps_floor = 50` and a 500 bps bid:

| Lender ask | Match? | `lender_rate` | `borrower_rate` | Protocol take |
|---|---|---|---|---|
| 500 | yes | 500 | `max(500, 550) = 550` | 50 |
| 480 | yes | 480 | `max(500, 530) = 530` | 50 |
| 400 | yes | 400 | `max(500, 450) = 500` | 100 |
| 510 | no (`500 < 510`) | — | — | — |

The borrower can pay up to the floor (50 bps) above their stated bid — they accept the protocol floor as a fee added on top. This construction structurally guarantees `borrower_rate ≥ lender_rate + floor`.

**The match still checks LTV.** Every cross verifies `actual_ltv ≤ profile.max_ltv_bps` at oracle prices. A bid whose collateral does not satisfy a profile's cap simply skips that ask and walks on to the next. There is no borrower-declared LTV — the lender-side cap is the only LTV gate.

---

## 3. Yield-alive capital

yDelta is designed so that capital remains productive across the lifecycle instead of being trapped in dead escrow:

```text
capital waiting to match     →  productive
collateral securing a loan   →  productive
capital awaiting withdrawal  →  productive
```

This is especially important on the borrower side. In many systems, collateral becomes economically silent once posted. In yDelta, borrower collateral is routed through marginfi, which means it can help offset the borrower's effective rate instead of remaining inert.

"No capital sits idle" is not marketing language here. It is a design principle.

---

## 4. The strategy-vault model

yDelta uses one `GlobalVault` per lending asset and supports multiple curator-managed risk profiles inside that vault.

```text
                    GlobalVault
                 (one vault per asset)
                           |
        ------------------------------------------------
        |                      |                      |
   Risk Profile 0         Risk Profile 1         Risk Profile 2
   lower LTV / term       medium LTV / term      higher LTV / term
        |                      |                      |
   market seats            market seats            market seats
        |                      |                      |
   quoted liquidity       quoted liquidity       quoted liquidity
```

Three advantages fall out of this structure:

- depositors do not need a new vault for every strategy
- curators can express multiple credit styles from one capital base
- liquidity fragmentation is reduced

Vault-side accounting can be summarized as:

```text
idle_principal = total_principal - deployed_principal - encumbered_in_orders
```

Where:

- `deployed_principal` funds active loans
- `encumbered_in_orders` backs live quoted liquidity
- `idle_principal` remains available for new matches

---

## 5. Why global vaults are needed

The average lending user does not want to operate like an orderbook market maker.

A direct yDelta lender has to think about:

- what rate to quote
- what duration to quote
- what markets to quote in
- when to cancel, reprice, or move capital
- how to balance idle capital versus deployed capital
- how to keep earnings productive while waiting for matches

That is powerful, but it is not how a typical lending user behaves.

The typical lending user wants something much simpler:

- deposit capital
- choose a risk style
- let the protocol keep the capital productive
- earn without actively managing quotes

That is exactly what global vaults provide.

They bridge the gap between:

- a traditional lending-protocol user, who wants to lend and forget
- an orderbook-based fixed-rate venue, where active quote management would otherwise be required

In other words, the global vault turns yDelta from a venue only for active lenders into a venue that can also serve passive lenders.

---

## 6. How a global vault works for the depositor

For the depositor, the vault experience is simple:

1. deposit into a chosen risk profile
2. receive vault shares
3. let the curator and profile logic deploy capital
4. redeem shares later for a larger atom balance if the strategy performed well

The depositor is not manually posting asks on the market. The profile does that on their behalf.

This is why a global vault is best described as a **lending optimizer**:

- idle capital remains productive
- quoted capital can be deployed across multiple markets
- active loans earn fixed lender-rate yield
- repayments recycle back into the same strategy

The depositor gets strategy exposure without having to manage the strategy.

---

## 7. The share model: what a vault user owns

Each risk profile has:

- `total_shares`
- `total_assets_atoms`
- `total_principal_atoms`

When a user deposits, they receive shares in that profile. The mint formula is:

```text
if total_shares == 0:
    shares_minted = atoms_in
else:
    shares_minted = atoms_in × total_shares / total_assets
```

The depositor owns a pro-rata claim on the profile, not on one individual loan.

At any point, the depositor's gross vault value is:

```text
user_value_atoms = user_shares × total_assets_atoms / total_shares
```

That is the most direct answer to "what does a global vault user own?" They own a proportional slice of the profile's asset base.

---

## 8. What counts as depositor profit

For a vault user, profit is the growth of their share-backed claim over time.

A useful approximation is:

```text
user_profit_atoms = current_user_value_atoms − user_principal_basis_atoms
```

Where:

- `current_user_value_atoms = user_shares × total_assets_atoms / total_shares`
- `user_principal_basis_atoms` is the depositor's effective cost basis in the profile

At the profile level, the system tracks both:

- `total_assets_atoms` — current economic value of the profile
- `total_principal_atoms` — principal base currently attributed to the profile

`total_assets_atoms` captures economic growth from yield accrual. `total_principal_atoms` tracks the principal pool used for idle-capital gating and realized capital accounting.

So from the depositor's perspective:

- profit shows up through rising share value
- realized pool capital is updated as loans close and cash returns

---

## 9. Where depositor profit comes from

Two yield streams accrue into each profile.

### 9.1 Supply yield on idle capital

Idle capital remains on marginfi:

```text
idle_principal = total_principal - deployed_principal - encumbered_in_orders
```

Supply yield is derived from the underlying bank share-value growth:

```text
growth = current_share_value / last_share_value − 1
idle_yield = idle_principal × growth
```

The protocol uses the underlying `asset_share_value` snapshot and applies the ratio delta to idle atoms.

Interpretation:

- capital waiting for deployment is still earning
- capital not yet matched is not wasted
- the vault behaves more like a productive reserve than a dead cash pile

### 9.2 Lender-rate yield on deployed capital

When vault capital is matched into active fixed-rate loans, the profile earns lender-side loan yield. The profile tracks:

```text
total_weighted_rate_bps = Σ(loan_principal × lender_rate_bps)
```

So the deployed-loan yield over elapsed time is:

```text
loan_yield = total_weighted_rate_bps × elapsed
             / (10_000 × seconds_per_year)
```

This is a key optimization. The protocol does not need to iterate every open loan to estimate depositor earnings. It can accrue the whole profile in O(1) using the running aggregate.

### 9.3 Combined profile growth

```text
total_yield = idle_yield + loan_yield
total_assets_after = total_assets_before + total_yield
```

That is the heart of the global-vault profit model. The depositor earns from productive idle capital and fixed-rate deployed loans simultaneously. The vault is not choosing between "idle but liquid" and "earning but locked." Both states are productive.

---

## 10. Why this is a better experience for passive lenders

For a passive lender, the vault is doing several jobs at once.

**It abstracts quote management.** The depositor does not need to decide what rate to post, which market to post into, whether to cancel or move a quote, or how much duration risk to take in each market. The risk profile and its curator handle that.

**It preserves productive capital.** Passive users expect a lending protocol to keep deposits working by default — that expectation comes from pool-based lending UX. Global vaults preserve that intuition inside yDelta: deposit once, let the vault keep capital productive, let the strategy deploy when opportunities appear.

**It turns yDelta into a familiar lender UX.** Without global vaults, yDelta would ask passive lenders to behave like traders. With global vaults, yDelta can offer an experience much closer to a traditional lending protocol: deposit, select risk profile, earn, withdraw when liquidity is available. That is the bridge between conventional lending UX and orderbook-based fixed-rate credit.

---

## 11. Withdrawal math and liquidity constraints

When a depositor withdraws, the profile computes:

```text
atoms_out = shares_burned × total_assets_atoms / total_shares
```

Withdrawal value is based on the depositor's pro-rata share of total economic assets. But there is an important liquidity constraint:

```text
idle_principal ≥ atoms_out
```

The protocol will not let a user withdraw capital that is currently:

- deployed inside active loans
- reserved by quoted liquidity

This is an important distinction:

- economic value can accrue continuously
- immediate redeemability depends on idle principal and realized cash flow

A vault depositor can be earning while part of the profile is deployed, but cannot necessarily redeem all of that economic value until capital becomes idle again through cancellations, repayments, or settlement.

That is the right tradeoff for a lending optimizer. The profile stays invested when opportunities exist, but still preserves a clear accounting rule for redemption.

---

## 12. Realized versus accrued profit

The code distinguishes accrued earnings from realized capital.

**Accrued earnings** are reflected through:

- `total_assets_atoms` growth
- cumulative yield indices
- rising share value

This is the economic profit the user has earned so far.

**Realized earnings** are reflected when repayment cash actually returns to the vault and the profile state is updated on claim. At that point:

- deployed principal falls
- weighted-rate contribution from the closed loan is removed
- realized interest increases the profile's principal base
- any shortfall reduces that principal base

So the depositor's vault return is economically visible before final repayment, but the principal pool is only fully refreshed when cash comes home.

---

## 13. Global vault profit as a lending-optimizer equation

A simple way to express the depositor outcome is:

```text
vault_user_return = productive_idle_yield
                  + fixed_loan_lender_yield
                  − realized_shortfalls
                  − any strategy-level fee drag
```

And the user's redeemable value is:

```text
redeemable_value = user_shares / total_shares × total_assets_atoms
```

subject to:

```text
idle_principal ≥ requested_atoms_out
```

This captures the whole point of the global vault:

- it keeps idle capital productive
- it deploys capital into fixed-rate loans
- it socializes those outcomes pro-rata to depositors
- it removes the need for each depositor to actively manage orders

---

## 14. `p0` as backdrop and fallback

The orderbook is the primary venue for fixed-rate matching. But credit markets need a backstop when book liquidity is thin.

yDelta uses `p0` (marginfi) as that backdrop through the protocol's `P2Pool` fallback path:

```text
borrow request
    │
    ├── fixed-rate book fills fully     →  fixed loan
    │
    ├── fixed-rate book fills partially →  fixed loan + fallback residual
    │
    └── fixed-rate book does not fill   →  fallback path or orderbook-only rest
```

This does two things for the protocol:

1. it reduces failed borrow intent when direct fixed-rate liquidity is not enough
2. it gives yDelta a strategic integration posture for future ecosystem connectivity

Using `p0` is not only about fallback liquidity today. It is also about choosing a rail that makes broader Solana protocol access easier to build around over time.

### 14.1 Upgrading variable-rate debt to fixed-rate

The fallback path is reversible. A borrower who took on `P2Pool` debt is not locked into the variable rate for the loan's life.

`convert_p2pool_to_fixed` lets a borrower walk the asks tree against their existing P2Pool position and convert any portion that crosses into fresh fixed-rate loan bodies. The cross gate is:

```text
ask.rate_bps      ≤ max_acceptable_rate_bps
ask.term_seconds  ≥ remaining_term_of_p2pool_loan
```

Each successful cross emits a fresh `Fixed` `MatchedLoan` queue node. Any unfilled residual stays on the original P2Pool body. Full conversion closes the P2Pool PDA — but only when the post-CPI live marginfi liability is genuinely zero, so a residual variable position can never be silently orphaned.

Conceptually:

- the fallback is a backstop, not a one-way commitment
- borrowers can opportunistically reprice when fixed-rate liquidity appears
- the same matching engine is reused — no separate "refinance" codepath

This closes the loop on the two-layer posture: variable-rate borrows are not strictly worse than fixed-rate borrows, because the borrower can migrate up whenever the book offers them better terms.

The strategic posture: **the orderbook is where credit gets priced; marginfi is where credit gets backstopped.** The two layers complement rather than compete.

---

## 15. Capital flow relationships

The protocol can be pictured as two productive sides feeding a match engine.

```text
 Lender wallet                            Borrower wallet
      │                                         │
      ▼                                         ▼
 productive debt-side rail              productive collateral-side rail
      │                                         │
      ▼                                         ▼
 lender seat                              borrower seat
      │                                         │
      └────────────── orderbook / matcher ──────┘
                             │
                             ▼
                        matched loan
                             │
           ┌─────────────────┴─────────────────────────┐
           │                                           │
           ▼                                           ▼
      repayment path                          keeper intervention path
           │                                           │
           ▼                                           ▼
   lender claimable balance                  partial settle / liquidate
```

This framing captures the real relationship more clearly than an escrow-first mental model. The market is not primarily about moving tokens in and out of dead holding areas. It is about managing productive balances, matching intent, and crystallizing those balances into fixed credit exposure when the terms align.

---

## 16. Loan economics

For a fixed loan, the borrower owes fixed debt growth over the term and the lender earns the lender-side fixed rate:

```text
borrower_interest = principal × borrower_rate × elapsed / year
lender_interest   = principal × lender_rate   × elapsed / year
spread            = borrower_interest − lender_interest
```

Conceptually:

- borrower debt grows at the borrower-side rate
- lender claim grows at the lender-side rate
- the spread is reserved for protocol-level fee logic

The product takeaway is that yDelta separates three things clearly:

- the borrower's contractual fixed cost
- the lender's contractual fixed return
- the productive background yield of parked capital before and after matching

---

## 17. Partial settlement and liquidation

Real markets need flexible unwind paths. yDelta supports:

- partial settlement on debt repayment paths
- partial liquidation on distressed paths

Resolution does not need to be all-or-nothing. Positions can be reduced in steps rather than requiring one full close event every time.

---

## 18. Execution efficiency

Fixed-rate orderbooks on Solana have to do orderbook work at orderbook speed. yDelta hits this by keeping `place_order_for_risk_profile` / `cancel_order_for_risk_profile` / `update_order_for_risk_profile` as pure bookkeeping on the market account.

A vault ask is a pure-memory entry: it carries no fixed principal and takes no seat encumbrance. The profile's `idle_principal_atoms` pool is the backing, read at match time. So:

```text
placing or repricing a vault ask is a tree mutation on the market account,
not a token transfer that has to round-trip through a bank.
```

Operationally:

- `place_order_for_risk_profile` fires **zero CPIs** (it also auto-creates the vault seat on first use)
- `cancel_order_for_risk_profile` and `update_order_for_risk_profile` fire **zero CPIs**
- a borrower IOC bid that crosses a resting ask fires **zero CPIs** at placement; atom migration is deferred to a 3-CPI permissionless cranker step
- only `deposit`, `withdraw`, `repay`, the cranker realization, and the P2Pool fallback path actually need to touch marginfi

The consequence is a market that can hold orders of magnitude more live state per unit of compute. A book with thousands of resting vault asks, where every cancel-and-reprice is a pure-memory mutation, costs dramatically less than one where every operation has to round-trip through an external lending pool.

This matters for liquidity. Curators reprice constantly. A protocol that taxes each reprice with CPI overhead pushes curators to quote wider, less often, in fewer markets. yDelta is designed so that quote churn is structurally cheap, which means the book can carry tighter, deeper, more responsive liquidity.

---

## 19. Two marginfi accounts per market

A single marginfi v0.1.8 account cannot simultaneously hold an asset position and a liability position on the same bank. That constraint matters here because a yDelta market has both:

- lender USDC sitting as an asset on the debt bank
- borrower USDC liability (via P2Pool fallback) on the same debt bank

If both lived on the same account, the second flow would be blocked by marginfi's per-`(account, bank)` mutual exclusion.

yDelta sidesteps the constraint by wrapping **two** marginfi accounts per market:

- a lender-side account that holds the debt-mint asset
- a borrower-side account that holds the collateral asset and any P2Pool debt liability

Both accounts have the same authority (`market_signer`), so the program can sign for either side. The split is invisible to users — they see a single market, deposit and withdraw normally — but it is what lets the protocol express both halves of a credit flow against the same underlying bank without giving up the yield-alive property on either side.

This is the cleanest example of how yDelta's design works with marginfi's constraints rather than around them.

---

## 20. Admin and pause primitives

Every admin role in the protocol — market admin, vault admin, profile curator, protocol-wide admin — uses a two-step transfer pattern:

```text
initiate_transfer  →  sets pending_admin
accept_transfer    →  pending_admin signs to commit
```

This prevents the most common admin-key footgun: a transfer to a non-controlled key. A typo in the initiator's instruction data cannot brick the role, because nothing has changed yet on the receiving side. The would-be successor has to actively sign before they take over.

There are also two kill switches:

- **per-market pause** — admin-set; while on, every state-mutating ix on that market rejects, while read-only ixs (mirror sync, simulation gates) stay live
- **global pause** — protocol-admin-set; same gating, but applied at the loader level across every ix that takes the `global_config` account

The design intent is that emergencies can be contained without redeploying the program. A market that loses oracle freshness, a vault that hits an accounting anomaly, a marginfi-side issue that needs to be quarantined — any of these can be frozen at the affected scope while the rest of the protocol keeps running.

Fresh markets ship paused by default. The documented setup order is **configure the fee config, then unpause** — `set_fee_config` is an admin-only header mutation with no atom flow and is intentionally allowed to run during the paused setup window. The admin unpauses only after verifying fee config, marginfi wiring, and oracle plumbing. This is defense-in-depth against the "fresh keypair every run" hazard in setup scripts.

---

## 21. Oracle integration

LTV math is only as trustworthy as the price feeds underneath it. yDelta accepts three oracle shapes through the marginfi adapter:

- **Pyth-Push** — single oracle account; rejects partial-verified updates outright (`MIN_PYTH_PUSH_VERIFICATION_LEVEL = Full`)
- **Switchboard-Pull** — single oracle account; decoded from the pulled feed's result value
- **StakedWithPythPush** — three accounts (Pyth feed + LST mint + stake state); the Pyth SOL price is adjusted by the stake-pool's accounting to derive the LST exchange rate

Every oracle read passes a confidence-interval check before LTV math runs. The threshold is `bank.config.oracle_max_confidence × price` (default 10%). A bounded future-skew gate rejects readings stamped too far ahead of the on-chain clock. A volatile, unconfident, or skewed reading rejects the LTV gate rather than producing a degraded number.

The design intent is uniform across feeds: a stale, retracted, low-confidence, or future-skewed price is **not** a price for purposes of LTV. The match-time check fails closed. This pushes the failure mode toward "loan doesn't open" rather than "loan opens at the wrong LTV" — the safer of the two.

---

## 22. Reading the implementation

The codebase maps closely to the design:

- `programs/ydelta/src/program/processor/place_order.rs` — borrower IOC bid flow and P2Pool fallback routing
- `programs/ydelta/src/program/processor/place_order_for_risk_profile.rs` — vault curator ask placement (auto-creates the vault seat)
- `programs/ydelta/src/program/processor/convert_p2pool_to_fixed.rs` — the variable-to-fixed upgrade path
- `programs/ydelta/src/state/market_helpers.rs` — the quote-only matching engine
- `programs/ydelta/src/state/market.rs` — market-level state, fee configuration, split integration accounts
- `programs/ydelta/src/state/vault.rs` — `GlobalVault` and risk-profile accounting
- `programs/ydelta/src/state/loan.rs` — promoted fixed-loan state
- `programs/ydelta/src/protocol/marginfi.rs` — marginfi v0.1.8 adapter, oracle confidence checks
- `programs/ydelta/tests/cases/` — lifecycle and mechanism coverage, including vaults, fallback, and liquidation

---

## Closing

yDelta is not trying to be another generic lending pool.

It is designed as a capital-efficient fixed-rate credit market where:

- credit is priced on a quote-only orderbook
- collateral remains economically useful
- one vault can express multiple strategies
- the orderbook has a pragmatic fallback

That combination is the protocol's identity.