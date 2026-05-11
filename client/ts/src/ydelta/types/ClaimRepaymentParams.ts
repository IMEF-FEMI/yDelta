import * as beet from '@metaplex-foundation/beet';

export type ClaimRepaymentParams = {
  lenderSeatIndexHint: beet.COption<number>;
};

export const claimRepaymentParamsBeet = new beet.FixableBeetArgsStruct<ClaimRepaymentParams>(
  [['lenderSeatIndexHint', beet.coption(beet.u32)]],
  'ClaimRepaymentParams',
);
