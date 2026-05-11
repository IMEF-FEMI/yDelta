import { expect } from 'chai';
import { Keypair, PublicKey } from '@solana/web3.js';
import {
  MARKET_FIXED_DISCRIMINANT,
  MARKET_FIXED_SIZE,
  MARKET_BLOCK_SIZE,
  NIL,
  RBTREE_OVERHEAD_BYTES,
} from '../src/utils/discriminator';
import { Market } from '../src/market';
import {
  Side,
  OrderKind,
  OrderType,
} from '../src/ydelta/types/enums';
import {
  findMatchableAsks,
  findMatchableBids,
  orderbookSnapshot,
  decodeError,
  parseCustomCodeFromLog,
  findYdeltaErrorCodeInLogs,
} from '../src/helpers';
import { YDELTA_PROGRAM_ID } from '../src/utils/programId';

const MARKET_ADDR = new PublicKey('11111111111111111111111111111112');
const DEBT_MINT = Keypair.generate().publicKey;
const COLL_MINT = Keypair.generate().publicKey;
const ADMIN = Keypair.generate().publicKey;

function buildMarketBufferWithAsks(
  asks: { rateBps: number; termSeconds: number; principalAtoms: bigint; seat: number }[],
  protocolFeeBpsFloor = 0,
): Buffer {
  // Build a flat left-leaning chain. Caller passes asks best-first
  // (ascending rate). Reverse-in-order on this shape yields the chain
  // root → left → left-of-left → ..., which means the SDK's `iterAsks`
  // visits asks in the caller-supplied order (best-first). This matches
  // the on-chain matching-engine walk: asks Ord puts the best ask at
  // `max_index` (resting_order.rs:329-332).
  const ordered = [...asks];
  const blocks = ordered.map((a, i) => {
    const block = Buffer.alloc(MARKET_BLOCK_SIZE);
    const left = i + 1 < ordered.length ? (i + 1) * MARKET_BLOCK_SIZE : NIL;
    const right = NIL;
    const parent = i === 0 ? NIL : (i - 1) * MARKET_BLOCK_SIZE;
    block.writeUInt32LE(left, 0);
    block.writeUInt32LE(right, 4);
    block.writeUInt32LE(parent, 8);
    // payload starts at +16
    const p = block.subarray(RBTREE_OVERHEAD_BYTES);
    p.writeUInt32LE(a.seat, 0);
    p.writeUInt32LE(0, 4); // loan_sequence_snapshot
    p.writeBigUInt64LE(BigInt(i + 1), 8); // sequence_number
    p.writeBigUInt64LE(a.principalAtoms, 16);
    p.writeBigUInt64LE(0n, 24);
    p.writeBigUInt64LE(0n, 32);
    p.writeBigInt64LE(0n, 40);
    p.writeUInt32LE(a.termSeconds, 80);
    p.writeUInt16LE(a.rateBps, 84);
    p.writeUInt8(Side.Ask, 86);
    p.writeUInt8(OrderKind.Primary, 87);
    p.writeUInt8(OrderType.Limit, 88);
    p.writeUInt8(0, 89); // flags
    return block;
  });
  const dynamic = Buffer.concat(blocks);
  const buf = Buffer.alloc(MARKET_FIXED_SIZE + dynamic.length);
  buf.writeBigUInt64LE(MARKET_FIXED_DISCRIMINANT, 0);
  DEBT_MINT.toBuffer().copy(buf, 16);
  COLL_MINT.toBuffer().copy(buf, 48);
  buf.writeUInt32LE(NIL, 180); // bids
  buf.writeUInt32LE(NIL, 184);
  buf.writeUInt32LE(asks.length > 0 ? 0 : NIL, 188); // asks root
  buf.writeUInt32LE(NIL, 192);
  buf.writeUInt32LE(NIL, 196);
  buf.writeUInt32LE(NIL, 200);
  // FeeConfig at offset 212; protocol_fee_bps_floor at 212+0
  buf.writeUInt16LE(protocolFeeBpsFloor, 212);
  ADMIN.toBuffer().copy(buf, 440);
  dynamic.copy(buf, MARKET_FIXED_SIZE);
  return buf;
}

