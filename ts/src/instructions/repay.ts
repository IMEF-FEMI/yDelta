import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';
import BN from 'bn.js';

import {
  borrowerIntegrationAccountPda,
  globalConfigPda,
  lenderIntegrationAccountPda,
  loanPda,
  marketSignerPda,
  marketTokenVaultPda,
  userAccountPda,
} from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 6 — `Repay`. Borrower-initiated. For a P2Pool loan, `fullRepay=true`
 * re-reads the live marginfi liability and retires it to zero; for a Fixed
 * loan the flag is ignored. On full repay the collateral returns to the
 * borrower's seat and the Loan PDA closes (rent → `crankerRefund`).
 */
export interface RepayArgs {
  borrower: PublicKey;
  market: PublicKey;
  sequence: bigint | BN | number;
  debtMint: PublicKey;
  borrowerToken: PublicKey;
  tokenProgram: PublicKey;
  marginfiGroup: PublicKey;
  debtBank: PublicKey;
  debtLiquidityVault: PublicKey;
  collateralBank: PublicKey;
  marginfiProgram: PublicKey;
  repayAtoms: bigint | BN | number;
  fullRepay?: boolean;
  borrowerSeatIndexHint?: number | null;
  /**
   * Account that gets the loan PDA's rent on full close. Bound at loan-create
   * time to whoever paid rent in `process_matched_loan`; passing the wrong
   * key will revert.
   */
  crankerRefund: PublicKey;
  /**
   * Lender vault PDA. **Required** for Fixed loans — the processor reads
   * it on full repay to apply per-loan risk-profile decrements and bump
   * `pending_claim_atoms`. Omit / pass `null` for P2Pool loans (the
   * on-chain loader expects the slot absent in that case).
   */
  globalVault?: PublicKey | null;
}

export function repayInstruction(args: RepayArgs): TransactionInstruction {
  const loan = loanPda(args.market, BigInt(args.sequence.toString()))[0];
  const lenderMa = lenderIntegrationAccountPda(args.market)[0];
  const borrowerMa = borrowerIntegrationAccountPda(args.market)[0];
  const marketSigner = marketSignerPda(args.market)[0];
  const vault = marketTokenVaultPda(args.market, args.debtMint)[0];

  const data = new Writer()
    .u8(InstructionTag.Repay)
    .u64(args.repayAtoms)
    .bool(args.fullRepay ?? false)
    .optionDataIndex(args.borrowerSeatIndexHint ?? null)
    .toBuffer();

  const accounts = [
    signerRw(args.borrower),
    ro(globalConfigPda()[0]),
    rw(args.market),
    rw(loan),
    rw(args.borrowerToken),
    rw(vault),
    ro(args.tokenProgram),
    ro(args.debtMint),
    ro(args.marginfiGroup),
    rw(lenderMa),
    rw(args.debtBank),
    rw(args.debtLiquidityVault),
    ro(args.collateralBank),
    rw(borrowerMa),
    ro(marketSigner),
    ro(args.marginfiProgram),
    rw(userAccountPda(args.borrower)[0]),
    ro(SystemProgram.programId),
    rw(args.crankerRefund),
  ];
  if (args.globalVault) {
    accounts.push(rw(args.globalVault));
  }
  return ydeltaIx(accounts, data);
}
