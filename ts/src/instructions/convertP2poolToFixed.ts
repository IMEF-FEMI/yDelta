import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';
import BN from 'bn.js';

import {
  borrowerIntegrationAccountPda,
  globalConfigPda,
  globalVaultIntegrationAccountPda,
  globalVaultPda,
  globalVaultSignerPda,
  loanPda,
  marketSignerPda,
  marketTokenVaultPda,
} from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 33 — `ConvertP2PoolToFixed`. Borrower-initiated. Walks the asks tree
 * and crosses every compatible vault risk-profile ask whose `rate_bps ≤
 * maxAcceptableRateBps` AND `term_seconds ≥ remaining_term`. Each cross
 * emits a fresh Fixed `MatchedLoan` queue node; unfilled residual stays
 * on the P2Pool loan body. The P2Pool PDA closes only when the live
 * marginfi liability is zero (rent → `crankerRefund`).
 */
export interface ConvertP2PoolToFixedArgs {
  borrower: PublicKey;
  market: PublicKey;
  loanSequence: bigint | BN | number;
  debtMint: PublicKey;
  debtBank: PublicKey;
  debtLiquidityVault: PublicKey;
  debtBankLiquidityVaultAuthority: PublicKey;
  debtOracles: PublicKey[];
  collateralBank: PublicKey;
  collateralOracles: PublicKey[];
  tokenProgram: PublicKey;
  marginfiGroup: PublicKey;
  marginfiProgram: PublicKey;
  maxAcceptableRateBps: number;
  /** REQUIRED — bound to the P2Pool PDA's `created_by` at allocate time. */
  crankerRefund: PublicKey;
}

export function convertP2poolToFixedInstruction(
  args: ConvertP2PoolToFixedArgs,
): TransactionInstruction {
  const loan = loanPda(args.market, BigInt(args.loanSequence.toString()))[0];
  const borrowerMa = borrowerIntegrationAccountPda(args.market)[0];
  const marketSigner = marketSignerPda(args.market)[0];
  const marketDebtVault = marketTokenVaultPda(args.market, args.debtMint)[0];
  const vault = globalVaultPda(args.debtMint)[0];
  const vaultSigner = globalVaultSignerPda(vault)[0];
  const vaultIntegrationAccount = globalVaultIntegrationAccountPda(vault)[0];

  const data = new Writer()
    .u8(InstructionTag.ConvertP2PoolToFixed)
    .u16(args.maxAcceptableRateBps)
    .toBuffer();

  // NOTE: lender_marginfi_account is NOT in this account list. The
  // redeployed convert ix funds matched fills from each vault's own
  // global_vault_integration_account, not from the per-market lender
  // marginfi account — that slot would shift `debt_bank` by one and
  // the loader's `debt_bank.key == market.debt_lending_pool` check
  // would fail with Custom(10).
  return ydeltaIx(
    [
      signerRw(args.borrower),
      ro(globalConfigPda()[0]),
      rw(args.market),
      rw(loan),
      ro(SystemProgram.programId),
      rw(borrowerMa),
      rw(args.debtBank),
      rw(args.debtLiquidityVault),
      ro(args.debtBankLiquidityVaultAuthority),
      ...args.debtOracles.map(ro),
      ro(args.collateralBank),
      ...args.collateralOracles.map(ro),
      rw(marketDebtVault),
      ro(args.debtMint),
      ro(marketSigner),
      ro(args.tokenProgram),
      ro(args.marginfiGroup),
      ro(args.marginfiProgram),
      rw(vault),
      ro(vaultSigner),
      rw(vaultIntegrationAccount),
      rw(args.crankerRefund),
    ],
    data,
  );
}
