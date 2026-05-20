/**
 * Tier 3a e2e: curator flow on the lender side.
 *
 *   1. GlobalVaultDeposit — depositor funds profile 0's idle pool.
 *      (Verifies profile.total_principal_atoms / total_shares grow.)
 *   2. PlaceOrderForRiskProfile — curator quotes an ask; zero-CPI.
 *      (Verifies the asks RB-tree has one resting order at the chosen rate.)
 *   3. UpdateOrderForRiskProfile — cancel-and-replace at a new rate.
 *   4. CancelOrderForRiskProfile — tree returns to empty.
 *
 * Builds on the Tier 2 setup (market + vault + profile). A pre-funded
 * USDC ATA is synthesised for the depositor since the loaded USDC mint is
 * the real mainnet one and we can't mint to it directly.
 */
import { beforeAll, describe, expect, it } from 'vitest';
import { Keypair, PublicKey } from '@solana/web3.js';

import {
  cancelOrderForRiskProfileInstruction,
  decodeGlobalVault,
  decodeMarket,
  globalVaultPda,
  globalVaultDepositInstruction,
  placeOrderForRiskProfileInstruction,
  updateOrderForRiskProfileInstruction,
} from '../../src/index.js';
import { bootBankrun, BankrunHandle } from './_bankrun.ts';
import {
  MARGINFI_GROUP,
  MARGINFI_PROGRAM_ID,
  SPL_TOKEN_PROGRAM_ID,
  USDC_BANK,
  USDC_LIQUIDITY_VAULT,
  USDC_MINT,
} from './_fixtures.ts';
import {
  setupGlobalConfig,
  setupMarket,
  setupRiskProfile,
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
    await setupRiskProfile(bk, admin, curator.publicKey);

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

  it('GlobalVaultDeposit credits profile.total_principal + total_shares', async () => {
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
          amountAtoms: 500_000_000_000n, // 500_000 USDC
        }),
      ],
      [depositor],
    );

    // Genesis-deposit invariant: when `total_shares == 0` pre-deposit, the
    // on-chain `atoms_to_vault_shares` formula returns `atoms` 1:1. So
    // the first deposit of 500B atoms must mint EXACTLY 500B vault shares.
    const [vaultPda] = globalVaultPda(USDC_MINT);
    const vault = decodeGlobalVault((await bk.getAccount(vaultPda))!.data);
    const profile = vault.riskProfiles[0].profile;
    expect(profile.totalPrincipalAtoms).toBe(500_000_000_000n);
    expect(profile.totalShares).toBe(500_000_000_000n); // 1:1 at genesis
    expect(profile.deployedPrincipalAtoms).toBe(0n);
    expect(profile.encumberedInOrdersAtoms).toBe(0n);
  });

  it('PlaceOrderForRiskProfile inserts a resting ask + a vault order ref', async () => {
    await bk.send(
      [
        placeOrderForRiskProfileInstruction({
          feePayer: curator.publicKey,
          curator: curator.publicKey,
          mint: USDC_MINT,
          market: market.publicKey,
          profileId: 0,
          rateBps: 750,
          termSeconds: 7 * 86_400,
        }),
      ],
      [curator],
    );

    const marketDecoded = decodeMarket((await bk.getAccount(market.publicKey))!.data);
    expect(marketDecoded.asks).toHaveLength(1);
    expect(marketDecoded.asks[0].order.rateBps).toBe(750);
    expect(marketDecoded.asks[0].order.termSeconds).toBe(7 * 86_400);
    expect(marketDecoded.asks[0].order.side).toBe(1); // Side::Ask

    const [vaultPda] = globalVaultPda(USDC_MINT);
    const vault = decodeGlobalVault((await bk.getAccount(vaultPda))!.data);
    expect(vault.marketOrders).toHaveLength(1);
    expect(vault.marketOrders[0].order.market.equals(market.publicKey)).toBe(true);
    expect(vault.marketOrders[0].order.profileId).toBe(0);
    expect(vault.marketOrders[0].order.rateBps).toBe(750);
  });

  it('UpdateOrderForRiskProfile changes rate + term (cancel-and-replace)', async () => {
    await bk.send(
      [
        updateOrderForRiskProfileInstruction({
          feePayer: curator.publicKey,
          curator: curator.publicKey,
          mint: USDC_MINT,
          market: market.publicKey,
          profileId: 0,
          newRateBps: 900,
          newTermSeconds: 14 * 86_400,
        }),
      ],
      [curator],
    );

    const marketDecoded = decodeMarket((await bk.getAccount(market.publicKey))!.data);
    expect(marketDecoded.asks).toHaveLength(1);
    expect(marketDecoded.asks[0].order.rateBps).toBe(900);
    expect(marketDecoded.asks[0].order.termSeconds).toBe(14 * 86_400);

    const [vaultPda] = globalVaultPda(USDC_MINT);
    const vault = decodeGlobalVault((await bk.getAccount(vaultPda))!.data);
    expect(vault.marketOrders[0].order.rateBps).toBe(900);
  });

  it('CancelOrderForRiskProfile empties both trees', async () => {
    await bk.send(
      [
        cancelOrderForRiskProfileInstruction({
          feePayer: curator.publicKey,
          curator: curator.publicKey,
          mint: USDC_MINT,
          market: market.publicKey,
          profileId: 0,
        }),
      ],
      [curator],
    );

    const marketDecoded = decodeMarket((await bk.getAccount(market.publicKey))!.data);
    expect(marketDecoded.asks).toHaveLength(0);
    const [vaultPda] = globalVaultPda(USDC_MINT);
    const vault = decodeGlobalVault((await bk.getAccount(vaultPda))!.data);
    expect(vault.marketOrders).toHaveLength(0);
  });
});
