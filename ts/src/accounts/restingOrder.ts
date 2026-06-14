/**
 * `RestingOrder` — a resting order body in the bid OR ask RB-tree. 144 bytes.
 * `side` says which: asks come from vault sub-vaults, bids are rested borrower
 * residuals (v1 D6). The rate sense of the tree key is flipped per side, so the
 * ask tree's `max_index` is the lowest rate (best ask) and the bid tree's
 * `max_index` is the highest rate (best bid) — the descending iterator
 * (`iterAsks` / `iterBids`) walks **best-first** on either side.
 *
 * Layout:
 *   @0   4    u32   trader_seat_index   (DataIndex of the vault's ClaimedSeat)
 *   @4   4    u8[4] _pad0
 *   @8   8    u64   sequence_number
 *   @16  8    u64   principal_atoms
 *   @24  8    u64   collateral_atoms
 *   @32  8    i64   last_valid_unix_ts  (0 = no expiration)
 *   @40  4    u32   term_seconds
 *   @44  2    u16   rate_bps
 *   @46  1    u8    side                (0 = Bid, 1 = Ask)
 *   @47  1    u8    order_type          (0 = Limit, 1 = IOC, 2 = PostOnly)
 *   @48  1    u8    flags
 *   @49  1    u8    _pad1
 *   @50  16   u128  share_price_snapshot_fp48
 *   @66  2    u16   ltv_buffer_bps      (borrower LTV buffer; persists on a rested bid)
 *   @68  4    u8[4] _pad2
 *   @72  72   u64[9] _reserved
 */
import { OrderType, Side } from '../types.js';
import { readI64, readU128, readU16, readU32, readU64, readU8 } from './_read.js';

export const RESTING_ORDER_SIZE = 144;

export interface RestingOrder {
  traderSeatIndex: number;
  sequenceNumber: bigint;
  principalAtoms: bigint;
  collateralAtoms: bigint;
  lastValidUnixTs: bigint;
  termSeconds: number;
  rateBps: number;
  side: Side;
  orderType: OrderType;
  flags: number;
  sharePriceSnapshotFp48: bigint;
  /**
   * Borrower-set LTV buffer (bps). On a rested bid it persists so a later
   * ask / MatchCrank cross honors the same `effective_cap`. `0` for asks and
   * for bids placed without a buffer.
   */
  ltvBufferBps: number;
}

export function decodeRestingOrder(payload: DataView): RestingOrder {
  return {
    traderSeatIndex: readU32(payload, 0),
    sequenceNumber: readU64(payload, 8),
    principalAtoms: readU64(payload, 16),
    collateralAtoms: readU64(payload, 24),
    lastValidUnixTs: readI64(payload, 32),
    termSeconds: readU32(payload, 40),
    rateBps: readU16(payload, 44),
    side: readU8(payload, 46) as Side,
    orderType: readU8(payload, 47) as OrderType,
    flags: readU8(payload, 48),
    sharePriceSnapshotFp48: readU128(payload, 50),
    ltvBufferBps: readU16(payload, 66),
  };
}
