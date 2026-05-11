import * as beet from '@metaplex-foundation/beet';

export type ConvertP2PoolToFixedParams = {
  maxAcceptableRateBps: number;
};

export const convertP2PoolToFixedParamsBeet = new beet.BeetArgsStruct<ConvertP2PoolToFixedParams>(
  [['maxAcceptableRateBps', beet.u16]],
  'ConvertP2PoolToFixedParams',
);
