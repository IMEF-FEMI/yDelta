import * as beet from '@metaplex-foundation/beet';
import BN from 'bn.js';

export type DepositParams = {
  amountAtoms: BN;
  traderIndexHint: beet.COption<number>;
};

export const depositParamsBeet = new beet.FixableBeetArgsStruct<DepositParams>(
  [
    ['amountAtoms', beet.u64],
    ['traderIndexHint', beet.coption(beet.u32)],
  ],
  'DepositParams',
);
