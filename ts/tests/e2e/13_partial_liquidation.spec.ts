/**
 * Tier 8b e2e: partial liquidation. Liquidator only has enough USDC to
 * cover part of the outstanding debt. The loan must stay `Active` with
 * reduced `outstanding_debt_atoms` and `collateral_atoms`.
 *
 *   1. driveToMatchLanded → processMatchedLoan → Loan PDA (principal=100).
 *   2. Crash SOL oracle to $0.001 so liquidation gate clears.
 *   3. Liquidator calls `LiquidateLoan` with `repay_atoms_max = 40`.
 *   4. Verify: loan.state == Active, outstanding decreased by 40 atoms,
 *      collateral decreased pro-rata.
 *   5. (Second tx in the same spec): liquidator finishes the job with
 *      `repay_atoms_max = 0` (= full) → loan flips to Repaid.
 */
import { beforeAll, describe, expect, it } from 'vitest';
import { Keypair, PublicKey } from '@solana/web3.js';

import {
  cuBudgetIx,
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

describe('e2e: partial liquidation leaves loan Active with reduced balances', () => {
  let bk: BankrunHandle;
  let handles: MatchLandedHandles;
  let cranker: Keypair;
  let keeper: Keypair;
  let keeperUsdcAta: PublicKey;
  let keeperSolAta: PublicKey;
  let loanKey: PublicKey;
  let principalAtoms: bigint;
  let collateralAtoms: bigint;

  beforeAll(async () => {
    bk = await bootBankrun({ loadMarginfiFixtures: true });
    // The partial-liquidation floor on-chain rejects `outstanding < 1000`,
    // so we override the default tiny atoms with larger amounts that still
    // pass LTV at mainnet wSOL prices: 1 USDC borrow against 0.05 SOL.
    handles = await driveToMatchLanded(bk, {
      principalAtoms: 1_000_000n,
      collateralAtoms: 50_000_000n,
      vaultDepositAtoms: 1_000_000_000n, // 1 000 USDC headroom
      wsolFundAtoms: 200_000_000n, // 0.2 SOL ATA budget
      collateralDepositAtoms: 100_000_000n, // 0.1 SOL deposited
    });
    cranker = await bk.fundedKeypair();
    loanKey = loanPda(handles.market.publicKey, handles.matchedLoanSequence)[0];
    principalAtoms = handles.principalAtoms;
    collateralAtoms = handles.collateralAtoms;

    // Crank the match into a real Loan PDA.
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

    // Crash SOL → $0.001 so the LTV gate clears for both partial calls.
    await bk.setSwbOraclePriceAtoms(SOL_ORACLE, 1_000_000_000_000_000n);
  });

  it('Partial liquidate: keeper pays exactly repay_atoms_max, loan stays Active with pro-rata reductions', async () => {
    // Partial-liquidation floor: must repay >= max(1% of outstanding, 1000).
    // Outstanding ≈ 1 USDC = 1_000_000 atoms → floor is 10_000 atoms.
    // 400_000 = 40% covers the floor handsomely.
    const partialRepay = 400_000n;

    const usdcPre = (await bk.tokenAccountBalance(keeperUsdcAta))!;
    const solPre = (await bk.tokenAccountBalance(keeperSolAta))!;

    await bk.refreshOracleFreshness({ pythOracle: USDC_ORACLE, switchboardOracle: SOL_ORACLE });
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
          repayAtomsMax: partialRepay,
          crankerRefund: cranker.publicKey,
        }),
      ],
      [keeper],
    );

    const loan = decodeLoanFixed((await bk.getAccount(loanKey))!.data);
    expect(loan.state).toBe(LoanState.Active);

    // Outstanding dropped by EXACTLY the partial repay amount (the
    // processor accrues interest first, then debits `repay_atoms_max`
    // from outstanding; with no time elapsed, interest = 0).
    expect(loan.outstandingDebtAtoms).toBe(principalAtoms - partialRepay);

    // Collateral seizure: at the crashed SOL price ($0.001), the
    // 50M-atom wSOL collateral is worth only ~$50 — far less than the
    // $400 of USDC the keeper just paid. The on-chain logic hits the
    // "under-collateralized" branch (README §7): liquidator seizes the
    // FULL `collateral_atoms` (capped at what's available), surplus
    // to borrower = 0, and the bad-debt gap is logged via BadDebtLog.
    // After this partial, virtually ALL collateral has been swept onto
    // the keeper, leaving the loan with `outstanding > 0` and
    // `collateral_atoms ≤ 1` (still Active, since outstanding ≠ 0).
    // The 1-atom dust is the standard marginfi share-round-down floor.
    expect(loan.collateralAtoms).toBeLessThanOrEqual(1n);

    // Keeper balance changes:
    //   - USDC: paid EXACTLY `partialRepay` (no skimming, no fee).
    //   - wSOL: received the entire 50M collateral atoms minus dust.
    const usdcPost = (await bk.tokenAccountBalance(keeperUsdcAta))!;
    const solPost = (await bk.tokenAccountBalance(keeperSolAta))!;
    expect(usdcPre - usdcPost).toBe(partialRepay);
    const solReceived = solPost - solPre;
    const recvDrift =
      solReceived > collateralAtoms ? solReceived - collateralAtoms : collateralAtoms - solReceived;
    expect(recvDrift).toBeLessThanOrEqual(1n);
  });

  it('Second pass with repay_atoms_max = 0 finishes the liquidation', async () => {
    const usdcPre = (await bk.tokenAccountBalance(keeperUsdcAta))!;
    const solPre = (await bk.tokenAccountBalance(keeperSolAta))!;

    await bk.refreshOracleFreshness({ pythOracle: USDC_ORACLE, switchboardOracle: SOL_ORACLE });
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
          repayAtomsMax: 0n, // 0 = full
          crankerRefund: cranker.publicKey,
        }),
      ],
      [keeper],
    );

    const loan = decodeLoanFixed((await bk.getAccount(loanKey))!.data);
    expect(loan.state).toBe(LoanState.Repaid);
    expect(loan.outstandingDebtAtoms).toBe(0n);
    expect(loan.collateralAtoms).toBe(0n);

    // Across BOTH passes the keeper has paid exactly the original
    // principal (interest accrued = 0 at our zero-time-elapse setup).
    // The second pass receives at most 1 atom of residual collateral —
    // pass 1 already swept the rest under the bad-debt cap.
    const usdcPost = (await bk.tokenAccountBalance(keeperUsdcAta))!;
    const solPost = (await bk.tokenAccountBalance(keeperSolAta))!;
    expect(usdcPre - usdcPost).toBe(principalAtoms - 400_000n); // = 600_000
    expect(solPost - solPre).toBeLessThanOrEqual(1n);
  });
});
