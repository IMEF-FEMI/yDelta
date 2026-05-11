import * as beet from '@metaplex-foundation/beet';

export type SetFeeConfigParams = {
  protocolFeeBpsFloor: beet.COption<number>;
  originationBps: beet.COption<number>;
  curatorSplitBps: beet.COption<number>;
  curatorFeeBps: beet.COption<number>;
  liquidationKeeperBps: beet.COption<number>;
  liquidationProtocolBps: beet.COption<number>;
  ltvBufferBps: beet.COption<number>;
  gracePeriodSeconds: beet.COption<number>;
};

export const setFeeConfigParamsBeet = new beet.FixableBeetArgsStruct<SetFeeConfigParams>(
  [
    ['protocolFeeBpsFloor', beet.coption(beet.u16)],
    ['originationBps', beet.coption(beet.u16)],
    ['curatorSplitBps', beet.coption(beet.u16)],
    ['curatorFeeBps', beet.coption(beet.u16)],
    ['liquidationKeeperBps', beet.coption(beet.u16)],
    ['liquidationProtocolBps', beet.coption(beet.u16)],
    ['ltvBufferBps', beet.coption(beet.u16)],
    ['gracePeriodSeconds', beet.coption(beet.u32)],
  ],
  'SetFeeConfigParams',
);
