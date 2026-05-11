import { PublicKey } from '@solana/web3.js';
import BN from 'bn.js';
import { Side } from '../../types/enums';

export const RISK_PROFILE_ORDER_REF_SIZE = 144;

export type RiskProfileOrderRef = {
  market: PublicKey;
  profileId: number;
  side: Side;
  rateBps: number;
  termSeconds: number;
  orderSequenceInMarket: BN;
  placedAtUnix: BN;
};

function readI64(buf: Buffer, offset: number): BN {
  const u = new BN(buf.subarray(offset, offset + 8), 'le');
  const TWO_64 = new BN(1).shln(64);
  return u.testn(63) ? u.sub(TWO_64) : u;
}

export function decodeRiskProfileOrderRef(buf: Buffer): RiskProfileOrderRef {
  return {
    market: new PublicKey(buf.subarray(0, 32)),
    profileId: buf.readUInt8(32),
    side: buf.readUInt8(33) as Side,
    rateBps: buf.readUInt16LE(36),
    termSeconds: buf.readUInt32LE(40),
    orderSequenceInMarket: new BN(buf.subarray(48, 56), 'le'),
    placedAtUnix: readI64(buf, 56),
  };
}
