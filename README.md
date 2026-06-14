# yDelta

**Fixed-rate lending on Solana — where your money never sits still.**

> **Looking for the TypeScript SDK?** Install and usage docs for
> [`@ydelta/sdk`](https://www.npmjs.com/package/@ydelta/sdk) live in
> **[`ts/README.md`](ts/README.md)** — that's what's published to npm. This
> README is the story of how the protocol works. Engineers who want the
> byte-level contract should head to [`docs/protocol-design.md`](docs/protocol-design.md).

---

Most on-chain lending is a savings account with mood swings. You deposit, and
the interest rate drifts with the market — good today, who-knows tomorrow, never
something you can build a plan around. Borrowers get the mirror image: a rate
that can lurch against them mid-loan, and a "take whatever's available" fill that
can leave them short of what they asked for.

yDelta does it the way credit actually works in the real world. Two sides meet on
an open market and agree on a **fixed rate** for a **fixed term**. The lender
knows exactly what they'll earn, and for how long. The borrower knows exactly
what they'll owe. No drift, no surprises.

And underneath the whole thing runs a quiet trick that makes it all pay:
**no token ever sits idle.** Every dollar waiting for a match, every bit of
collateral backing a loan, even money that's already been borrowed — all of it
keeps earning yield in the background until the very moment it's doing something
else.

That's the pitch. Here's how it works.

## Table of contents

- [1. The problem yDelta solves](#1-the-problem-ydelta-solves)
- [2. Your money never sleeps](#2-your-money-never-sleeps)
- [3. A real marketplace for credit](#3-a-real-marketplace-for-credit)
- [4. Deposit, pick a strategy, earn](#4-deposit-pick-a-strategy-earn)
- [5. A backstop, not a trap](#5-a-backstop-not-a-trap)
- [6. Borrow on your own terms](#6-borrow-on-your-own-terms)
- [7. Built to be cheap, fast, and fair](#7-built-to-be-cheap-fast-and-fair)
- [8. A loan, from start to finish](#8-a-loan-from-start-to-finish)
- [9. Under the hood](#9-under-the-hood)
- [10. Build \& test](#10-build--test)

---

## 1. The problem yDelta solves

Lending protocols today ask you to accept one of two bad deals.

**If you lend,** your capital has a lot of dead time. It sits in a pool waiting
for someone to borrow it. The rate floats, so you can't quote a return to anyone.
And when a loan is repaid, your money waits *again* to be redeployed. Most of the
time, most of your capital is either idle or earning a number nobody promised you.

**If you borrow,** you're at the mercy of a rate that moves while you sleep, and
of whatever liquidity happens to be sitting in the pool at your price. Ask for
$100k and the pool only has $60k at your rate? Tough — you take $60k or nothing.

yDelta's answer is to treat credit like a market with a safety net:

- A **two-sided marketplace** where lenders post offers and borrowers post bids,
  and a matching engine pairs them into real loans with the terms locked in.
- A **yield rail underneath** (the marginfi money market) so capital is productive
  in *every* state — waiting, working, or being paid back.
- A **backstop** so a borrower who can't be fully filled on the market doesn't hit
  a cliff — the leftover can fall through to a variable-rate loan, and even be
  upgraded back to fixed later.

Everything below is built from those three ideas working together — not as
separate features bolted on, but as one design.

## 2. Your money never sleeps

This is the part most people underestimate, so we lead with it.

In a normal fixed-term protocol, capital has three stretches of dead time: while a
lender's offer waits to be taken, while a borrower's collateral is locked up, and
while a repaid loan waits to be claimed. yDelta erases all three. **Every token in
the system is parked in a marginfi lending pool, earning supply yield, right up
until the instant it's needed elsewhere.**

```
   ┌──────────────────┐
   │ Vault depositor  │
   └────────┬─────────┘
            │ deposit USDC into a strategy
            ▼
   ┌──────────────────────┐
   │  marginfi USDC pool  │  ◄── these atoms earn yield the whole time:
   │  (earning supply APY)│      while offers rest, while loans run,
   └──────────┬───────────┘      while a repayment waits to be claimed
              │ shares
              ▼
   ┌──────────────────────┐
   │  Strategy vault      │
   │  idle  → deployed    │  ◄── a resting offer locks up nothing;
   └──────────────────────┘      the idle pool backs every match
              │
              │ match: idle becomes deployed; a loan records who owes what
              ▼
        Loan accrues at the fixed rate
              │ borrower repays → atoms re-enter marginfi → claimed back to the vault
              ▼
```

The upshot for a lender: even the boring states pay. Your deposit earns market
yield while it waits, and earns the fixed loan rate while it's lent — and a
borrower's collateral *also* earns yield while it backs their loan. There is
simply no "sitting in escrow doing nothing" anywhere in the protocol.

> **Under the hood.** Balances are tracked as marginfi *shares*, not raw token
> amounts. A share's value grows as the pool earns, so yield accrues uniformly
> whether the shares are free or locked. Each trader's balance is just
> `withdrawable_shares + encumbered_shares`, and share counts are conserved through
> every deposit, match, repay and claim — which is what lets the accounting stay
> exact while everything underneath keeps compounding.

## 3. A real marketplace for credit

yDelta prices credit on an order book with two sides.

**The lender side** is made entirely of **strategy vaults** (more on those in the
next section). A vault posts an *offer* — "I'll lend at this rate, up to this
term" — and that offer rests on the book. There are no anonymous wallet lenders;
every offer is backed by real vault capital.

**The borrower side** is people who want a loan. A borrower posts a *bid* — "I want
to borrow this much, at no more than this rate, for this term, and here's my
collateral." The moment it lands, the engine sweeps the offers and fills the bid
against the cheapest ones that qualify.

**Rates aren't pulled out of thin air — they track the live market.** A vault
doesn't post a hard number; it posts a *spread*. When its offer is placed, the
program reads the current marginfi lending rate for that asset and stores
`market_rate + spread`. So a lender is always quoting a premium over the
money-market rate, and that quote re-prices with one parameterless refresh. A
**fill-time floor** keeps it honest on the way out: if a stale offer has drifted
below the *current* market rate, the engine skips it rather than let it fill
below market.

> **Under the hood — how a match is priced.** A bid crosses any offer whose rate
> is at or below the bid's rate and whose max term covers the bid's term. On a
> cross the loan is stamped:
> ```
> lender_rate   = offer_rate
> borrower_rate = max(bid_rate, offer_rate + protocol_fee_floor)
> ```
> The bid rate is a *ceiling on the lender's cut*; the small protocol fee floor is
> always collected on top. The gap between what the borrower pays and what the
> lender earns is the protocol's revenue.

## 4. Deposit, pick a strategy, earn

Most people don't want to run an order book for a living. They don't want to
decide what spread to quote, which markets to quote in, or when to reprice. They
want to deposit, pick a risk style, and earn. That's what **strategy vaults** are
for — and since the lender side of the book is *only* vaults, they're how all
lending liquidity reaches the market.

One vault sits on top of one marginfi pool (say, USDC) and hosts many independent
**strategies**, each run by a curator with its own spread, loan-to-value ceiling,
liquidation threshold and term cap. Deposit into the conservative one, the
aggressive one, or several at once — all under a single deposit.

```
                          USDC vault
                          ┌──────────────────┐
                          │ idle: $10,000,000│
                          ├──────────────────┤
                          │ Strategy A       │  conservative: 60% max LTV,
                          │ (pooled)         │  30-day max term
                          ├──────────────────┤
                          │ Strategy B       │  aggressive: 75% max LTV,
                          │ (owner-run)      │  90-day max term
                          └────────┬─────────┘
                                   │  one strategy quotes into many markets
                  ┌────────────────┼────────────────┐
                  ▼                ▼                ▼
          USDC/SOL market   USDC/JTO market   USDC/JUP market
```

**One basket, every pair.** This is where it beats a spot order book. On a spot
book you can quote one token against many others, but your capital is trapped per
book: dollars resting on the SOL/USDC book can't fill a JTO/USDC order, so to make
markets everywhere you have to fragment your capital across every pair — and watch
most of it sit idle on each. A yDelta strategy is *one* basket of liquidity that
backs offers on every market its asset trades against, all at once. Nothing is
pre-allocated; a match is drawn from the live idle balance only when it actually
fills, and it's atomic, so two markets can never spend the same dollar twice. You
get the spot-book reach across many pairs — minus the capital fragmentation, and
(since every idle dollar is still earning on marginfi) minus the idle drag.

A single deposit can back several strategies, and there are two flavours: **Pool**
strategies (curator-run, anyone can deposit) and **Private** strategies (one owner,
permissionless to spin up). Either way, the depositor's experience is the one they
already expect from a lending app: deposit, choose, earn, withdraw.

And they earn from *two* streams at the same time — supply yield on the idle
portion, and the fixed loan rate on the deployed portion — both flowing into the
same share price, so there's never a trade-off between "liquid" and "earning."

> **Under the hood.** Per-depositor accounting is O(1) no matter how many loans a
> strategy has open: a running weighted-rate aggregate plus Aave-style cumulative
> indices mean a strategy with 10,000 active loans accrues in the same handful of
> cycles as one with 10. No iteration, ever.

## 5. A backstop, not a trap

Here's the borrower's worst moment in a normal protocol: they ask for $100k, the
book only has $70k at their price, and they're stuck. yDelta removes the cliff.
When a bid can't be fully filled on the market, the borrower decides up front what
happens to the leftover:

```
borrow request (bid sweeps the resting offers)
    │
    ├── fully filled                   →  fixed-rate loan(s), done
    │
    └── leftover remains:
          ├── Fall through  →  borrow the rest from marginfi at a variable rate
          ├── Rest          →  leave a standing bid on the book at your price
          └── Drop          →  cancel the rest, get that collateral back
```

The **fall-through** path is what makes the cliff disappear: the borrower walks
away with the *full* amount they asked for, with the unmatched slice funded by a
plain variable-rate marginfi loan.

And it's a backstop, not a trap. A borrower who took the variable rate isn't stuck
with it — **`convert-to-fixed`** walks the offer book and migrates that variable
debt back into fixed-rate loans the moment better terms show up. They can repay
the variable portion any time, with no penalty.

> The strategic posture in one line: **the order book is where credit gets
> *priced*; marginfi is where credit gets *backstopped*.** The two layers
> complement each other instead of competing.

## 6. Borrow on your own terms

A borrower's collateral is their own risk, so yDelta gives them the controls.

**Set your own safety margin.** Every strategy publishes a maximum loan-to-value —
the most it's willing to lend against your collateral. But you might not want to
borrow right up to that ceiling; the closer you start to the limit, the less room
you have before a price dip puts you at risk of liquidation. So a bid can carry an
optional **LTV buffer**: a slider that says "originate me at least *this far* below
the strategy's ceiling."

This is a clean control because of what it does *and doesn't* do. As you tighten
the buffer, one of two visible things happens: either you need to post a bit more
collateral, or you match against fewer offers — and your requested principal is
**never silently shrunk** to make a risky fill fit. Whatever the tighter limit
leaves unfilled just follows your fall-through / rest / drop choice, exactly like
any other leftover. The buffer rides along on a resting bid and through edits, so
a position you set up to be safe stays that way even when it's matched later.

**No nasty term surprises.** A fixed loan runs for its full term at the locked
rate — it never auto-rolls into something shorter. There's **no prepayment
penalty**: pay early, for free, whenever you like. The only repricing is the one
*you* opt into with `convert-to-fixed`.

> **Under the hood.** The buffer tightens the origination gate to
> `effective_cap = strategy_max_ltv − buffer` and that same number is stamped onto
> the loan as its origination LTV. The liquidation trigger is the strategy's,
> stamped separately at match time — so a curator changing their thresholds later
> never moves the goalposts on a loan that's already open.

## 7. Built to be cheap, fast, and fair

**Repricing is free.** A curator can place, cancel and refresh offers all day
without paying for it. A resting offer is a pure bookkeeping entry — it holds no
locked principal and fires **zero** external calls — so a market with thousands of
live offers costs a fraction of what it would in a protocol that round-trips
through a bank on every tweak.

| Operation | External calls |
|---|---|
| Place / cancel / refresh an offer | **0** |
| Place a bid that crosses an offer | 0 (the atom movement is deferred to a permissionless cranker) |
| Cancel / edit a resting bid | **0** |
| Deposit / withdraw / repay | 1 each |

**Liquidations are partial by default and fair to everyone.** A keeper can repay
as much or as little as they have liquidity for; the loan stays open until it's
square. The borrower gets any surplus collateral back, and a tiered keeper bonus
means liquidations get done promptly the same way arbitrage does — permissionlessly.

| Outcome | Liquidator takes | Borrower gets back |
|---|---|---|
| Healthy collateral (typical) | the debt value + a bonus | the rest of their collateral |
| Underwater | the collateral (capped) | nothing; the shortfall is logged |

**The liquidation line doesn't move under your feet.** A loan becomes liquidatable
against the threshold that was *stamped when it was made* — not a live number a
curator can change after the fact. And every strategy must keep a minimum gap
between its lending ceiling and its liquidation line, so no loan is ever born one
tick from liquidation.

## 8. A loan, from start to finish

Walk one all the way through — 100 USDC borrowed at 8% for 30 days, against 0.5 SOL.

1. **A lender funds a strategy.** They deposit 100 USDC into a vault strategy. From
   that second, the money is earning marginfi supply yield.
2. **The curator posts an offer.** No rate typed in by hand — the program quotes
   `market_rate + spread`. The offer rests on the book, backing itself with the
   strategy's idle pool. Nothing is locked.
3. **The borrower posts collateral and bids.** 0.5 SOL goes in (earning yield too),
   and a bid lands: 100 USDC, ≤ 8%, 30 days. The engine finds the offer, checks the
   collateral clears the strategy's LTV, and pairs them into a loan.
4. **A keeper finalizes the match.** Anyone can promote the match into a loan
   record; they front the rent and get it back when the loan closes.
5. **The loan accrues.** Interest is computed on demand — no upkeep transactions.
   The borrower's debt and the lender's claim both grow at their locked rates; the
   spread between them accrues to the protocol.
6. **The borrower repays.** The money flows back through marginfi, the collateral
   is released, and the loan record closes.
7. **The vault sweeps it up.** A permissionless cranker pulls the repaid funds back
   into the vault, ready to back the next offer.

And if the borrower goes quiet? After maturity (plus a grace window) a keeper can
settle it; if the collateral ever falls below the stamped liquidation line, a
keeper can liquidate it — partially or fully — and the lender is made whole.

## 9. Under the hood

This is the deep end — skip it if you came for the pitch.

```
                 ┌──────────────────┐                 ┌──────────────┐
                 │  Vault depositor │                 │   Borrower   │
                 │  + curator       │                 │   wallet     │
                 └────────┬─────────┘                 └──────┬───────┘
                          │ deposit + quote                  │ deposit collateral
                          ▼                                  ▼
       ┌─────────────────────────┐          ┌─────────────────────────┐
       │  marginfi (lender side) │          │ marginfi (borrower side)│
       │  — earns supply APY     │          │  — earns supply APY     │
       └────────────┬────────────┘          └────────────┬────────────┘
                    ▼                                     ▼
       ┌──────────────────────────────────────────────────────────────┐
       │                          Market                                │
       │   ┌──────────────┐ ┌──────────────┐ ┌──────────────────────┐  │
       │   │  Bids tree   │ │  Offers tree │ │   Seats (balances)   │  │
       │   └──────┬───────┘ └──────┬───────┘ └──────────────────────┘  │
       │          └─ matching engine ─┘   (bid takes · offer takes · crank) │
       │                       │                                        │
       │              ┌────────▼────────┐                               │
       │              │  Matched-loan   │ ─ keeper promotes ─► Loan record  │
       │              │     queue       │      (fixed rate, fixed term)   │
       └──────────────────────────────────────────────────────────────┘
```

A few design choices worth calling out:

- **Two marginfi accounts per market.** One holds the lent asset, the other holds
  the borrower's collateral and any variable-rate debt. The split sidesteps a
  marginfi rule (one account can't hold both an asset and a liability on the same
  pool) and keeps every flow unconditional.
- **LTV is the curator's, decoupled from marginfi.** A fixed loan's collateral is a
  pure asset on marginfi — it carries no marginfi liability — so marginfi's own
  risk weights were never the right gate. Origination is gated only by the
  strategy's ceiling (which can sit *above* marginfi's implied LTV, extending more
  borrowing power than marginfi itself would), and liquidation by a separately
  stamped threshold. Marginfi's weights still gate exactly one thing: the
  variable-rate fall-through, which opens a real marginfi borrow.
- **Offers are unbounded; matches are atomic.** An offer says "lend all my idle
  capital," carries no fixed size, and every cross against it is sized at match
  time from the strategy's live idle balance — so concurrent matches across
  different markets can never over-commit the same pool.
- **Oracles are conservative.** Pyth and Switchboard feeds are decoded directly,
  with confidence-interval and staleness rejection; a volatile, unconfident or
  stale reading fails the LTV check rather than producing a wrong number.

For the byte-level contract — account layouts, the matching engines, the full
decision log — see [`docs/protocol-design.md`](docs/protocol-design.md) and
[`docs/v1-spec.md`](docs/v1-spec.md).

## 10. Build & test

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
├── README.md                         # this file — the protocol story
├── programs/
│   ├── ydelta/                       # the on-chain program
│   ├── ydelta-test-harness/          # SBF test harness
│   └── marginfi-mocks/               # marginfi type mocks
├── lib/                              # shared libs (hypertree)
├── ts/
│   ├── README.md                     # @ydelta/sdk docs (published to npm)
│   ├── src/                          # @ydelta/sdk TypeScript source
│   ├── idl/ydelta.json               # IDL shipped with the npm package
│   ├── scripts/                      # operator scripts (bootstrap, cranks, debug)
│   └── tests/                        # SDK + integration tests
├── docs/
│   ├── v1-spec.md                    # the decision log (D1–D18) and contract
│   └── protocol-design.md            # engineering companion to this README
└── scripts/
    ├── build-program.sh
    ├── deploy-program.sh
    ├── upgrade-program.sh
    └── test.sh
```

---
