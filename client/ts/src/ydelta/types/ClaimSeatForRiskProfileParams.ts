import * as beet from '@metaplex-foundation/beet';
import BN from 'bn.js';

export type ClaimSeatForRiskProfileParams = {
  profileId: number;
  maxExposureAtoms: BN;
};

export const claimSeatForRiskProfileParamsBeet = new beet.BeetArgsStruct<ClaimSeatForRiskProfileParams>(
  [
    ['profileId', beet.u8],
    ['maxExposureAtoms', beet.u64],
  ],
  'ClaimSeatForRiskProfileParams',
);
