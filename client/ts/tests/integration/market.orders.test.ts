import { expect } from 'chai';
import {
  PublicKey,
  Transaction,
} from '@solana/web3.js';
import {
  TOKEN_PROGRAM_ID,
} from '@solana/spl-token';
import BN from 'bn.js';
import {
  bankLiquidityVaultAuthority,
  bankrunYdeltaClient,
  bootstrap,
  claimSeat,
  createAtaAndMintTo,
  createGlobalConfigIfMissing,
  createUsdcWsolMarket,
  expectYdeltaError,
  Harness,
  MARGINFI_GROUP,
  MARGINFI_PROGRAM_ID,
  overrideUsdcWsolAuthorities,
  patchUsdcSolBanksToPythPush,
  processTx,
  processTxOrThrow,
  seedYdeltaProgramData,
  setPythPullOraclePrice,
  SOL_BANK,
  SOL_ORACLE,
  USDC_BANK,
  USDC_LIQUIDITY_VAULT,
  USDC_MINT,
  USDC_ORACLE,
} from './_harness';
import {
  createDepositInstruction,
  createPlaceOrderInstruction,
  createCancelOrderInstruction,
} from '../../src/ydelta/instructions';
import { Market } from '../../src/market';
import { Side, OrderType, OrderKind } from '../../src/ydelta/types/enums';