describe('findMatchableAsks', () => {
  it('fully fills a bid that exceeds the rate floor', () => {
    const buf = buildMarketBufferWithAsks([
      { rateBps: 500, termSeconds: 86_400, principalAtoms: 100n, seat: 1 },
      { rateBps: 600, termSeconds: 86_400, principalAtoms: 200n, seat: 2 },
    ]);
    const m = Market.loadFromBuffer({ address: MARKET_ADDR, buffer: buf });
    const result = findMatchableAsks(m, {
      rateBps: 700,
      termSeconds: 86_400,
      principalAtoms: 150,
    });
    expect(result.totalFilled.toNumber()).to.equal(150);
    expect(result.residualPrincipal.toNumber()).to.equal(0);
    expect(result.fills.length).to.equal(2);
    expect(result.fills[0].rateBps).to.equal(500);
    expect(result.fills[1].rateBps).to.equal(600);
    // 100×500 + 50×600 = 50_000 + 30_000 = 80_000; / 150 = 533
    expect(result.weightedAvgRateBps).to.equal(533);
  });

  it('skips asks whose term is shorter than the bid term', () => {
    const buf = buildMarketBufferWithAsks([
      { rateBps: 500, termSeconds: 86_400, principalAtoms: 100n, seat: 1 },
      { rateBps: 600, termSeconds: 7 * 86_400, principalAtoms: 200n, seat: 2 },
    ]);
    const m = Market.loadFromBuffer({ address: MARKET_ADDR, buffer: buf });
    const result = findMatchableAsks(m, {
      rateBps: 800,
      termSeconds: 5 * 86_400,
      principalAtoms: 200,
    });
    expect(result.fills.length).to.equal(1);
    expect(result.fills[0].rateBps).to.equal(600);
    expect(result.residualPrincipal.toNumber()).to.equal(0); // filled to 200 from the 200-atom 600bps maker
  });

  it('respects the protocol fee floor', () => {
    const buf = buildMarketBufferWithAsks(
      [
        { rateBps: 500, termSeconds: 86_400, principalAtoms: 100n, seat: 1 },
        { rateBps: 600, termSeconds: 86_400, principalAtoms: 200n, seat: 2 },
      ],
      /*protocolFeeBpsFloor=*/ 150,
    );
    const m = Market.loadFromBuffer({ address: MARKET_ADDR, buffer: buf });
    // Bid rate 620 needs spread >= 150 → ask_rate <= 470. Neither maker qualifies.
    const result = findMatchableAsks(m, {
      rateBps: 620,
      termSeconds: 86_400,
      principalAtoms: 200,
    });
    expect(result.fills.length).to.equal(0);
    expect(result.residualPrincipal.toNumber()).to.equal(200);
  });

  it('PostOnly returns zero fills', () => {
    const buf = buildMarketBufferWithAsks([
      { rateBps: 500, termSeconds: 86_400, principalAtoms: 100n, seat: 1 },
    ]);
    const m = Market.loadFromBuffer({ address: MARKET_ADDR, buffer: buf });
    const result = findMatchableAsks(m, {
      rateBps: 800,
      termSeconds: 86_400,
      principalAtoms: 100,
      orderType: OrderType.PostOnly,
    });
    expect(result.fills.length).to.equal(0);
    expect(result.residualPrincipal.toNumber()).to.equal(100);
  });

  it('ImmediateOrCancel drops residual', () => {
    const buf = buildMarketBufferWithAsks([
      { rateBps: 500, termSeconds: 86_400, principalAtoms: 50n, seat: 1 },
    ]);
    const m = Market.loadFromBuffer({ address: MARKET_ADDR, buffer: buf });
    const result = findMatchableAsks(m, {
      rateBps: 800,
      termSeconds: 86_400,
      principalAtoms: 100,
      orderType: OrderType.ImmediateOrCancel,
    });
    expect(result.totalFilled.toNumber()).to.equal(50);
    expect(result.residualPrincipal.toNumber()).to.equal(0); // residual cancelled
  });

  it('self-match-guard skips the maker with the matching seat index', () => {
    const buf = buildMarketBufferWithAsks([
      { rateBps: 500, termSeconds: 86_400, principalAtoms: 100n, seat: 7 },
      { rateBps: 600, termSeconds: 86_400, principalAtoms: 100n, seat: 8 },
    ]);
    const m = Market.loadFromBuffer({ address: MARKET_ADDR, buffer: buf });
    const result = findMatchableAsks(m, {
      rateBps: 800,
      termSeconds: 86_400,
      principalAtoms: 200,
      ownSeatIndex: 7,
    });
    expect(result.fills.length).to.equal(1);
    expect(result.fills[0].maker.traderSeatIndex).to.equal(8);
    expect(result.residualPrincipal.toNumber()).to.equal(100);
  });
});

