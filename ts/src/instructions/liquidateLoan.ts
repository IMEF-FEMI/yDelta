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
 * Tag 17 — `LiquidateLoan`. Same account ordering as `SettleMaturedLoan`;
 * differs in the on-chain gate (LTV breach instead of past-grace). Data is
 * a bare LE `u64`. Heavy — wrap in `withCuBudget(...)`. Reverts with
 * `LoanStillSolvent` when current LTV is below the maintenance threshold.
 */
export interface LiquidateLoanArgs {
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
}

export function liquidateLoanInstruction(args: LiquidateLoanArgs): TransactionInstruction {
  const loan = loanPda(args.market, BigInt(args.sequence.toString()))[0];
  const marketDebtVault = marketTokenVaultPda(args.market, args.debtMint)[0];
  const marketCollateralVault = marketTokenVaultPda(args.market, args.collateralMint)[0];
  const marketSigner = marketSignerPda(args.market)[0];
  const lenderMa = lenderIntegrationAccountPda(args.market)[0];
  const borrowerMa = borrowerIntegrationAccountPda(args.market)[0];
  const data = new Writer()
    .u8(InstructionTag.LiquidateLoan)
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
    ],
    data,
  );
}
