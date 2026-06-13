/**
 * Tier 5 e2e: borrower repays, then a permissionless cranker realises the
 * repayment for the lender. This is the full UX:
 *
 *   Borrower side (the human action):
 *     1. `Repay` — atoms flow borrower → market debt vault →
 *        vault.integration; loan.outstanding_debt_atoms → 0. The loan
 *        PDA stays open at this point (state == Active) because the
 *        lender-side bookkeeping is a separate cranker step.
 *
 *   Lender / keeper side (permissionless, anyone can fire):
 *     2. `ClaimRepaymentForSubVault` — stateless sweep: drains the realised
 *        atoms (sub-vault `pending_claim_atoms`) out of the per-market
 *        `lender_marginfi_account` back into the vault's own integration
 *        account, and decrements the seat shares + `pending_claim_atoms`.
 *        v1: the loan PDA is already closed by full `Repay` (rent → original
 *        cranker); this sweep never touches a loan PDA and takes no sequence.
 */
import { beforeAll, describe, expect, it } from 'vitest';
import { Keypair, PublicKey } from '@solana/web3.js';

import {
  claimRepaymentForSubVaultInstruction,
  cuBudgetIx,
  decodeGlobalVault,
  decodeMarket,
  globalVaultIntegrationAccountPda,
  globalVaultPda,
  globalVaultSignerPda,
  globalVaultStagingPda,
  HEAVY_IX_CU_LIMIT,
  lenderIntegrationAccountPda,
  loanPda,
  marketSignerPda,
  marketTokenVaultPda,
  processMatchedLoanInstruction,
  repayInstruction,
} from '../../src/index.js';
import { bootBankrun, BankrunHandle } from './_bankrun.ts';
import {
  MARGINFI_GROUP,
  MARGINFI_PROGRAM_ID,
  SOL_BANK,
  SPL_TOKEN_PROGRAM_ID,
  USDC_BANK,
  USDC_LIQUIDITY_VAULT,
  USDC_MINT,
  USDC_ORACLE,
} from './_fixtures.ts';
import { bankLiquidityVaultAuthority, driveToMatchLanded, MatchLandedHandles } from './_setup.ts';

