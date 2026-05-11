import * as beet from '@metaplex-foundation/beet';
import BN from 'bn.js';

export type WithdrawParams = {
  amountAtoms: BN;
  traderIndexHint: beet.COption<number>;
};

export const withdrawParamsBeet = new beet.FixableBeetArgsStruct<WithdrawParams>(
  [
    ['amountAtoms', beet.u64],
    ['traderIndexHint', beet.coption(beet.u32)],
  ],
  'WithdrawParams',
);
