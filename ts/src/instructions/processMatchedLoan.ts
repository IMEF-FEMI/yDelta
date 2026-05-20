import { PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';
import BN from 'bn.js';

import { globalConfigPda, loanPda } from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Extra accounts required when the matched loan's lender is a vault risk
 * profile (i.e. the matched loan's `flags & VAULT_LENDER` is set).
 * The cranker uses these to drain principal from the vault → market
 * during the primary-promote.
 */
export interface VaultSettleAccounts {
  globalVault: PublicKey;
  globalVaultSigner: PublicKey;
  globalVaultStaging: PublicKey;
  globalVaultIntegrationAccount: PublicKey;
  marketDebtVault: PublicKey;
  marketLenderIntegrationAccount: PublicKey;
  marketSigner: PublicKey;
  debtLiquidityVault: PublicKey;
  debtBankLiquidityVaultAuthority: PublicKey;
  debtOracles: PublicKey[];
  debtMint: PublicKey;
  tokenProgram: PublicKey;
  marginfiGroup: PublicKey;
  marginfiProgram: PublicKey;
}

/**
 * Tag 5 — `ProcessMatchedLoan`. Permissionless. The cranker pays `Loan` PDA
 * rent and is reimbursed at loan close. Heavy if `vaultSettle` is set
 * (vault → market integration drain via marginfi CPI) — wrap in
 * `withCuBudget(...)`.
 */
export interface ProcessMatchedLoanArgs {
  payer: PublicKey;
  market: PublicKey;
  debtBank: PublicKey;
  marginfiProgram: PublicKey;
  sequence: bigint | BN | number;
  matchedLoanIndexHint?: number | null;
  /** Omit when the matched loan's lender is a user (no vault settle needed). */
  vaultSettle?: VaultSettleAccounts;
}

export function processMatchedLoanInstruction(
  args: ProcessMatchedLoanArgs,
): TransactionInstruction {
  const loan = loanPda(args.market, BigInt(args.sequence.toString()))[0];
  const data = new Writer()
    .u8(InstructionTag.ProcessMatchedLoan)
    .u64(args.sequence)
    .optionDataIndex(args.matchedLoanIndexHint ?? null)
    .toBuffer();

  const keys = [
    signerRw(args.payer),
    ro(globalConfigPda()[0]),
    rw(args.market),
    rw(loan),
    rw(args.debtBank),
    ro(args.marginfiProgram),
    ro(SystemProgram.programId),
  ];

  if (args.vaultSettle) {
    const v = args.vaultSettle;
    keys.push(
      rw(v.globalVault),
      ro(v.globalVaultSigner),
      rw(v.globalVaultStaging),
      rw(v.globalVaultIntegrationAccount),
      rw(v.marketDebtVault),
      rw(v.marketLenderIntegrationAccount),
      ro(v.marketSigner),
      rw(v.debtLiquidityVault),
      ro(v.debtBankLiquidityVaultAuthority),
      ...v.debtOracles.map(ro),
      ro(v.debtMint),
      ro(v.tokenProgram),
      ro(v.marginfiGroup),
      ro(v.marginfiProgram),
    );
  }

  return ydeltaIx(keys, data);
}