describe('e2e: PlaceOrder + Market tree decode', function () {
  this.timeout(120_000);
  let harness: Harness;
  let market: PublicKey;
  let aliceUsdc: PublicKey;

  const DEPOSIT_USDC_ATOMS = 1_000n * 1_000_000n; // 1000 USDC
  const ASK_PRINCIPAL_ATOMS = 500n * 1_000_000n; // 500 USDC
  const ASK_RATE_BPS = 600; // 6% APY
  const ASK_TERM_SECONDS = 86_400 * 30; // 30 days
  const TWO_WEEKS = 86_400 * 14;

  before(async () => {
    harness = await bootstrap();
    seedYdeltaProgramData(harness.context, harness.payer);
    overrideUsdcWsolAuthorities({
      context: harness.context,
      authority: harness.payer.publicKey,
    });
    await createGlobalConfigIfMissing({ context: harness.context, payer: harness.payer });
    // Flip mainnet USDC + SOL bank's oracle_setup from PythLegacy (1) →
    // PythPush (3) so yDelta's adapter recognises them. Mainnet was deployed
    // with the legacy variant; tests use PythPush exclusively.
    await patchUsdcSolBanksToPythPush(harness.context);
    // Diagnostic: confirm oracle_setup and bank's stored oracle_key.
    {
      const usdc = await harness.context.banksClient.getAccount(USDC_BANK);
      const sol = await harness.context.banksClient.getAccount(SOL_BANK);
      const usdcSetup = usdc!.data[8 + 288 + 313];
      const solSetup = sol!.data[8 + 288 + 313];
      const usdcOracleKey = new PublicKey(usdc!.data.slice(8 + 288 + 314, 8 + 288 + 314 + 32));
      const solOracleKey = new PublicKey(sol!.data.slice(8 + 288 + 314, 8 + 288 + 314 + 32));
      // eslint-disable-next-line no-console
      console.log(`USDC bank setup=${usdcSetup} oracle_key=${usdcOracleKey.toBase58()}`);
      // eslint-disable-next-line no-console
      console.log(`SOL  bank setup=${solSetup} oracle_key=${solOracleKey.toBase58()}`);
    }
    const r = await createUsdcWsolMarket({ context: harness.context, payer: harness.payer });
    market = r.market.publicKey;

    // Re-stamp the mainnet oracle accounts with fresh prices anchored to the
    // bankrun clock so they pass the marginfi staleness gate at match time.
    await setPythPullOraclePrice({
      context: harness.context,
      address: USDC_ORACLE,
      price: 1,
      decimals: 8,
    });
    // SOL_ORACLE on mainnet is a Switchboard account; force-flip its owner
    // to the Pyth receiver so yDelta's PythPush adapter accepts it.
    await setPythPullOraclePrice({
      context: harness.context,
      address: SOL_ORACLE,
      price: 100,
      decimals: 8,
      forceOwner: true,
    });

    // Diagnostic: oracle owners after re-stamp.
    {
      const usdcOracle = await harness.context.banksClient.getAccount(USDC_ORACLE);
      const solOracle = await harness.context.banksClient.getAccount(SOL_ORACLE);
      // eslint-disable-next-line no-console
      console.log(`USDC oracle owner=${usdcOracle!.owner.toBase58()} verify=${usdcOracle!.data[40]}`);
      // eslint-disable-next-line no-console
      console.log(`SOL  oracle owner=${solOracle!.owner.toBase58()} verify=${solOracle!.data[40]}`);
    }
    aliceUsdc = await createAtaAndMintTo({
      context: harness.context,
      payer: harness.payer,
      owner: harness.payer.publicKey,
      mint: USDC_MINT,
      mintAuthority: harness.payer,
      amountAtoms: DEPOSIT_USDC_ATOMS * 5n,
    });

    await claimSeat({ context: harness.context, payer: harness.payer, market });

    // Deposit USDC so our seat has lender-side debt shares to back an Ask.
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
      { amountAtoms: new BN(DEPOSIT_USDC_ATOMS.toString()), traderIndexHint: null },
    );
    await processTxOrThrow(
      harness.context.banksClient,
      new Transaction().add(depositIx),
      harness.payer,
    );
  });

  function placeOrderIx({
    side,
    rateBps,
    termSeconds,
    principalAtoms,
    collateralAtoms,
    orderType = OrderType.Limit,
  }: {
    side: Side;
    rateBps: number;
    termSeconds: number;
    principalAtoms: bigint;
    collateralAtoms: bigint;
    orderType?: OrderType;
  }): ReturnType<typeof createPlaceOrderInstruction> {
    return createPlaceOrderInstruction(
      {
        payer: harness.payer.publicKey,
        market,
        marginfiGroup: MARGINFI_GROUP,
        debtBank: USDC_BANK,
        collateralBank: SOL_BANK,
        debtOracles: [USDC_ORACLE],
        collateralOracles: [SOL_ORACLE],
        debtLiquidityVault: USDC_LIQUIDITY_VAULT,
        debtBankLiquidityVaultAuthority: bankLiquidityVaultAuthority(USDC_BANK),
        borrowerDebtToken: aliceUsdc,
        debtMint: USDC_MINT,
        tokenProgram: TOKEN_PROGRAM_ID,
        marginfiProgram: MARGINFI_PROGRAM_ID,
      },
      {
        seatIndexHint: null,
        side,
        orderType,
        flags: 0,
        kind: OrderKind.Primary,
        rateBps,
        termSeconds,
        principalAtoms: new BN(principalAtoms.toString()),
        collateralAtoms: new BN(collateralAtoms.toString()),
        askingPriceAtoms: new BN(0),
        lastValidUnixTs: new BN(0),
        borrowerLtvBps: null,
      },
    );
  }

  it('PlaceOrder Ask lands and surfaces in Market.asks()', async () => {
    const ix = placeOrderIx({
      side: Side.Ask,
      rateBps: ASK_RATE_BPS,
      termSeconds: ASK_TERM_SECONDS,
      principalAtoms: ASK_PRINCIPAL_ATOMS,
      collateralAtoms: 0n,
    });
    const res = await processTx(
      harness.context.banksClient,
      new Transaction().add(ix),
      harness.payer,
    );
    if (res.result !== null) {
      const logs = (res.meta?.logMessages ?? []) as string[];
      throw new Error(`PlaceOrder Ask failed: ${res.result}\n${logs.join('\n')}`);
    }

    const { connection } = bankrunYdeltaClient(harness.context);
    const m = await Market.loadFromAddress({ connection, address: market });
    const asks = m.asks();
    expect(asks.length).to.equal(1);
    expect(asks[0].rateBps).to.equal(ASK_RATE_BPS);
    expect(asks[0].termSeconds).to.equal(ASK_TERM_SECONDS);
    expect(asks[0].principalAtoms.toString()).to.equal(ASK_PRINCIPAL_ATOMS.toString());
    expect(asks[0].side).to.equal(Side.Ask);
    expect(m.bestAsk()!.rateBps).to.equal(ASK_RATE_BPS);
    expect(m.bids().length).to.equal(0);
  });

  it('CancelOrder removes the Ask', async () => {
    const { connection } = bankrunYdeltaClient(harness.context);
    let m = await Market.loadFromAddress({ connection, address: market });
    const askBefore = m.asks()[0];
    expect(askBefore, 'pre-cancel ask').to.not.equal(undefined);

    const cancelIx = createCancelOrderInstruction(
      {
        payer: harness.payer.publicKey,
        market,
      },
      {
        orderSequenceNumber: askBefore.sequenceNumber,
        orderIndexHint: null,
        seatIndexHint: null,
      },
    );
    await processTxOrThrow(
      harness.context.banksClient,
      new Transaction().add(cancelIx),
      harness.payer,
    );

    m = await Market.loadFromAddress({ connection, address: market });
    expect(m.asks().length).to.equal(0);
  });

  it('PostOnly Ask that would not cross rests cleanly', async () => {
    // No bids exist, so a PostOnly Ask trivially does not cross.
    const ix = placeOrderIx({
      side: Side.Ask,
      rateBps: 700,
      termSeconds: ASK_TERM_SECONDS,
      principalAtoms: 100n * 1_000_000n,
      collateralAtoms: 0n,
      orderType: OrderType.PostOnly,
    });
    await processTxOrThrow(
      harness.context.banksClient,
      new Transaction().add(ix),
      harness.payer,
    );
    const { connection } = bankrunYdeltaClient(harness.context);
    const m = await Market.loadFromAddress({ connection, address: market });
    expect(m.asks().length).to.equal(1);
    expect(m.asks()[0].rateBps).to.equal(700);
  });

  it('Ask with collateral > 0 fails with CollateralInsufficient', async () => {
    const ix = placeOrderIx({
      side: Side.Ask,
      rateBps: 500,
      termSeconds: ASK_TERM_SECONDS,
      principalAtoms: 100n * 1_000_000n,
      collateralAtoms: 1n, // illegal for Ask
    });
    const res = await processTx(
      harness.context.banksClient,
      new Transaction().add(ix),
      harness.payer,
    );
    expectYdeltaError(res, 'CollateralInsufficient');
  });

  it('zero term fails with TermNotCompatible', async () => {
    const ix = placeOrderIx({
      side: Side.Ask,
      rateBps: 500,
      termSeconds: 0,
      principalAtoms: 100n * 1_000_000n,
      collateralAtoms: 0n,
    });
    const res = await processTx(
      harness.context.banksClient,
      new Transaction().add(ix),
      harness.payer,
    );
    expectYdeltaError(res, 'TermNotCompatible');
  });

  void TWO_WEEKS;
});
