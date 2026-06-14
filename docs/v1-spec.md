# yDelta v1 — Design Spec

**Bank-keyed vaults · two-sided book · pool & private sub-vaults · spread-over-bank rates**

Status: agreed design, pre-implementation. The previous deployment has been
torn down; this spec supersedes the deployed layout with no migration
obligations. `docs/protocol-design.md` and the README describe the prior
iteration and will be rewritten against this spec once implementation lands.

Naming: the object formerly called `RiskProfile` is now **`SubVault`** —
the vault is subdivided into sub-vaults, each with its own rules, capital
accounting, and operator. Two kinds: **Pool** (curator-run, pooled
deposits) and **Private** (single-owner, owner-run). "Seat" stays reserved
for balance entries: `ClaimedSeat` (market side, unchanged) and
`SubVaultDepositorSeat` (vault side, renamed from
`RiskProfileDepositorSeat`).

---

## 0. Decisions log

| # | Decision | Resolution |
|---|---|---|
| D1 | Vault identity | Keyed by **marginfi bank**, not mint: `[b"vault", bank]` |
| D2 | Sub-vault kinds | **Pool** (curator-run, permissioned) + **Private** (single-owner, permissionless, curator-less — the owner is the curator) |
| D3 | Pool gating | **Admin-created**: protocol admin creates the Pool sub-vault and assigns its curator (no curator registry). Vault creation itself permissionless |
| D3b | Curator fee | Moves from market `FeeConfig` to the **sub-vault**: set by the admin at Pool creation (≤ protocol cap), 0 for Private, snapshotted at match as today |
| D4 | Rate model | Sub-vault stores `spread_bps`; placement computes `live marginfi lending APR + spread` and stores the result **in the order** (sort tree preserved) |
| D5 | Rate floor | Enforced at **placement and at fill** (engine skips asks whose stored rate < live bank lending APR) |
| D6 | Bid side | Restored. Borrower residual is tri-state: **P2Pool fallback \| rest \| drop** |
| D7 | Who crosses resting bids | Ask placement/update **takes**; permissionless **match crank** as backstop |
| D8 | Crossed book | Legal at rest (liquidity/LTV-constrained); invariant is "no fillable cross survives a taking ix" |
| D9 | Self-cross | Blocked at **owner level** (skip, don't abort) |
| D10 | Stale orders | `last_valid_unix_ts` expiry; expired **bids** pruned during cross/crank (asks are standing quotes) |
| D11 | Yield promise | Reframed: idle ≥ marginfi always; deployed earns curator-priced spread above a floored origination rate. No "always ≥ marginfi" claim |
| D12 | Prepayment fee | Rejected — borrowers keep free early repay |
| D13 | Naming | `RiskProfile` → `SubVault { kind: Pool \| Private }`; `RiskProfileDepositorSeat` → `SubVaultDepositorSeat`; `RiskProfileOrderRef` → `SubVaultOrderRef` |
| D14 | Market creation | Markets are **live at creation** (no paused-by-default): `CreateMarketParams` carries the full fee config, so there is no unconfigured window to defend |
| D15 | Sub-vault updates | `UpdateSubVault` is **curator-gated** (owner for Private): edits `spread_bps`, LTV params, `max_term_seconds`. `curator_fee_bps` is excluded — admin-set at creation, immutable. Open loans unaffected (match-time snapshots) |
| D16 | Sub-vault counters | `open_orders_count` (inc place / dec cancel + admin-cancel) and `open_loans_count` (inc fill / dec full close) on `SubVault`; removal gate requires both == 0 in addition to `is_empty()` — closes the latent orphaned-`SubVaultOrderRef` gap in `remove_risk_profile` |
| D17 | LTV decoupled from marginfi | Fixed-loan collateral is asset-only in marginfi (no liability) — marginfi weights were a self-imposed reference, not structural. Origination gate becomes the curator's `max_ltv_bps` alone (may **exceed** marginfi's implied LTV — more borrowing power than marginfi); liquidation triggers off a per-sub-vault `liquidation_ltv_bps` (≥ max_ltv + `MIN_LIQ_GAP_BPS`), both **stamped onto the loan at match**. Marginfi weights gate only the P2Pool fallback (enforced by marginfi itself). `LTV_AUTO_FROM_MARGINFI`, `LTV_AUTO_BUFFER_BPS`, and `FeeConfig.ltv_buffer_bps` are removed |
| D18 | Borrower-set LTV buffer | **Motivation:** borrower self-protection — a UI LTV slider where tightening either raises the collateral a given principal needs or narrows the asks that fill, with principal **never silently shrunk** (the `FeeConfig.ltv_buffer_bps` removed in D17 was the *curator's* knob; this is the *borrower's*, on the bid). **Decision:** an optional `ltv_buffer_bps` on the bid. **Gate, not shrink:** `effective_cap = max_ltv_bps.saturating_sub(buffer)`; the bid only fills against asks it clears *with* the buffer, and any principal the tightened gate leaves unfilled follows `residual_mode`. The same `effective_cap` is stamped as the loan's `origination_ltv_bps`; `liquidation_ltv_bps` is **unchanged** (still the sub-vault's). Applied in all three engines (`match_order`, `match_resting_bids`, `match_p2pool_residual_against_asks`) at both the collateral gate and the origination stamp; the P2Pool **fallback** path stays gated by marginfi init weights, not the buffer. The buffer **persists on the resting bid** (`RestingOrder.ltv_buffer_bps`, `u16` at offset 66, struct size unchanged) so a later ask cross / `MatchCrank` honors it, and is **editable via `UpdateOrder`**. `convert_p2pool_to_fixed` also accepts it. All three params (`PlaceOrderParams`, `UpdateOrderParams`, `ConvertP2PoolToFixedParams`) validate `≤ 10_000`. `buffer = 0` == prior behavior. **Surfaces:** `PlaceOrderParams` / `UpdateOrderParams` / `ConvertP2PoolToFixedParams` (new last field); `RestingOrder` (offset 66); `state/ltv.rs::effective_origination_cap_bps`; the three `market_helpers.rs` engines; matching instruction builders |

