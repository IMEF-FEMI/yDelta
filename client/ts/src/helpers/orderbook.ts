import BN from 'bn.js';
import { Market, L2Level } from '../market';
import { RestingOrder } from '../ydelta/accounts';
import { OrderKind, OrderType } from '../ydelta/types/enums';

export type FillPreview = {
  /** Maker order matched against. */
  maker: RestingOrder;
  /** Atoms consumed from this maker (≤ maker.principalAtoms). */
  filledAtoms: BN;
  /** Effective rate used for this fill (the maker's rate). */
  rateBps: number;
};

export type MatchPreview = {
  /** Fills in matching order (best-first per maker side). */
  fills: FillPreview[];
  /** Sum of `filledAtoms` across fills. */
  totalFilled: BN;
  /** Principal not filled (i.e. would rest as the taker's order, or P2Pool fallback for bids). */
  residualPrincipal: BN;
  /** Atom-weighted average maker rate across fills. 0 when no fills. */
  weightedAvgRateBps: number;
};

export type FindMatchableInput = {
  /** Taker order rate in basis points. */
  rateBps: number;
  /** Taker term in seconds. */
  termSeconds: number;
  /** Total atoms the taker is offering / borrowing. */
  principalAtoms: BN | bigint | number;
  /** Order type — `PostOnly` returns 0 fills; `ImmediateOrCancel` doesn't rest the residual.
   *  Default `Limit`. */
  orderType?: OrderType;
  /** Optional self-match guard: skip makers whose `traderSeatIndex` matches. */
  ownSeatIndex?: number;
  /** Optional cap on number of makers to walk (compute-budget guard). */
  maxMakers?: number;
};

function crossesBidAgainstAsk(
  bidRateBps: number,
  askRateBps: number,
  bidTerm: number,
  askTerm: number,
  protocolFeeBpsFloor: number,
): boolean {
  const spread = bidRateBps - askRateBps;
  return spread >= protocolFeeBpsFloor && bidTerm <= askTerm;
}

function toBN(v: BN | bigint | number): BN {
  if (BN.isBN(v)) return v;
  return new BN(v.toString());
}

/** Walk the asks tree applying the same gating rules as `place_order` to
 *  preview a bid's cross. Returns the fills (best-first), residual principal,
 *  and atom-weighted average rate.
 *
 *  Limitations: this does NOT apply vault-LTV gating, vault-exposure caps,
 *  or oracle-driven collateral checks — for those, simulate the tx on-chain.
 *  Sufficient for UI cross-preview where the user provides ample collateral. */
export function findMatchableAsks(
  market: Market,
  input: FindMatchableInput,
): MatchPreview {
  if (input.orderType === OrderType.PostOnly) {
    return emptyPreview(toBN(input.principalAtoms));
  }
  const floor = market.feeConfig().protocolFeeBpsFloor;
  let residual = toBN(input.principalAtoms);
  const fills: FillPreview[] = [];
  let weightedSum = new BN(0);
  let walked = 0;
  for (const ask of market.iterAsks()) {
    if (input.maxMakers !== undefined && walked >= input.maxMakers) break;
    walked++;
    if (ask.kind !== OrderKind.Primary) continue;
    if (input.ownSeatIndex !== undefined && ask.traderSeatIndex === input.ownSeatIndex) continue;
    if (!crossesBidAgainstAsk(input.rateBps, ask.rateBps, input.termSeconds, ask.termSeconds, floor)) {
      // Asks are walked best-first (lowest rate). Once an ask's rate breaches
      // `bid_rate - floor`, every deeper ask has an even higher rate, so the
      // protocol-fee-floor gate will continue to fail. Term-gate can flip on
      // a deeper ask, so we only short-circuit on rate.
      if (ask.rateBps > input.rateBps - floor) break;
      continue;
    }
    if (residual.lten(0)) break;
    const fill = BN.min(residual, ask.principalAtoms);
    fills.push({ maker: ask, filledAtoms: fill, rateBps: ask.rateBps });
    weightedSum = weightedSum.add(fill.muln(ask.rateBps));
    residual = residual.sub(fill);
  }
  const totalFilled = toBN(input.principalAtoms).sub(residual);
  const weightedAvgRateBps = totalFilled.isZero()
    ? 0
    : weightedSum.div(totalFilled).toNumber();
  // ImmediateOrCancel doesn't rest residual; report it as cancelled.
  return {
    fills,
    totalFilled,
    residualPrincipal: input.orderType === OrderType.ImmediateOrCancel ? new BN(0) : residual,
    weightedAvgRateBps,
  };
}

/** Walk the bids tree to preview an ask's cross. Bids best = highest rate. */
export function findMatchableBids(
  market: Market,
  input: FindMatchableInput,
): MatchPreview {
  if (input.orderType === OrderType.PostOnly) {
    return emptyPreview(toBN(input.principalAtoms));
  }
  const floor = market.feeConfig().protocolFeeBpsFloor;
  let residual = toBN(input.principalAtoms);
  const fills: FillPreview[] = [];
  let weightedSum = new BN(0);
  let walked = 0;
  for (const bid of market.iterBids()) {
    if (input.maxMakers !== undefined && walked >= input.maxMakers) break;
    walked++;
    if (bid.kind !== OrderKind.Primary) continue;
    if (input.ownSeatIndex !== undefined && bid.traderSeatIndex === input.ownSeatIndex) continue;
    if (!crossesBidAgainstAsk(bid.rateBps, input.rateBps, bid.termSeconds, input.termSeconds, floor)) {
      // Bids are walked best-first (highest rate). Once a bid drops below
      // `ask_rate + floor`, deeper bids have even lower rates and also fail.
      if (bid.rateBps < input.rateBps + floor) break;
      continue;
    }
    if (residual.lten(0)) break;
    const fill = BN.min(residual, bid.principalAtoms);
    fills.push({ maker: bid, filledAtoms: fill, rateBps: bid.rateBps });
    weightedSum = weightedSum.add(fill.muln(bid.rateBps));
    residual = residual.sub(fill);
  }
  const totalFilled = toBN(input.principalAtoms).sub(residual);
  const weightedAvgRateBps = totalFilled.isZero()
    ? 0
    : weightedSum.div(totalFilled).toNumber();
  return {
    fills,
    totalFilled,
    residualPrincipal: input.orderType === OrderType.ImmediateOrCancel ? new BN(0) : residual,
    weightedAvgRateBps,
  };
}

function emptyPreview(principal: BN): MatchPreview {
  return {
    fills: [],
    totalFilled: new BN(0),
    residualPrincipal: principal,
    weightedAvgRateBps: 0,
  };
}

export type OrderbookSnapshot = {
  bids: L2Level[];
  asks: L2Level[];
  bestBidBps?: number;
  bestAskBps?: number;
  spreadBps?: number;
  midBps?: number;
};

/** L2 snapshot suitable for an order-entry UI. */
export function orderbookSnapshot(market: Market, depth?: number): OrderbookSnapshot {
  const bids = market.bidsL2(depth);
  const asks = market.asksL2(depth);
  const bestBidBps = bids[0]?.rateBps;
  const bestAskBps = asks[0]?.rateBps;
  return {
    bids,
    asks,
    bestBidBps,
    bestAskBps,
    spreadBps: bestBidBps !== undefined && bestAskBps !== undefined ? bestBidBps - bestAskBps : undefined,
    midBps: market.midBps(),
  };
}

