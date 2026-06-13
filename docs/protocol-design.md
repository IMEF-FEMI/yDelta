# yDelta Protocol Design

yDelta runs a **two-sided** fixed-rate orderbook. The ask side holds
lender quotes, and those quotes come only from vault **sub-vaults** —
curator-run (Pool) or owner-run (Private). The bid side holds borrower
orders: a borrow request crosses resting asks immediately, and any
unfilled remainder can fall through to a variable-rate backstop, rest on
the book as a standing bid, or drop. Resting bids are crossed by later
ask placements and by a permissionless crank.

## Summary

yDelta is a fixed-rate, fixed-term lending protocol on Solana that prices
credit on an orderbook rather than a shared utilization curve.

The protocol is built around four ideas:

1. The book is two-sided — sub-vault curators quote asks, borrowers post
   bids — but every ask still originates from vault capital, never a bare
   wallet maker.
2. No capital should sit idle.
3. One vault should be able to run multiple strategies.
4. The orderbook should have a practical fallback when direct fixed-rate
   liquidity is not enough.

In short:

**Two-Sided Orderbook Credit + Yield-Alive Capital + Strategy Sub-Vaults + `p0` Backstop**

This document is the deeper engineering companion to the README. The
README sells the thesis; this doc closes the math.

---

## What yDelta does differently

A fixed-rate lending protocol can ship any subset of the properties
below. yDelta is designed so that all of them are **structural** — not
curator-toggles, not optional features, not bolt-ons that activate on
configuration.

**Capital is yield-alive across the entire lifecycle.** Every atom under
protocol control sits on marginfi. This applies to lender capital waiting
for a match, borrower collateral securing an active loan, idle vault
liquidity, and principal a borrower has drawn but not yet withdrawn to
their wallet. The protocol has no notion of "in-escrow but not earning" —
including a borrower's bid that rests on the book: its collateral keeps
earning marginfi supply yield while it waits.

**The orderbook has a structural variable-rate backstop, and the variable
portion is upgradeable.** Borrow intent that exceeds available fixed-rate
liquidity can fall through to marginfi at variable rate, so the borrower
never faces a partial-fill cliff. When better fixed-rate terms appear
later, the borrower can opportunistically convert the variable portion
back to fixed-rate. The fallback is a backstop, not a one-way commitment.

**Vaults express multiple curator strategies on one capital pool per
bank.** A single `GlobalVault` per marginfi **bank** hosts many
independent **sub-vaults**, each with its own operator, spread, LTV
ceiling, liquidation threshold and term cap. Sub-vaults come in two
kinds: **Pool** (protocol-admin-created, curator-run, pooled deposits)
and **Private** (permissionlessly created, single-owner — the owner is
the curator). A depositor can hold positions in multiple sub-vaults
inside the same vault; a sub-vault can quote on any market whose debt
side is that bank, with no fixed cap on how many. Strategy diversity does
not require fragmenting depositor capital into separate vault accounts.

**The book is two-sided, but asks are vault-only.** Ask-side quotes come
only from vault sub-vaults — there are no wallet makers and no
market-direct asks. Borrowers post bids: an immediate-or-cancel bid
crosses resting sub-vault asks in one transaction, and its unfilled
residual chooses one of three fates — fall through to the variable-rate
backstop, rest on the bid side of the book, or drop. A resting bid is
later crossed by a fresh or repriced sub-vault ask (which *takes* on
placement) or by anyone running the permissionless **match crank**.
Self-trading is blocked at the **owner** level: a wallet's bid never
fills against a sub-vault that same wallet curates — that pair is skipped,
not aborted, so the scan walks on to a real counterparty.

**Rates are quoted as a spread over the live bank rate.** A sub-vault
does not post an absolute rate. It stores a `spread_bps`, and at
placement the program reads the debt bank's live marginfi lending APR and
stores `lending_APR + spread` in the order. Repricing a market is a
parameterless re-sync. A fill-time floor enforces the same idea on the
way out: the matching engine skips any resting ask whose stored rate has
fallen below the *current* bank lending APR, so a stale quote never fills
below market.

**Fixed terms run to maturity.** A loan opens at the locked rate, accrues
for the full term, and resolves on borrower repay or keeper settlement
after grace. There is no auto-rolling into shorter terms, and there is no
prepayment fee — early repay is free. Repricing is opt-in (and only
available to borrowers on the variable-rate fallback), never imposed by
the protocol.

