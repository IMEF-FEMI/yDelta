import * as beet from '@metaplex-foundation/beet';

export type UpdateRiskProfileParams = {
  profileId: number;
  newMaxLtvBps: beet.COption<number>;
  newMaxTermSeconds: beet.COption<number>;
  newAllowedMarketMax: beet.COption<number>;
};

export const updateRiskProfileParamsBeet = new beet.FixableBeetArgsStruct<UpdateRiskProfileParams>(
  [
    ['profileId', beet.u8],
    ['newMaxLtvBps', beet.coption(beet.u16)],
    ['newMaxTermSeconds', beet.coption(beet.u32)],
    ['newAllowedMarketMax', beet.coption(beet.u8)],
  ],
  'UpdateRiskProfileParams',
);
