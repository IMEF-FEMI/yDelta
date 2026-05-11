import * as beet from '@metaplex-foundation/beet';
import BN from 'bn.js';

export type ProcessMatchedLoanParams = {
  matchedLoanSequence: BN;
  matchedLoanIndexHint: beet.COption<number>;
};

export const processMatchedLoanParamsBeet = new beet.FixableBeetArgsStruct<ProcessMatchedLoanParams>(
  [
    ['matchedLoanSequence', beet.u64],
    ['matchedLoanIndexHint', beet.coption(beet.u32)],
  ],
  'ProcessMatchedLoanParams',
);