**Curator order placement is near-zero-CPI.** A vault ask is a
pure-memory bookkeeping entry — it carries no fixed principal and takes no
seat encumbrance. Placing, canceling, and re-syncing a vault ask fires no
external CPIs; even when a placement *takes* a crossable resting bid, the
fill is recorded as a queued `MatchedLoan` node and the atom movement is
deferred to a permissionless cranker. Only deposit, withdraw, repay,
claim, the cranker realization, and the P2Pool fallback path touch
external programs. Curators can reprice continuously without a compute tax
per adjustment.

The combination is the protocol's identity: credit is priced on a
two-sided orderbook, backstopped by marginfi, kept productive in every
state, and quoted by curator-run strategies — all built on one shared set
of mechanisms.

---

## 1. Market model

At the highest level, yDelta matches:

- **lender supply:** sub-vault asks — fixed rate (bank APR + spread),
  fixed term (the sub-vault's `max_term_seconds`), unbounded size
- **borrower demand:** a bid — fixed rate ceiling, term, principal,
  collateral — that fills immediately and optionally rests its residual

The match creates a discrete loan rather than adding both sides to a
pooled balance sheet.

```text
Lender quote (resting ask):
  rate = live bank lending APR + sub_vault.spread_bps
  term = sub_vault.max_term_seconds
  placed by a sub-vault curator; unbounded principal

Borrower intent (bid; fills now, residual rests / falls back / drops):
  rate (ceiling), term, principal, collateral

Match result:
  fixed loan with locked terms
```

Both sides can rest, but they are not symmetric. Asks are **standing
quotes** owned by vault capital; bids are **transient** borrower orders
with attached collateral. A borrow request first crosses the asks tree in
the same transaction; only the leftover, if the borrower chose to rest
it, becomes a resting bid. The borrower is not simply choosing "how much
to borrow from a pool" — they are choosing the exact credit shape they
are willing to enter, and either fill it now, leave it resting at their
limit, or route it to the fallback.

A book can sit **crossed at rest** — a resting bid whose rate would cross
a resting ask — and that is legal. Crossability changes without order
flow: a vault deposit replenishes a sub-vault's idle capital, a repayment
frees capacity, an oracle move flips an LTV gate. The invariant is not
"the book is never crossed" but "no *fillable* cross survives a taking
instruction": after any bid placement, ask placement/re-sync, or match
crank, every remaining (best-bid, best-ask) pair fails at least one gate
(rate, term, sub-vault idle, LTV, floor, expiry, or self-cross).

---

## 2. Rate matching and the LTV gate

Every ask belongs to a sub-vault, and the sub-vault carries the policy
the engine reads live at match time: `spread_bps`, the `max_ltv_bps`
origination ceiling, the `liquidation_ltv_bps` trigger, and the
`max_term_seconds` cap. Reading these live means a curator's policy
change takes effect immediately, with no per-seat re-sync.

A borrower's bid crosses an ask when the rate and term are compatible:

```text
ask_rate ≤ bid_rate           (the bid's rate is a ceiling on the ask)
bid_term ≤ ask.max_term        (the loan can't outlast the sub-vault's cap)
```

On a cross the loan is stamped:

```text
lender_rate   = ask_rate
borrower_rate = max(bid_rate, ask_rate + protocol_fee_bps_floor)
```

The bid rate is a **ceiling on the lender rate**; the protocol fee floor
is always guaranteed on top. With `protocol_fee_bps_floor = 50` and a
500 bps bid:

| Lender ask | Match? | `lender_rate` | `borrower_rate` | Protocol take |
|---|---|---|---|---|
| 500 | yes | 500 | `max(500, 550) = 550` | 50 |
| 480 | yes | 480 | `max(500, 530) = 530` | 50 |
| 400 | yes | 400 | `max(500, 450) = 500` | 100 |
| 510 | no (`500 < 510`) | — | — | — |

The borrower can pay up to the floor (50 bps) above their stated bid —
they accept the protocol floor as a fee added on top. This construction
structurally guarantees `borrower_rate ≥ lender_rate + floor`.

**The fill-time floor.** Because a stored ask rate is a snapshot of
`bank_APR + spread` from whenever it was placed, the bank's own rate can
drift above it. The engine guards against filling stale: any resting ask
whose stored rate is below the *current* live bank lending APR is skipped
(and logged), never matched. The fix is a parameterless re-sync that
re-reads the bank and re-stores `current_APR + spread`.

**The LTV gate is the sub-vault's alone.** Every cross verifies the
borrower's collateral against `sub_vault.max_ltv_bps` at oracle prices —
nothing else. A fixed loan's collateral is an *asset* on marginfi (it
carries no marginfi liability), so marginfi's risk weights were only ever
a self-imposed reference, not a structural constraint. yDelta drops them
from origination: a curator may set `max_ltv_bps` **above** marginfi's
implied LTV and extend more borrowing power than marginfi itself would.
A bid whose collateral does not satisfy a given sub-vault's cap simply
skips that ask and walks to the next; the cap is per-ask lender policy,
not a property of the bid. (Marginfi's weights do still gate one path —
the variable-rate fallback — because that path opens a real marginfi
borrow. See §14 and §17.)

