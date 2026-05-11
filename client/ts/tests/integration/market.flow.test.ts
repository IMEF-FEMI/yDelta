import { expect } from 'chai';
import { Keypair, Transaction } from '@solana/web3.js';
import {
  bootstrap,
  bankrunYdeltaClient,
  seedYdeltaProgramData,
  createGlobalConfigIfMissing,
  createUsdcWsolMarket,
  claimSeat,
  Harness,
  processTx,
  expectYdeltaError,
} from './_harness';
import { createClaimSeatInstruction } from '../../src/ydelta/instructions';
import { Market } from '../../src/market';
import { GlobalConfig } from '../../src/globalConfig';
import { OwnerKind } from '../../src/ydelta/types/enums';

describe('e2e: market create + claim seat (full flow)', function () {
  this.timeout(120_000);

  let harness: Harness;
  let market: Keypair;

  before(async () => {
    harness = await bootstrap();
    // Bind the bankrun payer as the ydelta upgrade authority — unblocks
    // CreateGlobalConfig's ProtocolAdminRequired gate.
    seedYdeltaProgramData(harness.context, harness.payer);
    await createGlobalConfigIfMissing({ context: harness.context, payer: harness.payer });
    const result = await createUsdcWsolMarket({
      context: harness.context,
      payer: harness.payer,
    });
    market = result.market;
  });

  it('GlobalConfig is decodable and lists payer as protocol_admin', async () => {
    const { connection } = bankrunYdeltaClient(harness.context);
    const gc = await GlobalConfig.load({ connection });
    expect(gc).to.not.equal(null);
    expect(gc!.protocolAdmin().equals(harness.payer.publicKey)).to.equal(true);
    expect(gc!.isPaused()).to.equal(false);
  });

  it('Market header decodes with the right mints, admin, banks', async () => {
    const { connection } = bankrunYdeltaClient(harness.context);
    const m = await Market.loadFromAddress({ connection, address: market.publicKey });
    const { USDC_MINT, WSOL_MINT, USDC_BANK, SOL_BANK, MARGINFI_GROUP } = await import('./_harness/mainnet');
    expect(m.debtMint().equals(USDC_MINT)).to.equal(true);
    expect(m.collateralMint().equals(WSOL_MINT)).to.equal(true);
    expect(m.debtBank().equals(USDC_BANK)).to.equal(true);
    expect(m.collateralBank().equals(SOL_BANK)).to.equal(true);
    expect(m.marginfiGroup().equals(MARGINFI_GROUP)).to.equal(true);
    expect(m.admin().equals(harness.payer.publicKey)).to.equal(true);
    expect(m.isPaused()).to.equal(false);
    // Trees should be empty on a fresh market.
    expect(m.bids().length).to.equal(0);
    expect(m.asks().length).to.equal(0);
    expect(m.claimedSeats().length).to.equal(0);
  });

  it('ClaimSeat lands and surfaces in Market.seatFor', async () => {
    await claimSeat({ context: harness.context, payer: harness.payer, market: market.publicKey });
    const { connection } = bankrunYdeltaClient(harness.context);
    const m = await Market.loadFromAddress({ connection, address: market.publicKey });
    const seat = m.seatFor(harness.payer.publicKey, 0);
    expect(seat).to.not.equal(undefined);
    expect(seat!.ownerKind).to.equal(OwnerKind.User);
    expect(seat!.riskProfileId).to.equal(0);
    expect(seat!.debtWithdrawableShares.isZero()).to.equal(true);
    expect(seat!.collateralWithdrawableShares.isZero()).to.equal(true);
    expect(m.userSeats().length).to.equal(1);
    expect(m.vaultSeats().length).to.equal(0);
  });

  it('claiming the same seat twice fails with AlreadyClaimedSeat', async () => {
    const ix = createClaimSeatInstruction({
      payer: harness.payer.publicKey,
      market: market.publicKey,
    });
    const tx = new Transaction().add(ix);
    const res = await processTx(harness.context.banksClient, tx, harness.payer);
    expectYdeltaError(res, 'AlreadyClaimedSeat');
  });
});
