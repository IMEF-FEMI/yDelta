import { expect } from 'chai';
import { Keypair, PublicKey } from '@solana/web3.js';
import {
  MARKET_FIXED_DISCRIMINANT,
  MARKET_FIXED_SIZE,
  MARKET_BLOCK_SIZE,
  GLOBAL_CONFIG_DISCRIMINANT,
  LOAN_FIXED_DISCRIMINANT,
  NIL,
  RBTREE_OVERHEAD_BYTES,
} from '../src/utils/discriminator';
import { Market } from '../src/market';
import { Loan } from '../src/loan';
import { GlobalConfig } from '../src/globalConfig';
import { Side, OrderType, OrderKind, LoanState, LoanType, LenderKind } from '../src/ydelta/types';

const MARKET_ADDR = new PublicKey('11111111111111111111111111111112');
const DEBT_MINT = Keypair.generate().publicKey;
const COLL_MINT = Keypair.generate().publicKey;
const ADMIN = Keypair.generate().publicKey;

/** Build a minimal MarketFixed header with provided tree roots and an
 *  optional dynamic region for testing tree traversal. */
function buildMarketBuffer(opts: {
  asksRootIndex?: number;
  asksBestIndex?: number;
  bidsRootIndex?: number;
  bidsBestIndex?: number;
  dynamic?: Buffer;
}): Buffer {
  const dynamic = opts.dynamic ?? Buffer.alloc(0);
  const buf = Buffer.alloc(MARKET_FIXED_SIZE + dynamic.length);
  buf.writeBigUInt64LE(MARKET_FIXED_DISCRIMINANT, 0);
  // mints at offsets 16/48; debt vault @ 80, coll vault @ 112
  DEBT_MINT.toBuffer().copy(buf, 16);
  COLL_MINT.toBuffer().copy(buf, 48);
  // tree-root fields
  buf.writeUInt32LE(opts.bidsRootIndex ?? NIL, 180);
  buf.writeUInt32LE(opts.bidsBestIndex ?? NIL, 184);
  buf.writeUInt32LE(opts.asksRootIndex ?? NIL, 188);
  buf.writeUInt32LE(opts.asksBestIndex ?? NIL, 192);
  buf.writeUInt32LE(NIL, 196); // claimed seats
  buf.writeUInt32LE(NIL, 200); // matched loans
  // FeeConfig at offset 212 (24 bytes) — leave zero
  // admin at 440
  ADMIN.toBuffer().copy(buf, 440);
  // is_paused at 504
  buf.writeUInt8(0, 504);
  dynamic.copy(buf, MARKET_FIXED_SIZE);
  return buf;
}

/** Build a synthetic RestingOrder payload (144 bytes). */
function restingOrderPayload(args: {
  traderSeatIndex: number;
  rateBps: number;
  termSeconds: number;
  principalAtoms: bigint;
  side: Side;
  kind?: OrderKind;
  orderType?: OrderType;
  sequenceNumber?: bigint;
}): Buffer {
  const p = Buffer.alloc(144);
  p.writeUInt32LE(args.traderSeatIndex, 0);
  p.writeUInt32LE(0, 4); // loan_sequence_snapshot
  p.writeBigUInt64LE(args.sequenceNumber ?? 1n, 8);
  p.writeBigUInt64LE(args.principalAtoms, 16);
  p.writeBigUInt64LE(0n, 24); // collateral_atoms
  p.writeBigUInt64LE(0n, 32); // asking_price
  p.writeBigInt64LE(0n, 40); // last_valid_unix
  // loan_pda zero
  p.writeUInt32LE(args.termSeconds, 80);
  p.writeUInt16LE(args.rateBps, 84);
  p.writeUInt8(args.side, 86);
  p.writeUInt8(args.kind ?? OrderKind.Primary, 87);
  p.writeUInt8(args.orderType ?? OrderType.Limit, 88);
  p.writeUInt8(0, 89); // flags
  return p;
}

/** Build a single 160-byte RB-tree block (16-byte header + 144 payload). */
function rbBlock(
  left: number,
  right: number,
  parent: number,
  payload: Buffer,
): Buffer {
  const buf = Buffer.alloc(MARKET_BLOCK_SIZE);
  buf.writeUInt32LE(left, 0);
  buf.writeUInt32LE(right, 4);
  buf.writeUInt32LE(parent, 8);
  payload.copy(buf, RBTREE_OVERHEAD_BYTES);
  return buf;
}

