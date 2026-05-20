import { PublicKey, TransactionInstruction } from '@solana/web3.js';
import BN from 'bn.js';

import {
  globalConfigPda,
  globalVaultIntegrationAccountPda,
  globalVaultSignerPda,
  globalVaultStagingPda,
  loanPda,
  marketSignerPda,
  marketTokenVaultPda,
} from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 20 — `ClaimRepaymentForRiskProfile`. Permissionless cranker. Heavy
 * (3 CPIs + oracle read) — wrap in `withCuBudget(...)`.
 *
 * Only ONE oracle (`bank.config.primary_oracle()`) is loaded by the
 * vault-settle path — passing additional oracles would misalign the
 * trailing mint/token-program accounts. Callers must source `bankOracle`
 * from the debt bank's primary oracle slot.
 */
export interface ClaimRepaymentForRiskProfileArgs {
  payer: PublicKey;
  market: PublicKey;
  sequence: bigint | BN | number;
  globalVault: PublicKey;
  debtMint: PublicKey;
  debtBank: PublicKey;
  debtLiquidityVault: PublicKey;
  debtBankLiquidityVaultAuthority: PublicKey;
  bankOracle: PublicKey;
  lenderMarginfiAccount: PublicKey;
  tokenProgram: PublicKey;
  marginfiGroup: PublicKey;
  marginfiProgram: PublicKey;
  /** REQUIRED: cranker that paid the Loan PDA's rent — bound at loan-create time. */
  crankerRefund: PublicKey;
}

export function claimRepaymentForRiskProfileInstruction(
  args: ClaimRepaymentForRiskProfileArgs,
): TransactionInstruction {
  const loan = loanPda(args.market, BigInt(args.sequence.toString()))[0];
  const marketSigner = marketSignerPda(args.market)[0];
  const marketDebtVault = marketTokenVaultPda(args.market, args.debtMint)[0];
  const vaultSigner = globalVaultSignerPda(args.globalVault)[0];
  const vaultStaging = globalVaultStagingPda(args.globalVault)[0];
  const vaultIntegration = globalVaultIntegrationAccountPda(args.globalVault)[0];
  const data = new Writer()
    .u8(InstructionTag.ClaimRepaymentForRiskProfile)
    .toBuffer();
  const keys = [
    signerRw(args.payer),
    ro(globalConfigPda()[0]),
    rw(args.market),
    rw(loan),
    rw(args.globalVault),
    ro(vaultSigner),
    rw(vaultStaging),
    rw(vaultIntegration),
    rw(marketDebtVault),
    ro(marketSigner),
    rw(args.lenderMarginfiAccount),
    rw(args.debtBank),
    rw(args.debtLiquidityVault),
    ro(args.debtBankLiquidityVaultAuthority),
    ro(args.bankOracle),
    ro(args.debtMint),
    ro(args.tokenProgram),
    ro(args.marginfiGroup),
    ro(args.marginfiProgram),
    rw(args.crankerRefund),
  ];
  return ydeltaIx(keys, data);
}
