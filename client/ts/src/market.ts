import { Connection, PublicKey } from '@solana/web3.js';
import {
  MarketFixed,
  decodeMarketFixed,
  MARKET_FIXED_SIZE,
  ClaimedSeat,
  decodeClaimedSeat,
  RestingOrder,
  decodeRestingOrder,
  MatchedLoan,
  decodeMatchedLoan,
} from './ydelta/accounts';
import { Side, OwnerKind } from './ydelta/types/enums';
import { FeeConfig } from './ydelta/types/FeeConfig';
import { MARKET_BLOCK_SIZE, RBTREE_OVERHEAD_BYTES, isNotNil } from './utils/discriminator';
import { inOrderTraverseReverse, collectTree } from './utils/redBlackTree';

export type L2Level = {
  /** Annualised rate in basis points. */
  rateBps: number;
  /** Loan term in seconds. */
  termSeconds: number;
  /** Aggregated principal across orders at this (rate, term) bucket. */
  principalAtoms: bigint;
  /** Number of resting orders contributing to this level. */
  orderCount: number;
};

/** Wraps a `MarketFixed` account: header decode + RB-tree traversal of bids,
 *  asks, claimed seats, and matched loans. */
export class Market {
  readonly address: PublicKey;
  private _buffer: Buffer;
  private _header: MarketFixed;

  private constructor(address: PublicKey, buffer: Buffer) {
    this.address = address;
    this._buffer = buffer;
    this._header = decodeMarketFixed(buffer);
  }

  static async loadFromAddress({
    connection,
    address,
  }: {
    connection: Connection;
    address: PublicKey;
  }): Promise<Market> {
    const info = await connection.getAccountInfo(address);
    if (!info) throw new Error(`Market ${address.toBase58()} not found`);
    return new Market(address, Buffer.from(info.data));
  }

  static loadFromBuffer({
    address,
    buffer,
  }: {
    address: PublicKey;
    buffer: Buffer;
  }): Market {
    return new Market(address, buffer);
  }

  async reload(connection: Connection): Promise<void> {
    const info = await connection.getAccountInfo(this.address);
    if (!info) throw new Error(`Market ${this.address.toBase58()} not found`);
    this._buffer = Buffer.from(info.data);
    this._header = decodeMarketFixed(this._buffer);
  }

  /** Decoded `MarketFixed` header. */
  header(): MarketFixed {
    return this._header;
  }

  private dynamic(): Buffer {
    return this._buffer.subarray(MARKET_FIXED_SIZE);
  }

  // Both bid and ask trees place the BEST maker at `max_index` (rightmost),
  // per `RestingOrder::Ord` in resting_order.rs — bids order by rate ascending,
  // asks invert the rate comparison. Reverse-in-order (right-root-left)
  // therefore yields best-first for either side.

  /** All resting bids, best-first (highest rate). */
  bids(): RestingOrder[] {
    if (!isNotNil(this._header.bidsRootIndex)) return [];
    return [...this.iterBids()];
  }

  /** All resting asks, best-first (lowest rate). */
  asks(): RestingOrder[] {
    if (!isNotNil(this._header.asksRootIndex)) return [];
    return [...this.iterAsks()];
  }

  /** Aggregate bids into L2 levels keyed by `(rate_bps, term_seconds)`. */
  bidsL2(depth?: number): L2Level[] {
    return this.aggregateL2(this.bids(), depth);
  }

  /** Aggregate asks into L2 levels. */
  asksL2(depth?: number): L2Level[] {
    return this.aggregateL2(this.asks(), depth);
  }

  private aggregateL2(orders: RestingOrder[], depth?: number): L2Level[] {
    const buckets = new Map<string, L2Level>();
    const keys: string[] = [];
    for (const o of orders) {
      const k = `${o.rateBps}|${o.termSeconds}`;
      const existing = buckets.get(k);
      if (existing) {
        existing.principalAtoms += BigInt(o.principalAtoms.toString());
        existing.orderCount += 1;
      } else {
        keys.push(k);
        buckets.set(k, {
          rateBps: o.rateBps,
          termSeconds: o.termSeconds,
          principalAtoms: BigInt(o.principalAtoms.toString()),
          orderCount: 1,
        });
      }
    }
    const levels = keys.map((k) => buckets.get(k)!);
    return depth ? levels.slice(0, depth) : levels;
  }

  /** Best bid (highest rate) — `undefined` if the bids tree is empty.
   *  Reads the header's cached `bids_best_index` and decodes a single node
   *  in O(1), instead of walking the whole tree. */
  bestBid(): RestingOrder | undefined {
    const idx = this._header.bidsBestIndex;
    if (!isNotNil(idx)) return undefined;
    return this.decodeOrderAt(idx);
  }

  /** Best ask (lowest rate). O(1) via header-cached `asks_best_index`. */
  bestAsk(): RestingOrder | undefined {
    const idx = this._header.asksBestIndex;
    if (!isNotNil(idx)) return undefined;
    return this.decodeOrderAt(idx);
  }

  private decodeOrderAt(dataIndex: number): RestingOrder | undefined {
    const dyn = this.dynamic();
    const payloadStart = dataIndex + RBTREE_OVERHEAD_BYTES;
    const payloadLen = MARKET_BLOCK_SIZE - RBTREE_OVERHEAD_BYTES;
    if (payloadStart + payloadLen > dyn.length) return undefined;
    return decodeRestingOrder(dyn.subarray(payloadStart, payloadStart + payloadLen));
  }

  /** Mid-rate between best bid and ask in bps. `undefined` if either side is empty. */
  midBps(): number | undefined {
    const b = this.bestBid();
    const a = this.bestAsk();
    if (!b || !a) return undefined;
    return (b.rateBps + a.rateBps) / 2;
  }

