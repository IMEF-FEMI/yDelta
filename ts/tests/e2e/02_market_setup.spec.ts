/**
 * Tier 2 e2e: market lifecycle. Builds on a fresh bankrun ledger with the
 * marginfi mainnet fixtures loaded.
 *
 *   1. CreateGlobalConfig (precondition for every state-mutating ix).
 *   2. CreateMarket(params) — initialises both marginfi integration accounts
 *      via CPI AND applies any supplied `CreateMarketParams` fee overrides.
 *      Markets ship unpaused with a safe `FeeConfig::default()`.
 *   3. SetFeeConfig — retunes fee_config on a live market (still callable
 *      while paused, for emergency retunes).
 *   4. SetMarketPause(true) / (false) — admin halt/resume.
 *   5. CreateVault — opens the per-mint GlobalVault.
 *   6. CreatePoolSubVault — adds sub-vault 1.
 *
 * Verifies decoded state at each step.
 */
import { beforeAll, describe, expect, it } from 'vitest';
import {
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
} from '@solana/web3.js';

import {
  createGlobalConfigInstruction,
  createMarketInstruction,
  createPoolSubVaultInstruction,
  createVaultInstruction,
  decodeGlobalConfig,
  decodeGlobalVault,
  decodeMarketHeader,
  globalConfigPda,
  globalVaultPda,
  MARKET_FIXED_SIZE,
  setFeeConfigInstruction,
  setMarketPauseInstruction,
  YDELTA_PROGRAM_ID,
} from '../../src/index.js';
import { bootBankrun, BankrunHandle } from './_bankrun.ts';
import { MARGINFI_GROUP, MARGINFI_PROGRAM_ID, USDC_BANK, USDC_MINT, SOL_BANK, WSOL_MINT } from './_fixtures.ts';

const BPF_LOADER_UPGRADEABLE_PROGRAM_ID = new PublicKey(
  'BPFLoaderUpgradeab1e11111111111111111111111',
);

function programDataPda(): PublicKey {
  return PublicKey.findProgramAddressSync(
    [YDELTA_PROGRAM_ID.toBuffer()],
    BPF_LOADER_UPGRADEABLE_PROGRAM_ID,
  )[0];
}

