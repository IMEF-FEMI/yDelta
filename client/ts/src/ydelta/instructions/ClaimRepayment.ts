import * as beet from '@metaplex-foundation/beet';
import {
  AccountMeta,
  PublicKey,
  SystemProgram,
  TransactionInstruction,
} from '@solana/web3.js';
import BN from 'bn.js';
import { YDELTA_PROGRAM_ID } from '../../utils/programId';
import { YdeltaInstructionTag } from '../../utils/discriminator';
import { globalConfigPda, loanPda, userAccountPda } from '../../utils/pdas';
import { ClaimRepaymentParams } from '../types/ClaimRepaymentParams';

export const claimRepaymentInstructionDiscriminator = YdeltaInstructionTag.ClaimRepayment;

export type ClaimRepaymentInstructionAccounts = {
  lender: PublicKey;
  market: PublicKey;
  sequence: BN | number | bigint;
  debtBank: PublicKey;
  marginfiProgram: PublicKey;
  crankerRefund?: PublicKey;
};

export type ClaimRepaymentInstructionArgs = ClaimRepaymentParams;

const ClaimRepaymentStruct = new beet.FixableBeetArgsStruct<
  ClaimRepaymentInstructionArgs & { instructionDiscriminator: number }
>(
  [
    ['instructionDiscriminator', beet.u8],
    ['lenderSeatIndexHint', beet.coption(beet.u32)],
  ],
  'ClaimRepaymentInstructionArgs',
);

export function createClaimRepaymentInstruction(
  accounts: ClaimRepaymentInstructionAccounts,
  args: ClaimRepaymentInstructionArgs,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = ClaimRepaymentStruct.serialize({
    instructionDiscriminator: claimRepaymentInstructionDiscriminator,
    ...args,
  });
  const seq =
    typeof accounts.sequence === 'bigint'
      ? accounts.sequence
      : BigInt(accounts.sequence.toString());
  const [loan] = loanPda(accounts.market, seq, programId);
  const [userAccount] = userAccountPda(accounts.lender, programId);

  const keys: AccountMeta[] = [
    { pubkey: accounts.lender, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: accounts.market, isWritable: true, isSigner: false },
    { pubkey: loan, isWritable: true, isSigner: false },
    { pubkey: accounts.debtBank, isWritable: false, isSigner: false },
    { pubkey: accounts.marginfiProgram, isWritable: false, isSigner: false },
    { pubkey: userAccount, isWritable: true, isSigner: false },
    { pubkey: SystemProgram.programId, isWritable: false, isSigner: false },
  ];
  if (accounts.crankerRefund) {
    keys.push({ pubkey: accounts.crankerRefund, isWritable: true, isSigner: false });
  }
  return new TransactionInstruction({ programId, keys, data });
}
