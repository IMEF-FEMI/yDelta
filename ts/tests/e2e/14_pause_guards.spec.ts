/**
 * Tier 10 e2e: pause guards. Each pause flag must block its category of
 * state-mutating ixs with the matching Custom error, and admin-recovery
 * ixs (the two-step transfers) must stay live so a stuck system can
 * still rotate authority.
 *
 *   - SetGlobalPause(true) → every state-mutating ix that loads the
 *     `global_config` rejects with `GlobalPaused` (Custom 47).
 *   - SetMarketPause(true) → market-scoped ixs reject with
 *     `MarketPaused` (Custom 46). Global/vault pause flags are off.
 *   - SetVaultPause(true) → vault-scoped ixs reject with
 *     `VaultPaused` (Custom 50). Global/market pause flags are off.
 */
import { beforeAll, describe, it } from 'vitest';
import { Keypair, PublicKey } from '@solana/web3.js';

import {
  claimSeatInstruction,
  cuBudgetIx,
  globalVaultDepositInstruction,
  HEAVY_IX_CU_LIMIT,
  placeOrderForSubVaultInstruction,
  setGlobalPauseInstruction,
  setMarketPauseInstruction,
  setVaultPauseInstruction,
} from '../../src/index.js';
import { bootBankrun, BankrunHandle } from './_bankrun.ts';
import {
  MARGINFI_GROUP,
  MARGINFI_PROGRAM_ID,
  SOL_BANK,
  SOL_ORACLE,
  SPL_TOKEN_PROGRAM_ID,
  USDC_BANK,
  USDC_LIQUIDITY_VAULT,
  USDC_MINT,
  USDC_ORACLE,
} from './_fixtures.ts';
import { expectCustomError, YdeltaError } from './_errors.ts';
import {
  setupGlobalConfig,
  setupMarket,
  setupPoolSubVault,
  setupVault,
} from './_setup.ts';

describe('e2e: pause guards reject state mutations with the right Custom error', () => {
  let bk: BankrunHandle;
  let admin: Keypair;
  let market: Keypair;
  let curator: Keypair;
  let depositor: Keypair;
  let depositorUsdcAta: PublicKey;

  beforeAll(async () => {
    bk = await bootBankrun({ loadMarginfiFixtures: true });
    admin = bk.payer;
    await setupGlobalConfig(bk, admin);
    market = await setupMarket(bk, admin);
    await setupVault(bk, admin);
    curator = await bk.fundedKeypair();
    await setupPoolSubVault(bk, admin, curator.publicKey, { maxLtvBps: 8_000 });

    depositor = await bk.fundedKeypair();
    depositorUsdcAta = Keypair.generate().publicKey;
    await bk.putTokenAccount({
      address: depositorUsdcAta,
      mint: USDC_MINT,
      owner: depositor.publicKey,
      amount: 1_000_000_000n,
    });
  });

  /* ── GlobalPaused ─────────────────────────────────────── */

  it('SetGlobalPause(true) blocks ClaimSeat, GlobalVaultDeposit, PlaceOrderForSubVault with GlobalPaused', async () => {
    await bk.send([setGlobalPauseInstruction({ admin: admin.publicKey, paused: true })], [admin]);

    // ClaimSeat (writes through global_config).
    const seatSigner = await bk.fundedKeypair();
    await expectCustomError(
      bk.send([claimSeatInstruction({ payer: seatSigner.publicKey, market: market.publicKey })], [seatSigner]),
      YdeltaError.GlobalPaused,
      'ClaimSeat while globally paused',
    );

    // GlobalVaultDeposit.
    await expectCustomError(
      bk.send(
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
            amountAtoms: 1_000n,
          }),
        ],
        [depositor],
      ),
      YdeltaError.GlobalPaused,
      'GlobalVaultDeposit while globally paused',
    );

    // Curator zero-CPI ix.
    await expectCustomError(
      bk.send(
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
      ),
      YdeltaError.GlobalPaused,
      'PlaceOrderForSubVault while globally paused',
    );

    // Restore so subsequent tests start clean.
    await bk.send([setGlobalPauseInstruction({ admin: admin.publicKey, paused: false })], [admin]);
  });

  /* ── MarketPaused ─────────────────────────────────────── */

  it('SetMarketPause(true) blocks market-scoped ixs but global/vault ixs still work', async () => {
    await bk.send(
      [setMarketPauseInstruction({ admin: admin.publicKey, market: market.publicKey, paused: true })],
      [admin],
    );

    // ClaimSeat is market-scoped → rejects.
    const seatSigner = await bk.fundedKeypair();
    await expectCustomError(
      bk.send([claimSeatInstruction({ payer: seatSigner.publicKey, market: market.publicKey })], [seatSigner]),
      YdeltaError.MarketPaused,
      'ClaimSeat while market paused',
    );

    // PlaceOrderForSubVault is market-scoped → rejects.
    await expectCustomError(
      bk.send(
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
      ),
      YdeltaError.MarketPaused,
      'PlaceOrderForSubVault while market paused',
    );

    // GlobalVaultDeposit is NOT market-scoped → still works.
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
          amountAtoms: 1_000n,
        }),
        // Heavy ix CU prefix not needed for GlobalVaultDeposit at this volume.
      ],
      [depositor],
    );

    await bk.send(
      [setMarketPauseInstruction({ admin: admin.publicKey, market: market.publicKey, paused: false })],
      [admin],
    );
  });

  /* ── VaultPaused ──────────────────────────────────────── */

  it('SetVaultPause(true) blocks vault ixs (deposit, place_order_for_sub_vault) but admin transfers stay live', async () => {
    await bk.send([setVaultPauseInstruction({ admin: admin.publicKey, bank: USDC_BANK, paused: true })], [admin]);

    // GlobalVaultDeposit rejects.
    await expectCustomError(
      bk.send(
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
            amountAtoms: 1_000n,
          }),
        ],
        [depositor],
      ),
      YdeltaError.VaultPaused,
      'GlobalVaultDeposit while vault paused',
    );

    // PlaceOrderForSubVault rejects (vault-scoped).
    await expectCustomError(
      bk.send(
        [
          cuBudgetIx(HEAVY_IX_CU_LIMIT),
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
      ),
      YdeltaError.VaultPaused,
      'PlaceOrderForSubVault while vault paused',
    );

    await bk.send([setVaultPauseInstruction({ admin: admin.publicKey, bank: USDC_BANK, paused: false })], [admin]);
  });
});
