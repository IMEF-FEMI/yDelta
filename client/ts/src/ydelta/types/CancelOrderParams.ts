import * as beet from '@metaplex-foundation/beet';
import BN from 'bn.js';

export type CancelOrderParams = {
  orderSequenceNumber: BN;
  orderIndexHint: beet.COption<number>;
  seatIndexHint: beet.COption<number>;
};

export const cancelOrderParamsBeet = new beet.FixableBeetArgsStruct<CancelOrderParams>(
  [
    ['orderSequenceNumber', beet.u64],
    ['orderIndexHint', beet.coption(beet.u32)],
    ['seatIndexHint', beet.coption(beet.u32)],
  ],
  'CancelOrderParams',
);
