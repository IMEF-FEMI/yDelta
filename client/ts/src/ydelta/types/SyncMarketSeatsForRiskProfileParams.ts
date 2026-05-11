import * as beet from '@metaplex-foundation/beet';

export type SyncMarketSeatsForRiskProfileParams = {
  profileId: number;
};

export const syncMarketSeatsForRiskProfileParamsBeet = new beet.BeetArgsStruct<SyncMarketSeatsForRiskProfileParams>(
  [['profileId', beet.u8]],
  'SyncMarketSeatsForRiskProfileParams',
);