---

## 1. Why (one paragraph each)

**Bank-keyed vaults (D1).** The vault was already economically bound to one
bank: `lending_pool` is fixed at creation and the idle MTM
(`last_supply_share_value_fp48`) is only meaningful against one bank's
`asset_share_value`. Mint-keying just (a) blocked every second marginfi bank
of the same mint from ever having vault liquidity, and (b) left the
vault↔bank invariant as scattered stored-field checks (order placement gated
on *mint* match while settlement gated on *bank* match — a latent
cross-bank inconsistency). Bank-keying makes the invariant structural.

**Pool & Private sub-vaults (D2/D3).** Pool sub-vaults keep the
passive-lender thesis (deposit, pick a curator, earn). Private sub-vaults
formalize the sophisticated user who is their own curator: same object,
`owner == curator == sole depositor`, fee forced to zero, permissionless
creation. The book stays quote-only — every ask still belongs to a
sub-vault, so custody, idle-gating, and match-time reads are unchanged.

**Spread-over-bank rates (D4/D5).** One absolute rate across markets
misprices collateral risk; per-market manual rates tax the curator with one
transaction per market per reprice. Storing the *spread* on the sub-vault
and computing `bank lending APR + spread` at placement gives
per-market-correct quotes from one knob, keeps the rate-sorted ask tree
coherent (the computed rate is stored in the order), and makes "never lend
below marginfi" true by construction at origination. The fill-time floor
extends that protection mid-term: when marginfi spikes, stale cheap asks
stop filling until the operator re-syncs (one parameterless `update_order`
per market).

**Bid side (D6–D8).** The book was supply-only: curators quoted blind and
borrowers who declined the P2Pool fallback got "drop and retry."  Resting
bids give borrowers an on-chain limit order and give curators visible
demand. Resting is free-carry for the borrower: the encumbered collateral
keeps earning marginfi supply yield (yield-alive extends to resting
orders). The scaffolding already exists from the Manifest fork —
`_reserved_bids_root` / `_reserved_bids_best` in `MarketFixed`,
`OrderType::Limit` ("rest the residual"), `order_type_can_rest`, and the
`Side::Bid` encumbrance arm.

---

## 2. Validation against references

- **marginfi** (`references/marginfi-v2/programs/marginfi/src/state/interest_rate.rs:196-245`):
  `lending_rate_apr = base_rate_apr × utilization`;
  `borrowing_rate_apr = base × (1 + fee_ir) + fee_fixed`. Our replica
  (`protocol/marginfi_rate_calc.rs::calc_interest_rate_fp48`) implements the
  identical decomposition (legacy + seven-point curves, program-fee flag)
  and is already pinned by SBF tests that predict marginfi's post-accrue
  share values exactly. Placement reuses this function; no new rate math.
