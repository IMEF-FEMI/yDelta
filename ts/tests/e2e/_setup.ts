/**
 * Shared setup helpers for e2e specs. Each helper composes the SDK ixs that
 * a given spec needs as a precondition (global config, market, vault,
 * profile), so the spec files themselves stay focused on the flow under
 * test.
 */
import { Keypair, PublicKey, SystemProgram, Transaction } from '@solana/web3.js';

import {
  claimSeatInstruction,
  createGlobalConfigInstruction,
  createMarketInstruction,
  createRiskProfileInstruction,
  createVaultInstruction,
  cuBudgetIx,
  decodeMarket,
  depositInstruction,
  globalVaultDepositInstruction,
  HEAVY_IX_CU_LIMIT,
  MARKET_FIXED_SIZE,
  placeOrderForRiskProfileInstruction,
  placeOrderInstruction,
  YDELTA_PROGRAM_ID,
} from '../../src/index.js';
import { BankrunHandle } from './_bankrun.ts';
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

const BPF_LOADER_UPGRADEABLE_PROGRAM_ID = new PublicKey(
  'BPFLoaderUpgradeab1e11111111111111111111111',
);

export function programDataPda(): PublicKey {
  return PublicKey.findProgramAddressSync(
    [YDELTA_PROGRAM_ID.toBuffer()],
    BPF_LOADER_UPGRADEABLE_PROGRAM_ID,
  )[0];
}

/** Create the global config singleton with `admin` as protocol_admin. */
export async function setupGlobalConfig(bk: BankrunHandle, admin: Keypair): Promise<void> {
  await bk.send(
    [createGlobalConfigInstruction({ payer: admin.publicKey, programData: programDataPda() })],
    [admin],
  );
}

/**
 * Per-test fee_config overrides layered on top of the on-chain
 * `FeeConfig::default()`. The suite-wide convention is `ltv_buffer_bps: 0`
 * (matching-friendly — tiny atom amounts cross without hitting the buffer)
 * unless a spec needs to exercise non-zero fee accrual or LTV-tightening
 * behaviour.
 */
export interface FeeOverrides {
  protocolFeeBpsFloor?: number;
  originationBps?: number;
  curatorSplitBps?: number;
  curatorFeeBps?: number;
  liquidationKeeperBps?: number;
  liquidationProtocolBps?: number;
  ltvBufferBps?: number;
  gracePeriodSeconds?: number;
}

/**
 * Allocate + initialise a USDC/SOL market with the supplied fee overrides
 * baked in. Returns the market keypair (the caller should treat the pubkey
 * as the market id). The market ships **unpaused** and fully configured —
 * no follow-up `set_fee_config` / `set_market_pause` round-trip required.
 *
 * `ltvBufferBps` defaults to `0` to keep tiny-atom matching tests stable
 * across the suite (the on-chain default of 200 would tighten the
 * LTV-at-match gate enough to break those cases). Pass an explicit
 * `feeOverrides.ltvBufferBps` to opt in to a non-zero buffer.
 */
export async function setupMarket(
  bk: BankrunHandle,
  admin: Keypair,
  feeOverrides: FeeOverrides = {},
): Promise<Keypair> {
  const market = Keypair.generate();
  const blockhash = (await bk.client.getLatestBlockhash())![0];
  const rent = Number(
    await bk.client.getRent().then((r) => r.minimumBalance(BigInt(MARKET_FIXED_SIZE))),
  );
  const allocateTx = new Transaction({ feePayer: admin.publicKey, recentBlockhash: blockhash });
  allocateTx.add(
    SystemProgram.createAccount({
      fromPubkey: admin.publicKey,
      newAccountPubkey: market.publicKey,
      space: MARKET_FIXED_SIZE,
      lamports: rent,
      programId: YDELTA_PROGRAM_ID,
    }),
  );
  allocateTx.sign(admin, market);
  await bk.client.processTransaction(allocateTx);

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
          ltvBufferBps: feeOverrides.ltvBufferBps ?? 0,
          protocolFeeBpsFloor: feeOverrides.protocolFeeBpsFloor,
          originationBps: feeOverrides.originationBps,
          curatorSplitBps: feeOverrides.curatorSplitBps,
          curatorFeeBps: feeOverrides.curatorFeeBps,
          liquidationKeeperBps: feeOverrides.liquidationKeeperBps,
          liquidationProtocolBps: feeOverrides.liquidationProtocolBps,
          gracePeriodSeconds: feeOverrides.gracePeriodSeconds,
        },
      }),
    ],
    [admin],
  );
  return market;
}

/** Stand up the per-mint GlobalVault and stamp `admin` as `global_vault_admin`. */
export async function setupVault(bk: BankrunHandle, admin: Keypair): Promise<void> {
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
}