---

## 3. Yield-alive capital

yDelta is designed so that capital remains productive across the
lifecycle instead of being trapped in dead escrow:

```text
lender capital waiting to match   →  productive
collateral securing a loan        →  productive
collateral behind a resting bid   →  productive
capital awaiting withdrawal       →  productive
```

This is especially important on the borrower side. In many systems,
collateral becomes economically silent once posted. In yDelta, borrower
collateral is routed through marginfi, which means it can help offset the
borrower's effective rate instead of remaining inert — and that holds
even while a borrower's bid is merely *resting* on the book, unmatched.

"No capital sits idle" is not marketing language here. It is a design
principle.

---

## 4. The strategy-vault model

yDelta uses one `GlobalVault` per lending **bank** and supports multiple
operator-managed sub-vaults inside that vault.

```text
                    GlobalVault
              (one vault per marginfi bank)
                           |
        ------------------------------------------------
        |                      |                      |
    SubVault 0            SubVault 1             SubVault 2
   Pool, low LTV         Pool, med LTV         Private, owner-run
        |                      |                      |
   market seats            market seats            market seats
        |                      |                      |
   quoted liquidity       quoted liquidity       quoted liquidity
```

Keying the vault by **bank** rather than by mint makes the
vault↔bank relationship structural: the vault PDA is `[b"vault", bank]`,
and any market whose debt side is that bank can host the vault's asks.

Two sub-vault kinds share the same accounting:

- **Pool** — created by the protocol admin, who assigns the curator and a
  `curator_fee_bps` (bounded by a protocol cap). Deposits are pooled and
  shared pro-rata across depositors.
- **Private** — created permissionlessly; the signer becomes both curator
  and sole depositor, and the curator fee is zero. A single wallet's
  personal strategy.

Three advantages fall out of this structure:

- depositors do not need a new vault for every strategy
- operators can express multiple credit styles from one capital base
- liquidity fragmentation is reduced

Vault-side accounting can be summarized as:

```text
idle_principal = total_principal - deployed_principal - encumbered_in_orders
```

Where:

- `deployed_principal` funds active loans
- `encumbered_in_orders` backs fills reserved against resting bids
- `idle_principal` remains available for new matches

Each sub-vault also tracks two lifecycle counters — `open_orders_count`
(live order refs) and `open_loans_count` (queued matches plus
promoted-unclosed loans) — and a sub-vault can only be removed once both
are zero, closing the orphaned-order-ref gap.

---

## 5. Why global vaults are needed

The average lending user does not want to operate like an orderbook
market maker.

A direct yDelta lender has to think about:

- what spread to quote over the bank rate
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
- an orderbook-based fixed-rate venue, where active quote management would
  otherwise be required

In other words, the global vault turns yDelta from a venue only for active
lenders into a venue that can also serve passive lenders.

---

## 6. How a global vault works for the depositor

For the depositor, the vault experience is simple:

1. deposit into a chosen sub-vault
2. receive vault shares
3. let the curator and sub-vault logic deploy capital
4. redeem shares later for a larger atom balance if the strategy performed
   well

The depositor is not manually posting asks on the market. The sub-vault
does that on their behalf.

This is why a global vault is best described as a **lending optimizer**:

- idle capital remains productive
- quoted capital can be deployed across multiple markets
- active loans earn fixed lender-rate yield
- repayments recycle back into the same strategy

