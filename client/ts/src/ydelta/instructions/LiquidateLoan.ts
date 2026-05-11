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
  lenderMarginfiAccountPda,
  loanPda,
  marketSignerPda,
  marketVaultPda,
} from '../../utils/pdas';

export const liquidateLoanInstructionDiscriminator = YdeltaInstructionTag.LiquidateLoan;

export type LiquidateLoanInstructionAccounts = {
  payer: PublicKey;
  market: PublicKey;
  sequence: BN | number | bigint;
  debtMint: PublicKey;
  collateralMint: PublicKey;
  liquidatorDebtToken: PublicKey;
  liquidatorCollateralToken: PublicKey;
  debtBank: PublicKey;
  collateralBank: PublicKey;
  debtLiquidityVault: PublicKey;
  collateralLiquidityVault: PublicKey;
  collateralBankLva: PublicKey;
  debtOracles: PublicKey[];
  collateralOracles: PublicKey[];
  tokenProgram: PublicKey;
  marginfiGroup: PublicKey;
  marginfiProgram: PublicKey;
};

export type LiquidateLoanInstructionArgs = {
  /** 0 (or absent) → repay full outstanding. */
  repayAtomsMax: BN | bigint | number;
};

export function createLiquidateLoanInstruction(
  accounts: LiquidateLoanInstructionAccounts,
  args: LiquidateLoanInstructionArgs,
  programId: PublicKey = YDELTA_PROGRAM_ID,
): TransactionInstruction {
  const repayMax = BigInt(args.repayAtomsMax.toString());
  const data = Buffer.alloc(9);
  data.writeUInt8(liquidateLoanInstructionDiscriminator, 0);
  data.writeBigUInt64LE(repayMax, 1);

  const seq =
    typeof accounts.sequence === 'bigint'
      ? accounts.sequence
      : BigInt(accounts.sequence.toString());
  const [loan] = loanPda(accounts.market, seq, programId);
  const [marketDebtVault] = marketVaultPda(accounts.market, accounts.debtMint, programId);
  const [marketCollateralVault] = marketVaultPda(accounts.market, accounts.collateralMint, programId);
  const [marketSigner] = marketSignerPda(accounts.market, programId);
  const [lenderMarginfi] = lenderMarginfiAccountPda(accounts.market, programId);
  const [borrowerMarginfi] = borrowerMarginfiAccountPda(accounts.market, programId);

  const keys: AccountMeta[] = [
    { pubkey: accounts.payer, isWritable: true, isSigner: true },
    { pubkey: globalConfigPda(programId)[0], isWritable: false, isSigner: false },
    { pubkey: accounts.market, isWritable: true, isSigner: false },
    { pubkey: loan, isWritable: true, isSigner: false },
    { pubkey: accounts.liquidatorDebtToken, isWritable: true, isSigner: false },
    { pubkey: accounts.liquidatorCollateralToken, isWritable: true, isSigner: false },
    { pubkey: marketDebtVault, isWritable: true, isSigner: false },
    { pubkey: marketCollateralVault, isWritable: true, isSigner: false },
    { pubkey: marketSigner, isWritable: false, isSigner: false },
    { pubkey: lenderMarginfi, isWritable: true, isSigner: false },
    { pubkey: borrowerMarginfi, isWritable: true, isSigner: false },
    { pubkey: accounts.debtBank, isWritable: true, isSigner: false },
    { pubkey: accounts.collateralBank, isWritable: true, isSigner: false },
    { pubkey: accounts.debtLiquidityVault, isWritable: true, isSigner: false },
    { pubkey: accounts.collateralLiquidityVault, isWritable: true, isSigner: false },
    { pubkey: accounts.collateralBankLva, isWritable: false, isSigner: false },
  ];
  for (const o of accounts.debtOracles) {
    keys.push({ pubkey: o, isWritable: false, isSigner: false });
  }
  for (const o of accounts.collateralOracles) {
    keys.push({ pubkey: o, isWritable: false, isSigner: false });
  }
  keys.push(
    { pubkey: accounts.debtMint, isWritable: false, isSigner: false },
    { pubkey: accounts.collateralMint, isWritable: false, isSigner: false },
    { pubkey: accounts.tokenProgram, isWritable: false, isSigner: false },
    { pubkey: accounts.marginfiGroup, isWritable: false, isSigner: false },
    { pubkey: accounts.marginfiProgram, isWritable: false, isSigner: false },
  );
  return new TransactionInstruction({ programId, keys, data });
}
