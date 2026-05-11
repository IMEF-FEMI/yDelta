import * as beet from '@metaplex-foundation/beet';

export type UpdateOrderForRiskProfileParams = {
  profileId: number;
  newRateBps: number;
  newTermSeconds: number;
  newFlags: number;
};

export const updateOrderForRiskProfileParamsBeet = new beet.BeetArgsStruct<UpdateOrderForRiskProfileParams>(
  [
    ['profileId', beet.u8],
    ['newRateBps', beet.u16],
    ['newTermSeconds', beet.u32],
    ['newFlags', beet.u8],
  ],
  'UpdateOrderForRiskProfileParams',
);
