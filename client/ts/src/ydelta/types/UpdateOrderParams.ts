import * as beet from '@metaplex-foundation/beet';
import BN from 'bn.js';

export type UpdateOrderParams = {
  orderSequenceNumber: BN;
  orderIndexHint: beet.COption<number>;
  seatIndexHint: beet.COption<number>;
  newRateBps: beet.COption<number>;
  newTermSeconds: beet.COption<number>;
  newPrincipalAtoms: beet.COption<BN>;
  newCollateralAtoms: beet.COption<BN>;
  newLastValidUnixTs: beet.COption<BN>;
  newFlags: beet.COption<number>;
  newAskingPriceAtoms: beet.COption<BN>;
};

export const updateOrderParamsBeet = new beet.FixableBeetArgsStruct<UpdateOrderParams>(
  [
    ['orderSequenceNumber', beet.u64],
    ['orderIndexHint', beet.coption(beet.u32)],
    ['seatIndexHint', beet.coption(beet.u32)],
    ['newRateBps', beet.coption(beet.u16)],
    ['newTermSeconds', beet.coption(beet.u32)],
    ['newPrincipalAtoms', beet.coption(beet.u64)],
    ['newCollateralAtoms', beet.coption(beet.u64)],
    ['newLastValidUnixTs', beet.coption(beet.i64)],
    ['newFlags', beet.coption(beet.u8)],
    ['newAskingPriceAtoms', beet.coption(beet.u64)],
  ],
  'UpdateOrderParams',
);
