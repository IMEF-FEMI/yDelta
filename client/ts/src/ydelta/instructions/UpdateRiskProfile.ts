import * as beet from '@metaplex-foundation/beet';
import {
  AccountMeta,
  PublicKey,
  SystemProgram,
  TransactionInstruction,
} from '@solana/web3.js';
import { YDELTA_PROGRAM_ID } from '../../utils/programId';
import { YdeltaInstructionTag } from '../../utils/discriminator';
import { globalConfigPda, globalVaultPda } from '../../utils/pdas';
import { UpdateRiskProfileParams } from '../types/UpdateRiskProfileParams';

export const updateRiskProfileInstructionDiscriminator = YdeltaInstructionTag.UpdateRiskProfile;

export type UpdateRiskProfileInstructionAccounts = {
  payer: PublicKey;
  mint: PublicKey;
};

const Struct = new beet.FixableBeetArgsStruct<
  UpdateRiskProfileParams & { instructionDiscriminator: number }
>(
  [
    ['instructionDiscriminator', beet.u8],
    ['profileId', beet.u8],
    ['newMaxLtvBps', beet.coption(beet.u16)],
    ['newMaxTermSeconds', beet.coption(beet.u32)],
    ['newAllowedMarketMax', beet.coption(beet.u8)],
  ],
  'UpdateRiskProfileInstructionArgs',
);

export function createUpdateRiskProfileInstruction(
  accounts: UpdateRiskProfileInstructionAccounts,
  args: UpdateRiskProfileParams,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = Struct.serialize({
    instructionDiscriminator: updateRiskProfileInstructionDiscriminator,
    ...args,
  });
  const [vault] = globalVaultPda(accounts.mint, programId);

  const keys: AccountMeta[] = [
    { pubkey: accounts.payer, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: vault, isWritable: true, isSigner: false },
    { pubkey: SystemProgram.programId, isWritable: false, isSigner: false },
  ];
  return new TransactionInstruction({ programId, keys, data });
}
