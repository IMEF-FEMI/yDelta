/**
 * Tier 4 e2e: cranker promotes the matched-loan queue node into a real
 * `LoanFixed` PDA.
 *
 *   1. Use `driveToMatchLanded` to set up + land a matched-loan node.
 *   2. Any keypair (the "cranker") calls `process_matched_loan` with the
 *      full vault-settle account block — required for vault-lender matches.
 *   3. Verify:
 *      - The matched-loan queue node is gone (the cranker zeroed it).
 *      - A new Loan PDA exists, decoded as Fixed / Active with the locked
 *        rates + term + principal.
 *      - Profile: encumbered → 0, deployed = principal (atoms physically
 *        moved from vault.integration to market.lender_integration).
 *      - Borrower seat: collateral_withdrawable → encumbered.
 */
import { beforeAll, describe, expect, it } from 'vitest';
import { Keypair } from '@solana/web3.js';

import {
  cuBudgetIx,
  decodeBank,
  decodeGlobalVault,
  decodeLoanFixed,
  decodeMarket,
  FP48_SHIFT,
  globalVaultIntegrationAccountPda,
  globalVaultPda,
  globalVaultSignerPda,
  globalVaultStagingPda,
  HEAVY_IX_CU_LIMIT,
  lenderIntegrationAccountPda,
  loanPda,
  LoanState,
  LoanType,
  marketSignerPda,
  marketTokenVaultPda,
  processMatchedLoanInstruction,
} from '../../src/index.js';
import { bootBankrun, BankrunHandle } from './_bankrun.ts';
import {
  MARGINFI_GROUP,
  MARGINFI_PROGRAM_ID,
  SPL_TOKEN_PROGRAM_ID,
  USDC_BANK,
  USDC_LIQUIDITY_VAULT,
  USDC_MINT,
  USDC_ORACLE,
} from './_fixtures.ts';
import { bankLiquidityVaultAuthority, driveToMatchLanded, MatchLandedHandles } from './_setup.ts';

describe('e2e: cranker promotes the matched loan to a Loan PDA', () => {
  let bk: BankrunHandle;
  let handles: MatchLandedHandles;
  let cranker: Keypair;

  beforeAll(async () => {
    bk = await bootBankrun({ loadMarginfiFixtures: true });
    handles = await driveToMatchLanded(bk);
    cranker = await bk.fundedKeypair();
  });

  it('ProcessMatchedLoan stamps a Fixed/Active Loan PDA + drains vault → market', async () => {
    const vault = globalVaultPda(USDC_MINT)[0];
    const [loanKey] = loanPda(handles.market.publicKey, handles.matchedLoanSequence);

    // Vault-lender match → cranker must supply the full vault-settle block.
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

    // Loan PDA materialised + decoded.
    const loanAcc = await bk.getAccount(loanKey);
    expect(loanAcc).not.toBeNull();
    const loan = decodeLoanFixed(loanAcc!.data);
    expect(loan.market.equals(handles.market.publicKey)).toBe(true);
    expect(loan.principalDebtAtoms).toBe(handles.principalAtoms);
    expect(loan.collateralAtoms).toBe(handles.collateralAtoms);
    expect(loan.lenderRateBps).toBe(handles.askRateBps);
    expect(loan.state).toBe(LoanState.Active);
    expect(loan.loanType).toBe(LoanType.Fixed);
    expect(loan.createdBy.equals(cranker.publicKey)).toBe(true);
    // At promote-time both `outstanding_debt_atoms` and `lender_claimable_atoms`
    // are stamped equal to `principal` as the initial baseline — interest
    // (borrower-rate / lender-rate respectively) accrues on top from
    // `last_accrued_unix` going forward.
    expect(loan.outstandingDebtAtoms).toBe(handles.principalAtoms);
    expect(loan.lenderClaimableAtoms).toBe(handles.principalAtoms);

    // Matched-loan queue is empty (the cranker freed the node).
    const m = decodeMarket((await bk.getAccount(handles.market.publicKey))!.data);
    expect(m.matchedLoans).toHaveLength(0);

    // Profile bookkeeping: encumbered → 0, deployed = principal.
    const v = decodeGlobalVault((await bk.getAccount(vault))!.data);
    const profile = v.riskProfiles[0].profile;
    expect(profile.encumberedInOrdersAtoms).toBe(0n);
    expect(profile.deployedPrincipalAtoms).toBe(handles.principalAtoms);

    // Borrower seat: `process_matched_loan` credits the borrower's
    // `debt_withdrawable_shares` with EXACTLY (principal − origination_atoms)
    // worth of debt shares. With `origination_bps = 0` (our default), the
    // back-computed atom value must equal `principal` within share-rounding
    // dust (≤ 1 atom).
    const borrowerSeat = m.claimedSeats.find((s) => s.seat.owner.equals(handles.borrower.publicKey))!.seat;
    const bank = decodeBank((await bk.getAccount(USDC_BANK))!.data);
    const creditedAtoms =
      (borrowerSeat.debtWithdrawableShares * bank.assetShareValueFp48) >> (FP48_SHIFT * 2n);
    const drift =
      creditedAtoms > handles.principalAtoms
        ? creditedAtoms - handles.principalAtoms
        : handles.principalAtoms - creditedAtoms;
    expect(drift).toBeLessThanOrEqual(1n);
  });
});