  /** All claimed seats (both user and vault flavours) in tree order. */
  claimedSeats(): ClaimedSeat[] {
    if (!isNotNil(this._header.claimedSeatsRootIndex)) return [];
    return collectTree(
      this.dynamic(),
      this._header.claimedSeatsRootIndex,
      MARKET_BLOCK_SIZE,
      decodeClaimedSeat,
    );
  }

  /** Wallet-owned seats only. */
  userSeats(): ClaimedSeat[] {
    return this.claimedSeats().filter((s) => s.ownerKind === OwnerKind.User);
  }

  /** Vault-owned seats only. */
  vaultSeats(): ClaimedSeat[] {
    return this.claimedSeats().filter((s) => s.ownerKind === OwnerKind.Vault);
  }

  /** Wallet seat (`owner_kind == User`) for `owner`. */
  userSeatFor(owner: PublicKey): ClaimedSeat | null {
    for (const seat of this.claimedSeats()) {
      if (seat.ownerKind === OwnerKind.User && seat.owner.equals(owner)) return seat;
    }
    return null;
  }

  /** Vault seat (`owner_kind == Vault`) for `(vault, profileId)`. */
  vaultSeatFor(vault: PublicKey, profileId: number): ClaimedSeat | null {
    for (const seat of this.claimedSeats()) {
      if (
        seat.ownerKind === OwnerKind.Vault &&
        seat.owner.equals(vault) &&
        seat.riskProfileId === profileId
      ) {
        return seat;
      }
    }
    return null;
  }

  /** Generic `(owner, profile_id)` lookup. Prefer `userSeatFor` / `vaultSeatFor`. */
  seatFor(owner: PublicKey, profileId: number = 0): ClaimedSeat | undefined {
    for (const seat of this.claimedSeats()) {
      if (seat.owner.equals(owner) && seat.riskProfileId === profileId) {
        return seat;
      }
    }
    return undefined;
  }

  /** Pending matched-loan queue (cranker work). */
  matchedLoans(): MatchedLoan[] {
    if (!isNotNil(this._header.matchedLoansRootIndex)) return [];
    return collectTree(
      this.dynamic(),
      this._header.matchedLoansRootIndex,
      MARKET_BLOCK_SIZE,
      decodeMatchedLoan,
    );
  }

  /** Iterator alternative for large books; avoids materialising the full list.
   *  Yields best-first (highest rate). */
  *iterBids(): Generator<RestingOrder> {
    if (!isNotNil(this._header.bidsRootIndex)) return;
    yield* inOrderTraverseReverse(
      this.dynamic(),
      this._header.bidsRootIndex,
      MARKET_BLOCK_SIZE,
      decodeRestingOrder,
    );
  }

  /** Yields asks best-first (lowest rate). */
  *iterAsks(): Generator<RestingOrder> {
    if (!isNotNil(this._header.asksRootIndex)) return;
    yield* inOrderTraverseReverse(
      this.dynamic(),
      this._header.asksRootIndex,
      MARKET_BLOCK_SIZE,
      decodeRestingOrder,
    );
  }

  /** Decode the seat at `dataIndex` (a byte-offset returned by
   *  `loan.lender_seat_index` / `loan.borrower_seat_index`). Verifies that
   *  `dataIndex` is currently a member of the claimed-seats tree before
   *  decoding — guards against stale indices pointing at slots that have
   *  been freed and reused for a different payload type. */
  seatAtIndex(dataIndex: number): ClaimedSeat | null {
    if (!isNotNil(dataIndex)) return null;
    const dyn = this.dynamic();
    const payloadStart = dataIndex + RBTREE_OVERHEAD_BYTES;
    const payloadLen = MARKET_BLOCK_SIZE - RBTREE_OVERHEAD_BYTES;
    if (payloadStart + payloadLen > dyn.length) return null;
    if (!this.indexIsInTree(dataIndex, this._header.claimedSeatsRootIndex)) return null;
    return decodeClaimedSeat(dyn.subarray(payloadStart, payloadStart + payloadLen));
  }

  /** Walk the subtree rooted at `rootIndex` and return true iff `target` is
   *  one of the node indices (linear over tree size; trees are bounded by
   *  the per-market block budget so this is cheap in practice). */
  private indexIsInTree(target: number, rootIndex: number): boolean {
    if (!isNotNil(rootIndex)) return false;
    const dyn = this.dynamic();
    const stack: number[] = [rootIndex];
    while (stack.length > 0) {
      const cur = stack.pop()!;
      if (cur === target) return true;
      const left = dyn.readUInt32LE(cur + 0);
      const right = dyn.readUInt32LE(cur + 4);
      if (isNotNil(left)) stack.push(left);
      if (isNotNil(right)) stack.push(right);
    }
    return false;
  }

  // ─── Header convenience accessors ───

  admin(): PublicKey {
    return this._header.admin;
  }

  pendingAdmin(): PublicKey {
    return this._header.pendingAdmin;
  }

  isPaused(): boolean {
    return this._header.isPaused;
  }

  feeConfig(): FeeConfig {
    return this._header.feeConfig;
  }

  debtMint(): PublicKey {
    return this._header.debtMint;
  }

  collateralMint(): PublicKey {
    return this._header.collateralMint;
  }

  debtBank(): PublicKey {
    return this._header.debtLendingPool;
  }

  collateralBank(): PublicKey {
    return this._header.collateralLendingPool;
  }

  marginfiGroup(): PublicKey {
    return this._header.marginfiGroup;
  }

  marketSigner(): PublicKey {
    return this._header.marketSigner;
  }
}

// Re-export for convenience.
export { Side, OwnerKind };
