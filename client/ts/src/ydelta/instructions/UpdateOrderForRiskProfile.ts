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
import { UpdateOrderForRiskProfileParams } from '../types/UpdateOrderForRiskProfileParams';

export const updateOrderForRiskProfileInstructionDiscriminator = YdeltaInstructionTag.UpdateOrderForRiskProfile;

export type UpdateOrderForRiskProfileInstructionAccounts = {
  payer: PublicKey;
  mint: PublicKey;
  market: PublicKey;
};

export type UpdateOrderForRiskProfileInstructionArgs = UpdateOrderForRiskProfileParams;

const Struct = new beet.BeetArgsStruct<
  UpdateOrderForRiskProfileInstructionArgs & { instructionDiscriminator: number }
>(
  [
    ['instructionDiscriminator', beet.u8],
    ['profileId', beet.u8],
    ['newRateBps', beet.u16],
    ['newTermSeconds', beet.u32],
    ['newFlags', beet.u8],
  ],
  'UpdateOrderForRiskProfileInstructionArgs',
);

export function createUpdateOrderForRiskProfileInstruction(
  accounts: UpdateOrderForRiskProfileInstructionAccounts,
  args: UpdateOrderForRiskProfileInstructionArgs,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = Struct.serialize({
    instructionDiscriminator: updateOrderForRiskProfileInstructionDiscriminator,
    ...args,
  });
  const [vault] = globalVaultPda(accounts.mint, programId);

  const keys: AccountMeta[] = [
    { pubkey: accounts.payer, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: vault, isWritable: true, isSigner: false },
    { pubkey: accounts.market, isWritable: true, isSigner: false },
    { pubkey: SystemProgram.programId, isWritable: false, isSigner: false },
  ];
  return new TransactionInstruction({ programId, keys, data });
}
