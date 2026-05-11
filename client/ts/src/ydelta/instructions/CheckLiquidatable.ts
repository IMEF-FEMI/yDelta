import {
  AccountMeta,
  PublicKey,
  TransactionInstruction,
} from '@solana/web3.js';
import BN from 'bn.js';
import { YDELTA_PROGRAM_ID } from '../../utils/programId';
import { YdeltaInstructionTag } from '../../utils/discriminator';
import {
  borrowerMarginfiAccountPda,
  globalConfigPda,
  loanPda,
} from '../../utils/pdas';

// ─── CheckLtvLiquidatable (tag 40) ───

export const checkLtvLiquidatableInstructionDiscriminator = YdeltaInstructionTag.CheckLtvLiquidatable;

export type CheckLtvLiquidatableInstructionAccounts = {
  payer: PublicKey;
  market: PublicKey;
  sequence: BN | number | bigint;
  debtBank: PublicKey;
  collateralBank: PublicKey;
  debtOracles: PublicKey[];
  collateralOracles: PublicKey[];
  marginfiProgram: PublicKey;
};

export function createCheckLtvLiquidatableInstruction(
  accounts: CheckLtvLiquidatableInstructionAccounts,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const seq =
    typeof accounts.sequence === 'bigint'
      ? accounts.sequence
      : BigInt(accounts.sequence.toString());
  const [loan] = loanPda(accounts.market, seq, programId);
  const [borrowerMarginfi] = borrowerMarginfiAccountPda(accounts.market, programId);

  const keys: AccountMeta[] = [
    { pubkey: accounts.payer, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: accounts.market, isWritable: true, isSigner: false },
    { pubkey: loan, isWritable: true, isSigner: false },
    { pubkey: borrowerMarginfi, isWritable: true, isSigner: false },
    { pubkey: accounts.debtBank, isWritable: false, isSigner: false },
  ];
  for (const o of accounts.debtOracles) {
    keys.push({ pubkey: o, isWritable: false, isSigner: false });
  }
  keys.push({ pubkey: accounts.collateralBank, isWritable: false, isSigner: false });
  for (const o of accounts.collateralOracles) {
    keys.push({ pubkey: o, isWritable: false, isSigner: false });
  }
  keys.push({ pubkey: accounts.marginfiProgram, isWritable: false, isSigner: false });

  return new TransactionInstruction({
    programId,
    keys,
    data: Buffer.from([checkLtvLiquidatableInstructionDiscriminator]),
  });
}

// ─── CheckMaturityLiquidatable (tag 41) ───

export const checkMaturityLiquidatableInstructionDiscriminator = YdeltaInstructionTag.CheckMaturityLiquidatable;

export type CheckMaturityLiquidatableInstructionAccounts = {
  payer: PublicKey;
  market: PublicKey;
  sequence: BN | number | bigint;
  debtBank: PublicKey;
  marginfiProgram: PublicKey;
};

export function createCheckMaturityLiquidatableInstruction(
  accounts: CheckMaturityLiquidatableInstructionAccounts,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const seq =
    typeof accounts.sequence === 'bigint'
      ? accounts.sequence
      : BigInt(accounts.sequence.toString());
  const [loan] = loanPda(accounts.market, seq, programId);
  const [borrowerMarginfi] = borrowerMarginfiAccountPda(accounts.market, programId);

  const keys: AccountMeta[] = [
    { pubkey: accounts.payer, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: accounts.market, isWritable: true, isSigner: false },
    { pubkey: loan, isWritable: true, isSigner: false },
    { pubkey: borrowerMarginfi, isWritable: true, isSigner: false },
    { pubkey: accounts.debtBank, isWritable: false, isSigner: false },
    { pubkey: accounts.marginfiProgram, isWritable: false, isSigner: false },
  ];
  return new TransactionInstruction({
    programId,
    keys,
    data: Buffer.from([checkMaturityLiquidatableInstructionDiscriminator]),
  });
}
