import * as beet from '@metaplex-foundation/beet';

export type SetMarketPauseParams = {
  paused: number;
};

export const setMarketPauseParamsBeet = new beet.BeetArgsStruct<SetMarketPauseParams>(
  [['paused', beet.u8]],
  'SetMarketPauseParams',
);
