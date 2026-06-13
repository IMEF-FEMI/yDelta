/**
 * Tier 8 e2e: LTV-based liquidation.
 *
 *   1. driveToMatchLanded → processMatchedLoan → Loan PDA exists, solvent.
 *   2. Pre-crash sanity: LiquidateLoan must reject with `LoanStillSolvent`
 *      (Custom 40) — the loan is well over-collateralised at mainnet prices.
 *   3. Crash the SOL oracle price to $0.001 (fp18 = 1e15).
 *   4. Keeper calls LiquidateLoan → succeeds, seizes collateral, repays
 *      lender, and (v1) closes the loan PDA in-ix.
 *   5. ClaimRepaymentForSubVault sweeps the realised atoms back to the vault.
 *
 * Mirrors the Rust `liquidate_loan_breaches_at_oracle_drop_succeeds` test.
 */
import { beforeAll, describe, expect, it } from 'vitest';
import { Keypair, PublicKey } from '@solana/web3.js';

import {
  claimRepaymentForSubVaultInstruction,
  cuBudgetIx,
  decodeGlobalVault,
  globalVaultIntegrationAccountPda,
  globalVaultPda,
  globalVaultSignerPda,
  globalVaultStagingPda,
  HEAVY_IX_CU_LIMIT,
  lenderIntegrationAccountPda,
  liquidateLoanInstruction,
  loanPda,
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
            globalVault: globalVaultPda(USDC_BANK)[0], // Fixed loan: vault-lent
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
          globalVault: globalVaultPda(USDC_BANK)[0], // Fixed loan: vault-lent
        }),
      ],
      [keeper],
    );

    // v1: a full liquidation retires the debt, seizes the collateral, AND
    // closes the loan PDA in the same ix (rent → the original cranker). The
    // realised atoms land in the sub-vault's `pending_claim_atoms` bucket.
    expect(await bk.getAccount(loanKey)).toBeNull();

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

  it('ClaimRepaymentForSubVault sweeps the liquidated atoms back + frees the sub-vault', async () => {
    // The loan PDA was already closed at full-liquidation time; the sweep is a
    // stateless seat→vault move with no maturity gate. The realised atoms sit
    // in the sub-vault's pending_claim bucket until swept here.
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

    expect(await bk.getAccount(loanKey)).toBeNull();
    const v = decodeGlobalVault((await bk.getAccount(vault))!.data);
    const subVault = v.subVaults[0].subVault;
    // The sweep drains pending_claim down to AT MOST 1 atom of marginfi
    // share-round-down dust (withdraw shares→atoms rounds DOWN, so
    // `saturating_sub(actual_atoms)` can strand ≤ 1 atom — the program's
    // own idle invariant tolerates this rather than forcing it to 0).
    expect(subVault.pendingClaimAtoms).toBeLessThan(pendingPre);
    expect(subVault.pendingClaimAtoms).toBeLessThanOrEqual(1n);
    expect(subVault.deployedPrincipalAtoms).toBe(0n);
  });
});
