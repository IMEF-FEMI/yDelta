/**
 * Tier 9 e2e: borrower upgrades a P2Pool (variable-rate) loan to fixed-rate
 * by walking the asks tree.
 *
 *   1. Empty book → borrower IOC bid w/ fallback ON → P2Pool MatchedLoan
 *      queued → cranker promotes to a `LoanType.P2Pool` PDA (spec 09's flow
 *      replayed inline).
 *   2. Curator funds a vault profile + posts an ask at 600 bps.
 *   3. Borrower calls `ConvertP2PoolToFixed(max_acceptable_rate = 1_000)`.
 *      The walk crosses the 600 bps ask; the entire marginfi liability is
 *      retired and a fresh Fixed MatchedLoan node lands at the next sequence.
 *   4. Verify: borrower's marginfi liability → 0, P2Pool PDA closed,
 *      market.matched_loan_sequence = 2 (P2Pool seq 0 + Fixed seq 1).
 */
import { beforeAll, describe, expect, it } from 'vitest';
import { Keypair, PublicKey } from '@solana/web3.js';

import {
  borrowerIntegrationAccountPda,
  claimSeatInstruction,
  convertP2poolToFixedInstruction,
  cuBudgetIx,
  decodeMarginfiAccount,
  decodeMarket,
  depositInstruction,
  globalVaultDepositInstruction,
  HEAVY_IX_CU_LIMIT,
  lenderIntegrationAccountPda,
  loanPda,
  LoanType,
  placeOrderForRiskProfileInstruction,
  placeOrderInstruction,
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
import {
  bankLiquidityVaultAuthority,
  setupGlobalConfig,
  setupMarket,
  setupRiskProfile,
  setupVault,
  unpauseMarket,
} from './_setup.ts';

describe('e2e: ConvertP2PoolToFixed refinances variable debt at a curator ask', () => {
  let bk: BankrunHandle;
  let admin: Keypair;
  let market: Keypair;
  let borrower: Keypair;
  let borrowerUsdcAta: PublicKey;
  let p2poolSequence: bigint;
  let p2poolLoanKey: PublicKey;
  let p2poolCranker: Keypair;

  // Larger amounts so the convert flow's `liability_atoms_to_fully_cover`
  // share-rounding lands at meaningful share counts; the tiny 100-atom case
  // hits marginfi's withdraw-only-mode guard on the lender side.
  const principalAtoms = 1_000_000n;
  const collateralAtoms = 50_000_000n;
  const wsolFundAtoms = 200_000_000n;
  const collateralDepositAtoms = 100_000_000n;

  beforeAll(async () => {
    bk = await bootBankrun({ loadMarginfiFixtures: true });
    admin = bk.payer;
    await setupGlobalConfig(bk, admin);
    market = await setupMarket(bk, admin);
    await unpauseMarket(bk, admin, market.publicKey);

    // ── Step 0: an unrelated lender (Alice) deposits USDC first ──
    // This pre-initialises the lender-side marginfi-account's USDC balance
    // slot so the P2Pool fallback's `marginfi.deposit` reuses it rather than
    // allocating a fresh slot. The Rust integration tests do the same
    // before `convert_p2pool_to_fixed` — without it, the later
    // `marginfi.lending_account_withdraw` from `lender_marginfi_account`
    // hits `OperationWithdrawOnly` (Custom 6020).
    const alice = await bk.fundedKeypair();
    const aliceUsdcAta = Keypair.generate().publicKey;
    await bk.putTokenAccount({
      address: aliceUsdcAta,
      mint: USDC_MINT,
      owner: alice.publicKey,
      amount: 100_000_000n,
    });
    await bk.send([claimSeatInstruction({ payer: alice.publicKey, market: market.publicKey })], [alice]);
    await bk.send(
      [
        depositInstruction({
          payer: alice.publicKey,
          market: market.publicKey,
          mint: USDC_MINT,
          debtMint: USDC_MINT,
          traderToken: aliceUsdcAta,
          tokenProgram: SPL_TOKEN_PROGRAM_ID,
          marginfiGroup: MARGINFI_GROUP,
          bank: USDC_BANK,
          liquidityVault: USDC_LIQUIDITY_VAULT,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          amountAtoms: 10_000_000n,
        }),
      ],
      [alice],
    );

    // ── Step 1: open a P2Pool loan ──
    borrower = await bk.fundedKeypair();
    const borrowerSolAta = Keypair.generate().publicKey;
    borrowerUsdcAta = Keypair.generate().publicKey;
    await bk.putWsolTokenAccount({
      address: borrowerSolAta,
      owner: borrower.publicKey,
      amount: wsolFundAtoms,
    });
    await bk.putTokenAccount({
      address: borrowerUsdcAta,
      mint: USDC_MINT,
      owner: borrower.publicKey,
      amount: 0n,
    });
    await bk.send([claimSeatInstruction({ payer: borrower.publicKey, market: market.publicKey })], [borrower]);
    await bk.send(
      [
        depositInstruction({
          payer: borrower.publicKey,
          market: market.publicKey,
          mint: WSOL_MINT,
          debtMint: USDC_MINT,
          traderToken: borrowerSolAta,
          tokenProgram: SPL_TOKEN_PROGRAM_ID,
          marginfiGroup: MARGINFI_GROUP,
          bank: SOL_BANK,
          liquidityVault: SOL_LIQUIDITY_VAULT,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          amountAtoms: collateralDepositAtoms,
        }),
      ],
      [borrower],
    );

    await bk.refreshOracleFreshness({ pythOracle: USDC_ORACLE, switchboardOracle: SOL_ORACLE });
    await bk.send(
      [
        cuBudgetIx(HEAVY_IX_CU_LIMIT),
        placeOrderInstruction({
          payer: borrower.publicKey,
          market: market.publicKey,
          debtMint: USDC_MINT,
          marginfiGroup: MARGINFI_GROUP,
          debtBank: USDC_BANK,
          collateralBank: SOL_BANK,
          debtOracles: [USDC_ORACLE],
          collateralOracles: [SOL_ORACLE],
          debtLiquidityVault: USDC_LIQUIDITY_VAULT,
          debtBankLiquidityVaultAuthority: bankLiquidityVaultAuthority(USDC_BANK),
          borrowerDebtToken: borrowerUsdcAta,
          tokenProgram: SPL_TOKEN_PROGRAM_ID,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          rateBps: 800,
          termSeconds: 30 * 86_400,
          principalAtoms,
          collateralAtoms,
        }),
      ],
      [borrower],
    );

    // Crank the P2Pool MatchedLoan → real Loan PDA.
    const m = decodeMarket((await bk.getAccount(market.publicKey))!.data);
    p2poolSequence = m.matchedLoans[0].loan.sequence;
    p2poolLoanKey = loanPda(market.publicKey, p2poolSequence)[0];
    p2poolCranker = await bk.fundedKeypair();
    await bk.send(
      [
        cuBudgetIx(HEAVY_IX_CU_LIMIT),
        processMatchedLoanInstruction({
          payer: p2poolCranker.publicKey,
          market: market.publicKey,
          debtBank: USDC_BANK,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          sequence: p2poolSequence,
        }),
      ],
      [p2poolCranker],
    );

    // ── Step 2: stand up the vault + risk profile + curator ask AFTER the P2Pool loan exists ──
    await setupVault(bk, admin);
    const curator = await bk.fundedKeypair();
    await setupRiskProfile(bk, admin, curator.publicKey, { maxLtvBps: 8_000 });

    const vaultDepositor = await bk.fundedKeypair();
    const vaultDepositorAta = Keypair.generate().publicKey;
    await bk.putTokenAccount({
      address: vaultDepositorAta,
      mint: USDC_MINT,
      owner: vaultDepositor.publicKey,
      amount: 10_000_000_000n, // 10k USDC headroom
    });
    await bk.send(
      [
        globalVaultDepositInstruction({
          depositor: vaultDepositor.publicKey,
          mint: USDC_MINT,
          depositorToken: vaultDepositorAta,
          tokenProgram: SPL_TOKEN_PROGRAM_ID,
          marginfiGroup: MARGINFI_GROUP,
          lendingPool: USDC_BANK,
          liquidityVault: USDC_LIQUIDITY_VAULT,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          profileId: 0,
          amountAtoms: 10_000_000_000n, // 10k USDC idle — covers the full P2Pool conversion
        }),
      ],
      [vaultDepositor],
    );
    await bk.send(
      [
        placeOrderForRiskProfileInstruction({
          feePayer: curator.publicKey,
          curator: curator.publicKey,
          mint: USDC_MINT,
          market: market.publicKey,
          profileId: 0,
          rateBps: 600,
          termSeconds: 30 * 86_400,
        }),
      ],
      [curator],
    );
  });

  it('ConvertP2PoolToFixed refinances the liability + closes the P2Pool PDA', async () => {
    const borrowerMa = borrowerIntegrationAccountPda(market.publicKey)[0];

    // ─── Pre-state ────────────────────────────────────────
    const preAcc = decodeMarginfiAccount((await bk.getAccount(borrowerMa))!.data);
    const preLiab = preAcc.balances.find((b) => b.active && b.bankPk.equals(USDC_BANK));
    if (!preLiab) throw new Error('precondition: borrower must have USDC liability before convert');
    expect(preLiab.liabilitySharesFp48).toBeGreaterThan(0n);

    const preMarket = decodeMarket((await bk.getAccount(market.publicKey))!.data);
    const preSeq = preMarket.header.matchedLoanSequence;
    // After step-1 (P2Pool place_order) the seq counter is 1 (one node consumed seq=0).
    expect(preSeq).toBe(1n);

    // ─── Convert ───────────────────────────────────────────
    await bk.refreshOracleFreshness({ pythOracle: USDC_ORACLE, switchboardOracle: SOL_ORACLE });
    await bk.send(
      [
        cuBudgetIx(HEAVY_IX_CU_LIMIT),
        convertP2poolToFixedInstruction({
          borrower: borrower.publicKey,
          market: market.publicKey,
          loanSequence: p2poolSequence,
          debtMint: USDC_MINT,
          debtBank: USDC_BANK,
          debtLiquidityVault: USDC_LIQUIDITY_VAULT,
          debtBankLiquidityVaultAuthority: bankLiquidityVaultAuthority(USDC_BANK),
          debtOracles: [USDC_ORACLE],
          collateralBank: SOL_BANK,
          collateralOracles: [SOL_ORACLE],
          tokenProgram: SPL_TOKEN_PROGRAM_ID,
          marginfiGroup: MARGINFI_GROUP,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          maxAcceptableRateBps: 1_000,
          crankerRefund: p2poolCranker.publicKey,
        }),
      ],
      [borrower],
    );

    // ─── Invariant 1: borrower's marginfi USDC liability == 0 ───
    // A FULL refinance uses `repay_all` CPI, so the live liability lands at
    // exactly zero. If the balance slot was deactivated, the find returns
    // undefined — also a valid "zero" outcome. Either way liability must
    // be 0; assert both code paths converge to that.
    const postAcc = decodeMarginfiAccount((await bk.getAccount(borrowerMa))!.data);
    const postLiab = postAcc.balances.find((b) => b.active && b.bankPk.equals(USDC_BANK));
    const postLiabShares = postLiab?.liabilitySharesFp48 ?? 0n;
    expect(postLiabShares).toBe(0n);

    // ─── Invariant 2: P2Pool PDA closed ───
    // `close_account_and_refund` either removes the account or zeroes its
    // data. The "closed" condition is `account is None OR data is all
    // zero AND lamports == 0`. Anything else is a bug — the PDA still
    // contains live loan state but the protocol thinks it's gone.
    const loanAcc = await bk.getAccount(p2poolLoanKey);
    const isClosed =
      loanAcc === null ||
      (loanAcc.data.every((b) => b === 0) && loanAcc.lamports === 0);
    expect(isClosed).toBe(true);

    // ─── Invariant 3: matched-loan sequence advanced exactly by # of crosses ───
    // One ask on book → exactly one cross → seq counter goes from 1 → 2.
    const postMarket = decodeMarket((await bk.getAccount(market.publicKey))!.data);
    expect(postMarket.header.matchedLoanSequence).toBe(preSeq + 1n);

    // ─── Invariant 4: exactly one Fixed MatchedLoan node in the queue ───
    // The converted node MUST be present — if the convert silently lost
    // it, the borrower walks away with neither variable debt NOR fixed
    // debt while the lender expects to be repaid. Catch that.
    expect(postMarket.matchedLoans).toHaveLength(1);
    const convertedNode = postMarket.matchedLoans[0].loan;
    expect(convertedNode.loanType).toBe(LoanType.Fixed);
    expect(convertedNode.lenderRateBps).toBe(600);
    expect(convertedNode.sequence).toBe(1n); // pre-counter value
    // Principal of the converted loan: the convert matcher refinances the
    // borrower's LIVE outstanding (post-accrual). Since no time elapsed
    // between place_order and convert, the live debt ≈ the original 1 USDC
    // = 1_000_000 atoms. Allow up to 100 atoms (0.01%) of share-rounding
    // slop on top of the live liability.
    expect(convertedNode.principalAtoms).toBeGreaterThanOrEqual(1_000_000n);
    expect(convertedNode.principalAtoms).toBeLessThanOrEqual(1_000_100n);
  });
});
