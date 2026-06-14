/**
 * Tier 15 e2e: the TS LTV sizing math agrees with the on-chain gate to
 * the atom. We compute `requiredCollateralAtoms(...)` from live oracle
 * prices + bank weights, then drive two real `PlaceOrder` ixs:
 *
 *   1. `collateral = required` → must match (no `CollateralBelowMatchLTV`).
 *   2. `collateral = required − 1` → must fail with `CollateralBelowMatchLTV`
 *      (Custom 24).
 *
 * This is the bit-equivalence test for the on-chain LTV gate. If the
 * TS port drifts from `state/ltv.rs::required_collateral_at_ltv_cap`
 * (cap→weight conversion, decimal normalisation, ceil rounding), either
 * bound fails and the test surfaces the drift immediately.
 *
 * v1 D17: Fixed-loan origination gates on the sub-vault's explicit
 * `max_ltv_bps` cap — NOT on marginfi bank weights, and with NO
 * `ltv_buffer_bps` (the on-chain buffer field was removed entirely). The
 * sub-vault is configured with `max_ltv_bps = 8_000` (liq 9_000, a legal
 * ≥ MIN_LIQ_GAP_BPS gap) and the TS preflight passes that same cap.
 */
import { beforeAll, describe, expect, it } from 'vitest';
import { Keypair, PublicKey } from '@solana/web3.js';

