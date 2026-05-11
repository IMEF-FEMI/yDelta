import * as beet from '@metaplex-foundation/beet';
import BN from 'bn.js';

export type RepayParams = {
  repayAtoms: BN;
  fullRepay: boolean;
  borrowerSeatIndexHint: beet.COption<number>;
};

export const repayParamsBeet = new beet.FixableBeetArgsStruct<RepayParams>(
  [
    ['repayAtoms', beet.u64],
    ['fullRepay', beet.bool],
    ['borrowerSeatIndexHint', beet.coption(beet.u32)],
  ],
  'RepayParams',
);
