import * as beet from '@metaplex-foundation/beet';

/** Consumer-facing FeeConfig. Padding bytes are stripped — they exist on the
 *  on-chain struct purely for `#[repr(C)]` alignment and aren't useful data. */
export type FeeConfig = {
  protocolFeeBpsFloor: number;
  originationBps: number;
  curatorSplitBps: number;
  curatorFeeBps: number;
  liquidationKeeperBps: number;
  liquidationProtocolBps: number;
  ltvBufferBps: number;
  gracePeriodSeconds: number;
};

/** Wire-level shape including the two padding regions. Internal — use when
 *  encoding/decoding raw bytes for the on-chain struct. */
type FeeConfigWire = FeeConfig & {
  paddingForU32: number[];
  paddingTail: number[];
};

export const feeConfigBeet = new beet.BeetArgsStruct<FeeConfigWire>(
  [
    ['protocolFeeBpsFloor', beet.u16],
    ['originationBps', beet.u16],
    ['curatorSplitBps', beet.u16],
    ['curatorFeeBps', beet.u16],
    ['liquidationKeeperBps', beet.u16],
    ['liquidationProtocolBps', beet.u16],
    ['ltvBufferBps', beet.u16],
    ['paddingForU32', beet.uniformFixedSizeArray(beet.u8, 2)],
    ['gracePeriodSeconds', beet.u32],
    ['paddingTail', beet.uniformFixedSizeArray(beet.u8, 4)],
  ],
  'FeeConfig',
);
