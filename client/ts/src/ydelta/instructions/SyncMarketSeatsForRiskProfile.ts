import * as beet from '@metaplex-foundation/beet';
import {
  AccountMeta,
  PublicKey,
  TransactionInstruction,
} from '@solana/web3.js';
import { YDELTA_PROGRAM_ID } from '../../utils/programId';
import { YdeltaInstructionTag } from '../../utils/discriminator';
import { globalConfigPda, globalVaultPda } from '../../utils/pdas';

export const syncMarketSeatsForRiskProfileInstructionDiscriminator = YdeltaInstructionTag.SyncMarketSeatsForRiskProfile;

const Struct = new beet.BeetArgsStruct<{
  instructionDiscriminator: number;
  profileId: number;
}>(
  [
    ['instructionDiscriminator', beet.u8],
    ['profileId', beet.u8],
  ],
  'SyncMarketSeatsForRiskProfileInstructionArgs',
);

export type SyncMarketSeatsForRiskProfileInstructionAccounts = {
  payer: PublicKey;
  debtMint: PublicKey;
  /** Up to 8 markets that have a vault-side seat for this profile_id. */
  markets: PublicKey[];
};

export function createSyncMarketSeatsForRiskProfileInstruction(
  accounts: SyncMarketSeatsForRiskProfileInstructionAccounts,
  args: { profileId: number },
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = Struct.serialize({
    instructionDiscriminator: syncMarketSeatsForRiskProfileInstructionDiscriminator,
    ...args,
  });
  const [globalVault] = globalVaultPda(accounts.debtMint, programId);

  const keys: AccountMeta[] = [
    { pubkey: accounts.payer, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: globalVault, isWritable: false, isSigner: false },
  ];
  for (const m of accounts.markets) {
    keys.push({ pubkey: m, isWritable: true, isSigner: false });
  }
  return new TransactionInstruction({ programId, keys, data });
}
