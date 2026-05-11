import * as beet from '@metaplex-foundation/beet';
import { publicKey as beetPublicKey } from '@metaplex-foundation/beet-solana';
import { PublicKey } from '@solana/web3.js';

export type TransferMarketAdminParams = { newAdmin: PublicKey };
export type TransferGlobalVaultAdminParams = { newAdmin: PublicKey };
export type TransferProtocolAdminParams = { newAdmin: PublicKey };

export type TransferCuratorParams = {
  profileId: number;
  newCurator: PublicKey;
};

export type AcceptCuratorParams = {
  profileId: number;
};

export type SetGlobalPauseParams = {
  paused: number;
};

export const transferMarketAdminParamsBeet = new beet.BeetArgsStruct<TransferMarketAdminParams>(
  [['newAdmin', beetPublicKey]],
  'TransferMarketAdminParams',
);

export const transferGlobalVaultAdminParamsBeet = new beet.BeetArgsStruct<TransferGlobalVaultAdminParams>(
  [['newAdmin', beetPublicKey]],
  'TransferGlobalVaultAdminParams',
);

export const transferProtocolAdminParamsBeet = new beet.BeetArgsStruct<TransferProtocolAdminParams>(
  [['newAdmin', beetPublicKey]],
  'TransferProtocolAdminParams',
);

export const transferCuratorParamsBeet = new beet.BeetArgsStruct<TransferCuratorParams>(
  [
    ['profileId', beet.u8],
    ['newCurator', beetPublicKey],
  ],
  'TransferCuratorParams',
);

export const acceptCuratorParamsBeet = new beet.BeetArgsStruct<AcceptCuratorParams>(
  [['profileId', beet.u8]],
  'AcceptCuratorParams',
);

export const setGlobalPauseParamsBeet = new beet.BeetArgsStruct<SetGlobalPauseParams>(
  [['paused', beet.u8]],
  'SetGlobalPauseParams',
);
