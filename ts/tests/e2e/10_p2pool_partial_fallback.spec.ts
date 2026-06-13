/**
 * Tier 7b e2e: P2Pool fallback — partial. Borrower bid exceeds vault idle,
 * so part of the bid crosses the curator's ask (Fixed match) and the
 * residual falls through to `marginfi.borrow` (P2Pool fallback).
 *
 *   1. Vault has only 50 atoms idle (`GlobalVaultDeposit(50)`).
 *   2. Curator rests an unbounded ask (rate quoted live by the program).
 *   3. Borrower bids 100 atoms above the quoted ask.
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
  placeOrderForSubVaultInstruction,
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
  setupPoolSubVault,
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
  // v1: vault ask rate is program-quoted (bank APR + spread), read back below.
  let askRateBps: number;
  let bidRateBps: number;

  beforeAll(async () => {
    bk = await bootBankrun({ loadMarginfiFixtures: true });
    admin = bk.payer;
    await setupGlobalConfig(bk, admin);
    market = await setupMarket(bk, admin);
    await setupVault(bk, admin);
    curator = await bk.fundedKeypair();
    await setupPoolSubVault(bk, admin, curator.publicKey, { maxLtvBps: 8_000 });

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
          subVaultId: 1,
          amountAtoms: vaultDepositAtoms,
        }),
      ],
      [depositor],
    );

    // Curator ask. v1: no rate/term — program quotes live (bank APR + spread).
    await bk.refreshOracleFreshness({ pythOracle: USDC_ORACLE, switchboardOracle: SOL_ORACLE });
    await bk.send(
      [
        placeOrderForSubVaultInstruction({
          feePayer: curator.publicKey,
          curator: curator.publicKey,
          market: market.publicKey,
          debtBank: USDC_BANK,
          marginfiGroup: MARGINFI_GROUP,
          collateralBank: SOL_BANK,
          debtOracles: [USDC_ORACLE],
          collateralOracles: [SOL_ORACLE],
          subVaultId: 1,
        }),
      ],
      [curator],
    );

    // Read back the program-quoted ask rate; the borrower bids above it.
    const askMarket = decodeMarket((await bk.getAccount(market.publicKey))!.data);
    askRateBps = askMarket.asks[0].order.rateBps;
    bidRateBps = askRateBps + 300;

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
          rateBps: bidRateBps,
          termSeconds: 30 * 86_400,
          principalAtoms: bidPrincipalAtoms,
          collateralAtoms: 5_000n,
          // residualMode defaults to 0 → P2Pool fallback ON (v1 D6).
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

    // Fixed leg consumes the vault's idle pool. Baseline is the GROSS
    // deposit; the credited idle is gross minus the deposit's share-rounding,
    // so allow 2 atoms (deposit-acknowledgment + match share-rounding).
    const fixedDrift =
      fixedNode!.principalAtoms > vaultDepositAtoms
        ? fixedNode!.principalAtoms - vaultDepositAtoms
        : vaultDepositAtoms - fixedNode!.principalAtoms;
    expect(fixedDrift).toBeLessThanOrEqual(2n);
    expect(fixedNode!.flags & MATCHED_LOAN_FLAG_VAULT_LENDER).toBe(MATCHED_LOAN_FLAG_VAULT_LENDER);
    expect(fixedNode!.lenderRateBps).toBe(askRateBps);

    // P2Pool leg takes the residual; same 2-atom band (the residual absorbs
    // the deposit-acknowledgment rounding the Fixed leg didn't fill).
    const expectedResidual = bidPrincipalAtoms - vaultDepositAtoms;
    const residualDrift =
      p2poolNode!.principalAtoms > expectedResidual
        ? p2poolNode!.principalAtoms - expectedResidual
        : expectedResidual - p2poolNode!.principalAtoms;
    expect(residualDrift).toBeLessThanOrEqual(2n);
    expect(p2poolNode!.flags & MATCHED_LOAN_FLAG_VAULT_LENDER).toBe(0);
    expect(fixedNode!.principalAtoms + p2poolNode!.principalAtoms).toBe(bidPrincipalAtoms);

    // Vault sub-vault encumbrance tracks the fixed leg amount actually filled.
    const [vaultPda] = globalVaultPda(USDC_BANK);
    const v = decodeGlobalVault((await bk.getAccount(vaultPda))!.data);
    const subVault = v.subVaults[0].subVault;
    expect(subVault.encumberedInOrdersAtoms).toBe(fixedNode!.principalAtoms);

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
    expect(drift).toBeLessThanOrEqual(2n);
  });
});
