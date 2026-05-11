import * as beet from '@metaplex-foundation/beet';

export type CancelOrderForRiskProfileParams = {
  profileId: number;
};

export const cancelOrderForRiskProfileParamsBeet = new beet.BeetArgsStruct<CancelOrderForRiskProfileParams>(
  [['profileId', beet.u8]],
  'CancelOrderForRiskProfileParams',
);
