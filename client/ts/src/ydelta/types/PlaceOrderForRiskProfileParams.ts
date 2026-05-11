import * as beet from '@metaplex-foundation/beet';

export type PlaceOrderForRiskProfileParams = {
  profileId: number;
  rateBps: number;
  termSeconds: number;
  flags: number;
};

export const placeOrderForRiskProfileParamsBeet = new beet.BeetArgsStruct<PlaceOrderForRiskProfileParams>(
  [
    ['profileId', beet.u8],
    ['rateBps', beet.u16],
    ['termSeconds', beet.u32],
    ['flags', beet.u8],
  ],
  'PlaceOrderForRiskProfileParams',
);
