import { PublicKey } from '@solana/web3.js';
import BN from 'bn.js';

export const MARKET_POSITION_SIZE = 144;

export type MarketPosition = {
  market: PublicKey;
  seatIndexInMarket: number;
  debtWithdrawableShares: BN;
  debtEncumberedShares: BN;
  collateralWithdrawableShares: BN;
  collateralEncumberedShares: BN;
};

function readU128(buf: Buffer, offset: number): BN {
  return new BN(buf.subarray(offset, offset + 16), 'le');
}

export function decodeMarketPosition(buf: Buffer): MarketPosition {
  return {
    market: new PublicKey(buf.subarray(0, 32)),
    seatIndexInMarket: buf.readUInt32LE(32),
    debtWithdrawableShares: readU128(buf, 48),
    debtEncumberedShares: readU128(buf, 64),
    collateralWithdrawableShares: readU128(buf, 80),
    collateralEncumberedShares: readU128(buf, 96),
  };
}
