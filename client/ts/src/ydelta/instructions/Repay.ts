import * as beet from '@metaplex-foundation/beet';
import {
  AccountMeta,
  PublicKey,
  SystemProgram,
  TransactionInstruction,
} from '@solana/web3.js';
import { YDELTA_PROGRAM_ID } from '../../utils/programId';
import { YdeltaInstructionTag } from '../../utils/discriminator';
import {
  globalConfigPda,
  lenderMarginfiAccountPda,
  loanPda,
  marketSignerPda,
  marketVaultPda,
  userAccountPda,
} from '../../utils/pdas';
import { RepayParams } from '../types/RepayParams';
import BN from 'bn.js';

export const repayInstructionDiscriminator = YdeltaInstructionTag.Repay;

export type RepayInstructionAccounts = {
  borrower: PublicKey;
  market: PublicKey;
  sequence: BN | number | bigint;
  debtMint: PublicKey;
  borrowerToken: PublicKey;
  tokenProgram: PublicKey;
  marginfiGroup: PublicKey;
  debtBank: PublicKey;
  debtLiquidityVault: PublicKey;
  collateralBank: PublicKey;
  marginfiProgram: PublicKey;
};

export type RepayInstructionArgs = RepayParams;

const RepayStruct = new beet.FixableBeetArgsStruct<
  RepayInstructionArgs & { instructionDiscriminator: number }
>(
  [
    ['instructionDiscriminator', beet.u8],
    ['repayAtoms', beet.u64],
    ['fullRepay', beet.bool],
    ['borrowerSeatIndexHint', beet.coption(beet.u32)],
  ],
  'RepayInstructionArgs',
);

export function createRepayInstruction(
  accounts: RepayInstructionAccounts,
  args: RepayInstructionArgs,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const [data] = RepayStruct.serialize({
    instructionDiscriminator: repayInstructionDiscriminator,
    ...args,
  });
  const seq =
    typeof accounts.sequence === 'bigint'
      ? accounts.sequence
      : BigInt(accounts.sequence.toString());
  const [marginfiAccount] = lenderMarginfiAccountPda(accounts.market, programId);
  const [marketSigner] = marketSignerPda(accounts.market, programId);
  const [loan] = loanPda(accounts.market, seq, programId);
  const [vault] = marketVaultPda(accounts.market, accounts.debtMint, programId);
  const [userAccount] = userAccountPda(accounts.borrower, programId);

  const keys: AccountMeta[] = [
    { pubkey: accounts.borrower, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: accounts.market, isWritable: true, isSigner: false },
    { pubkey: loan, isWritable: true, isSigner: false },
    { pubkey: accounts.borrowerToken, isWritable: true, isSigner: false },
    { pubkey: vault, isWritable: true, isSigner: false },
    { pubkey: accounts.tokenProgram, isWritable: false, isSigner: false },
    { pubkey: accounts.debtMint, isWritable: false, isSigner: false },
    { pubkey: accounts.marginfiGroup, isWritable: false, isSigner: false },
    { pubkey: marginfiAccount, isWritable: true, isSigner: false },
    { pubkey: accounts.debtBank, isWritable: true, isSigner: false },
    { pubkey: accounts.debtLiquidityVault, isWritable: true, isSigner: false },
    { pubkey: accounts.collateralBank, isWritable: false, isSigner: false },
    { pubkey: marketSigner, isWritable: false, isSigner: false },
    { pubkey: accounts.marginfiProgram, isWritable: false, isSigner: false },
    { pubkey: userAccount, isWritable: true, isSigner: false },
    { pubkey: SystemProgram.programId, isWritable: false, isSigner: false },
  ];
  return new TransactionInstruction({ programId, keys, data });
}
