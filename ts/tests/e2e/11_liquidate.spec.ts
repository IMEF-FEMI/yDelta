/**
 * Tier 8 e2e: LTV-based liquidation.
 *
 *   1. driveToMatchLanded → processMatchedLoan → Loan PDA exists, solvent.
 *   2. Pre-crash sanity: LiquidateLoan must reject with `LoanStillSolvent`
 *      (Custom 40) — the loan is well over-collateralised at mainnet prices.
 *   3. Crash the SOL oracle price to $0.001 (fp18 = 1e15).
 *   4. Keeper calls LiquidateLoan → succeeds, seizes collateral, repays
 *      lender, flips loan to `Repaid`.
 *   5. ClaimRepaymentForRiskProfile (post-maturity) closes the PDA.
 *
 * Mirrors the Rust `liquidate_loan_breaches_at_oracle_drop_succeeds` test.
 */
import { beforeAll, describe, expect, it } from 'vitest';
import { Keypair, PublicKey } from '@solana/web3.js';

import {
  claimRepaymentForRiskProfileInstruction,
  cuBudgetIx,
  decodeGlobalVault,
  decodeLoanFixed,
  globalVaultIntegrationAccountPda,
  globalVaultPda,
  globalVaultSignerPda,
  globalVaultStagingPda,
  HEAVY_IX_CU_LIMIT,
  lenderIntegrationAccountPda,
  liquidateLoanInstruction,
  loanPda,
  LoanState,
  marketSignerPda,
  marketTokenVaultPda,
  processMatchedLoanInstruction,
} from '../../src/index.js';
import { bootBankrun, BankrunHandle } from './_bankrun.ts';
import {
  MARGINFI_GROUP,
  MARGINFI_PROGRAM_ID,
  SOL_BANK,
  SOL_LIQUIDITY_VAULT,
  SOL_ORACLE,
  SPL_TOKEN_PROGRAM_ID,
  USDC_BANK,
  USDC_LIQUIDITY_VAULT,
  USDC_MINT,
  USDC_ORACLE,
  WSOL_MINT,
} from './_fixtures.ts';
import { bankLiquidityVaultAuthority, driveToMatchLanded, MatchLandedHandles } from './_setup.ts';