The depositor gets strategy exposure without having to manage the
strategy.

---

## 7. The share model: what a vault user owns

Each sub-vault has:

- `total_shares`
- `total_assets_atoms`
- `total_principal_atoms`

When a user deposits, they receive shares in that sub-vault. The mint
formula is:

```text
if total_shares == 0:
    shares_minted = atoms_in
else:
    shares_minted = atoms_in × total_shares / total_assets
```

The depositor owns a pro-rata claim on the sub-vault, not on one
individual loan.

At any point, the depositor's gross vault value is:

```text
user_value_atoms = user_shares × total_assets_atoms / total_shares
```

That is the most direct answer to "what does a global vault user own?"
They own a proportional slice of the sub-vault's asset base.

---

## 8. What counts as depositor profit

For a vault user, profit is the growth of their share-backed claim over
time.

A useful approximation is:

```text
user_profit_atoms = current_user_value_atoms − user_principal_basis_atoms
```

Where:

- `current_user_value_atoms = user_shares × total_assets_atoms / total_shares`
- `user_principal_basis_atoms` is the depositor's effective cost basis in
  the sub-vault

At the sub-vault level, the system tracks both:

- `total_assets_atoms` — current economic value of the sub-vault
- `total_principal_atoms` — principal base currently attributed to the
  sub-vault

`total_assets_atoms` captures economic growth from yield accrual.
`total_principal_atoms` tracks the principal pool used for idle-capital
gating and realized capital accounting.

So from the depositor's perspective:

- profit shows up through rising share value
- realized pool capital is updated as loans close and cash returns

---

## 9. Where depositor profit comes from

Two yield streams accrue into each sub-vault.

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

The protocol uses the underlying `asset_share_value` snapshot and applies
the ratio delta to idle atoms.

Interpretation:

- capital waiting for deployment is still earning the bank's supply rate
- capital not yet matched is not wasted
- the vault behaves more like a productive reserve than a dead cash pile

### 9.2 Lender-rate yield on deployed capital

When vault capital is matched into active fixed-rate loans, the sub-vault
earns lender-side loan yield. The sub-vault tracks:

```text
total_weighted_rate_bps = Σ(loan_principal × lender_rate_bps)
```

So the deployed-loan yield over elapsed time is:

```text
loan_yield = total_weighted_rate_bps × elapsed
             / (10_000 × seconds_per_year)
```

This is a key optimization. The protocol does not need to iterate every
open loan to estimate depositor earnings. It can accrue the whole
sub-vault in O(1) using the running aggregate.

### 9.3 The yield promise, stated precisely

Earlier framings of yDelta implied lenders "always earn at least
marginfi." The honest version is two-part, and the spread model makes it
exact:

- **Idle capital ≥ marginfi, always.** Anything not deployed sits on the
  bank earning the supply rate — there is no idle drag.
- **Deployed capital earns a curator-priced spread above a floored
  origination rate.** Because each ask is stored at `bank_APR + spread`
  and the fill-time floor blocks anything below the live bank lending APR,
  a fixed loan is always struck at or above the bank's lending rate *at
  origination*. What a fixed term gives up is the *future*: if the bank
  rate climbs during the loan, that loan keeps its locked rate and earns
  less than freshly-deployed capital would — the lender chose rate
  certainty over rate-chasing. yDelta no longer claims "always ≥
  marginfi"; it claims "≥ marginfi at origination, with the spread as the
  curator's compensation for term risk."

### 9.4 Combined sub-vault growth

```text
total_yield = idle_yield + loan_yield
total_assets_after = total_assets_before + total_yield
```

That is the heart of the global-vault profit model. The depositor earns
from productive idle capital and fixed-rate deployed loans
simultaneously. The vault is not choosing between "idle but liquid" and
"earning but locked." Both states are productive.

---

## 10. Why this is a better experience for passive lenders

For a passive lender, the vault is doing several jobs at once.

**It abstracts quote management.** The depositor does not need to decide
what spread to post, which market to post into, whether to cancel or move
a quote, or how much duration risk to take in each market. The sub-vault
and its curator handle that.

**It preserves productive capital.** Passive users expect a lending
protocol to keep deposits working by default — that expectation comes from
pool-based lending UX. Global vaults preserve that intuition inside
yDelta: deposit once, let the vault keep capital productive, let the
strategy deploy when opportunities appear.