import {
  claimSeatInstruction,
  cuBudgetIx,
  decodeBank,
  decodeMarket,
  depositInstruction,
  globalVaultDepositInstruction,
  HEAVY_IX_CU_LIMIT,
  placeOrderForSubVaultInstruction,
  placeOrderInstruction,
  readOraclePriceFp48,
  requiredCollateralAtoms,
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

const BORROW_ATOMS = 1_000_000n;
const TERM_SECONDS = 30 * 86_400;
/** Sub-vault origination LTV cap — the binding v1 gate (D17). */
const MAX_LTV_BPS = 8_000;
/** Liquidation cap; ≥ max_ltv + MIN_LIQ_GAP_BPS (200) and ≤ 10_000. */
const LIQ_LTV_BPS = 9_000;
/** Residual mode 2 = Drop — no P2Pool fallback, so the LTV gate isn't masked. */
const RESIDUAL_DROP = 2;

describe('e2e: TS `requiredCollateralAtoms` matches the on-chain LTV gate atom-for-atom', () => {
  let bk: BankrunHandle;
  let admin: Keypair;
  let market: Keypair;
  let requiredAtoms: bigint;
  // v1: vault ask rate is program-quoted (bank APR + spread), read back below.
  let bidRateBps: number;

  beforeAll(async () => {
    bk = await bootBankrun({ loadMarginfiFixtures: true });
    admin = bk.payer;
    await setupGlobalConfig(bk, admin);
    market = await setupMarket(bk, admin);
    await setupVault(bk, admin);
    const curator = await bk.fundedKeypair();
    // v1 D17: the sub-vault `max_ltv_bps` cap is the binding origination
    // gate we're verifying. liquidation_ltv must satisfy
    // max_ltv + MIN_LIQ_GAP_BPS (200) ≤ v ≤ 10_000.
    await setupPoolSubVault(bk, admin, curator.publicKey, {
      maxLtvBps: MAX_LTV_BPS,
      liquidationLtvBps: LIQ_LTV_BPS,
      maxTermSeconds: TERM_SECONDS,
    });

    // Fund the vault subVault.
    const depositor = await bk.fundedKeypair();
    const depositorAta = Keypair.generate().publicKey;
    await bk.putTokenAccount({
      address: depositorAta,
      mint: USDC_MINT,
      owner: depositor.publicKey,
      amount: 10_000_000_000n,
    });
    await bk.send(
      [
        globalVaultDepositInstruction({
          depositor: depositor.publicKey,
          mint: USDC_MINT,
          depositorToken: depositorAta,
          tokenProgram: SPL_TOKEN_PROGRAM_ID,
          marginfiGroup: MARGINFI_GROUP,
          lendingPool: USDC_BANK,
          liquidityVault: USDC_LIQUIDITY_VAULT,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          subVaultId: 1,
          amountAtoms: 10_000_000_000n,
        }),
      ],
      [depositor],
    );

    // Curator quote. v1: no rate/term — program quotes live (bank APR + spread).
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
    bidRateBps = askMarket.asks[0].order.rateBps + 300;

    // Make oracles fresh, then read prices + bank weights and compute
    // the on-chain-equivalent required collateral via TS.
    await bk.refreshOracleFreshness({ pythOracle: USDC_ORACLE, switchboardOracle: SOL_ORACLE });
    const usdcOracleAcc = await bk.getAccount(USDC_ORACLE);
    const solOracleAcc = await bk.getAccount(SOL_ORACLE);
    const debtBank = decodeBank((await bk.getAccount(USDC_BANK))!.data);
    const collBank = decodeBank((await bk.getAccount(SOL_BANK))!.data);

    const debtPriceFp48 = readOraclePriceFp48(debtBank.oracleSetup, usdcOracleAcc!.data);
    const collPriceFp48 = readOraclePriceFp48(collBank.oracleSetup, solOracleAcc!.data);

    requiredAtoms = requiredCollateralAtoms({
      borrowAtoms: BORROW_ATOMS,
      debtPriceFp48,
      collateralPriceFp48: collPriceFp48,
      // v1 D17: origination sizes against the sub-vault cap, not bank weights.
      maxLtvBps: MAX_LTV_BPS,
      debtMintDecimals: debtBank.mintDecimals,
      collateralMintDecimals: collBank.mintDecimals,
    });
    // Sanity: a meaningful, non-trivial required for a 1 USDC borrow
    // backed by SOL at ~$80. The actual number depends on the mainnet
    // bank weights and oracle prices baked into the fixtures.
    expect(requiredAtoms).toBeGreaterThan(1_000n);
    expect(requiredAtoms).toBeLessThan(100_000_000n);
  });

  it('Bid with collateral < required (by 1 atom) → ask is skipped, no MatchedLoan lands', async () => {
    const borrower = await bk.fundedKeypair();
    const solAta = Keypair.generate().publicKey;
    const usdcAta = Keypair.generate().publicKey;
    // Fund borrower with collateral one atom short, plus the same in
    // the actual seat deposit. We need the seat to carry < required
    // atoms — so deposit exactly `required - 1`.
    await bk.putWsolTokenAccount({
      address: solAta,
      owner: borrower.publicKey,
      amount: requiredAtoms - 1n + 10_000n, // wallet has a bit extra for the deposit
    });
    await bk.putTokenAccount({
      address: usdcAta,
      mint: USDC_MINT,
      owner: borrower.publicKey,
      amount: 0n,
    });
    await bk.send(
      [claimSeatInstruction({ payer: borrower.publicKey, market: market.publicKey })],
      [borrower],
    );
    await bk.send(
      [
        depositInstruction({
          payer: borrower.publicKey,
          market: market.publicKey,
          mint: WSOL_MINT,
          debtMint: USDC_MINT,
          traderToken: solAta,
          tokenProgram: SPL_TOKEN_PROGRAM_ID,
          marginfiGroup: MARGINFI_GROUP,
          bank: SOL_BANK,
          liquidityVault: SOL_LIQUIDITY_VAULT,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          amountAtoms: requiredAtoms - 1n,
        }),
      ],
      [borrower],
    );

    // v1 D17: the sub-vault `max_ltv_bps` cap is a per-ask SKIP, NOT a
    // hard error — when `collateral < required_collateral_at_ltv_cap`, the
    // match engine skips the ask (state/market_helpers.rs) and walks on. With
    // `residualMode = 2` (Drop) there is no P2Pool fallback to absorb the
    // residual, so a `required - 1` bid finds NO eligible ask, drops cleanly,
    // and lands ZERO matched loans. The tx itself succeeds. This is the exact
    // boundary that proves the TS `requiredCollateralAtoms` port: one atom
    // below the on-chain requirement, nothing crosses.
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
          borrowerDebtToken: usdcAta,
          tokenProgram: SPL_TOKEN_PROGRAM_ID,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          rateBps: bidRateBps,
          termSeconds: TERM_SECONDS,
          principalAtoms: BORROW_ATOMS,
          collateralAtoms: requiredAtoms - 1n,
          residualMode: RESIDUAL_DROP, // Drop — no P2Pool absorption
        }),
      ],
      [borrower],
    );

    // Nothing crossed: the LTV-skipped ask left the matched-loan queue empty.
    const m = decodeMarket((await bk.getAccount(market.publicKey))!.data);
    expect(m.matchedLoans).toHaveLength(0);
  });

  it('Bid with collateral = required (exact) → match crosses + MatchedLoan lands', async () => {
    const borrower = await bk.fundedKeypair();
    const solAta = Keypair.generate().publicKey;
    const usdcAta = Keypair.generate().publicKey;
    await bk.putWsolTokenAccount({
      address: solAta,
      owner: borrower.publicKey,
      amount: requiredAtoms + 10_000n,
    });
    await bk.putTokenAccount({
      address: usdcAta,
      mint: USDC_MINT,
      owner: borrower.publicKey,
      amount: 0n,
    });
    await bk.send(
      [claimSeatInstruction({ payer: borrower.publicKey, market: market.publicKey })],
      [borrower],
    );
    await bk.send(
      [
        depositInstruction({
          payer: borrower.publicKey,
          market: market.publicKey,
          mint: WSOL_MINT,
          debtMint: USDC_MINT,
          traderToken: solAta,
          tokenProgram: SPL_TOKEN_PROGRAM_ID,
          marginfiGroup: MARGINFI_GROUP,
          bank: SOL_BANK,
          liquidityVault: SOL_LIQUIDITY_VAULT,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          amountAtoms: requiredAtoms,
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
          borrowerDebtToken: usdcAta,
          tokenProgram: SPL_TOKEN_PROGRAM_ID,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          rateBps: bidRateBps,
          termSeconds: TERM_SECONDS,
          principalAtoms: BORROW_ATOMS,
          collateralAtoms: requiredAtoms,
          residualMode: RESIDUAL_DROP,
        }),
      ],
      [borrower],
    );

    // Match landed: MatchedLoan queue has one entry with our principal +
    // collateral exactly.
    const m = decodeMarket((await bk.getAccount(market.publicKey))!.data);
    expect(m.matchedLoans).toHaveLength(1);
    expect(m.matchedLoans[0].loan.principalAtoms).toBe(BORROW_ATOMS);
    expect(m.matchedLoans[0].loan.collateralAtoms).toBe(requiredAtoms);
  });

  void PublicKey;
});
