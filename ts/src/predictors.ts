/**
 * `place_order` predictor — simulate a borrower IOC bid against the current
 * resting asks tree + vault sub-vault state. Mirrors the on-chain
 * matching engine's per-cross rules:
 *
 *   1. `ask.rate_bps ≤ bid.rate_bps`           — price compatibility
 *   2. `ask.term_seconds ≥ bid.term_seconds`   — term compatibility
 *      (the ask's term is a ceiling; a strategy quoting "up to N days"
 *      funds any borrower term ≤ N. Rate ordering is independent of
 *      term, so a worse-rate ask with a longer term may still cross —
 *      walk past, don't break.)
 *   3. Sub-vault LTV: `actual_ltv ≤ subVault.max_ltv_bps`
 *      (`actual_ltv` is computed at oracle prices — pass `ltvEstimator`
 *      for real oracle-driven estimation; the default treats
 *      `principal_atoms / collateral_atoms × 10_000` as a stand-in.)
 *   4. Sub-vault idle cap: each cross size ≤ subVault.idle_principal_atoms
 *
 * The on-chain match walks the asks tree best-first (lowest rate first); we
 * mirror that via `iterAsks` (descending Ord = best-first).
 *
 * Ask → sub-vault resolution is the canonical on-chain path:
 *   ask.trader_seat_index → market.ClaimedSeat[index] →
 *   seat.owner_kind == SubVault && seat.sub_vault_id → subVault.
 */
import type { Market } from './accounts/market.js';
import type { RestingOrder } from './accounts/restingOrder.js';
import type { SubVault } from './accounts/subVault.js';
import type { GlobalVault } from './accounts/vault.js';
import { lookupClaimedSeatAt } from './accounts/market.js';
import { OwnerKind } from './types.js';

export interface Cross {
  askIndex: number;
  askRateBps: number;
  askTermSeconds: number;
  matchedPrincipalAtoms: bigint;
  matchedCollateralAtoms: bigint;
  subVaultId: number;
  curatorRateBps: number;
}

export type SkipReason =
  | 'rate'
  | 'term'
  | 'ltv'
  | 'subVaultIdleExhausted'
  | 'subVaultNotFound'
  | 'seatNotFound'
  | 'seatNotSubVault';

export interface SimulationResult {
  fills: Cross[];
  /** Principal left over after the last cross. */
  residualPrincipalAtoms: bigint;
  /** True when the residual would route to the P2Pool fallback (`ResidualMode.P2PoolFallback`). */
  residualGoesToP2Pool: boolean;
  /** Why each candidate ask was skipped (for diagnostics). */
  skipped: Array<{ askIndex: number; reason: SkipReason }>;
}

export interface PlaceOrderSimArgs {
  market: Market;
  /** Raw market account bytes — needed for the seat random-access lookup. */
  marketAccountData: Uint8Array | Buffer;
  vault: GlobalVault;
  bidRateBps: number;
  bidTermSeconds: number;
  bidPrincipalAtoms: bigint;
  bidCollateralAtoms: bigint;
  /**
   * When true, the residual does NOT fall through to the marginfi P2Pool
   * borrow (mirrors `ResidualMode.Rest`/`Drop`); when false/omitted the
   * residual routes to P2Pool (`ResidualMode.P2PoolFallback`, the default).
   */
  noFallback?: boolean;
  /**
   * Pluggable LTV estimator. Receives the candidate match's sub-vault +
   * pro-rata principal/collateral; returns the actual LTV in bps. Replace
   * with an oracle-driven version for pre-flight accuracy.
   */
  ltvEstimator?: (args: {
    subVault: SubVault;
    principalAtoms: bigint;
    collateralAtoms: bigint;
  }) => number;
}

function defaultLtvEstimator(args: {
  subVault: SubVault;
  principalAtoms: bigint;
  collateralAtoms: bigint;
}): number {
  if (args.collateralAtoms === 0n) return Number.MAX_SAFE_INTEGER;
  return Number((args.principalAtoms * 10_000n) / args.collateralAtoms);
}