- **Manifest** (`references/manifest/programs/manifest/src/state/market.rs:166-167`,
  `resting_order.rs:243-256`): two trees (`bids_root/best`, `asks_root/best`)
  with side-aware `Ord` so that *tree max = best* on both sides (bids:
  ascending price; asks: descending). We restore the same wiring with
  `rate_bps` in place of price: best bid = highest rate (borrower paying
  most), best ask = lowest rate.

---

## 3. State layout

### 3.1 GlobalVaultFixed

- **PDA**: `[b"vault", bank]` (was `[b"vault", mint]`). `mint` stays as a
  cached field, derived from `bank.mint` at creation. Signer / integration /
  staging PDAs re-derive from the new vault address; seeds unchanged.
- `next_sub_vault_id`: `u8` → **`u16`** (monotonic, never reused; Private
  creation is permissionless so 255 was griefable).
- `sub_vault_count`: `u8` → `u16`.
- Creation is **permissionless** (anyone may instantiate the vault for a
  bank; creator pays rent). `global_vault_admin` retains only pause and
  Pool-housekeeping powers (sunset/remove/admin-cancel); it does NOT gate
  Pool creation (protocol admin does, see 3.3).
- 10MB single-account ceiling is accepted for v1 (sub-vaults are 512-byte
  blocks; seats/order-refs 160-byte blocks → thousands of sub-vaults).
  Sharding to per-sub-vault PDAs is explicitly out of scope (it would break
  the single-vault-account match-time read).

### 3.2 SubVault (formerly `RiskProfile`)

New/changed fields:

```text
kind: u8                  // 0 = Pool (curator-run), 1 = Private (single-owner)
sub_vault_id: u16         // widened
spread_bps: u16           // rate = market's live marginfi lending APR + spread
max_ltv_bps: u16          // origination cap, curator-set, NOT bounded by
                          // marginfi (D17); AUTO sentinel removed
liquidation_ltv_bps: u16  // liquidation trigger; enforced ≥ max_ltv_bps +
                          // MIN_LIQ_GAP_BPS at create/update (D17)
max_term_seconds: u32     // unchanged; also the term copied into orders
curator: Pubkey           // Pool: admin-assigned curator. Private: the owner
curator_fee_bps: u16      // set at create_pool_sub_vault (≤ MAX_CURATOR_FEE_BPS);
                          // 0 for Private; snapshotted onto MatchedLoan /
                          // LoanFixed at match. Replaces FeeConfig.curator_fee_bps
                          // — one consistent fee per Pool across every market.
                          // NOT editable via UpdateSubVault (admin-set, immutable).
open_orders_count: u16    // resting asks across all markets; inc on place,
                          // dec on cancel/admin-cancel (D16)
open_loans_count: u32     // open loans (queued matches + promoted, all markets);
                          // inc at fill, dec at full close (D16)
```

Removed: nothing — accounting fields (`total_shares`, assets/principal,
weighted-rate aggregates, yield indices, `pending_claim_atoms`,
`accumulated_curator_fee_atoms`) are unchanged.

Private semantics (enforced in processors, same struct):
- `global_vault_deposit` / `global_vault_withdraw` require
  `signer == sub_vault.curator` when `kind == Private`.
- Curator fee forced to 0 (snapshot stamped as 0 at match).
- Creation permissionless; creator pays the vault-realloc rent.

### 3.3 Pool creation (admin-created, no registry)

`CreatePoolSubVault` is **protocol-admin-gated** and takes
`{ curator, spread_bps, max_ltv_bps, max_term_seconds, curator_fee_bps }`
— the same shape as today's `create_risk_profile`, lifted from vault-admin
to protocol-admin, with the fee now a creation parameter
(`curator_fee_bps ≤ MAX_CURATOR_FEE_BPS`, a protocol constant). The fee is
fixed at creation for v1; open loans are protected by the match-time
snapshot regardless. The two-step `TransferCurator`/`AcceptCurator` flow
is unchanged for handing the curator role over. Removing a misbehaving
curator = sunset the sub-vault (existing loans run down normally — the
close paths must keep writing to the sub-vault, so freezing is never an
option).

