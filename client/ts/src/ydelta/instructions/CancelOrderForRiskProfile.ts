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
import { CancelOrderForRiskProfileParams } from '../types/CancelOrderForRiskProfileParams';

export const cancelOrderForRiskProfileInstructionDiscriminator = YdeltaInstructionTag.CancelOrderForRiskProfile;

/**
 * Split-payer layout. Cancel doesn't expand a region (it returns
 * blocks to the free list), so `feePayer` only covers the tx fee;
 * `curator` signs to satisfy the on-chain `profile.curator` gate.
 */
export type CancelOrderForRiskProfileInstructionAccounts = {
  feePayer: PublicKey;
  curator: PublicKey;
  mint: PublicKey;
  market: PublicKey;
};

export type CancelOrderForRiskProfileInstructionArgs = CancelOrderForRiskProfileParams;

const Struct = new beet.BeetArgsStruct<
  CancelOrderForRiskProfileInstructionArgs & { instructionDiscriminator: number }
>(
  [
    ['instructionDiscriminator', beet.u8],
    ['profileId', beet.u8],
  ],
  'CancelOrderForRiskProfileInstructionArgs',
);

export function createCancelOrderForRiskProfileInstruction(
  accounts: CancelOrderForRiskProfileInstructionAccounts,
  args: CancelOrderForRiskProfileInstructionArgs,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = Struct.serialize({
    instructionDiscriminator: cancelOrderForRiskProfileInstructionDiscriminator,
    ...args,
  });
  const [vault] = globalVaultPda(accounts.mint, programId);

  const keys: AccountMeta[] = [
    { pubkey: accounts.feePayer, isWritable: true, isSigner: true },
    { pubkey: accounts.curator, isWritable: false, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: vault, isWritable: true, isSigner: false },
    { pubkey: accounts.market, isWritable: true, isSigner: false },
    { pubkey: SystemProgram.programId, isWritable: false, isSigner: false },
  ];
  return new TransactionInstruction({ programId, keys, data });
}
