/**
 * Tier 7 e2e: P2Pool fallback — full coverage.
 *
 * UX: borrower bids into a market with NO vault liquidity (no curator asks
 * exist). With `flags = 0` (fallback ON), the IOC bid finds nothing to
 * cross and falls back to `marginfi.borrow` on the borrower-side marginfi
 * account for the full residual.
 *
 *   1. Global config + market (no vault / no profile).
 *   2. Borrower: claim seat + deposit wSOL collateral.
 *   3. Borrower: PlaceOrder with `flags = 0`, no asks on book.
 *      → marginfi.borrow fires → atoms land on `market.lender_integration`,
 *        a P2Pool MatchedLoan queue node is created, borrower's marginfi
 *        account picks up `liability_shares > 0`.
 *   4. Cranker: ProcessMatchedLoan promotes the P2Pool node to a Loan PDA
 *      (no vault-settle block needed — there is no vault lender).
 *
 * Bid amounts mirror the Rust `p2pool.rs` integration test recipe.
 */
import { beforeAll, describe, expect, it } from 'vitest';
import { Keypair, PublicKey } from '@solana/web3.js';

import {
  borrowerIntegrationAccountPda,
  claimSeatInstruction,
  cuBudgetIx,
  decodeBank,
  decodeLoanFixed,
  decodeMarginfiAccount,
  decodeMarket,
  depositInstruction,
  FP48_SHIFT,
  HEAVY_IX_CU_LIMIT,
  LoanState,
  LoanType,
  loanPda,
  MATCHED_LOAN_FLAG_VAULT_LENDER,
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
  unpauseMarket,
} from './_setup.ts';

describe('e2e: P2Pool full fallback (empty book, fallback on)', () => {
  let bk: BankrunHandle;
  let admin: Keypair;
  let market: Keypair;
  let borrower: Keypair;
  let borrowerSolAta: PublicKey;
  let borrowerUsdcAta: PublicKey;
  const principalAtoms = 100n;
  const collateralAtoms = 5_000n;

  beforeAll(async () => {
    bk = await bootBankrun({ loadMarginfiFixtures: true });
    admin = bk.payer;
    await setupGlobalConfig(bk, admin);
    market = await setupMarket(bk, admin);
    await unpauseMarket(bk, admin, market.publicKey);

    borrower = await bk.fundedKeypair();
    borrowerSolAta = Keypair.generate().publicKey;
    borrowerUsdcAta = Keypair.generate().publicKey;
    await bk.putWsolTokenAccount({
      address: borrowerSolAta,
      owner: borrower.publicKey,
      amount: 100_000n,
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
          amountAtoms: 10_000n,
        }),
      ],
      [borrower],
    );
  });

  it('PlaceOrder with empty book + fallback ON queues a P2Pool MatchedLoan + opens marginfi liability', async () => {
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
          // flags omitted → 0 → fallback ON
        }),
      ],
      [borrower],
    );

    // One MatchedLoan node landed — for the full fallback amount.
    const m = decodeMarket((await bk.getAccount(market.publicKey))!.data);
    expect(m.matchedLoans).toHaveLength(1);
    const ml = m.matchedLoans[0].loan;
    expect(ml.loanType).toBe(LoanType.P2Pool);
    expect(ml.flags & MATCHED_LOAN_FLAG_VAULT_LENDER).toBe(0); // no vault lender
    expect(ml.principalAtoms).toBe(principalAtoms);
    expect(ml.collateralAtoms).toBe(collateralAtoms);
    // P2Pool variable rate: borrower's effective rate is the live marginfi
    // borrow APR, not a stamped value. The match-time rates may be 0 here
    // (P2Pool path doesn't lock a fixed rate).

    // Borrower-side marginfi account has the new liability. Tighten:
    // `liability_shares × liability_share_value >> 48` should equal the
    // principal atoms borrowed (within 1 atom of marginfi share-rounding).
    const borrowerMa = borrowerIntegrationAccountPda(market.publicKey)[0];
    const marginfiAcc = decodeMarginfiAccount((await bk.getAccount(borrowerMa))!.data);
    const usdcLiability = marginfiAcc.balances.find((b) => b.active && b.bankPk.equals(USDC_BANK));
    if (!usdcLiability) throw new Error('borrower marginfi must carry the new USDC liability');

    const bank = decodeBank((await bk.getAccount(USDC_BANK))!.data);
    const liabilityAtoms =
      (usdcLiability.liabilitySharesFp48 * bank.liabilityShareValueFp48) >> (FP48_SHIFT * 2n);
    // Marginfi rounds at multiple points (atom→share on borrow,
    // share→atom on the back-computation). Net drift across both rounds
    // is bounded to ±1 atom; anything bigger is a real share-math drift
    // bug — e.g. wrong share_value snapshot or off-by-orders-of-magnitude
    // scaling.
    const drift =
      liabilityAtoms > principalAtoms ? liabilityAtoms - principalAtoms : principalAtoms - liabilityAtoms;
    expect(drift).toBeLessThanOrEqual(1n);
  });

  it('ProcessMatchedLoan promotes the P2Pool node to a LoanType.P2Pool PDA (no vault-settle)', async () => {
    const m = decodeMarket((await bk.getAccount(market.publicKey))!.data);
    const sequence = m.matchedLoans[0].loan.sequence;
    const cranker = await bk.fundedKeypair();
    const [loanKey] = loanPda(market.publicKey, sequence);

    await bk.send(
      [
        cuBudgetIx(HEAVY_IX_CU_LIMIT),
        processMatchedLoanInstruction({
          payer: cranker.publicKey,
          market: market.publicKey,
          debtBank: USDC_BANK,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          sequence,
          // No `vaultSettle` — lender is not a vault for P2Pool loans.
        }),
      ],
      [cranker],
    );

    const loan = decodeLoanFixed((await bk.getAccount(loanKey))!.data);
    expect(loan.loanType).toBe(LoanType.P2Pool);
    expect(loan.state).toBe(LoanState.Active);
    expect(loan.principalDebtAtoms).toBe(principalAtoms);
    expect(loan.collateralAtoms).toBe(collateralAtoms);
    expect(loan.createdBy.equals(cranker.publicKey)).toBe(true);
    // P2Pool loans carry the marginfi borrow_shares snapshot from match time.
    expect(loan.borrowerMarginfiBorrowShares).toBeGreaterThan(0n);
  });
});