/**
 * Simulate a `place_order` bid against the current market.
 *
 * Caveats:
 * - Per-sub-vault idle decrements are tracked across the walk, so multiple
 *   crosses against the same sub-vault share the same idle pool.
 * - The default LTV estimator is a price-naïve placeholder. Pass
 *   `ltvEstimator` for oracle-based estimation.
 * - The on-chain processor also rejects self-matches (taker and maker
 *   share a seat); since this sim doesn't know the bid's seat, that check
 *   is skipped.
 */
export function simulatePlaceOrder(args: PlaceOrderSimArgs): SimulationResult {
  const fills: Cross[] = [];
  const skipped: SimulationResult['skipped'] = [];
  const ltvEstimator = args.ltvEstimator ?? defaultLtvEstimator;

  // Per-sub-vault running idle pools — drained as we match.
  const idlePool = new Map<number, bigint>();
  const subVaultById = new Map<number, SubVault>();
  for (const { subVault } of args.vault.subVaults) {
    subVaultById.set(subVault.subVaultId, subVault);
    const idle =
      subVault.totalPrincipalAtoms >= subVault.deployedPrincipalAtoms
        ? subVault.totalPrincipalAtoms - subVault.deployedPrincipalAtoms
        : 0n;
    idlePool.set(subVault.subVaultId, idle);
  }

  let remaining = args.bidPrincipalAtoms;

  for (const { index, order } of args.market.asks) {
    if (remaining === 0n) break;

    const ask: RestingOrder = order;
    if (ask.rateBps > args.bidRateBps) {
      // Asks are best-first; first too-expensive ask ends the walk.
      skipped.push({ askIndex: index, reason: 'rate' });
      break;
    }
    if (ask.termSeconds < args.bidTermSeconds) {
      // Ask's term is a ceiling: it can't fund a longer-term loan. Rate
      // ordering is term-independent, so a later ask may still qualify.
      skipped.push({ askIndex: index, reason: 'term' });
      continue;
    }

    // Resolve the ask's maker: seat → (owner_kind, sub_vault_id) → subVault.
    const seat = lookupClaimedSeatAt(args.marketAccountData, ask.traderSeatIndex);
    if (!seat) {
      skipped.push({ askIndex: index, reason: 'seatNotFound' });
      continue;
    }
    if (seat.ownerKind !== OwnerKind.SubVault) {
      // The book is quote-only — every ask should belong to a vault. A
      // non-SubVault maker would be a protocol-level invariant break.
      skipped.push({ askIndex: index, reason: 'seatNotSubVault' });
      continue;
    }
    const subVault = subVaultById.get(seat.subVaultId);
    if (!subVault) {
      skipped.push({ askIndex: index, reason: 'subVaultNotFound' });
      continue;
    }

    // Pro-rata principal/collateral — sized by remaining principal, capped
    // by the sub-vault's live idle pool.
    const idle = idlePool.get(subVault.subVaultId) ?? 0n;
    if (idle === 0n) {
      skipped.push({ askIndex: index, reason: 'subVaultIdleExhausted' });
      continue;
    }
    const matchedPrincipal = remaining < idle ? remaining : idle;
    const matchedCollateral =
      args.bidPrincipalAtoms === 0n
        ? 0n
        : (args.bidCollateralAtoms * matchedPrincipal) / args.bidPrincipalAtoms;

    const ltv = ltvEstimator({
      subVault,
      principalAtoms: matchedPrincipal,
      collateralAtoms: matchedCollateral,
    });
    if (ltv > subVault.maxLtvBps) {
      skipped.push({ askIndex: index, reason: 'ltv' });
      continue;
    }

    fills.push({
      askIndex: index,
      askRateBps: ask.rateBps,
      askTermSeconds: ask.termSeconds,
      matchedPrincipalAtoms: matchedPrincipal,
      matchedCollateralAtoms: matchedCollateral,
      subVaultId: subVault.subVaultId,
      curatorRateBps: ask.rateBps,
    });

    remaining -= matchedPrincipal;
    idlePool.set(subVault.subVaultId, idle - matchedPrincipal);
  }

  return {
    fills,
    residualPrincipalAtoms: remaining,
    residualGoesToP2Pool: remaining > 0n && !args.noFallback,
    skipped,
  };
}
