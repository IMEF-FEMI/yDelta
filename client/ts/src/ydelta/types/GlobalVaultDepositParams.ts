import * as beet from '@metaplex-foundation/beet';
import BN from 'bn.js';

export type GlobalVaultDepositParams = {
  amountAtoms: BN;
  profileId: number;
};

export const globalVaultDepositParamsBeet = new beet.BeetArgsStruct<GlobalVaultDepositParams>(
  [
    ['amountAtoms', beet.u64],
    ['profileId', beet.u8],
  ],
  'GlobalVaultDepositParams',
);