describe('orderbookSnapshot', () => {
  it('returns L2 asks and undefined bids when no bids exist', () => {
    const buf = buildMarketBufferWithAsks([
      { rateBps: 500, termSeconds: 86_400, principalAtoms: 100n, seat: 1 },
    ]);
    const m = Market.loadFromBuffer({ address: MARKET_ADDR, buffer: buf });
    const snap = orderbookSnapshot(m);
    expect(snap.bids.length).to.equal(0);
    expect(snap.asks.length).to.equal(1);
    expect(snap.bestAskBps).to.equal(500);
    expect(snap.bestBidBps).to.equal(undefined);
    expect(snap.spreadBps).to.equal(undefined);
  });
});

describe('decodeError', () => {
  it('decodes a hex code', () => {
    const r = decodeError({ hex: '0x36' }); // 54 = MarketPaused
    expect(r).to.be.ok;
    expect(r!.name).to.equal('MarketPaused');
    expect(r!.code).to.equal(54);
  });

  it('decodes a numeric code', () => {
    const r = decodeError({ code: 14 });
    expect(r!.name).to.equal('InsufficientWithdrawableBalance');
  });

  it('returns null for unknown codes', () => {
    expect(decodeError({ code: 9999 })).to.equal(null);
  });

  it('parses Custom from a raw log line (hex)', () => {
    expect(parseCustomCodeFromLog('Program log: custom program error: 0x36')).to.equal(54);
  });

  it('parses Custom from a raw log line (decimal)', () => {
    expect(parseCustomCodeFromLog('custom program error: 17')).to.equal(17);
  });

  it('matches errors against the ydelta program in a logs[] array', () => {
    const programId = YDELTA_PROGRAM_ID.toBase58();
    const logs = [
      `Program ${programId} invoke [1]`,
      'Program log: validation failed',
      `Program ${programId} failed: custom program error: 0x3a`, // 58
    ];
    const code = findYdeltaErrorCodeInLogs(logs);
    expect(code).to.equal(58);
    expect(decodeError({ logs })!.name).to.equal('BorrowerLtvOverInit');
  });

  it('ignores Custom errors from other programs', () => {
    const otherId = 'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA';
    const logs = [
      `Program ${otherId} invoke [1]`,
      `Program ${otherId} failed: custom program error: 0x1`,
    ];
    expect(findYdeltaErrorCodeInLogs(logs)).to.equal(null);
  });

  it('does not decode a foreign error message without ydelta-scoped logs', () => {
    const otherId = 'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA';
    const error = `Program ${otherId} failed: custom program error: 0x1`;
    expect(decodeError({ error })).to.equal(null);
  });

  it('decodes a simulation response', () => {
    const programId = YDELTA_PROGRAM_ID.toBase58();
    const simulation = {
      logs: [
        `Program ${programId} invoke [1]`,
        `Program ${programId} failed: custom program error: 0x6`, // 6 = PostOnlyWouldCross
      ],
      err: null,
      accounts: null,
      unitsConsumed: 0,
      returnData: null,
    } as never;
    expect(decodeError({ simulation })!.name).to.equal('PostOnlyWouldCross');
  });
});

