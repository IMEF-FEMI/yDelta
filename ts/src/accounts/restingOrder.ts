/**
 * `RestingOrder` — vault ask body in the asks RB-tree. 144 bytes. Tree key is
 * `Ord on (rate_bps DESC, sequence_number DESC)` — best ask (lowest rate)
 * lands at the tree's `max_index`. The default tree iterator walks descending
 * Ord, i.e. **best ask first**.
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
 *   @46  1    u8    side                (0 = Bid, 1 = Ask — always Ask)
 *   @47  1    u8    order_type          (0 = Limit, 1 = IOC, 2 = PostOnly)
 *   @48  1    u8    flags
 *   @49  1    u8    _pad1
 *   @50  16   u128  share_price_snapshot_fp48
 *   @66  6    u8[6] _pad2
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
  };
}