/** Insert a single risk profile (`profile_id = 0`) with the given curator + policy. */
export async function setupRiskProfile(
  bk: BankrunHandle,
  admin: Keypair,
  curator: PublicKey,
  opts: { profileId?: number; maxLtvBps?: number; maxTermSeconds?: number } = {},
): Promise<void> {
  await bk.send(
    [
      createRiskProfileInstruction({
        payer: admin.publicKey,
        mint: USDC_MINT,
        profileId: opts.profileId ?? 0,
        curator,
        maxLtvBps: opts.maxLtvBps ?? 6_000,
        maxTermSeconds: opts.maxTermSeconds ?? 30 * 86_400,
      }),
    ],
    [admin],
  );
}

/* ── End-to-end "match landed" setup ─────────────────────── */

/**
 * Derive `bank_liquidity_vault_authority` for a marginfi v0.1.8 bank.
 * Seed: `[b"liquidity_vault_auth", bank_pubkey]`.
 */
export function bankLiquidityVaultAuthority(bank: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync(
    [Buffer.from('liquidity_vault_auth'), bank.toBuffer()],
    MARGINFI_PROGRAM_ID,
  )[0];
}

export interface MatchLandedHandles {
  market: Keypair;
  curator: Keypair;
  depositor: Keypair;
  borrower: Keypair;
  borrowerUsdcAta: PublicKey;
  matchedLoanSequence: bigint;
  /** Principal that was actually matched, post-`origination_bps` deduction (here 0). */
  principalAtoms: bigint;
  collateralAtoms: bigint;
  askRateBps: number;
  termSeconds: number;
}

/**
 * Driver: global config → market → vault → profile → vault deposit → curator
 * ask → borrower seat → borrower collateral deposit → borrower IOC bid that
 * crosses the ask. Returns the handles + the matched-loan sequence that the
 * cranker can promote in the next ix.
 *
 * Default parameters mirror the Rust `vault_ask_crossed_by_borrower_bid_full_fill`
 * integration test — tiny atom amounts so the LTV gate is trivially below
 * the 80% profile cap regardless of the dumped mainnet oracle prices. Pass
 * `overrides` when a test needs larger atoms (e.g. partial liquidation
 * requires `outstanding ≥ 1000 atoms`).
 */
export interface MatchLandedOverrides {
  principalAtoms?: bigint;
  collateralAtoms?: bigint;
  vaultDepositAtoms?: bigint;
  wsolFundAtoms?: bigint;
  collateralDepositAtoms?: bigint;
}

export async function driveToMatchLanded(
  bk: BankrunHandle,
  overrides: MatchLandedOverrides = {},
): Promise<MatchLandedHandles> {
  const admin = bk.payer;
  const vaultDepositAtoms = overrides.vaultDepositAtoms ?? 100_000_000n;
  const askRateBps = 500;
  const bidRateBps = 800;
  const termSeconds = 30 * 86_400;
  const maxLtvBps = 8_000;
  const wsolFundAtoms = overrides.wsolFundAtoms ?? 100_000n;
  const collateralDepositAtoms = overrides.collateralDepositAtoms ?? 10_000n;
  const principalAtoms = overrides.principalAtoms ?? 100n;
  const collateralAtoms = overrides.collateralAtoms ?? 5_000n;

  await setupGlobalConfig(bk, admin);
  const market = await setupMarket(bk, admin);
  await setupVault(bk, admin);
  const curator = await bk.fundedKeypair();
  await setupRiskProfile(bk, admin, curator.publicKey, { maxLtvBps, maxTermSeconds: termSeconds });

  // Vault funding.
  const depositor = await bk.fundedKeypair();
  const depositorUsdcAta = Keypair.generate().publicKey;
  await bk.putTokenAccount({
    address: depositorUsdcAta,
    mint: USDC_MINT,
    owner: depositor.publicKey,
    amount: vaultDepositAtoms * 2n,
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
        rateBps: askRateBps,
        termSeconds,
      }),
    ],
    [curator],
  );

  // Borrower seat + collateral.
  const borrower = await bk.fundedKeypair();
  const borrowerSolAta = Keypair.generate().publicKey;
  const borrowerUsdcAta = Keypair.generate().publicKey;
  await bk.putWsolTokenAccount({
    address: borrowerSolAta,
    owner: borrower.publicKey,
    amount: wsolFundAtoms,
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
        amountAtoms: collateralDepositAtoms,
      }),
    ],
    [borrower],
  );

  // Borrower IOC bid → cross.
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
        termSeconds,
        principalAtoms,
        collateralAtoms,
      }),
    ],
    [borrower],
  );

  // Read back the matched-loan sequence (process_matched_loan needs it).
  const m = decodeMarket((await bk.getAccount(market.publicKey))!.data);
  const matchedLoanSequence = m.matchedLoans[0].loan.sequence;

  return {
    market,
    curator,
    depositor,
    borrower,
    borrowerUsdcAta,
    matchedLoanSequence,
    principalAtoms,
    collateralAtoms,
    askRateBps,
    termSeconds,
  };
}