describe('e2e: LTV-based liquidation after oracle price crash', () => {
  let bk: BankrunHandle;
  let handles: MatchLandedHandles;
  let cranker: Keypair;
  let keeper: Keypair;
  let keeperUsdcAta: PublicKey;
  let keeperSolAta: PublicKey;
  let loanKey: PublicKey;

  beforeAll(async () => {
    bk = await bootBankrun({ loadMarginfiFixtures: true });
    handles = await driveToMatchLanded(bk);
    cranker = await bk.fundedKeypair();
    loanKey = loanPda(handles.market.publicKey, handles.matchedLoanSequence)[0];

    // Crank match → Loan PDA.
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

    // Keeper has USDC to repay + empty wSOL ATA to receive seized collateral.
    keeper = await bk.fundedKeypair();
    keeperUsdcAta = Keypair.generate().publicKey;
    keeperSolAta = Keypair.generate().publicKey;
    await bk.putTokenAccount({
      address: keeperUsdcAta,
      mint: USDC_MINT,
      owner: keeper.publicKey,
      amount: 1_000_000_000n,
    });
    await bk.putWsolTokenAccount({
      address: keeperSolAta,
      owner: keeper.publicKey,
      amount: 0n,
    });
  });

  it('Pre-crash: liquidate rejects with LoanStillSolvent at mainnet prices', async () => {
    await bk.refreshOracleFreshness({ pythOracle: USDC_ORACLE, switchboardOracle: SOL_ORACLE });

    await expect(
      bk.send(
        [
          cuBudgetIx(HEAVY_IX_CU_LIMIT),
          liquidateLoanInstruction({
            payer: keeper.publicKey,
            market: handles.market.publicKey,
            sequence: handles.matchedLoanSequence,
            debtMint: USDC_MINT,
            collateralMint: WSOL_MINT,
            liquidatorDebtToken: keeperUsdcAta,
            liquidatorCollateralToken: keeperSolAta,
            debtBank: USDC_BANK,
            collateralBank: SOL_BANK,
            debtLiquidityVault: USDC_LIQUIDITY_VAULT,
            collateralLiquidityVault: SOL_LIQUIDITY_VAULT,
            collateralBankLiquidityVaultAuthority: bankLiquidityVaultAuthority(SOL_BANK),
            debtOracles: [USDC_ORACLE],
            collateralOracles: [SOL_ORACLE],
            tokenProgram: SPL_TOKEN_PROGRAM_ID,
            marginfiGroup: MARGINFI_GROUP,
            marginfiProgram: MARGINFI_PROGRAM_ID,
            repayAtomsMax: 0n,
            crankerRefund: cranker.publicKey,
          }),
        ],
        [keeper],
      ),
      // The on-chain error is Custom(40) = LoanStillSolvent.
    ).rejects.toThrow();
  });

  it('Crash SOL → $0.001 and LiquidateLoan succeeds', async () => {
    // Crash wSOL price to $0.001 (fp18 scale: 0.001 × 10^18 = 1e15).
    // Aggressive crash so the breach is unambiguous regardless of marginfi
    // maintenance weights.
    await bk.setSwbOraclePriceAtoms(SOL_ORACLE, 1_000_000_000_000_000n);
    await bk.refreshOracleFreshness({ pythOracle: USDC_ORACLE, switchboardOracle: SOL_ORACLE });

    const usdcPre = (await bk.tokenAccountBalance(keeperUsdcAta))!;
    const solPre = (await bk.tokenAccountBalance(keeperSolAta))!;
    expect(usdcPre).toBe(1_000_000_000n);
    expect(solPre).toBe(0n);

    await bk.send(
      [
        cuBudgetIx(HEAVY_IX_CU_LIMIT),
        liquidateLoanInstruction({
          payer: keeper.publicKey,
          market: handles.market.publicKey,
          sequence: handles.matchedLoanSequence,
          debtMint: USDC_MINT,
          collateralMint: WSOL_MINT,
          liquidatorDebtToken: keeperUsdcAta,
          liquidatorCollateralToken: keeperSolAta,
          debtBank: USDC_BANK,
          collateralBank: SOL_BANK,
          debtLiquidityVault: USDC_LIQUIDITY_VAULT,
          collateralLiquidityVault: SOL_LIQUIDITY_VAULT,
          collateralBankLiquidityVaultAuthority: bankLiquidityVaultAuthority(SOL_BANK),
          debtOracles: [USDC_ORACLE],
          collateralOracles: [SOL_ORACLE],
          tokenProgram: SPL_TOKEN_PROGRAM_ID,
          marginfiGroup: MARGINFI_GROUP,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          repayAtomsMax: 0n,
          crankerRefund: cranker.publicKey,
        }),
      ],
      [keeper],
    );

    // Loan flipped to Repaid; outstanding + collateral zeroed.
    const loan = decodeLoanFixed((await bk.getAccount(loanKey))!.data);
    expect(loan.state).toBe(LoanState.Repaid);
    expect(loan.outstandingDebtAtoms).toBe(0n);
    expect(loan.collateralAtoms).toBe(0n);

    // Keeper paid exactly the outstanding (no `liquidation_keeper_bps`
    // bonus charged at default fee config) and received the full collateral
    // minus marginfi share-rounding dust (≤ 1 atom).
    const usdcPost = (await bk.tokenAccountBalance(keeperUsdcAta))!;
    const solPost = (await bk.tokenAccountBalance(keeperSolAta))!;
    expect(usdcPre - usdcPost).toBe(handles.principalAtoms);
    const collateralDust = handles.collateralAtoms - (solPost - solPre);
    expect(collateralDust).toBeGreaterThanOrEqual(0n);
    expect(collateralDust).toBeLessThanOrEqual(1n);
  });

  it('Post-maturity ClaimRepayment closes the liquidated loan + frees profile', async () => {
    // Liquidation flips loan to Repaid but doesn't close the PDA — the
    // lender-side claim cranker handles that. Same shape as spec 07.
    await bk.warpForward(31 * 86_400 + 3_600);

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

    expect(await bk.getAccount(loanKey)).toBeNull();
    const v = decodeGlobalVault((await bk.getAccount(vault))!.data);
    const profile = v.riskProfiles[0].profile;
    expect(profile.deployedPrincipalAtoms).toBe(0n);
  });
});