describe('e2e: market + vault setup', () => {
  let bk: BankrunHandle;
  let admin: Keypair;
  let market: Keypair;

  beforeAll(async () => {
    bk = await bootBankrun({ loadMarginfiFixtures: true });
    admin = bk.payer;
    market = Keypair.generate();
  });

  it('CreateGlobalConfig succeeds', async () => {
    await bk.send(
      [createGlobalConfigInstruction({ payer: admin.publicKey, programData: programDataPda() })],
      [admin],
    );
    const cfg = decodeGlobalConfig((await bk.getAccount(globalConfigPda()[0]))!.data);
    expect(cfg.protocolAdmin.equals(admin.publicKey)).toBe(true);
  });

  it('Allocate market account (512 bytes, owned by yDelta)', async () => {
    // The CreateMarket ix expects the market account to already exist as a
    // program-owned 512-byte buffer. The caller bundles a `createAccount`
    // ix in the same tx normally; we send it standalone here for clarity.
    const blockhash = (await bk.client.getLatestBlockhash())![0];
    const rent = Number(await bk.client.getRent().then((r) => r.minimumBalance(BigInt(MARKET_FIXED_SIZE))));
    const tx = new Transaction({ feePayer: admin.publicKey, recentBlockhash: blockhash });
    tx.add(
      SystemProgram.createAccount({
        fromPubkey: admin.publicKey,
        newAccountPubkey: market.publicKey,
        space: MARKET_FIXED_SIZE,
        lamports: rent,
        programId: YDELTA_PROGRAM_ID,
      }),
    );
    tx.sign(admin, market);
    await bk.client.processTransaction(tx);

    const acc = await bk.getAccount(market.publicKey);
    expect(acc).not.toBeNull();
    expect(acc!.owner.equals(YDELTA_PROGRAM_ID)).toBe(true);
    expect(acc!.data.length).toBe(MARKET_FIXED_SIZE);
  });

  it('CreateMarket(params) ships unpaused + applies the supplied fee overrides', async () => {
    await bk.send(
      [
        createMarketInstruction({
          marketCreator: admin.publicKey,
          market: market.publicKey,
          debtMint: USDC_MINT,
          collateralMint: WSOL_MINT,
          marginfiGroup: MARGINFI_GROUP,
          debtBank: USDC_BANK,
          collateralBank: SOL_BANK,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          params: {
            protocolFeeBpsFloor: 50,
            curatorSplitBps: 1_000,
            gracePeriodSeconds: 86_400,
          },
        }),
      ],
      [admin],
    );

    const header = decodeMarketHeader((await bk.getAccount(market.publicKey))!.data);
    expect(header.debtMint.equals(USDC_MINT)).toBe(true);
    expect(header.collateralMint.equals(WSOL_MINT)).toBe(true);
    expect(header.admin.equals(admin.publicKey)).toBe(true);
    // Market is fully configured AND unpaused in one ix — no follow-up
    // `set_fee_config` / `set_market_pause(false)` round-trip required.
    expect(header.isPaused).toBe(false);
    expect(header.feeConfig.protocolFeeBpsFloor).toBe(50);
    expect(header.feeConfig.curatorSplitBps).toBe(1_000);
    expect(header.feeConfig.gracePeriodSeconds).toBe(86_400);
    expect(header.marginfiGroup.equals(MARGINFI_GROUP)).toBe(true);
  });

  it('SetFeeConfig retunes individual fields on a live market', async () => {
    await bk.send(
      [
        setFeeConfigInstruction({
          admin: admin.publicKey,
          market: market.publicKey,
          protocolFeeBpsFloor: 75,
        }),
      ],
      [admin],
    );

    const header = decodeMarketHeader((await bk.getAccount(market.publicKey))!.data);
    // Only the field passed in changes; the others stay at their
    // create-market values (regression guard for the shared
    // `apply_fee_config_overrides` helper).
    expect(header.feeConfig.protocolFeeBpsFloor).toBe(75);
    expect(header.feeConfig.curatorSplitBps).toBe(1_000);
    expect(header.feeConfig.gracePeriodSeconds).toBe(86_400);
  });

  it('SetMarketPause toggles is_paused both directions', async () => {
    // Halt.
    await bk.send(
      [setMarketPauseInstruction({ admin: admin.publicKey, market: market.publicKey, paused: true })],
      [admin],
    );
    let header = decodeMarketHeader((await bk.getAccount(market.publicKey))!.data);
    expect(header.isPaused).toBe(true);
    // Resume.
    await bk.send(
      [setMarketPauseInstruction({ admin: admin.publicKey, market: market.publicKey, paused: false })],
      [admin],
    );
    header = decodeMarketHeader((await bk.getAccount(market.publicKey))!.data);
    expect(header.isPaused).toBe(false);
  });

  it('CreateVault opens the per-mint GlobalVault', async () => {
    await bk.send(
      [
        createVaultInstruction({
          payer: admin.publicKey,
          mint: USDC_MINT,
          marginfiGroup: MARGINFI_GROUP,
          lendingPool: USDC_BANK,
          marginfiProgram: MARGINFI_PROGRAM_ID,
        }),
      ],
      [admin],
    );
    const [vaultPda] = globalVaultPda(USDC_BANK);
    const vault = decodeGlobalVault((await bk.getAccount(vaultPda))!.data);
    expect(vault.header.mint.equals(USDC_MINT)).toBe(true);
    expect(vault.header.globalVaultAdmin.equals(admin.publicKey)).toBe(true);
    expect(vault.header.lendingPool.equals(USDC_BANK)).toBe(true);
    expect(vault.header.subVaultCount).toBe(0);
    expect(vault.subVaults).toHaveLength(0);
  });

  it('CreatePoolSubVault inserts sub-vault 1 into the tree', async () => {
    const curator = await bk.fundedKeypair();
    await bk.send(
      [
        createPoolSubVaultInstruction({
          payer: admin.publicKey,
          bank: USDC_BANK,
          curator: curator.publicKey,
          spreadBps: 0,
          maxLtvBps: 6_000,
          liquidationLtvBps: 7_000,
          maxTermSeconds: 30 * 86_400,
          curatorFeeBps: 0,
        }),
      ],
      [admin],
    );
    const [vaultPda] = globalVaultPda(USDC_BANK);
    const vault = decodeGlobalVault((await bk.getAccount(vaultPda))!.data);
    expect(vault.header.subVaultCount).toBe(1);
    expect(vault.subVaults).toHaveLength(1);
    const subVault = vault.subVaults[0].subVault;
    // First sub-vault on a fresh vault is assigned id 1 (0 is the sentinel).
    expect(subVault.subVaultId).toBe(1);
    expect(subVault.curator.equals(curator.publicKey)).toBe(true);
    expect(subVault.maxLtvBps).toBe(6_000);
    expect(subVault.liquidationLtvBps).toBe(7_000);
    expect(subVault.maxTermSeconds).toBe(30 * 86_400);
    expect(subVault.totalShares).toBe(0n);
    expect(subVault.deployedPrincipalAtoms).toBe(0n);
  });
});