describe('Market wrapper', () => {
  it('decodes a header with no trees', () => {
    const m = Market.loadFromBuffer({ address: MARKET_ADDR, buffer: buildMarketBuffer({}) });
    expect(m.debtMint().equals(DEBT_MINT)).to.equal(true);
    expect(m.collateralMint().equals(COLL_MINT)).to.equal(true);
    expect(m.admin().equals(ADMIN)).to.equal(true);
    expect(m.isPaused()).to.equal(false);
    expect(m.bids()).to.deep.equal([]);
    expect(m.asks()).to.deep.equal([]);
    expect(m.bestBid()).to.equal(undefined);
    expect(m.bestAsk()).to.equal(undefined);
    expect(m.midBps()).to.equal(undefined);
  });

  // The Ord impl on RestingOrder (resting_order.rs:329-332) inverts the rate
  // comparison for asks: lower rate ⇒ larger Ord ⇒ rightmost slot. So a
  // realistic on-chain layout puts the lowest-rate ask on the RIGHT, not
  // the left. Tests below build the synthetic tree that way, then verify
  // `Market.asks()` yields best-first (lowest rate first) via
  // reverse-in-order traversal.
  it('walks an asks tree with three nodes in price-time order', () => {
    // Layout (root = 600, left child = 700 worse, right child = 500 best):
    //   block 0 (rate 600) — root
    //     left:  block 1 (rate 700)   ← worse ask
    //     right: block 2 (rate 500)   ← best ask (max_index)
    const block0 = rbBlock(MARKET_BLOCK_SIZE, 2 * MARKET_BLOCK_SIZE, NIL,
      restingOrderPayload({ traderSeatIndex: 0, rateBps: 600, termSeconds: 86_400, principalAtoms: 100n, side: Side.Ask }));
    const block1 = rbBlock(NIL, NIL, 0,
      restingOrderPayload({ traderSeatIndex: 0, rateBps: 700, termSeconds: 86_400, principalAtoms: 50n, side: Side.Ask }));
    const block2 = rbBlock(NIL, NIL, 0,
      restingOrderPayload({ traderSeatIndex: 0, rateBps: 500, termSeconds: 86_400, principalAtoms: 200n, side: Side.Ask }));
    const dynamic = Buffer.concat([block0, block1, block2]);
    // asksBestIndex points at block 2 (best ask = rate 500, the rightmost
    // node under the asks Ord). The on-chain program maintains this cache
    // alongside the root; the test must populate it for `bestAsk()` to work.
    const m = Market.loadFromBuffer({
      address: MARKET_ADDR,
      buffer: buildMarketBuffer({
        asksRootIndex: 0,
        asksBestIndex: 2 * MARKET_BLOCK_SIZE,
        dynamic,
      }),
    });
    const asks = m.asks();
    expect(asks.map((a) => a.rateBps)).to.deep.equal([500, 600, 700]);
    expect(m.bestAsk()!.rateBps).to.equal(500);
  });

  it('aggregates L2 levels by (rate, term)', () => {
    // Two 600-rate asks (worse) on the left, one 500-rate ask (best) on the right.
    const block0 = rbBlock(MARKET_BLOCK_SIZE, 2 * MARKET_BLOCK_SIZE, NIL,
      restingOrderPayload({ traderSeatIndex: 0, rateBps: 600, termSeconds: 86_400, principalAtoms: 100n, side: Side.Ask, sequenceNumber: 1n }));
    const block1 = rbBlock(NIL, NIL, 0,
      restingOrderPayload({ traderSeatIndex: 0, rateBps: 600, termSeconds: 86_400, principalAtoms: 200n, side: Side.Ask, sequenceNumber: 2n }));
    const block2 = rbBlock(NIL, NIL, 0,
      restingOrderPayload({ traderSeatIndex: 0, rateBps: 500, termSeconds: 86_400, principalAtoms: 50n, side: Side.Ask, sequenceNumber: 3n }));
    const dynamic = Buffer.concat([block0, block1, block2]);
    const m = Market.loadFromBuffer({
      address: MARKET_ADDR,
      buffer: buildMarketBuffer({ asksRootIndex: 0, dynamic }),
    });
    const l2 = m.asksL2();
    expect(l2.length).to.equal(2);
    // The 500-rate level should be first (best ask = lowest rate).
    expect(l2[0].rateBps).to.equal(500);
    expect(l2[0].principalAtoms).to.equal(50n);
    expect(l2[0].orderCount).to.equal(1);
    expect(l2[1].rateBps).to.equal(600);
    expect(l2[1].principalAtoms).to.equal(300n);
    expect(l2[1].orderCount).to.equal(2);
  });
});

function buildGlobalConfigBuffer(admin: PublicKey, paused: boolean): Buffer {
  const buf = Buffer.alloc(128);
  buf.writeBigUInt64LE(GLOBAL_CONFIG_DISCRIMINANT, 0);
  admin.toBuffer().copy(buf, 8);
  // pending_protocol_admin at 40 — leave zero
  buf.writeUInt8(paused ? 1 : 0, 72);
  return buf;
}

