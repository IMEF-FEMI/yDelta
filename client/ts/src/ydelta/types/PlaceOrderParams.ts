import * as beet from '@metaplex-foundation/beet';
import BN from 'bn.js';

export type PlaceOrderParams = {
  seatIndexHint: beet.COption<number>;
  side: number;
  orderType: number;
  flags: number;
  kind: number;
  rateBps: number;
  termSeconds: number;
  principalAtoms: BN;
  collateralAtoms: BN;
  askingPriceAtoms: BN;
  lastValidUnixTs: BN;
  borrowerLtvBps: beet.COption<number>;
};

export const placeOrderParamsBeet = new beet.FixableBeetArgsStruct<PlaceOrderParams>(
  [
    ['seatIndexHint', beet.coption(beet.u32)],
    ['side', beet.u8],
    ['orderType', beet.u8],
    ['flags', beet.u8],
    ['kind', beet.u8],
    ['rateBps', beet.u16],
    ['termSeconds', beet.u32],
    ['principalAtoms', beet.u64],
    ['collateralAtoms', beet.u64],
    ['askingPriceAtoms', beet.u64],
    ['lastValidUnixTs', beet.i64],
    ['borrowerLtvBps', beet.coption(beet.u16)],
  ],
  'PlaceOrderParams',
);
