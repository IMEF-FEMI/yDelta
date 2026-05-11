import * as beet from '@metaplex-foundation/beet';
import {
  AccountMeta,
  PublicKey,
  SystemProgram,
  TransactionInstruction,
} from '@solana/web3.js';
import { YDELTA_PROGRAM_ID } from '../../utils/programId';
import { YdeltaInstructionTag } from '../../utils/discriminator';
import { globalConfigPda, userAccountPda } from '../../utils/pdas';
import { CancelOrderParams } from '../types/CancelOrderParams';

export const cancelOrderInstructionDiscriminator = YdeltaInstructionTag.CancelOrder;

export type CancelOrderInstructionAccounts = {
  payer: PublicKey;
  market: PublicKey;
  secondaryLoan?: PublicKey;
};

export type CancelOrderInstructionArgs = CancelOrderParams;

const CancelOrderStruct = new beet.FixableBeetArgsStruct<
  CancelOrderInstructionArgs & { instructionDiscriminator: number }
>(
  [
    ['instructionDiscriminator', beet.u8],
    ['orderSequenceNumber', beet.u64],
    ['orderIndexHint', beet.coption(beet.u32)],
    ['seatIndexHint', beet.coption(beet.u32)],
  ],
  'CancelOrderInstructionArgs',
);

export function createCancelOrderInstruction(
  accounts: CancelOrderInstructionAccounts,
  args: CancelOrderInstructionArgs,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = CancelOrderStruct.serialize({
    instructionDiscriminator: cancelOrderInstructionDiscriminator,
    ...args,
  });
  const [userAccount] = userAccountPda(accounts.payer, programId);

  const keys: AccountMeta[] = [
    { pubkey: accounts.payer, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: accounts.market, isWritable: true, isSigner: false },
    { pubkey: SystemProgram.programId, isWritable: false, isSigner: false },
    { pubkey: userAccount, isWritable: true, isSigner: false },
    { pubkey: SystemProgram.programId, isWritable: false, isSigner: false },
  ];
  if (accounts.secondaryLoan) {
    keys.push({ pubkey: accounts.secondaryLoan, isWritable: true, isSigner: false });
  }
  return new TransactionInstruction({ programId, keys, data });
}