**It turns yDelta into a familiar lender UX.** Without global vaults,
yDelta would ask passive lenders to behave like traders. With global
vaults, yDelta can offer an experience much closer to a traditional
lending protocol: deposit, select a sub-vault, earn, withdraw when
liquidity is available. That is the bridge between conventional lending
UX and orderbook-based fixed-rate credit.

---

## 11. Withdrawal math and liquidity constraints

When a depositor withdraws, the sub-vault computes:

```text
atoms_out = shares_burned × total_assets_atoms / total_shares
```

Withdrawal value is based on the depositor's pro-rata share of total
economic assets. But there is an important liquidity constraint:

```text
idle_principal ≥ atoms_out
```

The protocol will not let a user withdraw capital that is currently:

- deployed inside active loans
- reserved against fills on resting bids

This is an important distinction:

- economic value can accrue continuously
- immediate redeemability depends on idle principal and realized cash flow

A vault depositor can be earning while part of the sub-vault is deployed,
but cannot necessarily redeem all of that economic value until capital
becomes idle again through cancellations, repayments, or settlement.

That is the right tradeoff for a lending optimizer. The sub-vault stays
invested when opportunities exist, but still preserves a clear accounting
rule for redemption.

---

## 12. Realized versus accrued profit

The code distinguishes accrued earnings from realized capital.

**Accrued earnings** are reflected through:

- `total_assets_atoms` growth
- cumulative yield indices
- rising share value

This is the economic profit the user has earned so far.

**Realized earnings** are reflected when repayment cash actually returns
to the vault and the sub-vault state is updated on claim. At that point:

- deployed principal falls
- weighted-rate contribution from the closed loan is removed
- realized interest increases the sub-vault's principal base
- any shortfall reduces that principal base

So the depositor's vault return is economically visible before final
repayment, but the principal pool is only fully refreshed when cash comes
home.

---

## 13. Global vault profit as a lending-optimizer equation

A simple way to express the depositor outcome is:

```text
vault_user_return = productive_idle_yield
                  + fixed_loan_lender_yield
                  − realized_shortfalls
                  − curator fee on the lender-rate stream
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

The curator fee is a sub-vault property (`curator_fee_bps`), set by the
admin at Pool creation and zero for Private sub-vaults. It is snapshotted
onto each loan at match time, so a later fee change never touches loans
that are already open.

---

## 14. `p0` as backdrop and fallback

The orderbook is the primary venue for fixed-rate matching. But credit
markets need a backstop when book liquidity is thin. yDelta uses `p0`
(marginfi) as that backdrop, and the borrower chooses how the unfilled
residual of a bid is handled:

```text
borrow request (IOC bid crosses resting asks)
    │
    ├── fully filled                   →  fixed loan(s)
    │
    └── residual remains, and the borrower's residual_mode decides:
          ├── P2PoolFallback  →  open a variable-rate marginfi borrow
          ├── Rest            →  leave a resting bid at the limit price
          └── Drop            →  cancel the residual, release its collateral
