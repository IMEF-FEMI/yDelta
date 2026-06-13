import { PublicKey, TransactionInstruction } from '@solana/web3.js';

import {
  globalConfigPda,
  globalVaultIntegrationAccountPda,
  globalVaultSignerPda,
  globalVaultStagingPda,
  marketSignerPda,
  marketTokenVaultPda,
} from '../pdas.js';
import { ro, rw, signerRw, ydeltaIx } from './_helpers.js';
import { Writer } from './_serialise.js';
import { InstructionTag } from './_tags.js';

/**
 * Tag 20 — `ClaimRepaymentForSubVault`. Permissionless cranker — sweeps a
 * sub-vault's `pendingClaimAtoms` from the market's
 * `lenderMarginfiAccount` back into the vault's own `integrationAccount`
 * (marginfi withdraw via the vault staging ATA). Heavy (CPIs + oracle
 * read) — wrap in `withCuBudget(...)`.
 *
 * Only ONE oracle (`bank.config.primary_oracle()`) is loaded by the
 * vault-settle path — passing additional oracles would misalign the
 * trailing mint/token-program accounts. Callers must source `bankOracles`
 * from the debt bank's primary oracle slot.
 */
export interface ClaimRepaymentForSubVaultArgs {
  payer: PublicKey;
  market: PublicKey;
  subVaultId: number;
  globalVault: PublicKey;
  debtMint: PublicKey;
  debtBank: PublicKey;
  debtLiquidityVault: PublicKey;
  debtBankLiquidityVaultAuthority: PublicKey;
  bankOracles: PublicKey[];
  lenderMarginfiAccount: PublicKey;
  tokenProgram: PublicKey;
  marginfiGroup: PublicKey;
  marginfiProgram: PublicKey;
}

export function claimRepaymentForSubVaultInstruction(
  args: ClaimRepaymentForSubVaultArgs,
): TransactionInstruction {
  const marketSigner = marketSignerPda(args.market)[0];
  const marketDebtVault = marketTokenVaultPda(args.market, args.debtMint)[0];
  const vaultSigner = globalVaultSignerPda(args.globalVault)[0];
  const vaultStaging = globalVaultStagingPda(args.globalVault)[0];
  const vaultIntegration = globalVaultIntegrationAccountPda(args.globalVault)[0];
  const data = new Writer()
    .u8(InstructionTag.ClaimRepaymentForSubVault)
    .u16(args.subVaultId)
    .toBuffer();
  const keys = [
    signerRw(args.payer),
    ro(globalConfigPda()[0]),
    rw(args.market),
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
    ...args.bankOracles.map(ro),
    ro(args.debtMint),
    ro(args.tokenProgram),
    ro(args.marginfiGroup),
    ro(args.marginfiProgram),
  ];
  return ydeltaIx(keys, data);
}