### 3.4 Market

- `_reserved_bids_root` / `_reserved_bids_best` → **`bids_root_index` /
  `bids_best_index`** (live).
- `RestingOrder.Ord` becomes side-aware (Manifest pattern,
  `resting_order.rs:243`): bids compare `rate asc` (max = highest rate),
  asks keep `rate desc` (max = lowest rate); FIFO tiebreak on sequence in
  both. Bids and asks never share a tree, mirroring Manifest's
  `debug_assert!(same side)`.
- Resting bids carry real `principal_atoms`, `collateral_atoms`, the
  collateral share-price snapshot (field exists), and `last_valid_unix_ts`.
  Collateral is encumbered at rest (the existing IOC-window encumbrance is
  simply not unwound when the residual rests).
- `ClaimedSeat.risk_profile_id` → `sub_vault_id`: `u8` → **`u16`** (repacks
  one padding byte); `OWNER_KIND_RISK_PROFILE` → `OWNER_KIND_SUB_VAULT`.
  `MatchedLoan.lender_profile_id` and `LoanFixed.lender_profile_id` →
  `lender_sub_vault_id: u16` (both have adjacent reserved bytes to absorb
  it).
- `ACCOUNT_LAYOUT_VERSION` bumps; clean slate, no migration.

### 3.5 UserAccount

New node type `UserOrderRef` (payload-size-equal to the existing three so it
shares the free list):

```text
{ market: Pubkey, order_sequence: u64, side: u8, rate_bps: u16,
  term_seconds: u32, principal_atoms: u64, placed_at_unix: i64, ... }
```

Maintained by user place/cancel/update-bid paths and pruned when a bid is
consumed, cancelled, or expired-pruned (prune writes a log; the next
user-signed ix lazily drops a dangling ref — same tolerance as
`sync_market_position`).

The vault-side `RiskProfileOrderRef` is renamed `SubVaultOrderRef`
(unchanged semantics: one per (market, sub_vault_id)).

---

## 4. Rate model

### 4.1 Placement / update

```text
bank_apr_bps   = ceil(calc_interest_rate_fp48(utilization, cfg, fees).lending_apr × 10_000)
stored_rate    = bank_apr_bps + sub_vault.spread_bps     // u16, checked
```

- Rounding is **up** on the bank APR (protects the lender; one bp of
  conservatism).
- `place_order_for_sub_vault` / `update_order_for_sub_vault` take **no rate
  or term params** — both are derived (rate as above; term =
  `sub_vault.max_term_seconds`). Update is therefore a parameterless
  re-sync: cancel-and-replace internally, preserving
  one-ask-per-(sub-vault, market).
- These ixs now require the debt bank + group accounts (they already sit in
  the market header; account list grows by what the take-path needs anyway,
  §5.2).

### 4.2 Fill-time floor (D5)

At every fill attempt against an ask, the engine recomputes
`bank_apr_bps` (same function, accounts already in the tx) and **skips** the
ask if `ask.rate_bps < bank_apr_bps`, emitting `AskSkippedBelowFloorLog
{ market, sub_vault_id, order_sequence, ask_rate_bps, floor_bps }` so
operators can see their quotes go dark. Skip-don't-abort, like every other
per-ask gate. Recovery is one parameterless `update_order_for_sub_vault`
per market.

Borrower bids have **no floor** — a borrower may offer any rate; crossing
already requires `ask_rate ≤ bid_rate` and asks are floored.

### 4.3 Rate stamping (path-independent)

Identical regardless of which side took:

```text
cross requires  ask.rate ≤ bid.rate  AND  bid.term ≤ ask.term
lender_rate   = ask.rate
borrower_rate = max(bid.rate, ask.rate + protocol_fee_bps_floor)
```

---

## 5. Matching engine

### 5.1 Borrower path (`place_order`)

Unchanged IOC-first scan of the asks tree, plus per-ask gates in order:
expiry (bids only, §5.4) → owner-level self-cross (skip) → rate/term cross
check → sub-vault read (idle / sunset / term cap; skip) → **rate floor**
(skip, §4.2) → **sub-vault origination LTV** (skip; the only LTV gate —
oracle-priced collateral must cover `principal / max_ltv_bps`; marginfi
weights are no longer consulted, D17) → reserve fill on sub-vault → mint
`MatchedLoan` (stamping `origination_ltv_bps` + `liquidation_ltv_bps`
from the sub-vault, carried onto `LoanFixed` at promotion).