```

The **P2Pool fallback** is the only path that opens a *real* marginfi
borrow, so it is the only path marginfi's own risk weights gate: the
program pre-checks the residual against marginfi's init-weight collateral
requirement and rejects with a clear error rather than letting the borrow
CPI fail opaquely. The **Rest** mode is what makes the book two-sided —
the residual becomes a standing bid whose collateral keeps earning supply
yield, prunable by a `last_valid_unix_ts` expiry. **Drop** is the
orderbook-only borrower who wants a fixed loan or nothing.

This does two things for the protocol:

1. it reduces failed borrow intent when direct fixed-rate liquidity is not
   enough
2. it gives yDelta a strategic integration posture for future ecosystem
   connectivity

### 14.1 Crossing resting bids

A resting bid is not orphaned. It is crossed by:

- **a sub-vault ask placement or re-sync** — placement *takes*: once the
  ask's rate is known, the engine sweeps every crossable resting bid in
  the same instruction
- **the permissionless `MatchCrank`** — anyone may resolve a crossed-at-
  rest book. No keeper fee: the natural crankers are curators deploying
  idle, borrowers wanting fills, and UIs.

The borrower can also `CancelOrder` (release the bid's collateral at its
stored snapshot) or `UpdateOrder` (cancel-and-replace with a new rate,
term, or expiry under a fresh sequence — the encumbrance is untouched).

### 14.2 Upgrading variable-rate debt to fixed-rate

The fallback path is reversible. A borrower who took on `P2Pool` debt is
not locked into the variable rate for the loan's life.

`convert_p2pool_to_fixed` lets a borrower walk the asks tree against their
existing P2Pool position and convert any portion that crosses into fresh
fixed-rate loan bodies. The cross gate is:

```text
ask.rate_bps      ≤ max_acceptable_rate_bps
ask.term_seconds  ≥ remaining_term_of_p2pool_loan
```

Each successful cross emits a fresh `Fixed` `MatchedLoan` queue node. Any
unfilled residual stays on the original P2Pool body. Full conversion
closes the P2Pool PDA — but only when the post-CPI live marginfi liability
is genuinely zero, so a residual variable position can never be silently
orphaned. The refinance scan reuses the same engine, inheriting the
fill-time floor and the owner-level self-cross skip.

Conceptually:

- the fallback is a backstop, not a one-way commitment
- borrowers can opportunistically reprice when fixed-rate liquidity
  appears
- the same matching engine is reused — no separate "refinance" codepath

The strategic posture: **the orderbook is where credit gets priced;
marginfi is where credit gets backstopped.** The two layers complement
rather than compete.

---

## 15. Capital flow relationships

The protocol can be pictured as two productive sides feeding a match
engine.

```text
 Lender wallet                            Borrower wallet
      │                                         │
      ▼                                         ▼
 vault sub-vault (debt-side)            productive collateral-side rail
      │                                         │
      ▼                                         ▼
 sub-vault market seat                    borrower seat
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

This framing captures the real relationship more clearly than an
escrow-first mental model. The market is not primarily about moving tokens
in and out of dead holding areas. It is about managing productive
balances, matching intent, and crystallizing those balances into fixed
credit exposure when the terms align.

---

## 16. Loan economics

For a fixed loan, the borrower owes fixed debt growth over the term and
the lender earns the lender-side fixed rate:

```text
borrower_interest = principal × borrower_rate × elapsed / year
lender_interest   = principal × lender_rate   × elapsed / year
spread            = borrower_interest − lender_interest
```

Conceptually:

- borrower debt grows at the borrower-side rate
- lender claim grows at the lender-side rate
- the spread covers the protocol fee floor and the curator fee

The product takeaway is that yDelta separates three things clearly:

- the borrower's contractual fixed cost
- the lender's contractual fixed return
- the productive background yield of parked capital before and after
  matching

---

## 17. Liquidation and partial settlement

Real markets need flexible unwind paths. yDelta supports partial
settlement on debt repayment paths and partial liquidation on distressed
paths — resolution does not have to be all-or-nothing.

**The liquidation trigger is stamped, not live.** Decoupling LTV from
marginfi (§2) cuts both ways: just as origination uses the sub-vault's
`max_ltv_bps`, liquidation uses a per-sub-vault `liquidation_ltv_bps`.
Both are **stamped onto the loan at match time**. A curator who later
raises or lowers the sub-vault's thresholds does not move the trigger on
any loan that is already open — every open loan carries the policy it was
born under. The gate reads only the loan's stamped `liquidation_ltv_bps`
and the live oracle price; it does not consult marginfi maintenance
weights for fixed loans.

**No loan is born liquidatable.** Sub-vault creation and updates enforce
`liquidation_ltv_bps ≥ max_ltv_bps + MIN_LIQ_GAP_BPS`, so there is always
a strict band between the LTV at which a loan can originate and the LTV at
which it can be liquidated.

**P2Pool loans are the exception, by construction.** A variable-rate
fallback position genuinely lives as a liability on marginfi, so its
health is still measured against marginfi maintenance weights — that is
the one place marginfi's risk model remains load-bearing.

Every liquidation gate fails closed on a degenerate oracle: a stale,
retracted, or zero-confidence price rejects the gate rather than producing
a number, so a bad feed can neither liquidate a healthy loan nor be relied
on to spare an unhealthy one.

---

## 18. Execution efficiency

Fixed-rate orderbooks on Solana have to do orderbook work at orderbook
speed. yDelta hits this by keeping `place_order_for_sub_vault` /
`cancel_order_for_sub_vault` / `update_order_for_sub_vault` — and the
borrower-side `place_order` / `cancel_order` / `update_order` — as pure
bookkeeping on the market account.

