import { PublicKey } from '@solana/web3.js';
import BN from 'bn.js';
import { Side, OrderKind, OrderType } from '../../types/enums';

export const RESTING_ORDER_SIZE = 144;

export type RestingOrder = {
  traderSeatIndex: number;
  /** Set on `SecondaryLoanSale` orders to the referenced loan's sequence. Zero otherwise. */
  loanSequenceSnapshot: number;
  sequenceNumber: BN;
  principalAtoms: BN;
  collateralAtoms: BN;
  /** Par-exit invariant: for `SecondaryLoanSale` always equals `principal_atoms`. */
  askingPriceAtoms: BN;
  lastValidUnixTs: BN;
  /** All-zero for primary orders. */
  loanPda: PublicKey;
  termSeconds: number;
  rateBps: number;
  side: Side;
  kind: OrderKind;
  orderType: OrderType;
  flags: number;
  /** fp48 snapshot of the side-relevant bank's `asset_share_value` at place-order time. */
  sharePriceSnapshotFp48: BN;
  borrowerLtvBps: number;
};

function readI64(buf: Buffer, offset: number): BN {
  const u = new BN(buf.subarray(offset, offset + 8), 'le');
  const TWO_64 = new BN(1).shln(64);
  return u.testn(63) ? u.sub(TWO_64) : u;
}

export function decodeRestingOrder(buf: Buffer): RestingOrder {
  return {
    traderSeatIndex: buf.readUInt32LE(0),
    loanSequenceSnapshot: buf.readUInt32LE(4),
    sequenceNumber: new BN(buf.subarray(8, 16), 'le'),
    principalAtoms: new BN(buf.subarray(16, 24), 'le'),
    collateralAtoms: new BN(buf.subarray(24, 32), 'le'),
    askingPriceAtoms: new BN(buf.subarray(32, 40), 'le'),
    lastValidUnixTs: readI64(buf, 40),
    loanPda: new PublicKey(buf.subarray(48, 80)),
    termSeconds: buf.readUInt32LE(80),
    rateBps: buf.readUInt16LE(84),
    side: buf.readUInt8(86) as Side,
    kind: buf.readUInt8(87) as OrderKind,
    orderType: buf.readUInt8(88) as OrderType,
    flags: buf.readUInt8(89),
    sharePriceSnapshotFp48: new BN(buf.subarray(90, 106), 'le'),
    borrowerLtvBps: buf.readUInt16LE(106),
  };
}

/** `last_valid_unix_ts == 0` is the "never expires" sentinel. */
export const NO_EXPIRATION_LAST_VALID_UNIX_TS = 0;

export function isExpired(order: RestingOrder, nowUnixTs: number): boolean {
  if (order.lastValidUnixTs.isZero()) return false;
  return order.lastValidUnixTs.toNumber() < nowUnixTs;
}
