/**
 * Tier 12 e2e: interest accrual is precise.
 *
 *   - Open a Fixed loan (principal = 1 USDC = 1_000_000 atoms,
 *     borrower_rate = 800 bps, lender_rate = 500 bps).
 *   - Warp 30 days forward.
 *   - Trigger accrual via a partial Repay (10_001 atoms = just over the
 *     1% partial-repay floor).
 *   - Verify post-ix:
 *       outstanding ==  principal + borrower_interest − partial_repay
 *       lender_claimable == principal + lender_interest
 *       accumulated_protocol_fee_atoms == spread_interest  (= borrower − lender)
 *
 * Using simple-interest fixed-point math the on-chain code uses:
 *   interest = principal × rate_bps × elapsed / (10_000 × SECONDS_PER_YEAR)
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

const SECONDS_PER_YEAR = 31_536_000n;
const BPS_PER_UNIT = 10_000n;

describe('e2e: interest accrual lands at the exact simple-interest fp math', () => {
  let bk: BankrunHandle;
  let handles: MatchLandedHandles;
  let cranker: Keypair;
  let loanKey: PublicKey;
  /** Borrower rate stamped on the loan (= max(bid_rate, ask_rate + floor) ) */
  let borrowerRateBps: bigint;
  /** Lender rate stamped on the loan (= ask_rate). */
  let lenderRateBps: bigint;
  let principalAtoms: bigint;
  let lastAccruedAtPromote: bigint;

  beforeAll(async () => {
    bk = await bootBankrun({ loadMarginfiFixtures: true });
    handles = await driveToMatchLanded(bk, {
      principalAtoms: 1_000_000n,
      collateralAtoms: 50_000_000n,
      vaultDepositAtoms: 1_000_000_000n,
      wsolFundAtoms: 200_000_000n,
      collateralDepositAtoms: 100_000_000n,
    });
    cranker = await bk.fundedKeypair();
    loanKey = loanPda(handles.market.publicKey, handles.matchedLoanSequence)[0];

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

    // Top up the borrower's USDC ATA so they can pay the partial repay.
    await bk.putTokenAccount({
      address: handles.borrowerUsdcAta,
      mint: USDC_MINT,
      owner: handles.borrower.publicKey,
      amount: 10_000_000_000n,
    });

    const promoted = decodeLoanFixed((await bk.getAccount(loanKey))!.data);
    borrowerRateBps = BigInt(promoted.borrowerRateBps);
    lenderRateBps = BigInt(promoted.lenderRateBps);
    principalAtoms = promoted.principalDebtAtoms;
    lastAccruedAtPromote = promoted.lastAccruedUnix;
    // Confirm the locked-in rates match what we asked for in setup.
    expect(borrowerRateBps).toBe(800n);
    expect(lenderRateBps).toBe(500n);
    expect(principalAtoms).toBe(1_000_000n);
    // Pre-accrual baseline: outstanding == lender_claimable == principal.
    expect(promoted.outstandingDebtAtoms).toBe(principalAtoms);
    expect(promoted.lenderClaimableAtoms).toBe(principalAtoms);
    expect(promoted.accumulatedProtocolFeeAtoms).toBe(0n);
  });

  it('30-day warp + partial repay: outstanding, lender_claimable, and protocol-fee accumulator hit the exact formula', async () => {
    const warpSeconds = 30n * 86_400n;
    await bk.warpForward(warpSeconds);

    // Snapshot the new clock so we can compute elapsed precisely.
    const clock = await bk.client.getClock();
    const elapsed = clock.unixTimestamp - lastAccruedAtPromote;
    // Sanity: bankrun's setClock writes the exact value we asked for.
    expect(elapsed).toBe(warpSeconds);

    // The on-chain `accrue_loan` does NOT compute borrower interest as a
    // single floor of `borrower_rate × elapsed`. It floors the lender_gross
    // numerator and the spread numerator INDEPENDENTLY, then sums them.
    // That guarantees the conservation identity
    //   borrower_interest = lender_gross + spread
    // holds bit-exactly on every call (no per-segment rounding drift). The
    // borrower side may differ from a single-floor calculation by AT MOST
    // one atom — see the on-chain comment in `accrue_loan`.
    const denom = BPS_PER_UNIT * SECONDS_PER_YEAR;
    const expectedLenderInterest = (principalAtoms * lenderRateBps * elapsed) / denom;
    const spreadBps = borrowerRateBps - lenderRateBps;
    const expectedSpread = (principalAtoms * spreadBps * elapsed) / denom;
    const expectedBorrowerInterest = expectedLenderInterest + expectedSpread;

    // Trigger accrual via a partial repay. Repay amount must clear the
    // partial-repay floor of `max(1% outstanding, 1000)`; 100k > 10k.
    const partialRepay = 100_000n;
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
          repayAtoms: partialRepay,
          crankerRefund: cranker.publicKey,
        }),
      ],
      [handles.borrower],
    );

    const post = decodeLoanFixed((await bk.getAccount(loanKey))!.data);

    // outstanding_debt_atoms = principal + borrower_interest − partial_repay
    expect(post.outstandingDebtAtoms).toBe(principalAtoms + expectedBorrowerInterest - partialRepay);

    // lender_claimable_atoms = principal + lender_interest (repay credits
    // the lender via a separate path; partial repay reduces outstanding
    // but the lender's claim base + accrued is independent until the
    // cranker realises it).
    expect(post.lenderClaimableAtoms).toBe(principalAtoms + expectedLenderInterest);

    // accumulated_protocol_fee_atoms == spread interest. With our zero
    // `protocol_fee_bps_floor` setting, the entire (borrower − lender)
    // spread accrues to the protocol fee bucket on the loan body.
    expect(post.accumulatedProtocolFeeAtoms).toBe(expectedSpread);

    // Loan stays Active — outstanding > 0 after the partial.
    expect(post.state).toBe(LoanState.Active);
    expect(post.outstandingDebtAtoms).toBeGreaterThan(0n);

    // last_accrued_unix advanced to `now`.
    expect(post.lastAccruedUnix).toBe(clock.unixTimestamp);
  });
});
