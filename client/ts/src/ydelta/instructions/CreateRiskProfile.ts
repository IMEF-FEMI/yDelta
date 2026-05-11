import * as beet from '@metaplex-foundation/beet';
import { publicKey as beetPublicKey } from '@metaplex-foundation/beet-solana';
import {
  AccountMeta,
  PublicKey,
  SystemProgram,
  TransactionInstruction,
} from '@solana/web3.js';
import { YDELTA_PROGRAM_ID } from '../../utils/programId';
import { YdeltaInstructionTag } from '../../utils/discriminator';
import { globalConfigPda, globalVaultPda } from '../../utils/pdas';
import { CreateRiskProfileParams } from '../types/CreateRiskProfileParams';

export const createRiskProfileInstructionDiscriminator = YdeltaInstructionTag.CreateRiskProfile;

export type CreateRiskProfileInstructionAccounts = {
  payer: PublicKey;
  mint: PublicKey;
};

export type CreateRiskProfileInstructionArgs = CreateRiskProfileParams;

const CreateRiskProfileStruct = new beet.BeetArgsStruct<
  CreateRiskProfileInstructionArgs & { instructionDiscriminator: number }
>(
  [
    ['instructionDiscriminator', beet.u8],
    ['profileId', beet.u8],
    ['curator', beetPublicKey],
    ['maxLtvBps', beet.u16],
    ['maxTermSeconds', beet.u32],
    ['allowedMarketMax', beet.u8],
  ],
  'CreateRiskProfileInstructionArgs',
);

export function createCreateRiskProfileInstruction(
  accounts: CreateRiskProfileInstructionAccounts,
  args: CreateRiskProfileInstructionArgs,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = CreateRiskProfileStruct.serialize({
    instructionDiscriminator: createRiskProfileInstructionDiscriminator,
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
