/**
 * Tier 7b e2e: P2Pool fallback — partial. Borrower bid exceeds vault idle,
 * so part of the bid crosses the curator's ask (Fixed match) and the
 * residual falls through to `marginfi.borrow` (P2Pool fallback).
 *
 *   1. Vault has only 50 atoms idle (`GlobalVaultDeposit(50)`).
 *   2. Curator quotes an unbounded ask at 500 bps.
 *   3. Borrower bids 100 atoms at 800 bps.
 *      - 50 atoms cross against the vault → Fixed MatchedLoan node.
 *      - 50 atoms residual → marginfi.borrow → P2Pool MatchedLoan node.
 *   4. Market matched_loans has **two** entries with distinct loan_type.
 */
import { beforeAll, describe, expect, it } from 'vitest';
import { Keypair, PublicKey } from '@solana/web3.js';

import {
  borrowerIntegrationAccountPda,
  claimSeatInstruction,
  cuBudgetIx,
  decodeBank,
  decodeGlobalVault,
  decodeMarginfiAccount,
  decodeMarket,
  depositInstruction,
  FP48_SHIFT,
  globalVaultDepositInstruction,
  globalVaultPda,
  HEAVY_IX_CU_LIMIT,
  LoanType,
  MATCHED_LOAN_FLAG_VAULT_LENDER,
  placeOrderForRiskProfileInstruction,
  placeOrderInstruction,
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
} from './_setup.ts';

describe('e2e: P2Pool partial fallback (fixed match + marginfi residual)', () => {
  let bk: BankrunHandle;
  let admin: Keypair;
  let market: Keypair;
  let curator: Keypair;
  let depositor: Keypair;
  let depositorUsdcAta: PublicKey;
  let borrower: Keypair;
  let borrowerSolAta: PublicKey;
  let borrowerUsdcAta: PublicKey;
  const vaultDepositAtoms = 50n; // tiny so the bid eats it all
  const bidPrincipalAtoms = 100n; // bigger than vault → 50/50 split

  beforeAll(async () => {
    bk = await bootBankrun({ loadMarginfiFixtures: true });
    admin = bk.payer;
    await setupGlobalConfig(bk, admin);
    market = await setupMarket(bk, admin);
    await setupVault(bk, admin);
    curator = await bk.fundedKeypair();
    await setupRiskProfile(bk, admin, curator.publicKey, { maxLtvBps: 8_000 });

    // Tiny vault deposit so partial residual is forced.
    depositor = await bk.fundedKeypair();
    depositorUsdcAta = Keypair.generate().publicKey;
    await bk.putTokenAccount({
      address: depositorUsdcAta,
      mint: USDC_MINT,
      owner: depositor.publicKey,
      amount: 1_000_000n,
    });
    await bk.send(
      [
        globalVaultDepositInstruction({
          depositor: depositor.publicKey,
          mint: USDC_MINT,
          depositorToken: depositorUsdcAta,
          tokenProgram: SPL_TOKEN_PROGRAM_ID,
          marginfiGroup: MARGINFI_GROUP,
          lendingPool: USDC_BANK,
          liquidityVault: USDC_LIQUIDITY_VAULT,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          profileId: 0,
          amountAtoms: vaultDepositAtoms,
        }),
      ],
      [depositor],
    );

    // Curator ask.
    await bk.send(
      [
        placeOrderForRiskProfileInstruction({
          feePayer: curator.publicKey,
          curator: curator.publicKey,
          mint: USDC_MINT,
          market: market.publicKey,
          profileId: 0,
          rateBps: 500,
          termSeconds: 30 * 86_400,
        }),
      ],
      [curator],
    );

    // Borrower: seat + collateral.
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

  it('100-atom bid splits 50 Fixed + 50 P2Pool, surfaces both queue nodes', async () => {
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
          principalAtoms: bidPrincipalAtoms,
          collateralAtoms: 5_000n,
          // flags = 0 → fallback ON
        }),
      ],
      [borrower],
    );

    // Two MatchedLoan nodes — one Fixed, one P2Pool.
    const m = decodeMarket((await bk.getAccount(market.publicKey))!.data);
    expect(m.matchedLoans).toHaveLength(2);

    const fixedNode = m.matchedLoans.find((x) => x.loan.loanType === LoanType.Fixed)?.loan;
    const p2poolNode = m.matchedLoans.find((x) => x.loan.loanType === LoanType.P2Pool)?.loan;
    expect(fixedNode).toBeDefined();
    expect(p2poolNode).toBeDefined();

    // Fixed leg should consume the vault's idle pool within the usual
    // 1-atom marginfi share-rounding tolerance.
    const fixedDrift =
      fixedNode!.principalAtoms > vaultDepositAtoms
        ? fixedNode!.principalAtoms - vaultDepositAtoms
        : vaultDepositAtoms - fixedNode!.principalAtoms;
    expect(fixedDrift).toBeLessThanOrEqual(1n);
    expect(fixedNode!.flags & MATCHED_LOAN_FLAG_VAULT_LENDER).toBe(MATCHED_LOAN_FLAG_VAULT_LENDER);
    expect(fixedNode!.lenderRateBps).toBe(500);

    // P2Pool leg should take the residual, again allowing the same
    // 1-atom drift band from share math.
    const expectedResidual = bidPrincipalAtoms - vaultDepositAtoms;
    const residualDrift =
      p2poolNode!.principalAtoms > expectedResidual
        ? p2poolNode!.principalAtoms - expectedResidual
        : expectedResidual - p2poolNode!.principalAtoms;
    expect(residualDrift).toBeLessThanOrEqual(1n);
    expect(p2poolNode!.flags & MATCHED_LOAN_FLAG_VAULT_LENDER).toBe(0);
    expect(fixedNode!.principalAtoms + p2poolNode!.principalAtoms).toBe(bidPrincipalAtoms);

    // Vault profile encumbrance tracks the fixed leg amount actually filled.
    const [vaultPda] = globalVaultPda(USDC_MINT);
    const v = decodeGlobalVault((await bk.getAccount(vaultPda))!.data);
    const profile = v.riskProfiles[0].profile;
    expect(profile.encumberedInOrdersAtoms).toBe(fixedNode!.principalAtoms);

    // Borrower marginfi: only the P2Pool half lives as a marginfi liability.
    // The liability MUST reflect exactly the residual (bid - vault_idle = 50
    // atoms), NOT the full bid principal (100).
    const borrowerMa = borrowerIntegrationAccountPda(market.publicKey)[0];
    const acc = decodeMarginfiAccount((await bk.getAccount(borrowerMa))!.data);
    const usdcLiability = acc.balances.find((b) => b.active && b.bankPk.equals(USDC_BANK));
    if (!usdcLiability) throw new Error('borrower marginfi must carry the P2Pool residual liability');

    const bank = decodeBank((await bk.getAccount(USDC_BANK))!.data);
    const liabilityAtoms =
      (usdcLiability.liabilitySharesFp48 * bank.liabilityShareValueFp48) >> (FP48_SHIFT * 2n);
    const residualPrincipal = bidPrincipalAtoms - vaultDepositAtoms; // 50
    const drift =
      liabilityAtoms > residualPrincipal ? liabilityAtoms - residualPrincipal : residualPrincipal - liabilityAtoms;
    expect(drift).toBeLessThanOrEqual(1n);
  });
});
