/**
 * Tags 34 + 35 — read-only liquidatability gates designed for
 * `simulateTransaction` callers. A successful sim guarantees the real
 * `liquidate_loan` / `settle_matured_loan` will succeed (modulo CPI side
 * effects). Errors carry the reason — `LoanStillSolvent`, `LoanNotMatured`,
 * `OracleStale`, `InvalidArgument` (already-settled), etc.
 */
import { PublicKey, TransactionInstruction } from '@solana/web3.js';
import BN from 'bn.js';

import {
  borrowerIntegrationAccountPda,
  globalConfigPda,
  loanPda,
} from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

export interface CheckLtvLiquidatableArgs {
  payer: PublicKey;
  market: PublicKey;
  sequence: bigint | BN | number;
  debtBank: PublicKey;
  collateralBank: PublicKey;
  debtOracles: PublicKey[];
  collateralOracles: PublicKey[];
  marginfiProgram: PublicKey;
}

export function checkLtvLiquidatableInstruction(
  args: CheckLtvLiquidatableArgs,
): TransactionInstruction {
  const loan = loanPda(args.market, BigInt(args.sequence.toString()))[0];
  const borrowerMa = borrowerIntegrationAccountPda(args.market)[0];
  const data = new Writer().u8(InstructionTag.CheckLtvLiquidatable).toBuffer();
  return ydeltaIx(
    [
      signerRw(args.payer),
      ro(globalConfigPda()[0]),
      rw(args.market),
      rw(loan),
      rw(borrowerMa),
      ro(args.debtBank),
      ...args.debtOracles.map(ro),
      ro(args.collateralBank),
      ...args.collateralOracles.map(ro),
      ro(args.marginfiProgram),
    ],
    data,
  );
}

export interface CheckMaturityLiquidatableArgs {
  payer: PublicKey;
  market: PublicKey;
  sequence: bigint | BN | number;
  debtBank: PublicKey;
  marginfiProgram: PublicKey;
}

export function checkMaturityLiquidatableInstruction(
  args: CheckMaturityLiquidatableArgs,
): TransactionInstruction {
  const loan = loanPda(args.market, BigInt(args.sequence.toString()))[0];
  const borrowerMa = borrowerIntegrationAccountPda(args.market)[0];
  const data = new Writer().u8(InstructionTag.CheckMaturityLiquidatable).toBuffer();
  return ydeltaIx(
    [
      signerRw(args.payer),
      ro(globalConfigPda()[0]),
      rw(args.market),
      rw(loan),
      rw(borrowerMa),
      ro(args.debtBank),
      ro(args.marginfiProgram),
    ],
    data,
  );
}
