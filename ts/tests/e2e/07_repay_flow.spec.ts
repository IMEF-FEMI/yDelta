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
 *     2. `ClaimRepaymentForRiskProfile` — moves the lender's seat shares
 *        encumbered → withdrawable, drains the realised atoms back into
 *        the vault's marginfi integration account, updates the profile
 *        aggregates, and CLOSES the loan PDA (rent → original cranker).
 */
import { beforeAll, describe, expect, it } from 'vitest';
import { Keypair, PublicKey } from '@solana/web3.js';

import {
  claimRepaymentForRiskProfileInstruction,
  cuBudgetIx,
  decodeGlobalVault,
  decodeLoanFixed,
  decodeMarket,
  globalVaultIntegrationAccountPda,
  globalVaultPda,
  globalVaultSignerPda,
  globalVaultStagingPda,
  HEAVY_IX_CU_LIMIT,
  lenderIntegrationAccountPda,
  loanPda,
  LoanState,
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
    const vault = globalVaultPda(USDC_MINT)[0];
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

  it('Repay zeroes outstanding, flips state→Repaid, and debits the borrower ATA by exactly repay_atoms', async () => {
    const ataPre = (await bk.tokenAccountBalance(handles.borrowerUsdcAta))!;
    expect(ataPre).toBe(1_000_000_000n); // matches the top-up in beforeAll

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
        }),
      ],
      [handles.borrower],
    );

    // Loan PDA still exists; outstanding decremented to 0 and `state`
    // flips to `Repaid`. The PDA closes at lender-side `ClaimRepayment`
    // time, not at borrower repay time (fixed-term lock-up: lender cannot
    // drain until maturity even after the borrower paid back early).
    const loanAcc = await bk.getAccount(loanKey);
    expect(loanAcc).not.toBeNull();
    const loan = decodeLoanFixed(loanAcc!.data);
    expect(loan.outstandingDebtAtoms).toBe(0n);
    expect(loan.state).toBe(LoanState.Repaid);

    // The borrower's ATA was debited by EXACTLY repay_atoms — no over-
    // transfer, no skimming.
    const ataPost = (await bk.tokenAccountBalance(handles.borrowerUsdcAta))!;
    expect(ataPost).toBe(ataPre - handles.principalAtoms);
  });

  it('ClaimRepaymentForRiskProfile closes the loan + frees up profile.deployed', async () => {
    // Fast-forward past the 30-day term so the maturity gate clears.
    await bk.warpForward(30 * 86_400 + 60);

    const vault = globalVaultPda(USDC_MINT)[0];
    await bk.refreshOracleFreshness({ pythOracle: USDC_ORACLE });
    await bk.send(
      [
        cuBudgetIx(HEAVY_IX_CU_LIMIT),
        claimRepaymentForRiskProfileInstruction({
          payer: cranker.publicKey,
          market: handles.market.publicKey,
          sequence: handles.matchedLoanSequence,
          globalVault: vault,
          debtMint: USDC_MINT,
          debtBank: USDC_BANK,
          debtLiquidityVault: USDC_LIQUIDITY_VAULT,
          debtBankLiquidityVaultAuthority: bankLiquidityVaultAuthority(USDC_BANK),
          bankOracle: USDC_ORACLE,
          lenderMarginfiAccount: lenderIntegrationAccountPda(handles.market.publicKey)[0],
          tokenProgram: SPL_TOKEN_PROGRAM_ID,
          marginfiGroup: MARGINFI_GROUP,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          crankerRefund: cranker.publicKey,
        }),
      ],
      [cranker],
    );

    // Loan PDA closed.
    const loanAcc = await bk.getAccount(loanKey);
    expect(loanAcc).toBeNull();

    // Profile is back to all-idle. The vault is whole.
    const v = decodeGlobalVault((await bk.getAccount(vault))!.data);
    const profile = v.riskProfiles[0].profile;
    expect(profile.deployedPrincipalAtoms).toBe(0n);
    expect(profile.encumberedInOrdersAtoms).toBe(0n);

    // Asks tree still has the curator's unbounded resting ask — the
    // profile is ready to fund another match without re-quoting.
    const m = decodeMarket((await bk.getAccount(handles.market.publicKey))!.data);
    expect(m.asks).toHaveLength(1);
    expect(m.asks[0].order.rateBps).toBe(handles.askRateBps);
  });
});
