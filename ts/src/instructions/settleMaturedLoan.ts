import { PublicKey, TransactionInstruction } from '@solana/web3.js';
import BN from 'bn.js';

import {
  borrowerIntegrationAccountPda,
  globalConfigPda,
  lenderIntegrationAccountPda,
  loanPda,
  marketSignerPda,
  marketTokenVaultPda,
} from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 16 — `SettleMaturedLoan`. Heavy — wrap in `withCuBudget(...)`. The
 * data tail is a bare LE `u64` (no borsh struct framing). `repayAtomsMax = 0`
 * or `>= outstanding` means "full repay" (loan flips to `Repaid`).
 */
export interface SettleMaturedLoanArgs {
  payer: PublicKey;
  market: PublicKey;
  sequence: bigint | BN | number;
  debtMint: PublicKey;
  collateralMint: PublicKey;
  liquidatorDebtToken: PublicKey;
  liquidatorCollateralToken: PublicKey;
  debtBank: PublicKey;
  collateralBank: PublicKey;
  debtLiquidityVault: PublicKey;
  collateralLiquidityVault: PublicKey;
  collateralBankLiquidityVaultAuthority: PublicKey;
  debtOracles: PublicKey[];
  collateralOracles: PublicKey[];
  tokenProgram: PublicKey;
  marginfiGroup: PublicKey;
  marginfiProgram: PublicKey;
  repayAtomsMax: bigint | BN | number;
  crankerRefund: PublicKey;
  /**
   * Lender's global vault (`[b"vault", debtBank]`). REQUIRED for Fixed
   * loans — the full-settle lender credit lands on the vault-owned seat.
   * Omit only for P2Pool loans.
   */
  globalVault?: PublicKey;
}

export function settleMaturedLoanInstruction(
  args: SettleMaturedLoanArgs,
): TransactionInstruction {
  const loan = loanPda(args.market, BigInt(args.sequence.toString()))[0];
  const marketDebtVault = marketTokenVaultPda(args.market, args.debtMint)[0];
  const marketCollateralVault = marketTokenVaultPda(args.market, args.collateralMint)[0];
  const marketSigner = marketSignerPda(args.market)[0];
  const lenderMa = lenderIntegrationAccountPda(args.market)[0];
  const borrowerMa = borrowerIntegrationAccountPda(args.market)[0];
  const data = new Writer()
    .u8(InstructionTag.SettleMaturedLoan)
    .u64(args.repayAtomsMax)
    .toBuffer();
  return ydeltaIx(
    [
      signerRw(args.payer),
      ro(globalConfigPda()[0]),
      rw(args.market),
      rw(loan),
      rw(args.liquidatorDebtToken),
      rw(args.liquidatorCollateralToken),
      rw(marketDebtVault),
      rw(marketCollateralVault),
      ro(marketSigner),
      rw(lenderMa),
      rw(borrowerMa),
      rw(args.debtBank),
      rw(args.collateralBank),
      rw(args.debtLiquidityVault),
      rw(args.collateralLiquidityVault),
      ro(args.collateralBankLiquidityVaultAuthority),
      ...args.debtOracles.map(ro),
      ...args.collateralOracles.map(ro),
      ro(args.debtMint),
      ro(args.collateralMint),
      ro(args.tokenProgram),
      ro(args.marginfiGroup),
      ro(args.marginfiProgram),
      rw(args.crankerRefund),
      ...(args.globalVault ? [rw(args.globalVault)] : []),
    ],
    data,
  );
}