describe('GlobalConfig wrapper', () => {
  it('decodes admin + paused flag', () => {
    const buf = buildGlobalConfigBuffer(ADMIN, true);
    const gc = GlobalConfig.loadFromBuffer({
      address: new PublicKey('11111111111111111111111111111113'),
      buffer: buf,
    });
    expect(gc.protocolAdmin().equals(ADMIN)).to.equal(true);
    expect(gc.isPaused()).to.equal(true);
  });
});

function buildLoanBuffer(args: {
  market: PublicKey;
  outstandingAtoms: bigint;
  lenderClaimable: bigint;
  borrowerRateBps: number;
  lenderRateBps: number;
  startedAt: bigint;
  maturesAt: bigint;
  lastAccrued: bigint;
  state: LoanState;
  loanType: LoanType;
  lenderKind: LenderKind;
  /** Defaults to `outstandingAtoms` — fresh loans have outstanding == principal. */
  principalDebtAtoms?: bigint;
  /** Curator-fee snapshot in bps. Defaults to 0. */
  curatorFeeBpsSnapshot?: number;
}): Buffer {
  const buf = Buffer.alloc(288);
  buf.writeBigUInt64LE(LOAN_FIXED_DISCRIMINANT, 0);
  args.market.toBuffer().copy(buf, 8);
  // created_by @ 40 — zero
  buf.writeBigUInt64LE(1n, 72); // matched_loan_sequence
  // borrower_marginfi_borrow_shares @ 80 (u128) — zero
  // debt_index_at_last_accrual @ 96 (u128) — zero
  buf.writeBigUInt64LE(args.principalDebtAtoms ?? args.outstandingAtoms, 112);
  buf.writeBigUInt64LE(args.outstandingAtoms, 120); // outstanding_debt
  buf.writeBigUInt64LE(args.lenderClaimable, 128);
  buf.writeBigUInt64LE(0n, 136); // collateral
  buf.writeBigUInt64LE(0n, 144); // protocol fee
  buf.writeBigUInt64LE(0n, 152); // curator fee
  buf.writeBigInt64LE(args.startedAt, 160);
  buf.writeBigInt64LE(args.maturesAt, 168);
  buf.writeBigInt64LE(args.lastAccrued, 176);
  buf.writeUInt32LE(0, 184); // lender_seat_index
  buf.writeUInt32LE(0, 188); // borrower_seat_index
  buf.writeUInt16LE(args.borrowerRateBps, 192);
  buf.writeUInt16LE(args.lenderRateBps, 194);
  buf.writeUInt8(args.state, 196);
  buf.writeUInt8(args.loanType, 197);
  buf.writeUInt8(0, 198); // flags
  buf.writeUInt8(0, 199); // version
  buf.writeUInt8(255, 200); // bump
  buf.writeUInt8(args.lenderKind, 201);
  // curator_fee_bps_snapshot @ 204..206 (u16).
  buf.writeUInt16LE(args.curatorFeeBpsSnapshot ?? 0, 204);
  return buf;
}

