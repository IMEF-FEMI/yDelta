# yDelta Protocol Design

## Summary

yDelta is a fixed-rate, fixed-term lending protocol on Solana that treats credit as a two-sided market rather than a shared utilization curve.

The protocol is built around four ideas:

1. Both lenders and borrowers should be able to define the deal.
2. No capital should sit idle.
3. One vault should be able to run multiple strategies.
4. The orderbook should have a practical fallback when direct fixed-rate liquidity is not enough.

In short:

**Own-Term Credit + Yield-Alive Capital + Strategy Vaults + `p0` Backstop**

This document is the deeper engineering companion to the README. The README sells the thesis; this doc closes the math.

---

## 1. Market model

At the highest level, yDelta matches:

- **lender supply:** fixed rate, fixed term
- **borrower demand:** fixed rate, fixed term, principal, collateral, borrower-defined LTV

The match creates a discrete loan rather than adding both sides to a pooled balance sheet.

```text
Lender intent:
  rate, term

Borrower intent:
  rate, term, principal, collateral, borrower_ltv

Match result:
  fixed loan with locked terms
```

The borrower is not simply choosing "how much to borrow from a pool." The borrower is choosing the exact credit shape they are willing to enter.

---

## 2. Credit symmetry

One of yDelta's core ideas is that the borrower should have a direct say in risk tolerance, not just the lender or the protocol.

The match condition is:

```text
actual_ltv ≤ borrower_ltv ≤ lender_max_ltv
```

Where:

- `actual_ltv` is implied by principal, collateral, and oracle prices
- `borrower_ltv` is the borrower's declared LTV ceiling
- `lender_max_ltv` is the maximum LTV accepted by the lending side (set by the active risk profile, for vault-backed lending)

This creates a more expressive market structure:

- conservative borrowers can limit their own leverage
- conservative lenders can enforce tighter risk profiles
- aggressive borrowers must find matching liquidity willing to accept that risk

That is not just a validation rule. It is part of the protocol's market design.

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

## 17. Secondary lender exit

Fixed-rate products become more useful when the lender is not locked until maturity.

yDelta supports a secondary lender exit path so lenders can transfer fixed exposure before the loan matures. The protocol is not only matching new credit — it is also supporting the transfer of existing credit.

Strategically, this matters because a fixed-rate venue becomes much more attractive when "lend" does not also mean "lose optionality until maturity."

---

## 18. Partial settlement and liquidation

Real markets need flexible unwind paths. yDelta supports:

- partial settlement on debt repayment paths
- partial liquidation on distressed paths

Resolution does not need to be all-or-nothing. Positions can be reduced in steps rather than requiring one full close event every time.

---

## 19. Reading the implementation

The codebase maps closely to the design:

- `programs/ydelta/src/program/processor/place_order.rs` — primary order flow, borrower LTV checks, fallback routing
- `programs/ydelta/src/state/market.rs` — market-level state and fee configuration
- `programs/ydelta/src/state/vault.rs` — `GlobalVault` and risk-profile accounting
- `programs/ydelta/src/state/loan.rs` — promoted fixed-loan state
- `programs/ydelta/tests/cases/` — lifecycle and mechanism coverage, including vaults, fallback, borrower LTV, secondary flows, and liquidation

---

## Closing

yDelta is not trying to be another generic lending pool.

It is designed as a capital-efficient fixed-rate credit market where:

- both sides can define the deal
- collateral remains economically useful
- one vault can express multiple strategies
- the orderbook has a pragmatic fallback

That combination is the protocol's identity.