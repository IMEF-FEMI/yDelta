/**
 * Tier 3a e2e: curator flow on the lender side.
 *
 *   1. GlobalVaultDeposit — depositor funds sub-vault 1's idle pool.
 *      (Verifies subVault.total_principal_atoms / total_shares grow.)
 *   2. PlaceOrderForSubVault — curator rests an unbounded ask; the program
 *      quotes the rate live (bank lending APR + sub_vault.spread_bps).
 *      (Verifies the asks RB-tree has one resting order at that rate.)
 *   3. UpdateOrderForSubVault — cancel-and-replace (re-quoted live).
 *   4. CancelOrderForSubVault — tree returns to empty.
 *
 * Builds on the Tier 2 setup (market + vault + sub-vault). A pre-funded
 * USDC ATA is synthesised for the depositor since the loaded USDC mint is
 * the real mainnet one and we can't mint to it directly.
 */
import { beforeAll, describe, expect, it } from 'vitest';
import { Keypair, PublicKey } from '@solana/web3.js';

import {
  cancelOrderForSubVaultInstruction,
  decodeGlobalVault,
  decodeMarket,
  globalVaultPda,
  globalVaultDepositInstruction,
  placeOrderForSubVaultInstruction,
  updateOrderForSubVaultInstruction,
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
import {
  setupGlobalConfig,
  setupMarket,
  setupPoolSubVault,
  setupVault,
} from './_setup.ts';

describe('e2e: curator flow', () => {
  let bk: BankrunHandle;
  let admin: Keypair;
  let market: Keypair;
  let curator: Keypair;
  let depositor: Keypair;
  let depositorAta: PublicKey;

  beforeAll(async () => {
    bk = await bootBankrun({ loadMarginfiFixtures: true });
    admin = bk.payer;
    await setupGlobalConfig(bk, admin);
    market = await setupMarket(bk, admin);
    await setupVault(bk, admin);

    curator = await bk.fundedKeypair();
    await setupPoolSubVault(bk, admin, curator.publicKey);

    depositor = await bk.fundedKeypair();
    // Synthesise a USDC token account for the depositor with 1_000_000 USDC (6 dp = 1e12 atoms).
    depositorAta = Keypair.generate().publicKey;
    await bk.putTokenAccount({
      address: depositorAta,
      mint: USDC_MINT,
      owner: depositor.publicKey,
      amount: 1_000_000_000_000n,
    });
  });

  it('GlobalVaultDeposit credits subVault.total_principal + total_shares', async () => {
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
          amountAtoms: 500_000_000_000n, // 500_000 USDC
        }),
      ],
      [depositor],
    );

    // Genesis-deposit invariant: the vault credits the marginfi-ACKNOWLEDGED
    // atoms (gross minus sub-atom share-rounding on the deposit CPI), and at
    // genesis (`total_shares == 0`) mints shares 1:1 against that. So
    // principal == shares == acknowledged ≈ gross (within a couple atoms of
    // marginfi share-rounding).
    const GROSS = 500_000_000_000n; // 500_000 USDC
    const [vaultPda] = globalVaultPda(USDC_BANK);
    const vault = decodeGlobalVault((await bk.getAccount(vaultPda))!.data);
    const subVault = vault.subVaults[0].subVault;
    expect(subVault.totalPrincipalAtoms).toBeLessThanOrEqual(GROSS);
    expect(subVault.totalPrincipalAtoms).toBeGreaterThan(GROSS - 4n);
    expect(subVault.totalShares).toBe(subVault.totalPrincipalAtoms); // 1:1 at genesis
    expect(subVault.deployedPrincipalAtoms).toBe(0n);
    expect(subVault.encumberedInOrdersAtoms).toBe(0n);
  });

  it('PlaceOrderForSubVault inserts a resting ask + a vault order ref', async () => {
    // v1 D4: no rate/term args — the program quotes live (bank lending APR +
    // sub_vault.spread_bps) and uses sub_vault.max_term_seconds, so it reads
    // the bank + oracles.
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

    const marketDecoded = decodeMarket((await bk.getAccount(market.publicKey))!.data);
    expect(marketDecoded.asks).toHaveLength(1);
    // Program-quoted rate (bank APR + spread 0). The vault order ref mirrors
    // the resting ask, so the two must agree.
    const quotedRateBps = marketDecoded.asks[0].order.rateBps;
    expect(quotedRateBps).toBeGreaterThan(0);
    // setupPoolSubVault defaults max_term_seconds to 30 days.
    expect(marketDecoded.asks[0].order.termSeconds).toBe(30 * 86_400);
    expect(marketDecoded.asks[0].order.side).toBe(1); // Side::Ask

    const [vaultPda] = globalVaultPda(USDC_BANK);
    const vault = decodeGlobalVault((await bk.getAccount(vaultPda))!.data);
    expect(vault.marketOrders).toHaveLength(1);
    expect(vault.marketOrders[0].order.market.equals(market.publicKey)).toBe(true);
    expect(vault.marketOrders[0].order.subVaultId).toBe(1);
    expect(vault.marketOrders[0].order.rateBps).toBe(quotedRateBps);
  });

  it('UpdateOrderForSubVault re-quotes (cancel-and-replace)', async () => {
    // v1 D4: no rate/term args — the program re-quotes live. The resting ask
    // gets a fresh order sequence (back of price-time priority) at the
    // freshly-computed rate.
    await bk.refreshOracleFreshness({ pythOracle: USDC_ORACLE, switchboardOracle: SOL_ORACLE });
    await bk.send(
      [
        updateOrderForSubVaultInstruction({
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

    const marketDecoded = decodeMarket((await bk.getAccount(market.publicKey))!.data);
    expect(marketDecoded.asks).toHaveLength(1);
    const requotedRateBps = marketDecoded.asks[0].order.rateBps;
    expect(requotedRateBps).toBeGreaterThan(0);
    expect(marketDecoded.asks[0].order.termSeconds).toBe(30 * 86_400);

    const [vaultPda] = globalVaultPda(USDC_BANK);
    const vault = decodeGlobalVault((await bk.getAccount(vaultPda))!.data);
    expect(vault.marketOrders[0].order.rateBps).toBe(requotedRateBps);
  });

  it('CancelOrderForSubVault empties both trees', async () => {
    await bk.send(
      [
        cancelOrderForSubVaultInstruction({
          feePayer: curator.publicKey,
          curator: curator.publicKey,
          debtBank: USDC_BANK,
          market: market.publicKey,
          subVaultId: 1,
        }),
      ],
      [curator],
    );

    const marketDecoded = decodeMarket((await bk.getAccount(market.publicKey))!.data);
    expect(marketDecoded.asks).toHaveLength(0);
    const [vaultPda] = globalVaultPda(USDC_BANK);
    const vault = decodeGlobalVault((await bk.getAccount(vaultPda))!.data);
    expect(vault.marketOrders).toHaveLength(0);
  });
});