describe('e2e: borrower repays + cranker realises (loan PDA closes)', () => {
  let bk: BankrunHandle;
  let handles: MatchLandedHandles;
  let cranker: Keypair;
  let loanKey: PublicKey;

  beforeAll(async () => {
    bk = await bootBankrun({ loadMarginfiFixtures: true });
    handles = await driveToMatchLanded(bk);
    cranker = await bk.fundedKeypair();
    loanKey = loanPda(handles.market.publicKey, handles.matchedLoanSequence)[0];

    // Crank the match into a Loan PDA.
    const vault = globalVaultPda(USDC_BANK)[0];
    await bk.refreshOracleFreshness({ pythOracle: USDC_ORACLE });
    await bk.send(
      [
        cuBudgetIx(HEAVY_IX_CU_LIMIT),
        processMatchedLoanInstruction({
          payer: cranker.publicKey,
          market: handles.market.publicKey,
          debtBank: USDC_BANK,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          sequence: handles.matchedLoanSequence,
          vaultSettle: {
            globalVault: vault,
            globalVaultSigner: globalVaultSignerPda(vault)[0],
            globalVaultStaging: globalVaultStagingPda(vault)[0],
            globalVaultIntegrationAccount: globalVaultIntegrationAccountPda(vault)[0],
            marketDebtVault: marketTokenVaultPda(handles.market.publicKey, USDC_MINT)[0],
            marketLenderIntegrationAccount: lenderIntegrationAccountPda(handles.market.publicKey)[0],
            marketSigner: marketSignerPda(handles.market.publicKey)[0],
            debtLiquidityVault: USDC_LIQUIDITY_VAULT,
            debtBankLiquidityVaultAuthority: bankLiquidityVaultAuthority(USDC_BANK),
            debtOracles: [USDC_ORACLE],
            debtMint: USDC_MINT,
            tokenProgram: SPL_TOKEN_PROGRAM_ID,
            marginfiGroup: MARGINFI_GROUP,
            marginfiProgram: MARGINFI_PROGRAM_ID,
          },
        }),
      ],
      [cranker],
    );

    // Top up the borrower's USDC ATA so they can repay.
    await bk.putTokenAccount({
      address: handles.borrowerUsdcAta,
      mint: USDC_MINT,
      owner: handles.borrower.publicKey,
      amount: 1_000_000_000n,
    });
  });

  it('Repay zeroes outstanding, closes the loan PDA, and debits the borrower ATA by exactly repay_atoms', async () => {
    const ataPre = (await bk.tokenAccountBalance(handles.borrowerUsdcAta))!;
    expect(ataPre).toBe(1_000_000_000n); // matches the top-up in beforeAll

    const vault = globalVaultPda(USDC_BANK)[0];
    await bk.send(
      [
        repayInstruction({
          borrower: handles.borrower.publicKey,
          market: handles.market.publicKey,
          sequence: handles.matchedLoanSequence,
          debtMint: USDC_MINT,
          borrowerToken: handles.borrowerUsdcAta,
          tokenProgram: SPL_TOKEN_PROGRAM_ID,
          marginfiGroup: MARGINFI_GROUP,
          debtBank: USDC_BANK,
          debtLiquidityVault: USDC_LIQUIDITY_VAULT,
          collateralBank: SOL_BANK,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          repayAtoms: handles.principalAtoms,
          crankerRefund: cranker.publicKey,
          // Fixed (vault-lender) loan: the processor reads the lender vault on
          // full repay to apply the sub-vault decrements + bump
          // `pending_claim_atoms`.
          globalVault: vault,
        }),
      ],
      [handles.borrower],
    );

    // v1: a full Repay zeroes outstanding AND closes the loan PDA in the same
    // ix (rent → the original cranker). The realised atoms land in the
    // sub-vault's `pending_claim_atoms` bucket, swept by the lender-side
    // `ClaimRepaymentForSubVault` step below.
    expect(await bk.getAccount(loanKey)).toBeNull();

    // Sub-vault: deployed → 0 (decremented at close), pending_claim grew.
    const v = decodeGlobalVault((await bk.getAccount(vault))!.data);
    const subVault = v.subVaults[0].subVault;
    expect(subVault.deployedPrincipalAtoms).toBe(0n);
    expect(subVault.pendingClaimAtoms).toBeGreaterThan(0n);

    // The borrower's ATA was debited by EXACTLY repay_atoms — no over-
    // transfer, no skimming.
    const ataPost = (await bk.tokenAccountBalance(handles.borrowerUsdcAta))!;
    expect(ataPost).toBe(ataPre - handles.principalAtoms);
  });

  it('ClaimRepaymentForSubVault sweeps pending_claim back into the vault integration account', async () => {
    const vault = globalVaultPda(USDC_BANK)[0];
    const pendingPre = decodeGlobalVault((await bk.getAccount(vault))!.data).subVaults[0].subVault
      .pendingClaimAtoms;
    expect(pendingPre).toBeGreaterThan(0n);

    await bk.refreshOracleFreshness({ pythOracle: USDC_ORACLE });
    await bk.send(
      [
        cuBudgetIx(HEAVY_IX_CU_LIMIT),
        claimRepaymentForSubVaultInstruction({
          payer: cranker.publicKey,
          market: handles.market.publicKey,
          subVaultId: 1,
          globalVault: vault,
          debtMint: USDC_MINT,
          debtBank: USDC_BANK,
          debtLiquidityVault: USDC_LIQUIDITY_VAULT,
          debtBankLiquidityVaultAuthority: bankLiquidityVaultAuthority(USDC_BANK),
          bankOracles: [USDC_ORACLE],
          lenderMarginfiAccount: lenderIntegrationAccountPda(handles.market.publicKey)[0],
          tokenProgram: SPL_TOKEN_PROGRAM_ID,
          marginfiGroup: MARGINFI_GROUP,
          marginfiProgram: MARGINFI_PROGRAM_ID,
        }),
      ],
      [cranker],
    );

    // Loan PDA was already closed at full-repay time; the sweep never touches
    // it. The sub-vault is back to all-idle: pending_claim drained, nothing
    // deployed or encumbered.
    expect(await bk.getAccount(loanKey)).toBeNull();
    const v = decodeGlobalVault((await bk.getAccount(vault))!.data);
    const subVault = v.subVaults[0].subVault;
    // The sweep drains pending_claim down to AT MOST 1 atom of marginfi
    // share-round-down dust (the withdraw converts shares→atoms rounding
    // DOWN, so `saturating_sub(actual_atoms)` can strand ≤ 1 atom — the
    // program's own idle invariant tolerates this, never forcing it to 0).
    expect(subVault.pendingClaimAtoms).toBeLessThan(pendingPre);
    expect(subVault.pendingClaimAtoms).toBeLessThanOrEqual(1n);
    expect(subVault.deployedPrincipalAtoms).toBe(0n);
    expect(subVault.encumberedInOrdersAtoms).toBe(0n);

    // Asks tree still has the curator's unbounded resting ask — the
    // sub-vault is ready to fund another match without re-quoting.
    const m = decodeMarket((await bk.getAccount(handles.market.publicKey))!.data);
    expect(m.asks).toHaveLength(1);
    expect(m.asks[0].order.rateBps).toBe(handles.askRateBps);
  });
});