describe('Loan wrapper', () => {
  const start = 1_700_000_000n;
  const maturity = start + BigInt(86_400 * 30); // 30 days

  it('flags state and type predicates', () => {
    const buf = buildLoanBuffer({
      market: MARKET_ADDR,
      outstandingAtoms: 1_000_000n,
      lenderClaimable: 0n,
      borrowerRateBps: 1000,
      lenderRateBps: 800,
      startedAt: start,
      maturesAt: maturity,
      lastAccrued: start,
      state: LoanState.Active,
      loanType: LoanType.Fixed,
      lenderKind: LenderKind.User,
    });
    const loan = Loan.loadFromBuffer({ address: MARKET_ADDR, buffer: buf });
    expect(loan.isActive()).to.equal(true);
    expect(loan.isFixed()).to.equal(true);
    expect(loan.isP2Pool()).to.equal(false);
    expect(loan.isVaultFunded()).to.equal(false);
  });

  it('accrues outstanding linearly until maturity', () => {
    const buf = buildLoanBuffer({
      market: MARKET_ADDR,
      outstandingAtoms: 1_000_000n,
      lenderClaimable: 0n,
      borrowerRateBps: 1000, // 10%
      lenderRateBps: 800,
      startedAt: start,
      maturesAt: maturity,
      lastAccrued: start,
      state: LoanState.Active,
      loanType: LoanType.Fixed,
      lenderKind: LenderKind.User,
    });
    const loan = Loan.loadFromBuffer({ address: MARKET_ADDR, buffer: buf });
    // 1/2 year at 10% on 1M atoms = 50_000 atoms interest.
    const halfYear = Number(start) + 31_536_000 / 2;
    const out = loan.currentOutstanding(halfYear);
    // Capped at maturity (30 days < half year), so capped to ~30-day interest.
    // 30 days at 10% on 1M = ~8219 atoms.
    expect(out > 1_000_000n).to.equal(true);
    expect(out < 1_100_000n).to.equal(true);
  });

  it('lender claimable accrues at lender_rate_bps', () => {
    const buf = buildLoanBuffer({
      market: MARKET_ADDR,
      outstandingAtoms: 1_000_000n,
      lenderClaimable: 0n,
      borrowerRateBps: 1000,
      lenderRateBps: 800, // 8%
      startedAt: start,
      maturesAt: maturity,
      lastAccrued: start,
      state: LoanState.Active,
      loanType: LoanType.Fixed,
      lenderKind: LenderKind.Vault,
    });
    const loan = Loan.loadFromBuffer({ address: MARKET_ADDR, buffer: buf });
    const claim = loan.lenderClaimable(Number(maturity));
    // Lender share: 8% × principal × (30/365) ≈ 6575 atoms.
    expect(Number(claim)).to.be.greaterThan(0);
    expect(Number(claim)).to.be.lessThan(7000);
  });

  it('accrual uses principal_debt_atoms as basis, not outstanding', () => {
    // Mid-life loan: principal 1M atoms, outstanding has already grown to
    // 1.05M from prior accrue cycles. The Rust accrual computes interest
    // off `principal_debt_atoms`, not `outstanding` — so a one-second
    // simulator tick should add ~`principal × rate / (BPS × SECS_PER_YEAR)`,
    // independent of how much `outstanding` has drifted upward.
    const buf = buildLoanBuffer({
      market: MARKET_ADDR,
      outstandingAtoms: 1_050_000n,
      principalDebtAtoms: 1_000_000n,
      lenderClaimable: 0n,
      borrowerRateBps: 1000,
      lenderRateBps: 800,
      startedAt: start - 1_000n,
      maturesAt: maturity,
      lastAccrued: start,
      state: LoanState.Active,
      loanType: LoanType.Fixed,
      lenderKind: LenderKind.User,
    });
    const loan = Loan.loadFromBuffer({ address: MARKET_ADDR, buffer: buf });
    // 1 day = 86_400s at 10% on principal=1M = 1_000_000 × 1000 × 86_400
    //   ÷ (10_000 × 31_536_000) = 273 atoms (floor). outstanding → 1_050_273.
    const oneDay = Number(start) + 86_400;
    const out = loan.currentOutstanding(oneDay);
    expect(out).to.equal(1_050_273n);
  });

  it('lender claimable subtracts the curator cut on vault-funded loans', () => {
    // 30 days at lender_rate=800bps (8%) on principal=1M = ~6575 atoms gross.
    // curator_fee_bps_snapshot=2000 (20%) ⇒ curator takes 1315; lender net ≈ 5260.
    const buf = buildLoanBuffer({
      market: MARKET_ADDR,
      outstandingAtoms: 1_000_000n,
      principalDebtAtoms: 1_000_000n,
      lenderClaimable: 0n,
      borrowerRateBps: 1000,
      lenderRateBps: 800,
      startedAt: start,
      maturesAt: maturity,
      lastAccrued: start,
      state: LoanState.Active,
      loanType: LoanType.Fixed,
      lenderKind: LenderKind.Vault,
      curatorFeeBpsSnapshot: 2000,
    });
    const loan = Loan.loadFromBuffer({ address: MARKET_ADDR, buffer: buf });
    const claim = loan.lenderClaimable(Number(maturity));
    // Net of curator cut should be ~5260, definitely < 6000.
    expect(Number(claim)).to.be.greaterThan(5000);
    expect(Number(claim)).to.be.lessThan(6000);
  });

  it('P2Pool loans are a no-op for the accrual simulator', () => {
    // P2Pool outstanding tracks marginfi share inflation directly (Rust
    // `accrue_loan` bumps `last_accrued_unix` and returns).
    const buf = buildLoanBuffer({
      market: MARKET_ADDR,
      outstandingAtoms: 1_000_000n,
      principalDebtAtoms: 1_000_000n,
      lenderClaimable: 0n,
      borrowerRateBps: 1000,
      lenderRateBps: 800,
      startedAt: start,
      maturesAt: maturity,
      lastAccrued: start,
      state: LoanState.Active,
      loanType: LoanType.P2Pool,
      lenderKind: LenderKind.User,
    });
    const loan = Loan.loadFromBuffer({ address: MARKET_ADDR, buffer: buf });
    expect(loan.currentOutstanding(Number(maturity))).to.equal(1_000_000n);
    expect(loan.lenderClaimable(Number(maturity))).to.equal(0n);
  });
});