describe('findMatchableBids', () => {
  it('walks bids best-first (highest rate)', () => {
    // For brevity, we'll synthesize a bids tree the same way as asks.
    // Bids: best is highest rate; Market.bids() handles the reverse.
    const buf = buildBidsBuffer([
      { rateBps: 800, termSeconds: 86_400, principalAtoms: 100n, seat: 1 }, // best-rate
      { rateBps: 600, termSeconds: 86_400, principalAtoms: 200n, seat: 2 },
    ]);
    const m = Market.loadFromBuffer({ address: MARKET_ADDR, buffer: buf });
    // Ask at rate 500: spreads against both bids (800 and 600). Fills best-first.
    const result = findMatchableBids(m, {
      rateBps: 500,
      termSeconds: 86_400,
      principalAtoms: 150,
    });
    expect(result.fills.length).to.equal(2);
    expect(result.fills[0].rateBps).to.equal(800);
    expect(result.fills[1].rateBps).to.equal(600);
    expect(result.totalFilled.toNumber()).to.equal(150);
  });
});

function buildBidsBuffer(
  bids: { rateBps: number; termSeconds: number; principalAtoms: bigint; seat: number }[],
): Buffer {
  // Bids Ord is ascending by rate (resting_order.rs:330) — highest-rate bid
  // at `max_index` (rightmost). Build a left-leaning chain with bids passed
  // best-first (descending rate); reverse-in-order visits them in the
  // caller-supplied order.
  const sorted = [...bids].sort((a, b) => b.rateBps - a.rateBps);
  const blocks = sorted.map((b, i) => {
    const block = Buffer.alloc(MARKET_BLOCK_SIZE);
    block.writeUInt32LE(i + 1 < sorted.length ? (i + 1) * MARKET_BLOCK_SIZE : NIL, 0); // left → next worse bid
    block.writeUInt32LE(NIL, 4);
    block.writeUInt32LE(i === 0 ? NIL : (i - 1) * MARKET_BLOCK_SIZE, 8);
    const p = block.subarray(RBTREE_OVERHEAD_BYTES);
    p.writeUInt32LE(b.seat, 0);
    p.writeUInt32LE(0, 4);
    p.writeBigUInt64LE(BigInt(i + 1), 8);
    p.writeBigUInt64LE(b.principalAtoms, 16);
    p.writeBigUInt64LE(0n, 24);
    p.writeBigUInt64LE(0n, 32);
    p.writeBigInt64LE(0n, 40);
    p.writeUInt32LE(b.termSeconds, 80);
    p.writeUInt16LE(b.rateBps, 84);
    p.writeUInt8(Side.Bid, 86);
    p.writeUInt8(OrderKind.Primary, 87);
    p.writeUInt8(OrderType.Limit, 88);
    p.writeUInt8(0, 89);
    return block;
  });
  const dynamic = Buffer.concat(blocks);
  const buf = Buffer.alloc(MARKET_FIXED_SIZE + dynamic.length);
  buf.writeBigUInt64LE(MARKET_FIXED_DISCRIMINANT, 0);
  DEBT_MINT.toBuffer().copy(buf, 16);
  COLL_MINT.toBuffer().copy(buf, 48);
  buf.writeUInt32LE(0, 180); // bids root
  buf.writeUInt32LE(NIL, 184);
  buf.writeUInt32LE(NIL, 188);
  buf.writeUInt32LE(NIL, 192);
  buf.writeUInt32LE(NIL, 196);
  buf.writeUInt32LE(NIL, 200);
  ADMIN.toBuffer().copy(buf, 440);
  dynamic.copy(buf, MARKET_FIXED_SIZE);
  return buf;
}