A vault ask is a pure-memory entry: it carries no fixed principal and
takes no seat encumbrance. The sub-vault's `idle_principal` pool is the
backing, read at match time. So:

```text
placing or repricing a vault ask is a tree mutation on the market account,
not a token transfer that has to round-trip through a bank.
```

Operationally:

- `place_order_for_sub_vault` fires **zero external CPIs** (it also
  auto-creates the vault seat on first use, and *takes* any crossable
  resting bids — recording fills as queued `MatchedLoan` nodes)
- `update_order_for_sub_vault` (a parameterless re-sync) and
  `cancel_order_for_sub_vault` fire **zero external CPIs**
- a borrower bid that crosses a resting ask fires **zero external CPIs** at
  placement; the P2Pool fallback is the only borrower path that touches
  marginfi inline
- `MatchCrank` is a permissionless, zero-CPI sweep that resolves a
  crossed-at-rest book
- atom migration for every fixed match is deferred to a 3-CPI
  permissionless cranker (`process_matched_loan`)
- only `deposit`, `withdraw`, `repay`, the cranker realization, the P2Pool
  fallback, and liquidation actually need to touch marginfi

The consequence is a market that can hold orders of magnitude more live
state per unit of compute. A book with thousands of resting vault asks,
where every cancel-and-reprice is a pure-memory mutation, costs
dramatically less than one where every operation has to round-trip through
an external lending pool.

This matters for liquidity. Curators reprice constantly. A protocol that
taxes each reprice with CPI overhead pushes curators to quote wider, less
often, in fewer markets. yDelta is designed so that quote churn is
structurally cheap, which means the book can carry tighter, deeper, more
responsive liquidity.

---

## 19. Two marginfi accounts per market

A single marginfi v0.1.8 account cannot simultaneously hold an asset
position and a liability position on the same bank. That constraint
matters here because a yDelta market has both:

- lender USDC sitting as an asset on the debt bank
- borrower USDC liability (via P2Pool fallback) on the same debt bank

If both lived on the same account, the second flow would be blocked by
marginfi's per-`(account, bank)` mutual exclusion.

yDelta sidesteps the constraint by wrapping **two** marginfi accounts per
market:

- a lender-side account that holds the debt-mint asset
- a borrower-side account that holds the collateral asset and any P2Pool
  debt liability

Both accounts have the same authority (`market_signer`), so the program
can sign for either side. The split is invisible to users — they see a
single market, deposit and withdraw normally — but it is what lets the
protocol express both halves of a credit flow against the same underlying
bank without giving up the yield-alive property on either side.

This is the cleanest example of how yDelta's design works with marginfi's
constraints rather than around them.

---

## 20. Admin, pause, and sub-vault lifecycle

Every admin role in the protocol — market admin, vault admin, sub-vault
curator, protocol-wide admin — uses a two-step transfer pattern:

```text
initiate_transfer  →  sets pending_admin
accept_transfer    →  pending_admin signs to commit
```

This prevents the most common admin-key footgun: a transfer to a
non-controlled key. A typo in the initiator's instruction data cannot
brick the role, because nothing has changed yet on the receiving side. The
would-be successor has to actively sign before they take over.

There are two kill switches:

- **per-market pause** — admin-set; while on, every state-mutating ix on
  that market rejects, while read-only ixs (mirror sync, simulation gates)
  stay live
- **global pause** — protocol-admin-set; same gating, but applied at the
  loader level across every ix that takes the `global_config` account

**Markets are live at creation.** Unlike the prior iteration, fresh
markets do not ship paused: `CreateMarketParams` carries the full fee
config at creation, so there is no unconfigured setup window to defend.
The pause switches exist for *emergencies*, not for a configuration
handshake — a market that loses oracle freshness, a vault that hits an
accounting anomaly, or a marginfi-side issue can be frozen at the affected
scope while the rest of the protocol keeps running, without redeploying
the program.

**Sub-vault lifecycle.** A Pool or Private sub-vault can be *sunset*
(blocks new deposits, new orders, order updates, and matches; withdrawals,
fee claims, and cancellations stay open) and *resumed* by the vault admin.
Curator-set parameters — `spread_bps`, the LTV pair, `max_term_seconds` —
are editable via `UpdateSubVault` (curator-gated, owner for Private), but
`curator_fee_bps` is fixed at creation and never editable. Removal of a
sub-vault requires it to be empty *and* to carry zero open orders and zero
open loans.

