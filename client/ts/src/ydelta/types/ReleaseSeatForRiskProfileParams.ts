import * as beet from '@metaplex-foundation/beet';

export type ReleaseSeatForRiskProfileParams = {
  profileId: number;
};

export const releaseSeatForRiskProfileParamsBeet = new beet.BeetArgsStruct<ReleaseSeatForRiskProfileParams>(
  [['profileId', beet.u8]],
  'ReleaseSeatForRiskProfileParams',
);
