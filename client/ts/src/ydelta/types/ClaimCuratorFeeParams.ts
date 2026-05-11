import * as beet from '@metaplex-foundation/beet';

export type ClaimCuratorFeeParams = {
  profileId: number;
};

export const claimCuratorFeeParamsBeet = new beet.BeetArgsStruct<ClaimCuratorFeeParams>(
  [['profileId', beet.u8]],
  'ClaimCuratorFeeParams',
);