---

## 21. Oracle integration

LTV math is only as trustworthy as the price feeds underneath it. yDelta
accepts three oracle shapes through the marginfi adapter:

- **Pyth-Push** — single oracle account; rejects partial-verified updates
  outright (`MIN_PYTH_PUSH_VERIFICATION_LEVEL = Full`)
- **Switchboard-Pull** — single oracle account; decoded from the pulled
  feed's result value
- **StakedWithPythPush** — three accounts (Pyth feed + LST mint + stake
  state); the Pyth SOL price is adjusted by the stake-pool's accounting to
  derive the LST exchange rate

Every oracle read passes a confidence-interval check before LTV math runs.
The threshold is `bank.config.oracle_max_confidence × price` (default
10%). A bounded future-skew gate rejects readings stamped too far ahead of
the on-chain clock. A volatile, unconfident, or skewed reading rejects the
gate rather than producing a degraded number.

The design intent is uniform across feeds: a stale, retracted,
low-confidence, or future-skewed price is **not** a price for purposes of
LTV. Both the origination gate (against the sub-vault's `max_ltv_bps`) and
the liquidation gate (against the loan's stamped `liquidation_ltv_bps`)
fail closed. This pushes the failure mode toward "loan doesn't open" or
"liquidation can't prove a breach" rather than "loan opens at the wrong
LTV" — the safer of the two.

---

## 22. Reading the implementation

The codebase maps closely to the design:

- `programs/ydelta/src/program/processor/place_order.rs` — borrower bid
  flow, residual modes, and P2Pool fallback routing (with the marginfi
  init-weight fallback pre-check)
- `programs/ydelta/src/program/processor/place_order_for_sub_vault.rs` —
  sub-vault ask placement (auto-creates the vault seat; takes resting bids)
- `programs/ydelta/src/program/processor/update_order_for_sub_vault.rs` —
  parameterless spread-over-bank re-sync
- `programs/ydelta/src/program/processor/cancel_order.rs` /
  `update_order.rs` — borrower bid cancel and cancel-and-replace
- `programs/ydelta/src/program/processor/match_crank.rs` — permissionless
  crossed-at-rest resolution
- `programs/ydelta/src/program/processor/create_sub_vault.rs` — Pool
  (admin) and Private (permissionless) sub-vault creation
- `programs/ydelta/src/program/processor/convert_p2pool_to_fixed.rs` — the
  variable-to-fixed upgrade path
- `programs/ydelta/src/state/market_helpers.rs` — the two-sided matching
  engine (bid take, ask take, refinance) and the rate/LTV/floor gates
- `programs/ydelta/src/state/ltv.rs` — origination and stamped-liquidation
  LTV math
- `programs/ydelta/src/state/market.rs` — market-level state, fee
  configuration, split integration accounts
- `programs/ydelta/src/state/vault.rs` — `GlobalVault` and `SubVault`
  accounting
- `programs/ydelta/src/state/loan.rs` — promoted fixed-loan state with the
  stamped LTV pair
- `programs/ydelta/src/protocol/marginfi.rs` — marginfi v0.1.8 adapter,
  oracle confidence checks
- `programs/ydelta/src/protocol/marginfi_rate_calc.rs` — the live bank
  lending APR used by the spread model and the fill-time floor
- `programs/ydelta/tests/cases/` — lifecycle and mechanism coverage,
  including bids, the take path, the crank, self-cross, LTV decoupling, and
  liquidation
- `docs/v1-spec.md` — the decision log (D1–D17) and the authoritative v1
  contract this document describes

---

## Closing

yDelta is not trying to be another generic lending pool.

It is designed as a capital-efficient fixed-rate credit market where:

- credit is priced on a two-sided orderbook, with asks backed only by
  vault capital
- collateral remains economically useful — even while a bid merely rests
- one vault can express multiple curator strategies, keyed to a bank
- rates are quoted as a spread over the live bank rate, floored at fill
- LTV is the curator's to set, decoupled from marginfi, and stamped onto
  every loan
- the orderbook has a pragmatic, reversible fallback

That combination is the protocol's identity.
