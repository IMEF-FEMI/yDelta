import * as beet from '@metaplex-foundation/beet';
import BN from 'bn.js';
import { u128 } from '../../utils/beet';

export type GlobalVaultWithdrawParams = {
  sharesToBurn: BN;
  profileId: number;
};

export const globalVaultWithdrawParamsBeet = new beet.BeetArgsStruct<GlobalVaultWithdrawParams>(
  [
    ['sharesToBurn', u128],
    ['profileId', beet.u8],
  ],
  'GlobalVaultWithdrawParams',
);
