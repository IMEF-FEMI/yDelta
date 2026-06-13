/**
 * Tier 6 e2e: borrower defaults — keeper settles the matured loan.
 *
 *   1. driveToMatchLanded → processMatchedLoan → Loan PDA exists.
 *   2. Borrower does NOT repay.
 *   3. Warp past `matures_at_unix + grace_period_seconds`.
 *   4. A permissionless keeper / settler calls `SettleMaturedLoan` with
 *      enough USDC to cover the outstanding principal + accrued interest;
 *      they receive the borrower's collateral.
 *   5. Loan PDA closes (v1: a full settle closes it in-ix), subVault.deployed
 *      → 0, and the realised atoms land in subVault.pending_claim_atoms.
 *
 * Mirror of the on-chain `settle_matured_loan` flow — this exercises the
 * heavier 4-CPI cranker path that liquidate_loan / settle_matured_loan share.
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
  settleMaturedLoanInstruction,
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

describe('e2e: borrower defaults → keeper settles matured loan', () => {
  let bk: BankrunHandle;
  let handles: MatchLandedHandles;
  let cranker: Keypair;
  let settler: Keypair;
  let settlerUsdcAta: PublicKey;
  let settlerSolAta: PublicKey;
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

    // Settler comes in with USDC to pay off the loan + empty wSOL ATA to
    // receive the seized collateral.
    settler = await bk.fundedKeypair();
    settlerUsdcAta = Keypair.generate().publicKey;
    settlerSolAta = Keypair.generate().publicKey;
    await bk.putTokenAccount({
      address: settlerUsdcAta,
      mint: USDC_MINT,
      owner: settler.publicKey,
      amount: 1_000_000_000n,
    });
    await bk.putWsolTokenAccount({
      address: settlerSolAta,
      owner: settler.publicKey,
      amount: 0n,
    });
  });

  it('Pre-settle attempt before maturity fails with LoanNotMatured', async () => {
    await bk.refreshOracleFreshness({ pythOracle: USDC_ORACLE, switchboardOracle: SOL_ORACLE });
    await expect(
      bk.send(
        [
          cuBudgetIx(HEAVY_IX_CU_LIMIT),
          settleMaturedLoanInstruction({
            payer: settler.publicKey,
            market: handles.market.publicKey,
            sequence: handles.matchedLoanSequence,
            debtMint: USDC_MINT,
            collateralMint: WSOL_MINT,
            liquidatorDebtToken: settlerUsdcAta,
            liquidatorCollateralToken: settlerSolAta,
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
            repayAtomsMax: 0n, // 0 = full repay
            crankerRefund: cranker.publicKey,
            globalVault: globalVaultPda(USDC_BANK)[0], // Fixed loan: vault-lent
          }),
        ],
        [settler],
      ),
    ).rejects.toThrow();
  });

  it('After warping past matures_at + grace, SettleMaturedLoan closes the loan', async () => {
    // 30-day term + 24-hour grace period. The check is STRICT `now > maturity
    // + grace`, so we warp slightly past the boundary.
    await bk.warpForward(31 * 86_400 + 3_600);

    // Refresh oracles since maturity check & subsequent CPIs touch them.
    await bk.refreshOracleFreshness({ pythOracle: USDC_ORACLE, switchboardOracle: SOL_ORACLE });

    const usdcPre = (await bk.tokenAccountBalance(settlerUsdcAta))!;
    const solPre = (await bk.tokenAccountBalance(settlerSolAta))!;
    expect(usdcPre).toBe(1_000_000_000n);
    expect(solPre).toBe(0n);

    await bk.send(
      [
        cuBudgetIx(HEAVY_IX_CU_LIMIT),
        settleMaturedLoanInstruction({
          payer: settler.publicKey,
          market: handles.market.publicKey,
          sequence: handles.matchedLoanSequence,
          debtMint: USDC_MINT,
          collateralMint: WSOL_MINT,
          liquidatorDebtToken: settlerUsdcAta,
          liquidatorCollateralToken: settlerSolAta,
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
          repayAtomsMax: 0n, // 0 = full repay
          crankerRefund: cranker.publicKey,
          globalVault: globalVaultPda(USDC_BANK)[0], // Fixed loan: vault-lent
        }),
      ],
      [settler],
    );

    // v1: a full SettleMaturedLoan seizes the collateral, retires the debt,
    // and CLOSES the loan PDA in the same ix (rent → the original cranker).
    // The realised atoms land in the sub-vault's `pending_claim_atoms`
    // bucket, swept by the lender-side `ClaimRepaymentForSubVault` step.
    expect(await bk.getAccount(loanKey)).toBeNull();

    // Settler balance changes:
    //  - USDC: paid exactly the full outstanding (principal + integer-floor
    //    accrued interest over 31 days at 800 bps on 100 atoms ≈ 0).
    //  - wSOL: received the original collateral, minus AT MOST 1 atom
    //    of marginfi share-rounding dust. The on-chain settle uses
    //    `atoms_to_shares` (rounds DOWN), which strands ≤ 1 atom per
    //    side-transfer; tightly bounded so any larger drift trips the
    //    assertion.
    const usdcPost = (await bk.tokenAccountBalance(settlerUsdcAta))!;
    const solPost = (await bk.tokenAccountBalance(settlerSolAta))!;
    const usdcSpent = usdcPre - usdcPost;
    const solReceived = solPost - solPre;
    expect(usdcSpent).toBe(handles.principalAtoms); // exactly 100 atoms; interest = 0
    const collateralDust = handles.collateralAtoms - solReceived;
    expect(collateralDust).toBeGreaterThanOrEqual(0n);
    expect(collateralDust).toBeLessThanOrEqual(1n);
  });

  it('ClaimRepaymentForSubVault sweeps the settled atoms back into the vault', async () => {
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

    // Loan PDA was already closed at full-settle time; the sweep never
    // touches it. Sub-vault back to all-idle: pending_claim drained. Curator's
    // resting ask is still there — vault never withdrew it.
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
    expect(subVault.encumberedInOrdersAtoms).toBe(0n);
    const m = decodeMarket((await bk.getAccount(handles.market.publicKey))!.data);
    expect(m.asks).toHaveLength(1);
  });
});
