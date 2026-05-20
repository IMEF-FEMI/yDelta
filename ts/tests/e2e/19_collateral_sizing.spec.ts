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
 * TS port drifts from `state/ltv.rs::get_required_quote_collateral_to_back_debt`
 * (decimal normalisation, ceil rounding, buffer math, weight handling),
 * either bound fails and the test surfaces the drift immediately.
 *
 * Profile is configured with `max_ltv_bps = 9_999` so the bank-weight
 * gate is binding (not the profile-LTV gate). `ltv_buffer_bps = 0` (the
 * default in `unpauseMarket`) so the buffer doesn't enter the math.
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
  placeOrderForRiskProfileInstruction,
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
import { expectCustomError, YdeltaError } from './_errors.ts';
import {
  bankLiquidityVaultAuthority,
  setupGlobalConfig,
  setupMarket,
  setupRiskProfile,
  setupVault,
  unpauseMarket,
} from './_setup.ts';

const BORROW_ATOMS = 1_000_000n;
const ASK_RATE_BPS = 500;
const BID_RATE_BPS = 800;
const TERM_SECONDS = 30 * 86_400;

describe('e2e: TS `requiredCollateralAtoms` matches the on-chain LTV gate atom-for-atom', () => {
  let bk: BankrunHandle;
  let admin: Keypair;
  let market: Keypair;
  let requiredAtoms: bigint;

  beforeAll(async () => {
    bk = await bootBankrun({ loadMarginfiFixtures: true });
    admin = bk.payer;
    await setupGlobalConfig(bk, admin);
    market = await setupMarket(bk, admin);
    await unpauseMarket(bk, admin, market.publicKey); // ltv_buffer_bps = 0
    await setupVault(bk, admin);
    const curator = await bk.fundedKeypair();
    // max_ltv = 9_999 so the profile gate is wide-open and the
    // **bank-weight** gate is the binding constraint we're verifying.
    await setupRiskProfile(bk, admin, curator.publicKey, {
      maxLtvBps: 9_999,
      maxTermSeconds: TERM_SECONDS,
    });

    // Fund the vault profile.
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
          profileId: 0,
          amountAtoms: 10_000_000_000n,
        }),
      ],
      [depositor],
    );

    // Curator quote.
    await bk.send(
      [
        placeOrderForRiskProfileInstruction({
          feePayer: curator.publicKey,
          curator: curator.publicKey,
          mint: USDC_MINT,
          market: market.publicKey,
          profileId: 0,
          rateBps: ASK_RATE_BPS,
          termSeconds: TERM_SECONDS,
        }),
      ],
      [curator],
    );

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
      liabilityWeightInitFp48: debtBank.liabilityWeightInitFp48,
      collateralAssetWeightInitFp48: collBank.assetWeightInitFp48,
      ltvBufferBps: 0,
      debtMintDecimals: debtBank.mintDecimals,
      collateralMintDecimals: collBank.mintDecimals,
    });
    // Sanity: a meaningful, non-trivial required for a 1 USDC borrow
    // backed by SOL at ~$80. The actual number depends on the mainnet
    // bank weights and oracle prices baked into the fixtures.
    expect(requiredAtoms).toBeGreaterThan(1_000n);
    expect(requiredAtoms).toBeLessThan(100_000_000n);
  });

  it('Bid with collateral < required (by 1 atom) → CollateralBelowMatchLTV (Custom 24)', async () => {
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

    // Bid with `collateral = required - 1` on the IOC must error with
    // `CollateralBelowMatchLTV`. Use `flags = FLAG_OB_ONLY = 2` so the
    // P2Pool fallback doesn't mask the LTV rejection by absorbing the
    // residual into a marginfi.borrow.
    await bk.refreshOracleFreshness({ pythOracle: USDC_ORACLE, switchboardOracle: SOL_ORACLE });
    await expectCustomError(
      bk.send(
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
            rateBps: BID_RATE_BPS,
            termSeconds: TERM_SECONDS,
            principalAtoms: BORROW_ATOMS,
            collateralAtoms: requiredAtoms - 1n,
            flags: 0b10, // OB_ONLY — no P2Pool absorption
          }),
        ],
        [borrower],
      ),
      24, // YdeltaError::CollateralBelowMatchLTV
      `bid with collateral = required − 1 (${requiredAtoms - 1n})`,
    );
    void YdeltaError;
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
          rateBps: BID_RATE_BPS,
          termSeconds: TERM_SECONDS,
          principalAtoms: BORROW_ATOMS,
          collateralAtoms: requiredAtoms,
          flags: 0b10,
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
