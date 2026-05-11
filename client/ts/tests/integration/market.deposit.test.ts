import { expect } from 'chai';
import {
  PublicKey,
  Transaction,
} from '@solana/web3.js';
import { TOKEN_PROGRAM_ID } from '@solana/spl-token';
import BN from 'bn.js';
import {
  bankrunYdeltaClient,
  bootstrap,
  claimSeat,
  createGlobalConfigIfMissing,
  createUsdcWsolMarket,
  createAtaAndMintTo,
  expectYdeltaError,
  Harness,
  overrideUsdcWsolAuthorities,
  processTx,
  seedYdeltaProgramData,
  USDC_BANK,
  USDC_LIQUIDITY_VAULT,
  USDC_MINT,
  MARGINFI_GROUP,
  MARGINFI_PROGRAM_ID,
  getTokenBalance,
} from './_harness';
import {
  createDepositInstruction,
} from '../../src/ydelta/instructions';
import { Market } from '../../src/market';
import { marketVaultPda } from '../../src/utils/pdas';

describe('e2e: Deposit — full ix flow with marginfi CPI', function () {
  this.timeout(120_000);
  let harness: Harness;
  let market: PublicKey;
  let aliceUsdc: PublicKey;

  const DEPOSIT_AMOUNT_USDC = 1_000n * 1_000_000n; // 1000 USDC at 6 decimals

  before(async () => {
    harness = await bootstrap();
    seedYdeltaProgramData(harness.context, harness.payer);
    overrideUsdcWsolAuthorities({
      context: harness.context,
      authority: harness.payer.publicKey,
    });
    await createGlobalConfigIfMissing({ context: harness.context, payer: harness.payer });
    const r = await createUsdcWsolMarket({ context: harness.context, payer: harness.payer });
    market = r.market.publicKey;

    aliceUsdc = await createAtaAndMintTo({
      context: harness.context,
      payer: harness.payer,
      owner: harness.payer.publicKey,
      mint: USDC_MINT,
      mintAuthority: harness.payer,
      amountAtoms: DEPOSIT_AMOUNT_USDC * 5n,
    });

    await claimSeat({ context: harness.context, payer: harness.payer, market });
  });

  it('payer ATA is funded with USDC', async () => {
    const bal = await getTokenBalance(harness.context.banksClient, aliceUsdc);
    expect(bal).to.equal(DEPOSIT_AMOUNT_USDC * 5n);
  });

  it('Deposit ix lands and surfaces via Market.seatFor', async () => {
    const [debtVault] = marketVaultPda(market, USDC_MINT);

    const traderUsdcBefore = await getTokenBalance(harness.context.banksClient, aliceUsdc);
    const marketVaultBefore = await getTokenBalance(harness.context.banksClient, debtVault);
    const usdcBankVaultBefore = await getTokenBalance(
      harness.context.banksClient,
      USDC_LIQUIDITY_VAULT,
    );

    const depositIx = createDepositInstruction(
      {
        payer: harness.payer.publicKey,
        market,
        mint: USDC_MINT,
        debtMint: USDC_MINT,
        traderToken: aliceUsdc,
        tokenProgram: TOKEN_PROGRAM_ID,
        marginfiGroup: MARGINFI_GROUP,
        bank: USDC_BANK,
        liquidityVault: USDC_LIQUIDITY_VAULT,
        marginfiProgram: MARGINFI_PROGRAM_ID,
      },
      { amountAtoms: new BN(DEPOSIT_AMOUNT_USDC.toString()), traderIndexHint: null },
    );

    const res = await processTx(
      harness.context.banksClient,
      new Transaction().add(depositIx),
      harness.payer,
    );
    if (res.result !== null) {
      const logs = (res.meta?.logMessages ?? []) as string[];
      throw new Error(`Deposit failed: ${res.result}\n${logs.join('\n')}`);
    }

    // Wallet balance dropped by exactly the deposit amount.
    const traderUsdcAfter = await getTokenBalance(harness.context.banksClient, aliceUsdc);
    expect(traderUsdcBefore - traderUsdcAfter).to.equal(DEPOSIT_AMOUNT_USDC);

    // Market staging vault should be empty at-rest (atoms flowed through to marginfi).
    const marketVaultAfter = await getTokenBalance(harness.context.banksClient, debtVault);
    expect(marketVaultAfter).to.equal(marketVaultBefore);

    // Marginfi bank's liquidity vault should hold the new atoms.
    const usdcBankVaultAfter = await getTokenBalance(
      harness.context.banksClient,
      USDC_LIQUIDITY_VAULT,
    );
    expect(usdcBankVaultAfter - usdcBankVaultBefore).to.equal(DEPOSIT_AMOUNT_USDC);

    // Seat now carries non-zero debt_withdrawable_shares.
    const { connection } = bankrunYdeltaClient(harness.context);
    const m = await Market.loadFromAddress({ connection, address: market });
    const seat = m.seatFor(harness.payer.publicKey, 0);
    expect(seat, 'alice seat').to.not.equal(undefined);
    expect(seat!.debtWithdrawableShares.isZero(), 'deposit should mint shares').to.equal(false);
    expect(seat!.collateralWithdrawableShares.isZero()).to.equal(true);
  });

  it('Deposit with zero amount fails with InvalidDepositAccounts', async () => {
    const depositIx = createDepositInstruction(
      {
        payer: harness.payer.publicKey,
        market,
        mint: USDC_MINT,
        debtMint: USDC_MINT,
        traderToken: aliceUsdc,
        tokenProgram: TOKEN_PROGRAM_ID,
        marginfiGroup: MARGINFI_GROUP,
        bank: USDC_BANK,
        liquidityVault: USDC_LIQUIDITY_VAULT,
        marginfiProgram: MARGINFI_PROGRAM_ID,
      },
      { amountAtoms: new BN(0), traderIndexHint: null },
    );
    const res = await processTx(
      harness.context.banksClient,
      new Transaction().add(depositIx),
      harness.payer,
    );
    expectYdeltaError(res, 'InvalidDepositAccounts');
  });
});