Residual handling becomes an explicit enum in `PlaceOrderParams`
(replacing `FLAG_OB_ONLY`):

```text
ResidualMode { P2PoolFallback /*default*/, Rest, Drop }
```

- `P2PoolFallback`: as today (marginfi borrow + deposit). This is the one
  path where **marginfi's own LTV still rules** — marginfi's health check
  gates the borrow CPI. A friendly pre-check (marginfi init weights vs the
  residual) errors with `FallbackLtvInsufficient` before the CPI, so a
  high-LTV bid that exceeds marginfi's capacity gets a clear error and
  should be re-sent with `Rest` or `Drop`.
- `Rest`: insert the residual into the bids tree (collateral stays
  encumbered at the placement snapshot; `last_valid_unix_ts` from params).
- `Drop`: unwind encumbrance, `OrderFilledIocLog` (today's OB_ONLY).

### 5.2 Ask-side take (`place_order_for_sub_vault` / `update_order_for_sub_vault`)

After computing the stored rate (§4.1) and before resting, the ix walks the
**bids tree** best-down and fills while `bid.rate ≥ stored_rate`:

per-bid gates: expiry (prune) → owner self-cross (skip) → term
(`bid.term ≤ sub_vault.max_term_seconds`) → sub-vault idle (cap fill at
`idle − reserve`, partial-fill the bid, `RestingOrder::reduce`) → rate
floor already satisfied by construction → **sub-vault origination LTV** at
live oracle prices (skip bid on failure; D17) → reserve on sub-vault →
mint `MatchedLoan` (flags = `VAULT_LENDER`, LTV pair stamped), stamp
lender `open_lend_count`, decrement/consume the bid (consumed collateral
transfers to the MatchedLoan's encumbrance, as today).

Account list grows to roughly the borrower `place_order` set (oracles for
both banks, debt + collateral banks, the vault) — still **zero CPIs**: atom
movement stays deferred to `process_matched_loan`. What is given up is the
tiny-account-list reprice, not the no-CPI property.

A fully-filled resting bid is removed and its block freed; a partial fill
reduces `principal_atoms` and proportionally its collateral.

### 5.3 Match crank (new, permissionless)

`MatchCrank { max_fills }` with the full oracle/vault account set: crosses
best-bid × best-ask while fillable, same gates as §5.2. Needed because
crossability changes without order flow: vault deposits and
`claim_repayment` replenish idle; oracle moves flip LTV gates. No keeper
fee — interested parties (curators deploying idle, borrowers wanting fills,
UIs) crank. Crossed-at-rest is legal between events (D8).

### 5.4 Expiry pruning (D10)

Any scan (borrower take, ask take, crank) that encounters an expired
**bid** removes it: unencumber the seat's collateral at the order's stored
snapshot, free the block, emit `OrderExpiredLog`. Asks are standing quotes
and keep `NO_EXPIRATION` (pruning an ask would require mutating the
vault's `SubVaultOrderRef` tree mid-scan; the fill-time floor and
parameterless re-sync already cover ask staleness).

### 5.5 Self-cross (D9)

`SelfMatchForbidden` extends from seat-index equality to **owner**
equality: the maker seat's economic owner (the sub-vault's `curator` for
sub-vault seats — read from the vault account already in the tx; wallet
for user seats) vs the taker's wallet. Skip, don't abort.

### 5.6 Liquidation gate (D17)

`assert_ltv_breach` stops reading marginfi maint weights. For Fixed loans
it compares live oracle LTV against the loan's **stamped**
`liquidation_ltv_bps` (reusing `get_required_quote_collateral_to_back_debt`
with the stamped cap as the weight, exactly as the origination gate does).
Stamping means curator updates never move thresholds on open loans, and
`liquidation_ltv ≥ max_ltv + MIN_LIQ_GAP_BPS` guarantees no loan is born
liquidatable. Maturity-based settlement (`settle_matured_loan`) is
unchanged. P2Pool positions keep marginfi-derived health (their liability
genuinely lives on marginfi).

---

## 6. Instruction-set delta

### New

| Ix | Gate | Notes |
|---|---|---|
| `CancelOrder` (user bid) | bid owner | unencumber at stored snapshot, free block, drop `UserOrderRef` |
| `UpdateOrder` (user bid) | bid owner | cancel-and-replace; new rate/term/expiry |
| `MatchCrank` | permissionless | §5.3 |
| `CreatePrivateSubVault` | permissionless | `kind=Private`, `curator=signer`, fee=0 |
| `CreatePoolSubVault` | protocol admin | replaces `CreateRiskProfile`; assigns curator + `curator_fee_bps` (§3.3) |

### Changed

| Ix | Change |
|---|---|
| `CreateVault` | permissionless; PDA `[b"vault", bank]`; takes the bank, derives mint |
| `CreateMarket` | born unpaused (D14); fee config is a creation param as today |
| `PlaceOrder` | `flags: OB_ONLY` → `residual_mode` enum; residual may rest |
| `PlaceOrderForSubVault` / `UpdateOrderForSubVault` | renamed from `*ForRiskProfile`; no rate/term params; computes from sub-vault + live bank APR; **takes** resting bids; account list grows (both banks + oracles + vault writable) |
| `CancelOrderForSubVault` / `AdminCancelSubVaultOrder` | renamed; semantics unchanged |
| `GlobalVaultDeposit/Withdraw` | owner-gating when `kind=Private` |
| `UpdateRiskProfile` → `UpdateSubVault` | **curator-gated** (owner for Private); edits `spread_bps` / LTV params / `max_term_seconds` — never the fee (D15) |
| `SunsetRiskProfile` / `ResumeRiskProfile` / `RemoveRiskProfile` | renamed `*SubVault`; removal additionally requires `open_orders_count == 0 && open_loans_count == 0` (D16) |
| `ClaimRepaymentForRiskProfile` | renamed `ClaimRepaymentForSubVault`; semantics unchanged |
| `FeeConfig` | drops `curator_fee_bps` (moved to the sub-vault, §3.2/§3.3) and `ltv_buffer_bps` (D17 — origination LTV is sub-vault-only) |
| `LiquidateLoan` / `CheckLtvLiquidatable` | Fixed loans: gate on the loan's stamped `liquidation_ltv_bps` instead of marginfi maint weights (§5.6) |
| `LoanFixed` / `MatchedLoan` | gain stamped `origination_ltv_bps` + `liquidation_ltv_bps` (reserved bytes absorb them) |
| Matching engine | floor skip, owner self-cross, bid pruning, take-bids path; single sub-vault LTV gate (D17); curator-fee snapshot reads the **sub-vault**, not FeeConfig |
| All sub-vault-id carriers | `u8` → `u16` |

### Removed

- Per-market rate/term parameters on sub-vault order placement.
- `FLAG_OB_ONLY` (subsumed by `residual_mode`).
- `FeeConfig.curator_fee_bps` (now per-sub-vault).
- Vault-admin gating of sub-vault creation (Pool: protocol admin;
  Private: permissionless).

### Unchanged

`Deposit`, `Withdraw`, `Repay`, `ProcessMatchedLoan`, `SettleMaturedLoan`,
`LiquidateLoan`, `ConvertP2PoolToFixed`, `ClaimCuratorFee`, fee config
(minus the curator field), pause/admin-transfer family, check-gates.
(`ConvertP2PoolToFixed`'s refinance scan inherits the floor-skip and
owner-self-cross gates for consistency.)

---

## 7. Invariants (new and restated)

1. **No fillable cross survives a taking ix**: after any `place_order`,
   `place/update_order_for_sub_vault`, or `MatchCrank`, every remaining
   (best-bid, best-ask) pair fails at least one gate (rate, term, idle,
   LTV, floor, expiry, self-cross). Crossed-at-rest is otherwise legal.
2. **Bid-collateral conservation**: Σ resting-bid collateral (at stored
   snapshots) + Σ `MatchedLoan.collateral` + Σ open-loan collateral ==
   Σ seat `collateral_encumbered_shares` (extends the existing
   encumbrance-conservation test to the bid side).
3. **Floor**: no `MatchedLoan` is ever minted with
   `lender_rate_bps < bank lending APR (bps, ceil)` measured in the same
   transaction.
4. **`open_lend_count` = resting asks + open loans** per sub-vault seat
   (stamped at fill, retired at close/cancel — as fixed in the current
   code).
5. **Vault↔bank**: structural via PDA; `vault.lending_pool` field becomes
   redundant but is kept for reads (one 32-byte compare beats a
   `find_program_address` per check; assert field == seed once at
   `create_vault`).
6. **No loan born liquidatable**: `liquidation_ltv_bps ≥ max_ltv_bps +
   MIN_LIQ_GAP_BPS` enforced at sub-vault create/update; both stamped at
   match; the liquidation gate reads only the stamp (D17).
7. **Sub-vault counters**: `open_orders_count` == live `SubVaultOrderRef`
   nodes for that sub-vault; `open_loans_count` == queued matches +
   promoted-unclosed loans; removal requires both zero (D16).

---

## 8. Test plan (delta)

- Bid lifecycle: rest → partial fill by ask placement → full fill by crank;
  cancel/update; expiry prune unencumbers exactly the stored-snapshot
  shares.
- Floor: ask placed at spread S fills; marginfi utilization pushed up
  (mock bank state) → same ask skipped with `AskSkippedBelowFloorLog`;
  parameterless update re-syncs and fills.
- Spread rates: same sub-vault quoting two markets on different banks
  stores different rates.
- Private: second wallet's deposit to a Private sub-vault rejects; owner
  deposit/withdraw round-trips; curator fee snapshot is 0.
- Pool gating: `CreatePoolSubVault` by a non-protocol-admin rejects;
  `curator_fee_bps > MAX_CURATOR_FEE_BPS` rejects; fee snapshot at match
  reads the sub-vault, not FeeConfig.
- Self-cross: wallet bids against its own Private sub-vault's ask →
  skipped, fills the next ask.
- Crossed-at-rest: bid rests against idle-exhausted sub-vault; vault
  deposit; `MatchCrank` fills.
- LTV decoupling: sub-vault with `max_ltv` **above** marginfi's implied
  LTV fills a Fixed loan that marginfi itself would refuse; same
  collateral with `P2PoolFallback` residual errors `FallbackLtvInsufficient`.
- Liquidation stamp: loan at 85/92 stamps; curator raises sub-vault
  `liquidation_ltv` → open loan's threshold unchanged; oracle move past
  the stamped 92% liquidates; `MIN_LIQ_GAP_BPS` violation at
  create/update rejects (no loan born liquidatable).
- Counters: place/cancel round-trips `open_orders_count`; fill/close
  round-trips `open_loans_count`; `RemoveSubVault` rejects while either
  is non-zero.
- Invariant 2 extended in `encumbrance_conservation.rs`.

---

## 9. Pre-implementation cleanup (done)

Removed ahead of the build, since no live deployment constrains us:

- `LTV_AUTO_FROM_MARGINFI` / `LTV_AUTO_BUFFER_BPS` sentinel machinery
  (`effective_max_ltv_bps_for_profile`, `marginfi_implied_max_ltv_bps`,
  the auto-resolution in both engines); `create_risk_profile` now rejects
  `max_ltv_bps = None`.
- Stale secondary-sale framing (the `vault_loan_secondary_sale_constants_align`
  test referenced an `OrderKind::SecondaryLoanSale` that never existed in
  this codebase; renamed to `owner_kind_discriminants_are_pinned`).
- Test-harness `Withdraw` arm + `adapter_withdraw.rs` / `adapter_deposit.rs`
  (superseded by `ydelta::process_withdraw/deposit` end-to-end coverage).
  Harness `Deposit` is retained as setup for `adapter_borrow_repay.rs`;
  `Borrow`/`Repay` stay (no ydelta borrow ix exists).

Deferred to the implementation itself (entangled with new v1 behavior —
removing twice would be wasted motion): `FeeConfig.ltv_buffer_bps` + the
marginfi-weights origination gate (land with the liquidation stamping,
D17), `FLAG_OB_ONLY` (lands with `residual_mode`),
`FeeConfig.curator_fee_bps` (lands with the sub-vault fee field), the
`Option<max_ltv_bps>` → required-params re-signing (lands with
`liquidation_ltv_bps` in the create/update params).

## 10. Explicitly out of scope (v1)

- Secondary sale of loan claims (reserved fields stay reserved) — the
  structural answer to mid-term rate spikes; next major feature.
- Prepayment fees (rejected, D12).
- Vault-account sharding past the 10MB ceiling.
- Ask-side expiry (sub-vault asks remain standing quotes).
- Linking resting bids to P2Pool refinance intent (use
  `ConvertP2PoolToFixed` manually).
